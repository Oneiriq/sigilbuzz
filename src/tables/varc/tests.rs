//! Tests for the `VARC` parser: the header, component records and
//! their flags, uint32var decoding, rotation, and the work limits.

use super::*;
use alloc::vec;

/// Builds a coverage format-1 table with the listed gids in order.
fn build_coverage(gids: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&(gids.len() as u16).to_be_bytes());
    for g in gids {
        out.extend_from_slice(&g.to_be_bytes());
    }
    out
}

/// Builds a CFF2 INDEX with 1-byte offsets, or 4-byte offsets when
/// the payload does not fit in one byte.
fn build_cff2_index(entries: &[&[u8]]) -> Vec<u8> {
    let count = entries.len() as u32;
    let mut out = Vec::new();
    out.extend_from_slice(&count.to_be_bytes());
    if entries.is_empty() {
        return out;
    }
    let total: usize = entries.iter().map(|e| e.len()).sum();
    let wide = total + 1 > 255;
    let push_off = |out: &mut Vec<u8>, off: u32| {
        if wide {
            out.extend_from_slice(&off.to_be_bytes());
        } else {
            out.push(off as u8);
        }
    };
    out.push(if wide { 4 } else { 1 }); // off_size
    let mut cursor: u32 = 1;
    push_off(&mut out, cursor);
    for e in entries {
        cursor += e.len() as u32;
        push_off(&mut out, cursor);
    }
    for e in entries {
        out.extend_from_slice(e);
    }
    out
}

/// Builds a minimal VARC table with the given coverage gids and
/// raw glyph record bytes. `var_store` and `axis_indices` are
/// optional; passing `None` leaves their offsets at zero.
fn build_varc(
    coverage_gids: &[u16],
    glyph_records: &[&[u8]],
    var_store: Option<&[u8]>,
    axis_indices: Option<&[&[u8]]>,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    let cov_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // coverage
    let vs_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // varStore
    out.extend_from_slice(&0u32.to_be_bytes()); // conditionList
    let ail_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // axisIndicesList
    let gr_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // glyphRecords

    let cov_start = out.len() as u32;
    out[cov_off_slot..cov_off_slot + 4].copy_from_slice(&cov_start.to_be_bytes());
    out.extend_from_slice(&build_coverage(coverage_gids));

    if let Some(vs) = var_store {
        let vs_start = out.len() as u32;
        out[vs_off_slot..vs_off_slot + 4].copy_from_slice(&vs_start.to_be_bytes());
        out.extend_from_slice(vs);
    }
    if let Some(ail) = axis_indices {
        let ail_start = out.len() as u32;
        out[ail_off_slot..ail_off_slot + 4].copy_from_slice(&ail_start.to_be_bytes());
        out.extend_from_slice(&build_cff2_index(ail));
    }
    let gr_start = out.len() as u32;
    out[gr_off_slot..gr_off_slot + 4].copy_from_slice(&gr_start.to_be_bytes());
    out.extend_from_slice(&build_cff2_index(glyph_records));
    out
}

#[test]
fn parses_minimal_header_and_coverage() {
    let bytes = build_varc(&[42, 100], &[b"\x00\x00\x05", b"\x00\x00\x06"], None, None);
    let varc = Varc::parse(&bytes).unwrap();
    assert!(varc.covers(42));
    assert!(varc.covers(100));
    assert!(!varc.covers(99));
    assert_eq!(varc.glyph_record_count(), 2);
}

#[test]
fn rejects_wrong_major_version() {
    let mut bytes = build_varc(&[1], &[b"\x00\x00\x01"], None, None);
    bytes[0] = 0;
    bytes[1] = 2; // major = 2
    assert!(matches!(Varc::parse(&bytes), Err(Error::Malformed { .. })));
}

