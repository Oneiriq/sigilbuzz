//! Tests for ligature and multiple-substitution bookkeeping.

use super::*;
use crate::tables::gdef::Gdef;
use alloc::vec;
use alloc::vec::Vec;

#[test]
fn without_gdef_classes_ligation_updates_the_synthesized_class() {
    let classes = GlyphClasses::synthesized();
    let synthesized = |g: &Glyph| MatchGlyph::from(g).synthesized_kind();
    // Two bases form a real ligature.
    let mut glyphs = run(&[1, 2]);
    ligate_glyphs(&mut glyphs, &[0, 1], 8, &classes, MC);
    assert_eq!(synthesized(&glyphs[0]), GlyphKind::Ligature);
    assert_eq!(lig_id(&glyphs[0]), 1);
    // A base plus a (synthesized) mark stays a base.
    let mut glyphs = run(&[1, 5]);
    glyphs[1].unicode_props = match_prop::SYNTHESIZED_MARK;
    ligate_glyphs(&mut glyphs, &[0, 1], 9, &classes, MC);
    assert_eq!(synthesized(&glyphs[0]), GlyphKind::Base);
    assert_eq!(lig_id(&glyphs[0]), 0);
    // Splitting a ligature glyph again yields base glyphs.
    let mut glyphs = run(&[8, 8]);
    for g in &mut glyphs {
        g.unicode_props = match_prop::SYNTHESIZED_LIGATURE;
    }
    record_multiple(&mut glyphs, 0, 2);
    assert!(glyphs.iter().all(|g| synthesized(g) == GlyphKind::Base));
}

/// GDEF v1.0 whose class def lists glyphs 1..=9: 1-4 bases,
/// 5-7 marks, 8-9 ligatures.
fn gdef_bytes() -> Vec<u8> {
    let classes = [1u16, 1, 1, 1, 3, 3, 3, 2, 2];
    let mut out = vec![0, 1, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0];
    out.extend_from_slice(&1u16.to_be_bytes()); // ClassDef format 1
    out.extend_from_slice(&1u16.to_be_bytes()); // start glyph
    out.extend_from_slice(&(classes.len() as u16).to_be_bytes());
    for c in classes {
        out.extend_from_slice(&c.to_be_bytes());
    }
    out
}

fn run(ids: &[u32]) -> Vec<Glyph> {
    ids.iter()
        .enumerate()
        .map(|(i, &g)| Glyph::new(g, i as u32))
        .collect()
}

const MC: ClusterLevel = ClusterLevel::MonotoneCharacters;

/// Ligates `positions` into `lig_gid` with `gdef`'s classes at the
/// default cluster level.
fn ligate_mc(glyphs: &mut Vec<Glyph>, positions: &[usize], lig_gid: u16, gdef: &Gdef<'_>) {
    ligate_glyphs(
        glyphs,
        positions,
        lig_gid,
        &GlyphClasses::new(Some(gdef)),
        MC,
    );
}

#[test]
fn ligature_clusters_merge_at_monotone_levels_only() {
    let bytes = gdef_bytes();
    let gdef = Gdef::parse(&bytes).unwrap();
    let classes = GlyphClasses::new(Some(&gdef));
    // base(1)@0 mark(5)@1 base(2)@2, ligating the two bases around
    // the mark.
    for (level, clusters) in [
        (ClusterLevel::MonotoneGraphemes, [0, 0]),
        (ClusterLevel::MonotoneCharacters, [0, 0]),
        (ClusterLevel::Characters, [0, 1]),
        (ClusterLevel::Graphemes, [0, 1]),
    ] {
        let mut glyphs = run(&[1, 5, 2]);
        ligate_glyphs(&mut glyphs, &[0, 2], 8, &classes, level);
        let got: Vec<u32> = glyphs.iter().map(|g| g.cluster).collect();
        assert_eq!(got, clusters, "{level:?}");
    }
    // Text in reversed grapheme order: the ligature takes the
    // smallest cluster at a monotone level, its first component's
    // otherwise.
    let mut glyphs: Vec<Glyph> = [(1, 4), (2, 2)]
        .iter()
        .map(|&(id, c)| Glyph::new(id, c))
        .collect();
    let mut chars = glyphs.clone();
    ligate_glyphs(&mut glyphs, &[0, 1], 8, &classes, MC);
    assert_eq!(glyphs[0].cluster, 2);
    let level = ClusterLevel::Characters;
    ligate_glyphs(&mut chars, &[0, 1], 8, &classes, level);
    assert_eq!(chars[0].cluster, 4);
}

