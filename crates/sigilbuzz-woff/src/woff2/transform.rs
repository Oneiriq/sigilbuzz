//! Inverse of the WOFF2 `glyf` / `loca` transform.
//!
//! WOFF2 strips the SFNT `glyf` table apart into eight parallel
//! streams (per-glyph contour count, point count, flag bytes,
//! triplet-encoded coordinates, composite descriptions, bbox
//! metadata, instructions). The unwrapper walks every glyph,
//! re-interleaves those streams into TrueType simple-glyph or
//! composite-glyph records, and emits a fresh `loca` table with the
//! per-glyph offsets.
//!
//! The triplet decoder follows the algorithmic form used by Google's
//! reference `woff2` C++ implementation rather than a 128-entry
//! lookup table. The spec defines the same encoding both ways, but
//! the algorithmic form is unambiguous about sign conventions and
//! the splitting of the 4/4 X/Y nibbles.

use alloc::vec::Vec;

use crate::error::{Result, WoffError};
use crate::reader::Reader;

// TrueType simple-glyph flag bits (`glyf` spec).
const ON_CURVE_POINT: u8 = 0x01;
const X_SHORT_VECTOR: u8 = 0x02;
const Y_SHORT_VECTOR: u8 = 0x04;
const X_IS_SAME_OR_POSITIVE_X_SHORT_VECTOR: u8 = 0x10;
const Y_IS_SAME_OR_POSITIVE_Y_SHORT_VECTOR: u8 = 0x20;
const OVERLAP_SIMPLE: u8 = 0x40;

// Composite-glyph flags (`glyf` spec).
const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
const WE_HAVE_A_SCALE: u16 = 0x0008;
const MORE_COMPONENTS: u16 = 0x0020;
const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
const WE_HAVE_INSTRUCTIONS: u16 = 0x0100;

/// Decodes the next triplet from `glyph_stream`, given the flag byte
/// (low 7 bits = encoding index, high bit = off-curve marker).
///
/// Returns the (dx, dy, on_curve) tuple. Mirrors the
/// `TripletDecode` C++ in Google's reference `woff2` implementation.
fn decode_one_triplet(flag_byte: u8, glyph_stream: &mut Reader<'_>) -> Result<(i16, i16, bool)> {
    let on_curve = (flag_byte >> 7) == 0;
    let flag = flag_byte & 0x7F;

    let n_data_bytes: usize = if flag < 84 {
        1
    } else if flag < 120 {
        2
    } else if flag < 124 {
        3
    } else {
        4
    };
    let buf = glyph_stream.read_bytes(n_data_bytes, "glyph triplet data")?;

    let dx;
    let dy;
    if flag < 10 {
        dx = 0;
        let mag = (i32::from(flag & 0x0E) << 7) + i32::from(buf[0]);
        dy = with_sign(flag, mag);
    } else if flag < 20 {
        let mag = (i32::from((flag - 10) & 0x0E) << 7) + i32::from(buf[0]);
        dx = with_sign(flag, mag);
        dy = 0;
    } else if flag < 84 {
        let b0 = i32::from(flag - 20);
        let b1 = i32::from(buf[0]);
        let mag_x = 1 + (b0 & 0x30) + (b1 >> 4);
        let mag_y = 1 + ((b0 & 0x0C) << 2) + (b1 & 0x0F);
        dx = with_sign(flag, mag_x);
        dy = with_sign(flag >> 1, mag_y);
    } else if flag < 120 {
        let b0 = i32::from(flag - 84);
        let mag_x = 1 + ((b0 / 12) << 8) + i32::from(buf[0]);
        let mag_y = 1 + (((b0 % 12) >> 2) << 8) + i32::from(buf[1]);
        dx = with_sign(flag, mag_x);
        dy = with_sign(flag >> 1, mag_y);
    } else if flag < 124 {
        let b2 = i32::from(buf[1]);
        let mag_x = (i32::from(buf[0]) << 4) + (b2 >> 4);
        let mag_y = ((b2 & 0x0F) << 8) + i32::from(buf[2]);
        dx = with_sign(flag, mag_x);
        dy = with_sign(flag >> 1, mag_y);
    } else {
        let mag_x = (i32::from(buf[0]) << 8) + i32::from(buf[1]);
        let mag_y = (i32::from(buf[2]) << 8) + i32::from(buf[3]);
        dx = with_sign(flag, mag_x);
        dy = with_sign(flag >> 1, mag_y);
    }

    Ok((
        dx.clamp(i16::MIN.into(), i16::MAX.into()) as i16,
        dy.clamp(i16::MIN.into(), i16::MAX.into()) as i16,
        on_curve,
    ))
}

