//! Unit tests for the layout plan, the gid map, the byte-level readers
//! and the lookup renumber.

use super::bytes::parse_classdef_pairs_from_bytes;
use super::driver::{assemble_layout_table, build_renumber};
use super::*;
use alloc::{vec, vec::Vec};

const OPEN_SANS: &[u8] = include_bytes!("../../../../tests/fixtures/opensans_regular.ttf");

fn open_sans_face() -> sigilbuzz::Face<'static> {
    sigilbuzz::Face::parse_bytes(OPEN_SANS, 0).unwrap()
}

#[test]
fn drop_layout_when_retain_layout_false() {
    let face = open_sans_face();
    let kept: alloc::vec::Vec<u16> = (0..face.maxp().unwrap().num_glyphs).collect();
    let input = SubsetInput {
        retain_layout: false,
        ..Default::default()
    };
    let plan = decide(&face, &kept, &input, &Warnings::default()).unwrap();
    assert!(matches!(plan.gsub, Decision::Drop));
    assert!(matches!(plan.gpos, Decision::Drop));
    assert!(matches!(plan.gdef, Decision::Drop));
}

#[test]
fn preserve_layout_when_kept_set_is_identity() {
    let face = open_sans_face();
    let kept: alloc::vec::Vec<u16> = (0..face.maxp().unwrap().num_glyphs).collect();
    let input = SubsetInput {
        retain_layout: true,
        ..Default::default()
    };
    let plan = decide(&face, &kept, &input, &Warnings::default()).unwrap();
    assert!(matches!(plan.gsub, Decision::Preserve));
    assert!(matches!(plan.gpos, Decision::Preserve));
    assert!(matches!(plan.gdef, Decision::Preserve));
}

#[test]
fn proper_subset_routes_through_rewriter() {
    // Today the rewriter ships GSUB type 1 + GDEF classdefs; GPOS
    // drops everything. So we expect gsub: Rewrite-or-Drop, gpos:
    // Drop, gdef: Rewrite-or-Drop. The exact pick depends on
    // whether the source's lookups have any type-1 subtables, so
    // we just check that we do *not* hit Preserve (the old
    // identity-only policy) and that the proper-subset case
    // doesn't blow up.
    let face = open_sans_face();
    let kept: alloc::vec::Vec<u16> = alloc::vec![0, 36, 37, 38];
    let input = SubsetInput {
        retain_layout: true,
        ..Default::default()
    };
    let plan = decide(&face, &kept, &input, &Warnings::default()).unwrap();
    assert!(!matches!(plan.gsub, Decision::Preserve));
    assert!(!matches!(plan.gpos, Decision::Preserve));
    assert!(!matches!(plan.gdef, Decision::Preserve));
}

#[test]
fn gid_map_is_identity_for_full_kept_set() {
    let kept: Vec<u16> = (0..10).collect();
    let map = GidMap::from_kept(&kept);
    assert!(map.is_identity());
}

#[test]
fn gid_map_filters_dropped_gids() {
    let kept: Vec<u16> = vec![0, 5, 10];
    let map = GidMap::from_kept(&kept);
    assert_eq!(map.map(0), Some(0));
    assert_eq!(map.map(5), Some(1));
    assert_eq!(map.map(10), Some(2));
    assert_eq!(map.map(1), None);
    assert_eq!(map.map(99), None);
    assert!(!map.is_identity());
}

#[test]
fn parse_coverage_glyphs_handles_format1() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(&3u16.to_be_bytes());
    for g in [10u16, 20, 30] {
        bytes.extend_from_slice(&g.to_be_bytes());
    }
    let glyphs = parse_coverage_glyphs(&bytes);
    assert_eq!(glyphs, vec![10, 20, 30]);
}

#[test]
fn parse_coverage_glyphs_handles_format2() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&2u16.to_be_bytes());
    bytes.extend_from_slice(&1u16.to_be_bytes()); // rangeCount
    bytes.extend_from_slice(&5u16.to_be_bytes()); // start
    bytes.extend_from_slice(&7u16.to_be_bytes()); // end
    bytes.extend_from_slice(&0u16.to_be_bytes()); // startCov
    let glyphs = parse_coverage_glyphs(&bytes);
    assert_eq!(glyphs, vec![5, 6, 7]);
}

#[test]
fn parse_classdef_pairs_skips_class_zero() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&2u16.to_be_bytes()); // format
    bytes.extend_from_slice(&2u16.to_be_bytes()); // rangeCount
    for (start, end, class) in [(5u16, 5u16, 1u16), (7, 7, 0)] {
        bytes.extend_from_slice(&start.to_be_bytes());
        bytes.extend_from_slice(&end.to_be_bytes());
        bytes.extend_from_slice(&class.to_be_bytes());
    }
    let pairs = parse_classdef_pairs_from_bytes(&bytes);
    assert_eq!(pairs, vec![(5, 1)]);
}

#[test]
fn classdef_pairs_at_reads_a_null_offset_as_empty() {
    // A context format 2 header: its first word (2) would read as a
    // ClassDef format, so a null offset must not parse from 0.
    let mut sub = Vec::new();
    sub.extend_from_slice(&2u16.to_be_bytes()); // subtable format
    sub.extend_from_slice(&1u16.to_be_bytes());
    sub.extend_from_slice(&[0, 5, 0, 5, 0, 9]);
    let map = GidMap::from_kept(&[0]);
    assert_eq!(map.classdef_pairs_at(&sub, 0), Some(Vec::new()));
    let cd_off = sub.len();
    sub.extend_from_slice(&2u16.to_be_bytes()); // ClassDef format 2
    sub.extend_from_slice(&1u16.to_be_bytes()); // rangeCount
    sub.extend_from_slice(&[0, 8, 0, 8, 0, 2]);
    assert_eq!(map.classdef_pairs_at(&sub, cd_off), Some(vec![(8, 2)]));
    assert_eq!(map.classdef_pairs_at(&sub, sub.len() + 1), None);
}

