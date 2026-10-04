//! Adversarial inputs that must surface as structured `BadJpeg`
//! errors rather than panics.

use super::progressive::build_progressive_grayscale_jpeg;
use super::*;
use crate::jpeg_decode::idct::{idct_cos_table, idct_with_table};

// ---------------------------------------------------------------
// Adversarial marker coverage. SOF2 progressive *is* supported
// (#241), but the surrounding non-baseline rejection
// surface still has bite:
//
//   - Baseline (SOF0) still decodes (covered above).
//   - SOF1 (extended sequential): non-baseline, still rejected.
//   - SOF3 (lossless): non-baseline, still rejected.
//   - SOF0-SOS scan params with Ss != 0 must reject (the
//     baseline path doesn't morph into a progressive scanner
//     just because Ss looks progressive).
//   - 0xFF padding then EOF must error structured, not panic.
// ---------------------------------------------------------------

#[test]
fn marker_walker_rejects_extended_sequential_sof1() {
    // SOI then SOF1 (extended sequential). Same body shape as
    // SOF0 but with marker 0xC1.
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xC1];
    bytes.extend_from_slice(&8u16.to_be_bytes()); // length
    bytes.extend_from_slice(&[8, 0, 8, 0, 8, 0]); // dummy body
    let err = decode_jpeg(&bytes).unwrap_err();
    assert!(
        matches!(err, RenderError::BadJpeg("non-baseline SOF unsupported")),
        "SOF1 must surface as non-baseline, got {err:?}"
    );
}

#[test]
fn marker_walker_rejects_lossless_sof3() {
    // SOF3 is lossless, also non-baseline.
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xC3];
    bytes.extend_from_slice(&8u16.to_be_bytes());
    bytes.extend_from_slice(&[8, 0, 8, 0, 8, 0]);
    let err = decode_jpeg(&bytes).unwrap_err();
    assert!(matches!(
        err,
        RenderError::BadJpeg("non-baseline SOF unsupported")
    ));
}

/// Adversarial: SOS spectrum-selection start byte non-zero. This
/// is what a *progressive* DC-first scan would carry (Ss=0, Se=0)
/// or an AC scan (Ss=1, Se=63). Either flavor must surface as
/// "non-baseline scan parameters" rather than continuing into the
/// entropy decoder with bogus state.
#[test]
fn marker_walker_rejects_progressive_scan_params() {
    // Build a minimal SOI + SOF0 + DQT + DHT + SOS with Ss=1.
    // We can't easily run the full pipeline here, but the SOS
    // tail check fires before any entropy work.
    let mut bytes = vec![0xFF, 0xD8];
    // SOF0: 8x8 grayscale, qt=0
    bytes.extend_from_slice(&[0xFF, 0xC0]);
    bytes.extend_from_slice(&11u16.to_be_bytes());
    bytes.push(8); // precision
    bytes.extend_from_slice(&8u16.to_be_bytes()); // height
    bytes.extend_from_slice(&8u16.to_be_bytes()); // width
    bytes.push(1); // n_comp
    bytes.extend_from_slice(&[1, 0x11, 0]); // id, sampling, qt
                                            // DQT: identity 8-bit, dest 0
    bytes.extend_from_slice(&[0xFF, 0xDB]);
    bytes.extend_from_slice(&67u16.to_be_bytes());
    bytes.push(0x00);
    bytes.extend_from_slice(&[1u8; 64]);
    // DHT: minimal DC table (0 codes total -> empty), class 0 dest 0
    bytes.extend_from_slice(&[0xFF, 0xC4]);
    bytes.extend_from_slice(&19u16.to_be_bytes());
    bytes.push(0x00);
    bytes.extend_from_slice(&[0u8; 16]);
    // DHT: minimal AC table, class 1 dest 0
    bytes.extend_from_slice(&[0xFF, 0xC4]);
    bytes.extend_from_slice(&19u16.to_be_bytes());
    bytes.push(0x10);
    bytes.extend_from_slice(&[0u8; 16]);
    // SOS with Ss=1 (progressive AC scan signature).
    bytes.extend_from_slice(&[0xFF, 0xDA]);
    bytes.extend_from_slice(&8u16.to_be_bytes());
    bytes.push(1); // n_scan
    bytes.extend_from_slice(&[1, 0x00]); // comp id, td/ta=0
    bytes.extend_from_slice(&[1, 63, 0]); // Ss=1 (bad), Se, Ah/Al
    let err = decode_jpeg(&bytes).unwrap_err();
    assert!(
        matches!(err, RenderError::BadJpeg("non-baseline scan parameters")),
        "Ss=1 must surface as non-baseline scan params, got {err:?}"
    );
}

