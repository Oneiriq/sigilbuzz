//! The glyf and loca bake of a full instance: every simple glyph's
//! points moved by its gvar deltas at the instance coordinates, and
//! the coordinate stream re-encoded.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::tables::Reader;
use sigilbuzz::Face;

use crate::SubsetError;

// ---------------------------------------------------------------------------
// glyf + loca bake
// ---------------------------------------------------------------------------

pub(super) struct GlyfLocaBake {
    pub(super) glyf: Vec<u8>,
    pub(super) loca: Vec<u8>,
    pub(super) long_loca: bool,
}

pub(super) fn bake_glyf_loca(
    face: &Face<'_>,
    coords: &[f32],
    num_glyphs: u16,
) -> Result<GlyfLocaBake, SubsetError> {
    let loca = face.loca().map_err(SubsetError::from)?;
    let glyf_bytes = face.table_bytes(tag::GLYF).map_err(SubsetError::from)?;
    let glyf = face.glyf().map_err(SubsetError::from)?;
    let gvar = face.gvar().map_err(SubsetError::from)?;

    let mut new_bodies: Vec<Vec<u8>> = Vec::with_capacity(num_glyphs as usize);
    for gid in 0..num_glyphs {
        let body = match loca.range(gid) {
            Some((s, e)) if s != e => {
                glyf_bytes
                    .get(s as usize..e as usize)
                    .ok_or(SubsetError::Unsupported(
                        "instance: glyf range falls outside table",
                    ))?
            }
            _ => &[][..],
        };
        let baked = if body.is_empty() {
            Vec::new()
        } else if body.len() < 10 {
            return Err(SubsetError::Unsupported(
                "instance: glyf body shorter than 10 bytes",
            ));
        } else {
            let nc = i16::from_be_bytes([body[0], body[1]]);
            if nc >= 0 {
                // Simple glyph: bake gvar deltas into the contour
                // points, then re-encode.
                let num_points: u16 = glyf
                    .point_count(&loca, gid)
                    .map_err(SubsetError::from)?
                    .unwrap_or_default();
                let deltas = match gvar.as_ref() {
                    Some(g) if !coords.is_empty() => g.glyph_deltas(gid, coords, num_points),
                    _ => Vec::new(),
                };
                bake_simple_glyph(body, &deltas)?
            } else {
                // Composite: pass through verbatim. Component gids do
                // not change (instancing keeps every glyph) so no
                // rewrite is needed. Composite-level gvar deltas are
                // not applied: this is a known limitation of the bake.
                body.to_vec()
            }
        };
        new_bodies.push(baked);
    }

    // Pad each body to 2-byte alignment so short-loca offsets divide
    // cleanly.
    for body in &mut new_bodies {
        if body.len() % 2 != 0 {
            body.push(0);
        }
    }

    // Compute offsets and decide loca format.
    let mut offsets: Vec<u32> = Vec::with_capacity(new_bodies.len() + 1);
    let mut cursor: u32 = 0;
    offsets.push(0);
    for body in &new_bodies {
        cursor = cursor
            .checked_add(body.len() as u32)
            .ok_or(SubsetError::Unsupported("instance: glyf overflow"))?;
        offsets.push(cursor);
    }
    let long_loca = *offsets.last().unwrap_or(&0) > 0x1_FFFE;

    let mut glyf_out = Vec::with_capacity(cursor as usize);
    for body in &new_bodies {
        glyf_out.extend_from_slice(body);
    }
    while glyf_out.len() % 4 != 0 {
        glyf_out.push(0);
    }

    let loca_out = if long_loca {
        let mut out = Vec::with_capacity(offsets.len() * 4);
        for o in &offsets {
            out.extend_from_slice(&o.to_be_bytes());
        }
        out
    } else {
        let mut out = Vec::with_capacity(offsets.len() * 2);
        for o in &offsets {
            let half = (*o / 2) as u16;
            out.extend_from_slice(&half.to_be_bytes());
        }
        out
    };

    Ok(GlyfLocaBake {
        glyf: glyf_out,
        loca: loca_out,
        long_loca,
    })
}

