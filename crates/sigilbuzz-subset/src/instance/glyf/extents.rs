//! The bounding boxes of composite glyphs at the instance coordinates,
//! with the work of the whole bake bounded.
//!
//! A composite's box is the extremes of its outline drawn at the
//! instance, as HarfBuzz's instancer takes it. Drawing every composite
//! from scratch costs as much as its whole component tree, and a small
//! font can make that tree huge: a component reused at every level
//! doubles the tree each level down. So each glyph's extent is found
//! once and kept, and a composite's comes from its components':
//!
//! - A component placed by offset, with no transform or one that maps
//!   the axes onto the axes (a scale, a flip, a quarter turn), moves
//!   its component's extremes the way it moves the points, so the
//!   composite's extremes are exact, the same values a draw gives.
//! - A composite with a component placed by matching points, or with a
//!   rotation or skew, is drawn through
//!   [`sigilbuzz::tables::Glyf::outline_at_coords`]. The draws of one
//!   bake share one budget, [`DRAW_BUDGET`], charged with what each
//!   draw does: every glyph it visits, with its points and its `gvar`
//!   tuples (see [`Extent::cost`]). Once a draw would overrun it, a
//!   skewed composite takes the box around its transformed components'
//!   boxes, and one placed by matching points keeps its source box;
//!   both are reported.
//!
//! A composite whose components nest more than [`MAX_DEPTH`] levels
//! below it, or one that reaches itself, has no extent, as it cannot be
//! drawn; it keeps its source box and is reported. Both are properties
//! of the glyph, not of the order the bake reaches it in: each glyph is
//! worked out once, its height kept with its extent, and the walk keeps
//! its own stack, so a long chain of composites costs one step a glyph.

use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use sigilbuzz::tables::glyf::PhantomMetrics;
use sigilbuzz::tables::OutlineSink;
use sigilbuzz::Error;

use super::{read_component_records, BakeCtx, CompRecord, SimpleGlyph};
use crate::util::round_half_up;

/// Composite nesting a draw allows, as in the core outline walk and in
/// HarfBuzz: a glyph whose components nest deeper cannot be drawn.
const MAX_DEPTH: u8 = 64;

/// The work the composites one bake draws may do in all, summed over
/// the draws, in the units of [`Extent::cost`].
pub(super) const DRAW_BUDGET: u64 = 1 << 20;

/// Phantom points `gvar` adds to every glyph's own points.
const PHANTOM_POINTS: u64 = 4;

/// What the bake knows about one glyph drawn at the instance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Extent {
    /// The extremes `(xMin, yMin, xMax, yMax)` of the drawn points,
    /// unrounded; `None` when nothing is drawn.
    pub(super) bounds: Option<[f32; 4]>,
    /// The work a core draw of the glyph does, saturating: for the
    /// glyph and every component all the way down, one unit, its own
    /// points (a composite's are its components), and for each of its
    /// `gvar` tuples the axis count plus its points and phantom points.
    /// That is at least what the core walk spends decoding the tuples
    /// of every glyph it visits.
    cost: u64,
    /// Composite levels below the glyph: 0 for a simple or empty glyph.
    height: u8,
}

impl Extent {
    const EMPTY: Self = Self {
        bounds: None,
        cost: 1,
        height: 0,
    };
}

/// Why a glyph has no extent.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum NoExtent {
    /// The glyph's data or a component's cannot be read, the glyph
    /// reaches itself, or drawing it is past the budget.
    Broken(Error),
    /// Its components nest more than [`MAX_DEPTH`] levels below it.
    TooDeep,
}

/// One glyph's slot in the memo.
#[derive(Debug, Clone, PartialEq)]
enum Slot {
    Unknown,
    /// Being worked out: reaching it again means a cycle.
    Busy,
    Known(Result<Extent, NoExtent>),
}

/// A composite on the walk's stack: its components, and the first one
/// not yet known.
struct Frame {
    gid: u16,
    components: Vec<CompRecord>,
    next: usize,
}

