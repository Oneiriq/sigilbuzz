//! Regression tests for the device-offset base the GPOS walk uses.
//!
//! Every fixture places the device table the spec names at
//! `parent + D` and a decoy `VariationIndex` at `subtable + D`, where
//! `parent` is the Anchor or PairSet the slot belongs to. The decoy
//! resolves to a much larger delta, so a walk that measures the
//! offset from the wrong table folds a visibly wrong value. Anchors
//! and PairSets always sit at a non-zero subtable offset here, which
//! is the case the earlier fixtures (anchors at offset 0 of their
//! buffer) could not tell apart.

use alloc::vec;
use alloc::vec::Vec;

use crate::util::StoreDeltas;

use super::{
    bake_gpos_at_coords, strip_variation_indices, walk_gpos_device_slots,
    VARIATION_INDEX_DELTA_FORMAT,
};

/// Delta of the decoy row (item 0) at coord 1.0.
const DECOY_DELTA: i16 = 100;
/// Delta of the real row (item 1) at coord 1.0.
const REAL_DELTA: i16 = 7;

fn put_u16(buf: &mut [u8], pos: usize, v: u16) {
    buf[pos..pos + 2].copy_from_slice(&v.to_be_bytes());
}

fn get_i16(buf: &[u8], pos: usize) -> i16 {
    i16::from_be_bytes([buf[pos], buf[pos + 1]])
}

fn get_u16(buf: &[u8], pos: usize) -> u16 {
    u16::from_be_bytes([buf[pos], buf[pos + 1]])
}

/// One region peaking at coord 1.0, one ItemVariationData with one
/// i16 delta per item.
fn build_ivs(deltas: &[i16]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&12u32.to_be_bytes()); // regionListOffset
    out.extend_from_slice(&1u16.to_be_bytes()); // dataCount
    out.extend_from_slice(&22u32.to_be_bytes()); // dataOffset[0]
                                                 // RegionList at 12: axisCount 1, regionCount 1, (0, 1, 1).
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    for v in [0i16, 16384, 16384] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    // ItemVariationData at 22.
    out.extend_from_slice(&(deltas.len() as u16).to_be_bytes()); // itemCount
    out.extend_from_slice(&1u16.to_be_bytes()); // wordDeltaCount
    out.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
    out.extend_from_slice(&0u16.to_be_bytes()); // regionIndexes[0]
    for d in deltas {
        out.extend_from_slice(&d.to_be_bytes());
    }
    out
}

fn variation_index(inner: u16) -> [u8; 6] {
    let mut out = [0u8; 6];
    out[2..4].copy_from_slice(&inner.to_be_bytes());
    out[4..6].copy_from_slice(&VARIATION_INDEX_DELTA_FORMAT.to_be_bytes());
    out
}

/// Wraps one subtable in a GPOS header, LookupList, and Lookup.
/// Returns the table and the subtable's absolute offset.
fn wrap_lookup(lookup_type: u16, sub: &[u8]) -> (Vec<u8>, usize) {
    let mut gpos = Vec::new();
    gpos.extend_from_slice(&1u16.to_be_bytes()); // major
    gpos.extend_from_slice(&0u16.to_be_bytes()); // minor
    gpos.extend_from_slice(&0u16.to_be_bytes()); // scriptList (unused)
    gpos.extend_from_slice(&0u16.to_be_bytes()); // featureList (unused)
    gpos.extend_from_slice(&10u16.to_be_bytes()); // lookupList
    gpos.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
    gpos.extend_from_slice(&4u16.to_be_bytes()); // lookupOffsets[0]
    gpos.extend_from_slice(&lookup_type.to_be_bytes());
    gpos.extend_from_slice(&0u16.to_be_bytes()); // lookupFlag
    gpos.extend_from_slice(&1u16.to_be_bytes()); // subtableCount
    gpos.extend_from_slice(&8u16.to_be_bytes()); // subtableOffsets[0]
    let sub_off = gpos.len();
    gpos.extend_from_slice(sub);
    (gpos, sub_off)
}

