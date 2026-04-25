//! Forward direction of the WOFF2 `glyf` / `loca` transform.
//!
//! The inverse lives in `transform.rs`; this module produces the
//! same eight streams the decoder consumes:
//!
//! 1. n_contour_stream — i16 contour count per glyph.
//! 2. n_points_stream — per-contour point count, 255UInt16.
//! 3. flag_stream — per-point flag byte (low 7 bits = triplet
//!    encoding index, high bit = off-curve).
//! 4. glyph_stream — per-point coordinate triplet data + the
//!    255UInt16-encoded simple-instruction length.
//! 5. composite_stream — composite component records, verbatim.
//! 6. bbox_stream — bbox bitmap + per-glyph bbox bytes.
//! 7. instruction_stream — TrueType instructions (simple + composite).
//! 8. overlap_simple bitmap — bit per simple glyph signalling the
//!    OVERLAP_SIMPLE flag from the SFNT.
//!
//! We always emit a stored bbox for composites (mandatory) and skip
//! it for simple glyphs when the bbox can be re-derived from the
//! deltas. That matches what `transform.rs` decodes — a simple-glyph
//! bbox is stored only when the SFNT bbox doesn't agree with the
//! "sum of deltas" version.

use alloc::vec::Vec;

use crate::error::{Result, WoffError};
use crate::reader::Reader;

// SFNT simple-glyph flag bits (kept in sync with transform.rs).
const ON_CURVE_POINT: u8 = 0x01;
const X_SHORT_VECTOR: u8 = 0x02;
const Y_SHORT_VECTOR: u8 = 0x04;
const REPEAT_FLAG: u8 = 0x08;
const X_IS_SAME_OR_POSITIVE_X_SHORT_VECTOR: u8 = 0x10;
const Y_IS_SAME_OR_POSITIVE_Y_SHORT_VECTOR: u8 = 0x20;
const OVERLAP_SIMPLE: u8 = 0x40;

// Composite-glyph flag bits.
const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
const WE_HAVE_A_SCALE: u16 = 0x0008;
const MORE_COMPONENTS: u16 = 0x0020;
const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
const WE_HAVE_INSTRUCTIONS: u16 = 0x0100;

