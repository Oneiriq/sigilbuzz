//! End-to-end GSUB lookup tests on a synthetic font: contextual and
//! chained-context dispatch, reverse chaining, the nesting depth
//! guard, and the default-ignorable bookkeeping substitution does.

use super::*;
use crate::buffer::{unicode_prop, ClusterLevel, Glyph};
use crate::shape::gsub::substitute_glyph;
use crate::shape::gsub_buffer::GsubBuffer;
use crate::shape::segment::remap_segments;
use crate::tables::gsub::ChainContextAny;
use crate::tables::layout::Joiners;

// ---------------------------------------------------------------
// End-to-end fixtures for the contextual GSUB lookups. The font
// below is the same shape as `build_shapeable_font` plus a
// caller-supplied GSUB table that enables one feature `test` on
// the default script/DFLT LangSys.
// ---------------------------------------------------------------

/// Builds a font identical to `build_shapeable_font` plus a GSUB
/// table carrying whatever lookup subtables the caller provides.
/// Each entry of `lookups` is `(lookup_type, subtable_bytes)`;
/// only the lookup indices in `feature_indices` fire from the
/// top-level `test` feature. The rest are still in the
/// LookupList so nested-lookup dispatch can reach them.
pub(super) fn build_shapeable_font_with_gsub(
    lookups: &[(u16, Vec<u8>)],
    feature_indices: &[u16],
) -> Vec<u8> {
    let gsub_bytes = build_single_feature_gsub_with_filter(*b"test", lookups, feature_indices);

    // Reuse the build_shapeable_font bodies by re-assembling with
    // GSUB appended.
    let mut head = Vec::new();
    head.extend_from_slice(&1u16.to_be_bytes());
    head.extend_from_slice(&0u16.to_be_bytes());
    head.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    head.extend_from_slice(&0u32.to_be_bytes());
    head.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head.extend_from_slice(&0u16.to_be_bytes());
    head.extend_from_slice(&1000u16.to_be_bytes());
    head.extend_from_slice(&[0; 8 + 8 + 8 + 2 + 2 + 2]);
    head.extend_from_slice(&0i16.to_be_bytes());
    head.extend_from_slice(&0i16.to_be_bytes());

    let mut maxp = Vec::new();
    maxp.extend_from_slice(&0x0000_5000u32.to_be_bytes());
    maxp.extend_from_slice(&10u16.to_be_bytes()); // glyph count

    let mut hhea = Vec::new();
    hhea.extend_from_slice(&1u16.to_be_bytes());
    hhea.extend_from_slice(&0u16.to_be_bytes());
    hhea.extend_from_slice(&800i16.to_be_bytes());
    hhea.extend_from_slice(&(-200i16).to_be_bytes());
    hhea.extend_from_slice(&0i16.to_be_bytes());
    hhea.extend_from_slice(&[0; 14]);
    hhea.extend_from_slice(&[0; 8]);
    hhea.extend_from_slice(&0i16.to_be_bytes());
    hhea.extend_from_slice(&10u16.to_be_bytes()); // numberOfHMetrics

    let mut hmtx = Vec::new();
    for adv in [0u16, 500, 600, 700, 500, 500, 500, 500, 500, 500] {
        hmtx.extend_from_slice(&adv.to_be_bytes());
        hmtx.extend_from_slice(&0i16.to_be_bytes());
    }

    // cmap format 4 mapping: A..=C -> 1..=3 (delta -64), D..=F ->
    // 4..=6 (delta -64 too, same range works because we chain
    // another segment). Simplest: map A..=F with delta -64.
    let cmap_sub = build_format4(&[(b'A' as u16, b'F' as u16, -64)]);
    let cmap = build_cmap_wrapper(&[(3, 1, cmap_sub)]);

    let tables: Vec<([u8; 4], Vec<u8>)> = alloc::vec![
        (*b"GSUB", gsub_bytes),
        (*b"cmap", cmap),
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"hmtx", hmtx),
        (*b"maxp", maxp),
    ];
    assemble_sfnt(&tables)
}