/// Adversarial: SOS with Ah/Al non-zero, the successive-
/// approximation refinement-bit field used by progressive scans.
/// Must reject with the same structured error.
#[test]
fn marker_walker_rejects_refinement_bit_field() {
    // Reuse minimal harness from the prior test but flip Ah/Al.
    let mut bytes = vec![0xFF, 0xD8];
    bytes.extend_from_slice(&[0xFF, 0xC0]);
    bytes.extend_from_slice(&11u16.to_be_bytes());
    bytes.push(8);
    bytes.extend_from_slice(&8u16.to_be_bytes());
    bytes.extend_from_slice(&8u16.to_be_bytes());
    bytes.push(1);
    bytes.extend_from_slice(&[1, 0x11, 0]);
    bytes.extend_from_slice(&[0xFF, 0xDB]);
    bytes.extend_from_slice(&67u16.to_be_bytes());
    bytes.push(0x00);
    bytes.extend_from_slice(&[1u8; 64]);
    bytes.extend_from_slice(&[0xFF, 0xC4]);
    bytes.extend_from_slice(&19u16.to_be_bytes());
    bytes.push(0x00);
    bytes.extend_from_slice(&[0u8; 16]);
    bytes.extend_from_slice(&[0xFF, 0xC4]);
    bytes.extend_from_slice(&19u16.to_be_bytes());
    bytes.push(0x10);
    bytes.extend_from_slice(&[0u8; 16]);
    bytes.extend_from_slice(&[0xFF, 0xDA]);
    bytes.extend_from_slice(&8u16.to_be_bytes());
    bytes.push(1);
    bytes.extend_from_slice(&[1, 0x00]);
    bytes.extend_from_slice(&[0, 63, 0x11]); // Ss=0, Se=63, Ah/Al=0x11
    let err = decode_jpeg(&bytes).unwrap_err();
    assert!(
        matches!(err, RenderError::BadJpeg("non-baseline scan parameters")),
        "Ah/Al non-zero must reject, got {err:?}"
    );
}

/// Adversarial: arbitrary 0xFF padding before a marker is allowed
/// by spec; the marker walker should not get stuck in the skip
/// loop nor panic on EOF inside it.
#[test]
fn marker_walker_handles_ff_padding_then_eof() {
    let bytes = vec![0xFF, 0xD8, 0xFF, 0xFF, 0xFF, 0xFF];
    let err = decode_jpeg(&bytes).unwrap_err();
    // Acceptable: any structured BadJpeg. The point is no panic.
    assert!(matches!(err, RenderError::BadJpeg(_)), "got {err:?}");
}

/// Index of the marker code byte that follows the first `0xFF m`.
fn marker_pos(bytes: &[u8], m: u8) -> usize {
    bytes
        .windows(2)
        .position(|w| w == [0xFF, m])
        .map(|p| p + 1)
        .expect("marker present")
}

#[test]
fn sos_dc_table_selector_past_the_table_slots_is_an_error() {
    // Mirrors a fuzzer crash: the third SOS component names DC table
    // 9, but only slots 0..=3 exist. This used to index out of
    // bounds. SOS layout after the marker: length (2), count, then
    // (id, Td/Ta) pairs.
    let mut bytes = build_constant_jpeg(0, 0, 0);
    let sos = marker_pos(&bytes, MARKER_SOS);
    bytes[sos + 9] = 0x91;
    assert_eq!(
        decode_jpeg(&bytes).unwrap_err(),
        RenderError::BadJpeg("missing DC Huffman table")
    );
}