/// Builds the WOFF2 transformed-glyf payload for the given SFNT
/// `glyf` and `loca` tables.
///
/// `head_table` is consulted for `indexToLocFormat` (short / long
/// loca); `maxp_table` for `numGlyphs`. Both must be present in any
/// SFNT carrying a `glyf` — we error out cleanly if not.
///
/// `retain_hints` controls whether simple-glyph instructions are
/// included in the instruction_stream. Composite-glyph
/// instructions are always preserved structurally (the
/// `WE_HAVE_INSTRUCTIONS` flag in the component record is part of
/// the composite stream); their *body* is dropped when
/// `retain_hints == false`.
pub(crate) fn transform_glyf(
    glyf: &[u8],
    loca: &[u8],
    head_table: Option<&[u8]>,
    maxp_table: Option<&[u8]>,
    retain_hints: bool,
) -> Result<Vec<u8>> {
    let head = head_table.ok_or(WoffError::Malformed {
        offset: 0,
        context: "wrap_woff2: SFNT missing `head` table",
    })?;
    let maxp = maxp_table.ok_or(WoffError::Malformed {
        offset: 0,
        context: "wrap_woff2: SFNT missing `maxp` table",
    })?;

    if head.len() < 54 {
        return Err(WoffError::Malformed {
            offset: 0,
            context: "wrap_woff2: head table too short",
        });
    }
    let index_to_loc_format = u16::from_be_bytes([head[50], head[51]]);
    if maxp.len() < 6 {
        return Err(WoffError::Malformed {
            offset: 0,
            context: "wrap_woff2: maxp too short",
        });
    }
    let num_glyphs = u16::from_be_bytes([maxp[4], maxp[5]]) as usize;

    // Decode loca offsets.
    let offsets = decode_loca(loca, index_to_loc_format, num_glyphs)?;

    // Per-glyph stream buffers.
    let mut n_contour_stream: Vec<u8> = Vec::with_capacity(num_glyphs * 2);
    let mut n_points_stream: Vec<u8> = Vec::new();
    let mut flag_stream: Vec<u8> = Vec::new();
    let mut glyph_stream: Vec<u8> = Vec::new();
    let mut composite_stream: Vec<u8> = Vec::new();
    let mut instruction_stream: Vec<u8> = Vec::new();

    let bbox_bitmap_len = num_glyphs.div_ceil(32) * 4;
    let mut bbox_bitmap: Vec<u8> = alloc::vec![0u8; bbox_bitmap_len];
    let mut bbox_data: Vec<u8> = Vec::new();

    let mut overlap_bitmap: Vec<u8> = alloc::vec![0u8; num_glyphs.div_ceil(8)];
    let mut any_overlap = false;

    for gid in 0..num_glyphs {
        let start = offsets[gid] as usize;
        let end = offsets[gid + 1] as usize;
        if end < start || end > glyf.len() {
            return Err(WoffError::Malformed {
                offset: start,
                context: "wrap_woff2: loca points outside glyf",
            });
        }
        let body = &glyf[start..end];

        if body.is_empty() {
            // Empty glyph: nContour = 0, no bbox, no streams. The
            // unwrap-side rejects empty + bbox bit set, so we leave
            // both bits clear here.
            n_contour_stream.extend_from_slice(&0i16.to_be_bytes());
            continue;
        }

        if body.len() < 10 {
            return Err(WoffError::Malformed {
                offset: start,
                context: "wrap_woff2: glyph header truncated",
            });
        }
        let n_contours = i16::from_be_bytes([body[0], body[1]]);
        let stored_x_min = i16::from_be_bytes([body[2], body[3]]);
        let stored_y_min = i16::from_be_bytes([body[4], body[5]]);
        let stored_x_max = i16::from_be_bytes([body[6], body[7]]);
        let stored_y_max = i16::from_be_bytes([body[8], body[9]]);

        n_contour_stream.extend_from_slice(&n_contours.to_be_bytes());

        if n_contours > 0 {
            // ---- Simple glyph ----
            let info = parse_simple_glyph(&body[10..], n_contours as usize)?;

            // Per-contour point counts: WOFF2 stores the count of
            // points in each contour as a 255UInt16. Reconstruct
            // counts from end-of-contour indices.
            let mut prev_end_plus_one: u32 = 0;
            for &epc in &info.end_pts {
                let n = (u32::from(epc) + 1).checked_sub(prev_end_plus_one).ok_or(
                    WoffError::Malformed {
                        offset: start,
                        context: "wrap_woff2: non-monotonic endPtsOfContours",
                    },
                )?;
                if n == 0 {
                    return Err(WoffError::Malformed {
                        offset: start,
                        context: "wrap_woff2: contour with zero points",
                    });
                }
                if n > u32::from(u16::MAX) {
                    return Err(WoffError::Malformed {
                        offset: start,
                        context: "wrap_woff2: contour > u16::MAX points",
                    });
                }
                write_packed_u16(&mut n_points_stream, n as u16);
                prev_end_plus_one = u32::from(epc) + 1;
            }

            // Compute deltas + per-point on-curve flags, then
            // triplet-encode them.
            //
            // The decoder expects flag_byte high-bit = off-curve.
            // We emit one flag byte and 1..=4 data bytes per point.
            let mut acc_x: i32 = 0;
            let mut acc_y: i32 = 0;
            let mut bx_min = i32::MAX;
            let mut by_min = i32::MAX;
            let mut bx_max = i32::MIN;
            let mut by_max = i32::MIN;

            for i in 0..info.xs.len() {
                let dx = info.xs[i];
                let dy = info.ys[i];
                let on = info.on_curves[i];
                let (flag_byte, data) = encode_triplet(dx, dy, on);
                flag_stream.push(flag_byte);
                glyph_stream.extend_from_slice(&data);

                acc_x += i32::from(dx);
                acc_y += i32::from(dy);
                bx_min = bx_min.min(acc_x);
                by_min = by_min.min(acc_y);
                bx_max = bx_max.max(acc_x);
                by_max = by_max.max(acc_y);
            }

            // Instruction length follows in the glyph_stream as a
            // 255UInt16; the instructions themselves go to the
            // instruction_stream.
            let instr_len = if retain_hints {
                info.instructions.len()
            } else {
                0
            };
            if instr_len > u32::from(u16::MAX) as usize {
                return Err(WoffError::Malformed {
                    offset: start,
                    context: "wrap_woff2: simple glyph instruction len > u16::MAX",
                });
            }
            write_packed_u16(&mut glyph_stream, instr_len as u16);
            if retain_hints {
                instruction_stream.extend_from_slice(info.instructions);
            }

            // Decide whether to store the bbox. WOFF2 §5.1 says the
            // bbox is stored only when it can't be derived from the
            // deltas. The "derived" bbox is just the running sum of
            // deltas (clamped to i16). If the SFNT-stored bbox
            // differs we must store it, otherwise we drop it.
            let derived_x_min = clamp_i16(bx_min);
            let derived_y_min = clamp_i16(by_min);
            let derived_x_max = clamp_i16(bx_max);
            let derived_y_max = clamp_i16(by_max);
            let needs_bbox = stored_x_min != derived_x_min
                || stored_y_min != derived_y_min
                || stored_x_max != derived_x_max
                || stored_y_max != derived_y_max;

            if needs_bbox {
                bbox_bitmap[gid / 8] |= 1 << (7 - (gid % 8));
                bbox_data.extend_from_slice(&stored_x_min.to_be_bytes());
                bbox_data.extend_from_slice(&stored_y_min.to_be_bytes());
                bbox_data.extend_from_slice(&stored_x_max.to_be_bytes());
                bbox_data.extend_from_slice(&stored_y_max.to_be_bytes());
            }

            // OVERLAP_SIMPLE on point 0 propagates to the bitmap.
            if info.overlap_first {
                overlap_bitmap[gid / 8] |= 1 << (7 - (gid % 8));
                any_overlap = true;
            }
        } else if n_contours == -1 {
            // ---- Composite glyph ----
            let body_after_hdr = &body[10..];
            let mut cr = Reader::new(body_after_hdr);
            let mut had_instructions = false;
            loop {
                let flags = cr.read_u16("composite flags")?;
                let glyph_index = cr.read_u16("composite glyph index")?;
                composite_stream.extend_from_slice(&flags.to_be_bytes());
                composite_stream.extend_from_slice(&glyph_index.to_be_bytes());

                let arg_size = if flags & ARG_1_AND_2_ARE_WORDS != 0 {
                    4
                } else {
                    2
                };
                let args = cr.read_bytes(arg_size, "composite args")?;
                composite_stream.extend_from_slice(args);

                let xform_size = if flags & WE_HAVE_A_SCALE != 0 {
                    2
                } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
                    4
                } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
                    8
                } else {
                    0
                };
                if xform_size > 0 {
                    let xform = cr.read_bytes(xform_size, "composite xform")?;
                    composite_stream.extend_from_slice(xform);
                }

                if flags & WE_HAVE_INSTRUCTIONS != 0 {
                    had_instructions = true;
                }
                if flags & MORE_COMPONENTS == 0 {
                    break;
                }
            }
            if had_instructions {
                let instr_len = cr.read_u16("composite instruction len")? as usize;
                let instructions = cr.read_bytes(instr_len, "composite instructions")?;
                let written = if retain_hints { instr_len } else { 0 };
                if written > u32::from(u16::MAX) as usize {
                    return Err(WoffError::Malformed {
                        offset: start,
                        context: "wrap_woff2: composite instruction len > u16::MAX",
                    });
                }
                write_packed_u16(&mut glyph_stream, written as u16);
                if retain_hints {
                    instruction_stream.extend_from_slice(instructions);
                }
            }

            // Composite bbox is mandatory.
            bbox_bitmap[gid / 8] |= 1 << (7 - (gid % 8));
            bbox_data.extend_from_slice(&stored_x_min.to_be_bytes());
            bbox_data.extend_from_slice(&stored_y_min.to_be_bytes());
            bbox_data.extend_from_slice(&stored_x_max.to_be_bytes());
            bbox_data.extend_from_slice(&stored_y_max.to_be_bytes());
        } else {
            // Spec says nContours can be -1 (composite) or > 0
            // (simple) or 0 (empty). Anything else is malformed.
            return Err(WoffError::Malformed {
                offset: start,
                context: "wrap_woff2: nContours other than -1 / 0 / >0",
            });
        }
    }

    // Assemble the transformed glyf payload.
    let mut out: Vec<u8> = Vec::with_capacity(36 + glyf.len());
    out.extend_from_slice(&0u16.to_be_bytes()); // reserved
    let option_flags: u16 = if any_overlap { 0x0001 } else { 0x0000 };
    out.extend_from_slice(&option_flags.to_be_bytes()); // optionFlags
    out.extend_from_slice(&(num_glyphs as u16).to_be_bytes()); // numGlyphs
    out.extend_from_slice(&index_to_loc_format.to_be_bytes()); // indexFormat
    out.extend_from_slice(&(n_contour_stream.len() as u32).to_be_bytes());
    out.extend_from_slice(&(n_points_stream.len() as u32).to_be_bytes());
    out.extend_from_slice(&(flag_stream.len() as u32).to_be_bytes());
    out.extend_from_slice(&(glyph_stream.len() as u32).to_be_bytes());
    out.extend_from_slice(&(composite_stream.len() as u32).to_be_bytes());

    let bbox_total = bbox_bitmap.len() + bbox_data.len();
    out.extend_from_slice(&(bbox_total as u32).to_be_bytes());
    out.extend_from_slice(&(instruction_stream.len() as u32).to_be_bytes());

    out.extend_from_slice(&n_contour_stream);
    out.extend_from_slice(&n_points_stream);
    out.extend_from_slice(&flag_stream);
    out.extend_from_slice(&glyph_stream);
    out.extend_from_slice(&composite_stream);
    out.extend_from_slice(&bbox_bitmap);
    out.extend_from_slice(&bbox_data);
    out.extend_from_slice(&instruction_stream);
    if any_overlap {
        out.extend_from_slice(&overlap_bitmap);
    }

    Ok(out)
}

