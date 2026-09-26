//! Which glyph an attachment lookup attaches to, per HarfBuzz's
//! MarkBasePos / MarkLigPos / MarkMarkPos / CursivePos: coverage
//! decides what attaches, the backward search passes over default
//! ignorables, ligature component ids pick the component, and
//! mark-to-mark stacks only marks of the same component.

use super::*;
use crate::buffer::{unicode_prop, ClusterLevel};
use crate::tables::layout::LOOKUP_FLAG_IGNORE_BASE_GLYPHS;

const MC: ClusterLevel = ClusterLevel::MonotoneCharacters;

/// A GSUB substitution callback that only swaps the glyph id.
fn swap_id(g: &mut Glyph, gid: u16) {
    g.glyph_id = u32::from(gid);
}

fn ignorable(props: u16) -> Glyph {
    let mut g = glyph(9, 0);
    g.unicode_props = unicode_prop::DEFAULT_IGNORABLE | props;
    g
}

#[test]
fn mark_coverage_not_the_gdef_class_decides_what_attaches() {
    // As in HarfBuzz, the attaching glyph only has to be in the mark
    // coverage: with no GDEF at all it still attaches to the glyph
    // before it.
    let bytes = mark_attach_subtable(2, &anchor1(0, 0), 1, &anchor1(100, 0));
    let subs = [AttachSubtable::parse(gpos_lt::MARK_TO_BASE, &bytes).unwrap()];
    let mut glyphs = vec![glyph(1, 600), glyph(2, 0)];
    let slots = run_lookup(&subs, &mut glyphs, None, 0, Direction::Ltr, &VarCtx::none());
    assert_eq!(slots[1], mark_slot(-1));

    // A glyph GDEF does not class as a mark attaches as well.
    let gdef_raw = gdef_bytes(&[], &[]);
    let gdef = Gdef::parse(&gdef_raw).unwrap();
    let mut glyphs = vec![glyph(1, 600), glyph(2, 0)];
    let slots = run_lookup(
        &subs,
        &mut glyphs,
        Some(&gdef),
        0,
        Direction::Ltr,
        &VarCtx::none(),
    );
    assert_eq!(slots[1], mark_slot(-1));
}

#[test]
fn mark_to_ligature_uses_the_component_recorded_at_ligation() {
    let bytes = mark_liga_subtable(2, 1, &[100, 400]);
    let subs = [AttachSubtable::parse(gpos_lt::MARK_TO_LIGATURE, &bytes).unwrap()];
    let gdef_raw = gdef_bytes(&[2], &[1]);
    let gdef = Gdef::parse(&gdef_raw).unwrap();
    // Components 4 and 5 ligate into 1 around the mark, which lands
    // on component 1 (index 0); a mark that follows the ligature
    // without having been inside it goes on the last component.
    let mut glyphs = vec![glyph(4, 0), glyph(2, 0), glyph(5, 0), glyph(2, 0)];
    lig::ligate(&mut glyphs, 0, &[0, 2], 1, Some(&gdef), swap_id, MC);
    assert_eq!(glyphs.len(), 3);
    glyphs[0].x_advance = 800;
    let slots = run_lookup(
        &subs,
        &mut glyphs,
        Some(&gdef),
        0,
        Direction::Ltr,
        &VarCtx::none(),
    );
    assert_eq!((slots[1], glyphs[1].x_offset), (mark_slot(-1), 100));
    assert_eq!((slots[2], glyphs[2].x_offset), (mark_slot(-2), 400));
}

#[test]
fn base_search_passes_over_zwnj_and_over_zwj_unless_joiners_are_manual() {
    let bytes = mark_attach_subtable(2, &anchor1(0, 0), 1, &anchor1(100, 0));
    let subs = [AttachSubtable::parse(gpos_lt::MARK_TO_BASE, &bytes).unwrap()];
    let gdef_raw = gdef_bytes(&[2], &[]);
    let gdef = Gdef::parse(&gdef_raw).unwrap();
    let zwnj = ignorable(unicode_prop::NON_JOINER);
    let zwj = ignorable(unicode_prop::JOINER);
    // `mark` and `mkmk` lookups register their joiners as manual, so
    // they see a ZWJ (no attachment across it); ZWNJ is always
    // passed over.
    for (between, ignore_zwj, expect) in [
        (zwnj, false, mark_slot(-2)),
        (zwj, true, mark_slot(-2)),
        (zwj, false, Slot::default()),
    ] {
        let mut glyphs = vec![glyph(1, 600), between, glyph(2, 0)];
        let slots = run_lookup_zwj(
            &subs,
            &mut glyphs,
            Some(&gdef),
            0,
            Direction::Ltr,
            &VarCtx::none(),
            ignore_zwj,
        );
        assert_eq!(slots[2], expect, "ignore_zwj {ignore_zwj}");
    }
}