/// How starting on a glyph went.
enum Start {
    /// Its extent is known now (or it has none).
    Done,
    /// A composite, whose components the walk takes first.
    Composite(Vec<CompRecord>),
}

/// The extents of a bake's glyphs, each worked out once.
pub(super) struct Extents {
    slots: RefCell<Vec<Slot>>,
    /// Draw budget left.
    draws_left: Cell<u64>,
    /// Extents worked out: at most one per glyph.
    pub(super) computed: Cell<u64>,
    /// Composites drawn through the core outline walk.
    pub(super) drawn: Cell<u64>,
}

impl Extents {
    pub(super) fn new(num_glyphs: u16) -> Self {
        Self {
            slots: RefCell::new(alloc::vec![Slot::Unknown; usize::from(num_glyphs)]),
            draws_left: Cell::new(DRAW_BUDGET),
            computed: Cell::new(0),
            drawn: Cell::new(0),
        }
    }

    /// The draw budget spent so far.
    #[cfg(test)]
    pub(super) fn spent(&self) -> u64 {
        DRAW_BUDGET - self.draws_left.get()
    }

    /// The extent of glyph `gid` drawn on its own.
    pub(super) fn of(&self, cx: &BakeCtx<'_, '_>, gid: u16) -> Result<Extent, NoExtent> {
        let mut stack: Vec<Frame> = Vec::new();
        if let Start::Composite(components) = self.start(cx, gid) {
            stack.push(Frame {
                gid,
                components,
                next: 0,
            });
        }
        while let Some(top) = stack.last_mut() {
            // Step over the components already known, down into the
            // first that is not.
            let mut down = None;
            let mut failed = None;
            while let Some(c) = top.components.get(top.next) {
                match self.slot(c.glyph) {
                    // A component past the last glyph draws nothing.
                    None | Some(Slot::Known(Ok(_))) => top.next += 1,
                    Some(Slot::Known(Err(e))) => {
                        failed = Some(e);
                        break;
                    }
                    Some(Slot::Busy) => {
                        failed = Some(NoExtent::Broken(Error::Malformed {
                            offset: 0,
                            context: "glyf composite reaches itself",
                        }));
                        break;
                    }
                    Some(Slot::Unknown) => {
                        down = Some(c.glyph);
                        break;
                    }
                }
            }
            if let Some(e) = failed {
                let gid = top.gid;
                stack.pop();
                self.set(gid, Slot::Known(Err(e)));
            } else if let Some(child) = down {
                if let Start::Composite(components) = self.start(cx, child) {
                    stack.push(Frame {
                        gid: child,
                        components,
                        next: 0,
                    });
                }
            } else if let Some(frame) = stack.pop() {
                let found = self.finish(cx, frame.gid, &frame.components);
                self.set(frame.gid, Slot::Known(found));
            }
        }
        match self.slot(gid) {
            Some(Slot::Known(known)) => known,
            _ => Ok(Extent::EMPTY),
        }
    }

    fn slot(&self, gid: u16) -> Option<Slot> {
        self.slots.borrow().get(usize::from(gid)).cloned()
    }

    fn set(&self, gid: u16, slot: Slot) {
        if let Some(s) = self.slots.borrow_mut().get_mut(usize::from(gid)) {
            *s = slot;
        }
    }

