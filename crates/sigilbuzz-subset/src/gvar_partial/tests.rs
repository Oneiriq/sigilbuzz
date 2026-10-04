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

/// Builds a gvar table with long offsets. `offsets` has one entry
/// per glyph plus one. `data` is the glyph variation data array.
fn gvar_with_offsets(
    axis_count: u16,
    shared_tuples: &[u8],
    offsets: &[u32],
    data: &[u8],
) -> Vec<u8> {
    let glyph_count = (offsets.len() - 1) as u16;
    let shared_tuple_count = if axis_count == 0 {
        0
    } else {
        (shared_tuples.len() / (usize::from(axis_count) * 2)) as u16
    };
    let shared_off = (20 + offsets.len() * 4) as u32;
    let data_off = shared_off + shared_tuples.len() as u32;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&axis_count.to_be_bytes());
    out.extend_from_slice(&shared_tuple_count.to_be_bytes());
    out.extend_from_slice(&shared_off.to_be_bytes());
    out.extend_from_slice(&glyph_count.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // long offsets
    out.extend_from_slice(&data_off.to_be_bytes());
    for off in offsets {
        out.extend_from_slice(&off.to_be_bytes());
    }
    out.extend_from_slice(shared_tuples);
    out.extend_from_slice(data);
    out
}

/// Glyph variation data with `tuple_count` tuples that all point
/// at shared tuple 0 and carry `tuple_data` each.
fn body_of_shared_tuples(tuple_count: u16, tuple_data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&tuple_count.to_be_bytes());
    body.extend_from_slice(&(4 + 4 * tuple_count).to_be_bytes()); // dataOffset
    for _ in 0..tuple_count {
        body.extend_from_slice(&(tuple_data.len() as u16).to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes()); // shared tuple 0
    }
    for _ in 0..tuple_count {
        body.extend_from_slice(tuple_data);
    }
    body
}

#[test]
fn shared_tuple_projection_is_not_repeated_per_tuple() {
    // 20000 axes, one all-zero shared tuple, and ten glyphs of 4095
    // tuples that each point at it. Projecting the 20000-axis
    // region again for every tuple cost billions of steps.
    const AXES: usize = 20_000;
    let shared = alloc::vec![0u8; AXES * 2];
    let body = body_of_shared_tuples(0x0FFF, &[]);
    let offsets: Vec<u32> = (0..=10).map(|i| i * body.len() as u32).collect();
    let gvar = gvar_with_offsets(AXES as u16, &shared, &offsets, &body.repeat(10));
    let mut pins = alloc::vec![AxisPin::Pin; AXES];
    pins[0] = AxisPin::Keep;
    let coords = alloc::vec![0.0f32; AXES];
    let out = bake_gvar_partial(&gvar, &coords, &pins, 1).expect("partial bake");
    // Every tuple has an all-zero Keep peak and drops.
    let parsed = ParsedGvar::parse(&out).expect("parses");
    assert_eq!(parsed.axis_count(), 1);
}

