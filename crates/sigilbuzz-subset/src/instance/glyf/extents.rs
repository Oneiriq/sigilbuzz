//! The bounding boxes of composite glyphs at the instance coordinates,
//! with bounded work.
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
//!   bake share a budget, [`DRAW_BUDGET`], counted in the glyphs and
//!   points they visit. Once a draw would overrun it, a skewed
//!   composite takes the box around its transformed components' boxes,
//!   and one placed by matching points keeps its source box; both are
//!   reported.
//!
//! A composite more than [`MAX_DEPTH`] levels deep, or one that reaches
//! itself, has no extent, as it cannot be drawn; it keeps its source box
//! and is reported.

use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use sigilbuzz::tables::glyf::PhantomMetrics;
use sigilbuzz::tables::OutlineSink;
use sigilbuzz::Error;

use super::{read_component_records, BakeCtx, CompRecord, SimpleGlyph};
use crate::util::round_half_up;

/// Composite nesting a draw allows, as in the core outline walk and in
/// HarfBuzz.
const MAX_DEPTH: u8 = 64;

/// The glyphs and points the composites one bake draws may visit in
/// all, summed over the draws.
pub(super) const DRAW_BUDGET: u64 = 1 << 20;

/// What the bake knows about one glyph drawn at the instance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Extent {
    /// The extremes `(xMin, yMin, xMax, yMax)` of the drawn points,
    /// unrounded; `None` when nothing is drawn.
    pub(super) bounds: Option<[f32; 4]>,
    /// Glyph records a draw visits (the glyph and every component, all
    /// the way down), saturating.
    visits: u64,
    /// Contour points a draw lays down, saturating.
    points: u64,
    /// Composite levels below the glyph: 0 for a simple or empty glyph.
    height: u8,
}

impl Extent {
    const EMPTY: Self = Self {
        bounds: None,
        visits: 1,
        points: 0,
        height: 0,
    };
}

/// Why a glyph has no extent.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum NoExtent {
    /// The glyph's data or a component's cannot be read, or the glyph
    /// reaches itself. A property of the glyph, so it is kept.
    Broken(Error),
    /// The walk went more than [`MAX_DEPTH`] levels down from the glyph
    /// it started at. A property of that walk, not of the glyphs on the
    /// way, so it is not kept for them.
    TooDeep,
}

/// One glyph's slot in the memo.
#[derive(Debug, Clone, PartialEq)]
enum Slot {
    Unknown,
    /// Being worked out: reaching it again means a cycle.
    Busy,
    Known(Result<Extent, Error>),
}

/// The extents of a bake's glyphs, each worked out once.
pub(super) struct Extents {
    slots: RefCell<Vec<Slot>>,
    /// Draw budget left.
    draws_left: Cell<u64>,
    /// Extents worked out (each glyph at most once, but for walks cut
    /// off by depth).
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

    /// The extent of glyph `gid` drawn on its own.
    pub(super) fn of(&self, cx: &BakeCtx<'_, '_>, gid: u16) -> Result<Extent, NoExtent> {
        self.at_depth(cx, gid, 0)
    }

    /// The extent of glyph `gid`, `depth` levels below the glyph the
    /// walk started at.
    fn at_depth(&self, cx: &BakeCtx<'_, '_>, gid: u16, depth: u8) -> Result<Extent, NoExtent> {
        if depth > MAX_DEPTH {
            return Err(NoExtent::TooDeep);
        }
        let i = usize::from(gid);
        let slot = self.slots.borrow().get(i).cloned();
        match slot {
            // A component past the last glyph draws nothing.
            None => return Ok(Extent::EMPTY),
            Some(Slot::Known(known)) => return known.map_err(NoExtent::Broken),
            Some(Slot::Busy) => {
                return Err(NoExtent::Broken(Error::Malformed {
                    offset: 0,
                    context: "glyf composite reaches itself",
                }))
            }
            Some(Slot::Unknown) => {}
        }
        self.set(i, Slot::Busy);
        self.computed.set(self.computed.get() + 1);
        let found = self.compute(cx, gid, depth);
        // A walk cut off by depth says nothing about this glyph on its
        // own: it is worked out again when reached another way.
        let slot = match &found {
            Ok(extent) => Slot::Known(Ok(*extent)),
            Err(NoExtent::Broken(e)) => Slot::Known(Err(e.clone())),
            Err(NoExtent::TooDeep) => Slot::Unknown,
        };
        self.set(i, slot);
        found
    }

    fn set(&self, i: usize, slot: Slot) {
        if let Some(s) = self.slots.borrow_mut().get_mut(i) {
            *s = slot;
        }
    }

    fn compute(&self, cx: &BakeCtx<'_, '_>, gid: u16, depth: u8) -> Result<Extent, NoExtent> {
        let broken = |context| NoExtent::Broken(Error::Malformed { offset: 0, context });
        let body = match cx.loca.range(gid) {
            Some((s, e)) if s != e => cx
                .glyf_bytes
                .get(s as usize..e as usize)
                .ok_or_else(|| broken("glyf range from loca falls outside glyf table"))?,
            _ => &[][..],
        };
        if body.len() < 10 {
            return Ok(Extent::EMPTY);
        }
        let nc = i16::from_be_bytes([body[0], body[1]]);
        if nc == 0 {
            return Ok(Extent::EMPTY);
        }
        if nc > 0 {
            let glyph =
                SimpleGlyph::decode(body).map_err(|_| broken("glyf simple glyph truncated"))?;
            let points = glyph.points();
            let deltas = cx.deltas(gid, &points, &glyph.end_pts);
            let mut bounds: Option<[f32; 4]> = None;
            for (p, d) in points.iter().zip(&deltas) {
                add_point(&mut bounds, p.0 as f32 + d.0, p.1 as f32 + d.1);
            }
            return Ok(Extent {
                bounds,
                visits: 1,
                points: points.len() as u64,
                height: 0,
            });
        }
        let components =
            read_component_records(body).map_err(|_| broken("glyf composite truncated"))?;
        let mut extent = Extent::EMPTY;
        let mut children = Vec::with_capacity(components.len());
        for c in &components {
            let child = self.at_depth(cx, c.glyph, depth + 1)?;
            extent.visits = extent.visits.saturating_add(child.visits);
            extent.points = extent.points.saturating_add(child.points);
            extent.height = extent.height.max(child.height.saturating_add(1));
            children.push(child);
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
        extent.bounds = self.drawn_bounds(cx, gid, &components, &children, &extent)?;
        Ok(extent)
    }

    /// The bounds of composite `gid`, whose components are not all
    /// placed by an axis-keeping offset, from a core draw while the
    /// budget lasts. Past it, a composite placed by offsets takes the
    /// box around its components' transformed boxes; one placed by
    /// matching points has no exact bounds to give.
    fn drawn_bounds(
        &self,
        cx: &BakeCtx<'_, '_>,
        gid: u16,
        components: &[CompRecord],
        children: &[Extent],
        extent: &Extent,
    ) -> Result<Option<[f32; 4]>, NoExtent> {
        let cost = extent.visits.saturating_add(extent.points);
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