/// Decoded simple-glyph data: one entry per point, plus the
/// trailing instruction slice and the OVERLAP_SIMPLE bit from
/// point 0.
#[derive(Debug)]
struct SimpleGlyph<'a> {
    end_pts: Vec<u16>,
    instructions: &'a [u8],
    on_curves: Vec<bool>,
    /// Per-point delta-x (signed). The first point is relative to
    /// (0, 0); subsequent points are deltas from the previous point.
    /// This matches what the decoder consumes via triplet decoding.
    xs: Vec<i16>,
    ys: Vec<i16>,
    overlap_first: bool,
}

fn parse_simple_glyph(body: &[u8], n_contours: usize) -> Result<SimpleGlyph<'_>> {
    let mut r = Reader::new(body);

    let mut end_pts: Vec<u16> = Vec::with_capacity(n_contours);
    for _ in 0..n_contours {
        end_pts.push(r.read_u16("endPtsOfContours")?);
    }
    let total_points = if let Some(&last) = end_pts.last() {
        usize::from(last) + 1
    } else {
        0
    };
    if total_points == 0 {
        return Err(WoffError::Malformed {
            offset: 0,
            context: "wrap_woff2: simple glyph with zero contours / points",
        });
    }

    let instr_len = r.read_u16("instructionLength")? as usize;
    let instructions = r.read_bytes(instr_len, "simple instructions")?;

    // Expand the per-point flag array (REPEAT_FLAG compresses runs).
    let mut flags: Vec<u8> = Vec::with_capacity(total_points);
    while flags.len() < total_points {
        let f = r.read_u8("simple flag byte")?;
        let count: usize = if f & REPEAT_FLAG != 0 {
            usize::from(r.read_u8("simple flag repeat count")?) + 1
        } else {
            1
        };
        for _ in 0..count {
            if flags.len() >= total_points {
                return Err(WoffError::Malformed {
                    offset: 0,
                    context: "wrap_woff2: simple flag repeat overflows point count",
                });
            }
            flags.push(f);
        }
    }

    // Walk x, then y.
    let mut xs: Vec<i16> = Vec::with_capacity(total_points);
    for &f in &flags {
        let dx: i16 = if f & X_SHORT_VECTOR != 0 {
            let mag = i16::from(r.read_u8("simple x-short")?);
            if f & X_IS_SAME_OR_POSITIVE_X_SHORT_VECTOR != 0 {
                mag
            } else {
                -mag
            }
        } else if f & X_IS_SAME_OR_POSITIVE_X_SHORT_VECTOR != 0 {
            0
        } else {
            r.read_i16("simple x")?
        };
        xs.push(dx);
    }
    let mut ys: Vec<i16> = Vec::with_capacity(total_points);
    for &f in &flags {
        let dy: i16 = if f & Y_SHORT_VECTOR != 0 {
            let mag = i16::from(r.read_u8("simple y-short")?);
            if f & Y_IS_SAME_OR_POSITIVE_Y_SHORT_VECTOR != 0 {
                mag
            } else {
                -mag
            }
        } else if f & Y_IS_SAME_OR_POSITIVE_Y_SHORT_VECTOR != 0 {
            0
        } else {
            r.read_i16("simple y")?
        };
        ys.push(dy);
    }

    let on_curves: Vec<bool> = flags.iter().map(|f| f & ON_CURVE_POINT != 0).collect();
    let overlap_first = flags.first().is_some_and(|f| f & OVERLAP_SIMPLE != 0);

    Ok(SimpleGlyph {
        end_pts,
        instructions,
        on_curves,
        xs,
        ys,
        overlap_first,
    })
}