/// Appends the decoy at `D = sub.len()`, then a real VariationIndex at
/// `parent + D` for each parent, and returns `D`. The caller writes
/// `D` into the slots it wants resolved against those parents.
fn add_decoy_and_real_tables(sub: &mut Vec<u8>, parents: &[usize]) -> u16 {
    let d = sub.len();
    sub.extend_from_slice(&variation_index(0));
    let end = parents.iter().map(|p| p + d + 6).max().unwrap_or(0);
    if sub.len() < end {
        sub.resize(end, 0);
    }
    for &p in parents {
        sub[p + d..p + d + 6].copy_from_slice(&variation_index(1));
    }
    d as u16
}

/// A 10-byte AnchorFormat3 with both device slots set to `dev`.
fn anchor3(x: i16, y: i16, dev: u16) -> [u8; 10] {
    let mut out = [0u8; 10];
    out[0..2].copy_from_slice(&3u16.to_be_bytes());
    out[2..4].copy_from_slice(&x.to_be_bytes());
    out[4..6].copy_from_slice(&y.to_be_bytes());
    out[6..8].copy_from_slice(&dev.to_be_bytes());
    out[8..10].copy_from_slice(&dev.to_be_bytes());
    out
}

fn bake(gpos: &[u8]) -> Vec<u8> {
    let ivs = build_ivs(&[DECOY_DELTA, REAL_DELTA]);
    let store = StoreDeltas::new(&ivs, &[1.0]).unwrap();
    bake_gpos_at_coords(gpos, Some(&store)).unwrap()
}

/// MarkBasePos (or MarkMarkPos) with one mark and one base, both
/// AnchorFormat3, laid out at subtable offsets 22 and 32.
fn mark_base_subtable() -> (Vec<u8>, [usize; 2]) {
    let mut sub = Vec::new();
    sub.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    sub.extend_from_slice(&42u16.to_be_bytes()); // markCoverage
    sub.extend_from_slice(&42u16.to_be_bytes()); // baseCoverage
    sub.extend_from_slice(&1u16.to_be_bytes()); // markClassCount
    sub.extend_from_slice(&12u16.to_be_bytes()); // markArray
    sub.extend_from_slice(&18u16.to_be_bytes()); // baseArray
                                                 // MarkArray at 12: one record, class 0, anchor at 22 (rel 10).
    sub.extend_from_slice(&1u16.to_be_bytes());
    sub.extend_from_slice(&0u16.to_be_bytes());
    sub.extend_from_slice(&10u16.to_be_bytes());
    // BaseArray at 18: one record, anchor at 32 (rel 14).
    sub.extend_from_slice(&1u16.to_be_bytes());
    sub.extend_from_slice(&14u16.to_be_bytes());
    sub.extend_from_slice(&anchor3(10, 20, 0)); // 22
    sub.extend_from_slice(&anchor3(100, 200, 0)); // 32
                                                  // Coverage at 42: format 1, one glyph.
    sub.extend_from_slice(&[0, 1, 0, 1, 0, 5]);
    let anchors = [22, 32];
    let d = add_decoy_and_real_tables(&mut sub, &anchors);
    for a in anchors {
        put_u16(&mut sub, a + 6, d);
        put_u16(&mut sub, a + 8, d);
    }
    (sub, anchors)
}

fn assert_anchor(baked: &[u8], anchor_abs: usize, x: i16, y: i16) {
    assert_eq!(get_i16(baked, anchor_abs + 2), x, "x at {anchor_abs}");
    assert_eq!(get_i16(baked, anchor_abs + 4), y, "y at {anchor_abs}");
    assert_eq!(get_u16(baked, anchor_abs + 6), 0, "x slot at {anchor_abs}");
    assert_eq!(get_u16(baked, anchor_abs + 8), 0, "y slot at {anchor_abs}");
}