fn props(glyphs: &[Glyph]) -> Vec<(u32, u8, u8)> {
    glyphs
        .iter()
        .map(|g| (g.glyph_id, lig_id(g), lig_comp(g)))
        .collect()
}

#[test]
fn marks_between_components_take_their_component_index() {
    let bytes = gdef_bytes();
    let gdef = Gdef::parse(&bytes).unwrap();
    // base(1) mark(5) base(2) mark(6) base(3) mark(7): ligate the
    // three bases, skipping the marks between them.
    let mut glyphs = run(&[1, 5, 2, 6, 3, 7]);
    ligate_mc(&mut glyphs, &[0, 2, 4], 8, &gdef);
    assert_eq!(
        props(&glyphs),
        [(8, 1, 0), (5, 1, 1), (6, 1, 2), (7, 0, 0)],
        "the trailing mark never joined a ligature, so it keeps id 0"
    );
    assert_eq!(num_comps(&glyphs[0], &GlyphClasses::new(Some(&gdef))), 3);
}

#[test]
fn base_plus_marks_is_not_a_ligature() {
    let bytes = gdef_bytes();
    let gdef = Gdef::parse(&bytes).unwrap();
    let mut glyphs = run(&[1, 5, 6]);
    ligate_mc(&mut glyphs, &[0, 1, 2], 4, &gdef);
    assert_eq!(props(&glyphs), [(4, 0, 0)]);
    assert_eq!(num_comps(&glyphs[0], &GlyphClasses::new(Some(&gdef))), 1);
}

#[test]
fn marks_after_the_last_component_stay_out_of_a_plain_ligature() {
    let bytes = gdef_bytes();
    let gdef = Gdef::parse(&bytes).unwrap();
    let mut glyphs = run(&[1, 2, 5]);
    ligate_mc(&mut glyphs, &[0, 1], 8, &gdef);
    assert_eq!(props(&glyphs), [(8, 1, 0), (5, 0, 0)]);
}

#[test]
fn nested_ligature_renumbers_the_inner_ligatures_marks() {
    let bytes = gdef_bytes();
    let gdef = Gdef::parse(&bytes).unwrap();
    // Ligate 1+2 around mark 5 into lig 8 (after base 3), then
    // ligate 3 with lig 8: the mark sat on component 1 of the
    // two-component ligature, which is component 2 of the new
    // three-component one.
    let mut glyphs = run(&[3, 1, 5, 2]);
    ligate_mc(&mut glyphs, &[1, 3], 8, &gdef);
    assert_eq!(props(&glyphs), [(3, 0, 0), (8, 1, 0), (5, 1, 1)]);
    ligate_mc(&mut glyphs, &[0, 1], 9, &gdef);
    assert_eq!(props(&glyphs), [(9, 2, 0), (5, 2, 2)]);
    assert_eq!(num_comps(&glyphs[0], &GlyphClasses::new(Some(&gdef))), 3);
}

#[test]
fn nested_ligature_keeps_an_inner_mark_on_its_component() {
    let bytes = gdef_bytes();
    let gdef = Gdef::parse(&bytes).unwrap();
    let mut glyphs = run(&[1, 5, 2, 3]);
    ligate_mc(&mut glyphs, &[0, 2], 8, &gdef);
    ligate_mc(&mut glyphs, &[0, 2], 9, &gdef);
    assert_eq!(props(&glyphs), [(9, 2, 0), (5, 2, 1)]);
    assert_eq!(num_comps(&glyphs[0], &GlyphClasses::new(Some(&gdef))), 3);
}

#[test]
fn ids_avoid_ligatures_already_in_the_run() {
    let bytes = gdef_bytes();
    let gdef = Gdef::parse(&bytes).unwrap();
    let mut glyphs = run(&[1, 2, 3, 4]);
    ligate_mc(&mut glyphs, &[0, 1], 8, &gdef);
    ligate_mc(&mut glyphs, &[1, 2], 9, &gdef);
    assert_eq!(props(&glyphs), [(8, 1, 0), (9, 2, 0)]);
}

