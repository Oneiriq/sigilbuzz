//! Tests for the contextual and chained-context parsers and matchers,
//! with and without a mark-skipping filter.

use super::*;

fn build_coverage_format1(glyphs: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
    for g in glyphs {
        out.extend_from_slice(&g.to_be_bytes());
    }
    out
}

fn build_classdef_format2(ranges: &[(u16, u16, u16)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(&(ranges.len() as u16).to_be_bytes());
    for (s, e, c) in ranges {
        out.extend_from_slice(&s.to_be_bytes());
        out.extend_from_slice(&e.to_be_bytes());
        out.extend_from_slice(&c.to_be_bytes());
    }
    out
}

// ------------------------- Context format 1 -------------------------
//
// Layout after the format/coverage/ruleSetCount header (offsets are
// absolute from subtable start):
//   coverage at C
//   ruleSet[i] at S_i; each ruleSet has ruleCount + rule offsets
//     relative to the ruleSet
//   each Rule is { glyphCount, lookupCount, input_tail..., records... }

#[test]
fn context1_parses_and_matches_a_rule() {
    // Cover glyph 10. One ruleset with one rule: input [10, 20, 30],
    // lookup (seq=1, lk=7).
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&0u16.to_be_bytes()); // coverage offset slot
    out.extend_from_slice(&1u16.to_be_bytes()); // rule set count
    out.extend_from_slice(&0u16.to_be_bytes()); // rule set offset slot

    let coverage_off_slot = 2;
    let rule_set_off_slot = 6;

    // Append the ruleset at current length.
    let rule_set_off = out.len();
    out.extend_from_slice(&1u16.to_be_bytes()); // rule count
    out.extend_from_slice(&0u16.to_be_bytes()); // rule offset slot
    let rule_off_slot = rule_set_off + 2;

    // Rule lives immediately after the rule table.
    let rule_off_rel = out.len() - rule_set_off;
    out.extend_from_slice(&3u16.to_be_bytes()); // glyphCount
    out.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
    out.extend_from_slice(&20u16.to_be_bytes()); // tail[0]
    out.extend_from_slice(&30u16.to_be_bytes()); // tail[1]
    out.extend_from_slice(&1u16.to_be_bytes()); // seq
    out.extend_from_slice(&7u16.to_be_bytes()); // lookup index

    // Append coverage and patch slots.
    let cov_off = out.len();
    out.extend_from_slice(&build_coverage_format1(&[10]));
    out[coverage_off_slot..coverage_off_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());
    out[rule_set_off_slot..rule_set_off_slot + 2]
        .copy_from_slice(&(rule_set_off as u16).to_be_bytes());
    out[rule_off_slot..rule_off_slot + 2].copy_from_slice(&(rule_off_rel as u16).to_be_bytes());

    let ctx = Context1::parse(&out).unwrap();
    let (n, lookups) = ctx.matches(&[10, 20, 30, 99], 0).unwrap();
    assert_eq!(n, 3);
    assert_eq!(lookups.len(), 1);
    assert_eq!(lookups[0].sequence_index, 1);
    assert_eq!(lookups[0].lookup_list_index, 7);

    // First glyph uncovered: no match.
    assert!(ctx.matches(&[11, 20, 30], 0).is_none());
    // Input tail mismatch.
    assert!(ctx.matches(&[10, 21, 30], 0).is_none());
}

