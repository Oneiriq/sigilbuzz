//! Progressive (SOF2) decode tests built from synthetic two-scan
//! streams.

use super::encode::{
    assemble_progressive, deep_script, encode_baseline, encode_progressive, libjpeg_script,
    raw_data, scan, scan_data, spectral_script, Image, Scan,
};
use super::*;
use alloc::format;

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
    bytes.push(0x00); // one byte of scan data
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
    bytes.push(0x00); // one byte of scan data
    bytes.push(0xFF);
    bytes.push(MARKER_EOI);

    let err = decode_jpeg(&bytes).unwrap_err();
    assert!(matches!(
        err,
        RenderError::BadJpeg("progressive AC scan Ss > Se")
    ));
}

#[test]
fn progressive_ac_refinement_scan_without_a_table_is_an_error() {
    // SOF2 + an AC scan with Ah=1 (refinement) and no DHT. A
    // refinement scan codes its run/size symbols with the AC table.
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
    bytes.push(0x00); // one byte of scan data
    bytes.push(0xFF);
    bytes.push(MARKER_EOI);

    let err = decode_jpeg(&bytes).unwrap_err();
    assert!(matches!(
        err,
        RenderError::BadJpeg("missing AC Huffman table")
    ));
}

/// Sampling layouts the decoder accepts: grayscale, then YCbCr 4:4:4,
/// 4:2:2, 4:2:0, and 4:4:0, then grayscale that declares 2x2 sampling,
/// which a one-component frame ignores.
const LAYOUTS: [&[(u8, u8)]; 6] = [
    &[(1, 1)],
    &[(1, 1), (1, 1), (1, 1)],
    &[(2, 1), (1, 1), (1, 1)],
    &[(2, 2), (1, 1), (1, 1)],
    &[(1, 2), (1, 1), (1, 1)],
    &[(2, 2)],
];

/// Frame sizes: whole blocks, whole MCUs, and sizes that end partway
/// through a block or an MCU, where non-interleaved scans visit fewer
/// blocks than the MCU grid holds.
const SIZES: [(u16, u16); 6] = [(8, 8), (1, 1), (20, 12), (33, 17), (16, 16), (47, 9)];

#[test]
fn progressive_ac_coefficients_land_at_their_zigzag_index() {
    // AC energy at zig-zag 1, 2, and 5: natural (0,1), (1,0), (0,2).
    // A coefficient mapped through the zig-zag table twice lands at
    // the wrong frequency, so the block no longer matches baseline.
    let mut block = [0i16; 64];
    block[0] = 40;
    block[1] = 30;
    block[2] = -20;
    block[5] = 12;
    block[63] = 3;
    let img = Image {
        width: 8,
        height: 8,
        sampling: vec![(1, 1)],
        blocks: vec![vec![block]],
    };
    let baseline = decode_jpeg(&encode_baseline(&img)).unwrap();
    let progressive = decode_jpeg(&encode_progressive(&img, &spectral_script(1))).unwrap();
    assert_same_pixels(&progressive, &baseline, "one block");
    // The block is not flat, so the check has teeth.
    assert_ne!(baseline.get(0, 0), baseline.get(7, 0));
}

#[test]
fn progressive_spectral_selection_matches_baseline() {
    // The same coefficients coded as one baseline scan and as a
    // spectral-selection progressive script decode to the same pixels
    // for every layout and size.
    for (li, layout) in LAYOUTS.iter().enumerate() {
        for (si, &(w, h)) in SIZES.iter().enumerate() {
            let seed = 0x9E37_79B9 ^ ((li as u32) << 8) ^ si as u32;
            let img = Image::random(w, h, layout, seed, 40, 3);
            let baseline = decode_jpeg(&encode_baseline(&img)).unwrap();
            let script = spectral_script(layout.len());
            let progressive = decode_jpeg(&encode_progressive(&img, &script)).unwrap();
            assert_eq!(
                (progressive.width, progressive.height),
                (u32::from(w), u32::from(h))
            );
            assert_same_pixels(&progressive, &baseline, &format!("{layout:?} at {w}x{h}"));
        }
    }
}

