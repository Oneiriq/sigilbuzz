//! Anchor fold tests for CursivePos and the mark attachment lookups.

use super::*;

// -----------------------------------------------------------------
// Mark*/Cursive anchor fold tests
// -----------------------------------------------------------------

/// AnchorFormat 1 has no device slots. The fold must be a pure
/// no-op on every byte.
#[test]
fn fold_anchor_format1_is_noop() {
    // Pad 4 bytes up front so anchor_off != 0 (the helper treats
    // an offset of 0 as the spec's "absent" sentinel).
    let mut buf = vec![0u8; 4];
    buf.extend_from_slice(&1u16.to_be_bytes());
    buf.extend_from_slice(&300i16.to_be_bytes());
    buf.extend_from_slice(&(-200i16).to_be_bytes());
    let original = buf.clone();
    let ivs_bytes = build_ivs_one_region_one_item(40);
    let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
    fold_anchor_variations(&mut buf, 4, Some(&store), &[1.0]);
    assert_eq!(buf, original);
}

/// AnchorFormat 2 (contour-point hint) carries no device slots:
/// fold must leave every byte untouched.
#[test]
fn fold_anchor_format2_is_noop() {
    let mut buf = vec![0u8; 4];
    buf.extend_from_slice(&2u16.to_be_bytes());
    buf.extend_from_slice(&50i16.to_be_bytes());
    buf.extend_from_slice(&75i16.to_be_bytes());
    buf.extend_from_slice(&42u16.to_be_bytes()); // anchorPoint
    let original = buf.clone();
    let ivs_bytes = build_ivs_one_region_one_item(40);
    let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
    fold_anchor_variations(&mut buf, 4, Some(&store), &[1.0]);
    assert_eq!(buf, original);
}

/// AnchorFormat 3 with both x and y device offsets pointing at a
/// VariationIndex: both static fields must absorb the delta and
/// both device slots must zero.
#[test]
fn fold_anchor_format3_folds_x_and_y() {
    // Build: 4 bytes of pad, anchor at byte 4 (10 B), then the
    // shared VariationIndex. The pad keeps anchor_off != 0 so the
    // helper doesn't treat the anchor as "absent".
    let mut buf = vec![0u8; 4];
    buf.extend_from_slice(&3u16.to_be_bytes()); // format
    buf.extend_from_slice(&100i16.to_be_bytes()); // xCoord
    buf.extend_from_slice(&(-50i16).to_be_bytes()); // yCoord
    let x_dev_pos = buf.len();
    buf.extend_from_slice(&0u16.to_be_bytes()); // xDevice (filled)
    let y_dev_pos = buf.len();
    buf.extend_from_slice(&0u16.to_be_bytes()); // yDevice (filled)
    let vi_pos = buf.len();
    buf.extend_from_slice(&0u16.to_be_bytes()); // outer
    buf.extend_from_slice(&0u16.to_be_bytes()); // inner
    buf.extend_from_slice(&0x8000u16.to_be_bytes()); // deltaFormat
                                                     // Device offsets are relative to the anchor at byte 4.
    buf[x_dev_pos..x_dev_pos + 2].copy_from_slice(&((vi_pos - 4) as u16).to_be_bytes());
    buf[y_dev_pos..y_dev_pos + 2].copy_from_slice(&((vi_pos - 4) as u16).to_be_bytes());

    let ivs_bytes = build_ivs_one_region_one_item(25);
    let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
    fold_anchor_variations(&mut buf, 4, Some(&store), &[1.0]);
    let x = i16::from_be_bytes([buf[6], buf[7]]);
    let y = i16::from_be_bytes([buf[8], buf[9]]);
    assert_eq!(x, 125);
    assert_eq!(y, -25);
    assert_eq!(u16::from_be_bytes([buf[x_dev_pos], buf[x_dev_pos + 1]]), 0);
    assert_eq!(u16::from_be_bytes([buf[y_dev_pos], buf[y_dev_pos + 1]]), 0);
}

