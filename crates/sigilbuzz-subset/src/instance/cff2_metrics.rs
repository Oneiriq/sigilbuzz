//! The glyph metrics of a CFF2 instance that follow from its outlines:
//! the left side bearings in `hmtx`, the `head` bounding box, and the
//! `hhea` extremes, as HarfBuzz's instancer works them out.
//!
//! HarfBuzz measures each glyph of the source at the instance's outline
//! coordinates (`update_instance_metrics_map_from_cff2`): the box of
//! every point the outline passes through or pulls toward, curve
//! control points included, before its blends round. The box's ends
//! round halves up, as HarfBuzz's `roundf` (`floor(x + 0.5)`) does. A
//! glyph with a box takes its left end
//! as its left side bearing; one without keeps the source's. The `head`
//! box is the union of the glyphs' boxes, and the `hhea` extremes count
//! every glyph, one without a box as zero wide.
//!
//! The glyphs are drawn through the core's CFF2 outlines right after
//! the bake has run every charstring within its budget, so drawing them
//! again costs the same.

use alloc::vec::Vec;

use sigilbuzz::tables::OutlineSink;
use sigilbuzz::Face;

use super::glyf::clamp_i16;
use super::metrics::HmtxBake;
use crate::hmtx::emit_long_metrics;
use crate::util::round_half_up;

/// A glyph's box as HarfBuzz's `hb_glyph_extents_t` holds it: the left
/// end and width, the top end and (negative) height, each end rounded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Extents {
    pub(super) x_bearing: i32,
    pub(super) width: i32,
    pub(super) y_bearing: i32,
    pub(super) height: i32,
}

impl Extents {
    /// Whether the box says anything: HarfBuzz keeps the source's side
    /// bearing for a glyph whose four values are all zero.
    fn has_bounds(&self) -> bool {
        *self != Self::default()
    }
}

/// Collects the box of an outline the way HarfBuzz's CFF2 extents do:
/// a contour's start point counts once a segment leaves it, then every
/// segment's control and end points.
#[derive(Debug, Clone, Copy)]
struct ControlBox {
    current: (f32, f32),
    open: bool,
    min: (f32, f32),
    max: (f32, f32),
}

impl ControlBox {
    fn new() -> Self {
        Self {
            current: (0.0, 0.0),
            open: false,
            min: (f32::INFINITY, f32::INFINITY),
            max: (f32::NEG_INFINITY, f32::NEG_INFINITY),
        }
    }

    fn add(&mut self, x: f32, y: f32) {
        self.min = (self.min.0.min(x), self.min.1.min(y));
        self.max = (self.max.0.max(x), self.max.1.max(y));
    }

    /// Counts the contour's start point before its first segment.
    fn segment(&mut self) {
        if !self.open {
            self.open = true;
            let (x, y) = self.current;
            self.add(x, y);
        }
    }

    fn extents(&self) -> Extents {
        // HarfBuzz's `roundf`: halves up. It saturates at the `i32`
        // range.
        let (x_bearing, width) = if self.min.0 >= self.max.0 {
            (0, 0)
        } else {
            let left = round_half_up(self.min.0);
            (left, round_half_up(self.max.0).saturating_sub(left))
        };
        let (y_bearing, height) = if self.min.1 >= self.max.1 {
            (0, 0)
        } else {
            let top = round_half_up(self.max.1);
            (top, round_half_up(self.min.1).saturating_sub(top))
        };
        Extents {
            x_bearing,
            width,
            y_bearing,
            height,
        }
    }
}

impl OutlineSink for ControlBox {
    fn move_to(&mut self, x: f32, y: f32) {
        self.open = false;
        self.current = (x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.segment();
        self.current = (x, y);
        self.add(x, y);
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.segment();
        self.add(cx, cy);
        self.current = (x, y);
        self.add(x, y);
    }

    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.segment();
        self.add(c1x, c1y);
        self.add(c2x, c2y);
        self.current = (x, y);
        self.add(x, y);
    }

    fn close(&mut self) {}
}

/// The extents of every glyph of `face`'s `CFF2` at the outline
/// `coords`, in glyph order: `None` for a glyph the core cannot draw,
/// which HarfBuzz leaves out too. `None` for them all when the core
/// cannot read the table.
pub(super) fn cff2_extents(
    face: &Face<'_>,
    coords: &[f32],
    num_glyphs: u16,
) -> Option<Vec<Option<Extents>>> {
    let cff2 = face.cff2().ok()?;
    let mut out = Vec::with_capacity(usize::from(num_glyphs));
    for gid in 0..num_glyphs {
        let mut sink = ControlBox::new();
        let drawn = matches!(cff2.outline(gid, coords, &mut sink), Ok(true));
        out.push(drawn.then(|| sink.extents()));
    }
    Some(out)
}

/// What the outlines of a CFF2 instance set besides its advances.
pub(super) struct Cff2Metrics {
    /// `hmtx` with each glyph's left side bearing from its box.
    pub(super) hmtx: HmtxBake,
    /// The union of the boxes, `(xMin, yMin, xMax, yMax)`, or `None`
    /// when no glyph has one and `head` keeps its box.
    pub(super) head_box: Option<[i16; 4]>,
    /// `hhea`'s largest advance.
    pub(super) max_advance: u16,
    /// `hhea`'s smallest left and right side bearings and largest
    /// extent, or `None` for a font without glyphs.
    pub(super) extremes: Option<[i16; 3]>,
}

/// Sets the left side bearings of `hmtx` (advances baked, bearings the
/// source's) from `extents`, and works out the `head` box and the
/// `hhea` extremes, as HarfBuzz's instancer does for a CFF2 font. A
/// glyph without extents keeps its bearing and counts in neither.
pub(super) fn apply_extents(hmtx: &HmtxBake, extents: &[Option<Extents>]) -> Cff2Metrics {
    let mut lsbs = hmtx.lsbs.clone();
    let mut head: Option<[i32; 4]> = None;
    let mut max_advance: u16 = 0;
    let mut extremes: Option<[i32; 3]> = None;
    for (i, &advance) in hmtx.advances.iter().enumerate() {
        max_advance = max_advance.max(advance);
        let Some(e) = extents.get(i).copied().flatten() else {
            continue;
        };
        let lsb = lsbs.get_mut(i);
        let lsb = match lsb {
            Some(lsb) => {
                if e.has_bounds() {
                    *lsb = clamp_i16(e.x_bearing);
                }
                i32::from(*lsb)
            }
            None => 0,
        };
        if e.has_bounds() {
            let right = e.x_bearing.saturating_add(e.width);
            let bottom = e.y_bearing.saturating_add(e.height);
            let b = head.get_or_insert([e.x_bearing, bottom, right, e.y_bearing]);
            *b = [
                b[0].min(e.x_bearing),
                b[1].min(bottom),
                b[2].max(right),
                b[3].max(e.y_bearing),
            ];
        }
        let rsb = i32::from(advance) - lsb - e.width;
        let extent = lsb + e.width;
        let x = extremes.get_or_insert([lsb, rsb, extent]);
        *x = [x[0].min(lsb), x[1].min(rsb), x[2].max(extent)];
    }
    let (bytes, number_of_h_metrics) = emit_long_metrics(&hmtx.advances, &lsbs);
    Cff2Metrics {
        hmtx: HmtxBake {
            bytes,
            number_of_h_metrics,
            advances: hmtx.advances.clone(),
            lsbs,
        },
        head_box: head.map(|b| b.map(clamp_i16)),
        max_advance,
        extremes: extremes.map(|x| x.map(clamp_i16)),
    }
}

#[cfg(test)]
mod tests;