/// Test-only handle on the otherwise-private triplet decoder so the
/// forward encoder in `wrap_transform.rs` can round-trip against it
/// without exposing the helper to the rest of the crate.
#[cfg(test)]
pub(crate) fn __test_decode_one_triplet(
    flag_byte: u8,
    glyph_stream: &mut Reader<'_>,
) -> Result<(i16, i16, bool)> {
    decode_one_triplet(flag_byte, glyph_stream)
}

/// Sign convention from the WOFF2 spec: bit 0 of the relevant flag
/// piece selects the sign. `flag & 1 == 1` means positive, `0` means
/// negative.
const fn with_sign(flag: u8, magnitude: i32) -> i32 {
    if flag & 1 != 0 {
        magnitude
    } else {
        -magnitude
    }
}

/// Reconstructs the SFNT `glyf` and `loca` tables from a transformed
/// WOFF2 glyf payload.
#[allow(clippy::too_many_lines)]
pub(crate) fn reconstruct_glyf_and_loca(payload: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut r = Reader::new(payload);

    let _reserved = r.read_u16("glyf reserved")?;
    let option_flags = r.read_u16("glyf optionFlags")?;
    let num_glyphs = r.read_u16("glyf numGlyphs")? as usize;
    let index_format = r.read_u16("glyf indexFormat")?;
    let n_contour_size = r.read_u32("glyf nContourStreamSize")? as usize;
    let n_points_size = r.read_u32("glyf nPointsStreamSize")? as usize;
    let flag_size = r.read_u32("glyf flagStreamSize")? as usize;
    let glyph_size = r.read_u32("glyf glyphStreamSize")? as usize;
    let composite_size = r.read_u32("glyf compositeStreamSize")? as usize;
    let bbox_size = r.read_u32("glyf bboxStreamSize")? as usize;
    let instruction_size = r.read_u32("glyf instructionStreamSize")? as usize;

    if n_contour_size != num_glyphs * 2 {
        return Err(WoffError::Malformed {
            offset: r.position(),
            context: "glyf nContourStream size != numGlyphs * 2",
        });
    }
    let bbox_bitmap_len = num_glyphs.div_ceil(32) * 4;
    if bbox_size < bbox_bitmap_len {
        return Err(WoffError::Malformed {
            offset: r.position(),
            context: "glyf bboxStream smaller than bitmap",
        });
    }

    // Slice each stream off the payload in declared order. Each
    // stream is pulled by reference; downstream readers walk them
    // independently.
    let n_contour_stream = r.read_bytes(n_contour_size, "glyf nContourStream")?;
    let n_points_stream = r.read_bytes(n_points_size, "glyf nPointsStream")?;
    let flag_stream = r.read_bytes(flag_size, "glyf flagStream")?;
    let glyph_stream = r.read_bytes(glyph_size, "glyf glyphStream")?;
    let composite_stream = r.read_bytes(composite_size, "glyf compositeStream")?;
    let bbox_stream = r.read_bytes(bbox_size, "glyf bboxStream")?;
    let instruction_stream = r.read_bytes(instruction_size, "glyf instructionStream")?;

    let overlap_bitmap = if option_flags & 0x0001 != 0 {
        let len = num_glyphs.div_ceil(8);
        Some(r.read_bytes(len, "glyf overlapSimpleBitmap")?)
    } else {
        None
    };

    let bbox_bitmap = &bbox_stream[..bbox_bitmap_len];
    let bbox_data = &bbox_stream[bbox_bitmap_len..];

    let mut n_points_reader = Reader::new(n_points_stream);
    let mut flag_reader = Reader::new(flag_stream);
    let mut glyph_reader = Reader::new(glyph_stream);
    let mut composite_reader = Reader::new(composite_stream);
    let mut instr_reader = Reader::new(instruction_stream);
    let mut bbox_reader = Reader::new(bbox_data);

    // glyf is 2-byte aligned; build it linearly.
    let mut glyf = Vec::with_capacity(payload.len() * 2);
    let mut offsets: Vec<u32> = Vec::with_capacity(num_glyphs + 1);

    for gid in 0..num_glyphs {
        offsets.push(glyf.len() as u32);

        let n_contours =
            i16::from_be_bytes([n_contour_stream[gid * 2], n_contour_stream[gid * 2 + 1]]);

        let bbox_present = (bbox_bitmap[gid / 8] >> (7 - (gid % 8))) & 1 != 0;

        if n_contours == 0 {
            // Empty glyph: zero-length record. WOFF2 §5.1 forbids
            // an empty glyph from carrying a stored bbox: accepting
            // it silently would leave the bbox stream cursor in the
            // wrong place and corrupt every subsequent glyph's bbox
            // decode (Google's reference woff2 rejects this for the
            // same reason).
            if bbox_present {
                return Err(WoffError::Malformed {
                    offset: 0,
                    context: "empty glyph has bbox bitmap bit set",
                });
            }
            continue;
        }

        if n_contours > 0 {
            // ---- Simple glyph ----
            let stored_bbox = if bbox_present {
                let xn = bbox_reader.read_i16("simple bbox xMin")?;
                let yn = bbox_reader.read_i16("simple bbox yMin")?;
                let xm = bbox_reader.read_i16("simple bbox xMax")?;
                let ym = bbox_reader.read_i16("simple bbox yMax")?;
                Some((xn, yn, xm, ym))
            } else {
                None
            };

            // Per-contour endpoint counts encode the total point
            // count via 255UInt16; convert to glyf-style endPts.
            let n_contours_u = n_contours as usize;
            let mut end_pts: Vec<u16> = Vec::with_capacity(n_contours_u);
            let mut total_points: u32 = 0;
            for _ in 0..n_contours_u {
                let n = u32::from(n_points_reader.read_packed_u16()?);
                total_points = total_points.checked_add(n).ok_or(WoffError::Malformed {
                    offset: 0,
                    context: "point count overflow",
                })?;
                if total_points == 0 {
                    return Err(WoffError::Malformed {
                        offset: 0,
                        context: "simple glyph contour with zero points",
                    });
                }
                end_pts.push((total_points - 1) as u16);
            }

            // Walk the flag + glyph streams in lock-step.
            let mut on_curves: Vec<bool> = Vec::with_capacity(total_points as usize);
            let mut xs: Vec<i16> = Vec::with_capacity(total_points as usize);
            let mut ys: Vec<i16> = Vec::with_capacity(total_points as usize);
            for _ in 0..total_points {
                let f = flag_reader.read_u8("simple flag")?;
                let (dx, dy, on) = decode_one_triplet(f, &mut glyph_reader)?;
                on_curves.push(on);
                xs.push(dx);
                ys.push(dy);
            }

            // Instructions length follows the coords, length is
            // 255UInt16-encoded in glyphStream itself.
            let instr_len = u32::from(glyph_reader.read_packed_u16()?);
            let instructions =
                instr_reader.read_bytes(instr_len as usize, "simple instructions")?;

            // Compute bbox if not stored.
            let (x_min, y_min, x_max, y_max) = if let Some(b) = stored_bbox {
                b
            } else {
                let mut acc_x: i32 = 0;
                let mut acc_y: i32 = 0;
                let mut bx_min = i32::MAX;
                let mut by_min = i32::MAX;
                let mut bx_max = i32::MIN;
                let mut by_max = i32::MIN;
                for i in 0..xs.len() {
                    acc_x += i32::from(xs[i]);
                    acc_y += i32::from(ys[i]);
                    bx_min = bx_min.min(acc_x);
                    by_min = by_min.min(acc_y);
                    bx_max = bx_max.max(acc_x);
                    by_max = by_max.max(acc_y);
                }
                (
                    bx_min.clamp(i16::MIN.into(), i16::MAX.into()) as i16,
                    by_min.clamp(i16::MIN.into(), i16::MAX.into()) as i16,
                    bx_max.clamp(i16::MIN.into(), i16::MAX.into()) as i16,
                    by_max.clamp(i16::MIN.into(), i16::MAX.into()) as i16,
                )
            };

            // Emit TrueType simple-glyph record.
            let glyph_start = glyf.len();
            glyf.extend_from_slice(&n_contours.to_be_bytes());
            glyf.extend_from_slice(&x_min.to_be_bytes());
            glyf.extend_from_slice(&y_min.to_be_bytes());
            glyf.extend_from_slice(&x_max.to_be_bytes());
            glyf.extend_from_slice(&y_max.to_be_bytes());
            for ep in &end_pts {
                glyf.extend_from_slice(&ep.to_be_bytes());
            }
            glyf.extend_from_slice(&(instr_len as u16).to_be_bytes());
            glyf.extend_from_slice(instructions);

            let overlap_first = overlap_bitmap
                .map(|bm| (bm[gid / 8] >> (7 - (gid % 8))) & 1 != 0)
                .unwrap_or(false);

            let (packed_flags, x_bytes, y_bytes) =
                pack_simple_glyph_coords(&on_curves, &xs, &ys, overlap_first);
            glyf.extend_from_slice(&packed_flags);
            glyf.extend_from_slice(&x_bytes);
            glyf.extend_from_slice(&y_bytes);

            if (glyf.len() - glyph_start) % 2 != 0 {
                glyf.push(0);
            }
        } else {
            // ---- Composite glyph ----
            if !bbox_present {
                return Err(WoffError::Malformed {
                    offset: 0,
                    context: "composite glyph missing required bbox",
                });
            }
            let x_min = bbox_reader.read_i16("comp bbox xMin")?;
            let y_min = bbox_reader.read_i16("comp bbox yMin")?;
            let x_max = bbox_reader.read_i16("comp bbox xMax")?;
            let y_max = bbox_reader.read_i16("comp bbox yMax")?;

            let glyph_start = glyf.len();
            glyf.extend_from_slice(&(-1i16).to_be_bytes());
            glyf.extend_from_slice(&x_min.to_be_bytes());
            glyf.extend_from_slice(&y_min.to_be_bytes());
            glyf.extend_from_slice(&x_max.to_be_bytes());
            glyf.extend_from_slice(&y_max.to_be_bytes());

            let mut had_instructions = false;
            loop {
                let flags = composite_reader.read_u16("composite flags")?;
                let glyph_index = composite_reader.read_u16("composite glyphIndex")?;
                glyf.extend_from_slice(&flags.to_be_bytes());
                glyf.extend_from_slice(&glyph_index.to_be_bytes());

                let arg_size = if flags & ARG_1_AND_2_ARE_WORDS != 0 {
                    4
                } else {
                    2
                };
                let args = composite_reader.read_bytes(arg_size, "composite args")?;
                glyf.extend_from_slice(args);

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
                    let xform = composite_reader.read_bytes(xform_size, "composite xform")?;
                    glyf.extend_from_slice(xform);
                }

                if flags & WE_HAVE_INSTRUCTIONS != 0 {
                    had_instructions = true;
                }
                if flags & MORE_COMPONENTS == 0 {
                    break;
                }
            }

            if had_instructions {
                let instr_len = u32::from(glyph_reader.read_packed_u16()?);
                let instructions =
                    instr_reader.read_bytes(instr_len as usize, "composite instructions")?;
                glyf.extend_from_slice(&(instr_len as u16).to_be_bytes());
                glyf.extend_from_slice(instructions);
            }

            if (glyf.len() - glyph_start) % 2 != 0 {
                glyf.push(0);
            }
        }
    }
    offsets.push(glyf.len() as u32);

    // Build loca per indexFormat: short = u16 offset/2; long = u32.
    let loca = if index_format == 0 {
        let mut l = Vec::with_capacity(offsets.len() * 2);
        for o in &offsets {
            if o % 2 != 0 || o / 2 > u32::from(u16::MAX) {
                return Err(WoffError::Malformed {
                    offset: 0,
                    context: "short loca cannot represent glyph offset",
                });
            }
            l.extend_from_slice(&((*o / 2) as u16).to_be_bytes());
        }
        l
    } else {
        let mut l = Vec::with_capacity(offsets.len() * 4);
        for o in &offsets {
            l.extend_from_slice(&o.to_be_bytes());
        }
        l
    };

    Ok((glyf, loca))
}