    /// Starts on glyph `gid`, whose slot is unknown or past the end: a
    /// simple or empty glyph is worked out at once; a composite is
    /// marked busy and its components handed back.
    fn start(&self, cx: &BakeCtx<'_, '_>, gid: u16) -> Start {
        match self.slot(gid) {
            None | Some(Slot::Known(_) | Slot::Busy) => return Start::Done,
            Some(Slot::Unknown) => {}
        }
        self.computed.set(self.computed.get() + 1);
        let broken = |context| NoExtent::Broken(Error::Malformed { offset: 0, context });
        let body = match cx.loca.range(gid) {
            Some((s, e)) if s != e => cx.glyf_bytes.get(s as usize..e as usize),
            _ => Some(&[][..]),
        };
        let Some(body) = body else {
            self.set(
                gid,
                Slot::Known(Err(broken("glyf range from loca falls outside glyf table"))),
            );
            return Start::Done;
        };
        let nc = body
            .first_chunk::<10>()
            .map_or(0, |h| i16::from_be_bytes([h[0], h[1]]));
        if nc == 0 {
            self.set(gid, Slot::Known(Ok(Extent::EMPTY)));
            return Start::Done;
        }
        if nc > 0 {
            let found = match SimpleGlyph::decode(body) {
                Ok(glyph) => {
                    let points = glyph.points();
                    let deltas = cx.deltas(gid, &points, &glyph.end_pts);
                    let mut bounds: Option<[f32; 4]> = None;
                    for (p, d) in points.iter().zip(&deltas) {
                        add_point(&mut bounds, p.0 as f32 + d.0, p.1 as f32 + d.1);
                    }
                    Ok(Extent {
                        bounds,
                        cost: own_cost(cx, gid, points.len()),
                        height: 0,
                    })
                }
                Err(_) => Err(broken("glyf simple glyph truncated")),
            };
            self.set(gid, Slot::Known(found));
            return Start::Done;
        }
        match read_component_records(body) {
            Ok(components) => {
                self.set(gid, Slot::Busy);
                Start::Composite(components)
            }
            Err(_) => {
                self.set(gid, Slot::Known(Err(broken("glyf composite truncated"))));
                Start::Done
            }
        }
    }

    /// The extent of composite `gid` from its `components`, every one
    /// of which has its extent.
    fn finish(
        &self,
        cx: &BakeCtx<'_, '_>,
        gid: u16,
        components: &[CompRecord],
    ) -> Result<Extent, NoExtent> {
        let mut extent = Extent {
            bounds: None,
            cost: own_cost(cx, gid, components.len()),
            height: 0,
        };
        let mut children = Vec::with_capacity(components.len());
        for c in components {
            let child = match self.slot(c.glyph) {
                Some(Slot::Known(known)) => known?,
                _ => Extent::EMPTY,
            };
            extent.cost = extent.cost.saturating_add(child.cost);
            extent.height = extent.height.max(child.height.saturating_add(1));
            children.push(child);
        }
        if extent.height > MAX_DEPTH {
            return Err(NoExtent::TooDeep);
        }
        if components.iter().all(CompRecord::keeps_axes) {
            let points: Vec<(i32, i32)> = components.iter().map(CompRecord::gvar_point).collect();
            let deltas = cx.deltas(gid, &points, &[]);
            for (i, (c, child)) in components.iter().zip(&children).enumerate() {
                let Some(b) = child.bounds else {
                    continue;
                };
                let (dx, dy) = deltas.get(i).copied().unwrap_or((0.0, 0.0));
                let (tx, ty) = c.offset(dx, dy);
                for (x, y) in c.axis_extremes(b) {
                    add_point(&mut extent.bounds, x + tx, y + ty);
                }
            }
            return Ok(extent);
        }
        extent.bounds = self.drawn_bounds(cx, gid, components, &children, extent.cost)?;
        Ok(extent)
    }