#[test]
fn context2_class_based_matches_and_rejects() {
    // Coverage: glyphs 10..=11 (both class 1).
    // Class def: 10->1, 11->1, 20..=29 -> 2.
    // One ClassSet at index 1 with one rule: input classes [1, 2],
    //   lookup (seq=0, lk=5).
    //
    // Manual layout:
    //   u16 format=2
    //   u16 coverageOffset
    //   u16 classDefOffset
    //   u16 classSetCount
    //   u16 classSetOffsets[count]
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes()); // format
    out.extend_from_slice(&0u16.to_be_bytes()); // cov slot
    out.extend_from_slice(&0u16.to_be_bytes()); // classDef slot
    out.extend_from_slice(&2u16.to_be_bytes()); // classSetCount (index 0 NULL, index 1 real)
    out.extend_from_slice(&0u16.to_be_bytes()); // set[0] (NULL)
    out.extend_from_slice(&0u16.to_be_bytes()); // set[1] slot

    let cov_slot = 2;
    let cd_slot = 4;
    let set1_slot = 10;

    // ClassSet 1:
    let set_off = out.len();
    out.extend_from_slice(&1u16.to_be_bytes()); // ruleCount
    out.extend_from_slice(&0u16.to_be_bytes()); // rule slot

    // Rule:
    let rule_off_rel = out.len() - set_off;
    out.extend_from_slice(&2u16.to_be_bytes()); // glyphCount
    out.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
    out.extend_from_slice(&2u16.to_be_bytes()); // class tail
    out.extend_from_slice(&0u16.to_be_bytes()); // seq
    out.extend_from_slice(&5u16.to_be_bytes()); // lookup idx

    // Patch the rule offset inside the classset.
    out[set_off + 2..set_off + 4].copy_from_slice(&(rule_off_rel as u16).to_be_bytes());
    out[set1_slot..set1_slot + 2].copy_from_slice(&(set_off as u16).to_be_bytes());

    // Coverage.
    let cov_off = out.len();
    out.extend_from_slice(&build_coverage_format1(&[10, 11]));
    out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());

    // ClassDef (format 2).
    let cd_off = out.len();
    out.extend_from_slice(&build_classdef_format2(&[(10, 11, 1), (20, 29, 2)]));
    out[cd_slot..cd_slot + 2].copy_from_slice(&(cd_off as u16).to_be_bytes());

    let ctx = Context2::parse(&out).unwrap();
    let (n, lookups) = ctx.matches(&[10, 25], 0).unwrap();
    assert_eq!(n, 2);
    assert_eq!(lookups[0].lookup_list_index, 5);
    // Mismatched class for second slot.
    assert!(ctx.matches(&[10, 40], 0).is_none());
    // Uncovered first glyph.
    assert!(ctx.matches(&[12, 25], 0).is_none());
}

#[test]
fn context3_matches_coverage_input() {
    // Input: [cov{5,6}, cov{7}]. On match fire (seq=0, lk=3).
    let mut out = Vec::new();
    out.extend_from_slice(&3u16.to_be_bytes()); // format
    out.extend_from_slice(&2u16.to_be_bytes()); // glyphCount
    out.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
    out.extend_from_slice(&0u16.to_be_bytes()); // cov[0] slot
    out.extend_from_slice(&0u16.to_be_bytes()); // cov[1] slot
    out.extend_from_slice(&0u16.to_be_bytes()); // seq
    out.extend_from_slice(&3u16.to_be_bytes()); // lookup idx
    let cov0_slot = 6;
    let cov1_slot = 8;
    let cov0_off = out.len();
    out.extend_from_slice(&build_coverage_format1(&[5, 6]));
    let cov1_off = out.len();
    out.extend_from_slice(&build_coverage_format1(&[7]));
    out[cov0_slot..cov0_slot + 2].copy_from_slice(&(cov0_off as u16).to_be_bytes());
    out[cov1_slot..cov1_slot + 2].copy_from_slice(&(cov1_off as u16).to_be_bytes());

    let ctx = Context3::parse(&out).unwrap();
    assert!(ctx.matches(&[5, 7], 0));
    assert!(ctx.matches(&[6, 7], 0));
    assert!(!ctx.matches(&[5, 8], 0));
    assert_eq!(ctx.lookups()[0].lookup_list_index, 3);
}

