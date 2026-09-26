//! Tests for the lookup-flag filter, the skipping rules, and sequence
//! matching.

use alloc::vec::Vec;

use super::*;
use crate::tables::layout::SequenceLookupRecord;

/// A GDEF v1.0 whose GlyphClassDef (format 2, one range per pair)
/// gives each listed glyph its class (1 base, 2 ligature, 3 mark).
fn make_gdef(classes: &[(u16, u16)]) -> Vec<u8> {
    let mut cd = Vec::new();
    cd.extend_from_slice(&2u16.to_be_bytes());
    cd.extend_from_slice(&(classes.len() as u16).to_be_bytes());
    for (gid, cls) in classes {
        cd.extend_from_slice(&gid.to_be_bytes());
        cd.extend_from_slice(&gid.to_be_bytes());
        cd.extend_from_slice(&cls.to_be_bytes());
    }
    let mut gdef = Vec::new();
    gdef.extend_from_slice(&1u16.to_be_bytes()); // major
    gdef.extend_from_slice(&0u16.to_be_bytes()); // minor
    gdef.extend_from_slice(&12u16.to_be_bytes()); // glyphClassDefOff
    gdef.extend_from_slice(&[0u8; 6]);
    gdef.extend_from_slice(&cd);
    gdef
}

/// A GDEF v1.2 with a GlyphClassDef (10 base, 11 and 12 marks), a
/// MarkAttachClassDef (11 class 1, 12 class 2) and one mark glyph set
/// holding glyph 11.
fn make_gdef_v12() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&1u16.to_be_bytes());
    b.extend_from_slice(&2u16.to_be_bytes());
    b.extend_from_slice(&[0u8; 10]);
    let class_def = |b: &mut Vec<u8>, start: u16, classes: &[u16]| {
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&start.to_be_bytes());
        b.extend_from_slice(&(classes.len() as u16).to_be_bytes());
        for c in classes {
            b.extend_from_slice(&c.to_be_bytes());
        }
    };
    let gc = b.len() as u16;
    b[4..6].copy_from_slice(&gc.to_be_bytes());
    class_def(&mut b, 10, &[1, 3, 3]);
    let mac = b.len() as u16;
    b[10..12].copy_from_slice(&mac.to_be_bytes());
    class_def(&mut b, 11, &[1, 2]);
    let mgs = b.len();
    b[12..14].copy_from_slice(&(mgs as u16).to_be_bytes());
    b.extend_from_slice(&1u16.to_be_bytes()); // format
    b.extend_from_slice(&1u16.to_be_bytes()); // count
    b.extend_from_slice(&8u32.to_be_bytes()); // coverage, from the set table
    b.extend_from_slice(&[0, 1, 0, 1, 0, 11]); // coverage format 1 {11}
    b
}

fn plain(ids: &[u16]) -> Vec<MatchGlyph> {
    ids.iter().map(|&id| MatchGlyph::new(id)).collect()
}

fn ignorable(id: u16, extra: u16) -> MatchGlyph {
    MatchGlyph::with_props(id, match_prop::DEFAULT_IGNORABLE | extra)
}

fn gsub(filter: MatchFilter<'_>) -> MatchContext<'_> {
    MatchContext::new(filter, LayoutTable::Gsub, Joiners::AUTO)
}

fn walk(rules: &SkipRules<'_>, glyphs: &[MatchGlyph]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = rules.next_any(glyphs, from) {
        out.push(i);
        from = i + 1;
    }
    out
}

#[test]
fn pass_through_filter_accepts_every_glyph() {
    let rules = MatchContext::plain().input();
    assert_eq!(walk(&rules, &plain(&[10, 11, 12, 13])), [0, 1, 2, 3]);
}

#[test]
fn ignore_flags_follow_gdef_classes() {
    // 10 base, 11 mark, 12 ligature, 13 unclassified.
    let bytes = make_gdef(&[(10, 1), (11, 3), (12, 2)]);
    let gdef = Gdef::parse(&bytes).unwrap();
    let run = plain(&[10, 11, 12, 13]);
    let skipped = |flag| {
        let f = MatchFilter::for_lookup(flag, Some(&gdef), None);
        run.iter().map(|&g| f.is_skipped(g)).collect::<Vec<_>>()
    };
    assert_eq!(
        skipped(LOOKUP_FLAG_IGNORE_MARKS),
        [false, true, false, false]
    );
    assert_eq!(
        skipped(LOOKUP_FLAG_IGNORE_LIGATURES),
        [false, false, true, false]
    );
    // An unclassified glyph is not a base glyph: HarfBuzz gives it no
    // class bit, so IgnoreBaseGlyphs keeps it.
    assert_eq!(
        skipped(LOOKUP_FLAG_IGNORE_BASE_GLYPHS),
        [true, false, false, false]
    );
}