fn decode_loca(loca: &[u8], index_format: u16, num_glyphs: usize) -> Result<Vec<u32>> {
    let n = num_glyphs + 1;
    let mut out: Vec<u32> = Vec::with_capacity(n);
    if index_format == 0 {
        if loca.len() < n * 2 {
            return Err(WoffError::Malformed {
                offset: 0,
                context: "wrap_woff2: short loca too small",
            });
        }
        for i in 0..n {
            let v = u16::from_be_bytes([loca[i * 2], loca[i * 2 + 1]]);
            out.push(u32::from(v) * 2);
        }
    } else {
        if loca.len() < n * 4 {
            return Err(WoffError::Malformed {
                offset: 0,
                context: "wrap_woff2: long loca too small",
            });
        }
        for i in 0..n {
            let v = u32::from_be_bytes([
                loca[i * 4],
                loca[i * 4 + 1],
                loca[i * 4 + 2],
                loca[i * 4 + 3],
            ]);
            out.push(v);
        }
    }
    Ok(out)
}

// -----------------------------------------------------------------------------
// Triplet encoder (inverse of `transform.rs::decode_one_triplet`).
// -----------------------------------------------------------------------------

/// Forward triplet encoding. Returns `(flag_byte, data_bytes)`.
///
/// The flag byte's high bit signals off-curve; the low 7 bits index
/// into the same 128-entry encoding the spec uses.
///
/// We pick the smallest encoding that covers the (dx, dy) pair.
/// Worst-case fallback is the 4-byte encoding (flags 124..128) which
/// can represent any i16 pair.
fn encode_triplet(dx: i16, dy: i16, on_curve: bool) -> (u8, Vec<u8>) {
    let off_curve_bit: u8 = if on_curve { 0 } else { 0x80 };

    // Range 1: dx == 0, |dy| <= 1279 (mag_hi in 0..=4, low byte 0..=255).
    // The decoder computes mag = ((flag >> 1) & 7) * 256 + data[0]
    // with flag in 0..=9, so mag_hi is capped at 4.
    if dx == 0 && dy.abs() <= 1279 {
        let abs = dy.unsigned_abs();
        let sign_bit: u8 = if dy >= 0 { 1 } else { 0 };
        let mag_low = (abs & 0xFF) as u8;
        let mag_hi = ((abs >> 8) & 0x07) as u8; // 0..=4 by abs cap above
        let flag = (mag_hi << 1) | sign_bit;
        return (off_curve_bit | flag, alloc::vec![mag_low]);
    }

    // Range 2: dy == 0, |dx| <= 1279. flag in 10..=19, decoder uses
    // (flag - 10) >> 1 for mag_hi.
    if dy == 0 && dx.abs() <= 1279 {
        let abs = dx.unsigned_abs();
        let sign_bit: u8 = if dx >= 0 { 1 } else { 0 };
        let mag_low = (abs & 0xFF) as u8;
        let mag_hi = ((abs >> 8) & 0x07) as u8;
        let flag = 10 + ((mag_hi << 1) | sign_bit);
        return (off_curve_bit | flag, alloc::vec![mag_low]);
    }

    // Range 3: small both. The decoder for flag in 20..84:
    //   b0 = flag - 20  (range 0..64)
    //   b1 = data[0]
    //   mag_x = 1 + (b0 & 0x30) + (b1 >> 4)      // 1..=64 (b0&0x30 in {0,16,32,48}, low nib 0..15)
    //   mag_y = 1 + ((b0 & 0x0C) << 2) + (b1 & 0x0F)  // 1..=64
    //   sign_x = flag & 1
    //   sign_y = (flag >> 1) & 1
    // So |dx|, |dy| in 1..=64.
    let abs_x = dx.unsigned_abs();
    let abs_y = dy.unsigned_abs();
    if (1..=64).contains(&abs_x) && (1..=64).contains(&abs_y) {
        let mag_x = (abs_x - 1) as u8; // 0..=63
        let mag_y = (abs_y - 1) as u8;
        // mag_x = (b0 & 0x30) + (b1 >> 4); pick b0_high = mag_x & 0x30 (high 2 bits of 6-bit mag) and b1_high = mag_x & 0x0F.
        let b0_high = mag_x & 0x30; // bits 4..5 of mag_x
        let b1_high = mag_x & 0x0F; // bits 0..3 of mag_x
        let b0_low = (mag_y & 0x30) >> 2; // mag_y bits 4..5 -> b0 bits 2..3
        let b1_low = mag_y & 0x0F;
        let b0 = b0_high | b0_low; // 0..63
        let b1 = (b1_high << 4) | b1_low;
        let sign_x: u8 = if dx >= 0 { 1 } else { 0 };
        let sign_y: u8 = if dy >= 0 { 1 } else { 0 };
        let flag = 20 + b0 + (sign_y << 1) + sign_x;
        // The decoder uses `flag >> 1` for sign_y and `flag & 1` for
        // sign_x, but the multiplexed magnitudes still come from
        // `flag - 20` which we precomputed as `b0`. Add the sign
        // bits *after* — wait, we need to verify. The decoder does:
        //   b0 = flag - 20
        //   mag_x = 1 + (b0 & 0x30) + (b1 >> 4)   -> b0 bits 4..5
        //   mag_y = 1 + ((b0 & 0x0C) << 2) + (b1 & 0x0F) -> b0 bits 2..3
        //   sign_x = flag & 1
        //   sign_y = (flag >> 1) & 1
        // So `b0` of the decoder includes ALL 6 bits of (flag - 20).
        // The low 2 bits of `b0` (from `flag - 20`) are `(flag - 20) & 3`,
        // which equal `flag & 3` only when flag is in [20, 23] etc.
        // But the decoder's `b0 & 0x0C` *masks out* the low 2 bits of
        // b0 and keeps bits 2..3, so the sign bits don't pollute mag.
        // Symmetrically, the encoder must put sign bits in bits 0..1
        // of `(flag - 20)`. We did: flag = 20 + b0 + (sign_y<<1) + sign_x,
        // but b0 already contained `b0_low = (mag_y & 0x30) >> 2`
        // which lands in bits 2..3, and `b0_high` in bits 4..5. So
        // the low 2 bits of (flag - 20) come purely from sign — good.
        // Reject any case where b0's low 2 bits weren't already 0.
        debug_assert_eq!(b0 & 0x03, 0);
        return (off_curve_bit | flag, alloc::vec![b1]);
    }

    // Range 4: medium, flag in 84..120. Decoder:
    //   b0 = flag - 84  (0..=35)
    //   mag_x = 1 + ((b0 / 12) << 8) + data[0]          -> 1..=767
    //   mag_y = 1 + (((b0 % 12) >> 2) << 8) + data[1]   -> 1..=767
    //   b0 / 12      in 0..=2  (high 2 bits of mag_x)
    //   (b0 % 12) >> 2 in 0..=2  (high 2 bits of mag_y)
    //   sign_x = flag & 1, sign_y = (flag >> 1) & 1
    if (1..=767).contains(&abs_x) && (1..=767).contains(&abs_y) {
        let abs_x_minus_1 = abs_x - 1; // 0..=766
        let hi_x = abs_x_minus_1 >> 8; // 0..=2
        let lo_x = (abs_x_minus_1 & 0xFF) as u8;
        let abs_y_minus_1 = abs_y - 1; // 0..=766
        let hi_y = abs_y_minus_1 >> 8; // 0..=2
        let lo_y = (abs_y_minus_1 & 0xFF) as u8;

        let sign_x: u8 = if dx >= 0 { 1 } else { 0 };
        let sign_y: u8 = if dy >= 0 { 1 } else { 0 };
        // b0 = hi_x*12 + hi_y*4 + sign_y*2 + sign_x.
        // Max: 2*12 + 2*4 + 2 + 1 = 35. flag = 84+35 = 119 < 120. OK.
        let b0 = (hi_x as u8) * 12 + ((hi_y as u8) << 2) + (sign_y << 1) + sign_x;
        let flag = 84 + b0;
        return (off_curve_bit | flag, alloc::vec![lo_x, lo_y]);
    }

    // Range 5: 12-bit each, flag in 120..124. Decoder:
    //   data = [b0, b1, b2]
    //   mag_x = (b0 << 4) + (b1 >> 4)              -> 0..=4095
    //   mag_y = ((b1 & 0x0F) << 8) + b2            -> 0..=4095
    //   sign_x = flag & 1, sign_y = (flag >> 1) & 1
    if abs_x <= 4095 && abs_y <= 4095 {
        let b0 = ((abs_x >> 4) & 0xFF) as u8;
        let b1_hi = ((abs_x & 0x0F) << 4) as u8;
        let b1_lo = ((abs_y >> 8) & 0x0F) as u8;
        let b1 = b1_hi | b1_lo;
        let b2 = (abs_y & 0xFF) as u8;
        let sign_x: u8 = if dx >= 0 { 1 } else { 0 };
        let sign_y: u8 = if dy >= 0 { 1 } else { 0 };
        let flag = 120 + (sign_y << 1) + sign_x;
        return (off_curve_bit | flag, alloc::vec![b0, b1, b2]);
    }

    // Range 6: full i16 each, flag in 124..128.
    //   data = [b0, b1, b2, b3]
    //   mag_x = (b0 << 8) + b1   -> 0..=65535
    //   mag_y = (b2 << 8) + b3
    //   sign_x = flag & 1, sign_y = (flag >> 1) & 1
    let sign_x: u8 = if dx >= 0 { 1 } else { 0 };
    let sign_y: u8 = if dy >= 0 { 1 } else { 0 };
    let flag = 124 + (sign_y << 1) + sign_x;
    let abs_x_u = abs_x;
    let abs_y_u = abs_y;
    (
        off_curve_bit | flag,
        alloc::vec![
            (abs_x_u >> 8) as u8,
            (abs_x_u & 0xFF) as u8,
            (abs_y_u >> 8) as u8,
            (abs_y_u & 0xFF) as u8,
        ],
    )
}