#[test]
fn chain_context3_walks_backtrack_input_lookahead() {
    // Backtrack: [cov{10}], input: [cov{20}], lookahead: [cov{30}].
    let mut out = Vec::new();
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // bt count
    out.extend_from_slice(&0u16.to_be_bytes()); // bt slot
    out.extend_from_slice(&1u16.to_be_bytes()); // in count
    out.extend_from_slice(&0u16.to_be_bytes()); // in slot
    out.extend_from_slice(&1u16.to_be_bytes()); // la count
    out.extend_from_slice(&0u16.to_be_bytes()); // la slot
    out.extend_from_slice(&1u16.to_be_bytes()); // lookup count
    out.extend_from_slice(&0u16.to_be_bytes()); // seq
    out.extend_from_slice(&1u16.to_be_bytes()); // lookup idx
    let bt_slot = 4;
    let in_slot = 8;
    let la_slot = 12;
    let bt_off = out.len();
    out.extend_from_slice(&build_coverage_format1(&[10]));
    let in_off = out.len();
    out.extend_from_slice(&build_coverage_format1(&[20]));
    let la_off = out.len();
    out.extend_from_slice(&build_coverage_format1(&[30]));
    out[bt_slot..bt_slot + 2].copy_from_slice(&(bt_off as u16).to_be_bytes());
    out[in_slot..in_slot + 2].copy_from_slice(&(in_off as u16).to_be_bytes());
    out[la_slot..la_slot + 2].copy_from_slice(&(la_off as u16).to_be_bytes());

    let ctx = ChainContext3::parse(&out).unwrap();
    assert_eq!(ctx.context_len(), (1, 1, 1));
    assert!(ctx.matches(&[10, 20, 30], 1));
    assert!(!ctx.matches(&[11, 20, 30], 1));
    assert!(!ctx.matches(&[10, 21, 30], 1));
    assert!(!ctx.matches(&[10, 20, 31], 1));
}

#[test]
fn chain_context1_glyph_based_matches() {
    // Coverage: {10}. Ruleset with one rule:
    //   backtrack=[5], input_tail=[20], lookahead=[30], lookup=(0,4).
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&0u16.to_be_bytes()); // cov slot
    out.extend_from_slice(&1u16.to_be_bytes()); // ruleset count
    out.extend_from_slice(&0u16.to_be_bytes()); // set slot

    let cov_slot = 2;
    let set_slot = 6;

    let set_off = out.len();
    out.extend_from_slice(&1u16.to_be_bytes()); // rule count
    out.extend_from_slice(&0u16.to_be_bytes()); // rule slot

    let rule_off_rel = out.len() - set_off;
    out.extend_from_slice(&1u16.to_be_bytes()); // bt count
    out.extend_from_slice(&5u16.to_be_bytes()); // bt[0]
    out.extend_from_slice(&2u16.to_be_bytes()); // input count (2 => tail of 1)
    out.extend_from_slice(&20u16.to_be_bytes()); // input tail
    out.extend_from_slice(&1u16.to_be_bytes()); // la count
    out.extend_from_slice(&30u16.to_be_bytes()); // la[0]
    out.extend_from_slice(&1u16.to_be_bytes()); // lookup count
    out.extend_from_slice(&0u16.to_be_bytes()); // seq
    out.extend_from_slice(&4u16.to_be_bytes()); // lookup idx

    out[set_off + 2..set_off + 4].copy_from_slice(&(rule_off_rel as u16).to_be_bytes());
    out[set_slot..set_slot + 2].copy_from_slice(&(set_off as u16).to_be_bytes());

    let cov_off = out.len();
    out.extend_from_slice(&build_coverage_format1(&[10]));
    out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());

    let ctx = ChainContext1::parse(&out).unwrap();
    let (n, lookups) = ctx.matches(&[5, 10, 20, 30], 1).unwrap();
    assert_eq!(n, 2);
    assert_eq!(lookups[0].lookup_list_index, 4);
    assert!(ctx.matches(&[99, 10, 20, 30], 1).is_none()); // bt mismatch
    assert!(ctx.matches(&[5, 10, 21, 30], 1).is_none()); // input mismatch
    assert!(ctx.matches(&[5, 10, 20, 31], 1).is_none()); // la mismatch
}