#[test]
fn without_a_glyph_class_def_the_synthesized_classes_apply() {
    let mark = MatchGlyph::with_props(11, match_prop::SYNTHESIZED_MARK);
    let lig = MatchGlyph::with_props(12, match_prop::SYNTHESIZED_LIGATURE);
    let base = MatchGlyph::new(10);
    let f = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_MARKS, None, None);
    assert!(f.is_skipped(mark));
    assert!(!f.is_skipped(base) && !f.is_skipped(lig));
    let f = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_BASE_GLYPHS, None, None);
    assert!(f.is_skipped(base) && !f.is_skipped(mark));
    // A GDEF without a GlyphClassDef counts as none.
    let empty = [0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let gdef = Gdef::parse(&empty).unwrap();
    let f = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_MARKS, Some(&gdef), None);
    assert!(f.is_skipped(mark) && !f.is_skipped(base));
    // With a GlyphClassDef the synthesized bits are ignored.
    let bytes = make_gdef(&[(11, 1)]);
    let gdef = Gdef::parse(&bytes).unwrap();
    let f = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_MARKS, Some(&gdef), None);
    assert!(!f.is_skipped(mark));
}

#[test]
fn mark_attachment_type_and_filtering_set() {
    let bytes = make_gdef_v12();
    let gdef = Gdef::parse(&bytes).unwrap();
    let (base, m1, m2) = (
        MatchGlyph::new(10),
        MatchGlyph::new(11),
        MatchGlyph::new(12),
    );
    let f = MatchFilter::for_lookup(0x0100, Some(&gdef), None);
    assert!(!f.is_skipped(base) && !f.is_skipped(m1) && f.is_skipped(m2));
    let f = MatchFilter::for_lookup(LOOKUP_FLAG_USE_MARK_FILTERING_SET, Some(&gdef), Some(0));
    assert!(!f.is_skipped(base) && !f.is_skipped(m1) && f.is_skipped(m2));
    // The filtering set wins over the attachment type (HarfBuzz's
    // match_properties_mark): type 2 would keep only glyph 12.
    let flag = LOOKUP_FLAG_USE_MARK_FILTERING_SET | 0x0200;
    let f = MatchFilter::for_lookup(flag, Some(&gdef), Some(0));
    assert!(!f.is_skipped(m1) && f.is_skipped(m2));
    // A synthesized mark has attachment class 0.
    let f = MatchFilter::for_lookup(0x0100, None, None);
    assert!(f.is_skipped(MatchGlyph::with_props(11, match_prop::SYNTHESIZED_MARK)));
    // A missing set skips every mark.
    let f = MatchFilter::for_lookup(LOOKUP_FLAG_USE_MARK_FILTERING_SET, None, Some(0));
    assert!(f.is_skipped(MatchGlyph::with_props(11, match_prop::SYNTHESIZED_MARK)));
}

#[test]
fn may_skip_follows_the_joiner_and_table_rules() {
    let zwj = ignorable(1, match_prop::ZWJ);
    let zwnj = ignorable(2, match_prop::ZWNJ);
    let hidden = ignorable(3, match_prop::HIDDEN);
    let shy = ignorable(4, 0);
    let verdicts = |rules: SkipRules<'_>| {
        [zwj, zwnj, hidden, shy, MatchGlyph::new(5)].map(|g| rules.may_skip(g))
    };
    let (no, maybe) = (MaySkip::No, MaySkip::Maybe);
    let cx = |table, joiners| MatchContext::new(MatchFilter::none(), table, joiners);
    let gsub_auto = cx(LayoutTable::Gsub, Joiners::AUTO);
    assert_eq!(verdicts(gsub_auto.input()), [maybe, no, no, maybe, no]);
    assert_eq!(verdicts(gsub_auto.context()), [maybe, maybe, no, maybe, no]);
    let gsub_manual = cx(LayoutTable::Gsub, Joiners::MANUAL);
    assert_eq!(verdicts(gsub_manual.input()), [no, no, no, maybe, no]);
    assert_eq!(verdicts(gsub_manual.context()), [maybe, no, no, maybe, no]);
    let gpos_manual = cx(LayoutTable::Gpos, Joiners::MANUAL);
    assert_eq!(verdicts(gpos_manual.input()), [no, maybe, maybe, maybe, no]);
    assert_eq!(
        verdicts(gpos_manual.context()),
        [maybe, maybe, maybe, maybe, no]
    );
}