#[test]
fn uint32var_roundtrip() {
    // 0x42 = single byte
    let mut r = Reader::new(&[0x42]);
    assert_eq!(read_uint32var(&mut r).unwrap(), 0x42);
    // 0x80 0x42 = (0 << 8) | 0x42
    let mut r = Reader::new(&[0x80, 0x42]);
    assert_eq!(read_uint32var(&mut r).unwrap(), 0x42);
    // 0xC1 0x02 0x03 = (1 << 16) | (2 << 8) | 3
    let mut r = Reader::new(&[0xC1, 0x02, 0x03]);
    assert_eq!(read_uint32var(&mut r).unwrap(), 0x10203);
    // 0xE0 0x01 0x02 0x03 = (0 << 24) | ...
    let mut r = Reader::new(&[0xE0, 0x01, 0x02, 0x03]);
    assert_eq!(read_uint32var(&mut r).unwrap(), 0x01_0203);
    // 0xF0 + u32
    let mut r = Reader::new(&[0xF0, 0xAB, 0xCD, 0xEF, 0x01]);
    assert_eq!(read_uint32var(&mut r).unwrap(), 0xABCD_EF01);
}

#[test]
fn resolves_translation_only_component() {
    // One component: flags = HAVE_TRANSLATE_X | HAVE_TRANSLATE_Y,
    // gid = 5, tx = 100, ty = -50.
    let flags = VC_HAVE_TRANSLATE_X | VC_HAVE_TRANSLATE_Y;
    // uint32var encoding for `flags`. flags = 0x30 (HAVE_TRANSLATE_X = 1<<4 = 0x10,
    // HAVE_TRANSLATE_Y = 1<<5 = 0x20). 0x30 < 0x80, so single byte.
    assert!(flags < 0x80);
    let mut record = Vec::new();
    record.push(flags as u8);
    record.extend_from_slice(&5u16.to_be_bytes()); // gid
    record.extend_from_slice(&100i16.to_be_bytes()); // tx
    record.extend_from_slice(&(-50i16).to_be_bytes()); // ty

    let bytes = build_varc(&[42], &[&record], None, None);
    let varc = Varc::parse(&bytes).unwrap();
    let comp = varc.composite(42, &[]).unwrap();
    assert_eq!(comp.components.len(), 1);
    assert_eq!(comp.components[0].gid, 5);
    // Identity matrix + translation.
    let t = comp.components[0].transform;
    assert!((t[0] - 1.0).abs() < 1e-5);
    assert!(t[1].abs() < 1e-5);
    assert!(t[2].abs() < 1e-5);
    assert!((t[3] - 1.0).abs() < 1e-5);
    assert!((t[4] - 100.0).abs() < 1e-3);
    assert!((t[5] - -50.0).abs() < 1e-3);
}

#[test]
fn resolves_scale_only_component() {
    // flags = VC_HAVE_SCALE_X | VC_HAVE_SCALE_Y = 0x300.
    // Two-byte uint32var: 0x80|0x03, 0x00.
    let mut record = Vec::new();
    record.push(0x80 | 0x03);
    record.push(0x00);
    record.extend_from_slice(&7u16.to_be_bytes());
    // Scale 2.0 in F6.10 = 2.0 * 1024 = 2048
    record.extend_from_slice(&2048i16.to_be_bytes());
    record.extend_from_slice(&512i16.to_be_bytes()); // 0.5
    let bytes = build_varc(&[1], &[&record], None, None);
    let varc = Varc::parse(&bytes).unwrap();
    let comp = varc.composite(1, &[]).unwrap();
    let t = comp.components[0].transform;
    assert!((t[0] - 2.0).abs() < 1e-3, "xx={}", t[0]);
    assert!((t[3] - 0.5).abs() < 1e-3, "yy={}", t[3]);
}

#[test]
fn gid_24bit_flag_reads_three_byte_glyph_id() {
    // flags = VC_GID_IS_24BIT = 0x1000.
    // Two-byte uint32var: 0x80|0x10 = 0x90, 0x00.
    let mut record = Vec::new();
    record.push(0x90);
    record.push(0x00);
    // 24-bit gid 0x010203, but u16 truncation in the API means
    // we should pick a value that fits in u16.
    record.extend_from_slice(&[0x00, 0x12, 0x34]);
    let bytes = build_varc(&[1], &[&record], None, None);
    let varc = Varc::parse(&bytes).unwrap();
    let comp = varc.composite(1, &[]).unwrap();
    assert_eq!(comp.components[0].gid, 0x1234);
}