/// Builds a GSUB table wiring a single feature tag to a subset
/// of the given lookup list. `feature_lookup_indices` selects
/// which entries fire from the feature; lookups outside the set
/// are still reachable via nested dispatch but do not run as the
/// top-level feature walk.
fn build_single_feature_gsub_with_filter(
    tag: [u8; 4],
    lookups: &[(u16, Vec<u8>)],
    feature_lookup_indices: &[u16],
) -> Vec<u8> {
    // ---- Build LookupList bytes ----
    let mut lookup_list = Vec::new();
    lookup_list.extend_from_slice(&(lookups.len() as u16).to_be_bytes());
    let offsets_start = lookup_list.len();
    for _ in 0..lookups.len() {
        lookup_list.extend_from_slice(&[0u8; 2]);
    }
    // Each lookup header: u16 type, u16 flag, u16 subtableCount,
    // u16 subtableOffset[]. Subtable bodies are appended after
    // the header.
    for (i, (lt, body)) in lookups.iter().enumerate() {
        let lookup_off = lookup_list.len();
        let slot = offsets_start + i * 2;
        lookup_list[slot..slot + 2].copy_from_slice(&(lookup_off as u16).to_be_bytes());
        lookup_list.extend_from_slice(&lt.to_be_bytes());
        lookup_list.extend_from_slice(&0u16.to_be_bytes()); // flag
        lookup_list.extend_from_slice(&1u16.to_be_bytes()); // subtable count
        let sub_slot = lookup_list.len();
        lookup_list.extend_from_slice(&[0u8; 2]); // subtable offset slot
        let sub_off_rel = lookup_list.len() - lookup_off;
        lookup_list[sub_slot..sub_slot + 2].copy_from_slice(&(sub_off_rel as u16).to_be_bytes());
        lookup_list.extend_from_slice(body);
    }

    // ---- FeatureList ----
    // One feature referencing the filtered lookup indices.
    let mut feature_list = Vec::new();
    feature_list.extend_from_slice(&1u16.to_be_bytes()); // featureCount
    let feat_rec_off = feature_list.len();
    feature_list.extend_from_slice(&tag); // featureTag
    feature_list.extend_from_slice(&0u16.to_be_bytes()); // feature offset slot
    let feat_off_rel = feature_list.len();
    // Feature table: featureParamsOffset=0, lookupIndexCount, lookupIndexArray.
    feature_list.extend_from_slice(&0u16.to_be_bytes()); // params offset
    feature_list.extend_from_slice(&(feature_lookup_indices.len() as u16).to_be_bytes());
    for &i in feature_lookup_indices {
        feature_list.extend_from_slice(&i.to_be_bytes());
    }
    feature_list[feat_rec_off + 4..feat_rec_off + 6]
        .copy_from_slice(&(feat_off_rel as u16).to_be_bytes());

    // ---- ScriptList: one DFLT script with default LangSys and feature 0 ----
    let mut script_list = Vec::new();
    script_list.extend_from_slice(&1u16.to_be_bytes()); // scriptCount
    let script_rec_off = script_list.len();
    script_list.extend_from_slice(b"DFLT");
    script_list.extend_from_slice(&0u16.to_be_bytes()); // script offset slot
    let script_off_rel = script_list.len();
    // Script table: defaultLangSysOffset, langSysCount=0.
    script_list.extend_from_slice(&0u16.to_be_bytes()); // default langSys slot
    script_list.extend_from_slice(&0u16.to_be_bytes()); // langSysCount
    let default_langsys_slot = script_off_rel;
    let default_langsys_off_rel = script_list.len() - script_off_rel;
    // LangSys: lookupOrderOffset=0, requiredFeatureIndex=0xFFFF,
    //          featureIndexCount, featureIndexArray.
    script_list.extend_from_slice(&0u16.to_be_bytes()); // lookupOrder
    script_list.extend_from_slice(&0xFFFFu16.to_be_bytes()); // required (none)
    script_list.extend_from_slice(&1u16.to_be_bytes()); // feature count
    script_list.extend_from_slice(&0u16.to_be_bytes()); // feature index 0
    script_list[default_langsys_slot..default_langsys_slot + 2]
        .copy_from_slice(&(default_langsys_off_rel as u16).to_be_bytes());
    script_list[script_rec_off + 4..script_rec_off + 6]
        .copy_from_slice(&(script_off_rel as u16).to_be_bytes());

    // ---- Assemble top-level GSUB header ----
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    let sl_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    let fl_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    let ll_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());

    let sl_off = out.len();
    out.extend_from_slice(&script_list);
    let fl_off = out.len();
    out.extend_from_slice(&feature_list);
    let ll_off = out.len();
    out.extend_from_slice(&lookup_list);

    out[sl_slot..sl_slot + 2].copy_from_slice(&(sl_off as u16).to_be_bytes());
    out[fl_slot..fl_slot + 2].copy_from_slice(&(fl_off as u16).to_be_bytes());
    out[ll_slot..ll_slot + 2].copy_from_slice(&(ll_off as u16).to_be_bytes());
    out
}