#[test]
fn sos_ac_table_selector_past_the_table_slots_is_an_error() {
    let mut bytes = build_constant_jpeg(0, 0, 0);
    let sos = marker_pos(&bytes, MARKER_SOS);
    bytes[sos + 5] = 0x0C;
    assert_eq!(
        decode_jpeg(&bytes).unwrap_err(),
        RenderError::BadJpeg("missing AC Huffman table")
    );
}

#[test]
fn progressive_dc_table_selector_past_the_table_slots_is_an_error() {
    let mut bytes = build_progressive_grayscale_jpeg(0);
    let sos = marker_pos(&bytes, MARKER_SOS);
    bytes[sos + 5] = 0xF0;
    assert_eq!(
        decode_jpeg(&bytes).unwrap_err(),
        RenderError::BadJpeg("missing DC Huffman table")
    );
}

#[test]
fn sos_before_sof_is_an_error() {
    // A zero-component scan with no frame header used to reach the
    // YCbCr composer with no components and index out of bounds.
    let bytes = [
        0xFF, MARKER_SOI, 0xFF, MARKER_SOS, 0x00, 0x06, 0x00, 0x00, 0x3F, 0x00, 0xFF, MARKER_EOI,
    ];
    assert_eq!(
        decode_jpeg(&bytes).unwrap_err(),
        RenderError::BadJpeg("SOS before SOF")
    );
}

/// Baseline grayscale stream of `blocks` 8x8 blocks in one row.
/// Every block carries the largest DC difference (+32767) and an
/// EOB, and the quantizer is 255, so the running DC predictor
/// times the quantizer leaves `i32` range after 258 blocks.
fn build_growing_dc_jpeg(blocks: u16) -> Vec<u8> {
    let mut out = vec![0xFF, MARKER_SOI];
    out.extend_from_slice(&[0xFF, MARKER_DQT, 0x00, 67, 0x00]);
    out.extend_from_slice(&[255u8; 64]);
    out.extend_from_slice(&[0xFF, MARKER_SOF0, 0x00, 11, 8]);
    out.extend_from_slice(&8u16.to_be_bytes());
    out.extend_from_slice(&(blocks * 8).to_be_bytes());
    out.extend_from_slice(&[1, 1, 0x11, 0]);
    // DC table 0: one 1-bit code for magnitude 15. AC table 0: one
    // 1-bit code for EOB.
    for (class, symbol) in [(0x00u8, 15u8), (0x10, 0x00)] {
        out.extend_from_slice(&[0xFF, MARKER_DHT, 0x00, 20, class, 1]);
        out.extend_from_slice(&[0u8; 15]);
        out.push(symbol);
    }
    out.extend_from_slice(&[0xFF, MARKER_SOS, 0x00, 8, 1, 1, 0x00, 0, 63, 0]);
    let mut bw = BitWriter::default();
    for _ in 0..blocks {
        bw.write_bits(0, 1); // DC code: magnitude 15
        bw.write_bits(0x7FFF, 15); // +32767
        bw.write_bits(0, 1); // EOB
    }
    bw.flush();
    out.extend_from_slice(&bw.bytes);
    out.extend_from_slice(&[0xFF, MARKER_EOI]);
    out
}

#[test]
fn growing_dc_predictor_wraps_instead_of_overflowing() {
    // Used to panic in debug builds with "attempt to multiply with
    // overflow" once the predictor passed 2^31 / 255.
    let pix = decode_jpeg(&build_growing_dc_jpeg(300)).expect("decodes");
    assert_eq!((pix.width, pix.height), (2400, 8));
}

/// SOI, one 8-bit quantization table, and a frame header of the
/// given size and marker, followed by `tail`.
fn frame_only(marker: u8, width: u16, height: u16, tail: &[u8]) -> Vec<u8> {
    let mut out = vec![0xFF, MARKER_SOI];
    out.extend_from_slice(&[0xFF, MARKER_DQT, 0x00, 67, 0x00]);
    out.extend_from_slice(&[1u8; 64]);
    out.extend_from_slice(&[0xFF, marker, 0x00, 11, 8]);
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&[1, 1, 0x11, 0]);
    out.extend_from_slice(tail);
    out
}