#[test]
fn tuple_headers_past_the_u16_data_offset_are_rejected() {
    // 4095 tuples each point at a shared tuple of their own, so their
    // headers take 16 KB. Each survives with 16 Keep axes and a region
    // of its own, so none merge, and the rewrite embeds the peaks,
    // which needs 147 KB of headers: past what the u16 dataOffset can
    // address. The offset used to be truncated, which corrupted the
    // glyph.
    const AXES: usize = 17;
    const TUPLES: u16 = 0x0FFF;
    // Shared tuple `i` peaks at 1.0 on every axis but axis 1, where it
    // peaks at its own `(i + 1) / 16384`.
    let mut shared: Vec<u8> = Vec::new();
    for i in 0..TUPLES {
        for axis in 0..AXES {
            let peak: i16 = if axis == 1 { i as i16 + 1 } else { 0x4000 };
            shared.extend_from_slice(&peak.to_be_bytes());
        }
    }
    // All-points deltas for a glyph of one point: x 1, y 1.
    let tuple_data = [0x00, 1, 0x00, 1];
    let mut body = Vec::new();
    body.extend_from_slice(&TUPLES.to_be_bytes());
    body.extend_from_slice(&(4 + 4 * TUPLES).to_be_bytes()); // dataOffset
    for i in 0..TUPLES {
        body.extend_from_slice(&(tuple_data.len() as u16).to_be_bytes());
        body.extend_from_slice(&i.to_be_bytes()); // shared tuple i
    }
    for _ in 0..TUPLES {
        body.extend_from_slice(&tuple_data);
    }
    let gvar = gvar_with_offsets(AXES as u16, &shared, &[0, body.len() as u32], &body);
    let mut pins = alloc::vec![AxisPin::Keep; AXES];
    pins[0] = AxisPin::Pin;
    let mut coords = alloc::vec![0.0f32; AXES];
    coords[0] = 1.0;
    let r = bake_gvar_partial(&gvar, &coords, &pins, (AXES - 1) as u16);
    assert!(matches!(r, Err(SubsetError::Unsupported(_))), "{r:?}");
}

#[test]
fn glyphs_sharing_one_data_range_are_rejected() {
    // One 65 KB glyph body (a tuple with 32000 x and y deltas) and
    // 2000 glyphs whose offsets alternate 0, len, 0, ... so every
    // other glyph rewrites the same body. The output used to hold
    // a thousand rewritten copies.
    let mut deltas = Vec::new();
    for _ in 0..1000 {
        deltas.push(DELTA_COUNT_MASK); // i8 run of 64
        deltas.extend_from_slice(&[5u8; 64]);
    }
    let mut body = Vec::new();
    body.extend_from_slice(&1u16.to_be_bytes()); // one tuple
    body.extend_from_slice(&12u16.to_be_bytes()); // dataOffset
    body.extend_from_slice(&(deltas.len() as u16).to_be_bytes());
    body.extend_from_slice(&FLAG_EMBEDDED_PEAK.to_be_bytes());
    body.extend_from_slice(&0x4000i16.to_be_bytes());
    body.extend_from_slice(&0x4000i16.to_be_bytes());
    body.extend_from_slice(&deltas);
    let offsets: Vec<u32> = (0..=2000u32)
        .map(|i| if i % 2 == 0 { 0 } else { body.len() as u32 })
        .collect();
    let gvar = gvar_with_offsets(2, &[], &offsets, &body);
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let r = bake_gvar_partial(&gvar, &[1.0, 0.0], &pins, 1);
    assert!(matches!(r, Err(SubsetError::Unsupported(_))), "{r:?}");
}