#[test]
fn mark_base_anchor_devices_resolve_against_the_anchor() {
    let (sub, [mark, base]) = mark_base_subtable();
    let (gpos, sub_off) = wrap_lookup(4, &sub);
    let baked = bake(&gpos);
    let r = REAL_DELTA;
    assert_anchor(&baked, sub_off + mark, 10 + r, 20 + r);
    assert_anchor(&baked, sub_off + base, 100 + r, 200 + r);
}

#[test]
fn mark_mark_anchor_devices_resolve_against_the_anchor() {
    let (sub, [mark1, mark2]) = mark_base_subtable();
    let (gpos, sub_off) = wrap_lookup(6, &sub);
    let baked = bake(&gpos);
    let r = REAL_DELTA;
    assert_anchor(&baked, sub_off + mark1, 10 + r, 20 + r);
    assert_anchor(&baked, sub_off + mark2, 100 + r, 200 + r);
}

#[test]
fn extension_wrapped_anchor_devices_resolve_against_the_anchor() {
    let (inner, [mark, base]) = mark_base_subtable();
    let mut ext = Vec::new();
    ext.extend_from_slice(&1u16.to_be_bytes()); // format
    ext.extend_from_slice(&4u16.to_be_bytes()); // extensionLookupType
    ext.extend_from_slice(&8u32.to_be_bytes()); // extensionOffset
    ext.extend_from_slice(&inner);
    let (gpos, sub_off) = wrap_lookup(9, &ext);
    let baked = bake(&gpos);
    let inner_off = sub_off + 8;
    assert_anchor(&baked, inner_off + mark, 10 + REAL_DELTA, 20 + REAL_DELTA);
    assert_anchor(&baked, inner_off + base, 100 + REAL_DELTA, 200 + REAL_DELTA);
}

#[test]
fn cursive_anchor_devices_resolve_against_the_anchor() {
    let mut sub = Vec::new();
    sub.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    sub.extend_from_slice(&30u16.to_be_bytes()); // coverage
    sub.extend_from_slice(&1u16.to_be_bytes()); // entryExitCount
    sub.extend_from_slice(&10u16.to_be_bytes()); // entryAnchor
    sub.extend_from_slice(&20u16.to_be_bytes()); // exitAnchor
    sub.extend_from_slice(&anchor3(500, 100, 0)); // 10
    sub.extend_from_slice(&anchor3(-200, 50, 0)); // 20
    sub.extend_from_slice(&[0, 1, 0, 1, 0, 7]); // coverage at 30
    let anchors = [10, 20];
    let d = add_decoy_and_real_tables(&mut sub, &anchors);
    for a in anchors {
        put_u16(&mut sub, a + 6, d);
        put_u16(&mut sub, a + 8, d);
    }
    let (gpos, sub_off) = wrap_lookup(3, &sub);
    let baked = bake(&gpos);
    assert_anchor(&baked, sub_off + 10, 507, 107);
    assert_anchor(&baked, sub_off + 20, -193, 57);
}

