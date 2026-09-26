//! Unit tests for the partial `gvar` rewrite and its packed delta
//! codec.

use super::*;
use alloc::vec;
use sigilbuzz::tables::gvar::Gvar as ParsedGvar;

#[test]
fn passthrough_when_every_axis_keeps() {
    // Synthetic gvar bytes pass through verbatim when no axis
    // pins. Use a minimal but valid 1-axis header.
    let bytes = build_minimal_gvar();
    let pins = vec![AxisPin::Keep];
    let coords = vec![0.0_f32];
    let out = bake_gvar_partial(&bytes, &coords, &pins, 1).unwrap();
    assert_eq!(out, bytes);
}

#[test]
fn pin_axis_axiscount_collapses_when_no_keep() {
    // All axes pin: every tuple folds; output has axisCount=0
    // and surviving tuples are no-ops (peak=0 across kept axes).
    let bytes = build_minimal_gvar();
    let pins = vec![AxisPin::Pin];
    let coords = vec![1.0_f32]; // pin at peak
    let out = bake_gvar_partial(&bytes, &coords, &pins, 0).unwrap();
    assert_eq!(u16::from_be_bytes([out[4], out[5]]), 0);
}

#[test]
fn rejects_pins_coords_length_mismatch() {
    let bytes = build_minimal_gvar();
    let pins = vec![AxisPin::Pin];
    let coords = vec![0.0_f32, 0.0_f32];
    assert!(bake_gvar_partial(&bytes, &coords, &pins, 0).is_err());
}

#[test]
fn encode_packed_deltas_round_trips_i8_run() {
    let mut out = Vec::new();
    encode_packed_deltas(&[1, 2, -3, 4], &mut out);
    let (decoded, _) = read_packed_deltas_n(&out, 4).unwrap();
    assert_eq!(decoded, vec![1, 2, -3, 4]);
}

#[test]
fn encode_packed_deltas_round_trips_i16_run() {
    let values: Vec<i32> = vec![500, -500, 1000];
    let mut out = Vec::new();
    encode_packed_deltas(&values, &mut out);
    let (decoded, _) = read_packed_deltas_n(&out, 3).unwrap();
    assert_eq!(decoded, values);
}

#[test]
fn encode_packed_deltas_compresses_zero_run() {
    let mut out = Vec::new();
    encode_packed_deltas(&[0, 0, 0, 0, 0], &mut out);
    // ALL_ZERO control byte: 0x80 | 0x04 (run-1=4) -> 0x84.
    assert_eq!(out, vec![0x84]);
}

#[test]
fn encode_packed_deltas_mixed_runs() {
    let mut out = Vec::new();
    encode_packed_deltas(&[5, 0, 0, 10], &mut out);
    // i8 run of 1 (control 0x00, byte 5),
    // ALL_ZERO run of 2 (control 0x81),
    // i8 run of 1 (control 0x00, byte 10).
    let (decoded, _) = read_packed_deltas_n(&out, 4).unwrap();
    assert_eq!(decoded, vec![5, 0, 0, 10]);
}

