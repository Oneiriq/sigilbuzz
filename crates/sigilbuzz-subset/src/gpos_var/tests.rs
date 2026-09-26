//! Unit tests for the GPOS variation bake: the per-field fold, the
//! ValueRecord walks, and (in `anchors`) the anchor walks.

use super::anchors::visit_anchor;
use super::value_records::{
    value_record_size, visit_value_record, VR_X_ADVANCE, VR_X_ADVANCE_DEVICE,
};
use super::*;
use alloc::vec;

mod anchors;

/// A device slot at `slot` whose offset is measured from byte 0
/// and that adjusts the i16 at `field`.
fn subtable_slot(field: usize, slot: usize) -> DeviceSlot {
    DeviceSlot {
        base: 0,
        field: Some(field),
        slot,
    }
}

/// Points the AnchorFormat3 `xDeviceOffset` slot at `x_dev_pos` at
/// the table at `table_pos`. The anchor starts 6 bytes before the
/// slot, and the offset is measured from there.
fn set_x_device(gpos: &mut [u8], x_dev_pos: usize, table_pos: usize) {
    let rel = (table_pos - (x_dev_pos - 6)) as u16;
    gpos[x_dev_pos..x_dev_pos + 2].copy_from_slice(&rel.to_be_bytes());
}

/// Same as [`set_x_device`] for the `yDeviceOffset` slot, which
/// sits 8 bytes into the anchor.
fn set_y_device(gpos: &mut [u8], y_dev_pos: usize, table_pos: usize) {
    let rel = (table_pos - (y_dev_pos - 8)) as u16;
    gpos[y_dev_pos..y_dev_pos + 2].copy_from_slice(&rel.to_be_bytes());
}

/// Folds the Anchor at `anchor_off` the way the bake does.
fn fold_anchor_variations(
    buf: &mut [u8],
    anchor_off: usize,
    store: Option<&ItemVariationStore<'_>>,
    coords: &[f32],
) {
    visit_anchor(buf, anchor_off, &mut |b, slot| {
        fold_one_field(b, slot, store, coords);
    });
}

/// Builds a one-region one-item ItemVariationStore: at coord 1.0
/// the single item resolves to `delta`; at 0.0 it resolves to 0;
/// linear in between.
fn build_ivs_one_region_one_item(delta: i16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    let region_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // subtable count
    let subtable_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());

    #[allow(clippy::cast_possible_truncation)]
    let region_start = out.len() as u32;
    out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_start.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
    out.extend_from_slice(&1u16.to_be_bytes()); // regionCount
                                                // F2DOT14 (start, peak, end) = (0.0, 1.0, 1.0)
    out.extend_from_slice(&0i16.to_be_bytes());
    out.extend_from_slice(&16384i16.to_be_bytes());
    out.extend_from_slice(&16384i16.to_be_bytes());

    #[allow(clippy::cast_possible_truncation)]
    let sub_start = out.len() as u32;
    out[subtable_slot..subtable_slot + 4].copy_from_slice(&sub_start.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // itemCount
    out.extend_from_slice(&1u16.to_be_bytes()); // wordDeltaCount
    out.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
    out.extend_from_slice(&0u16.to_be_bytes()); // region index 0
    out.extend_from_slice(&delta.to_be_bytes());
    out
}

#[test]
fn value_record_size_matches_popcount() {
    assert_eq!(value_record_size(0), 0);
    assert_eq!(value_record_size(VR_X_ADVANCE), 2);
    assert_eq!(value_record_size(VR_X_ADVANCE | VR_X_ADVANCE_DEVICE), 4);
    assert_eq!(value_record_size(0xFF), 16);
}

#[test]
fn fold_one_field_zeros_absent_offset_noop() {
    let mut buf = vec![0u8; 8];
    // Static field at 0..2 starts at 100; offset slot at 4..6 is
    // zero (absent). Fold must be a no-op.
    buf[0..2].copy_from_slice(&100i16.to_be_bytes());
    let ivs_bytes = build_ivs_one_region_one_item(80);
    let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
    fold_one_field(&mut buf, subtable_slot(0, 4), Some(&store), &[1.0]);
    let cur = i16::from_be_bytes([buf[0], buf[1]]);
    assert_eq!(cur, 100);
}

