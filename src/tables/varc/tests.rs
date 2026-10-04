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
pub(crate) fn build_varc(
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
    // Deltas apply only away from the default instance; the store's
    // one region has no axes, so its scalar is 1 at any coords.
    let comp = varc.composite(1, &[0.0]).unwrap();
    assert_eq!(comp.components.len(), 1);
}

#[test]
fn wide_axis_components_stop_at_the_work_budget() {
    // Each 6-byte component lists axis 4095, the last one HarfBuzz
    // keeps, so its coord vector grows to 4096 values. 100 000 of them
    // would allocate about 1.6 GB. Each costs 4098 units (the record,
    // its one axis value, its 4096 coords) after the one unit that
    // keeps the empty coords, so 255 fit.
    let axis_indices: &[u8] = &[0x40, 0x0F, 0xFF]; // one i16: 4095
    let component: [u8; 6] = [VC_HAVE_AXES as u8, 0x00, 0x05, 0x00, 0x00, 0x00];
    let record = component.repeat(100_000);
    let bytes = build_varc(&[1], &[&record], None, Some(&[axis_indices]));
    let varc = Varc::parse(&bytes).unwrap();
    let comp = varc.composite(1, &[]).unwrap();
    assert_eq!(comp.components.len() as u64, (MAX_WALK_WORK - 1) / 4098);
    assert_eq!(comp.components.len(), 255);
    assert!(comp.components.iter().all(|c| c.coords.len() == 4096));
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

/// The first component of glyph 1 in a table with `record` and the
/// given store and axis indices lists.
fn first(
    record: &[u8],
    store: Option<&[u8]>,
    lists: Option<&[&[u8]]>,
    coords: &[f32],
) -> VarcComposite {
    let bytes = build_varc(&[1], &[record], store, lists);
    Varc::parse(&bytes).unwrap().composite(1, coords).unwrap()
}

#[test]
fn scale_y_defaults_to_scale_x() {
    // The spec's default for ScaleY is ScaleX, not 1.
    let mut record = vec![0x81, 0x00, 0x00, 0x05]; // uint32var HAVE_SCALE_X, gid 5
    record.extend_from_slice(&1536i16.to_be_bytes()); // 1.5
    let c = first(&record, None, None, &[]);
    let t = c.components[0].transform;
    assert!(
        (t[0] - 1.5).abs() < 1e-6 && (t[3] - 1.5).abs() < 1e-6,
        "{t:?}"
    );
}

#[test]
fn reserved_flag_bits_each_skip_one_uint32var() {
    // Bits 15 and 20 are reserved. Each costs one uint32var after the
    // transform fields, which readers skip, and the next component
    // reads as usual.
    let flags = (1u32 << 15) | (1 << 20) | VC_HAVE_TRANSLATE_X;
    let mut record = vec![0xC0 | (flags >> 16) as u8, (flags >> 8) as u8, flags as u8];
    record.extend_from_slice(&[0x00, 0x05]); // gid 5
    record.extend_from_slice(&7i16.to_be_bytes()); // translate x
    record.extend_from_slice(&[0x05, 0x81, 0x2C]); // two reserved uint32vars
    record.extend_from_slice(&[VC_HAVE_TRANSLATE_X as u8, 0x00, 0x06]);
    record.extend_from_slice(&9i16.to_be_bytes());
    let c = first(&record, None, None, &[]);
    let got: Vec<(u16, f32)> = c
        .components
        .iter()
        .map(|c| (c.gid, c.transform[4]))
        .collect();
    assert_eq!(got, [(5, 7.0), (6, 9.0)]);
}

#[test]
fn axis_values_var_index_is_read_without_axes() {
    // AXIS_VALUES_HAVE_VARIATION brings its index whether or not the
    // component has axes, so the translation after it reads right.
    let flags = VC_AXIS_VALUES_HAVE_VARIATION | VC_HAVE_TRANSLATE_X;
    let mut record = vec![flags as u8, 0x00, 0x05, 0x00];
    record.extend_from_slice(&200i16.to_be_bytes());
    let c = first(&record, None, None, &[0.5]);
    assert_eq!(c.components[0].transform[4], 200.0);
}

#[test]
fn delta_sets_hold_one_tuple_per_region() {
    // Two regions (axis 0 and axis 1, each peaking at +1) and two
    // translations. The delta set is region 0's tuple, then region 1's:
    // [10, 20] then [-3, 5].
    let store = build_store(
        &[&[(0, 0.0, 1.0, 1.0)], &[(1, 0.0, 1.0, 1.0)]],
        &[&[0x03, 10, 20, (-3i8) as u8, 5]],
    );
    let flags = VC_TRANSFORM_HAS_VARIATION | VC_HAVE_TRANSLATE_X | VC_HAVE_TRANSLATE_Y;
    let mut record = vec![flags as u8, 0x00, 0x05, 0x00];
    record.extend_from_slice(&0i16.to_be_bytes());
    record.extend_from_slice(&0i16.to_be_bytes());
    let t = |coords: &[f32]| {
        let c = first(&record, Some(&store), None, coords);
        (c.components[0].transform[4], c.components[0].transform[5])
    };
    assert_eq!(t(&[1.0, 0.0]), (10.0, 20.0));
    assert_eq!(t(&[0.0, 1.0]), (-3.0, 5.0));
    assert_eq!(t(&[0.5, 1.0]), (2.0, 15.0));
    // A delta set that ends before it fills both tuples adds the values
    // it holds, as HarfBuzz does: region 0's tuple, then the first value
    // of region 1's.
    let short = build_store(
        &[&[(0, 0.0, 1.0, 1.0)], &[(1, 0.0, 1.0, 1.0)]],
        &[&[0x02, 10, 20, 7]],
    );
    let c = first(&record, Some(&short), None, &[1.0, 1.0]);
    assert_eq!(
        (c.components[0].transform[4], c.components[0].transform[5]),
        (17.0, 20.0)
    );
    // At axis 1 alone only region 1 counts: the reader skips region
    // 0's tuple, and region 1's holds one value.
    let c = first(&record, Some(&short), None, &[0.0, 1.0]);
    assert_eq!(
        (c.components[0].transform[4], c.components[0].transform[5]),
        (7.0, 0.0)
    );
    // Extra values past the last tuple are ignored.
    let long = build_store(
        &[&[(0, 0.0, 1.0, 1.0)], &[(1, 0.0, 1.0, 1.0)]],
        &[&[0x05, 10, 20, (-3i8) as u8, 5, 99, 99]],
    );
    let c = first(&record, Some(&long), None, &[1.0, 1.0]);
    assert_eq!(
        (c.components[0].transform[4], c.components[0].transform[5]),
        (7.0, 25.0)
    );
}

#[test]
fn a_run_cut_short_ends_one_tuple_at_its_control_byte() {
    // Region 0's tuple starts with a run of two words that the one
    // byte after it cannot hold. HarfBuzz's reader gives up on that
    // tuple past the control byte, and region 1's tuple starts at the
    // next byte, read as a control byte: one i8, 5.
    let store = build_store(
        &[&[(0, 0.0, 1.0, 1.0)], &[(1, 0.0, 1.0, 1.0)]],
        &[&[0x41, 0x00, 0x05]],
    );
    let flags = VC_TRANSFORM_HAS_VARIATION | VC_HAVE_TRANSLATE_X | VC_HAVE_TRANSLATE_Y;
    let mut record = vec![flags as u8, 0x00, 0x05, 0x00];
    record.extend_from_slice(&0i16.to_be_bytes());
    record.extend_from_slice(&0i16.to_be_bytes());
    let c = first(&record, Some(&store), None, &[1.0, 1.0]);
    assert_eq!(
        (c.components[0].transform[4], c.components[0].transform[5]),
        (5.0, 0.0)
    );
    // Skipping region 0 (scalar zero) stops at the same control byte,
    // so region 1 reads the same way.
    let c = first(&record, Some(&store), None, &[0.0, 1.0]);
    assert_eq!(
        (c.components[0].transform[4], c.components[0].transform[5]),
        (5.0, 0.0)
    );
}

#[test]
fn deltas_skip_the_default_instance_and_no_variation() {
    // A region without axes has scalar 1 everywhere, but HarfBuzz adds
    // no deltas when the font has no coords, nor for index 0xFFFFFFFF.
    let store = build_store(&[&[]], &[&[0x00, 50]]);
    let mut record = vec![
        (VC_TRANSFORM_HAS_VARIATION | VC_HAVE_TRANSLATE_X) as u8,
        0x00,
        0x05,
    ];
    record.push(0x00); // index 0
    record.extend_from_slice(&1i16.to_be_bytes());
    assert_eq!(
        first(&record, Some(&store), None, &[]).components[0].transform[4],
        1.0
    );
    assert_eq!(
        first(&record, Some(&store), None, &[0.0]).components[0].transform[4],
        51.0
    );
    let mut none = vec![
        (VC_TRANSFORM_HAS_VARIATION | VC_HAVE_TRANSLATE_X) as u8,
        0x00,
        0x05,
    ];
    none.extend_from_slice(&[0xF0, 0xFF, 0xFF, 0xFF, 0xFF]);
    none.extend_from_slice(&1i16.to_be_bytes());
    assert_eq!(
        first(&none, Some(&store), None, &[0.0]).components[0].transform[4],
        1.0
    );
}

#[test]
fn axis_indices_past_harfbuzz_limit_are_ignored() {
    // HarfBuzz keeps at most 4096 component axes and drops a write to
    // a later one. Axis 65535 used to grow the vector to 65536 coords.
    let lists: &[&[u8]] = &[&[0x41, 0xFF, 0xFF, 0x00, 0x01]]; // 65535, then 1
    let mut record = vec![VC_HAVE_AXES as u8, 0x00, 0x05, 0x00];
    record.extend_from_slice(&[0x41, 0x20, 0x00, 0x10, 0x00]); // 0.5, 0.25
    let c = first(&record, None, Some(lists), &[0.1]);
    assert_eq!(c.components[0].coords, [0.1, 0.25]);
}

#[test]
fn axis_indices_index_past_the_list_names_no_axes() {
    // As in HarfBuzz, an out-of-range index is an empty tuple: no axis
    // values follow, and the next bytes are the next fields.
    let lists: &[&[u8]] = &[&[0x00, 0x00]];
    let flags = VC_HAVE_AXES | VC_HAVE_TRANSLATE_X;
    let mut record = vec![flags as u8, 0x00, 0x05, 0x07];
    record.extend_from_slice(&33i16.to_be_bytes());
    let c = first(&record, None, Some(lists), &[0.1]);
    assert_eq!(c.components[0].coords, [0.1]);
    assert_eq!(c.components[0].transform[4], 33.0);
}

#[test]
fn glyph_ids_past_16_bits_draw_nothing() {
    // A 24-bit glyph id past 65535 names no glyph of a 16-bit face. It
    // used to wrap to a low id (0x010005 drew glyph 5).
    let flags = VC_GID_IS_24BIT;
    let mut record = vec![0x80 | (flags >> 8) as u8, flags as u8, 0x01, 0x00, 0x05];
    record.extend_from_slice(&[0x00, 0x00, 0x06]); // then glyph 6
    let c = first(&record, None, None, &[]);
    let gids: Vec<u16> = c.components.iter().map(|c| c.gid).collect();
    assert_eq!(gids, [6]);
}

#[test]
fn axis_indices_walk_malformed_streams_like_harfbuzz() {
    assert_eq!(decode_axis_indices(&[0x00, 3, 0x81]), [3, 0, 0]);
    // A run of two words with one byte left yields a zero, then the
    // walk reads that byte as the next control.
    assert_eq!(decode_axis_indices(&[0x41, 0x00]), [0]);
    // Negative values name no axis.
    assert_eq!(decode_axis_indices(&[0x00, 0xFF]), [u32::MAX]);
    assert!(decode_axis_indices(&[]).is_empty());
}

/// `table` with `conditions` appended as its ConditionList.
pub(crate) fn with_conditions(mut table: Vec<u8>, conditions: &[u8]) -> Vec<u8> {
    let at = table.len() as u32;
    table[12..16].copy_from_slice(&at.to_be_bytes());
    table.extend_from_slice(conditions);
    table
}

fn f2dot14(v: f32) -> [u8; 2] {
    ((v * 16384.0) as i16).to_be_bytes()
}

fn cond_axis(axis: u16, min: f32, max: f32) -> Vec<u8> {
    let mut out = vec![0, 1];
    out.extend_from_slice(&axis.to_be_bytes());
    out.extend_from_slice(&f2dot14(min));
    out.extend_from_slice(&f2dot14(max));
    out
}

/// And (3) or Or (4) over `children`, laid out after the offsets.
fn cond_op(format: u8, children: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0, format, children.len() as u8];
    let mut at = 3 + 3 * children.len();
    for c in children {
        out.extend_from_slice(&(at as u32).to_be_bytes()[1..]);
        at += c.len();
    }
    for c in children {
        out.extend_from_slice(c);
    }
    out
}