/// Zero anchor offset (the spec's "absent" sentinel): the fold
/// must early-return rather than walk into byte 0 of the subtable.
#[test]
fn fold_anchor_zero_offset_is_noop() {
    let mut buf = vec![0xAAu8; 16];
    let original = buf.clone();
    fold_anchor_variations(&mut buf, 0, None, &[1.0]);
    assert_eq!(buf, original);
}

/// CursivePos round-trip: one EntryExitRecord with both anchors
/// in format 3, each pointing at a VariationIndex into a non-zero
/// IVS delta. Bake at coord 1.0 and assert the static fields
/// absorb the delta and every device offset is zeroed.
#[test]
fn cursive_pos_anchor_variation_folds() {
    let mut gpos = Vec::new();
    // Header.
    gpos.extend_from_slice(&1u16.to_be_bytes()); // major
    gpos.extend_from_slice(&0u16.to_be_bytes()); // minor
    gpos.extend_from_slice(&100u16.to_be_bytes()); // scriptListOff (unused)
    gpos.extend_from_slice(&100u16.to_be_bytes()); // featureListOff (unused)
    gpos.extend_from_slice(&10u16.to_be_bytes()); // lookupListOff
                                                  // LookupList at 10:
    gpos.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
    gpos.extend_from_slice(&4u16.to_be_bytes()); // lookupOffset[0] (rel to LookupList)
                                                 // Lookup at 14:
    gpos.extend_from_slice(&3u16.to_be_bytes()); // lookupType = CursivePos
    gpos.extend_from_slice(&0u16.to_be_bytes()); // flag
    gpos.extend_from_slice(&1u16.to_be_bytes()); // subtableCount
    gpos.extend_from_slice(&8u16.to_be_bytes()); // subtableOffset[0] (rel to Lookup)
                                                 // CursivePos subtable at byte 22.
    let sub_off = gpos.len();
    gpos.extend_from_slice(&1u16.to_be_bytes()); // posFormat=1
    gpos.extend_from_slice(&0u16.to_be_bytes()); // coverageOff (filled below)
    gpos.extend_from_slice(&1u16.to_be_bytes()); // entryExitCount=1
                                                 // EntryExitRecord[0]: entryAnchorOffset, exitAnchorOffset.
    let ee_pos = gpos.len();
    gpos.extend_from_slice(&0u16.to_be_bytes()); // entryAnchorOffset (filled)
    gpos.extend_from_slice(&0u16.to_be_bytes()); // exitAnchorOffset (filled)
                                                 // Entry anchor (format 3) at end-of-records.
    let entry_anchor_off = gpos.len() - sub_off;
    let entry_x_pos = gpos.len() + 2;
    let entry_y_pos = gpos.len() + 4;
    let entry_x_dev_pos = gpos.len() + 6;
    let entry_y_dev_pos = gpos.len() + 8;
    gpos.extend_from_slice(&3u16.to_be_bytes()); // format=3
    gpos.extend_from_slice(&500i16.to_be_bytes()); // xCoord
    gpos.extend_from_slice(&100i16.to_be_bytes()); // yCoord
    gpos.extend_from_slice(&0u16.to_be_bytes()); // xDevice (filled below)
    gpos.extend_from_slice(&0u16.to_be_bytes()); // yDevice (filled below)
                                                 // Exit anchor (format 3).
    let exit_anchor_off = gpos.len() - sub_off;
    let exit_x_pos = gpos.len() + 2;
    let exit_y_pos = gpos.len() + 4;
    let exit_x_dev_pos = gpos.len() + 6;
    let exit_y_dev_pos = gpos.len() + 8;
    gpos.extend_from_slice(&3u16.to_be_bytes());
    gpos.extend_from_slice(&(-200i16).to_be_bytes());
    gpos.extend_from_slice(&50i16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());

    // Coverage at end.
    let coverage_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&77u16.to_be_bytes());

    // Shared VariationIndex at end of subtable.
    let vi_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0x8000u16.to_be_bytes());

    // Patch slots.
    gpos[sub_off + 2..sub_off + 4].copy_from_slice(&coverage_rel.to_be_bytes());
    gpos[ee_pos..ee_pos + 2].copy_from_slice(&(entry_anchor_off as u16).to_be_bytes());
    gpos[ee_pos + 2..ee_pos + 4].copy_from_slice(&(exit_anchor_off as u16).to_be_bytes());
    // Point both anchors' x/y devices at the shared VariationIndex.
    set_x_device(&mut gpos, entry_x_dev_pos, sub_off + vi_rel as usize);
    set_y_device(&mut gpos, entry_y_dev_pos, sub_off + vi_rel as usize);
    set_x_device(&mut gpos, exit_x_dev_pos, sub_off + vi_rel as usize);
    set_y_device(&mut gpos, exit_y_dev_pos, sub_off + vi_rel as usize);

    let ivs_bytes = build_ivs_one_region_one_item(60);
    let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
    let baked = bake_gpos_at_coords(&gpos, Some(&store), &[1.0]).unwrap();

    // Entry: x=500+60=560, y=100+60=160.
    assert_eq!(
        i16::from_be_bytes([baked[entry_x_pos], baked[entry_x_pos + 1]]),
        560
    );
    assert_eq!(
        i16::from_be_bytes([baked[entry_y_pos], baked[entry_y_pos + 1]]),
        160
    );
    // Exit: x=-200+60=-140, y=50+60=110.
    assert_eq!(
        i16::from_be_bytes([baked[exit_x_pos], baked[exit_x_pos + 1]]),
        -140
    );
    assert_eq!(
        i16::from_be_bytes([baked[exit_y_pos], baked[exit_y_pos + 1]]),
        110
    );
    // All four device offset slots zeroed.
    for off in [
        entry_x_dev_pos,
        entry_y_dev_pos,
        exit_x_dev_pos,
        exit_y_dev_pos,
    ] {
        assert_eq!(u16::from_be_bytes([baked[off], baked[off + 1]]), 0);
    }
}