#[test]
fn fold_one_field_resolves_variation_index_and_zeros_offset() {
    // Static field at 0..2 = 50; offset slot at 4..6 = 8 (points
    // at the VariationIndex header at byte 8). At coord 1.0 the
    // delta is 80 -> 50 + 80 = 130. After fold the offset slot is
    // zero.
    let mut buf = vec![0u8; 14];
    buf[0..2].copy_from_slice(&50i16.to_be_bytes());
    buf[4..6].copy_from_slice(&8u16.to_be_bytes());
    // VariationIndex at byte 8: outer=0, inner=0, deltaFormat=0x8000.
    buf[8..10].copy_from_slice(&0u16.to_be_bytes());
    buf[10..12].copy_from_slice(&0u16.to_be_bytes());
    buf[12..14].copy_from_slice(&0x8000u16.to_be_bytes());
    let ivs_bytes = build_ivs_one_region_one_item(80);
    let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
    fold_one_field(&mut buf, subtable_slot(0, 4), Some(&store), &[1.0]);
    let cur = i16::from_be_bytes([buf[0], buf[1]]);
    assert_eq!(cur, 130);
    assert_eq!(buf[4], 0);
    assert_eq!(buf[5], 0);
}

#[test]
fn fold_one_field_device_table_zeros_offset_only() {
    // Device-shape (deltaFormat = 3): must zero the offset but
    // leave the static field alone.
    let mut buf = vec![0u8; 14];
    buf[0..2].copy_from_slice(&50i16.to_be_bytes());
    buf[4..6].copy_from_slice(&8u16.to_be_bytes());
    buf[8..10].copy_from_slice(&8u16.to_be_bytes()); // startSize
    buf[10..12].copy_from_slice(&16u16.to_be_bytes()); // endSize
    buf[12..14].copy_from_slice(&3u16.to_be_bytes()); // deltaFormat = Device
    fold_one_field(&mut buf, subtable_slot(0, 4), None, &[]);
    let cur = i16::from_be_bytes([buf[0], buf[1]]);
    assert_eq!(cur, 50);
    assert_eq!(buf[4], 0);
    assert_eq!(buf[5], 0);
}

#[test]
fn fold_one_field_variation_without_store_zeros_offset_only() {
    let mut buf = vec![0u8; 14];
    buf[0..2].copy_from_slice(&50i16.to_be_bytes());
    buf[4..6].copy_from_slice(&8u16.to_be_bytes());
    buf[8..10].copy_from_slice(&0u16.to_be_bytes());
    buf[10..12].copy_from_slice(&0u16.to_be_bytes());
    buf[12..14].copy_from_slice(&0x8000u16.to_be_bytes());
    fold_one_field(&mut buf, subtable_slot(0, 4), None, &[1.0]);
    let cur = i16::from_be_bytes([buf[0], buf[1]]);
    assert_eq!(cur, 50);
    assert_eq!(buf[4], 0);
    assert_eq!(buf[5], 0);
}

#[test]
fn fold_one_field_saturates_at_i16_max() {
    let mut buf = vec![0u8; 14];
    buf[0..2].copy_from_slice(&30000i16.to_be_bytes());
    buf[4..6].copy_from_slice(&8u16.to_be_bytes());
    buf[8..10].copy_from_slice(&0u16.to_be_bytes());
    buf[10..12].copy_from_slice(&0u16.to_be_bytes());
    buf[12..14].copy_from_slice(&0x8000u16.to_be_bytes());
    // delta = 30000 -> 30000 + 30000 saturates at i16::MAX (32767).
    let ivs_bytes = build_ivs_one_region_one_item(30000);
    let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
    fold_one_field(&mut buf, subtable_slot(0, 4), Some(&store), &[1.0]);
    let cur = i16::from_be_bytes([buf[0], buf[1]]);
    assert_eq!(cur, i16::MAX);
}