#[test]
fn uncovered_gid_returns_none() {
    let bytes = build_varc(&[1], &[b"\x00\x00\x05"], None, None);
    let varc = Varc::parse(&bytes).unwrap();
    assert!(varc.composite(99, &[]).is_none());
}

#[test]
fn multiple_components_in_one_record() {
    let flags = VC_HAVE_TRANSLATE_X;
    let mut record = Vec::new();
    // Component 1: gid 5, tx 10
    record.push(flags as u8);
    record.extend_from_slice(&5u16.to_be_bytes());
    record.extend_from_slice(&10i16.to_be_bytes());
    // Component 2: gid 7, tx 20
    record.push(flags as u8);
    record.extend_from_slice(&7u16.to_be_bytes());
    record.extend_from_slice(&20i16.to_be_bytes());
    let bytes = build_varc(&[1], &[&record], None, None);
    let varc = Varc::parse(&bytes).unwrap();
    let comp = varc.composite(1, &[]).unwrap();
    assert_eq!(comp.components.len(), 2);
    assert_eq!(comp.components[0].gid, 5);
    assert_eq!(comp.components[1].gid, 7);
    assert!((comp.components[0].transform[4] - 10.0).abs() < 1e-3);
    assert!((comp.components[1].transform[4] - 20.0).abs() < 1e-3);
}

#[test]
fn have_axes_overrides_coord_at_axis_index() {
    // axisIndices = [1] (one axis index, axis 1).
    // axisValues = [F2DOT14(0.5)] = 8192. Encode as 0x40 | 0x00 = 0x40
    // (run of 1 i16), then 8192 BE = 0x20 0x00.
    let axis_indices_payload = vec![0x00_u8, 0x01]; // run of 1 i8: index 1
    let bytes_axis_indices = build_cff2_index(&[&axis_indices_payload]);
    // Place the axis_indices at the right offset by building VARC
    // with axis_indices block.
    let flags = VC_HAVE_AXES;
    // 0x02 < 0x80 -> single byte uint32var.
    let mut record = Vec::new();
    record.push(flags as u8);
    record.extend_from_slice(&3u16.to_be_bytes()); // gid 3
    record.push(0x00); // axisIndicesIndex = 0
                       // axisValues: 1 value, F2DOT14 = 0.5 = 8192. Word run.
    record.push(0x40); // ctrl: words, run_len 1
    record.extend_from_slice(&8192i16.to_be_bytes());

    // Build VARC with the axis_indices block. We need to use the
    // separate axis_indices arg.
    let _ = bytes_axis_indices; // (we let build_varc rebuild it)
    let bytes = build_varc(&[1], &[&record], None, Some(&[&axis_indices_payload]));
    let varc = Varc::parse(&bytes).unwrap();
    let comp = varc.composite(1, &[0.0, 0.0]).unwrap();
    let coords = &comp.components[0].coords;
    // axis 0 untouched, axis 1 set to 0.5.
    assert!(coords.len() >= 2);
    assert!((coords[1] - 0.5).abs() < 1e-3);
}

#[test]
fn rotation_rotates_unit_vector() {
    // rotation = 0.5 (= 90° = 0.5 * π). F4DOT12 raw = 0.5 * 4096 = 2048.
    let flags = VC_HAVE_ROTATION;
    // 0x40 = 64 < 0x80 -> single byte.
    let mut record = Vec::new();
    record.push(flags as u8);
    record.extend_from_slice(&5u16.to_be_bytes());
    record.extend_from_slice(&2048i16.to_be_bytes());
    let bytes = build_varc(&[1], &[&record], None, None);
    let varc = Varc::parse(&bytes).unwrap();
    let comp = varc.composite(1, &[]).unwrap();
    let t = comp.components[0].transform;
    // Apply transform to (1, 0). Should land near (0, 1).
    let x_out = t[0] * 1.0 + t[1] * 0.0 + t[4];
    let y_out = t[2] * 1.0 + t[3] * 0.0 + t[5];
    assert!(x_out.abs() < 1e-3, "expected 0, got {x_out}");
    assert!((y_out - 1.0).abs() < 1e-3, "expected 1, got {y_out}");
}