#[test]
fn progressive_eob_runs_and_zero_runs_match_baseline() {
    // Sparse blocks: most AC bands are empty, so the AC scans code
    // long EOB runs across blocks, and the few nonzero values sit
    // behind runs of more than 16 zeros (ZRL).
    for layout in [LAYOUTS[0], LAYOUTS[3]] {
        let img = Image::random(64, 48, layout, 0x0BAD_5EED, 9, 40);
        let baseline = decode_jpeg(&encode_baseline(&img)).unwrap();
        let script = spectral_script(layout.len());
        let progressive = decode_jpeg(&encode_progressive(&img, &script)).unwrap();
        assert_same_pixels(&progressive, &baseline, &format!("{layout:?}"));
    }
}

#[test]
fn progressive_single_component_dc_scans_visit_only_the_sample_blocks() {
    // 4:2:0 at 20x12: the luma MCU grid is 4x2 blocks, but its samples
    // fill only 3x2. A luma-only DC scan codes 6 blocks; reading 8
    // would take the chroma scans' bits.
    let img = Image::random(20, 12, LAYOUTS[3], 7, 30, 4);
    let mut script = vec![scan(vec![0], 0, 0, 0, 0), scan(vec![1, 2], 0, 0, 0, 0)];
    script.extend(spectral_script(3).into_iter().skip(1));
    let baseline = decode_jpeg(&encode_baseline(&img)).unwrap();
    let progressive = decode_jpeg(&encode_progressive(&img, &script)).unwrap();
    assert_same_pixels(&progressive, &baseline, "luma-only DC scan");
}

#[test]
fn one_component_frames_ignore_their_sampling_factors() {
    // A one-component scan is non-interleaved, so a grayscale frame
    // that declares 2x2 sampling codes its blocks in the same order as
    // one that declares 1x1. Both modes must read them that way.
    let (w, h) = (20u16, 12u16);
    let plain = Image::random(w, h, &[(1, 1)], 0x51DE, 40, 3);
    let mut wide = Image::random(w, h, &[(2, 2)], 1, 0, 1);
    // Copy each visible block to the same position of the 2x2 grid:
    // 3x2 blocks of samples, 3 blocks per row in the 1x1 grid, 4 in
    // the 2x2 grid.
    for by in 0..2 {
        for bx in 0..3 {
            wide.blocks[0][by * 4 + bx] = plain.blocks[0][by * 3 + bx];
        }
    }
    let expected = decode_jpeg(&encode_baseline(&plain)).unwrap();
    let baseline = decode_jpeg(&encode_baseline(&wide)).unwrap();
    assert_same_pixels(&baseline, &expected, "baseline");
    let progressive = decode_jpeg(&encode_progressive(&wide, &spectral_script(1))).unwrap();
    assert_same_pixels(&progressive, &expected, "progressive");
}

/// Panics at the first byte where two decodes differ.
#[track_caller]
pub(super) fn assert_same_pixels(got: &ColorPixmap, want: &ColorPixmap, what: &str) {
    assert_eq!((got.width, got.height), (want.width, want.height), "{what}");
    if let Some(i) = got.data.iter().zip(&want.data).position(|(a, b)| a != b) {
        panic!(
            "{what}: byte {i} is {} instead of {}",
            got.data[i], want.data[i]
        );
    }
}

#[test]
fn progressive_refinement_scripts_match_baseline() {
    // libjpeg's default progression (what Pillow writes) and a script
    // that refines every bit in its own pass rebuild the coefficients
    // exactly, so they decode to the baseline pixels.
    for (li, layout) in LAYOUTS.iter().enumerate() {
        for (si, &(w, h)) in SIZES.iter().enumerate() {
            let seed = 0x85EB_CA6B ^ ((li as u32) << 8) ^ si as u32;
            let img = Image::random(w, h, layout, seed, 100, 3);
            let baseline = decode_jpeg(&encode_baseline(&img)).unwrap();
            for (name, script) in [
                ("libjpeg", libjpeg_script(layout.len())),
                ("deep", deep_script(layout.len())),
            ] {
                let progressive = decode_jpeg(&encode_progressive(&img, &script)).unwrap();
                let what = format!("{name} script, {layout:?} at {w}x{h}");
                assert_same_pixels(&progressive, &baseline, &what);
            }
        }
    }
}