fn cond_not(child: Option<&[u8]>) -> Vec<u8> {
    let mut out = vec![0, 5, 0, 0, if child.is_some() { 5 } else { 0 }];
    out.extend_from_slice(child.unwrap_or_default());
    out
}

pub(crate) fn condition_list(conditions: &[Vec<u8>]) -> Vec<u8> {
    let mut out = (conditions.len() as u32).to_be_bytes().to_vec();
    let mut at = 4 + 4 * conditions.len();
    for c in conditions {
        out.extend_from_slice(&(at as u32).to_be_bytes());
        at += c.len();
    }
    for c in conditions {
        out.extend_from_slice(c);
    }
    out
}

/// A component of glyph `gid` gated by condition `index`.
fn gated(gid: u16, index: u8) -> Vec<u8> {
    // HAVE_CONDITION is 0x80, which takes the two-byte uint32var form.
    let mut out = vec![0x80, VC_HAVE_CONDITION as u8];
    out.extend_from_slice(&gid.to_be_bytes());
    out.push(index);
    out
}

#[test]
fn conditions_gate_components() {
    let store = build_store(&[&[(0, 0.0, 1.0, 1.0)]], &[&[0x00, 2]]);
    let mut value = vec![0, 2];
    value.extend_from_slice(&(-1i16).to_be_bytes());
    value.extend_from_slice(&0u32.to_be_bytes()); // -1 + 2 at axis 0 = +1
    let conditions = condition_list(&[
        cond_axis(0, 0.25, 1.0),
        value,
        cond_op(
            3,
            &[
                cond_axis(1, -1.0, 0.0),
                cond_not(Some(&cond_axis(0, 0.5, 1.0))),
            ],
        ),
        cond_op(4, &[cond_axis(0, -1.0, -0.5), cond_axis(1, 0.5, 1.0)]),
        cond_not(None),
        vec![0, 9, 0, 0],
        vec![0, 3, 3, 0, 0, 6],
    ]);
    let mut record = Vec::new();
    for i in 0..7u8 {
        record.extend_from_slice(&gated(10 + u16::from(i), i));
    }
    record.extend_from_slice(&gated(99, 99));
    record.extend_from_slice(&[0x00, 0x00, 0x05]); // ungated glyph 5
    let table = with_conditions(
        build_varc(&[1], &[&record], Some(&store), None),
        &conditions,
    );
    let varc = Varc::parse(&table).unwrap();
    let shown = |coords: &[f32]| -> Vec<u16> {
        let c = varc.composite(1, coords).unwrap();
        c.components.iter().map(|c| c.gid).collect()
    };
    // 10: axis range; 11: value plus delta; 12: And with Negate; 13:
    // Or; 14: Negate of the Null condition; 15: unknown format; 16: And
    // cut short; 99: past the list.
    assert_eq!(shown(&[]), [12, 14, 5]);
    assert_eq!(shown(&[0.0, 0.0]), [12, 14, 5]);
    assert_eq!(shown(&[0.25, 0.0]), [10, 12, 14, 5]);
    assert_eq!(shown(&[0.75, -0.5]), [10, 11, 14, 5]);
    assert_eq!(shown(&[0.75, 0.5]), [10, 11, 13, 14, 5]);
    assert_eq!(shown(&[-0.5, 0.0]), [12, 13, 14, 5]);
    // A coord off the F2DOT14 grid compares after rounding.
    assert_eq!(shown(&[0.25 - 1.0 / 65536.0, 0.0]), [10, 12, 14, 5]);
}