// Simple-glyph flag bits.
pub(super) const FLAG_ON_CURVE: u8 = 0x01;
const FLAG_X_SHORT: u8 = 0x02;
const FLAG_Y_SHORT: u8 = 0x04;
pub(super) const FLAG_REPEAT: u8 = 0x08;
pub(super) const FLAG_X_SAME_OR_POS: u8 = 0x10;
pub(super) const FLAG_Y_SAME_OR_POS: u8 = 0x20;

/// Re-encodes a simple glyph with `deltas` applied to its contour
/// points. The new bbox is recomputed from the baked coordinates.
pub(super) fn bake_simple_glyph(
    body: &[u8],
    deltas: &[sigilbuzz::tables::PointDelta],
) -> Result<Vec<u8>, SubsetError> {
    let mut r = Reader::new(body);
    let nc = r
        .read_i16()
        .map_err(|_| SubsetError::Unsupported("instance: simple header"))?;
    debug_assert!(nc >= 0);
    // Skip the source bbox. We recompute it below.
    r.skip(8)
        .map_err(|_| SubsetError::Unsupported("instance: simple bbox"))?;

    // endPtsOfContours.
    let mut end_pts = Vec::with_capacity(nc as usize);
    for _ in 0..nc {
        end_pts.push(
            r.read_u16()
                .map_err(|_| SubsetError::Unsupported("instance: endPtsOfContours"))?,
        );
    }
    let total_points = end_pts
        .last()
        .copied()
        .map(|e| e.saturating_add(1))
        .unwrap_or(0) as usize;

    // Instructions: read past them. We do not preserve hints in the
    // baked output. They reference the source's `cvt` / `prep` /
    // `fpgm`, which we forward verbatim, but the variable-font deltas
    // mean the hinted grid no longer matches the rasterized outline.
    // Stripping is the safest default and matches what fonttools'
    // instancer does in `--no-recalc-hints` mode.
    let instr_len =
        r.read_u16()
            .map_err(|_| SubsetError::Unsupported("instance: instructionLength"))? as usize;
    r.skip(instr_len)
        .map_err(|_| SubsetError::Unsupported("instance: instructions"))?;

    // Flags with REPEAT expansion.
    let mut flags = Vec::with_capacity(total_points);
    while flags.len() < total_points {
        let f = r
            .read_u8()
            .map_err(|_| SubsetError::Unsupported("instance: flags byte"))?;
        flags.push(f);
        if f & FLAG_REPEAT != 0 {
            let rep = r
                .read_u8()
                .map_err(|_| SubsetError::Unsupported("instance: flag repeat"))?;
            for _ in 0..rep {
                flags.push(f);
                if flags.len() >= total_points {
                    break;
                }
            }
        }
    }
    flags.truncate(total_points);

    // X coords.
    let mut xs: Vec<i32> = Vec::with_capacity(total_points);
    let mut x_cur: i32 = 0;
    for &f in &flags {
        let short = f & FLAG_X_SHORT != 0;
        let same_or_pos = f & FLAG_X_SAME_OR_POS != 0;
        let delta: i32 = if short {
            let v = i32::from(
                r.read_u8()
                    .map_err(|_| SubsetError::Unsupported("instance: x byte"))?,
            );
            if same_or_pos {
                v
            } else {
                -v
            }
        } else if same_or_pos {
            0
        } else {
            i32::from(
                r.read_i16()
                    .map_err(|_| SubsetError::Unsupported("instance: x i16"))?,
            )
        };
        x_cur += delta;
        xs.push(x_cur);
    }

    // Y coords.
    let mut ys: Vec<i32> = Vec::with_capacity(total_points);
    let mut y_cur: i32 = 0;
    for &f in &flags {
        let short = f & FLAG_Y_SHORT != 0;
        let same_or_pos = f & FLAG_Y_SAME_OR_POS != 0;
        let delta: i32 = if short {
            let v = i32::from(
                r.read_u8()
                    .map_err(|_| SubsetError::Unsupported("instance: y byte"))?,
            );
            if same_or_pos {
                v
            } else {
                -v
            }
        } else if same_or_pos {
            0
        } else {
            i32::from(
                r.read_i16()
                    .map_err(|_| SubsetError::Unsupported("instance: y i16"))?,
            )
        };
        y_cur += delta;
        ys.push(y_cur);
    }

    // Apply deltas. gvar's PointDelta vector is sparse: points
    // without an entry pick up zero deltas. Phantom-point deltas (point
    // index >= total_points) influence advances via HVAR rather than
    // contour points, so we ignore them here. When a point appears
    // more than once, its first entry wins. The dense per-point table
    // keeps the lookup linear for glyphs with many points.
    let mut point_deltas: Vec<Option<(f32, f32)>> = alloc::vec![None; total_points];
    for d in deltas {
        if let Some(slot @ None) = point_deltas.get_mut(usize::from(d.point)) {
            *slot = Some((d.dx, d.dy));
        }
    }
    let mut baked_x: Vec<i32> = Vec::with_capacity(total_points);
    let mut baked_y: Vec<i32> = Vec::with_capacity(total_points);
    for ((&x, &y), delta) in xs.iter().zip(&ys).zip(&point_deltas) {
        let mut x = x as f32;
        let mut y = y as f32;
        if let Some((dx, dy)) = *delta {
            x += dx;
            y += dy;
        }
        baked_x.push(round_half_to_even(x));
        baked_y.push(round_half_to_even(y));
    }

    // Recompute bbox from baked points. An empty contour list yields a
    // (0, 0, 0, 0) bbox per the glyf spec convention for whitespace
    // glyphs.
    let (x_min, y_min, x_max, y_max) = if total_points == 0 {
        (0, 0, 0, 0)
    } else {
        let mut xmn = baked_x[0];
        let mut xmx = baked_x[0];
        let mut ymn = baked_y[0];
        let mut ymx = baked_y[0];
        for i in 1..total_points {
            if baked_x[i] < xmn {
                xmn = baked_x[i];
            }
            if baked_x[i] > xmx {
                xmx = baked_x[i];
            }
            if baked_y[i] < ymn {
                ymn = baked_y[i];
            }
            if baked_y[i] > ymx {
                ymx = baked_y[i];
            }
        }
        (
            clamp_i16(xmn),
            clamp_i16(ymn),
            clamp_i16(xmx),
            clamp_i16(ymx),
        )
    };

    // Encode.
    let mut out = Vec::with_capacity(body.len());
    out.extend_from_slice(&nc.to_be_bytes());
    out.extend_from_slice(&x_min.to_be_bytes());
    out.extend_from_slice(&y_min.to_be_bytes());
    out.extend_from_slice(&x_max.to_be_bytes());
    out.extend_from_slice(&y_max.to_be_bytes());
    for &e in &end_pts {
        out.extend_from_slice(&e.to_be_bytes());
    }
    // No instructions.
    out.extend_from_slice(&0u16.to_be_bytes());

    encode_simple_coords(&baked_x, &baked_y, &flags, &mut out);

    Ok(out)
}