#[test]
fn drifting_sparse_tuples_are_rewritten_within_the_work_budget() {
    // One glyph of 2,000 collinear points whose default points move
    // unevenly, and 600 sparse tuples on the kept axis, each listing
    // the two end points. Every tuple drifts, and writing one with
    // every point costs 3 x 2,004 units of work: the budget covers
    // some of them, and the rest keep their two points.
    const POINTS: u16 = 2000;
    const TUPLES: u16 = 600;
    let mut tuple = vec![2, 0x81];
    tuple.extend_from_slice(&0u16.to_be_bytes());
    tuple.extend_from_slice(&(POINTS - 1).to_be_bytes());
    tuple.extend_from_slice(&[0x01, 0, 90, 0x01, 0, 60]);
    let mut body = Vec::new();
    body.extend_from_slice(&TUPLES.to_be_bytes());
    body.extend_from_slice(&(4 + 8 * TUPLES).to_be_bytes());
    for k in 0..TUPLES {
        body.extend_from_slice(&(tuple.len() as u16).to_be_bytes());
        body.extend_from_slice(&0xA000u16.to_be_bytes()); // peak, private points
        body.extend_from_slice(&0i16.to_be_bytes()); // wght
        body.extend_from_slice(&(16384 - k as i16).to_be_bytes()); // opsz
    }
    for _ in 0..TUPLES {
        body.extend_from_slice(&tuple);
    }
    let gvar = gvar_with_offsets(2, &[], &[0, body.len() as u32], &body);
    let before: Vec<(i32, i32)> = (0..i32::from(POINTS)).map(|i| (i, 0)).collect();
    let after: Vec<(i32, i32)> = before
        .iter()
        .map(|&(x, y)| (x + if x % 2 == 0 { 40 } else { -40 }, y))
        .collect();
    let points = |_| {
        Some(GlyphPoints {
            end_pts: vec![POINTS - 1],
            before: before.clone(),
            after: Some(after.clone()),
        })
    };
    let warnings = Warnings::default();
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let out =
        bake_gvar_partial_with(&gvar, &[1.0, 0.0], &pins, 1, &points, &warnings).expect("bake");
    // Read back each tuple's data size: the dense ones are large.
    let data_off = u32::from_be_bytes([out[16], out[17], out[18], out[19]]) as usize;
    let glyph = &out[data_off..];
    let count = usize::from(u16::from_be_bytes([glyph[0], glyph[1]]) & 0x0FFF);
    let sizes: Vec<usize> = (0..count)
        .map(|i| usize::from(u16::from_be_bytes([glyph[4 + 6 * i], glyph[5 + 6 * i]])))
        .collect();
    let dense = sizes.iter().filter(|&&s| s > 100).count();
    let budget = gvar.len() as u64 * WORK_PER_BYTE + MIN_WORK;
    let per_dense = 3 * u64::from(POINTS + 4);
    assert_eq!(count, usize::from(TUPLES));
    assert_eq!(dense as u64, budget / per_dense);
    assert!(dense < usize::from(TUPLES));
    let warnings = warnings.into_sorted();
    assert!(
        warnings
            .iter()
            .any(|w| w.context.contains("tuples keep their points")),
        "{warnings:?}"
    );
}

#[test]
fn a_glyph_past_the_decoding_budget_loses_its_variations() {
    // 40 tuples on the kept axis, each covering every point with 500
    // bytes of zero runs per stream: 32,000 deltas a stream, 64 per
    // byte. Decoding them all takes more than the 40 KB table may
    // decode, so the glyph keeps no variations, and says so.
    const TUPLES: u16 = 40;
    let mut tuple = vec![0xBFu8; 500]; // x: 500 runs of 64 zeros
    tuple.extend(vec![0xBFu8; 500]); // y
    let mut body = Vec::new();
    body.extend_from_slice(&TUPLES.to_be_bytes());
    body.extend_from_slice(&(4 + 8 * TUPLES).to_be_bytes());
    for _ in 0..TUPLES {
        body.extend_from_slice(&(tuple.len() as u16).to_be_bytes());
        body.extend_from_slice(&0x8000u16.to_be_bytes());
        body.extend_from_slice(&0i16.to_be_bytes());
        body.extend_from_slice(&16384i16.to_be_bytes());
    }
    for _ in 0..TUPLES {
        body.extend_from_slice(&tuple);
    }
    let gvar = gvar_with_offsets(2, &[], &[0, body.len() as u32], &body);
    let decoding = u64::from(TUPLES) * (2 * 32_000 + 1);
    assert!(decoding > gvar.len() as u64 * DECODE_PER_BYTE + MIN_DECODE);
    let warnings = Warnings::default();
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let out =
        bake_gvar_partial_with(&gvar, &[1.0, 0.0], &pins, 1, &|_| None, &warnings).expect("bake");
    let parsed = ParsedGvar::parse(&out).expect("parses");
    assert!(parsed.glyph_deltas(0, &[1.0], 32_004).is_empty());
    let warnings = warnings.into_sorted();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].context, OVER_BUDGET);
}
