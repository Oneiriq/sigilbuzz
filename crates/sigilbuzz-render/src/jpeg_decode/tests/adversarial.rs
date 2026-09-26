//! Adversarial inputs that must surface as structured `BadJpeg`
//! errors rather than panics.

use super::*;

// ---------------------------------------------------------------
// Wave-21 adversarial pass. SOF2 progressive *is* now supported
// (sibling PR #241), but the surrounding non-baseline rejection
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