// Helpers for building individual subtable bodies.
pub(super) fn build_cov_fmt1(glyphs: &[u16]) -> Vec<u8> {
    let mut o = Vec::new();
    o.extend_from_slice(&1u16.to_be_bytes());
    o.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
    for g in glyphs {
        o.extend_from_slice(&g.to_be_bytes());
    }
    o
}

fn build_classdef_fmt2(ranges: &[(u16, u16, u16)]) -> Vec<u8> {
    let mut o = Vec::new();
    o.extend_from_slice(&2u16.to_be_bytes());
    o.extend_from_slice(&(ranges.len() as u16).to_be_bytes());
    for (s, e, c) in ranges {
        o.extend_from_slice(&s.to_be_bytes());
        o.extend_from_slice(&e.to_be_bytes());
        o.extend_from_slice(&c.to_be_bytes());
    }
    o
}

/// Builds a GSUB type-1 format-2 (explicit) single-sub subtable
/// that maps each glyph in `coverage` to the corresponding entry
/// in `substitutes`.
pub(super) fn build_single_fmt2_subst(coverage_glyphs: &[u16], substitutes: &[u16]) -> Vec<u8> {
    assert_eq!(coverage_glyphs.len(), substitutes.len());
    let mut o = Vec::new();
    o.extend_from_slice(&2u16.to_be_bytes()); // format
    o.extend_from_slice(&0u16.to_be_bytes()); // cov offset slot
    o.extend_from_slice(&(substitutes.len() as u16).to_be_bytes());
    for s in substitutes {
        o.extend_from_slice(&s.to_be_bytes());
    }
    let cov_off = o.len();
    o.extend_from_slice(&build_cov_fmt1(coverage_glyphs));
    o[2..4].copy_from_slice(&(cov_off as u16).to_be_bytes());
    o
}

#[test]
fn gsub_context_fmt3_runs_nested_single_substitution() {
    // Setup: shape "ABC" where glyph A=1, B=2, C=3.
    // Lookup 0: single-sub that rewrites glyph 2 -> 9.
    // Lookup 1: context fmt 3, input = cov{1}, cov{2}, cov{3};
    //           nested lookup (seq=1, lk=0) i.e. fire lookup 0 on
    //           the B position.
    let single = build_single_fmt2_subst(&[2], &[9]);

    // Context fmt 3 subtable:
    //   u16 format = 3
    //   u16 glyphCount = 3
    //   u16 lookupCount = 1
    //   u16 covOffset[3]
    //   (seq, lookup) * 1
    let mut ctx = Vec::new();
    ctx.extend_from_slice(&3u16.to_be_bytes());
    ctx.extend_from_slice(&3u16.to_be_bytes());
    ctx.extend_from_slice(&1u16.to_be_bytes());
    ctx.extend_from_slice(&[0u8; 6]); // three cov slots
    ctx.extend_from_slice(&1u16.to_be_bytes()); // seq
    ctx.extend_from_slice(&0u16.to_be_bytes()); // lookup index
    let cov_slots_start = 6;
    for (j, gs) in [&[1u16][..], &[2u16][..], &[3u16][..]].iter().enumerate() {
        let off = ctx.len();
        ctx.extend_from_slice(&build_cov_fmt1(gs));
        let slot = cov_slots_start + j * 2;
        ctx[slot..slot + 2].copy_from_slice(&(off as u16).to_be_bytes());
    }

    // Feature fires only lookup index 1 (the context lookup);
    // lookup 0 (single-sub) is reachable via nested dispatch.
    let data = build_shapeable_font_with_gsub(&[(1, single), (5, ctx)], &[1]);
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let mut buffer = Buffer::new();
    buffer.push_str("ABC");

    // Enable the custom feature tag with value 1 so the
    // override walker dispatches through apply_gsub_feature.
    let features = [Feature {
        tag: *b"test",
        value: 1,
    }];
    let shaped = shape(&font, &buffer, &features).unwrap();
    assert_eq!(shaped.len(), 3);
    assert_eq!(shaped.glyphs[0].glyph_id, 1); // A unchanged
    assert_eq!(shaped.glyphs[1].glyph_id, 9); // B -> 9 via nested single-sub
    assert_eq!(shaped.glyphs[2].glyph_id, 3); // C unchanged

    // Buffer of length 2 does not match the 3-wide context, so
    // no substitution fires.
    let mut buffer2 = Buffer::new();
    buffer2.push_str("AB");
    let shaped2 = shape(&font, &buffer2, &features).unwrap();
    assert_eq!(shaped2.glyphs[1].glyph_id, 2);
}