#[test]
fn chain_context2_class_based_matches() {
    // Coverage: {10, 11}. bt/input/la all share one class def for
    // simplicity.
    //   cd: 5->1, 10->2, 11->2, 20->3, 30->4.
    // Ruleset for class 2 (input class of 10) contains:
    //   bt=[1], input_tail=[3], la=[4], lookup=(0,9).
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes()); // format
    out.extend_from_slice(&0u16.to_be_bytes()); // cov slot
    out.extend_from_slice(&0u16.to_be_bytes()); // bt cd slot
    out.extend_from_slice(&0u16.to_be_bytes()); // in cd slot
    out.extend_from_slice(&0u16.to_be_bytes()); // la cd slot
    out.extend_from_slice(&3u16.to_be_bytes()); // set count
    out.extend_from_slice(&0u16.to_be_bytes()); // set[0]
    out.extend_from_slice(&0u16.to_be_bytes()); // set[1]
    out.extend_from_slice(&0u16.to_be_bytes()); // set[2]
    let cov_slot = 2;
    let bt_cd_slot = 4;
    let in_cd_slot = 6;
    let la_cd_slot = 8;
    // Header is 12 bytes (format..setCount), set[0] at 12, set[1] at 14, set[2] at 16.
    let set2_slot = 16;

    // ClassSet at input-class=2.
    let set_off = out.len();
    out.extend_from_slice(&1u16.to_be_bytes()); // rule count
    out.extend_from_slice(&0u16.to_be_bytes()); // rule slot
    let rule_off_rel = out.len() - set_off;
    out.extend_from_slice(&1u16.to_be_bytes()); // bt count
    out.extend_from_slice(&1u16.to_be_bytes()); // bt class
    out.extend_from_slice(&2u16.to_be_bytes()); // input count
    out.extend_from_slice(&3u16.to_be_bytes()); // input tail class
    out.extend_from_slice(&1u16.to_be_bytes()); // la count
    out.extend_from_slice(&4u16.to_be_bytes()); // la class
    out.extend_from_slice(&1u16.to_be_bytes()); // lookup count
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&9u16.to_be_bytes());
    out[set_off + 2..set_off + 4].copy_from_slice(&(rule_off_rel as u16).to_be_bytes());
    out[set2_slot..set2_slot + 2].copy_from_slice(&(set_off as u16).to_be_bytes());

    // Coverage.
    let cov_off = out.len();
    out.extend_from_slice(&build_coverage_format1(&[10, 11]));
    out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());

    // Shared classdef.
    let cd_off = out.len();
    let cd = build_classdef_format2(&[(5, 5, 1), (10, 11, 2), (20, 20, 3), (30, 30, 4)]);
    out.extend_from_slice(&cd);
    for slot in [bt_cd_slot, in_cd_slot, la_cd_slot] {
        out[slot..slot + 2].copy_from_slice(&(cd_off as u16).to_be_bytes());
    }

    let ctx = ChainContext2::parse(&out).unwrap();
    let (n, lookups) = ctx.matches(&[5, 10, 20, 30], 1).unwrap();
    assert_eq!(n, 2);
    assert_eq!(lookups[0].lookup_list_index, 9);
    assert!(ctx.matches(&[99, 10, 20, 30], 1).is_none());
    assert!(ctx.matches(&[5, 10, 99, 30], 1).is_none());
    assert!(ctx.matches(&[5, 10, 20, 99], 1).is_none());
}