/// MarkBasePos round-trip: one mark, one base, single mark class.
/// Both anchors are AnchorFormat 3 with x/yDevice -> VariationIndex.
/// Assert both anchors' static fields absorb the delta and every
/// device slot zeros.
#[test]
fn mark_base_pos_anchor_variation_folds() {
    // GPOS header / lookup list / lookup identical to the cursive
    // test, but with lookupType = 4.
    let mut gpos = Vec::new();
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&100u16.to_be_bytes());
    gpos.extend_from_slice(&100u16.to_be_bytes());
    gpos.extend_from_slice(&10u16.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&4u16.to_be_bytes());
    gpos.extend_from_slice(&4u16.to_be_bytes()); // lookupType=4
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&8u16.to_be_bytes());
    let sub_off = gpos.len();
    // MarkBasePos: u16 format=1, u16 markCovOff, u16 baseCovOff,
    // u16 markClassCount, o16 markArrayOff, o16 baseArrayOff.
    gpos.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    gpos.extend_from_slice(&0u16.to_be_bytes()); // markCovOff (fill)
    gpos.extend_from_slice(&0u16.to_be_bytes()); // baseCovOff (fill)
    gpos.extend_from_slice(&1u16.to_be_bytes()); // markClassCount=1
    gpos.extend_from_slice(&0u16.to_be_bytes()); // markArrayOff (fill)
    gpos.extend_from_slice(&0u16.to_be_bytes()); // baseArrayOff (fill)

    // MarkArray.
    let mark_array_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&1u16.to_be_bytes()); // markCount
    gpos.extend_from_slice(&0u16.to_be_bytes()); // markRecord.class=0
                                                 // markAnchorOffset (rel to MarkArray), fill below.
    let mark_anchor_off_pos = gpos.len();
    gpos.extend_from_slice(&0u16.to_be_bytes());

    // Mark anchor (format 3).
    let mark_anchor_rel_to_marray = (gpos.len() - sub_off - mark_array_rel as usize) as u16;
    let mark_x_pos = gpos.len() + 2;
    let mark_y_pos = gpos.len() + 4;
    let mark_x_dev_pos = gpos.len() + 6;
    let mark_y_dev_pos = gpos.len() + 8;
    gpos.extend_from_slice(&3u16.to_be_bytes());
    gpos.extend_from_slice(&10i16.to_be_bytes());
    gpos.extend_from_slice(&20i16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());

    // BaseArray.
    let base_array_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&1u16.to_be_bytes()); // baseCount
                                                 // baseAnchorOffsets[markClassCount=1], rel to BaseArray.
    let base_anchor_off_pos = gpos.len();
    gpos.extend_from_slice(&0u16.to_be_bytes());

    // Base anchor (format 3).
    let base_anchor_rel_to_barray = (gpos.len() - sub_off - base_array_rel as usize) as u16;
    let base_x_pos = gpos.len() + 2;
    let base_y_pos = gpos.len() + 4;
    let base_x_dev_pos = gpos.len() + 6;
    let base_y_dev_pos = gpos.len() + 8;
    gpos.extend_from_slice(&3u16.to_be_bytes());
    gpos.extend_from_slice(&100i16.to_be_bytes());
    gpos.extend_from_slice(&200i16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());

    // Coverage records at the end (same shape, same content; we
    // don't actually consult them in the bake walk).
    let cov_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&50u16.to_be_bytes());

    // Shared VariationIndex.
    let vi_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0x8000u16.to_be_bytes());

    // Patch slots.
    gpos[sub_off + 2..sub_off + 4].copy_from_slice(&cov_rel.to_be_bytes());
    gpos[sub_off + 4..sub_off + 6].copy_from_slice(&cov_rel.to_be_bytes());
    gpos[sub_off + 8..sub_off + 10].copy_from_slice(&mark_array_rel.to_be_bytes());
    gpos[sub_off + 10..sub_off + 12].copy_from_slice(&base_array_rel.to_be_bytes());
    gpos[mark_anchor_off_pos..mark_anchor_off_pos + 2]
        .copy_from_slice(&mark_anchor_rel_to_marray.to_be_bytes());
    gpos[base_anchor_off_pos..base_anchor_off_pos + 2]
        .copy_from_slice(&base_anchor_rel_to_barray.to_be_bytes());
    set_x_device(&mut gpos, mark_x_dev_pos, sub_off + vi_rel as usize);
    set_y_device(&mut gpos, mark_y_dev_pos, sub_off + vi_rel as usize);
    set_x_device(&mut gpos, base_x_dev_pos, sub_off + vi_rel as usize);
    set_y_device(&mut gpos, base_y_dev_pos, sub_off + vi_rel as usize);

    let ivs_bytes = build_ivs_one_region_one_item(15);
    let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
    let baked = bake_gpos_at_coords(&gpos, Some(&store), &[1.0]).unwrap();

    assert_eq!(
        i16::from_be_bytes([baked[mark_x_pos], baked[mark_x_pos + 1]]),
        25
    );
    assert_eq!(
        i16::from_be_bytes([baked[mark_y_pos], baked[mark_y_pos + 1]]),
        35
    );
    assert_eq!(
        i16::from_be_bytes([baked[base_x_pos], baked[base_x_pos + 1]]),
        115
    );
    assert_eq!(
        i16::from_be_bytes([baked[base_y_pos], baked[base_y_pos + 1]]),
        215
    );
    for off in [
        mark_x_dev_pos,
        mark_y_dev_pos,
        base_x_dev_pos,
        base_y_dev_pos,
    ] {
        assert_eq!(u16::from_be_bytes([baked[off], baked[off + 1]]), 0);
    }
}