#[test]
fn substitute_glyph_clears_only_the_ignorable_bit() {
    let mut g = Glyph::new(0, 4);
    g.unicode_props = unicode_prop::DEFAULT_IGNORABLE | unicode_prop::JOINER;
    substitute_glyph(&mut g, 7);
    assert_eq!(g.glyph_id, 7);
    assert_eq!(g.unicode_props, unicode_prop::JOINER);
    assert_eq!(g.cluster, 4);
}

#[test]
fn multiple_substitution_marks_every_output_glyph_substituted() {
    let mut g = Glyph::new(0, 2);
    g.unicode_props = unicode_prop::DEFAULT_IGNORABLE | unicode_prop::NON_JOINER;
    let mut buf = GsubBuffer::new(alloc::vec![Glyph::new(1, 0), g], None);
    buf.clear_output();
    buf.next_glyph();
    lig::multiply(&mut buf, &[5, 6]);
    let glyphs = buf.into_glyphs();
    assert_eq!(glyphs.len(), 3);
    for (i, out) in glyphs[1..].iter().enumerate() {
        // The low bits are the Unicode properties; the ligature
        // bookkeeping above them numbers the outputs.
        assert_eq!(out.unicode_props & 0x7F, unicode_prop::NON_JOINER);
        assert!(lig::is_multiplied(out));
        assert_eq!(usize::from(lig::lig_comp(out)), i);
        assert_eq!(out.cluster, 2);
    }
}

#[test]
fn unsubstituted_ignorable_without_a_space_glyph_is_deleted() {
    // This font maps neither ZWJ nor space. HarfBuzz hides an
    // ignorable by swapping in the space glyph, and deletes it
    // when there is none; the pen does not move for it either way.
    let data = build_shapeable_font_with_gsub(&[(1, build_single_fmt2_subst(&[0], &[3]))], &[0]);
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let mut buffer = Buffer::new();
    buffer.push_str("A\u{200D}B");
    let shaped = shape(&font, &buffer, &[]).unwrap();
    let ids: Vec<u32> = shaped.glyphs.iter().map(|g| g.glyph_id).collect();
    let advances: Vec<i32> = shaped.glyphs.iter().map(|g| g.x_advance).collect();
    let clusters: Vec<u32> = shaped.glyphs.iter().map(|g| g.cluster).collect();
    assert_eq!(ids, [1, 2]);
    assert_eq!(advances, [500, 600]);
    assert_eq!(clusters, [0, 4]);
}

#[test]
fn gsub_substituted_ignorable_keeps_its_advance() {
    // A single substitution rewrites the ZWJ slot (glyph 0) to
    // glyph 3 (advance 700). HarfBuzz stops hiding a
    // default-ignorable once GSUB substitutes it, so the pen must
    // move by glyph 3's advance.
    let data = build_shapeable_font_with_gsub(&[(1, build_single_fmt2_subst(&[0], &[3]))], &[0]);
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let mut buffer = Buffer::new();
    buffer.push_str("A\u{200D}B");
    let features = [Feature {
        tag: *b"test",
        value: 1,
    }];
    let shaped = shape(&font, &buffer, &features).unwrap();
    let ids: Vec<u32> = shaped.glyphs.iter().map(|g| g.glyph_id).collect();
    let advances: Vec<i32> = shaped.glyphs.iter().map(|g| g.x_advance).collect();
    assert_eq!(ids, [1, 3, 2]);
    assert_eq!(advances, [500, 700, 600]);
}