/// Encodes the flags + x + y streams for a simple glyph, using the
/// SHORT / SAME-OR-POS encoding the spec defines. Each flag byte
/// derives from the on-curve bit of the input flag stream and the
/// per-point delta size; REPEAT runs collapse identical adjacent
/// flag bytes.
fn encode_simple_coords(xs: &[i32], ys: &[i32], src_flags: &[u8], out: &mut Vec<u8>) {
    let n = xs.len();
    if n == 0 {
        // No flags / x / y stream needed.
        return;
    }

    // Compute per-point deltas + new flag bytes.
    let mut new_flags: Vec<u8> = Vec::with_capacity(n);
    let mut x_payload: Vec<i16> = Vec::with_capacity(n);
    let mut y_payload: Vec<i16> = Vec::with_capacity(n);
    let mut prev_x: i32 = 0;
    let mut prev_y: i32 = 0;
    for i in 0..n {
        let dx = xs[i] - prev_x;
        let dy = ys[i] - prev_y;
        prev_x = xs[i];
        prev_y = ys[i];

        let mut f = src_flags[i] & FLAG_ON_CURVE;

        if dx == 0 {
            f |= FLAG_X_SAME_OR_POS;
        } else if (-255..=255).contains(&dx) {
            f |= FLAG_X_SHORT;
            if dx > 0 {
                f |= FLAG_X_SAME_OR_POS;
                x_payload.push(dx as i16);
            } else {
                x_payload.push(-dx as i16);
            }
        } else {
            // i16 range. Clamp to keep the encoding well-defined; in
            // practice glyf coords always fit because the source font
            // already used i16 deltas.
            let v = dx.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
            x_payload.push(v);
        }

        if dy == 0 {
            f |= FLAG_Y_SAME_OR_POS;
        } else if (-255..=255).contains(&dy) {
            f |= FLAG_Y_SHORT;
            if dy > 0 {
                f |= FLAG_Y_SAME_OR_POS;
                y_payload.push(dy as i16);
            } else {
                y_payload.push(-dy as i16);
            }
        } else {
            let v = dy.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
            y_payload.push(v);
        }

        new_flags.push(f);
    }

    // RLE-compress flags. A run of up to 256 identical flag bytes
    // collapses to `(flag | REPEAT) + count_byte`. Single occurrences
    // emit unchanged.
    let mut i = 0;
    let mut compressed: Vec<u8> = Vec::with_capacity(new_flags.len());
    while i < new_flags.len() {
        let f = new_flags[i];
        let mut j = i + 1;
        while j < new_flags.len() && new_flags[j] == f && (j - i) < 256 {
            j += 1;
        }
        let run = j - i;
        if run >= 2 {
            compressed.push(f | FLAG_REPEAT);
            compressed.push((run - 1) as u8);
        } else {
            compressed.push(f);
        }
        i = j;
    }
    out.extend_from_slice(&compressed);

    // X stream. SHORT entries are unsigned bytes; non-SHORT, non-SAME
    // entries are i16. We walk the original new_flags so the index
    // stays in step with x_payload.
    let mut xpi = 0usize;
    for &f in &new_flags {
        if f & FLAG_X_SHORT != 0 {
            // payload[xpi] is the absolute byte value (0..=255).
            let v = x_payload[xpi];
            out.push(v as u8);
            xpi += 1;
        } else if f & FLAG_X_SAME_OR_POS != 0 {
            // No bytes: same as previous.
        } else {
            let v = x_payload[xpi];
            out.extend_from_slice(&v.to_be_bytes());
            xpi += 1;
        }
    }

    // Y stream.
    let mut ypi = 0usize;
    for &f in &new_flags {
        if f & FLAG_Y_SHORT != 0 {
            let v = y_payload[ypi];
            out.push(v as u8);
            ypi += 1;
        } else if f & FLAG_Y_SAME_OR_POS != 0 {
            // No bytes.
        } else {
            let v = y_payload[ypi];
            out.extend_from_slice(&v.to_be_bytes());
            ypi += 1;
        }
    }
}

fn round_half_to_even(v: f32) -> i32 {
    // Banker's rounding to keep large-N delta sums stable. The spec
    // doesn't mandate a specific rounding mode for instancing, but
    // round-half-to-even is what fonttools' instancer uses, and it
    // matches IEEE 754's default.
    if (v - v.floor() - 0.5).abs() < f32::EPSILON {
        // Only values below 2^23 in magnitude have a fractional part,
        // so `f + 1` cannot overflow.
        let f = v.floor() as i32;
        if f % 2 == 0 {
            f
        } else {
            f + 1
        }
    } else {
        v.round() as i32
    }
}

pub(super) fn clamp_i16(v: i32) -> i16 {
    v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}