/// MarkLigPos round-trip: one mark, one ligature with two
/// components, single mark class. The component matrix is
/// `componentCount * markClassCount`, 2 anchors per ligature.
/// Assert both component anchors absorb the delta and zero their
/// device slots.
#[test]
fn mark_lig_pos_anchor_variation_folds() {
    let mut gpos = Vec::new();
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&100u16.to_be_bytes());
    gpos.extend_from_slice(&100u16.to_be_bytes());
    gpos.extend_from_slice(&10u16.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&4u16.to_be_bytes());
    gpos.extend_from_slice(&5u16.to_be_bytes()); // lookupType=5
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&8u16.to_be_bytes());
    let sub_off = gpos.len();
    gpos.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    gpos.extend_from_slice(&0u16.to_be_bytes()); // markCovOff (fill)
    gpos.extend_from_slice(&0u16.to_be_bytes()); // ligCovOff (fill)
    gpos.extend_from_slice(&1u16.to_be_bytes()); // markClassCount=1
    gpos.extend_from_slice(&0u16.to_be_bytes()); // markArrayOff (fill)
    gpos.extend_from_slice(&0u16.to_be_bytes()); // ligArrayOff (fill)

    // MarkArray.
    let mark_array_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&1u16.to_be_bytes()); // markCount=1
    gpos.extend_from_slice(&0u16.to_be_bytes()); // markRecord.class=0
    let mark_anchor_off_pos = gpos.len();
    gpos.extend_from_slice(&0u16.to_be_bytes()); // markAnchorOff (fill)

    // Mark anchor (format 3, single shared VariationIndex).
    let mark_anchor_rel_to_marray = (gpos.len() - sub_off - mark_array_rel as usize) as u16;
    let mark_x_pos = gpos.len() + 2;
    let mark_x_dev_pos = gpos.len() + 6;
    let mark_y_dev_pos = gpos.len() + 8;
    gpos.extend_from_slice(&3u16.to_be_bytes());
    gpos.extend_from_slice(&5i16.to_be_bytes());
    gpos.extend_from_slice(&5i16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());

    // LigatureArray.
    let lig_array_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&1u16.to_be_bytes()); // ligatureCount=1
    let lig_attach_off_pos = gpos.len();
    gpos.extend_from_slice(&0u16.to_be_bytes()); // ligAttachOff (fill)

    // LigatureAttach.
    let lig_attach_rel_to_larray = (gpos.len() - sub_off - lig_array_rel as usize) as u16;
    let lig_attach_abs_in_sub = gpos.len() - sub_off;
    gpos.extend_from_slice(&2u16.to_be_bytes()); // componentCount=2
                                                 // 2 components * 1 markClass = 2 anchor offsets.
    let comp0_anchor_off_pos = gpos.len();
    gpos.extend_from_slice(&0u16.to_be_bytes());
    let comp1_anchor_off_pos = gpos.len();
    gpos.extend_from_slice(&0u16.to_be_bytes());

    // Component 0 anchor (format 3).
    let comp0_anchor_rel_to_la = (gpos.len() - sub_off - lig_attach_abs_in_sub) as u16;
    let comp0_x_pos = gpos.len() + 2;
    let comp0_x_dev_pos = gpos.len() + 6;
    let comp0_y_dev_pos = gpos.len() + 8;
    gpos.extend_from_slice(&3u16.to_be_bytes());
    gpos.extend_from_slice(&100i16.to_be_bytes());
    gpos.extend_from_slice(&50i16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());

    // Component 1 anchor (format 3).
    let comp1_anchor_rel_to_la = (gpos.len() - sub_off - lig_attach_abs_in_sub) as u16;
    let comp1_x_pos = gpos.len() + 2;
    let comp1_x_dev_pos = gpos.len() + 6;
    let comp1_y_dev_pos = gpos.len() + 8;
    gpos.extend_from_slice(&3u16.to_be_bytes());
    gpos.extend_from_slice(&200i16.to_be_bytes());
    gpos.extend_from_slice(&75i16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());

    // Coverage filler.
    let cov_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&60u16.to_be_bytes());

    // Shared VariationIndex.
    let vi_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0x8000u16.to_be_bytes());

    // Patch slots.
    gpos[sub_off + 2..sub_off + 4].copy_from_slice(&cov_rel.to_be_bytes());
    gpos[sub_off + 4..sub_off + 6].copy_from_slice(&cov_rel.to_be_bytes());
    gpos[sub_off + 8..sub_off + 10].copy_from_slice(&mark_array_rel.to_be_bytes());
    gpos[sub_off + 10..sub_off + 12].copy_from_slice(&lig_array_rel.to_be_bytes());
    gpos[mark_anchor_off_pos..mark_anchor_off_pos + 2]
        .copy_from_slice(&mark_anchor_rel_to_marray.to_be_bytes());
    gpos[lig_attach_off_pos..lig_attach_off_pos + 2]
        .copy_from_slice(&lig_attach_rel_to_larray.to_be_bytes());
    gpos[comp0_anchor_off_pos..comp0_anchor_off_pos + 2]
        .copy_from_slice(&comp0_anchor_rel_to_la.to_be_bytes());
    gpos[comp1_anchor_off_pos..comp1_anchor_off_pos + 2]
        .copy_from_slice(&comp1_anchor_rel_to_la.to_be_bytes());
    set_x_device(&mut gpos, mark_x_dev_pos, sub_off + vi_rel as usize);
    set_y_device(&mut gpos, mark_y_dev_pos, sub_off + vi_rel as usize);
    set_x_device(&mut gpos, comp0_x_dev_pos, sub_off + vi_rel as usize);
    set_y_device(&mut gpos, comp0_y_dev_pos, sub_off + vi_rel as usize);
    set_x_device(&mut gpos, comp1_x_dev_pos, sub_off + vi_rel as usize);
    set_y_device(&mut gpos, comp1_y_dev_pos, sub_off + vi_rel as usize);

    let ivs_bytes = build_ivs_one_region_one_item(20);
    let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
    let baked = bake_gpos_at_coords(&gpos, Some(&store), &[1.0]).unwrap();

    // Mark anchor: 5+20=25.
    assert_eq!(
        i16::from_be_bytes([baked[mark_x_pos], baked[mark_x_pos + 1]]),
        25
    );
    // Component 0 anchor: 100+20=120.
    assert_eq!(
        i16::from_be_bytes([baked[comp0_x_pos], baked[comp0_x_pos + 1]]),
        120
    );
    // Component 1 anchor: 200+20=220.
    assert_eq!(
        i16::from_be_bytes([baked[comp1_x_pos], baked[comp1_x_pos + 1]]),
        220
    );
    for off in [
        mark_x_dev_pos,
        mark_y_dev_pos,
        comp0_x_dev_pos,
        comp0_y_dev_pos,
        comp1_x_dev_pos,
        comp1_y_dev_pos,
    ] {
        assert_eq!(u16::from_be_bytes([baked[off], baked[off + 1]]), 0);
    }
}