#[test]
fn chain_context2_null_class_defs_put_every_glyph_in_class_zero() {
    // fontmake leaves the backtrack (and often lookahead) ClassDef
    // offset null. A null ClassDef means every glyph is class 0, as
    // in HarfBuzz; reading the subtable header as a ClassDef instead
    // either fails to parse or invents classes.
    //   input cd: 10..=11 -> 1. Rule for input class 1:
    //   bt=[0], input_tail=[], la=[0].
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes()); // format
    out.extend_from_slice(&0u16.to_be_bytes()); // cov slot
    out.extend_from_slice(&0u16.to_be_bytes()); // bt cd: null
    out.extend_from_slice(&0u16.to_be_bytes()); // in cd slot
    out.extend_from_slice(&0u16.to_be_bytes()); // la cd: null
    out.extend_from_slice(&2u16.to_be_bytes()); // set count
    out.extend_from_slice(&0u16.to_be_bytes()); // set[0]
    out.extend_from_slice(&0u16.to_be_bytes()); // set[1]
    let (cov_slot, in_cd_slot, set1_slot) = (2, 6, 14);

    let set_off = out.len();
    out.extend_from_slice(&1u16.to_be_bytes()); // rule count
    out.extend_from_slice(&4u16.to_be_bytes()); // rule offset (from the set)
    out.extend_from_slice(&1u16.to_be_bytes()); // bt count
    out.extend_from_slice(&0u16.to_be_bytes()); // bt class 0
    out.extend_from_slice(&1u16.to_be_bytes()); // input count
    out.extend_from_slice(&1u16.to_be_bytes()); // la count
    out.extend_from_slice(&0u16.to_be_bytes()); // la class 0
    out.extend_from_slice(&0u16.to_be_bytes()); // lookup count
    out[set1_slot..set1_slot + 2].copy_from_slice(&(set_off as u16).to_be_bytes());

    let cov_off = out.len();
    out.extend_from_slice(&build_coverage_format1(&[10, 11]));
    out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());
    let cd_off = out.len();
    out.extend_from_slice(&build_classdef_format2(&[(10, 11, 1)]));
    out[in_cd_slot..in_cd_slot + 2].copy_from_slice(&(cd_off as u16).to_be_bytes());

    let ctx = ChainContext2::parse(&out).expect("null ClassDef offsets are valid");
    assert_eq!(ctx.backtrack_class().class_of(5), 0);
    assert_eq!(ctx.lookahead_class().class_of(11), 0);
    // Any glyph satisfies a class-0 backtrack or lookahead slot,
    // including ones the input ClassDef puts in another class.
    assert!(ctx.matches(&[99, 10, 77], 1).is_some());
    assert!(ctx.matches(&[11, 10, 11], 1).is_some());
    // The context still needs a glyph on each side.
    assert!(ctx.matches(&[10, 77], 0).is_none());
}

/// Build a minimal GDEF where each listed glyph has the given class.
/// ClassDef format 2 requires sorted ranges. The helper sorts
/// the caller's (gid, class) pairs to avoid silent binary-search
/// misses.
fn build_gdef_with_classes(classes: &[(u16, u16)]) -> alloc::vec::Vec<u8> {
    let mut sorted: alloc::vec::Vec<(u16, u16)> = classes.to_vec();
    sorted.sort_by_key(|&(gid, _)| gid);
    let mut cd = alloc::vec::Vec::new();
    cd.extend_from_slice(&2u16.to_be_bytes());
    cd.extend_from_slice(&(sorted.len() as u16).to_be_bytes());
    for (gid, cls) in &sorted {
        cd.extend_from_slice(&gid.to_be_bytes());
        cd.extend_from_slice(&gid.to_be_bytes());
        cd.extend_from_slice(&cls.to_be_bytes());
    }
    let mut gdef = alloc::vec::Vec::new();
    gdef.extend_from_slice(&1u16.to_be_bytes()); // major
    gdef.extend_from_slice(&0u16.to_be_bytes()); // minor
    gdef.extend_from_slice(&12u16.to_be_bytes()); // glyphClassDefOff
    gdef.extend_from_slice(&[0u8; 6]);
    gdef.extend_from_slice(&cd);
    gdef
}