#[test]
fn multiple_substitution_numbers_its_outputs() {
    let mut glyphs = run(&[1, 2, 3]);
    record_multiple(&mut glyphs, 0, 3);
    assert_eq!(props(&glyphs), [(1, 0, 0), (2, 0, 1), (3, 0, 2)]);
    assert!(glyphs.iter().all(is_multiplied));
    // A ligature glyph formed from multiplied glyphs is not
    // multiplied any more.
    let bytes = gdef_bytes();
    let gdef = Gdef::parse(&bytes).unwrap();
    ligate_mc(&mut glyphs, &[0, 1], 8, &gdef);
    assert!(!is_multiplied(&glyphs[0]));
    assert!(is_multiplied(&glyphs[1]));
}

#[test]
fn later_pieces_of_a_multiple_substitution_add_no_component() {
    // HarfBuzz 14.5.0 `_hb_glyph_info_get_lig_num_comps_in_ligation`
    // (issue 4969): base 1 expanded into 1, 2; ligating both pieces
    // with base 3 gives a two-component ligature, not three.
    let bytes = gdef_bytes();
    let gdef = Gdef::parse(&bytes).unwrap();
    let mut glyphs = run(&[1, 2, 3]);
    record_multiple(&mut glyphs, 0, 2);
    ligate_mc(&mut glyphs, &[0, 1, 2], 8, &gdef);
    assert_eq!(num_comps(&glyphs[0], &GlyphClasses::new(Some(&gdef))), 2);
}

#[test]
fn a_ligature_of_a_nonspacing_mark_is_no_mark_any_more() {
    // `ligate_input` turns a first component of General_Category Mn
    // into Lo when a real ligature forms.
    let classes = GlyphClasses::synthesized();
    let mut glyphs = run(&[1, 2]);
    glyphs[0].char_class = char_class::MARK | char_class::NONSPACING_MARK;
    glyphs[0].combining_class = 230;
    ligate_glyphs(&mut glyphs, &[0, 1], 8, &classes, MC);
    assert_eq!((glyphs[0].char_class, glyphs[0].combining_class), (0, 0));
}

#[test]
fn ligate_ignores_positions_that_leave_the_run_or_do_not_rise() {
    let classes = GlyphClasses::synthesized();
    let mut glyphs = run(&[1, 2, 3]);
    let before = glyphs.clone();
    let cases: [&[usize]; 5] = [&[0, 3], &[0, 2, 1], &[1, 1], &[0, usize::MAX - 1], &[]];
    for positions in cases {
        ligate_glyphs(&mut glyphs, positions, 8, &classes, MC);
        assert_eq!(glyphs, before, "{positions:?}");
    }
}

#[test]
fn ligate_keeps_the_glyphs_between_components_in_order() {
    let classes = GlyphClasses::synthesized();
    let mut glyphs = run(&[1, 5, 2, 6, 7, 3, 4]);
    ligate_glyphs(&mut glyphs, &[0, 2, 5], 8, &classes, MC);
    let ids: Vec<u32> = glyphs.iter().map(|g| g.glyph_id).collect();
    assert_eq!(ids, [8, 5, 6, 7, 4]);
}

#[test]
fn record_multiple_ignores_a_span_past_the_run() {
    let mut glyphs = run(&[1, 2]);
    record_multiple(&mut glyphs, 1, usize::MAX);
    record_multiple(&mut glyphs, 1, 2);
    assert!(glyphs.iter().all(|g| g.unicode_props == 0));
}

#[test]
fn single_output_multiple_substitution_records_nothing() {
    let mut glyphs = run(&[1]);
    record_multiple(&mut glyphs, 0, 1);
    assert_eq!(glyphs[0].unicode_props, 0);
}

#[test]
fn multiply_outputs_every_glyph_of_the_sequence_at_the_cursor() {
    let mut buf = GsubBuffer::new(run(&[1, 2, 3]), None);
    buf.clear_output();
    buf.next_glyph();
    multiply(&mut buf, &[7, 8, 9]);
    assert_eq!(buf.cursor(), 4);
    let glyphs = buf.into_glyphs();
    assert_eq!(
        props(&glyphs),
        [(1, 0, 0), (7, 0, 0), (8, 0, 1), (9, 0, 2), (3, 0, 0)]
    );
    assert!(glyphs[1..4]
        .iter()
        .all(|g| is_multiplied(g) && g.cluster == 1));
}

#[test]
fn low_unicode_bits_survive_the_bookkeeping() {
    let mut g = Glyph::new(1, 0);
    g.unicode_props = 0b101;
    set_for_mark(&mut g, 3, 2);
    assert_eq!(g.unicode_props & 0x7F, 0b101);
    assert_eq!((lig_id(&g), lig_comp(&g)), (3, 2));
}