#[test]
fn mark_lig_anchor_devices_resolve_against_the_anchor() {
    let mut sub = Vec::new();
    sub.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    sub.extend_from_slice(&0u16.to_be_bytes()); // markCoverage (unused)
    sub.extend_from_slice(&0u16.to_be_bytes()); // ligatureCoverage (unused)
    sub.extend_from_slice(&1u16.to_be_bytes()); // markClassCount
    sub.extend_from_slice(&12u16.to_be_bytes()); // markArray
    sub.extend_from_slice(&18u16.to_be_bytes()); // ligatureArray
                                                 // MarkArray at 12: one record, anchor at 30 (rel 18).
    sub.extend_from_slice(&1u16.to_be_bytes());
    sub.extend_from_slice(&0u16.to_be_bytes());
    sub.extend_from_slice(&18u16.to_be_bytes());
    // LigatureArray at 18: one LigatureAttach at 22 (rel 4).
    sub.extend_from_slice(&1u16.to_be_bytes());
    sub.extend_from_slice(&4u16.to_be_bytes());
    // LigatureAttach at 22: two components, anchors at 40 and 50
    // (rel 18 and 28 from the LigatureAttach).
    sub.extend_from_slice(&2u16.to_be_bytes());
    sub.extend_from_slice(&18u16.to_be_bytes());
    sub.extend_from_slice(&28u16.to_be_bytes());
    sub.extend_from_slice(&[0u8; 2]); // pad to 30
    sub.extend_from_slice(&anchor3(5, 6, 0)); // 30
    sub.extend_from_slice(&anchor3(100, 50, 0)); // 40
    sub.extend_from_slice(&anchor3(200, 75, 0)); // 50
    let anchors = [30, 40, 50];
    let d = add_decoy_and_real_tables(&mut sub, &anchors);
    for a in anchors {
        put_u16(&mut sub, a + 6, d);
        put_u16(&mut sub, a + 8, d);
    }
    let (gpos, sub_off) = wrap_lookup(5, &sub);
    let baked = bake(&gpos);
    assert_anchor(&baked, sub_off + 30, 12, 13);
    assert_anchor(&baked, sub_off + 40, 107, 57);
    assert_anchor(&baked, sub_off + 50, 207, 82);
}

/// PairPos format 1: the PairValueRecord device offsets are relative
/// to the PairSet, which sits at subtable offset 12 here.
#[test]
fn pair_set_value_record_devices_resolve_against_the_pair_set() {
    let value_format1 = 0x0044u16; // X_ADVANCE | X_ADVANCE_DEVICE
    let mut sub = Vec::new();
    sub.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    sub.extend_from_slice(&20u16.to_be_bytes()); // coverage
    sub.extend_from_slice(&value_format1.to_be_bytes());
    sub.extend_from_slice(&0u16.to_be_bytes()); // valueFormat2
    sub.extend_from_slice(&1u16.to_be_bytes()); // pairSetCount
    sub.extend_from_slice(&12u16.to_be_bytes()); // pairSetOffsets[0]
                                                 // PairSet at 12: one PairValueRecord (second glyph 9, -50, device).
    sub.extend_from_slice(&1u16.to_be_bytes());
    sub.extend_from_slice(&9u16.to_be_bytes());
    sub.extend_from_slice(&(-50i16).to_be_bytes());
    sub.extend_from_slice(&0u16.to_be_bytes()); // device slot at 18
    sub.extend_from_slice(&[0, 1, 0, 1, 0, 8]); // coverage at 20
    let d = add_decoy_and_real_tables(&mut sub, &[12]);
    put_u16(&mut sub, 18, d);
    let (gpos, sub_off) = wrap_lookup(2, &sub);
    let baked = bake(&gpos);
    assert_eq!(get_i16(&baked, sub_off + 16), -50 + REAL_DELTA);
    assert_eq!(get_u16(&baked, sub_off + 18), 0);
}

/// SinglePos keeps the subtable as its base: a device offset there
/// still resolves against the subtable start.
#[test]
fn single_pos_value_record_devices_resolve_against_the_subtable() {
    let mut sub = Vec::new();
    sub.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    sub.extend_from_slice(&10u16.to_be_bytes()); // coverage
    sub.extend_from_slice(&0x0044u16.to_be_bytes()); // valueFormat
    sub.extend_from_slice(&30i16.to_be_bytes()); // xAdvance
    sub.extend_from_slice(&16u16.to_be_bytes()); // xAdvDevice -> 16
    sub.extend_from_slice(&[0, 1, 0, 1, 0, 8]); // coverage at 10
    sub.extend_from_slice(&variation_index(1)); // 16
    let (gpos, sub_off) = wrap_lookup(1, &sub);
    let baked = bake(&gpos);
    assert_eq!(get_i16(&baked, sub_off + 6), 30 + REAL_DELTA);
    assert_eq!(get_u16(&baked, sub_off + 8), 0);
}

