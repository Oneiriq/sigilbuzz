//! Progressive (SOF2) decode tests built from synthetic two-scan
//! streams.

use super::*;

/// Build a minimal **progressive** (SOF2) grayscale JPEG that
/// encodes a single 8x8 block via two scans:
///
///   - SOS #1: DC scan (Ss=0, Se=0, Ah=0, Al=0) emits the DC
///     coefficient using the standard luma DC table.
///   - SOS #2: AC first-time scan (Ss=1, Se=63, Ah=0, Al=0) emits
///     a single EOB so all 63 AC coefficients stay zero.
///
/// After IDCT + level-shift the spatial output is uniform
/// `dc / 8 + 128` (clamped). This exercises the progressive
/// dispatch end-to-end: SOF2 walker entry, two `read_sos_progressive`
/// calls, `scan_dc_first` + `scan_ac_first`, and `finalize_progressive`.
pub(super) fn build_progressive_grayscale_jpeg(dc: i32) -> Vec<u8> {
    let mut out = vec![0xFF, MARKER_SOI];

    // DQT: single identity table at dest 0.
    out.push(0xFF);
    out.push(MARKER_DQT);
    out.extend_from_slice(&(2u16 + 1 + 64).to_be_bytes());
    out.push(0); // precision 0, dest 0
    out.extend_from_slice(&[1u8; 64]);

    // SOF2: 8x8, 1 component (grayscale), 1x1 sampling, qt 0.
    out.push(0xFF);
    out.push(MARKER_SOF2);
    out.extend_from_slice(&(2u16 + 6 + 3).to_be_bytes());
    out.push(8); // precision
    out.extend_from_slice(&8u16.to_be_bytes()); // height
    out.extend_from_slice(&8u16.to_be_bytes()); // width
    out.push(1); // 1 component
    out.push(1); // id
    out.push((1 << 4) | 1); // 1x1
    out.push(0); // qt 0

    // DHT: DC luma + AC luma at dest 0. Reuse the standard
    // tables defined in the baseline fixture.
    let dc_lum_counts: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
    let dc_lum_syms: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    let ac_lum_counts: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
    let ac_lum_syms: [u8; 162] = [
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
        0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52,
        0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25,
        0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45,
        0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64,
        0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
        0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6,
        0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3,
        0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8,
        0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ];
    let mut dht_body = Vec::new();
    let push_table = |body: &mut Vec<u8>, class: u8, dest: u8, counts: &[u8], syms: &[u8]| {
        body.push((class << 4) | dest);
        body.extend_from_slice(counts);
        body.extend_from_slice(syms);
    };
    push_table(&mut dht_body, 0, 0, &dc_lum_counts, &dc_lum_syms);
    push_table(&mut dht_body, 1, 0, &ac_lum_counts, &ac_lum_syms);
    out.push(0xFF);
    out.push(MARKER_DHT);
    out.extend_from_slice(&((dht_body.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&dht_body);

    // SOS #1: DC scan (Ss=0, Se=0, Ah=Al=0).
    out.push(0xFF);
    out.push(MARKER_SOS);
    out.extend_from_slice(&(2u16 + 1 + 2 + 3).to_be_bytes());
    out.push(1); // 1 component
    out.push(1); // id
    out.push(0); // DC=0 AC=0
    out.push(0); // Ss
    out.push(0); // Se
    out.push(0); // Ah/Al

    // Entropy: encode DC value only (no AC) for a single block.
    let mut bw = BitWriter::default();
    let (size, code) = magnitude_encode(dc);
    let (huff_code, huff_len) = std_dc_lum_code(size);
    bw.write_bits(huff_code, huff_len);
    if size > 0 {
        bw.write_bits(code, size);
    }
    bw.flush();
    out.extend_from_slice(&bw.bytes);

    // SOS #2: AC first-time scan (Ss=1, Se=63, Ah=Al=0).
    out.push(0xFF);
    out.push(MARKER_SOS);
    out.extend_from_slice(&(2u16 + 1 + 2 + 3).to_be_bytes());
    out.push(1); // 1 component
    out.push(1); // id
    out.push(0); // DC=0 AC=0
    out.push(1); // Ss
    out.push(63); // Se
    out.push(0); // Ah/Al

    // Entropy: single EOB (AC luma symbol 0x00).
    let mut bw = BitWriter::default();
    let (eob_code, eob_len) = std_ac_lum_code(0x00);
    bw.write_bits(eob_code, eob_len);
    bw.flush();
    out.extend_from_slice(&bw.bytes);

    out.push(0xFF);
    out.push(MARKER_EOI);
    out
}

#[test]
fn progressive_grayscale_decodes_to_constant() {
    // dc = 0 -> spatial sample 0 -> after level shift 128.
    let bytes = build_progressive_grayscale_jpeg(0);
    let pix = decode_jpeg(&bytes).unwrap();
    assert_eq!(pix.width, 8);
    assert_eq!(pix.height, 8);
    let center = pix.get(4, 4);
    assert_eq!(center[3], 255);
    // Grayscale: R=G=B=128 (within rounding).
    for (ch, value) in center.iter().take(3).enumerate() {
        assert!(
            (126..=130).contains(value),
            "progressive grayscale channel {ch} value {value} near 128"
        );
    }
}

#[test]
fn progressive_matches_baseline_for_constant_block() {
    // A progressive 1-component decode of dc=64 should land on
    // approximately the same luminance as a baseline decode of
    // YCbCr (64, 0, 0). With identity quantization, dequant DC=64
    // and IDCT -> spatial 8 per pixel, level-shifted to 136.
    let bytes = build_progressive_grayscale_jpeg(64);
    let pix = decode_jpeg(&bytes).unwrap();
    let center = pix.get(4, 4);
    assert_eq!(center[3], 255);
    // R=G=B for grayscale, all near 136.
    assert!(
        (134..=138).contains(&center[0]),
        "R ({}) near 136",
        center[0]
    );
    assert_eq!(center[0], center[1]);
    assert_eq!(center[1], center[2]);
}

#[test]
fn progressive_sos_rejects_dc_scan_with_se_nonzero() {
    // Build a minimal SOF2 + SOS where Ss=0 and Se=5 (illegal):
    // a DC scan must have Se=0 per T.81 §F.1.4.2.
    let mut bytes = vec![0xFF, MARKER_SOI];
    // Minimal DQT.
    bytes.push(0xFF);
    bytes.push(MARKER_DQT);
    bytes.extend_from_slice(&(2u16 + 1 + 64).to_be_bytes());
    bytes.push(0);
    bytes.extend_from_slice(&[1u8; 64]);
    // SOF2.
    bytes.push(0xFF);
    bytes.push(MARKER_SOF2);
    bytes.extend_from_slice(&(2u16 + 6 + 3).to_be_bytes());
    bytes.push(8);
    bytes.extend_from_slice(&8u16.to_be_bytes());
    bytes.extend_from_slice(&8u16.to_be_bytes());
    bytes.push(1);
    bytes.push(1);
    bytes.push((1 << 4) | 1);
    bytes.push(0);
    // SOS: Ss=0 but Se=5 (malformed for DC scan).
    bytes.push(0xFF);
    bytes.push(MARKER_SOS);
    bytes.extend_from_slice(&(2u16 + 1 + 2 + 3).to_be_bytes());
    bytes.push(1);
    bytes.push(1);
    bytes.push(0);
    bytes.push(0); // Ss
    bytes.push(5); // Se
    bytes.push(0);
    bytes.push(0xFF);
    bytes.push(MARKER_EOI);

    let err = decode_jpeg(&bytes).unwrap_err();
    assert!(matches!(
        err,
        RenderError::BadJpeg("progressive DC scan Se != 0")
    ));
}

#[test]
fn progressive_sos_rejects_ac_scan_with_ss_greater_than_se() {
    let mut bytes = vec![0xFF, MARKER_SOI];
    bytes.push(0xFF);
    bytes.push(MARKER_DQT);
    bytes.extend_from_slice(&(2u16 + 1 + 64).to_be_bytes());
    bytes.push(0);
    bytes.extend_from_slice(&[1u8; 64]);
    bytes.push(0xFF);
    bytes.push(MARKER_SOF2);
    bytes.extend_from_slice(&(2u16 + 6 + 3).to_be_bytes());
    bytes.push(8);
    bytes.extend_from_slice(&8u16.to_be_bytes());
    bytes.extend_from_slice(&8u16.to_be_bytes());
    bytes.push(1);
    bytes.push(1);
    bytes.push((1 << 4) | 1);
    bytes.push(0);
    bytes.push(0xFF);
    bytes.push(MARKER_SOS);
    bytes.extend_from_slice(&(2u16 + 1 + 2 + 3).to_be_bytes());
    bytes.push(1);
    bytes.push(1);
    bytes.push(0);
    bytes.push(20); // Ss
    bytes.push(5); // Se (< Ss)
    bytes.push(0);
    bytes.push(0xFF);
    bytes.push(MARKER_EOI);

    let err = decode_jpeg(&bytes).unwrap_err();
    assert!(matches!(
        err,
        RenderError::BadJpeg("progressive AC scan Ss > Se")
    ));
}

#[test]
fn progressive_ac_refinement_scan_is_unsupported() {
    // SOF2 + an AC scan with Ah=1 (refinement). The implementation
    // surfaces this as BadJpeg explicitly because AC refinement
    // is not implemented.
    let mut bytes = vec![0xFF, MARKER_SOI];
    bytes.push(0xFF);
    bytes.push(MARKER_DQT);
    bytes.extend_from_slice(&(2u16 + 1 + 64).to_be_bytes());
    bytes.push(0);
    bytes.extend_from_slice(&[1u8; 64]);
    bytes.push(0xFF);
    bytes.push(MARKER_SOF2);
    bytes.extend_from_slice(&(2u16 + 6 + 3).to_be_bytes());
    bytes.push(8);
    bytes.extend_from_slice(&8u16.to_be_bytes());
    bytes.extend_from_slice(&8u16.to_be_bytes());
    bytes.push(1);
    bytes.push(1);
    bytes.push((1 << 4) | 1);
    bytes.push(0);
    // AC scan with Ah=1, Al=0.
    bytes.push(0xFF);
    bytes.push(MARKER_SOS);
    bytes.extend_from_slice(&(2u16 + 1 + 2 + 3).to_be_bytes());
    bytes.push(1);
    bytes.push(1);
    bytes.push(0);
    bytes.push(1); // Ss
    bytes.push(63); // Se
    bytes.push(1 << 4); // Ah=1, Al=0
    bytes.push(0xFF);
    bytes.push(MARKER_EOI);

    let err = decode_jpeg(&bytes).unwrap_err();
    assert!(matches!(
        err,
        RenderError::BadJpeg("progressive AC refinement scans not supported")
    ));
}