#[test]
fn gsub_chain_context_fmt2_class_based_runs_real_font() {
    // Fixture: glyphs 1=A, 2=B, 3=C, 4=D, 5=E, 6=F.
    //
    // Shared class def (format 2):
    //   1 -> class 1 (A, "consonant")
    //   2 -> class 2 (B, "vowel")
    //   3 -> class 3 (C, "punct")
    //   4 -> class 1
    //   5 -> class 2
    //
    // Rule: for first glyph of class 2 (vowels B, E), if the
    // preceding glyph is class 1 (A/D) and the following glyph
    // is class 3 (C), run nested single-sub at seq 0 that maps
    // B->7, E->8 (glyph in slot 7/8).
    //
    // Lookup layout:
    //   Lookup 0: single-sub explicit, coverage={2,5}, subs={7,8}.
    //   Lookup 1: chain-context fmt 2, fires lookup 0 at seq 0.
    let single = build_single_fmt2_subst(&[2, 5], &[7, 8]);

    // Build chain-context fmt 2.
    //   u16 format=2
    //   u16 coverageOffset
    //   u16 backtrackClassDefOffset
    //   u16 inputClassDefOffset
    //   u16 lookaheadClassDefOffset
    //   u16 classSetCount (4: classes 0..=3, nulls for 0, 1, 3)
    //   u16 classSetOffsets[]
    let mut cc = Vec::new();
    cc.extend_from_slice(&2u16.to_be_bytes()); // format
    cc.extend_from_slice(&0u16.to_be_bytes()); // cov slot
    cc.extend_from_slice(&0u16.to_be_bytes()); // bt cd
    cc.extend_from_slice(&0u16.to_be_bytes()); // in cd
    cc.extend_from_slice(&0u16.to_be_bytes()); // la cd
    cc.extend_from_slice(&4u16.to_be_bytes()); // class set count
    cc.extend_from_slice(&0u16.to_be_bytes()); // set[0] NULL
    cc.extend_from_slice(&0u16.to_be_bytes()); // set[1] NULL
    cc.extend_from_slice(&0u16.to_be_bytes()); // set[2] slot
    cc.extend_from_slice(&0u16.to_be_bytes()); // set[3] NULL
    let cov_slot = 2;
    let bt_cd_slot = 4;
    let in_cd_slot = 6;
    let la_cd_slot = 8;
    let set2_slot = 16;

    // ClassSet 2 with one rule.
    let set_off = cc.len();
    cc.extend_from_slice(&1u16.to_be_bytes()); // rule count
    cc.extend_from_slice(&0u16.to_be_bytes()); // rule slot
    let rule_off_rel = cc.len() - set_off;
    // Rule body: bt_count=1, bt_classes=[1]; in_count=1, (no tail);
    // la_count=1, la_classes=[3]; lookup_count=1, (seq, lk)=(0, 0).
    cc.extend_from_slice(&1u16.to_be_bytes()); // bt count
    cc.extend_from_slice(&1u16.to_be_bytes()); // bt class
    cc.extend_from_slice(&1u16.to_be_bytes()); // input count (1, so tail is 0)
    cc.extend_from_slice(&1u16.to_be_bytes()); // la count
    cc.extend_from_slice(&3u16.to_be_bytes()); // la class
    cc.extend_from_slice(&1u16.to_be_bytes()); // lookup count
    cc.extend_from_slice(&0u16.to_be_bytes()); // seq index
    cc.extend_from_slice(&0u16.to_be_bytes()); // lookup list index
    cc[set_off + 2..set_off + 4].copy_from_slice(&(rule_off_rel as u16).to_be_bytes());
    cc[set2_slot..set2_slot + 2].copy_from_slice(&(set_off as u16).to_be_bytes());

    // Coverage: {2, 5}.
    let cov_off = cc.len();
    cc.extend_from_slice(&build_cov_fmt1(&[2, 5]));
    cc[cov_slot..cov_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());

    // Shared ClassDef format 2.
    let cd = build_classdef_fmt2(&[(1, 1, 1), (2, 2, 2), (3, 3, 3), (4, 4, 1), (5, 5, 2)]);
    let cd_off = cc.len();
    cc.extend_from_slice(&cd);
    for slot in [bt_cd_slot, in_cd_slot, la_cd_slot] {
        cc[slot..slot + 2].copy_from_slice(&(cd_off as u16).to_be_bytes());
    }

    let data = build_shapeable_font_with_gsub(&[(1, single), (6, cc)], &[1]);
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let features = [Feature {
        tag: *b"test",
        value: 1,
    }];

    // "ABC": A=class1, B=class2, C=class3 -> fires, B->7.
    let mut buf = Buffer::new();
    buf.push_str("ABC");
    let out = shape(&font, &buf, &features).unwrap();
    assert_eq!(out.glyphs[1].glyph_id, 7);
    assert_eq!(out.glyphs[0].glyph_id, 1); // A unchanged
    assert_eq!(out.glyphs[2].glyph_id, 3); // C unchanged

    // "DEC": D=class1, E=class2, C=class3 -> fires, E->8.
    let mut buf = Buffer::new();
    buf.push_str("DEC");
    let out = shape(&font, &buf, &features).unwrap();
    assert_eq!(out.glyphs[1].glyph_id, 8);

    // "ABF": B is class2 but F is class0 (not class3) -> no fire.
    let mut buf = Buffer::new();
    buf.push_str("ABF");
    let out = shape(&font, &buf, &features).unwrap();
    assert_eq!(out.glyphs[1].glyph_id, 2);

    // "BBC": first B has no preceding class-1 backtrack -> no fire.
    let mut buf = Buffer::new();
    buf.push_str("BBC");
    let out = shape(&font, &buf, &features).unwrap();
    assert_eq!(out.glyphs[0].glyph_id, 2);
    // Second B has preceding class-2, not class-1, so also no fire.
    assert_eq!(out.glyphs[1].glyph_id, 2);
}