/// A device offset that runs past the end of the table is severed
/// without touching the static coordinate.
#[test]
fn anchor_device_past_end_is_severed_without_folding() {
    let (mut sub, [mark, _]) = mark_base_subtable();
    put_u16(&mut sub, mark + 6, 0xFFF0);
    let (gpos, sub_off) = wrap_lookup(4, &sub);
    let baked = bake(&gpos);
    assert_eq!(get_i16(&baked, sub_off + mark + 2), 10);
    assert_eq!(get_u16(&baked, sub_off + mark + 6), 0);
    // The y slot still resolves normally.
    assert_eq!(get_i16(&baked, sub_off + mark + 4), 20 + REAL_DELTA);
}

/// An anchor whose 10-byte header runs off the end of the table is
/// left alone rather than read out of bounds.
#[test]
fn truncated_anchor_header_is_left_alone() {
    let (sub, [_, base]) = mark_base_subtable();
    let (mut gpos, sub_off) = wrap_lookup(4, &sub);
    gpos.truncate(sub_off + base + 8);
    let baked = bake(&gpos);
    assert_eq!(baked[sub_off + base..], gpos[sub_off + base..]);
}

#[test]
fn walk_reports_anchor_and_pair_set_bases() {
    let (sub, [mark, base]) = mark_base_subtable();
    let (mut gpos, _) = wrap_lookup(4, &sub);
    let mut bases = Vec::new();
    assert!(walk_gpos_device_slots(&mut gpos, &mut |_, slot| {
        bases.push(slot.base);
    }));
    // Positions are relative to the subtable slice the visitor sees.
    assert_eq!(bases, vec![mark, mark, base, base]);
}

/// Rubik VF carries thousands of AnchorFormat3 and PairPos format 1
/// device slots. Every one of them names a VariationIndex once it is
/// resolved against the spec base; measured from the subtable instead,
/// almost none would.
#[test]
fn rubik_device_slots_all_resolve_to_variation_indices() {
    const RUBIK: &[u8] = include_bytes!("../../../../tests/fixtures/rubik_vf.ttf");
    let face = sigilbuzz::Face::parse_bytes(RUBIK, 0).unwrap();
    let mut gpos = face
        .table_bytes(sigilbuzz::tables::tag::GPOS)
        .unwrap()
        .to_vec();
    let (mut total, mut resolved) = (0usize, 0usize);
    assert!(walk_gpos_device_slots(&mut gpos, &mut |b, slot| {
        if slot.target(b).is_some() {
            total += 1;
            if slot.delta_format(b) == Some(VARIATION_INDEX_DELTA_FORMAT) {
                resolved += 1;
            }
        }
    }));
    assert!(total > 10_000, "expected Rubik device slots, saw {total}");
    assert_eq!(resolved, total);
}

#[test]
fn strip_variation_indices_keeps_hinting_devices() {
    let mut sub = Vec::new();
    sub.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    sub.extend_from_slice(&12u16.to_be_bytes()); // coverage
    sub.extend_from_slice(&0x00C4u16.to_be_bytes()); // xAdv + xAdvDev + yAdvDev
    sub.extend_from_slice(&30i16.to_be_bytes()); // xAdvance
    sub.extend_from_slice(&18u16.to_be_bytes()); // xAdvDevice -> VariationIndex
    sub.extend_from_slice(&24u16.to_be_bytes()); // yAdvDevice -> hinting Device
    sub.extend_from_slice(&[0, 1, 0, 1, 0, 8]); // coverage at 12
    sub.extend_from_slice(&variation_index(1)); // 18
                                                // Device format 1 covering ppem 12..12 at 24.
    sub.extend_from_slice(&[0, 12, 0, 12, 0, 1, 0x40, 0]);
    let (mut gpos, sub_off) = wrap_lookup(1, &sub);
    strip_variation_indices(&mut gpos);
    assert_eq!(
        get_u16(&gpos, sub_off + 8),
        0,
        "VariationIndex slot cleared"
    );
    assert_eq!(get_u16(&gpos, sub_off + 10), 24, "Device slot kept");
    assert_eq!(get_i16(&gpos, sub_off + 6), 30, "static field untouched");
}
