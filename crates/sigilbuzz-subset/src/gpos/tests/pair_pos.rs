//! GPOS type 2 (pair adjustment) rewriter tests, formats 1 and 2.

use super::*;

// ----- Type 2: Pair Adjustment, format 1 -----

fn build_pair_pos_format1(covered: &[u16], pairs: &[&[(u16, i16, i16)]]) -> Vec<u8> {
    assert_eq!(covered.len(), pairs.len());
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    let cov_off_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&X_ADVANCE.to_be_bytes()); // valueFormat1
    out.extend_from_slice(&X_ADVANCE.to_be_bytes()); // valueFormat2
    out.extend_from_slice(&(pairs.len() as u16).to_be_bytes()); // pairSetCount
    let pair_set_offsets_start = out.len();
    for _ in 0..pairs.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    for (i, entries) in pairs.iter().enumerate() {
        let set_start = out.len();
        out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        for (second, v1, v2) in *entries {
            out.extend_from_slice(&second.to_be_bytes());
            out.extend_from_slice(&v1.to_be_bytes());
            out.extend_from_slice(&v2.to_be_bytes());
        }
        let slot = pair_set_offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&(set_start as u16).to_be_bytes());
    }
    let cov_start = out.len();
    out.extend_from_slice(&build_coverage_format1(covered));
    out[cov_off_slot..cov_off_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
    out
}

