//! Simple glyph decoding: contour end points, the flags stream and
//! the delta-encoded x / y coordinate streams.

use alloc::vec::Vec;

use super::{
    Contour, FlatGlyph, FlatPoint, FlattenBudget, PhantomPoints, Variation, FLAG_ON_CURVE,
    FLAG_REPEAT, FLAG_X_SAME_OR_POS, FLAG_X_SHORT, FLAG_Y_SAME_OR_POS, FLAG_Y_SHORT,
};
use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// Number of contour points of a simple glyph whose `endPtsOfContours`
/// starts at `r`: the last end point plus one, or zero for a glyph
/// with no contours.
pub(super) fn simple_point_count(r: &mut Reader<'_>, num_contours: u16) -> Result<usize> {
    if num_contours == 0 {
        return Ok(0);
    }
    r.skip((usize::from(num_contours) - 1) * 2)?;
    Ok(usize::from(r.read_u16()?) + 1)
}

/// Decodes the simple glyph at `glyph`, a reader past the glyph header
/// and the glyph's byte offset in `glyf`, and appends its points, moved,
/// to `out`, in the glyph's own frame: a composite places them after
/// this returns, as HarfBuzz places each component's points. Returns
/// the deltas of the glyph's four phantom points.
///
/// The points move by `deltas` (dense, in point order) or, when `var`
/// is set, by the glyph's own `gvar` deltas with untouched points
/// inferred. `var` carries the glyph id the deltas belong to.
pub(super) fn flatten_simple_glyph(
    glyph: (Reader<'_>, usize),
    num_contours: u16,
    deltas: Option<&[(f32, f32)]>,
    var: Option<(Variation<'_>, u16)>,
    out: &mut FlatGlyph,
    budget: &mut FlattenBudget,
) -> Result<PhantomPoints> {
    let (mut r, offset) = glyph;
    let mut phantom_deltas = [(0.0, 0.0); 4];
    if num_contours == 0 {
        // No points of its own: the tuples move the phantom points only.
        if let Some((v, glyph_id)) = var {
            phantom_deltas = v
                .gvar
                .phantom_deltas(glyph_id, v.coords, 0, &mut budget.work)?;
        }
        return Ok(phantom_deltas);
    }
    // endPtsOfContours.
    let mut end_pts = Vec::with_capacity(num_contours as usize);
    for _ in 0..num_contours {
        end_pts.push(r.read_u16()?);
    }
    let total_points = end_pts.last().map_or(0, |e| e.saturating_add(1));
    budget.take_points(usize::from(total_points), offset)?;

    // instructions: skip.
    let instr_len = r.read_u16()? as usize;
    r.skip(instr_len)?;

    // Flags with REPEAT expansion.
    let mut flags = Vec::with_capacity(total_points as usize);
    while flags.len() < total_points as usize {
        let f = r.read_u8()?;
        flags.push(f);
        if f & FLAG_REPEAT != 0 {
            let rep = r.read_u8()?;
            for _ in 0..rep {
                flags.push(f);
                if flags.len() >= total_points as usize {
                    break;
                }
            }
        }
    }
    flags.truncate(total_points as usize);

    // X coordinates.
    let mut xs = Vec::with_capacity(total_points as usize);
    let mut x_cur: i32 = 0;
    for &f in &flags {
        let short = f & FLAG_X_SHORT != 0;
        let same_or_pos = f & FLAG_X_SAME_OR_POS != 0;
        let delta: i32 = if short {
            let v = i32::from(r.read_u8()?);
            if same_or_pos {
                v
            } else {
                -v
            }
        } else if same_or_pos {
            0
        } else {
            i32::from(r.read_i16()?)
        };
        x_cur += delta;
        xs.push(x_cur);
    }

    // Y coordinates.
    let mut ys = Vec::with_capacity(total_points as usize);
    let mut y_cur: i32 = 0;
    for &f in &flags {
        let short = f & FLAG_Y_SHORT != 0;
        let same_or_pos = f & FLAG_Y_SAME_OR_POS != 0;
        let delta: i32 = if short {
            let v = i32::from(r.read_u8()?);
            if same_or_pos {
                v
            } else {
                -v
            }
        } else if same_or_pos {
            0
        } else {
            i32::from(r.read_i16()?)
        };
        y_cur += delta;
        ys.push(y_cur);
    }

    // gvar deltas need the default points to infer the deltas of the
    // points a tuple skips.
    let varied;
    let deltas = match var {
        Some((v, glyph_id)) => {
            let points: Vec<(i32, i32)> = xs.iter().copied().zip(ys.iter().copied()).collect();
            varied = v.gvar.glyph_point_deltas_with(
                glyph_id,
                v.coords,
                &points,
                &end_pts,
                &mut budget.work,
            )?;
            // The last four are the phantom points'.
            for (slot, d) in phantom_deltas.iter_mut().zip(varied.iter().skip(xs.len())) {
                *slot = *d;
            }
            Some(varied.as_slice())
        }
        None => deltas,
    };

    // Materialize the points with optional deltas, in the glyph's own
    // frame, where gvar moves them.
    let base_idx = out.points.len();
    for (i, &f) in flags.iter().enumerate() {
        let mut x = xs[i] as f32;
        let mut y = ys[i] as f32;
        if let Some(ds) = deltas {
            if let Some(&(dx, dy)) = ds.get(i) {
                x += dx;
                y += dy;
            }
        }
        out.points.push((x, y));
        out.flags.push(FlatPoint {
            on_curve: f & FLAG_ON_CURVE != 0,
        });
    }

    // Per-contour ends, rebased onto the running point count.
    let mut start: usize = 0;
    for &end in &end_pts {
        let end_idx = end as usize;
        if end_idx >= xs.len() || end_idx < start {
            return Err(Error::Malformed {
                offset,
                context: "glyf endPtsOfContours out of range",
            });
        }
        out.contours.push(Contour {
            start: base_idx + start,
            end: base_idx + end_idx,
        });
        start = end_idx + 1;
    }
    Ok(phantom_deltas)
}