    /// The bounds of composite `gid`, whose components are not all
    /// placed by an axis-keeping offset, from a core draw costing `cost`
    /// while the budget lasts. Past it, a composite placed by offsets
    /// takes the box around its components' transformed boxes; one
    /// placed by matching points has no exact bounds to give.
    fn drawn_bounds(
        &self,
        cx: &BakeCtx<'_, '_>,
        gid: u16,
        components: &[CompRecord],
        children: &[Extent],
        cost: u64,
    ) -> Result<Option<[f32; 4]>, NoExtent> {
        if let Some(left) = self.draws_left.get().checked_sub(cost) {
            self.draws_left.set(left);
            self.drawn.set(self.drawn.get() + 1);
            let metrics = PhantomMetrics {
                hmtx: cx.hmtx,
                vmtx: cx.vmtx,
            };
            let mut sink = BoundsSink::default();
            cx.glyf
                .outline_at_coords(cx.loca, gid, cx.gvar, cx.coords, Some(&metrics), &mut sink)
                .map_err(NoExtent::Broken)?;
            return Ok(sink.extremes);
        }
        if components.iter().any(CompRecord::is_anchored) {
            return Err(NoExtent::Broken(Error::Unsupported {
                context: "glyf composite too costly to draw for its bounds",
            }));
        }
        cx.warnings.push(
            sigilbuzz::tables::tag::GLYF,
            cx.loca.range(gid).map_or(0, |(s, _)| s as usize),
            "glyf composite too costly to draw for its bounds",
            "the exact bounding box (the box around its components' boxes is used)",
        );
        let points: Vec<(i32, i32)> = components.iter().map(CompRecord::gvar_point).collect();
        let deltas = cx.deltas(gid, &points, &[]);
        let mut bounds = None;
        for (i, (c, child)) in components.iter().zip(children).enumerate() {
            let Some(b) = child.bounds else {
                continue;
            };
            let (dx, dy) = deltas.get(i).copied().unwrap_or((0.0, 0.0));
            let (tx, ty) = c.offset(dx, dy);
            for (x, y) in [(b[0], b[1]), (b[0], b[3]), (b[2], b[1]), (b[2], b[3])] {
                let (x, y) = c.transform(x, y);
                add_point(&mut bounds, x + tx, y + ty);
            }
        }
        Ok(bounds)
    }
}

/// The work a core draw does on glyph `gid` itself, which has `points`
/// points of its own (a composite's are its components): one unit, the
/// points, and for each of its `gvar` tuples the axis count plus the
/// points and phantom points.
fn own_cost(cx: &BakeCtx<'_, '_>, gid: u16, points: usize) -> u64 {
    let points = points as u64;
    let per_tuple = cx
        .tuples
        .axis_count
        .saturating_add(points)
        .saturating_add(PHANTOM_POINTS);
    1u64.saturating_add(points)
        .saturating_add(cx.tuples.of(gid).saturating_mul(per_tuple))
}

/// Each glyph's `gvar` tuple count, read from the table's bytes: the
/// part of a draw's cost that the glyph's points do not show.
#[derive(Debug, Default)]
pub(super) struct TupleCounts<'f> {
    data: &'f [u8],
    axis_count: u64,
    glyph_count: u16,
    long_offsets: bool,
    data_array_off: usize,
}

impl<'f> TupleCounts<'f> {
    /// The counts of the `gvar` table `data`; none when there is no
    /// table or its header cannot be read.
    pub(super) fn new(data: Option<&'f [u8]>) -> Self {
        let Some(data) = data else {
            return Self::default();
        };
        let u16_at = |at: usize| {
            data.get(at..at + 2)
                .map(|b| u16::from_be_bytes([b[0], b[1]]))
        };
        let u32_at = |at: usize| {
            data.get(at..at + 4)
                .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        };
        let (Some(axis_count), Some(glyph_count), Some(flags), Some(data_array_off)) =
            (u16_at(4), u16_at(12), u16_at(14), u32_at(16))
        else {
            return Self::default();
        };
        Self {
            data,
            axis_count: u64::from(axis_count),
            glyph_count,
            long_offsets: flags & 1 != 0,
            data_array_off: data_array_off as usize,
        }
    }