/// MarkMarkPos shares MarkBasePos's shape (two MarkArrays). Single
/// targeted check that MarkMark dispatches into the same anchor
/// walk by routing one anchor through the type-6 branch.
#[test]
fn mark_mark_pos_anchor_variation_folds() {
    let mut gpos = Vec::new();
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&100u16.to_be_bytes());
    gpos.extend_from_slice(&100u16.to_be_bytes());
    gpos.extend_from_slice(&10u16.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&4u16.to_be_bytes());
    gpos.extend_from_slice(&6u16.to_be_bytes()); // lookupType=6
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&8u16.to_be_bytes());
    let sub_off = gpos.len();
    gpos.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    gpos.extend_from_slice(&0u16.to_be_bytes()); // mark1CovOff (fill)
    gpos.extend_from_slice(&0u16.to_be_bytes()); // mark2CovOff (fill)
    gpos.extend_from_slice(&1u16.to_be_bytes()); // markClassCount=1
    gpos.extend_from_slice(&0u16.to_be_bytes()); // mark1ArrayOff (fill)
    gpos.extend_from_slice(&0u16.to_be_bytes()); // mark2ArrayOff (fill)

    // Mark1Array.
    let m1_array_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    let m1_anchor_off_pos = gpos.len();
    gpos.extend_from_slice(&0u16.to_be_bytes());
    let m1_anchor_rel_to_array = (gpos.len() - sub_off - m1_array_rel as usize) as u16;
    let m1_x_pos = gpos.len() + 2;
    let m1_x_dev_pos = gpos.len() + 6;
    let m1_y_dev_pos = gpos.len() + 8;
    gpos.extend_from_slice(&3u16.to_be_bytes());
    gpos.extend_from_slice(&7i16.to_be_bytes());
    gpos.extend_from_slice(&8i16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());

    // Mark2Array (same shape as BaseArray).
    let m2_array_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&1u16.to_be_bytes()); // mark2Count
    let m2_anchor_off_pos = gpos.len();
    gpos.extend_from_slice(&0u16.to_be_bytes());
    let m2_anchor_rel_to_array = (gpos.len() - sub_off - m2_array_rel as usize) as u16;
    let m2_x_pos = gpos.len() + 2;
    let m2_x_dev_pos = gpos.len() + 6;
    let m2_y_dev_pos = gpos.len() + 8;
    gpos.extend_from_slice(&3u16.to_be_bytes());
    gpos.extend_from_slice(&77i16.to_be_bytes());
    gpos.extend_from_slice(&88i16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());

    let cov_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&1u16.to_be_bytes());
    gpos.extend_from_slice(&90u16.to_be_bytes());

    let vi_rel = (gpos.len() - sub_off) as u16;
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes());
    gpos.extend_from_slice(&0x8000u16.to_be_bytes());

    gpos[sub_off + 2..sub_off + 4].copy_from_slice(&cov_rel.to_be_bytes());
    gpos[sub_off + 4..sub_off + 6].copy_from_slice(&cov_rel.to_be_bytes());
    gpos[sub_off + 8..sub_off + 10].copy_from_slice(&m1_array_rel.to_be_bytes());
    gpos[sub_off + 10..sub_off + 12].copy_from_slice(&m2_array_rel.to_be_bytes());
    gpos[m1_anchor_off_pos..m1_anchor_off_pos + 2]
        .copy_from_slice(&m1_anchor_rel_to_array.to_be_bytes());
    gpos[m2_anchor_off_pos..m2_anchor_off_pos + 2]
        .copy_from_slice(&m2_anchor_rel_to_array.to_be_bytes());
    set_x_device(&mut gpos, m1_x_dev_pos, sub_off + vi_rel as usize);
    set_y_device(&mut gpos, m1_y_dev_pos, sub_off + vi_rel as usize);
    set_x_device(&mut gpos, m2_x_dev_pos, sub_off + vi_rel as usize);
    set_y_device(&mut gpos, m2_y_dev_pos, sub_off + vi_rel as usize);

    let ivs_bytes = build_ivs_one_region_one_item(11);
    let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
    let baked = bake_gpos_at_coords(&gpos, Some(&store), &[1.0]).unwrap();

    assert_eq!(
        i16::from_be_bytes([baked[m1_x_pos], baked[m1_x_pos + 1]]),
        18
    );
    assert_eq!(
        i16::from_be_bytes([baked[m2_x_pos], baked[m2_x_pos + 1]]),
        88
    );
    for off in [m1_x_dev_pos, m1_y_dev_pos, m2_x_dev_pos, m2_y_dev_pos] {
        assert_eq!(u16::from_be_bytes([baked[off], baked[off + 1]]), 0);
    }
}