#[test]
fn synthetic_two_axis_pin_wght_keep_wdth_drops_pin_dimension() {
    // Two-axis (wght + wdth) gvar with one tuple at peak=(1,1).
    // Pin wght=1 (scalar 1, no payload scaling), Keep wdth.
    // Output gvar must have axisCount=1 and the surviving tuple's
    // peak must be the wdth-only [1.0].
    let bytes = build_two_axis_gvar();
    let pins = vec![AxisPin::Pin, AxisPin::Keep];
    let coords = vec![1.0_f32, 0.0];
    let out = bake_gvar_partial(&bytes, &coords, &pins, 1).unwrap();
    // Header axisCount=1.
    assert_eq!(u16::from_be_bytes([out[4], out[5]]), 1);
    // Parse the output and confirm the surviving tuple's peak
    // matches (wdth=1.0 only).
    let parsed = ParsedGvar::parse(&out).expect("parses");
    assert_eq!(parsed.axis_count(), 1);
    // Glyph 0's deltas at wdth=1 equal the source's deltas at
    // (wght=1, wdth=1).
    let new_deltas = parsed.glyph_deltas(0, &[1.0_f32], 4);
    let src = ParsedGvar::parse(&bytes).expect("parses src");
    let src_deltas = src.glyph_deltas(0, &[1.0_f32, 1.0], 4);
    assert_eq!(new_deltas.len(), src_deltas.len());
    for (a, b) in new_deltas.iter().zip(src_deltas.iter()) {
        assert_eq!(a.point, b.point);
        assert!((a.dx - b.dx).abs() < 1e-3, "dx {} vs {}", a.dx, b.dx);
        assert!((a.dy - b.dy).abs() < 1e-3, "dy {} vs {}", a.dy, b.dy);
    }
}

/// Probe (#197): pinning at a coord outside the tuple's region,
/// `axis_support_scalar` returns 0, the tuple drops via
/// `project_region_onto_kept_axes`. No infinite loops, no panics,
/// no NaN leak through the round-trip.  Coords approaching f32::MAX
/// must clamp through the existing NaN/Inf hardening (#185 / #186).
#[test]
fn pin_with_extreme_coord_drops_tuple_without_panic() {
    let bytes = build_two_axis_gvar();
    let pins = vec![AxisPin::Pin, AxisPin::Keep];
    let coords = vec![1.0e30_f32, 0.0]; // wildly out of [-1, 1]
    let out = bake_gvar_partial(&bytes, &coords, &pins, 1).expect("partial bake");
    // Output must parse cleanly.
    let parsed = ParsedGvar::parse(&out).expect("parses");
    assert_eq!(parsed.axis_count(), 1);
    // Surviving tuples (if any) must produce finite deltas; the
    // tuple should drop because pin scalar is 0 at coord 1e30.
    let new_deltas = parsed.glyph_deltas(0, &[1.0_f32], 4);
    for d in &new_deltas {
        assert!(d.dx.is_finite(), "dx must be finite, got {}", d.dx);
        assert!(d.dy.is_finite(), "dy must be finite, got {}", d.dy);
    }
}

#[test]
fn synthetic_two_axis_pin_wght_at_half_scales_payload() {
    // Pin wght=0.5 -> scalar 0.5; Keep wdth. Surviving tuple's
    // peak is wdth-only; payload scales by 0.5.
    let bytes = build_two_axis_gvar();
    let pins = vec![AxisPin::Pin, AxisPin::Keep];
    let coords = vec![0.5_f32, 0.0];
    let out = bake_gvar_partial(&bytes, &coords, &pins, 1).unwrap();
    let parsed = ParsedGvar::parse(&out).expect("parses");
    assert_eq!(parsed.axis_count(), 1);
    // At wdth=1 the new deltas equal half the source's at peak.
    let new_deltas = parsed.glyph_deltas(0, &[1.0_f32], 4);
    // Source dx=10 at peak (1,1) -> new dx=5 at wdth=1.
    for d in &new_deltas {
        assert!((d.dx - 5.0).abs() < 1e-3, "expected 5, got {}", d.dx);
    }
}