#[test]
fn gsub_reverse_chain_iterates_right_to_left() {
    // Fixture: glyphs 1=A, 2=B, 3=C, 4=D, 5=E, 6=F.
    //
    // Reverse-chain rule: cover {2,5} (B, E). When lookahead is
    // {3,6} (C, F) substitute B->7, E->8. No backtrack.
    //
    // "BECF": walked right-to-left:
    //   pos 3 (F): not in coverage, skip.
    //   pos 2 (C): not in coverage, skip.
    //   pos 1 (E): cov idx = 1 -> sub 8; lookahead glyph is
    //              currently C which is in {3,6}; substitute E->8.
    //   pos 0 (B): cov idx = 0 -> sub 7; lookahead glyph is
    //              (post-sub) 8 which is NOT in {3,6}, so reject.
    // Expected out: [B=2, E->8, C=3, F=6].
    let mut rc = Vec::new();
    rc.extend_from_slice(&1u16.to_be_bytes()); // format
    rc.extend_from_slice(&0u16.to_be_bytes()); // cov slot
    rc.extend_from_slice(&0u16.to_be_bytes()); // backtrack count
    rc.extend_from_slice(&1u16.to_be_bytes()); // lookahead count
    rc.extend_from_slice(&0u16.to_be_bytes()); // la cov slot
    rc.extend_from_slice(&2u16.to_be_bytes()); // glyphCount
    rc.extend_from_slice(&7u16.to_be_bytes());
    rc.extend_from_slice(&8u16.to_be_bytes());
    // Offsets: format(2) + cov(2) = cov slot at 2; bt_count(2) +
    // la_count(2) = lookahead cov slot at 8.
    let cov_slot = 2;
    let la_slot = 8;
    let cov_off = rc.len();
    rc.extend_from_slice(&build_cov_fmt1(&[2, 5]));
    rc[cov_slot..cov_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());
    let la_off = rc.len();
    rc.extend_from_slice(&build_cov_fmt1(&[3, 6]));
    rc[la_slot..la_slot + 2].copy_from_slice(&(la_off as u16).to_be_bytes());

    let data = build_shapeable_font_with_gsub(&[(8, rc)], &[0]);
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let features = [Feature {
        tag: *b"test",
        value: 1,
    }];

    let mut buf = Buffer::new();
    buf.push_str("BECF");
    let out = shape(&font, &buf, &features).unwrap();
    assert_eq!(
        out.glyphs[0].glyph_id, 2,
        "B should survive: its post-sub lookahead no longer matches"
    );
    assert_eq!(
        out.glyphs[1].glyph_id, 8,
        "E should become 8 via reverse-chain"
    );
    assert_eq!(out.glyphs[2].glyph_id, 3);
    assert_eq!(out.glyphs[3].glyph_id, 6);
}