#[test]
fn conditions_nested_past_64_levels_fail() {
    // A chain of And tables with one child each, ending in a condition
    // that holds. HarfBuzz's sanitizer drops a chain deeper than 64.
    let chain = |levels: usize| {
        let mut c = cond_axis(0, -1.0, 1.0);
        for _ in 0..levels {
            c = cond_op(3, &[c]);
        }
        let table = with_conditions(
            build_varc(&[1], &[&gated(10, 0)], None, None),
            &condition_list(&[c]),
        );
        let varc = Varc::parse(&table).unwrap();
        varc.composite(1, &[0.0]).unwrap().components.len()
    };
    assert_eq!(chain(60), 1);
    assert_eq!(chain(64), 0);
    assert_eq!(chain(70), 0);
}

#[test]
fn shared_condition_trees_are_evaluated_once() {
    // Five levels of And tables whose 255 offsets all name the next
    // level: 255^5 paths. Each table's result is kept for the call, so
    // the walk visits each level's 255 offsets once and the component,
    // whose every leaf holds, shows.
    let level_len = 3 + 3 * 255;
    let mut conditions = Vec::new();
    for _ in 0..5 {
        conditions.extend_from_slice(&[0, 3, 255]);
        for _ in 0..255 {
            conditions.extend_from_slice(&(level_len as u32).to_be_bytes()[1..]);
        }
    }
    conditions.extend_from_slice(&cond_axis(0, -1.0, 1.0));
    let table = with_conditions(
        build_varc(&[1], &[&gated(10, 0)], None, None),
        &condition_list(&[conditions]),
    );
    let varc = Varc::parse(&table).unwrap();
    let mut memo = VarcMemo::new();
    let c = varc.composite_in(1, &[0.0], &[0.0], &mut memo).unwrap();
    assert_eq!(c.components.len(), 1);
    assert_eq!(memo.condition_visits(), 1 + 5 * 255);
}