/// PairPos fmt 1 round-trip: build a single AV pair whose
/// `valueRecord1.x_advance` carries a `VariationIndex` to a
/// non-zero IVS delta. After fold at coord 1.0 the static
/// `x_advance` equals `source + delta`, and the device offset is
/// zero.
#[test]
fn pair_pos_fmt1_x_advance_variation_folds() {
    // Lay out: GPOS header 10 bytes
    // + LookupList at offset 10:
    //   u16 lookupCount = 1
    //   u16 lookupOffset[0] = 4 (relative to LookupList start)
    // + Lookup at offset 14:
    //   u16 lookupType = 2
    //   u16 lookupFlag = 0
    //   u16 subtableCount = 1
    //   u16 subtableOffset[0] = 8 (relative to Lookup start)
    // + PairPos subtable at offset 22.

    let mut gpos = Vec::new();
    // Header
    gpos.extend_from_slice(&1u16.to_be_bytes()); // major
    gpos.extend_from_slice(&0u16.to_be_bytes()); // minor
    gpos.extend_from_slice(&100u16.to_be_bytes()); // scriptListOff (unused)
    gpos.extend_from_slice(&100u16.to_be_bytes()); // featureListOff (unused)
    gpos.extend_from_slice(&10u16.to_be_bytes()); // lookupListOff
                                                  // LookupList
    gpos.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
    gpos.extend_from_slice(&4u16.to_be_bytes()); // lookupOffset[0]
                                                 // Lookup at 14
    gpos.extend_from_slice(&2u16.to_be_bytes()); // lookupType
    gpos.extend_from_slice(&0u16.to_be_bytes()); // flag
    gpos.extend_from_slice(&1u16.to_be_bytes()); // subtableCount
    gpos.extend_from_slice(&8u16.to_be_bytes()); // subtableOffset[0]
                                                 // PairPos at 22, sub_off = 22.
    let sub_off = gpos.len();
    let value_format1 = VR_X_ADVANCE | VR_X_ADVANCE_DEVICE; // 0x44
    let value_format2 = 0u16;
    gpos.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    gpos.extend_from_slice(&0u16.to_be_bytes()); // coverageOff (filled below)
    gpos.extend_from_slice(&value_format1.to_be_bytes());
    gpos.extend_from_slice(&value_format2.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes()); // pairSetCount
    gpos.extend_from_slice(&0u16.to_be_bytes()); // pairSetOffset (filled below)
                                                 // PairSet
    let pair_set_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&1u16.to_be_bytes()); // pairValueCount
    gpos.extend_from_slice(&60u16.to_be_bytes()); // secondGlyph
                                                  // ValueRecord1: x_advance (i16) + x_advance_device (offset16).
    let x_advance_pos = gpos.len();
    gpos.extend_from_slice(&(-50i16).to_be_bytes()); // x_advance source
    let device_off_pos = gpos.len();
    // Will fill device_off below: points at the VariationIndex
    // header that we tack on at the end of the subtable.
    gpos.extend_from_slice(&0u16.to_be_bytes());
    // ValueRecord2 is empty (format2 == 0).

    // Coverage at the end of the subtable.
    let coverage_rel = (gpos.len() - sub_off) as u16;
    // Coverage format 1, glyphCount 1, glyph 50.
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&50u16.to_be_bytes());

    // VariationIndex at the end: outer=0, inner=0, deltaFormat=0x8000.
    let vi_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0x8000u16.to_be_bytes());

    // Patch slots.
    gpos[sub_off + 2..sub_off + 4].copy_from_slice(&coverage_rel.to_be_bytes());
    gpos[sub_off + 10..sub_off + 12].copy_from_slice(&pair_set_rel.to_be_bytes());
    // PairValueRecord device offsets are relative to the PairSet.
    gpos[device_off_pos..device_off_pos + 2]
        .copy_from_slice(&(vi_rel - pair_set_rel).to_be_bytes());

    // Build IVS and run the bake at coord 1.0.
    let ivs_bytes = build_ivs_one_region_one_item(75);
    let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
    let baked = bake_gpos_at_coords(&gpos, Some(&store), &[1.0]).unwrap();

    // x_advance: -50 + 75 = 25.
    let baked_x = i16::from_be_bytes([baked[x_advance_pos], baked[x_advance_pos + 1]]);
    assert_eq!(baked_x, 25);
    // device offset slot zeroed.
    let baked_off = u16::from_be_bytes([baked[device_off_pos], baked[device_off_pos + 1]]);
    assert_eq!(baked_off, 0);
}