#[test]
fn context3_filtered_matches_across_marks() {
    use crate::tables::gdef::Gdef;
    use crate::tables::layout::skip_iter::{MatchFilter, LOOKUP_FLAG_IGNORE_MARKS};

    // Input: [cov{5,6}, cov{7}]. Same encoding as the pass-through test.
    let mut out = Vec::new();
    out.extend_from_slice(&3u16.to_be_bytes()); // format
    out.extend_from_slice(&2u16.to_be_bytes()); // glyphCount
    out.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&3u16.to_be_bytes());
    let cov0_slot = 6;
    let cov1_slot = 8;
    let cov0_off = out.len();
    out.extend_from_slice(&build_coverage_format1(&[5, 6]));
    let cov1_off = out.len();
    out.extend_from_slice(&build_coverage_format1(&[7]));
    out[cov0_slot..cov0_slot + 2].copy_from_slice(&(cov0_off as u16).to_be_bytes());
    out[cov1_slot..cov1_slot + 2].copy_from_slice(&(cov1_off as u16).to_be_bytes());

    let ctx = Context3::parse(&out).unwrap();
    // 5 = base, 99 = mark, 7 = base.
    let gdef_bytes = build_gdef_with_classes(&[(5, 1), (99, 3), (7, 1)]);
    let gdef = Gdef::parse(&gdef_bytes).unwrap();
    let filter = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_MARKS, Some(&gdef), None);

    // With the filter the mark is hopped over; span covers index 0..=2.
    assert_eq!(ctx.matches_filtered(&[5, 99, 7], 0, &filter), Some(3));
    // Without the filter the plain .matches fails at the mark.
    assert!(!ctx.matches(&[5, 99, 7], 0));
}

#[test]
fn chain_context3_filtered_backtrack_skips_marks() {
    use crate::tables::gdef::Gdef;
    use crate::tables::layout::skip_iter::{MatchFilter, LOOKUP_FLAG_IGNORE_MARKS};

    // bt=[cov{10}], input=[cov{20}], lookahead=[cov{30}].
    let mut out = Vec::new();
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // bt count
    out.extend_from_slice(&0u16.to_be_bytes()); // bt slot
    out.extend_from_slice(&1u16.to_be_bytes()); // in count
    out.extend_from_slice(&0u16.to_be_bytes()); // in slot
    out.extend_from_slice(&1u16.to_be_bytes()); // la count
    out.extend_from_slice(&0u16.to_be_bytes()); // la slot
    out.extend_from_slice(&1u16.to_be_bytes()); // lookup count
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    let bt_slot = 4;
    let in_slot = 8;
    let la_slot = 12;
    let bt_off = out.len();
    out.extend_from_slice(&build_coverage_format1(&[10]));
    let in_off = out.len();
    out.extend_from_slice(&build_coverage_format1(&[20]));
    let la_off = out.len();
    out.extend_from_slice(&build_coverage_format1(&[30]));
    out[bt_slot..bt_slot + 2].copy_from_slice(&(bt_off as u16).to_be_bytes());
    out[in_slot..in_slot + 2].copy_from_slice(&(in_off as u16).to_be_bytes());
    out[la_slot..la_slot + 2].copy_from_slice(&(la_off as u16).to_be_bytes());

    let ctx = ChainContext3::parse(&out).unwrap();
    // GDEF: 10=base, 99=mark, 20=base, 30=base.
    let gdef_bytes = build_gdef_with_classes(&[(10, 1), (99, 3), (20, 1), (30, 1)]);
    let gdef = Gdef::parse(&gdef_bytes).unwrap();
    let filter = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_MARKS, Some(&gdef), None);

    // [10, 99, 20, 30]: plain matcher fails on the mark in backtrack (i=2).
    assert!(!ctx.matches(&[10, 99, 20, 30], 2));
    // With the filter, the mark is skipped and the match fires.
    assert_eq!(ctx.matches_filtered(&[10, 99, 20, 30], 2, &filter), Some(1));
}

#[test]
fn rejects_format_mismatch() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&9u16.to_be_bytes());
    assert!(Context1::parse(&bytes).is_err());
    assert!(Context2::parse(&bytes).is_err());
    assert!(Context3::parse(&bytes).is_err());
    assert!(ChainContext1::parse(&bytes).is_err());
    assert!(ChainContext2::parse(&bytes).is_err());
    assert!(ChainContext3::parse(&bytes).is_err());
}