#[test]
fn build_renumber_skips_dropped() {
    let rewritten: Vec<Option<RewrittenLookup>> = vec![
        Some(RewrittenLookup {
            lookup_type: 1,
            lookup_flag: 0,
            mark_filtering_set: None,
            subtables: vec![RewrittenSubtable { bytes: vec![] }],
        }),
        None,
        Some(RewrittenLookup {
            lookup_type: 1,
            lookup_flag: 0,
            mark_filtering_set: None,
            subtables: vec![RewrittenSubtable { bytes: vec![] }],
        }),
    ];
    let r = build_renumber(&rewritten);
    assert_eq!(r, vec![Some(0), None, Some(1)]);
}

/// Format 2 table (Coverage or ClassDef) with `count` copies of a
/// range covering every glyph, all in class 1.
fn overlapping_full_ranges(count: u16) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&2u16.to_be_bytes());
    bytes.extend_from_slice(&count.to_be_bytes());
    for _ in 0..count {
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&0xFFFFu16.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
    }
    bytes
}

#[test]
fn parse_coverage_glyphs_caps_overlapping_ranges() {
    // 65535 full ranges describe about 4.3 billion entries in 393 KB.
    // The walk used to materialize all of them.
    let bytes = overlapping_full_ranges(u16::MAX);
    assert_eq!(parse_coverage_glyphs(&bytes).len(), MAX_GLYPH_ENTRIES);
}

#[test]
fn parse_classdef_pairs_caps_overlapping_ranges() {
    let bytes = overlapping_full_ranges(u16::MAX);
    assert_eq!(
        parse_classdef_pairs_from_bytes(&bytes).len(),
        MAX_GLYPH_ENTRIES
    );
}

#[test]
fn gid_map_budget_stops_coverage_walks() {
    let map = GidMap::from_kept(&[0, 1]);
    let bytes = overlapping_full_ranges(1);
    assert!(map.coverage_glyphs(&bytes).is_some());
    assert!(!map.spend(usize::MAX));
    assert!(map.budget_spent());
    assert!(map.coverage_glyphs(&bytes).is_none());
    assert!(map.classdef_pairs(&bytes).is_none());
    map.reset_budget();
    assert!(map.coverage_glyphs(&bytes).is_some());
}

/// One lookup of type 1 holding `count` subtables of `size` bytes
/// each. Subtable bytes start with format 1 so they read as valid.
fn big_lookup(count: usize, size: usize) -> RewrittenLookup {
    let mut body = vec![0u8; size];
    body[1] = 1;
    RewrittenLookup {
        lookup_type: 1,
        lookup_flag: 0,
        mark_filtering_set: None,
        subtables: (0..count)
            .map(|_| RewrittenSubtable {
                bytes: body.clone(),
            })
            .collect(),
    }
}

/// Reads `(lookup type, [(wrapped type, subtable offset)])` for each
/// lookup of an assembled table, following Extension records.
fn read_lookups(table: &[u8], ext: u16) -> Vec<(u16, Vec<(u16, usize)>)> {
    let rd = |o: usize| usize::from(u16::from_be_bytes([table[o], table[o + 1]]));
    let ll = rd(8);
    (0..rd(ll))
        .map(|i| {
            let base = ll + rd(ll + 2 + i * 2);
            let ty = rd(base) as u16;
            let subs = (0..rd(base + 4))
                .map(|s| {
                    let sub = base + rd(base + 6 + s * 2);
                    if ty == ext {
                        let off = u32::from_be_bytes([
                            table[sub + 4],
                            table[sub + 5],
                            table[sub + 6],
                            table[sub + 7],
                        ]) as usize;
                        (rd(sub + 2) as u16, sub + off)
                    } else {
                        (ty, sub)
                    }
                })
                .collect();
            (ty, subs)
        })
        .collect()
}

#[test]
fn assemble_keeps_inline_layout_when_offsets_fit() {
    let lookups = vec![big_lookup(2, 100)];
    let table = assemble_layout_table(&[0, 0], &[0, 0], &lookups, 7, None).unwrap();
    let read = read_lookups(&table, 7);
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].0, 1);
}

#[test]
fn assemble_promotes_to_extension_lookups_past_16_bit_offsets() {
    // Three lookups of 40 KB each: the third lookup starts past
    // 64 KB, which used to wrap its Offset16 and point it at the
    // wrong bytes.
    let lookups = vec![
        big_lookup(1, 40_000),
        big_lookup(1, 40_000),
        big_lookup(2, 30_000),
    ];
    let table = assemble_layout_table(&[0, 0], &[0, 0], &lookups, 7, None).unwrap();
    let read = read_lookups(&table, 7);
    assert_eq!(read.len(), 3);
    for (lookup, subs) in &read {
        assert_eq!(*lookup, 7, "every lookup becomes an Extension lookup");
        for &(wrapped, off) in subs {
            assert_eq!(wrapped, 1);
            assert_eq!(&table[off..off + 2], &1u16.to_be_bytes());
        }
    }
    assert_eq!(read[2].1.len(), 2);
}

#[test]
fn assemble_fails_when_script_and_feature_lists_overflow() {
    let lookups = vec![big_lookup(1, 10)];
    let huge = vec![0u8; 70_000];
    assert!(assemble_layout_table(&huge, &[0, 0], &lookups, 7, None).is_err());
}