/// A MultiItemVariationStore with one region (axis 0: 0, 1, 1) and one
/// subtable that names it `mentions` times, whose one delta set holds a
/// delta of 1 per mention.
pub(crate) fn mention_store(mentions: u16) -> Vec<u8> {
    let mut store = Vec::new();
    store.extend_from_slice(&1u16.to_be_bytes()); // format
    store.extend_from_slice(&12u32.to_be_bytes()); // region list
    store.extend_from_slice(&1u16.to_be_bytes()); // one subtable
    store.extend_from_slice(&28u32.to_be_bytes()); // its offset
    store.extend_from_slice(&1u16.to_be_bytes()); // region count, at 12
    store.extend_from_slice(&6u32.to_be_bytes());
    store.extend_from_slice(&1u16.to_be_bytes()); // one axis
    store.extend_from_slice(&0u16.to_be_bytes());
    store.extend_from_slice(&f2dot14(0.0));
    store.extend_from_slice(&f2dot14(1.0));
    store.extend_from_slice(&f2dot14(1.0));
    assert_eq!(store.len(), 28);
    store.push(1); // subtable format
    store.extend_from_slice(&mentions.to_be_bytes());
    for _ in 0..mentions {
        store.extend_from_slice(&0u16.to_be_bytes());
    }
    // Runs of up to 64 i8 deltas of 1.
    let mut deltas = Vec::new();
    let mut left = usize::from(mentions);
    while left > 0 {
        let run = left.min(64);
        deltas.push((run - 1) as u8);
        deltas.extend(core::iter::repeat(1u8).take(run));
        left -= run;
    }
    store.extend_from_slice(&build_cff2_index(&[&deltas]));
    store
}