/// Packs decoded coordinate deltas into the SFNT simple-glyph layout:
/// returns `(flags, x_bytes, y_bytes)` ready to drop into the glyph
/// record.
///
/// We don't try to use the REPEAT compression. Keeping every flag
/// explicit yields a slightly larger glyph but is byte-perfect against
/// ttf-parser, fontTools and other consumers.
fn pack_simple_glyph_coords(
    on_curves: &[bool],
    xs: &[i16],
    ys: &[i16],
    overlap_first: bool,
) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let n = on_curves.len();
    let mut flags: Vec<u8> = Vec::with_capacity(n);
    let mut x_bytes: Vec<u8> = Vec::new();
    let mut y_bytes: Vec<u8> = Vec::new();

    for i in 0..n {
        let mut f: u8 = 0;
        if on_curves[i] {
            f |= ON_CURVE_POINT;
        }
        if i == 0 && overlap_first {
            f |= OVERLAP_SIMPLE;
        }

        let dx = xs[i];
        let dy = ys[i];

        if dx == 0 {
            f |= X_IS_SAME_OR_POSITIVE_X_SHORT_VECTOR;
        } else if (-255..=255).contains(&dx) {
            f |= X_SHORT_VECTOR;
            if dx > 0 {
                f |= X_IS_SAME_OR_POSITIVE_X_SHORT_VECTOR;
            }
            x_bytes.push(dx.unsigned_abs() as u8);
        } else {
            x_bytes.extend_from_slice(&dx.to_be_bytes());
        }

        if dy == 0 {
            f |= Y_IS_SAME_OR_POSITIVE_Y_SHORT_VECTOR;
        } else if (-255..=255).contains(&dy) {
            f |= Y_SHORT_VECTOR;
            if dy > 0 {
                f |= Y_IS_SAME_OR_POSITIVE_Y_SHORT_VECTOR;
            }
            y_bytes.push(dy.unsigned_abs() as u8);
        } else {
            y_bytes.extend_from_slice(&dy.to_be_bytes());
        }

        flags.push(f);
    }

    (flags, x_bytes, y_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_sign_matches_spec() {
        // Odd flag = positive, even flag = negative.
        assert_eq!(with_sign(1, 100), 100);
        assert_eq!(with_sign(0, 100), -100);
        assert_eq!(with_sign(3, 50), 50);
    }

    #[test]
    fn empty_glyph_with_bbox_bit_is_rejected() {
        // WOFF2 spec section 5.1: an empty glyph (nContours == 0)
        // must NOT have a stored bbox. A bbox bitmap claiming
        // otherwise mis-aligns the bbox stream for every subsequent
        // glyph because the empty path used to skip the read. We
        // expose the divergence indirectly: gid 0 is empty + bbox
        // bit set; gid 1 is simple with 1 contour + bbox bit set.
        // Without the reject, gid 1 reads gid 0's "phantom" bbox
        // bytes and decodes a corrupted bbox.
        // Stream sizes:
        //   nContourStream: 2 glyphs * 2 = 4
        //   nPointsStream: 1 contour for gid 1: 255UInt16 of 1 = 1 byte
        //   flagStream: 1 flag byte
        //   glyphStream: 1 triplet (1-byte) + instr_len 0 (1-byte) = 2
        //   compositeStream: empty
        //   bboxStream: bitmap = ceil(2/8) = 1 byte; two bbox * 8 = 16
        //   instructionStream: empty
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u16.to_be_bytes()); // reserved
        payload.extend_from_slice(&0u16.to_be_bytes()); // optionFlags
        payload.extend_from_slice(&2u16.to_be_bytes()); // numGlyphs
        payload.extend_from_slice(&1u16.to_be_bytes()); // indexFormat = long
        let n_contour_size: u32 = 4;
        let n_points_size: u32 = 1;
        let flag_size: u32 = 1;
        let glyph_size: u32 = 2;
        let composite_size: u32 = 0;
        let bbox_size: u32 = 1 + 16;
        let instr_size: u32 = 0;
        payload.extend_from_slice(&n_contour_size.to_be_bytes());
        payload.extend_from_slice(&n_points_size.to_be_bytes());
        payload.extend_from_slice(&flag_size.to_be_bytes());
        payload.extend_from_slice(&glyph_size.to_be_bytes());
        payload.extend_from_slice(&composite_size.to_be_bytes());
        payload.extend_from_slice(&bbox_size.to_be_bytes());
        payload.extend_from_slice(&instr_size.to_be_bytes());

        // nContourStream: gid 0 empty (0), gid 1 simple (1 contour).
        payload.extend_from_slice(&0i16.to_be_bytes());
        payload.extend_from_slice(&1i16.to_be_bytes());
        // nPointsStream: gid 1 has one contour with 1 point. 255UInt16
        // direct value: 1.
        payload.push(1);
        // flagStream: one on-curve point with X_SHORT and dx-only
        // delta. We use flag byte 0x01 (decoded triplet from `< 10`
        // case, dy=0, dx pulled from the next stream byte).
        payload.push(0x01);
        // glyphStream: 1 byte for the triplet decode + 1 byte for
        // the 255UInt16-encoded instruction length (0).
        payload.push(0x00);
        payload.push(0);
        // compositeStream: empty.
        // bboxStream: bitmap byte 0b1100_0000 (gid 0 and gid 1 both
        // present) + 16 bytes of bbox data (gid 0's phantom bbox,
        // then gid 1's real bbox).
        payload.push(0b1100_0000);
        // gid 0 phantom bbox values that are easy to spot if leaked.
        payload.extend_from_slice(&0xDEADu16.to_be_bytes());
        payload.extend_from_slice(&0xDEADu16.to_be_bytes());
        payload.extend_from_slice(&0xDEADu16.to_be_bytes());
        payload.extend_from_slice(&0xDEADu16.to_be_bytes());
        // gid 1 real bbox.
        payload.extend_from_slice(&10i16.to_be_bytes());
        payload.extend_from_slice(&20i16.to_be_bytes());
        payload.extend_from_slice(&30i16.to_be_bytes());
        payload.extend_from_slice(&40i16.to_be_bytes());
        // instructionStream: empty.

        let result = reconstruct_glyf_and_loca(&payload);
        // Per WOFF2 §5.1 the decoder must reject this payload: an
        // empty glyph with a bbox bit set is malformed input.
        assert!(
            result.is_err(),
            "empty glyph with bbox bit set must be rejected; got Ok and the \
             bbox stream cursor would silently mis-align for later glyphs"
        );
    }
}