#[test]
fn frame_larger_than_entropy_data_is_rejected_before_allocating() {
    // Mirrors a fuzzer timeout: a 9731x4103 frame backed by a few
    // hundred bytes used to allocate the full planes and decode
    // zero-padded blocks for seconds.
    let tail = [0u8; 400];
    for marker in [MARKER_SOF0, MARKER_SOF2] {
        for (w, h) in [(9731, 4103), (16384, 16384)] {
            assert_eq!(
                decode_jpeg(&frame_only(marker, w, h, &tail)).unwrap_err(),
                RenderError::BadJpeg("frame larger than entropy data"),
            );
        }
    }
}

#[test]
fn idct_cos_table_matches_inline_cosines() {
    // The table must reproduce the inline `theta.cos()` evaluation
    // bit for bit, so decoded samples do not change.
    fn idct_inline(coeffs: &[i32; 64], out: &mut [u8; 64]) {
        let mut tmp = [0.0f32; 64];
        for i in 0..64 {
            tmp[i] = coeffs[i] as f32;
        }
        let mut work = [0.0f32; 64];
        for row in 0..8 {
            let base = row * 8;
            for x in 0..8 {
                let mut acc = 0.0f32;
                for u in 0..8 {
                    let cu = if u == 0 {
                        core::f32::consts::FRAC_1_SQRT_2
                    } else {
                        1.0
                    };
                    let theta = ((2 * x + 1) as f32) * (u as f32) * core::f32::consts::PI / 16.0;
                    acc += cu * tmp[base + u] * theta.cos();
                }
                work[base + x] = acc * 0.5;
            }
        }
        for col in 0..8 {
            for y in 0..8 {
                let mut acc = 0.0f32;
                for v in 0..8 {
                    let cv = if v == 0 {
                        core::f32::consts::FRAC_1_SQRT_2
                    } else {
                        1.0
                    };
                    let theta = ((2 * y + 1) as f32) * (v as f32) * core::f32::consts::PI / 16.0;
                    acc += cv * work[v * 8 + col] * theta.cos();
                }
                tmp[y * 8 + col] = acc * 0.5;
            }
        }
        for i in 0..64 {
            let v = (tmp[i] + 128.0).round() as i32;
            out[i] = v.clamp(0, 255) as u8;
        }
    }
    let table = idct_cos_table();
    let mut state = 0x2545_F491_u32;
    for _ in 0..2000 {
        let mut coeffs = [0i32; 64];
        for c in &mut coeffs {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *c = (state % 2048) as i32 - 1024;
        }
        let (mut a, mut b) = ([0u8; 64], [0u8; 64]);
        idct_inline(&coeffs, &mut a);
        idct_with_table(&coeffs, &mut b, &table);
        assert_eq!(a, b);
    }
}

#[test]
fn a_second_frame_is_an_error() {
    // A stream holds one frame. Each SOF used to size a fresh frame
    // with a fresh scan budget, so a stream of frames multiplied the
    // work, as libjpeg's JERR_SOF_DUPLICATE prevents.
    use super::encode::{encode_progressive, libjpeg_script, Image};
    let img = Image::random(16, 16, &[(1, 1)], 9, 40, 3);
    let one = encode_progressive(&img, &libjpeg_script(1));
    let sof = marker_pos(&one, MARKER_SOF2) - 1;
    let len = usize::from(u16::from_be_bytes([one[sof + 2], one[sof + 3]]));
    let frame_header = one[sof..sof + 2 + len].to_vec();
    // The header again after the last scan, before EOI.
    let mut two = one[..one.len() - 2].to_vec();
    two.extend_from_slice(&frame_header);
    two.extend_from_slice(&[0xFF, MARKER_EOI]);
    assert!(decode_jpeg(&one).is_ok());
    assert_eq!(
        decode_jpeg(&two).unwrap_err(),
        RenderError::BadJpeg("second SOF")
    );
    // A baseline header repeated before its scan.
    let base = build_constant_jpeg(0, 0, 0);
    let sof = marker_pos(&base, MARKER_SOF0) - 1;
    let len = usize::from(u16::from_be_bytes([base[sof + 2], base[sof + 3]]));
    let mut twice = base[..sof + 2 + len].to_vec();
    twice.extend_from_slice(&base[sof..]);
    assert_eq!(
        decode_jpeg(&twice).unwrap_err(),
        RenderError::BadJpeg("second SOF")
    );
}