/// A chain of `levels` And tables, each with two offsets that both name
/// the next, ending in the Value condition `default + delta > 0`, whose
/// delta is variation index 0.
pub(crate) fn deep_value_condition(levels: usize, default: i16) -> Vec<u8> {
    let mut condition = vec![0, 2];
    condition.extend_from_slice(&default.to_be_bytes());
    condition.extend_from_slice(&0u32.to_be_bytes());
    for _ in 0..levels {
        // And with two offsets, both to the child right after them.
        let mut and = vec![0, 3, 2];
        and.extend_from_slice(&9u32.to_be_bytes()[1..]);
        and.extend_from_slice(&9u32.to_be_bytes()[1..]);
        and.extend_from_slice(&condition);
        condition = and;
    }
    condition
}

/// A VARC table with one component of glyph 10 gated by
/// [`deep_value_condition`] over [`mention_store`].
fn deep_value_condition_table(levels: usize, mentions: u16, default: i16) -> Vec<u8> {
    with_conditions(
        build_varc(&[1], &[&gated(10, 0)], Some(&mention_store(mentions)), None),
        &condition_list(&[deep_value_condition(levels, default)]),
    )
}

#[test]
fn deep_shared_value_conditions_cost_linear_work() {
    // 20 And levels whose two offsets name the same child (2^20 paths)
    // over a Value condition whose delta set walks 60,000 mentions of
    // one region. Each table is evaluated once and the subtable's
    // scalars are worked out once: 41 table visits, 60,000 scalar steps
    // and one region axis, and 60,000 delta values, besides keeping the
    // coords (2 units), the record (1), the 40 And offsets (40) and the
    // component's coords (1). Walking every path took 2^20 times that.
    let table = deep_value_condition_table(20, 60_000, -30_000);
    let varc = Varc::parse(&table).unwrap();
    // At axis 0 = 1 the deltas add 60,000: the condition holds.
    let mut memo = VarcMemo::new();
    let c = varc.composite_in(1, &[1.0], &[1.0], &mut memo).unwrap();
    assert_eq!(c.components.len(), 1);
    assert_eq!(memo.condition_visits(), 41);
    assert_eq!(memo.work_done(), 120_045);
    // At 0.25 they add 15,000: it does not.
    assert!(varc.composite(1, &[0.25]).unwrap().components.is_empty());
}

