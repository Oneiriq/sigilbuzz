//! Glyph ink extents for fallback mark positioning, as HarfBuzz's
//! `hb_font_get_glyph_extents` computes them for outline fonts.

use crate::error::{Error, Result};
use crate::face::Face;
use crate::tables::PathOp;

/// HarfBuzz's `hb_glyph_extents_t`, in font design units: the left and
/// top edges, the width, and the height (negative, since y grows up).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Extents {
    pub(super) x_bearing: i32,
    pub(super) y_bearing: i32,
    pub(super) width: i32,
    pub(super) height: i32,
}

/// The extents of glyph `gid`, or `None` when the font has no outline
/// table sigilbuzz reads extents from (only `glyf`, `CFF `, and `CFF2`
/// are consulted; HarfBuzz also asks the bitmap and color tables).
///
/// - `glyf` at the default instance: the glyph header's box, with the
///   `hmtx` left side bearing as the left edge (HarfBuzz follows the
///   rasterizers there), and zero extents for an empty glyph.
/// - `glyf` at other coordinates: the box moved by the glyph's `gvar`
///   deltas (see `Face::glyph_bounds_at_coords`).
/// - `CFF ` and `CFF2`: the box of every outline point, control points
///   included, rounded to whole units, as HarfBuzz's charstring
///   extents are.
pub(super) fn glyph_extents(face: &Face<'_>, coords: &[f32], gid: u16) -> Result<Option<Extents>> {
    let bounds = if coords.is_empty() {
        face.glyph_bounds(gid)
    } else {
        face.glyph_bounds_at_coords(gid, coords)
    };
    match bounds {
        Ok(Some(b)) => {
            let (x_min, x_max) = (b.x_min.min(b.x_max), b.x_min.max(b.x_max));
            let (y_min, y_max) = (b.y_min.min(b.y_max), b.y_min.max(b.y_max));
            let lsb = if coords.is_empty() {
                face.hmtx()?.lsb(gid).unwrap_or(x_min)
            } else {
                x_min
            };
            return Ok(Some(Extents {
                x_bearing: i32::from(lsb),
                y_bearing: i32::from(y_max),
                width: i32::from(x_max) - i32::from(x_min),
                height: i32::from(y_min) - i32::from(y_max),
            }));
        }
        Ok(None) => return Ok(Some(Extents::default())),
        Err(Error::MissingTable { .. }) => {}
        Err(e) => return Err(e),
    }
    let has_cff = face.table_bytes(*b"CFF ").is_ok() || face.table_bytes(*b"CFF2").is_ok();
    if !has_cff {
        return Ok(None);
    }
    let outline = face.glyph_outline_at_coords(gid, coords)?;
    Ok(Some(
        outline.map_or_else(Extents::default, |o| control_box(o.ops())),
    ))
}

/// The rounded box of an outline's points, control points included; a
/// move that starts no segment does not count.
fn control_box(ops: &[PathOp]) -> Extents {
    let mut min = (f32::INFINITY, f32::INFINITY);
    let mut max = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    let mut add = |x: f32, y: f32| {
        min = (min.0.min(x), min.1.min(y));
        max = (max.0.max(x), max.1.max(y));
    };
    let mut pending: Option<(f32, f32)> = None;
    for op in ops {
        let points: &[(f32, f32)] = match *op {
            PathOp::MoveTo { x, y } => {
                pending = Some((x, y));
                continue;
            }
            PathOp::Close => continue,
            PathOp::LineTo { x, y } => &[(x, y)],
            PathOp::QuadTo { cx, cy, x, y } => &[(cx, cy), (x, y)],
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => &[(c1x, c1y), (c2x, c2y), (x, y)],
        };
        if let Some((x, y)) = pending.take() {
            add(x, y);
        }
        for &(x, y) in points {
            add(x, y);
        }
    }
    // HarfBuzz's `roundf`: halves round up.
    let round = crate::tables::parse::hb_round;
    let mut e = Extents::default();
    if min.0 < max.0 {
        e.x_bearing = round(min.0);
        e.width = round(max.0) - e.x_bearing;
    }
    if min.1 < max.1 {
        e.y_bearing = round(max.1);
        e.height = round(min.1) - e.y_bearing;
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_control_box_counts_control_points_and_rounds() {
        let ops = [
            PathOp::MoveTo { x: 10.4, y: 0.0 },
            PathOp::CubicTo {
                c1x: 10.4,
                c1y: 700.6,
                c2x: 300.0,
                c2y: 700.6,
                x: 300.0,
                y: 0.0,
            },
            PathOp::Close,
            // A trailing move draws nothing.
            PathOp::MoveTo { x: 900.0, y: 900.0 },
        ];
        assert_eq!(
            control_box(&ops),
            Extents {
                x_bearing: 10,
                y_bearing: 701,
                width: 290,
                height: -701,
            }
        );
        assert_eq!(control_box(&[]), Extents::default());
    }
}