    /// The tuple count of glyph `gid`; 0 when it has no variation data
    /// or the count cannot be read.
    fn of(&self, gid: u16) -> u64 {
        if gid >= self.glyph_count {
            return 0;
        }
        let offset = |i: usize| -> Option<usize> {
            if self.long_offsets {
                let at = 20 + 4 * i;
                let b = self.data.get(at..at + 4)?;
                Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
            } else {
                let at = 20 + 2 * i;
                let b = self.data.get(at..at + 2)?;
                Some(usize::from(u16::from_be_bytes([b[0], b[1]])) * 2)
            }
        };
        let i = usize::from(gid);
        let (Some(start), Some(end)) = (offset(i), offset(i + 1)) else {
            return 0;
        };
        if end <= start {
            return 0;
        }
        let at = self.data_array_off.saturating_add(start);
        self.data
            .get(at..at.saturating_add(2))
            .map_or(0, |b| u64::from(u16::from_be_bytes([b[0], b[1]]) & 0x0FFF))
    }
}

/// An [`OutlineSink`] that keeps the extremes of every point it is
/// given. A TrueType outline passes on every contour point, on or off
/// the curve, and nothing outside them, so these are the extremes of
/// the glyph's points.
#[derive(Debug, Default)]
struct BoundsSink {
    extremes: Option<[f32; 4]>,
}

impl BoundsSink {
    fn add(&mut self, x: f32, y: f32) {
        let e = self.extremes.get_or_insert([x, y, x, y]);
        e[0] = e[0].min(x);
        e[1] = e[1].min(y);
        e[2] = e[2].max(x);
        e[3] = e[3].max(y);
    }
}

impl OutlineSink for BoundsSink {
    fn move_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.add(cx, cy);
        self.add(x, y);
    }
    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.add(c1x, c1y);
        self.add(c2x, c2y);
        self.add(x, y);
    }
    fn close(&mut self) {}
}

/// Widens `bounds` to take in `(x, y)`.
fn add_point(bounds: &mut Option<[f32; 4]>, x: f32, y: f32) {
    let b = bounds.get_or_insert([x, y, x, y]);
    b[0] = b[0].min(x);
    b[1] = b[1].min(y);
    b[2] = b[2].max(x);
    b[3] = b[3].max(y);
}

/// `bounds` rounded to whole units, halves up.
pub(super) fn rounded(bounds: Option<[f32; 4]>) -> Option<[i16; 4]> {
    bounds.map(|b| b.map(|v| super::clamp_i16(round_half_up(v))))
}

impl CompRecord {
    /// True when the component is placed by offset and its transform
    /// maps the axes onto the axes, so it moves its component's
    /// extremes as it moves the points.
    fn keeps_axes(&self) -> bool {
        let [xx, yx, xy, yy] = self.matrix;
        !self.is_anchored() && ((xy == 0.0 && yx == 0.0) || (xx == 0.0 && yy == 0.0))
    }

    /// `(x, y)` through the component's 2x2, as the core walk applies
    /// it: `xx * x + xy * y`, `yx * x + yy * y`.
    fn transform(&self, x: f32, y: f32) -> (f32, f32) {
        let [xx, yx, xy, yy] = self.matrix;
        (xx * x + xy * y + 0.0, yx * x + yy * y + 0.0)
    }

    /// The component's offset with its delta `(dx, dy)`, through the
    /// 2x2 when `SCALED_COMPONENT_OFFSET` asks for it, as the core walk
    /// places it.
    fn offset(&self, dx: f32, dy: f32) -> (f32, f32) {
        let (ox, oy) = self.gvar_point();
        let (lx, ly) = (ox as f32 + dx, oy as f32 + dy);
        if self.scales_offset() {
            self.transform(lx, ly)
        } else {
            (lx, ly)
        }
    }

    /// The points of the component's box that its axis-keeping 2x2
    /// takes to the transformed box's extremes: two opposite corners,
    /// as the transform maps each axis to one axis.
    fn axis_extremes(&self, b: [f32; 4]) -> [(f32, f32); 2] {
        let lo = self.transform(b[0], b[1]);
        let hi = self.transform(b[2], b[3]);
        [lo, hi]
    }
}
