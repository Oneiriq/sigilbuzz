//! Unit tests for the GPOS stage: feature collection, value records,
//! and the pair-adjustment cursor rules.

use super::*;
use crate::tables::gpos::value_record::{X_ADVANCE, X_ADVANCE_DEVICE, Y_ADVANCE};
use crate::tables::variation_store::ItemVariationStore;
use alloc::vec;

fn feature(tag: &[u8; 4], value: u32) -> Feature {
    Feature { tag: *tag, value }
}

/// Lookup table for [`stage_lookups`]: `abvm` -> [4], `mark` -> [2, 5],
/// `mkmk` -> [5], `kern` -> [1], `dist` -> [1, 3], `ss01` -> [0].
fn lookups(tag: [u8; 4]) -> Vec<u16> {
    match &tag {
        b"abvm" => vec![4],
        b"mark" => vec![2, 5],
        b"mkmk" => vec![5],
        b"kern" => vec![1],
        b"dist" => vec![1, 3],
        b"ss01" => vec![0],
        _ => Vec::new(),
    }
}

fn indices(stage: &[StageLookup]) -> Vec<u16> {
    stage.iter().map(|l| l.index).collect()
}

#[test]
fn stage_merges_features_in_lookup_order_and_runs_shared_lookups_once() {
    let stage = stage_lookups(&[], true, lookups);
    // `dist` and `kern` share lookup 1: it runs once.
    assert_eq!(indices(&stage), [1, 2, 3, 4, 5]);
    // Lookups of `mark` / `mkmk` do not pass over ZWJ.
    let zwj = |i: u16| stage.iter().find(|l| l.index == i).unwrap().auto_zwj;
    assert!(zwj(1) && zwj(3) && zwj(4));
    assert!(!zwj(2) && !zwj(5));
}

#[test]
fn a_user_enabled_default_feature_is_not_applied_twice() {
    let with_dist = stage_lookups(&[feature(b"dist", 1)], true, lookups);
    assert_eq!(indices(&with_dist), [1, 2, 3, 4, 5]);
}

#[test]
fn user_features_join_and_disabled_defaults_leave_the_stage() {
    let stage = stage_lookups(
        &[
            feature(b"ss01", 1),
            feature(b"kern", 0),
            feature(b"dist", 0),
        ],
        true,
        lookups,
    );
    assert_eq!(indices(&stage), [0, 2, 4, 5]);
}

#[test]
fn vertical_runs_leave_out_the_horizontal_defaults() {
    let stage = stage_lookups(&[], false, lookups);
    assert_eq!(indices(&stage), [2, 4, 5]);
    // A caller can still ask for one.
    let stage = stage_lookups(&[feature(b"kern", 1)], false, lookups);
    assert_eq!(indices(&stage), [1, 2, 4, 5]);
}

fn glyph(gid: u32) -> Glyph {
    let mut g = Glyph::new(gid, 0);
    g.x_advance = 500;
    g.y_advance = -1000;
    g
}

#[test]
fn value_records_move_the_advance_of_the_run_direction_only() {
    let v = ValueRecord {
        x_placement: 5,
        y_placement: 7,
        x_advance: -30,
        y_advance: 40,
        ..ValueRecord::default()
    };
    let var = VarCtx::none();
    let mut g = glyph(1);
    apply_value(&mut g, &v, &[], &var, true);
    assert_eq!((g.x_offset, g.y_offset), (5, 7));
    assert_eq!((g.x_advance, g.y_advance), (470, -1000));

    // Vertical: y_advance grows downward, so a positive font-space
    // value shortens the (negative) advance.
    let mut g = glyph(1);
    apply_value(&mut g, &v, &[], &var, false);
    assert_eq!((g.x_offset, g.y_offset), (5, 7));
    assert_eq!((g.x_advance, g.y_advance), (500, -1040));
}

/// Stacked adjustments pin a position at the `i32` bounds instead of
/// overflowing (which panics in debug builds).
#[test]
fn value_records_saturate_at_the_i32_bounds() {
    let v = ValueRecord {
        x_placement: i16::MAX,
        y_placement: i16::MIN,
        x_advance: i16::MAX,
        y_advance: i16::MAX,
        ..ValueRecord::default()
    };
    let var = VarCtx::none();
    let mut g = glyph(1);
    g.x_offset = i32::MAX - 1;
    g.y_offset = i32::MIN + 1;
    g.x_advance = i32::MAX - 1;
    apply_value(&mut g, &v, &[], &var, true);
    assert_eq!(
        (g.x_offset, g.y_offset, g.x_advance),
        (i32::MAX, i32::MIN, i32::MAX)
    );

    let mut g = glyph(1);
    g.y_advance = i32::MIN + 1;
    apply_value(&mut g, &v, &[], &var, false);
    assert_eq!(g.y_advance, i32::MIN);
}