#[test]
fn sincos_pi_huge_angle_terminates() {
    // Reduction used to subtract 2 until the angle fell below 1,
    // which never ends for infinity or for values where x - 2 == x.
    let (c, s) = sincos_pi(1.0e30);
    assert_eq!((c, s), (1.0, 0.0));
    assert!(sincos_pi(f32::INFINITY).0.is_nan());
    assert!(sincos_pi(f32::NEG_INFINITY).1.is_nan());
    // The remainder path agrees with the loop path, sign of zero
    // included.
    assert_eq!(sincos_pi(21.25), sincos_pi(1.25));
    assert_eq!(sincos_pi(-18.0).1.to_bits(), sincos_pi(0.0).1.to_bits());
}

/// A MultiItemVariationStore with one axis-free region (scalar 1
/// everywhere) and one subtable naming it `mentions` times, whose
/// single delta set is `mentions` copies of `i32::MAX`.
fn build_max_delta_store(mentions: u16) -> Vec<u8> {
    let mut payload = Vec::new();
    let mut left = usize::from(mentions);
    while left > 0 {
        let run = left.min(64);
        payload.push(0xC0 | (run - 1) as u8); // i32 run
        for _ in 0..run {
            payload.extend_from_slice(&i32::MAX.to_be_bytes());
        }
        left -= run;
    }
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&12u32.to_be_bytes()); // region list
    out.extend_from_slice(&1u16.to_be_bytes()); // one subtable
    out.extend_from_slice(&20u32.to_be_bytes()); // subtable offset
                                                 // Region list at 12: one region at relative offset 6.
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&6u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // zero axes
                                                // Subtable at 20.
    out.push(1);
    out.extend_from_slice(&mentions.to_be_bytes());
    for _ in 0..mentions {
        out.extend_from_slice(&0u16.to_be_bytes());
    }
    out.extend_from_slice(&build_cff2_index(&[&payload]));
    out
}

#[test]
fn huge_rotation_delta_does_not_hang() {
    // 128 deltas of i32::MAX push the rotation to 2^26 half turns,
    // where subtracting 2 no longer changes an f32.
    let store = build_max_delta_store(128);
    let flags = VC_TRANSFORM_HAS_VARIATION | VC_HAVE_ROTATION;
    let mut record = Vec::new();
    record.push(flags as u8);
    record.extend_from_slice(&5u16.to_be_bytes()); // gid
    record.push(0x00); // transform var index: outer 0, inner 0
    record.extend_from_slice(&0i16.to_be_bytes()); // rotation
    let bytes = build_varc(&[1], &[&record], Some(&store), None);
    let varc = Varc::parse(&bytes).unwrap();
    let comp = varc.composite(1, &[]).unwrap();
    assert_eq!(comp.components.len(), 1);
}

#[test]
fn wide_axis_components_stop_at_coord_budget() {
    // Each 6-byte component lists axis 65535, so its coord vector
    // grows to 65536 values. 100 000 of them used to allocate
    // about 26 GB.
    let axis_indices: &[u8] = &[0x40, 0xFF, 0xFF]; // one i16: 65535
    let component: [u8; 6] = [VC_HAVE_AXES as u8, 0x00, 0x05, 0x00, 0x00, 0x00];
    let record = component.repeat(100_000);
    let bytes = build_varc(&[1], &[&record], None, Some(&[axis_indices]));
    let varc = Varc::parse(&bytes).unwrap();
    let comp = varc.composite(1, &[]).unwrap();
    assert_eq!(comp.components.len(), MAX_COMPOSITE_COORDS / 65536);
    assert!(comp.components.iter().all(|c| c.coords.len() == 65536));
}