#[test]
fn gsub_chain_context_depth_guard_bottoms_out() {
    // Self-referential chained-context: lookup 0 is a chain-
    // context fmt 3 whose nested lookup is 0 itself. Without a
    // depth guard this would infinitely recurse and overflow the
    // stack; with the guard, it must bottom out harmlessly.
    //
    // The subtable matches any single glyph at pos i (input
    // coverage = {all glyphs 1..=6}), no backtrack/lookahead,
    // and fires lookup 0 at seq 0 (the lookup itself).
    let mut chain = Vec::new();
    chain.extend_from_slice(&3u16.to_be_bytes()); // format
    chain.extend_from_slice(&0u16.to_be_bytes()); // backtrack count
    chain.extend_from_slice(&1u16.to_be_bytes()); // input count
    chain.extend_from_slice(&0u16.to_be_bytes()); // input cov slot
    chain.extend_from_slice(&0u16.to_be_bytes()); // lookahead count
    chain.extend_from_slice(&1u16.to_be_bytes()); // lookup count
    chain.extend_from_slice(&0u16.to_be_bytes()); // seq
    chain.extend_from_slice(&0u16.to_be_bytes()); // lookup list index = self
    let in_slot = 4;
    let cov_off = chain.len();
    chain.extend_from_slice(&build_cov_fmt1(&[1, 2, 3, 4, 5, 6]));
    chain[in_slot..in_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());

    let data = build_shapeable_font_with_gsub(&[(6, chain)], &[0]);
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let features = [Feature {
        tag: *b"test",
        value: 1,
    }];

    let mut buf = Buffer::new();
    buf.push_str("AB");
    // If the depth guard works, this returns without stack
    // overflow and leaves the glyphs untouched (every nested
    // dispatch is itself a chain-context that does not ultimately
    // perform any concrete substitution).
    let out = shape(&font, &buf, &features).unwrap();
    assert_eq!(out.glyphs[0].glyph_id, 1);
    assert_eq!(out.glyphs[1].glyph_id, 2);
}

#[test]
fn gsub_nested_fan_out_is_bounded() {
    // Chain context whose one rule fires lookup 0 (itself) eight
    // times at the same position. The depth guard alone allows
    // 8^16 nested calls, which never finishes. The nested budget
    // stops the walk after a few thousand.
    let mut chain = Vec::new();
    chain.extend_from_slice(&3u16.to_be_bytes()); // format
    chain.extend_from_slice(&0u16.to_be_bytes()); // backtrack count
    chain.extend_from_slice(&1u16.to_be_bytes()); // input count
    chain.extend_from_slice(&0u16.to_be_bytes()); // input cov slot
    chain.extend_from_slice(&0u16.to_be_bytes()); // lookahead count
    chain.extend_from_slice(&8u16.to_be_bytes()); // lookup count
    for _ in 0..8 {
        chain.extend_from_slice(&0u16.to_be_bytes()); // seq
        chain.extend_from_slice(&0u16.to_be_bytes()); // lookup 0 = self
    }
    let cov_off = chain.len();
    chain.extend_from_slice(&build_cov_fmt1(&[1, 2, 3, 4, 5, 6]));
    chain[6..8].copy_from_slice(&(cov_off as u16).to_be_bytes());
    assert!(ChainContextAny::parse(&chain).is_ok());

    let data = build_shapeable_font_with_gsub(&[(6, chain)], &[0]);
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let features = [Feature {
        tag: *b"test",
        value: 1,
    }];
    let mut buf = Buffer::new();
    buf.push_str("AB");
    let out = shape(&font, &buf, &features).unwrap();
    assert_eq!(out.glyphs.len(), 2);
    assert_eq!(out.glyphs[0].glyph_id, 1);
    assert_eq!(out.glyphs[1].glyph_id, 2);
}

#[test]
fn gsub_multiple_substitution_growth_is_capped() {
    // Two lookups that each turn glyph A into 256 copies. Without
    // a cap one character becomes 65 536 glyphs, and one more
    // lookup exhausts memory. One shape() call may grow the run to
    // max(64 x input glyphs, 16 384) glyphs and no further.
    let lookups: Vec<(u16, Vec<u8>)> = (0..2).map(|_| (2, repeat_a_subtable(256))).collect();
    let data = build_shapeable_font_with_gsub(&lookups, &[0, 1]);
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let features = [Feature {
        tag: *b"test",
        value: 1,
    }];

    let mut buf = Buffer::new();
    buf.push_str("A");
    let out = shape(&font, &buf, &features).unwrap();
    assert!(out.glyphs.len() > 256);
    assert!(out.glyphs.len() <= MAX_LEN_MIN);
    assert!(out.glyphs.iter().all(|g| g.glyph_id == 1 && g.cluster == 0));

    // One lookup stays inside the cap and applies in full.
    let data = build_shapeable_font_with_gsub(&lookups[..1], &[0]);
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let out = shape(&font, &buf, &features).unwrap();
    assert_eq!(out.glyphs.len(), 256);
}

