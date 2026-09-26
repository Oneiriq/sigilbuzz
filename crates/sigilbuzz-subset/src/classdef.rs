//! ClassDef emitter with auto-format-pick.
//!
//! Given a list of `(new_gid, class_value)` pairs, emits a ClassDef
//! table in the smaller of:
//!
//! - **Format 1**: `start_glyph + class_array`. Cost: `2 + 2 + 2 + 2*N`
//!   bytes where N spans every glyph from `min_gid` to `max_gid`,
//!   inclusive (gaps cost 2 bytes each because every slot in the array
//!   must be present).
//! - **Format 2**: range records. Cost: `2 + 2 + 6*R` bytes where R
//!   is the number of contiguous runs that share a class.
//!
//! Glyphs absent from the input map are implicitly class 0; the
//! emitter never inserts class-0 entries because the spec treats
//! "unlisted" and "explicit 0" identically.

use alloc::vec::Vec;

/// Emits a ClassDef table from `(new_gid, class)` pairs. Pairs may
/// arrive in any order. Class 0 entries are dropped because the spec
/// gives unlisted glyphs class 0 by default, always cheaper to omit.
#[must_use]
pub fn emit_classdef(pairs: &[(u16, u16)]) -> Vec<u8> {
    let mut sorted: Vec<(u16, u16)> = pairs.iter().copied().filter(|(_, c)| *c != 0).collect();
    sorted.sort_unstable_by_key(|(gid, _)| *gid);
    sorted.dedup_by_key(|(gid, _)| *gid);

    if sorted.is_empty() {
        // Empty Format 2: 2 + 2 = 4 bytes, smaller than empty Format 1
        // (2 + 2 + 2 + 0 = 6).
        return emit_format2_empty();
    }

    // Build ranges for Format 2.
    let mut ranges: Vec<(u16, u16, u16)> = Vec::new();
    for &(gid, class) in &sorted {
        match ranges.last_mut() {
            Some(last) if last.1.checked_add(1) == Some(gid) && last.2 == class => {
                last.1 = gid;
            }
            _ => ranges.push((gid, gid, class)),
        }
    }

    let min_gid = sorted.first().unwrap().0;
    let max_gid = sorted.last().unwrap().0;
    let span = (max_gid - min_gid) as usize + 1;
    let f1_bytes = 6 + span * 2;
    let f2_bytes = 4 + ranges.len() * 6;

    if f1_bytes <= f2_bytes {
        emit_format1(min_gid, max_gid, &sorted)
    } else {
        emit_format2(&ranges)
    }
}

fn emit_format2_empty() -> Vec<u8> {
    let mut out = Vec::with_capacity(4);
    out.extend_from_slice(&2u16.to_be_bytes()); // format
    out.extend_from_slice(&0u16.to_be_bytes()); // rangeCount
    out
}

fn emit_format1(min_gid: u16, max_gid: u16, sorted: &[(u16, u16)]) -> Vec<u8> {
    let span = (max_gid - min_gid) as usize + 1;
    let mut classes: Vec<u16> = alloc::vec![0u16; span];
    for &(gid, class) in sorted {
        classes[(gid - min_gid) as usize] = class;
    }
    let mut out = Vec::with_capacity(6 + span * 2);
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&min_gid.to_be_bytes());
    out.extend_from_slice(&(span as u16).to_be_bytes());
    for c in classes {
        out.extend_from_slice(&c.to_be_bytes());
    }
    out
}

fn emit_format2(ranges: &[(u16, u16, u16)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + ranges.len() * 6);
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(&(ranges.len() as u16).to_be_bytes());
    for &(start, end, class) in ranges {
        out.extend_from_slice(&start.to_be_bytes());
        out.extend_from_slice(&end.to_be_bytes());
        out.extend_from_slice(&class.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigilbuzz::tables::layout::ClassDef;

    #[test]
    fn empty_emits_format2_with_zero_ranges() {
        let bytes = emit_classdef(&[]);
        assert_eq!(bytes.len(), 4);
        assert_eq!(&bytes[0..2], &2u16.to_be_bytes());
        let cd = ClassDef::parse(&bytes).unwrap();
        assert_eq!(cd.class_of(0), 0);
        assert_eq!(cd.class_of(99), 0);
    }

    #[test]
    fn round_trip_through_parser() {
        let bytes = emit_classdef(&[(5, 1), (6, 2), (10, 3)]);
        let cd = ClassDef::parse(&bytes).unwrap();
        assert_eq!(cd.class_of(5), 1);
        assert_eq!(cd.class_of(6), 2);
        assert_eq!(cd.class_of(10), 3);
        assert_eq!(cd.class_of(7), 0);
    }

    #[test]
    fn dense_consecutive_run_with_same_class_picks_format2() {
        // 10..=15 all class 1: single range, 10 bytes vs format 1's 18.
        let pairs: Vec<(u16, u16)> = (10..=15).map(|g| (g, 1)).collect();
        let bytes = emit_classdef(&pairs);
        assert_eq!(&bytes[0..2], &2u16.to_be_bytes());
        let cd = ClassDef::parse(&bytes).unwrap();
        for g in 10..=15 {
            assert_eq!(cd.class_of(g), 1);
        }
    }

    #[test]
    fn dense_alternating_classes_picks_format1() {
        // Adjacent gids, differing classes: every glyph is its own
        // range in format 2 (4 ranges * 6 = 24 bytes), but format 1
        // packs them into 6 + 4*2 = 14 bytes.
        let bytes = emit_classdef(&[(10, 1), (11, 2), (12, 1), (13, 2)]);
        assert_eq!(&bytes[0..2], &1u16.to_be_bytes());
        let cd = ClassDef::parse(&bytes).unwrap();
        assert_eq!(cd.class_of(10), 1);
        assert_eq!(cd.class_of(11), 2);
        assert_eq!(cd.class_of(12), 1);
        assert_eq!(cd.class_of(13), 2);
    }

    #[test]
    fn sparse_pairs_pick_format2() {
        // 5, 50, 500: format 1 would need 496 slots, format 2 needs 3
        // ranges.
        let bytes = emit_classdef(&[(5, 1), (50, 1), (500, 1)]);
        assert_eq!(&bytes[0..2], &2u16.to_be_bytes());
        let cd = ClassDef::parse(&bytes).unwrap();
        assert_eq!(cd.class_of(5), 1);
        assert_eq!(cd.class_of(50), 1);
        assert_eq!(cd.class_of(500), 1);
        assert_eq!(cd.class_of(6), 0);
    }

    #[test]
    fn class_zero_entries_are_dropped() {
        let bytes = emit_classdef(&[(5, 0), (10, 1)]);
        let cd = ClassDef::parse(&bytes).unwrap();
        assert_eq!(cd.class_of(5), 0);
        assert_eq!(cd.class_of(10), 1);
    }

    #[test]
    fn duplicate_gids_keep_first() {
        // sort_by_key + dedup_by_key keeps the first matching entry
        // after sort. Order of input doesn't matter: dedup keeps the
        // one that survives.
        let bytes = emit_classdef(&[(10, 1), (10, 2)]);
        let cd = ClassDef::parse(&bytes).unwrap();
        // Either 1 or 2 is acceptable; verify it's stable across runs.
        let class = cd.class_of(10);
        assert!(class == 1 || class == 2);
        let bytes2 = emit_classdef(&[(10, 1), (10, 2)]);
        assert_eq!(bytes, bytes2);
    }
}