fn write_packed_u16(out: &mut Vec<u8>, value: u16) {
    // Inverse of `Reader::read_packed_u16`:
    //   value < 253          → 1 byte: value as u8
    //   value in 253..506    → 2 bytes: 255, (value - 253)
    //   value in 506..759    → 2 bytes: 254, (value - 506)
    //   value >= 759         → 3 bytes: 253, hi, lo
    if value < 253 {
        out.push(value as u8);
    } else if value < 506 {
        out.push(255);
        out.push((value - 253) as u8);
    } else if value < 759 {
        out.push(254);
        out.push((value - 506) as u8);
    } else {
        out.push(253);
        out.extend_from_slice(&value.to_be_bytes());
    }
}

const fn clamp_i16(v: i32) -> i16 {
    if v > i16::MAX as i32 {
        i16::MAX
    } else if v < i16::MIN as i32 {
        i16::MIN
    } else {
        v as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::Reader;

    /// Round-trip every `(dx, dy)` pair in a small sweep through the
    /// encoder + decoder pair.
    #[test]
    fn triplet_encode_decode_round_trip() {
        let cases: &[(i16, i16)] = &[
            (0, 0),
            (0, 1),
            (1, 0),
            (-1, 0),
            (0, -1),
            (10, -10),
            (64, 64),
            (-64, -64),
            (65, 0),
            (0, 65),
            (200, -300),
            (1280, 0),
            (0, -1280),
            (1281, 0),
            (511, 100),
            (4095, 4095),
            (-4095, -4095),
            (5000, 5000),
            (i16::MAX, i16::MIN),
            (i16::MIN, i16::MAX),
        ];
        for &(dx, dy) in cases {
            for &on in &[true, false] {
                let (flag_byte, data) = encode_triplet(dx, dy, on);
                let mut buf: Vec<u8> = Vec::new();
                buf.extend_from_slice(&data);
                let mut r = Reader::new(&buf);
                let (rdx, rdy, ron) =
                    super::super::transform::__test_decode_one_triplet(flag_byte, &mut r)
                        .expect("triplet decodes");
                assert_eq!(
                    (rdx, rdy, ron),
                    (dx, dy, on),
                    "roundtrip mismatch for ({dx}, {dy}, on={on}); flag={flag_byte:#x} data={data:?}"
                );
            }
        }
    }

    #[test]
    fn packed_u16_round_trip() {
        for &v in &[0u16, 1, 252, 253, 254, 505, 506, 758, 759, 1000, u16::MAX] {
            let mut buf: Vec<u8> = Vec::new();
            write_packed_u16(&mut buf, v);
            let mut r = Reader::new(&buf);
            let decoded = r.read_packed_u16().expect("decodes");
            assert_eq!(decoded, v, "mismatch for {v}");
        }
    }
}