/// SinglePos fmt 2 round-trip: per-glyph ValueRecord array, each
/// with an x_advance variation. Verify every entry's static field
/// gets the delta and every device offset slot is zeroed.
#[test]
fn single_pos_fmt2_x_advance_variation_folds_for_every_entry() {
    let mut gpos = Vec::new();
    gpos.extend_from_slice(&1u16.to_be_bytes()); // major
    gpos.extend_from_slice(&0u16.to_be_bytes()); // minor
    gpos.extend_from_slice(&100u16.to_be_bytes());
    gpos.extend_from_slice(&100u16.to_be_bytes());
    gpos.extend_from_slice(&10u16.to_be_bytes()); // lookupListOff
    gpos.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
    gpos.extend_from_slice(&4u16.to_be_bytes()); // lookupOffset[0]
    gpos.extend_from_slice(&1u16.to_be_bytes()); // lookupType = SinglePos
    gpos.extend_from_slice(&0u16.to_be_bytes()); // flag
    gpos.extend_from_slice(&1u16.to_be_bytes()); // subtableCount
    gpos.extend_from_slice(&8u16.to_be_bytes()); // subtableOffset[0]

    let sub_off = gpos.len();
    let value_format = VR_X_ADVANCE | VR_X_ADVANCE_DEVICE; // 0x44
    gpos.extend_from_slice(&2u16.to_be_bytes()); // posFormat = 2
    gpos.extend_from_slice(&0u16.to_be_bytes()); // coverageOff (fill below)
    gpos.extend_from_slice(&value_format.to_be_bytes());
    gpos.extend_from_slice(&2u16.to_be_bytes()); // valueCount = 2
                                                 // Two ValueRecords: each is 4 bytes (i16 + o16).
    let vr0_pos = gpos.len();
    gpos.extend_from_slice(&10i16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes()); // device off (fill below)
    let vr1_pos = gpos.len();
    gpos.extend_from_slice(&20i16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes()); // device off (fill below)

    let coverage_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&1u16.to_be_bytes()); // cov format 1
    gpos.extend_from_slice(&2u16.to_be_bytes()); // glyph count
    gpos.extend_from_slice(&30u16.to_be_bytes());
    gpos.extend_from_slice(&31u16.to_be_bytes());

    let vi_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0x8000u16.to_be_bytes());

    gpos[sub_off + 2..sub_off + 4].copy_from_slice(&coverage_rel.to_be_bytes());
    gpos[vr0_pos + 2..vr0_pos + 4].copy_from_slice(&vi_rel.to_be_bytes());
    gpos[vr1_pos + 2..vr1_pos + 4].copy_from_slice(&vi_rel.to_be_bytes());

    let ivs_bytes = build_ivs_one_region_one_item(40);
    let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
    let baked = bake_gpos_at_coords(&gpos, Some(&store), &[1.0]).unwrap();

    let v0 = i16::from_be_bytes([baked[vr0_pos], baked[vr0_pos + 1]]);
    let v1 = i16::from_be_bytes([baked[vr1_pos], baked[vr1_pos + 1]]);
    assert_eq!(v0, 50);
    assert_eq!(v1, 60);
    let off0 = u16::from_be_bytes([baked[vr0_pos + 2], baked[vr0_pos + 3]]);
    let off1 = u16::from_be_bytes([baked[vr1_pos + 2], baked[vr1_pos + 3]]);
    assert_eq!(off0, 0);
    assert_eq!(off1, 0);
}