#[test]
fn progressive_refinement_eob_runs_match_baseline() {
    // Sparse blocks: refinement scans code long EOB runs that carry
    // the correction bits of every block they end, and ZRLs that the
    // encoder can only fold into an EOB past the last new coefficient.
    for layout in [LAYOUTS[0], LAYOUTS[3]] {
        for amplitude in [1, 3, 70] {
            let img = Image::random(96, 64, layout, 0xC2B2_AE35, amplitude, 30);
            let baseline = decode_jpeg(&encode_baseline(&img)).unwrap();
            for script in [libjpeg_script(layout.len()), deep_script(layout.len())] {
                let progressive = decode_jpeg(&encode_progressive(&img, &script)).unwrap();
                let what = format!("{layout:?}, amplitude {amplitude}");
                assert_same_pixels(&progressive, &baseline, &what);
            }
        }
    }
}

/// An 8x16 grayscale frame (two blocks) whose AC coefficients are all
/// zero, so a refinement scan over it has no correction bits to read,
/// and the scans that code it at `Al = 1`.
fn flat_two_blocks() -> (Image, Vec<(Scan, Vec<u8>)>) {
    let mut img = Image::random(8, 16, &[(1, 1)], 3, 0, 1);
    img.blocks[0][0][0] = 24;
    img.blocks[0][1][0] = -40;
    let parts = [scan(vec![0], 0, 0, 0, 0), scan(vec![0], 1, 63, 0, 1)]
        .into_iter()
        .map(|s| {
            let data = scan_data(&img, &s);
            (s, data)
        })
        .collect();
    (img, parts)
}

#[test]
fn refinement_eob_run_past_the_last_block_ends_with_the_scan() {
    // EOB14 with every extra bit set ends 32,767 blocks, far more than
    // the frame has. The run stops at the end of the scan, and the
    // next scan still decodes.
    let (img, parts) = flat_two_blocks();
    let expected = decode_jpeg(&assemble_progressive(&img, &parts)).unwrap();
    let mut long = parts.clone();
    let refine = scan(vec![0], 1, 63, 1, 0);
    long.push((refine.clone(), raw_data(&[(0xE0, 0x3FFF, 14)])));
    long.push((refine, raw_data(&[(0x00, 0, 0)])));
    let got = decode_jpeg(&assemble_progressive(&img, &long)).unwrap();
    assert_same_pixels(&got, &expected, "EOB run past the frame");
}

#[test]
fn refinement_value_past_the_band_is_dropped() {
    // Three ZRLs and a run of 15 ask for a new coefficient 64 zeros
    // in, past coefficient 63. The walk stops at the band's end and
    // the value is dropped, so the block keeps its first-pass value.
    let (img, parts) = flat_two_blocks();
    let expected = decode_jpeg(&assemble_progressive(&img, &parts)).unwrap();
    let mut over = parts.clone();
    let data = raw_data(&[
        (0xF0, 0, 0),
        (0xF0, 0, 0),
        (0xF0, 0, 0),
        (0xF1, 1, 1),
        (0x00, 0, 0),
    ]);
    over.push((scan(vec![0], 1, 63, 1, 0), data));
    let got = decode_jpeg(&assemble_progressive(&img, &over)).unwrap();
    assert_same_pixels(&got, &expected, "value past the band");
}

#[test]
fn refinement_scan_cut_short_reads_zero_bits() {
    // A refinement scan whose data stops after one symbol reads zero
    // bits for the rest, like libjpeg, instead of failing or running
    // on.
    let img = Image::random(32, 32, &[(1, 1)], 11, 60, 3);
    let script = libjpeg_script(1);
    let mut parts: Vec<(Scan, Vec<u8>)> = script
        .iter()
        .map(|s| (s.clone(), scan_data(&img, s)))
        .collect();
    let (_, last) = parts.last_mut().unwrap();
    last.truncate(1);
    let pix = decode_jpeg(&assemble_progressive(&img, &parts)).unwrap();
    assert_eq!((pix.width, pix.height), (32, 32));
}

