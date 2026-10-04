use alloc::vec;

use super::*;
use crate::cff::walk_dict;

/// A CFF2-style store (subtables without rows) on one axis: subtable 0
/// names region 0 (0, 1, 1) and region 1 (-1, -1, 0).
fn store() -> Vec<u8> {
    let mut out = vec![0, 1]; // format
    out.extend_from_slice(&12u32.to_be_bytes()); // region list
    out.extend_from_slice(&1u16.to_be_bytes()); // one subtable
    out.extend_from_slice(&28u32.to_be_bytes());
    out.extend_from_slice(&[0, 1, 0, 2]); // axis count 1, two regions
    for v in [0i16, 16384, 16384, -16384, -16384, 0] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(&[0, 0, 0, 0, 0, 2, 0, 0, 0, 1]); // no rows, regions 0 and 1
    out
}

/// A DICT integer (the one-byte form).
fn n(v: i32) -> u8 {
    (v + 139) as u8
}

/// The integer operands of `entry`.
fn ints(entry: &DictEntry) -> Vec<Option<i32>> {
    entry.operands.iter().map(|o| o.int_value).collect()
}

#[test]
fn a_full_instance_resolves_private_blends() {
    // vsindex 0; StdHW 10 varying by 1 and 2; BlueValues -15, 15 (an
    // array of differences) varying by 2, 0 and -2, 0; BlueFuzz 1.
    let dict = [
        n(0),
        22,
        n(10),
        n(1),
        n(2),
        n(1),
        23,
        10,
        n(-15),
        n(15),
        n(2),
        n(0),
        n(-2),
        n(0),
        n(2),
        23,
        6,
        n(1),
        12,
        11,
    ];
    let ivs = store();
    let parsed = ItemVariationStore::parse(&ivs).unwrap();
    let coords = [0.5f32];
    let mut blend = BlendCache::new(Some(&parsed), &ivs, &coords);
    let out = bake_private(walk_dict(&dict).unwrap(), &mut blend).unwrap();
    let ops: Vec<u16> = out.iter().map(|e| e.op).collect();
    // vsindex and blend are gone.
    assert_eq!(ops, [10, 6, 0x0C0B]);
    // 10 + 0.5 rounds up to 11.
    assert_eq!(ints(&out[0]), [Some(11)]);
    // -15 + 1 and 15 - 1: absolute -14 and 0, stored as -14, 14.
    assert_eq!(ints(&out[1]), [Some(-14), Some(14)]);
    // No blend fed BlueFuzz: its bytes stay.
    assert_eq!(out[2].operands[0].raw, [n(1)]);
}

#[test]
fn a_partial_instance_keeps_the_surviving_regions_of_private_blends() {
    let dict = [n(10), n(1), n(2), n(1), 23, 10];
    let ivs = store();
    let parsed = ItemVariationStore::parse(&ivs).unwrap();
    // Region 0 survives, scaled by a half; region 1 does not.
    let survivors = [CffSubtableSurvivors {
        new_outer: Some(0),
        surviving: vec![(0, 0.5)],
        folded: vec![],
    }];
    let out = project_private(walk_dict(&dict).unwrap(), &parsed, &survivors).unwrap();
    assert_eq!(out.iter().map(|e| e.op).collect::<Vec<_>>(), [23, 10]);
    let blend = &out[0].operands;
    assert_eq!(blend[0].int_value, Some(10));
    assert_eq!(decode_real(&blend[1].raw), Some(0.5));
    assert_eq!(blend[2].int_value, Some(1));
    // With the subtable gone, the default goes to StdHW as it is.
    let gone = [CffSubtableSurvivors::default()];
    let out = project_private(walk_dict(&dict).unwrap(), &parsed, &gone).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(ints(&out[0]), [Some(10)]);
}

#[test]
fn a_partial_instance_moves_pinned_only_private_deltas_into_the_defaults() {
    // StdHW 10, deltas 1 and 2. Region 1 lies on the pinned axes only:
    // its delta, scaled, moves into the default, where a renderer at
    // the new default reads it, and leaves the blend.
    let dict = [n(10), n(1), n(2), n(1), 23, 10];
    let ivs = store();
    let parsed = ItemVariationStore::parse(&ivs).unwrap();
    let survivors = [CffSubtableSurvivors {
        new_outer: Some(0),
        surviving: vec![(0, 0.5)],
        folded: vec![(1, 0.5)],
    }];
    let out = project_private(walk_dict(&dict).unwrap(), &parsed, &survivors).unwrap();
    let blend = &out[0].operands;
    assert_eq!(blend[0].int_value, Some(11), "10 + 2 x 0.5");
    assert_eq!(decode_real(&blend[1].raw), Some(0.5));
    // Every region folded: no blend is left, and StdHW reads the moved
    // default.
    let folded = [CffSubtableSurvivors {
        new_outer: None,
        surviving: vec![],
        folded: vec![(0, 1.0), (1, 1.0)],
    }];
    let out = project_private(walk_dict(&dict).unwrap(), &parsed, &folded).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(ints(&out[0]), [Some(13)]);
}

#[test]
fn a_blend_short_of_operands_is_an_error() {
    // Two deltas are needed and one is there.
    let dict = [n(10), n(1), n(1), 23, 10];
    let ivs = store();
    let parsed = ItemVariationStore::parse(&ivs).unwrap();
    let coords = [0.5f32];
    let mut blend = BlendCache::new(Some(&parsed), &ivs, &coords);
    assert!(bake_private(walk_dict(&dict).unwrap(), &mut blend).is_err());
}

#[test]
fn reals_round_trip() {
    for v in [0.0375, -2.594909668, 0.5, 1234.25, -0.00001] {
        let raw = encode_real(v);
        assert_eq!(raw[0], 30);
        let back = decode_real(&raw).unwrap();
        assert!((back - v).abs() < 1e-9, "{v} -> {back}");
    }
}