#[test]
fn standalone_feature_growth_is_capped_per_source_byte() {
    // The `ot` pre-shapers apply features one call at a time, so
    // each call gets a fresh budget. The cap on those calls follows
    // the cluster span and cannot compound: twelve doubling lookups
    // leave one character at 64 glyphs, not 4096.
    let lookups: Vec<(u16, Vec<u8>)> = (0..12).map(|_| (2, repeat_a_subtable(2))).collect();
    let feature_indices: Vec<u16> = (0..12).collect();
    let data = build_shapeable_font_with_gsub(&lookups, &feature_indices);
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let gsub = face.gsub().unwrap().expect("GSUB");

    let mut glyphs = alloc::vec![Glyph::new(1, 0)];
    apply_gsub_feature_in_scripts(
        &gsub,
        &mut glyphs,
        None,
        *b"test",
        0,
        DFLT_TEST,
        Joiners::AUTO,
    );
    assert_eq!(glyphs.len(), MAX_LEN_FACTOR);
    for _ in 0..2 {
        apply_gsub_feature_in_scripts(
            &gsub,
            &mut glyphs,
            None,
            *b"test",
            0,
            DFLT_TEST,
            Joiners::AUTO,
        );
    }
    assert_eq!(glyphs.len(), MAX_LEN_FACTOR);
}

/// Multiple substitution format 1 that maps glyph 1 to `count`
/// copies of itself.
fn repeat_a_subtable(count: u16) -> Vec<u8> {
    let mut mult = Vec::new();
    mult.extend_from_slice(&1u16.to_be_bytes()); // format
    let cov_off = 8 + 2 + 2 * count;
    mult.extend_from_slice(&cov_off.to_be_bytes()); // coverage offset
    mult.extend_from_slice(&1u16.to_be_bytes()); // sequence count
    mult.extend_from_slice(&8u16.to_be_bytes()); // sequence offset
    mult.extend_from_slice(&count.to_be_bytes()); // glyph count
    for _ in 0..count {
        mult.extend_from_slice(&1u16.to_be_bytes());
    }
    mult.extend_from_slice(&build_cov_fmt1(&[1]));
    mult
}

#[test]
fn remap_segments_follows_morx_origins() {
    let segments = [
        ProcessedSegment {
            range: 0..2,
            script_priority: DFLT_TEST,
        },
        ProcessedSegment {
            range: 2..3,
            script_priority: ARAB_TEST,
        },
    ];
    // Glyphs 0 and 1 ligate. An inserted glyph follows the
    // Arabic one.
    let remapped = remap_segments(&segments, &[0, 2, usize::MAX]);
    assert_eq!(remapped.len(), 2);
    assert_eq!(remapped[0].range, 0..1);
    assert_eq!(remapped[0].script_priority, DFLT_TEST);
    assert_eq!(remapped[1].range, 1..3);
    assert_eq!(remapped[1].script_priority, ARAB_TEST);

    assert!(remap_segments(&[], &[0, 1]).is_empty());
}

pub(super) const DFLT_TEST: &[[u8; 4]] = &[*b"DFLT"];
const ARAB_TEST: &[[u8; 4]] = &[*b"arab", *b"DFLT"];

#[test]
fn an_empty_multiple_substitution_sequence_deletes_its_glyph() {
    // HarfBuzz's `Sequence::apply` deletes the glyph when the sequence
    // is empty (HarfBuzz issue 253) through `hb_buffer_t::delete_glyph`,
    // whose cluster goes to the glyph before it, or at the monotone
    // levels to the glyph after it when it starts the run. Expected
    // output from HarfBuzz 14.5.0 (uharfbuzz 0.56.2) on these font
    // bytes.
    let data = build_shapeable_font_with_gsub(&[(2, repeat_a_subtable(0))], &[0]);
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let features = [Feature {
        tag: *b"test",
        value: 1,
    }];
    let run = |text: &str, level: ClusterLevel| {
        let mut buffer = Buffer::new();
        buffer.push_str(text);
        buffer.set_cluster_level(level);
        let shaped = shape(&font, &buffer, &features).unwrap();
        shaped
            .glyphs
            .iter()
            .map(|g| (g.glyph_id, g.cluster))
            .collect::<Vec<_>>()
    };
    let mc = ClusterLevel::MonotoneCharacters;
    assert_eq!(run("BAB", mc), [(2, 0), (2, 2)]);
    assert_eq!(
        run("CAB", ClusterLevel::MonotoneGraphemes),
        [(3, 0), (2, 2)]
    );
    assert_eq!(run("AB", mc), [(2, 0)]);
    assert_eq!(run("AAAB", mc), [(2, 0)]);
    assert_eq!(run("AB", ClusterLevel::Characters), [(2, 1)]);
}