/// Builds a 2-axis gvar with one glyph, one tuple at
/// peak=(1.0, 1.0), all-points i8 deltas: x=+10 x4, y=0 x4.
fn build_two_axis_gvar() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&2u16.to_be_bytes()); // axisCount
    out.extend_from_slice(&0u16.to_be_bytes()); // sharedTupleCount
    let shared_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // glyphCount
    out.extend_from_slice(&0u16.to_be_bytes()); // flags (short)
    let data_array_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());

    let glyph_off_start = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());

    while out.len() % 4 != 0 {
        out.push(0);
    }

    let shared_off = out.len() as u32;
    out[shared_off_slot..shared_off_slot + 4].copy_from_slice(&shared_off.to_be_bytes());
    let data_array_off = out.len() as u32;
    out[data_array_slot..data_array_slot + 4].copy_from_slice(&data_array_off.to_be_bytes());

    let gvd_start = out.len();
    out.extend_from_slice(&1u16.to_be_bytes()); // tupleVariationCount=1
    let data_off_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());

    let vds_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // variationDataSize
    out.extend_from_slice(&FLAG_EMBEDDED_PEAK.to_be_bytes());
    write_f2dot14(&mut out, 1.0); // axis 0 peak
    write_f2dot14(&mut out, 1.0); // axis 1 peak

    let dataoff_val = (out.len() - gvd_start) as u16;
    out[data_off_slot..data_off_slot + 2].copy_from_slice(&dataoff_val.to_be_bytes());

    let tuple_start = out.len();
    out.push(0x03);
    out.resize(out.len() + 4, 10);
    out.push(0x83);
    let tuple_len = (out.len() - tuple_start) as u16;
    out[vds_slot..vds_slot + 2].copy_from_slice(&tuple_len.to_be_bytes());

    let body_len = (out.len() as u32) - data_array_off;
    let half = (body_len / 2) as u16;
    out[glyph_off_start + 2..glyph_off_start + 4].copy_from_slice(&half.to_be_bytes());

    out
}

/// Builds a minimal gvar with one axis, one glyph, one all-points
/// embedded-peak tuple at peak=1 with x deltas = +10 and y = 0,
/// 4 points.
fn build_minimal_gvar() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
    out.extend_from_slice(&0u16.to_be_bytes()); // sharedTupleCount
    let shared_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // glyphCount
    out.extend_from_slice(&0u16.to_be_bytes()); // flags (short)
    let data_array_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());

    // glyphVariationDataOffsets[2]: [0, body_len/2].
    let glyph_off_start = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());

    // Pad to 4-byte alignment.
    while out.len() % 4 != 0 {
        out.push(0);
    }

    let shared_off = out.len() as u32;
    out[shared_off_slot..shared_off_slot + 4].copy_from_slice(&shared_off.to_be_bytes());
    // sharedTupleCount is 0, so no shared tuples.

    let data_array_off = out.len() as u32;
    out[data_array_slot..data_array_slot + 4].copy_from_slice(&data_array_off.to_be_bytes());

    // Glyph variation data body.
    let gvd_start = out.len();
    out.extend_from_slice(&1u16.to_be_bytes()); // tupleVariationCount=1, no shared points
    let data_off_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // dataOffset (patch)

    // TupleVariationHeader: variationDataSize, tupleIndex(EMBEDDED_PEAK), peak[1].
    let vds_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // variationDataSize (patch)
    out.extend_from_slice(&FLAG_EMBEDDED_PEAK.to_be_bytes());
    write_f2dot14(&mut out, 1.0);

    let dataoff_val = (out.len() - gvd_start) as u16;
    out[data_off_slot..data_off_slot + 2].copy_from_slice(&dataoff_val.to_be_bytes());

    // Tuple data: x deltas (+10 x4) then y deltas (0 x4). No
    // private/shared point numbers, equivalent to all-points.
    let tuple_start = out.len();
    out.push(0x03); // i8 run, run-1=3 -> 4 deltas
    out.resize(out.len() + 4, 10);
    out.push(0x83); // ALL_ZERO run-1=3 -> 4 zeros
    let tuple_len = (out.len() - tuple_start) as u16;
    out[vds_slot..vds_slot + 2].copy_from_slice(&tuple_len.to_be_bytes());

    // Patch glyph offset (short, half-encoded).
    let body_len = (out.len() as u32) - data_array_off;
    let half = (body_len / 2) as u16;
    out[glyph_off_start + 2..glyph_off_start + 4].copy_from_slice(&half.to_be_bytes());

    out
}