#[test]
fn walks_stop_at_a_mismatch_and_match_an_ignorable_the_rule_names() {
    let rules = MatchContext::plain().input();
    let run = [MatchGlyph::new(1), ignorable(9, 0), MatchGlyph::new(2)];
    // The ignorable does not match 2, so it is skipped.
    assert_eq!(rules.next(&run, 1, |g| Some(g == 2)), Some(2));
    // It matches 9, so the walk stops there.
    assert_eq!(rules.next(&run, 1, |g| Some(g == 9)), Some(1));
    // A plain glyph that does not match ends the walk.
    assert_eq!(rules.next(&run, 0, |g| Some(g == 2)), None);
    assert_eq!(rules.prev(&run, 3, |g| Some(g == 1)), None);
    assert_eq!(rules.prev_any(&run, 2), Some(0));
}

#[test]
fn match_input_reports_positions_and_end() {
    let bytes = make_gdef(&[(10, 1), (11, 3), (12, 1)]);
    let gdef = Gdef::parse(&bytes).unwrap();
    let cx = gsub(MatchFilter::for_lookup(
        LOOKUP_FLAG_IGNORE_MARKS,
        Some(&gdef),
        None,
    ));
    let run = plain(&[10, 11, 12, 13]);
    let m = match_input(&run, 0, 2, &cx, |k, g| g == [12, 13][k]).unwrap();
    assert_eq!(m.positions.as_slice(), [0, 2, 3]);
    assert_eq!(m.end, 4);
    assert!(match_input(&run, 0, 2, &cx, |k, g| g == [12, 14][k]).is_none());
    assert!(match_lookahead(&run, 1, 1, &cx, |_, g| g == 12));
    assert!(match_backtrack(&run, 2, 1, &cx, |_, g| g == 10));
}

#[test]
fn match_input_keeps_ligature_components_apart() {
    // A mark left inside ligature 1, on component `c`.
    let at = |c: u16| MatchGlyph::with_props(21, ((1 << 5) | c) << 8);
    let run = [at(1), at(2)];
    let cx = MatchContext::plain();
    // Marks on different components of one ligature do not match.
    assert!(match_input(&run, 0, 1, &cx, |_, _| true).is_none());
    // Marks on the same component do.
    let run = [at(1), at(1)];
    assert!(match_input(&run, 0, 1, &cx, |_, _| true).is_some());
    // A free glyph may not match a glyph attached to a ligature.
    let run = [MatchGlyph::new(5), at(1)];
    assert!(match_input(&run, 0, 1, &cx, |_, _| true).is_none());
    // Unless the ligature itself is one the lookup ignores (HarfBuzz
    // issue 545): here a two-component ligature skipped by
    // IgnoreLigatures, then marks on its two components.
    let lig_props = (1 << 5) | u16::from(match_prop::IS_LIG_BASE) | 2;
    let lig = MatchGlyph::with_props(30, match_prop::SYNTHESIZED_LIGATURE | (lig_props << 8));
    let run = [lig, at(1), at(2)];
    let ignore_ligatures = gsub(MatchFilter::for_lookup(
        LOOKUP_FLAG_IGNORE_LIGATURES,
        None,
        None,
    ));
    assert!(match_input(&run, 1, 1, &ignore_ligatures, |_, _| true).is_some());
    assert!(match_input(&run, 1, 1, &cx, |_, _| true).is_none());
}

#[test]
fn nested_lookups_track_length_changes() {
    let records = |seq: &[u16]| {
        seq.iter()
            .map(|&s| SequenceLookupRecord {
                sequence_index: s,
                lookup_list_index: s,
            })
            .collect::<Vec<_>>()
    };
    // Input at 0, 2, 4; the first nested lookup deletes one glyph
    // (a two-glyph ligature at 0), the second runs at the old third
    // input glyph, now at 3.
    let mut positions = MatchPositions::new(0);
    positions.push(2);
    positions.push(4);
    let mut seen = Vec::new();
    let end = apply_nested(&mut positions, 5, 6, &records(&[0, 1]), |lookup, at| {
        seen.push((lookup, at));
        Some(if lookup == 0 { -1 } else { 0 })
    });
    assert_eq!(seen, [(0, 0), (1, 3)]);
    assert_eq!(end, 4);
    // Growth: a multiple substitution at the first input adds two
    // glyphs after it, and later positions move along.
    let mut positions = MatchPositions::new(0);
    positions.push(1);
    let end = apply_nested(&mut positions, 2, 2, &records(&[0]), |_, _| Some(2));
    assert_eq!(positions.as_slice(), [0, 1, 2, 3]);
    assert_eq!(end, 4);
}
