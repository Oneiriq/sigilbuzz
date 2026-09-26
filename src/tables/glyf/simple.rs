//! Simple glyph decoding: contour end points, the flags stream and
//! the delta-encoded x / y coordinate streams.

use alloc::vec::Vec;

use super::{
    Contour, FlatGlyph, FlatPoint, Transform, FLAG_ON_CURVE, FLAG_REPEAT, FLAG_X_SAME_OR_POS,
    FLAG_X_SHORT, FLAG_Y_SAME_OR_POS, FLAG_Y_SHORT,
};
use crate::error::{Error, Result};
use crate::tables::parse::Reader;

pub(super) fn flatten_simple_glyph(
    r: &mut Reader<'_>,
    num_contours: u16,
    deltas: Option<&[(f32, f32)]>,
    tf: &Transform,
    out: &mut FlatGlyph,
) -> Result<()> {
    if num_contours == 0 {
        return Ok(());
    }
    // endPtsOfContours.
    let mut end_pts = Vec::with_capacity(num_contours as usize);
    for _ in 0..num_contours {
        end_pts.push(r.read_u16()?);
    }
    let total_points = end_pts
        .last()
        .copied()
        .map(|e| e.saturating_add(1))
        .unwrap_or(0);

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

    // Materialize absolute, transformed points with optional deltas.
    // Deltas live in design-unit space and apply *before* the
    // composite transform: gvar feeds them into the simple-glyph
    // coord stream, so they share the glyph's own frame.
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
        let (tx, ty) = tf.apply(x, y);
        out.points.push((tx, ty));
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
                offset: 0,
                context: "glyf endPtsOfContours out of range",
            });
        }
        out.contours.push(Contour {
            start: base_idx + start,
            end: base_idx + end_idx,
        });
        start = end_idx + 1;
    }
    Ok(())
}