/// No-IVS source: the bake must still walk and zero VariationIndex
/// offsets even though it cannot resolve a delta. This is the
/// "GDEF.IVS will be pruned next" path. Leaving the offsets
/// dangling would re-create the orphan that #173 already shipped.
#[test]
fn bake_without_ivs_zeros_offsets_without_changing_static_fields() {
    // Reuse the pair-pos fixture from above without an IVS.
    let mut gpos = Vec::new();
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&100u16.to_be_bytes());
    gpos.extend_from_slice(&100u16.to_be_bytes());
    gpos.extend_from_slice(&10u16.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&4u16.to_be_bytes());
    gpos.extend_from_slice(&2u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&8u16.to_be_bytes());
    let sub_off = gpos.len();
    let vf1 = VR_X_ADVANCE | VR_X_ADVANCE_DEVICE;
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&vf1.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    let pair_set_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&60u16.to_be_bytes());
    let x_advance_pos = gpos.len();
    gpos.extend_from_slice(&(-50i16).to_be_bytes());
    let device_off_pos = gpos.len();
    gpos.extend_from_slice(&0u16.to_be_bytes());
    let coverage_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&50u16.to_be_bytes());
    let vi_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0x8000u16.to_be_bytes());
    gpos[sub_off + 2..sub_off + 4].copy_from_slice(&coverage_rel.to_be_bytes());
    gpos[sub_off + 10..sub_off + 12].copy_from_slice(&pair_set_rel.to_be_bytes());
    // PairValueRecord device offsets are relative to the PairSet.
    gpos[device_off_pos..device_off_pos + 2]
        .copy_from_slice(&(vi_rel - pair_set_rel).to_be_bytes());

    let baked = bake_gpos_at_coords(&gpos, None, &[1.0]).unwrap();
    // Static field unchanged.
    let baked_x = i16::from_be_bytes([baked[x_advance_pos], baked[x_advance_pos + 1]]);
    assert_eq!(baked_x, -50);
    // Offset zeroed.
    let baked_off = u16::from_be_bytes([baked[device_off_pos], baked[device_off_pos + 1]]);
    assert_eq!(baked_off, 0);
}

#[test]
fn value_record_device_without_static_field_skips_write() {
    // ValueFormat 0x0010: xPlaDevice without xPlacement. The slot
    // has no static field to fold into, which used to overflow
    // (debug) or index out of bounds (release) once the delta was
    // non-zero.
    let mut buf = vec![0u8; 8];
    // ValueRecord at 0: one Offset16 pointing at byte 2.
    buf[0..2].copy_from_slice(&2u16.to_be_bytes());
    // VariationIndex at byte 2: outer=0, inner=0, deltaFormat=0x8000.
    buf[6..8].copy_from_slice(&0x8000u16.to_be_bytes());
    let ivs_bytes = build_ivs_one_region_one_item(80);
    let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
    visit_value_record(&mut buf, 0, 0, 0x0010, &mut |b, slot| {
        assert_eq!(slot.field, None);
        fold_one_field(b, slot, Some(&store), &[1.0]);
    });
    // The offset slot is zeroed and nothing else changes.
    assert_eq!(&buf[0..2], &[0, 0]);
    assert_eq!(&buf[6..8], &0x8000u16.to_be_bytes());
}

#[test]
fn bake_visits_a_shared_subtable_once() {
    // Two lookup-list entries point at the same SinglePos lookup, so
    // its subtable is reached twice. Folding is idempotent, so the
    // second visit is skipped and the result matches one visit.
    let mut sub = Vec::new();
    sub.extend_from_slice(&1u16.to_be_bytes()); // posFormat 1
    sub.extend_from_slice(&0u16.to_be_bytes()); // coverage (unused)
    sub.extend_from_slice(&0x0011u16.to_be_bytes()); // xPlacement + device
    sub.extend_from_slice(&10i16.to_be_bytes()); // xPlacement
    sub.extend_from_slice(&10u16.to_be_bytes()); // device offset
    sub.extend_from_slice(&0u16.to_be_bytes()); // outer
    sub.extend_from_slice(&0u16.to_be_bytes()); // inner
    sub.extend_from_slice(&0x8000u16.to_be_bytes()); // VariationIndex

    let mut gpos = Vec::new();
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&[0u8; 4]); // script / feature lists unused
    gpos.extend_from_slice(&10u16.to_be_bytes()); // lookupListOffset
                                                  // LookupList at 10: two entries, both at offset 6.
    gpos.extend_from_slice(&2u16.to_be_bytes());
    gpos.extend_from_slice(&6u16.to_be_bytes());
    gpos.extend_from_slice(&6u16.to_be_bytes());
    // Lookup at 16: type 1, flag 0, one subtable at offset 8.
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&8u16.to_be_bytes());
    gpos.extend_from_slice(&sub);

    let ivs_bytes = build_ivs_one_region_one_item(80);
    let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
    let baked = bake_gpos_at_coords(&gpos, Some(&store), &[1.0]).unwrap();
    let x = i16::from_be_bytes([baked[24 + 6], baked[24 + 7]]);
    assert_eq!(x, 90);
    assert_eq!(&baked[24 + 8..24 + 10], &[0, 0]);
}