#[test]
fn delta_work_past_the_budget_ends_the_composite() {
    // Twenty components whose translation varies through the delta set
    // of 60,000 region mentions. The first costs 120,004 units (its
    // record and coords, the scalars and their one axis, then the
    // walk), each later one 60,002, after the 2 that keep the coords:
    // 16 fit the 2^20 budget. The 17th is read, but its deltas do not
    // fit, so it keeps its stored translation, and the list ends there.
    let flags = (VC_TRANSFORM_HAS_VARIATION | VC_HAVE_TRANSLATE_X) as u8;
    let mut record = Vec::new();
    for _ in 0..20 {
        record.extend_from_slice(&[flags, 0x00, 0x05, 0x00]);
        record.extend_from_slice(&0i16.to_be_bytes());
    }
    let table = build_varc(&[1], &[&record], Some(&mention_store(60_000)), None);
    let varc = Varc::parse(&table).unwrap();
    let c = varc.composite(1, &[1.0]).unwrap();
    let tx: Vec<f32> = c.components.iter().map(|c| c.transform[4]).collect();
    assert_eq!(tx[..16], [60_000.0; 16]);
    assert_eq!(tx[16..], [0.0]);
}

#[test]
fn a_memo_keeps_composites_and_scalars_by_coords() {
    // Resolving glyph 1 again at the same coords costs nothing; at
    // other coords it costs its record, coords and deltas again.
    let flags = (VC_TRANSFORM_HAS_VARIATION | VC_HAVE_TRANSLATE_X) as u8;
    let mut record = vec![flags, 0x00, 0x05, 0x00];
    record.extend_from_slice(&0i16.to_be_bytes());
    let table = build_varc(&[1], &[&record], Some(&mention_store(1000)), None);
    let varc = Varc::parse(&table).unwrap();
    let mut memo = VarcMemo::new();
    let first = varc.resolve(1, &[1.0], &[], &mut memo).unwrap();
    // Keeping [1.0] (2), the record (1), its coords (1), the scalars
    // (1000 and one axis) and the walk (1000).
    assert_eq!(memo.work_done(), 2005);
    let again = varc.resolve(1, &[1.0], &[], &mut memo).unwrap();
    assert!(Rc::ptr_eq(&first, &again));
    assert_eq!(memo.work_done(), 2005);
    let half = varc.resolve(1, &[0.5], &[], &mut memo).unwrap();
    assert_eq!(half.components[0].transform[4], 500.0);
    assert_eq!(memo.work_done(), 2 * 2005);
    // A glyph VARC has no record for costs nothing.
    assert!(varc.resolve(5, &[0.25], &[], &mut memo).is_none());
    assert_eq!(memo.work_done(), 2 * 2005);
}