/// A MultiItemVariationStore with the given sparse regions, each a list
/// of `(axis, start, peak, end)`, and one subtable over all of them
/// whose delta sets are the given TupleValues streams.
fn build_store(regions: &[&[(u16, f32, f32, f32)]], delta_sets: &[&[u8]]) -> Vec<u8> {
    let f2 = |v: f32| ((v * 16384.0) as i16).to_be_bytes();
    let mut list = Vec::new();
    list.extend_from_slice(&(regions.len() as u16).to_be_bytes());
    let mut bodies = Vec::new();
    for region in regions {
        let off = 2 + 4 * regions.len() + bodies.len();
        list.extend_from_slice(&(off as u32).to_be_bytes());
        bodies.extend_from_slice(&(region.len() as u16).to_be_bytes());
        for &(axis, start, peak, end) in *region {
            bodies.extend_from_slice(&axis.to_be_bytes());
            bodies.extend_from_slice(&f2(start));
            bodies.extend_from_slice(&f2(peak));
            bodies.extend_from_slice(&f2(end));
        }
    }
    list.extend_from_slice(&bodies);
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&12u32.to_be_bytes()); // region list
    out.extend_from_slice(&1u16.to_be_bytes()); // one subtable
    let sub_off = 12 + list.len();
    out.extend_from_slice(&(sub_off as u32).to_be_bytes());
    out.extend_from_slice(&list);
    out.push(1);
    out.extend_from_slice(&(regions.len() as u16).to_be_bytes());
    for i in 0..regions.len() {
        out.extend_from_slice(&(i as u16).to_be_bytes());
    }
    out.extend_from_slice(&build_cff2_index(delta_sets));
    out
}

/// One component of glyph 5 that sets axis 0 to `value` (F2DOT14
/// units) with a delta of `delta` units at axis 0 = +1.
fn varied_axis_value_table(value: i16, delta: i8) -> Vec<u8> {
    let store = build_store(&[&[(0, 0.0, 1.0, 1.0)]], &[&[0x00, delta as u8]]);
    let flags = VC_HAVE_AXES | VC_AXIS_VALUES_HAVE_VARIATION;
    let mut record = vec![flags as u8, 0x00, 0x05]; // flags, gid 5
    record.push(0); // axisIndicesIndex
    record.push(0x40); // one i16 axis value
    record.extend_from_slice(&value.to_be_bytes());
    record.push(0); // axisValuesVarIndex: outer 0, inner 0
    build_varc(&[1], &[&record], Some(&store), Some(&[&[0x00, 0x00]]))
}

#[test]
fn axis_values_plus_deltas_round_to_f2dot14() {
    // HarfBuzz adds the deltas to the axis values in F2DOT14 units and
    // rounds the sum to a whole F2DOT14 value, halves up.
    let child_coord = |value: i16, delta: i8, coord: f32| {
        let bytes = varied_axis_value_table(value, delta);
        let varc = Varc::parse(&bytes).unwrap();
        varc.composite(1, &[coord]).unwrap().components[0].coords[0]
    };
    // 8192 + 0.5 rounds up to 8193.
    assert_eq!(child_coord(8192, 1, 0.5), 8193.0 / 16384.0);
    // 8192 + 0.25 rounds down to 8192.
    assert_eq!(child_coord(8192, 1, 0.25), 0.5);
    // -8192 + 0.5 is a half: up, toward positive infinity.
    assert_eq!(child_coord(-8192, 1, 0.5), -8191.0 / 16384.0);
    // -8192 - 0.5 rounds up too.
    assert_eq!(child_coord(-8192, -1, 0.5), -8192.0 / 16384.0);
    // 8192 + 0.75 rounds to 8193.
    assert_eq!(child_coord(8192, 3, 0.25), 8193.0 / 16384.0);
    // At the default instance no delta applies.
    let bytes = varied_axis_value_table(8192, 100);
    let varc = Varc::parse(&bytes).unwrap();
    assert_eq!(varc.composite(1, &[]).unwrap().components[0].coords, [0.5]);
}