#[test]
fn mark_base_prefers_the_first_glyph_of_a_multiple_substitution() {
    // Base 1 expanded by a multiple substitution into 1, 6: a mark
    // after the expansion attaches to the first output, the second
    // one being turned down (HarfBuzz issue 740) as long as the
    // subtable does not cover it.
    let bytes = mark_attach_subtable(2, &anchor1(0, 0), 1, &anchor1(100, 0));
    let subs = [AttachSubtable::parse(gpos_lt::MARK_TO_BASE, &bytes).unwrap()];
    let gdef_raw = gdef_bytes(&[2], &[]);
    let gdef = Gdef::parse(&gdef_raw).unwrap();
    let mut glyphs = vec![glyph(1, 600), glyph(6, 300), glyph(2, 0)];
    lig::record_multiple(&mut glyphs, 0, 2);
    let slots = run_lookup(
        &subs,
        &mut glyphs,
        Some(&gdef),
        0,
        Direction::Ltr,
        &VarCtx::none(),
    );
    assert_eq!(slots[2], mark_slot(-2));
}

#[test]
fn mark_to_mark_stacks_only_marks_of_one_component() {
    let mkmk = mark_attach_subtable(3, &anchor1(0, 0), 2, &anchor1(10, 300));
    let subs = [AttachSubtable::parse(gpos_lt::MARK_TO_MARK, &mkmk).unwrap()];
    let gdef_raw = gdef_bytes(&[2, 3], &[1]);
    let gdef = Gdef::parse(&gdef_raw).unwrap();
    // 4 m2 5 m3 with 4 and 5 ligated: m2 sits on component 1 and m3
    // on component 2, so m3 does not stack on m2.
    let mut glyphs = vec![glyph(4, 0), glyph(2, 0), glyph(5, 0), glyph(3, 0)];
    lig::ligate(&mut glyphs, 0, &[0, 2], 1, Some(&gdef), swap_id, MC);
    let slots = run_lookup(
        &subs,
        &mut glyphs,
        Some(&gdef),
        0,
        Direction::Ltr,
        &VarCtx::none(),
    );
    assert_eq!(slots[2], Slot::default(), "different components");

    // Two marks on one base stack, even when the lookup ignores base
    // glyphs: mark-to-mark drops the ignore flags for its search.
    let mut glyphs = vec![glyph(4, 0), glyph(2, 0), glyph(3, 0)];
    let slots = run_lookup(
        &subs,
        &mut glyphs,
        Some(&gdef),
        LOOKUP_FLAG_IGNORE_BASE_GLYPHS,
        Direction::Ltr,
        &VarCtx::none(),
    );
    assert_eq!(slots[2], mark_slot(-1));

    // A base between the two marks stops the backward search.
    let mut glyphs = vec![glyph(2, 0), glyph(4, 0), glyph(3, 0)];
    let slots = run_lookup(
        &subs,
        &mut glyphs,
        Some(&gdef),
        0,
        Direction::Ltr,
        &VarCtx::none(),
    );
    assert_eq!(slots[2], Slot::default());
}

#[test]
fn cursive_joins_across_a_zwnj_the_iterator_passes_over() {
    // A ZWNJ glyph between two joining glyphs does not stop the
    // lookup from finding the previous one.
    let bytes = simple_cursive();
    let subs = [AttachSubtable::parse(gpos_lt::CURSIVE_ATTACHMENT, &bytes).unwrap()];
    let mut glyphs = vec![
        glyph(1, 600),
        ignorable(unicode_prop::NON_JOINER),
        glyph(2, 500),
    ];
    let slots = run_lookup(&subs, &mut glyphs, None, 0, Direction::Ltr, &VarCtx::none());
    assert_eq!(slots[2], cursive_slot(-2));
}