#[test]
fn reset_unspecified_axes_starts_from_the_font_coords() {
    // Two components setting axis 0 to 0.5, one with
    // RESET_UNSPECIFIED_AXES, and a third that resets without axes.
    let lists: &[&[u8]] = &[&[0x00, 0x00]];
    let mut record = Vec::new();
    for flags in [VC_HAVE_AXES, VC_HAVE_AXES | VC_RESET_UNSPECIFIED_AXES] {
        record.extend_from_slice(&[flags as u8, 0x00, 0x05, 0x00, 0x40, 0x20, 0x00]);
    }
    record.extend_from_slice(&[VC_RESET_UNSPECIFIED_AXES as u8, 0x00, 0x06]);
    let table = build_varc(&[1], &[&record], None, Some(lists));
    let varc = Varc::parse(&table).unwrap();
    let coords = |c: &VarcComposite| -> Vec<Vec<f32>> {
        c.components.iter().map(|c| c.coords.clone()).collect()
    };
    let nested = varc
        .composite_with_font_coords(1, &[0.7, 0.3], &[0.1, 0.2])
        .unwrap();
    assert_eq!(
        coords(&nested),
        [vec![0.5, 0.3], vec![0.5, 0.2], vec![0.1, 0.2]]
    );
    // For a glyph drawn on its own the font's coords are its own.
    let top = varc.composite(1, &[0.7, 0.3]).unwrap();
    assert_eq!(
        coords(&top),
        [vec![0.5, 0.3], vec![0.5, 0.3], vec![0.7, 0.3]]
    );
}

#[test]
fn coords_past_harfbuzz_limit_start_from_the_font_coords() {
    // HarfBuzz copies at most 4096 coords, and starts a component of a
    // glyph with more from the font's coords.
    let table = build_varc(&[1], &[&[0x00, 0x00, 0x05]], None, None);
    let varc = Varc::parse(&table).unwrap();
    let wide = vec![0.5; 4097];
    let c = varc.composite_with_font_coords(1, &wide, &[0.25]).unwrap();
    assert_eq!(c.components[0].coords, [0.25]);
    let c = varc
        .composite_with_font_coords(1, &wide[..4096], &[0.25])
        .unwrap();
    assert_eq!(c.components[0].coords.len(), 4096);
}