fn be16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// PairPos format 1 covering first glyph 1 with the single pair
/// (1, 2). `value_format2` decides whether the second record exists;
/// its value is zero. When `device` is set, record 1 carries an
/// x-advance VariationIndex (outer 0, inner 0) placed right after the
/// PairSet's record, so its offset is measured from the PairSet.
fn pair_pos_format1(v1_x_advance: i16, value_format2: u16, device: bool) -> Vec<u8> {
    let vf1 = if device {
        X_ADVANCE | X_ADVANCE_DEVICE
    } else {
        X_ADVANCE
    };
    let rec2_len = if value_format2 == 0 { 0 } else { 2 };
    let rec1_len = if device { 4 } else { 2 };
    let set_off = 12u16;
    let set_len = 2 + 2 + rec1_len + rec2_len;
    let device_len = if device { 6 } else { 0 };
    let cov_off = set_off + set_len as u16 + device_len;
    let mut out = Vec::new();
    be16(&mut out, 1); // posFormat
    be16(&mut out, cov_off);
    be16(&mut out, vf1);
    be16(&mut out, value_format2);
    be16(&mut out, 1); // pairSetCount
    be16(&mut out, set_off);
    be16(&mut out, 1); // pairValueCount
    be16(&mut out, 2); // secondGlyph
    be16(&mut out, v1_x_advance as u16);
    if device {
        // Offset from the PairSet start to the VariationIndex that
        // follows the set.
        be16(&mut out, set_len as u16);
    }
    if value_format2 != 0 {
        be16(&mut out, 0);
    }
    if device {
        be16(&mut out, 0); // outer
        be16(&mut out, 0); // inner
        be16(&mut out, 0x8000); // VariationIndex format
    }
    be16(&mut out, 1); // Coverage format 1
    be16(&mut out, 1);
    be16(&mut out, 1);
    out
}

fn state(filter: MatchFilter<'_>) -> LookupState<'_> {
    LookupState {
        filter,
        flag: 0,
        mark_filtering_set: None,
        auto_zwj: true,
        index: 0,
    }
}

#[test]
fn pair_leaves_the_cursor_on_the_second_glyph_without_a_second_record() {
    let bytes = pair_pos_format1(-40, 0, false);
    let pp = PairPos::parse(&bytes).unwrap();
    let mut glyphs = vec![glyph(1), glyph(2)];
    let next = apply_pair(
        &pp,
        &state(MatchFilter::none()),
        &mut glyphs,
        0,
        &VarCtx::none(),
        true,
    );
    assert_eq!(next, Some(1));
    assert_eq!(glyphs[0].x_advance, 460);
}

#[test]
fn pair_moves_past_the_second_glyph_when_value_format2_is_set() {
    // HarfBuzz checks the format, not the values: an all-zero second
    // record still moves the cursor past the second glyph.
    let bytes = pair_pos_format1(-40, Y_ADVANCE, false);
    let pp = PairPos::parse(&bytes).unwrap();
    let mut glyphs = vec![glyph(1), glyph(2)];
    let next = apply_pair(
        &pp,
        &state(MatchFilter::none()),
        &mut glyphs,
        0,
        &VarCtx::none(),
        true,
    );
    assert_eq!(next, Some(2));
}

#[test]
fn pair_finds_the_second_glyph_across_default_ignorables() {
    let bytes = pair_pos_format1(-40, 0, false);
    let pp = PairPos::parse(&bytes).unwrap();
    let mut zwj = glyph(9);
    zwj.unicode_props = unicode_prop::DEFAULT_IGNORABLE | unicode_prop::JOINER;
    let mut glyphs = vec![glyph(1), zwj, glyph(2)];
    let next = apply_pair(
        &pp,
        &state(MatchFilter::none()),
        &mut glyphs,
        0,
        &VarCtx::none(),
        true,
    );
    assert_eq!(next, Some(2));
    assert_eq!(glyphs[0].x_advance, 460);

    // A lookup that must see ZWJ (manual joiners) does not kern
    // across it.
    let mut glyphs = vec![glyph(1), zwj, glyph(2)];
    let manual = LookupState {
        auto_zwj: false,
        ..state(MatchFilter::none())
    };
    let next = apply_pair(&pp, &manual, &mut glyphs, 0, &VarCtx::none(), true);
    assert_eq!(next, None);
    assert_eq!(glyphs[0].x_advance, 500);
}

#[test]
fn pair_set_device_offsets_are_measured_from_the_pair_set() {
    // One-axis store: region peaking at +1.0, row 0 delta -100.
    let mut ivs = Vec::new();
    be16(&mut ivs, 1);
    ivs.extend_from_slice(&12u32.to_be_bytes());
    be16(&mut ivs, 1);
    ivs.extend_from_slice(&22u32.to_be_bytes());
    for v in [1u16, 1, 0, 0x4000, 0x4000, 1, 1, 1, 0] {
        be16(&mut ivs, v);
    }
    be16(&mut ivs, (-100i16) as u16);
    let store = ItemVariationStore::parse(&ivs).unwrap();
    let coords = [1.0f32];
    let var = VarCtx {
        coords: &coords,
        store: Some(&store),
    };
    let bytes = pair_pos_format1(-40, 0, true);
    let pp = PairPos::parse(&bytes).unwrap();
    let mut glyphs = vec![glyph(1), glyph(2)];
    apply_pair(&pp, &state(MatchFilter::none()), &mut glyphs, 0, &var, true);
    assert_eq!(glyphs[0].x_advance, 500 - 40 - 100);
}