#[test]
fn repeated_scans_run_out_the_work_budget() {
    // Each scan costs its band width per block it covers. A frame may
    // spend 1024 coefficient visits per block, and every pass over AC
    // 1..=63 costs 63 per block, so a stream that keeps repeating the last refinement scan stops
    // with an error instead of walking the frame again and again.
    let (img, mut parts) = flat_two_blocks();
    let refine = scan(vec![0], 1, 63, 1, 0);
    for _ in 0..40 {
        parts.push((refine.clone(), raw_data(&[(0x10, 1, 1)])));
    }
    assert_eq!(
        decode_jpeg(&assemble_progressive(&img, &parts)).unwrap_err(),
        RenderError::BadJpeg("progressive scans exceed work budget")
    );
}

#[test]
fn dc_refinement_scans_need_no_dc_table() {
    // A DC refinement scan reads one raw bit per block, so its DC
    // table selector may name an empty slot, as libjpeg allows.
    let img = Image::random(16, 16, LAYOUTS[1], 5, 50, 3);
    let baseline = decode_jpeg(&encode_baseline(&img)).unwrap();
    let mut bytes = encode_progressive(&img, &libjpeg_script(3));
    // The seventh scan is the interleaved DC refinement. Point its
    // three DC selectors at slot 3, which holds no table.
    let sos = bytes
        .windows(2)
        .enumerate()
        .filter(|(_, w)| *w == [0xFF, MARKER_SOS])
        .map(|(i, _)| i)
        .nth(6)
        .unwrap();
    for c in 0..3 {
        bytes[sos + 6 + 2 * c] = 0x30;
    }
    let progressive = decode_jpeg(&bytes).unwrap();
    assert_same_pixels(&progressive, &baseline, "DC refinement with table 3");
}

/// One Pillow (libjpeg-turbo) fixture: a progressive file written with
/// libjpeg's default script, its baseline twin with the same quantized
/// coefficients, and for layouts without chroma subsampling Pillow's
/// own decode of the progressive file as raw RGB or gray samples.
struct PillowCase {
    name: &'static str,
    progressive: &'static [u8],
    baseline: &'static [u8],
    pillow: Option<&'static [u8]>,
}

/// Built by `tests/tools/build_progressive_jpeg_fixtures.py`.
const PILLOW: [PillowCase; 4] = [
    PillowCase {
        name: "4:4:4 20x12",
        progressive: include_bytes!("fixtures/pillow_rgb444_20x12_prog.jpg"),
        baseline: include_bytes!("fixtures/pillow_rgb444_20x12_base.jpg"),
        pillow: Some(include_bytes!("fixtures/pillow_rgb444_20x12_prog.raw")),
    },
    PillowCase {
        name: "4:2:2 33x17",
        progressive: include_bytes!("fixtures/pillow_rgb422_33x17_prog.jpg"),
        baseline: include_bytes!("fixtures/pillow_rgb422_33x17_base.jpg"),
        pillow: None,
    },
    PillowCase {
        name: "4:2:0 33x17",
        progressive: include_bytes!("fixtures/pillow_rgb420_33x17_prog.jpg"),
        baseline: include_bytes!("fixtures/pillow_rgb420_33x17_base.jpg"),
        pillow: None,
    },
    PillowCase {
        name: "gray 47x9",
        progressive: include_bytes!("fixtures/pillow_gray_47x9_prog.jpg"),
        baseline: include_bytes!("fixtures/pillow_gray_47x9_base.jpg"),
        pillow: Some(include_bytes!("fixtures/pillow_gray_47x9_prog.raw")),
    },
];

#[test]
fn pillow_progressive_files_decode_like_their_baseline_twins() {
    // libjpeg-turbo's progressive files: a DHT before each scan, DC
    // and AC refinement, and frames that end partway through an MCU.
    for case in &PILLOW {
        let progressive = decode_jpeg(case.progressive).unwrap();
        let baseline = decode_jpeg(case.baseline).unwrap();
        assert_same_pixels(&progressive, &baseline, case.name);
        let Some(pillow) = case.pillow else {
            continue;
        };
        // Pillow's integer IDCT and color conversion land a few levels
        // from the float IDCT here.
        let pixels = (progressive.width * progressive.height) as usize;
        let channels = pillow.len() / pixels;
        let mut max = 0;
        for (i, px) in progressive.data.chunks_exact(4).enumerate() {
            for (c, &v) in px.iter().take(3).enumerate() {
                max = max.max(v.abs_diff(pillow[i * channels + c.min(channels - 1)]));
            }
        }
        assert!(max <= 4, "{}: {max} levels from Pillow", case.name);
    }
}