#[test]
fn rewrite_pair_pos_format1_keeps_surviving_pairs() {
    // first 10 -> second {15, 25}; first 20 -> second {5}.
    // Map: 10->1, 15->2, 20->3, drop 5, drop 25.
    let bytes = build_pair_pos_format1(&[10, 20], &[&[(15, -30, 0), (25, 5, 0)], &[(5, -50, 0)]]);
    let map = map_from_pairs(&[(0, 0), (10, 1), (15, 2), (20, 3)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = one(rewrite_pair_pos_format1(&ctx, &bytes));
    let pp = PairPos::parse(&rs.bytes).unwrap();
    // (1, 2) -> -30 survives; (1, 25)/(3, 5) drop.
    let (v1, _) = pp.lookup(1, 2).unwrap();
    assert_eq!(v1.x_advance, -30);
    // First 3 (was 20) had only second 5 which dropped; that
    // PairSet should be gone, so first 3 is not in the new
    // coverage.
    assert!(pp.lookup(3, 5).is_none());
}

#[test]
fn rewrite_pair_pos_format1_returns_none_when_all_drop() {
    let bytes = build_pair_pos_format1(&[10], &[&[(15, -30, 0)]]);
    let map = map_from_pairs(&[(0, 0)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_pair_pos_format1(&ctx, &bytes).is_empty());
}

// ----- Type 2: Pair Adjustment, format 2 -----

fn build_classdef_format1(start: u16, values: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&start.to_be_bytes());
    out.extend_from_slice(&(values.len() as u16).to_be_bytes());
    for v in values {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}

fn build_pair_pos_format2(covered: &[u16], cd1: &[u8], cd2: &[u8], matrix: &[&[i16]]) -> Vec<u8> {
    let class1_count = matrix.len() as u16;
    let class2_count = matrix.first().map_or(0, |row| row.len()) as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes()); // posFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset
    out.extend_from_slice(&X_ADVANCE.to_be_bytes()); // valueFormat1
    out.extend_from_slice(&0u16.to_be_bytes()); // valueFormat2
    let cd1_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // classDef1Offset
    let cd2_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // classDef2Offset
    out.extend_from_slice(&class1_count.to_be_bytes());
    out.extend_from_slice(&class2_count.to_be_bytes());
    for row in matrix {
        for cell in *row {
            out.extend_from_slice(&cell.to_be_bytes());
        }
    }
    let cov_start = out.len();
    out.extend_from_slice(&build_coverage_format1(covered));
    out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
    let cd1_start = out.len();
    out.extend_from_slice(cd1);
    out[cd1_slot..cd1_slot + 2].copy_from_slice(&(cd1_start as u16).to_be_bytes());
    let cd2_start = out.len();
    out.extend_from_slice(cd2);
    out[cd2_slot..cd2_slot + 2].copy_from_slice(&(cd2_start as u16).to_be_bytes());
    out
}

#[test]
fn rewrite_pair_pos_format2_pass_through_when_class_structure_preserved() {
    // Coverage: 10, 11. classDef1: both class 1. classDef2: 20->0, 21->1, 22->2.
    // Matrix 2x3: [[0,0,0], [0,-25,-15]].
    // Map every gid to itself but down by 1 (10->9, etc.). Class
    // structure is preserved (we keep all members of each class).
    let cd1 = build_classdef_format1(10, &[1, 1]);
    let cd2 = build_classdef_format1(20, &[0, 1, 2]);
    let matrix: &[&[i16]] = &[&[0, 0, 0], &[0, -25, -15]];
    let bytes = build_pair_pos_format2(&[10, 11], &cd1, &cd2, matrix);

    let map = map_from_pairs(&[(0, 0), (10, 9), (11, 10), (20, 19), (21, 20), (22, 21)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = one(rewrite_pair_pos_format2(&ctx, &bytes).unwrap());
    let pp = PairPos::parse(&rs.bytes).unwrap();
    // (9, 20) -> class1=1, class2=1 -> -25.
    let (v1, _) = pp.lookup(9, 20).unwrap();
    assert_eq!(v1.x_advance, -25);
    // (10, 21) -> class1=1, class2=2 -> -15.
    let (v1b, _) = pp.lookup(10, 21).unwrap();
    assert_eq!(v1b.x_advance, -15);
}

// ----- PairPos format 2: class collapse via fmt-1 fallback -----

#[test]
fn rewrite_pair_pos_format2_class_collapse_uses_format1_fallback() {
    // Source: 4 first-glyphs in 2 classes (10/11 -> class 1, 12/13 ->
    // class 2); 4 second-glyphs in 2 classes (20/21 -> class 1,
    // 22/23 -> class 2). Matrix:
    //
    //   class1=0: [0, 0, 0]
    //   class1=1: [0, -10, -20]
    //   class1=2: [0, -30, -40]
    //
    // Drop 11 and 13: the kept set spans class 1 (via 10) and
    // class 2 (via 12) on the first axis, but only class 1
    // (via 20) and class 2 (via 22) on the second.
    let cd1 = build_classdef_format1(10, &[1, 1, 2, 2]);
    let cd2 = build_classdef_format1(20, &[1, 1, 2, 2]);
    let matrix: &[&[i16]] = &[&[0, 0, 0], &[0, -10, -20], &[0, -30, -40]];
    let bytes = build_pair_pos_format2(&[10, 11, 12, 13], &cd1, &cd2, matrix);

    // Drop 11 and 13.
    let map = map_from_pairs(&[(0, 0), (10, 1), (12, 2), (20, 3), (22, 4)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = one(rewrite_pair_pos_format2(&ctx, &bytes).unwrap());
    let pp = PairPos::parse(&rs.bytes).unwrap();
    // (1, 3) -> original (10, 20), class1=1 x class2=1 -> -10.
    let (v1, _) = pp.lookup(1, 3).unwrap();
    assert_eq!(v1.x_advance, -10);
    // (1, 4) -> (10, 22), class1=1 x class2=2 -> -20.
    let (v1b, _) = pp.lookup(1, 4).unwrap();
    assert_eq!(v1b.x_advance, -20);
    // (2, 3) -> (12, 20), class1=2 x class2=1 -> -30.
    let (v1c, _) = pp.lookup(2, 3).unwrap();
    assert_eq!(v1c.x_advance, -30);
    // (2, 4) -> (12, 22), class1=2 x class2=2 -> -40.
    let (v1d, _) = pp.lookup(2, 4).unwrap();
    assert_eq!(v1d.x_advance, -40);
}

#[test]
fn rewrite_pair_pos_format2_class_collapse_drops_zero_kerning() {
    // Same shape as above but matrix[1][1] = 0. The (1, 3) pair
    // should drop because the surviving cell is all-zero.
    let cd1 = build_classdef_format1(10, &[1, 1]);
    let cd2 = build_classdef_format1(20, &[1, 1]);
    let matrix: &[&[i16]] = &[&[0, 0], &[0, 0]];
    let bytes = build_pair_pos_format2(&[10, 11], &cd1, &cd2, matrix);
    let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2)]);
    let ctx = RewriterCtx::new(&map, None);
    // Every cell is zero -> no surviving pairs -> subtable drops.
    assert!(rewrite_pair_pos_format2(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_pair_pos_format2_pass_through_for_large_subsets() {
    // When the kept set is large the heuristic in
    // `should_use_format1_fallback` keeps us on the fmt-2
    // pass-through. We exercise that path by building a kept-gid
    // set whose first x second cross product blows past the 256
    // budget. The matrix bytes survive verbatim through the
    // pass-through path.
    let covered: Vec<u16> = (10..=30).collect();
    let cd1_classes: Vec<u16> = covered.iter().map(|_| 1).collect();
    let cd1 = build_classdef_format1(10, &cd1_classes);
    let cd2 = build_classdef_format1(40, &alloc::vec![1u16; 21]);
    let matrix: &[&[i16]] = &[&[0, 0], &[0, -25]];
    let bytes = build_pair_pos_format2(&covered, &cd1, &cd2, matrix);

    // Keep everything (large kept set; cross product = 21 * ~21 = 441 > 256).
    let mut pairs: Vec<(u16, u16)> = alloc::vec![(0, 0)];
    for g in 10..=30 {
        pairs.push((g, g - 9));
    }
    for g in 40..=60 {
        pairs.push((g, g - 18));
    }
    let map = map_from_pairs(&pairs);
    let ctx = RewriterCtx::new(&map, None);
    let rs = one(rewrite_pair_pos_format2(&ctx, &bytes).unwrap());
    let pp = PairPos::parse(&rs.bytes).unwrap();
    // (1, 22) -> original (10, 40): class1=1, class2=1 -> -25.
    let (v1, _) = pp.lookup(1, 22).unwrap();
    assert_eq!(v1.x_advance, -25);
}
