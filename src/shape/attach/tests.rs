//! Unit tests for GPOS attachment: the resolve pass on hand-built
//! glyph runs, and the mark / cursive lookups on hand-built subtable
//! fixtures (anchors, GDEF classes, a small ItemVariationStore).

use super::*;
use crate::tables::variation_store::ItemVariationStore;
use alloc::vec;

// ----------------------------------------------------------------
// Fixture builders
// ----------------------------------------------------------------

fn be16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn coverage(glyphs: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    be16(&mut out, 1);
    be16(&mut out, glyphs.len() as u16);
    for &g in glyphs {
        be16(&mut out, g);
    }
    out
}

fn anchor1(x: i16, y: i16) -> Vec<u8> {
    let mut out = Vec::new();
    be16(&mut out, 1);
    be16(&mut out, x as u16);
    be16(&mut out, y as u16);
    out
}

/// AnchorFormat3 whose x slot is a VariationIndex into IVS row
/// `x_row` (outer 0); the y slot stays null.
fn anchor3_x_var(x: i16, y: i16, x_row: u16) -> Vec<u8> {
    let mut out = Vec::new();
    be16(&mut out, 3);
    be16(&mut out, x as u16);
    be16(&mut out, y as u16);
    be16(&mut out, 10); // xDeviceOffset, relative to the anchor
    be16(&mut out, 0);
    be16(&mut out, 0); // outer
    be16(&mut out, x_row); // inner
    be16(&mut out, 0x8000); // VariationIndex
    out
}

/// Mark-to-base / mark-to-mark subtable (the two share a wire
/// layout) with one mark class: `mark` attaches to `base`.
fn mark_attach_subtable(mark: u16, mark_anchor: &[u8], base: u16, base_anchor: &[u8]) -> Vec<u8> {
    let mark_cov = coverage(&[mark]);
    let base_cov = coverage(&[base]);
    let mark_cov_off = 12;
    let base_cov_off = mark_cov_off + mark_cov.len();
    let mark_array_off = base_cov_off + base_cov.len();
    let mut mark_array = Vec::new();
    be16(&mut mark_array, 1); // markCount
    be16(&mut mark_array, 0); // class 0
    be16(&mut mark_array, 6); // anchor right after the record
    mark_array.extend_from_slice(mark_anchor);
    let base_array_off = mark_array_off + mark_array.len();
    let mut base_array = Vec::new();
    be16(&mut base_array, 1); // baseCount
    be16(&mut base_array, 4); // class-0 anchor right after
    base_array.extend_from_slice(base_anchor);

    let mut out = Vec::new();
    be16(&mut out, 1);
    be16(&mut out, mark_cov_off as u16);
    be16(&mut out, base_cov_off as u16);
    be16(&mut out, 1); // markClassCount
    be16(&mut out, mark_array_off as u16);
    be16(&mut out, base_array_off as u16);
    out.extend_from_slice(&mark_cov);
    out.extend_from_slice(&base_cov);
    out.extend_from_slice(&mark_array);
    out.extend_from_slice(&base_array);
    out
}

/// Mark-to-ligature subtable with one mark class and a ligature
/// whose components carry `component_anchors` (x only, y = 0).
fn mark_liga_subtable(mark: u16, lig: u16, component_anchors: &[i16]) -> Vec<u8> {
    let mark_cov = coverage(&[mark]);
    let lig_cov = coverage(&[lig]);
    let mark_cov_off = 12;
    let lig_cov_off = mark_cov_off + mark_cov.len();
    let mark_array_off = lig_cov_off + lig_cov.len();
    let mut mark_array = Vec::new();
    be16(&mut mark_array, 1);
    be16(&mut mark_array, 0);
    be16(&mut mark_array, 6);
    mark_array.extend_from_slice(&anchor1(0, 0));
    let lig_array_off = mark_array_off + mark_array.len();
    // LigatureArray: count 1, offset 4 to the LigatureAttach.
    let mut lig_array = Vec::new();
    be16(&mut lig_array, 1);
    be16(&mut lig_array, 4);
    // LigatureAttach: componentCount + one anchor offset per
    // component, anchors after the records.
    let n = component_anchors.len();
    let records_len = 2 + n * 2;
    be16(&mut lig_array, n as u16);
    for c in 0..n {
        be16(&mut lig_array, (records_len + c * 6) as u16);
    }
    for &x in component_anchors {
        lig_array.extend_from_slice(&anchor1(x, 0));
    }
    let mut out = Vec::new();
    be16(&mut out, 1);
    be16(&mut out, mark_cov_off as u16);
    be16(&mut out, lig_cov_off as u16);
    be16(&mut out, 1);
    be16(&mut out, mark_array_off as u16);
    be16(&mut out, lig_array_off as u16);
    out.extend_from_slice(&mark_cov);
    out.extend_from_slice(&lig_cov);
    out.extend_from_slice(&mark_array);
    out.extend_from_slice(&lig_array);
    out
}

/// An optional `(x, y)` anchor point; `None` is a null offset.
type Point = Option<(i16, i16)>;

/// Cursive subtable; `records` pairs `(glyph, entry, exit)`, glyphs
/// sorted ascending.
fn cursive_subtable(records: &[(u16, Point, Point)]) -> Vec<u8> {
    let glyphs: Vec<u16> = records.iter().map(|r| r.0).collect();
    let cov = coverage(&glyphs);
    let header_len = 6 + records.len() * 4;
    let anchors_base = header_len + cov.len();
    let mut anchors = Vec::new();
    let mut offsets = Vec::new();
    for &(_, entry, exit) in records {
        let mut place = |a: Option<(i16, i16)>| match a {
            None => 0u16,
            Some((x, y)) => {
                let off = (anchors_base + anchors.len()) as u16;
                anchors.extend_from_slice(&anchor1(x, y));
                off
            }
        };
        let e = place(entry);
        let x = place(exit);
        offsets.push((e, x));
    }
    let mut out = Vec::new();
    be16(&mut out, 1);
    be16(&mut out, header_len as u16);
    be16(&mut out, records.len() as u16);
    for (e, x) in offsets {
        be16(&mut out, e);
        be16(&mut out, x);
    }
    out.extend_from_slice(&cov);
    out.extend_from_slice(&anchors);
    out
}

/// GDEF 1.0 whose glyph class def marks `marks` as class 3 (mark)
/// and `ligatures` as class 2; everything else is a base.
fn gdef_bytes(marks: &[u16], ligatures: &[u16]) -> Vec<u8> {
    let mut ranges: Vec<(u16, u16)> = marks.iter().map(|&m| (m, 3)).collect();
    ranges.extend(ligatures.iter().map(|&l| (l, 2)));
    ranges.sort_unstable();
    let mut out = Vec::new();
    be16(&mut out, 1);
    be16(&mut out, 0);
    be16(&mut out, 12); // glyphClassDef
    be16(&mut out, 0);
    be16(&mut out, 0);
    be16(&mut out, 0);
    be16(&mut out, 2); // ClassDef format 2
    be16(&mut out, ranges.len() as u16);
    for (g, class) in ranges {
        be16(&mut out, g);
        be16(&mut out, g);
        be16(&mut out, class);
    }
    out
}

/// Single-axis ItemVariationStore with one region peaking at +1.0
/// and one i16 delta per row.
fn ivs_bytes(rows: &[i16]) -> Vec<u8> {
    let mut out = Vec::new();
    be16(&mut out, 1);
    out.extend_from_slice(&12u32.to_be_bytes()); // regionList
    be16(&mut out, 1);
    out.extend_from_slice(&22u32.to_be_bytes()); // data 0
    be16(&mut out, 1); // axisCount
    be16(&mut out, 1); // regionCount
    be16(&mut out, 0);
    be16(&mut out, 0x4000);
    be16(&mut out, 0x4000);
    be16(&mut out, rows.len() as u16);
    be16(&mut out, 1); // wordDeltaCount
    be16(&mut out, 1); // regionIndexCount
    be16(&mut out, 0);
    for &r in rows {
        be16(&mut out, r as u16);
    }
    out
}

fn glyph(gid: u32, x_advance: i32) -> Glyph {
    let mut g = Glyph::new(gid, 0);
    g.x_advance = x_advance;
    g
}

fn offsets(glyphs: &[Glyph]) -> Vec<(i32, i32)> {
    glyphs.iter().map(|g| (g.x_offset, g.y_offset)).collect()
}

fn run_lookup(
    subs: &[AttachSubtable<'_>],
    glyphs: &mut [Glyph],
    gdef: Option<&Gdef<'_>>,
    lookup_flag: u16,
    direction: Direction,
    var: &VarCtx<'_>,
) -> Vec<Slot> {
    run_lookup_zwj(subs, glyphs, gdef, lookup_flag, direction, var, true)
}

/// [`run_lookup`] with the lookup's `auto_zwj` setting spelled out.
fn run_lookup_zwj(
    subs: &[AttachSubtable<'_>],
    glyphs: &mut [Glyph],
    gdef: Option<&Gdef<'_>>,
    lookup_flag: u16,
    direction: Direction,
    var: &VarCtx<'_>,
    ignore_zwj: bool,
) -> Vec<Slot> {
    let mut slots = new_slots(glyphs.len());
    let filter = MatchFilter::for_lookup(lookup_flag, gdef, None);
    let cx = LookupCx {
        gdef,
        filter: &filter,
        lookup_flag,
        mark_filtering_set: None,
        ignore_zwj,
        var,
        lookup_index: 0,
    };
    let mut att = Attach::new(direction, &mut slots);
    apply_lookup(subs, glyphs, &mut att, &cx);
    slots
}

fn mark_slot(chain: i32) -> Slot {
    Slot {
        kind: AttachKind::Mark,
        chain,
    }
}

fn cursive_slot(chain: i32) -> Slot {
    Slot {
        kind: AttachKind::Cursive,
        chain,
    }
}

// ----------------------------------------------------------------
// resolve_attachments
// ----------------------------------------------------------------

#[test]
fn forward_mark_subtracts_advances_from_parent_to_mark() {
    // base (600) + mark carrying the raw anchor delta (200, 300).
    let mut glyphs = vec![glyph(1, 600), glyph(2, 0)];
    glyphs[1].x_offset = 200;
    glyphs[1].y_offset = 300;
    let mut slots = vec![Slot::default(), mark_slot(-1)];
    resolve_attachments(&mut glyphs, &mut slots, Direction::Ltr);
    assert_eq!(offsets(&glyphs), vec![(0, 0), (-400, 300)]);
}

#[test]
fn backward_mark_adds_advances_after_parent_through_mark() {
    // Same logical run, RTL: the run is reversed later, so the mark
    // ends up before its base and only its own advance counts.
    let mut glyphs = vec![glyph(1, 600), glyph(2, 0)];
    glyphs[1].x_offset = 200;
    glyphs[1].y_offset = 300;
    let mut slots = vec![Slot::default(), mark_slot(-1)];
    resolve_attachments(&mut glyphs, &mut slots, Direction::Rtl);
    assert_eq!(offsets(&glyphs), vec![(0, 0), (200, 300)]);

    // A mark that keeps a non-zero advance shifts by it.
    let mut glyphs = vec![glyph(1, 600), glyph(2, 50)];
    glyphs[1].x_offset = 200;
    let mut slots = vec![Slot::default(), mark_slot(-1)];
    resolve_attachments(&mut glyphs, &mut slots, Direction::Rtl);
    assert_eq!(glyphs[1].x_offset, 250);
}

#[test]
fn intermediate_glyph_advances_are_compensated_both_ways() {
    // base(600) mark1(40) mark2 -> base.
    let mut forward = vec![glyph(1, 600), glyph(2, 40), glyph(3, 10)];
    forward[2].x_offset = 100;
    let mut slots = vec![Slot::default(), Slot::default(), mark_slot(-2)];
    resolve_attachments(&mut forward, &mut slots, Direction::Ltr);
    assert_eq!(forward[2].x_offset, 100 - 600 - 40);

    let mut backward = vec![glyph(1, 600), glyph(2, 40), glyph(3, 10)];
    backward[2].x_offset = 100;
    let mut slots = vec![Slot::default(), Slot::default(), mark_slot(-2)];
    resolve_attachments(&mut backward, &mut slots, Direction::Rtl);
    assert_eq!(backward[2].x_offset, 100 + 40 + 10);
}

#[test]
fn mark_follows_a_displaced_parent() {
    // A kerning placement on the base moves its mark too.
    for (dir, expect) in [
        (Direction::Ltr, -20 + 100 - 500),
        (Direction::Rtl, -20 + 100),
    ] {
        let mut glyphs = vec![glyph(1, 500), glyph(2, 0)];
        glyphs[0].x_offset = -20;
        glyphs[0].y_offset = 7;
        glyphs[1].x_offset = 100;
        let mut slots = vec![Slot::default(), mark_slot(-1)];
        resolve_attachments(&mut glyphs, &mut slots, dir);
        assert_eq!(glyphs[1].x_offset, expect, "{dir:?}");
        assert_eq!(glyphs[1].y_offset, 7, "{dir:?}");
    }
}

#[test]
fn stacked_marks_resolve_parents_first_in_any_order() {
    // base <- m1 <- m2. m2's resolved offset includes m1's resolved
    // offset, whichever order the chain is discovered in.
    let build = || {
        let mut g = vec![glyph(1, 500), glyph(2, 0), glyph(3, 0)];
        g[1].x_offset = 100;
        g[1].y_offset = 400;
        g[2].x_offset = 5;
        g[2].y_offset = 200;
        g
    };
    let mut glyphs = build();
    let mut slots = vec![Slot::default(), mark_slot(-1), mark_slot(-1)];
    resolve_attachments(&mut glyphs, &mut slots, Direction::Ltr);
    assert_eq!(offsets(&glyphs), vec![(0, 0), (-400, 400), (-395, 600)]);

    let mut glyphs = build();
    let mut slots = vec![Slot::default(), mark_slot(-1), mark_slot(-1)];
    resolve_attachments(&mut glyphs, &mut slots, Direction::Rtl);
    assert_eq!(offsets(&glyphs), vec![(0, 0), (100, 400), (105, 600)]);
    assert!(slots.iter().all(|s| s.chain == 0), "every link consumed");
}

#[test]
fn cursive_links_only_carry_the_cross_stream_axis() {
    let mut glyphs = vec![glyph(1, 500), glyph(2, 500)];
    glyphs[1].x_offset = 33;
    glyphs[1].y_offset = 120;
    glyphs[0].x_offset = 11;
    glyphs[0].y_offset = 40;
    let mut slots = vec![cursive_slot(1), Slot::default()];
    resolve_attachments(&mut glyphs, &mut slots, Direction::Rtl);
    assert_eq!(glyphs[0].x_offset, 11, "main axis untouched");
    assert_eq!(glyphs[0].y_offset, 160);

    let mut glyphs = vec![glyph(1, 0), glyph(2, 0)];
    glyphs[1].x_offset = 33;
    glyphs[1].y_offset = 120;
    let mut slots = vec![cursive_slot(1), Slot::default()];
    resolve_attachments(&mut glyphs, &mut slots, Direction::Ttb);
    assert_eq!(offsets(&glyphs)[0], (33, 0), "vertical: x follows");
}

#[test]
fn vertical_marks_use_y_advances() {
    // TTB advances are negative (pen moves down).
    let mut glyphs = vec![Glyph::new(1, 0), Glyph::new(2, 0)];
    glyphs[0].y_advance = -1000;
    glyphs[1].y_offset = 50;
    let mut slots = vec![Slot::default(), mark_slot(-1)];
    resolve_attachments(&mut glyphs, &mut slots, Direction::Ttb);
    assert_eq!(glyphs[1].y_offset, 1050);

    let mut glyphs = vec![Glyph::new(1, 0), Glyph::new(2, 0)];
    glyphs[1].y_advance = -30;
    glyphs[1].y_offset = 50;
    let mut slots = vec![Slot::default(), mark_slot(-1)];
    resolve_attachments(&mut glyphs, &mut slots, Direction::Btt);
    assert_eq!(glyphs[1].y_offset, 20);
}

#[test]
fn long_cursive_chains_do_not_recurse() {
    // 20k glyphs, each hanging from the next: an iterative walk
    // handles it and accumulates the whole chain.
    let n = 20_000;
    let mut glyphs: Vec<Glyph> = (0..n).map(|i| glyph(i, 100)).collect();
    let mut slots = new_slots(n as usize);
    for i in 0..(n as usize - 1) {
        glyphs[i].y_offset = 1;
        slots[i] = cursive_slot(1);
    }
    resolve_attachments(&mut glyphs, &mut slots, Direction::Rtl);
    assert_eq!(glyphs[0].y_offset, n as i32 - 1);
    assert_eq!(glyphs[n as usize - 1].y_offset, 0);
}

#[test]
fn cycles_and_out_of_range_links_terminate() {
    let mut glyphs = vec![glyph(1, 100), glyph(2, 100), glyph(3, 100)];
    glyphs[0].y_offset = 5;
    glyphs[1].y_offset = 7;
    let mut slots = vec![cursive_slot(1), cursive_slot(-1), mark_slot(40)];
    resolve_attachments(&mut glyphs, &mut slots, Direction::Ltr);
    // 0 -> 1 -> 0: the walk from 0 consumes both links; 1 is folded
    // from the still-raw 0, then 0 from the updated 1.
    assert_eq!(glyphs[1].y_offset, 12);
    assert_eq!(glyphs[0].y_offset, 17);
    // Index 2 points past the end: dropped, offset unchanged.
    assert_eq!(offsets(&glyphs)[2], (0, 0));
    assert!(slots.iter().all(|s| s.chain == 0));
}

// ----------------------------------------------------------------
// Mark lookups
// ----------------------------------------------------------------

#[test]
fn mark_base_records_raw_delta_and_link() {
    let bytes = mark_attach_subtable(2, &anchor1(50, 0), 1, &anchor1(300, 500));
    let subs = [AttachSubtable::parse(gpos_lt::MARK_TO_BASE, &bytes).unwrap()];
    let gdef_raw = gdef_bytes(&[2], &[]);
    let gdef = Gdef::parse(&gdef_raw).unwrap();
    // The mark has a (non-zero) hmtx advance of 30.
    let mut glyphs = vec![glyph(1, 600), glyph(2, 30)];
    // A placement from an earlier lookup is replaced, not added to.
    glyphs[1].x_offset = 999;
    let mut slots = run_lookup(
        &subs,
        &mut glyphs,
        Some(&gdef),
        0,
        Direction::Ltr,
        &VarCtx::none(),
    );
    assert_eq!(offsets(&glyphs)[1], (250, 500));
    assert_eq!(slots[1], mark_slot(-1));
    // The mark keeps its own advance; zeroing is the shaper's
    // mark-width pass, which some scripts skip (as in HarfBuzz).
    assert_eq!(glyphs[1].x_advance, 30);

    resolve_attachments(&mut glyphs, &mut slots, Direction::Ltr);
    assert_eq!(offsets(&glyphs)[1], (250 - 600, 500));
}

#[test]
fn mark_base_skips_intervening_marks_to_find_the_base() {
    let bytes = mark_attach_subtable(3, &anchor1(0, 0), 1, &anchor1(100, 0));
    let subs = [AttachSubtable::parse(gpos_lt::MARK_TO_BASE, &bytes).unwrap()];
    let gdef_raw = gdef_bytes(&[2, 3], &[]);
    let gdef = Gdef::parse(&gdef_raw).unwrap();
    let mut glyphs = vec![glyph(1, 600), glyph(2, 0), glyph(3, 0)];
    let slots = run_lookup(
        &subs,
        &mut glyphs,
        Some(&gdef),
        0,
        Direction::Ltr,
        &VarCtx::none(),
    );
    assert_eq!(slots[2], mark_slot(-2));
    assert_eq!(slots[1], Slot::default(), "gid 2 is not covered");
}

#[test]
fn first_matching_subtable_wins() {
    let first = mark_attach_subtable(2, &anchor1(0, 0), 1, &anchor1(111, 0));
    let second = mark_attach_subtable(2, &anchor1(0, 0), 1, &anchor1(222, 0));
    let subs = [
        AttachSubtable::parse(gpos_lt::MARK_TO_BASE, &first).unwrap(),
        AttachSubtable::parse(gpos_lt::MARK_TO_BASE, &second).unwrap(),
    ];
    let gdef_raw = gdef_bytes(&[2], &[]);
    let gdef = Gdef::parse(&gdef_raw).unwrap();
    let mut glyphs = vec![glyph(1, 600), glyph(2, 0)];
    run_lookup(
        &subs,
        &mut glyphs,
        Some(&gdef),
        0,
        Direction::Ltr,
        &VarCtx::none(),
    );
    assert_eq!(glyphs[1].x_offset, 111);
}

#[test]
fn mark_lookups_respect_the_lookup_filter() {
    let bytes = mark_attach_subtable(2, &anchor1(0, 0), 1, &anchor1(100, 0));
    let subs = [AttachSubtable::parse(gpos_lt::MARK_TO_BASE, &bytes).unwrap()];
    let mut glyphs = vec![glyph(1, 600), glyph(2, 0)];

    // IgnoreMarks on the lookup skips the mark itself.
    let gdef_raw = gdef_bytes(&[2], &[]);
    let gdef = Gdef::parse(&gdef_raw).unwrap();
    let slots = run_lookup(
        &subs,
        &mut glyphs,
        Some(&gdef),
        crate::tables::layout::LOOKUP_FLAG_IGNORE_MARKS,
        Direction::Ltr,
        &VarCtx::none(),
    );
    assert_eq!(slots[1], Slot::default());
}

#[test]
fn mark_mark_stacks_onto_the_previous_mark() {
    let base = mark_attach_subtable(2, &anchor1(0, 0), 1, &anchor1(100, 500));
    let mkmk = mark_attach_subtable(3, &anchor1(0, 0), 2, &anchor1(10, 300));
    let gdef_raw = gdef_bytes(&[2, 3], &[]);
    let gdef = Gdef::parse(&gdef_raw).unwrap();
    let mut glyphs = vec![glyph(1, 600), glyph(2, 0), glyph(3, 0)];
    let mut slots = new_slots(3);
    let filter = MatchFilter::none();
    let var = VarCtx::none();
    let cx = LookupCx {
        gdef: Some(&gdef),
        filter: &filter,
        lookup_flag: 0,
        mark_filtering_set: None,
        ignore_zwj: true,
        var: &var,
        lookup_index: 0,
    };
    let mut att = Attach::new(Direction::Ltr, &mut slots);
    let mark_subs = [AttachSubtable::parse(gpos_lt::MARK_TO_BASE, &base).unwrap()];
    let mkmk_subs = [AttachSubtable::parse(gpos_lt::MARK_TO_MARK, &mkmk).unwrap()];
    apply_lookup(&mark_subs, &mut glyphs, &mut att, &cx);
    apply_lookup(&mkmk_subs, &mut glyphs, &mut att, &cx);
    assert_eq!(att.slots[2], mark_slot(-1));
    resolve_attachments(&mut glyphs, &mut slots, Direction::Ltr);
    // m1: 100 - 600; m2: 10 + m1 - adv(m1).
    assert_eq!(offsets(&glyphs), vec![(0, 0), (-500, 500), (-490, 800)]);
}

#[test]
fn mark_anchors_follow_variation_deltas() {
    let ivs_raw = ivs_bytes(&[40]);
    let store = ItemVariationStore::parse(&ivs_raw).unwrap();
    let bytes = mark_attach_subtable(2, &anchor1(0, 0), 1, &anchor3_x_var(300, 500, 0));
    let subs = [AttachSubtable::parse(gpos_lt::MARK_TO_BASE, &bytes).unwrap()];
    let gdef_raw = gdef_bytes(&[2], &[]);
    let gdef = Gdef::parse(&gdef_raw).unwrap();
    for (coord, expect) in [(0.0f32, 300), (0.5, 320), (1.0, 340)] {
        let coords = [coord];
        let var = VarCtx {
            coords: &coords,
            store: Some(&store),
        };
        let mut glyphs = vec![glyph(1, 600), glyph(2, 0)];
        run_lookup(&subs, &mut glyphs, Some(&gdef), 0, Direction::Ltr, &var);
        assert_eq!(glyphs[1].x_offset, expect, "coord {coord}");
    }
}

// ----------------------------------------------------------------
// Cursive
// ----------------------------------------------------------------

/// gid 1 exits at (550, 100); gid 2 enters at (50, 300).
fn simple_cursive() -> Vec<u8> {
    cursive_subtable(&[(1, None, Some((550, 100))), (2, Some((50, 300)), None)])
}

#[test]
fn cursive_ltr_moves_the_join_onto_the_exit_point() {
    let bytes = simple_cursive();
    let subs = [AttachSubtable::parse(gpos_lt::CURSIVE_ATTACHMENT, &bytes).unwrap()];
    let mut glyphs = vec![glyph(1, 600), glyph(2, 500)];
    let mut slots = run_lookup(&subs, &mut glyphs, None, 0, Direction::Ltr, &VarCtx::none());
    assert_eq!(glyphs[0].x_advance, 550);
    assert_eq!(glyphs[1].x_advance, 450);
    assert_eq!(glyphs[1].x_offset, -50);
    // No RightToLeft flag: the later glyph is the child.
    assert_eq!(slots[1], cursive_slot(-1));
    assert_eq!(glyphs[1].y_offset, -200);
    resolve_attachments(&mut glyphs, &mut slots, Direction::Ltr);
    assert_eq!(glyphs[1].y_offset, -200);
}

#[test]
fn cursive_rtl_swaps_entry_and_exit_roles() {
    let bytes = simple_cursive();
    let subs = [AttachSubtable::parse(gpos_lt::CURSIVE_ATTACHMENT, &bytes).unwrap()];
    let mut glyphs = vec![glyph(1, 600), glyph(2, 500)];
    let slots = run_lookup(
        &subs,
        &mut glyphs,
        None,
        LOOKUP_FLAG_RIGHT_TO_LEFT,
        Direction::Rtl,
        &VarCtx::none(),
    );
    assert_eq!(glyphs[0].x_advance, 50);
    assert_eq!(glyphs[0].x_offset, -550);
    assert_eq!(glyphs[1].x_advance, 50);
    // RightToLeft flag: the earlier glyph hangs from the later one.
    assert_eq!(slots[0], cursive_slot(1));
    assert_eq!(glyphs[0].y_offset, 200);
}

#[test]
fn cursive_vertical_directions_adjust_y() {
    let bytes = simple_cursive();
    let subs = [AttachSubtable::parse(gpos_lt::CURSIVE_ATTACHMENT, &bytes).unwrap()];
    let mut glyphs = vec![Glyph::new(1, 0), Glyph::new(2, 0)];
    glyphs[1].y_advance = -1000;
    let slots = run_lookup(&subs, &mut glyphs, None, 0, Direction::Ttb, &VarCtx::none());
    assert_eq!(glyphs[0].y_advance, 100);
    assert_eq!(glyphs[1].y_advance, -1300);
    assert_eq!(glyphs[1].y_offset, -300);
    assert_eq!(glyphs[1].x_offset, 500, "cross-stream is x");
    assert_eq!(slots[1], cursive_slot(-1));

    let mut glyphs = vec![Glyph::new(1, 0), Glyph::new(2, 0)];
    glyphs[0].y_advance = -1000;
    run_lookup(&subs, &mut glyphs, None, 0, Direction::Btt, &VarCtx::none());
    assert_eq!(glyphs[0].y_advance, -1100);
    assert_eq!(glyphs[0].y_offset, -100);
    assert_eq!(glyphs[1].y_advance, 300);
}

#[test]
fn cursive_needs_entry_on_current_and_exit_on_previous() {
    // Reverse order: gid 2 then gid 1. gid 1 has no entry.
    let bytes = simple_cursive();
    let subs = [AttachSubtable::parse(gpos_lt::CURSIVE_ATTACHMENT, &bytes).unwrap()];
    let mut glyphs = vec![glyph(2, 500), glyph(1, 600)];
    let slots = run_lookup(&subs, &mut glyphs, None, 0, Direction::Ltr, &VarCtx::none());
    assert!(slots.iter().all(|s| *s == Slot::default()));
    assert_eq!(glyphs[0].x_advance, 500);
    assert_eq!(glyphs[1].x_advance, 600);
}

#[test]
fn cursive_skips_glyphs_the_lookup_ignores() {
    // gid 1, mark gid 9, gid 2 with IgnoreMarks: 1 and 2 still join.
    let bytes = simple_cursive();
    let subs = [AttachSubtable::parse(gpos_lt::CURSIVE_ATTACHMENT, &bytes).unwrap()];
    let gdef_raw = gdef_bytes(&[9], &[]);
    let gdef = Gdef::parse(&gdef_raw).unwrap();
    let mut glyphs = vec![glyph(1, 600), glyph(9, 0), glyph(2, 500)];
    let slots = run_lookup(
        &subs,
        &mut glyphs,
        Some(&gdef),
        crate::tables::layout::LOOKUP_FLAG_IGNORE_MARKS,
        Direction::Ltr,
        &VarCtx::none(),
    );
    assert_eq!(slots[2], cursive_slot(-2));
    assert_eq!(glyphs[0].x_advance, 550);
}

#[test]
fn cursive_chain_accumulates_through_resolve() {
    // Three glyphs joined RTL-style: 0 -> 1 -> 2.
    let bytes = cursive_subtable(&[
        (1, None, Some((0, 10))),
        (2, Some((0, 50)), Some((0, 20))),
        (3, Some((0, 100)), None),
    ]);
    let subs = [AttachSubtable::parse(gpos_lt::CURSIVE_ATTACHMENT, &bytes).unwrap()];
    let mut glyphs = vec![glyph(1, 500), glyph(2, 500), glyph(3, 500)];
    let mut slots = run_lookup(
        &subs,
        &mut glyphs,
        None,
        LOOKUP_FLAG_RIGHT_TO_LEFT,
        Direction::Rtl,
        &VarCtx::none(),
    );
    assert_eq!(slots[0], cursive_slot(1));
    assert_eq!(slots[1], cursive_slot(1));
    resolve_attachments(&mut glyphs, &mut slots, Direction::Rtl);
    // 1: 100 - 20 = 80; 0: (50 - 10) + 80 = 120.
    assert_eq!(glyphs[1].y_offset, 80);
    assert_eq!(glyphs[0].y_offset, 120);
    assert_eq!(glyphs[2].y_offset, 0);
}

#[test]
fn cursive_reattachment_reverses_the_old_chain() {
    let bytes = simple_cursive();
    let ltr_flagless = [AttachSubtable::parse(gpos_lt::CURSIVE_ATTACHMENT, &bytes).unwrap()];
    let rtl_bytes = cursive_subtable(&[(2, None, Some((0, 0))), (3, Some((0, 70)), None)]);
    let rtl_flagged = [AttachSubtable::parse(gpos_lt::CURSIVE_ATTACHMENT, &rtl_bytes).unwrap()];
    let mut glyphs = vec![glyph(1, 600), glyph(2, 500), glyph(3, 500)];
    let mut slots = new_slots(3);
    let filter = MatchFilter::none();
    let var = VarCtx::none();
    let mut att = Attach::new(Direction::Ltr, &mut slots);
    // First lookup (no RightToLeft): 1 hangs from 0.
    let cx = LookupCx {
        gdef: None,
        filter: &filter,
        lookup_flag: 0,
        mark_filtering_set: None,
        ignore_zwj: true,
        var: &var,
        lookup_index: 0,
    };
    apply_lookup(&ltr_flagless, &mut glyphs, &mut att, &cx);
    assert_eq!(att.slots[1], cursive_slot(-1));
    assert_eq!(glyphs[1].y_offset, -200);
    // Second lookup (RightToLeft): 1 now hangs from 2, so the old
    // link flips and 0 hangs from 1 with the negated offset.
    let cx = LookupCx {
        gdef: None,
        filter: &filter,
        lookup_flag: LOOKUP_FLAG_RIGHT_TO_LEFT,
        mark_filtering_set: None,
        ignore_zwj: true,
        var: &var,
        lookup_index: 0,
    };
    apply_lookup(&rtl_flagged, &mut glyphs, &mut att, &cx);
    assert_eq!(att.slots[1], cursive_slot(1));
    assert_eq!(att.slots[0], cursive_slot(1));
    assert_eq!(glyphs[0].y_offset, 200);
    assert_eq!(glyphs[1].y_offset, 70);
    resolve_attachments(&mut glyphs, &mut slots, Direction::Ltr);
    assert_eq!(glyphs[0].y_offset, 270);
}

#[test]
fn cursive_separates_a_parent_attached_to_its_new_child() {
    // RightToLeft first (0 hangs from 1), then flagless (1 hangs
    // from 0): the earlier link is dropped instead of forming a
    // two-glyph cycle.
    let bytes = simple_cursive();
    let subs = [AttachSubtable::parse(gpos_lt::CURSIVE_ATTACHMENT, &bytes).unwrap()];
    let mut glyphs = vec![glyph(1, 600), glyph(2, 500)];
    let mut slots = new_slots(2);
    let filter = MatchFilter::none();
    let var = VarCtx::none();
    let mut att = Attach::new(Direction::Rtl, &mut slots);
    for flag in [LOOKUP_FLAG_RIGHT_TO_LEFT, 0] {
        let cx = LookupCx {
            gdef: None,
            filter: &filter,
            lookup_flag: flag,
            mark_filtering_set: None,
            ignore_zwj: true,
            var: &var,
            lookup_index: 0,
        };
        apply_lookup(&subs, &mut glyphs, &mut att, &cx);
    }
    assert_eq!(att.slots[1], cursive_slot(-1));
    assert_eq!(att.slots[0].chain, 0);
    assert_eq!(glyphs[0].y_offset, 0);
    assert_eq!(glyphs[1].y_offset, -200);
}

#[test]
fn apply_at_ignores_positions_past_the_end() {
    let bytes = simple_cursive();
    let sub = AttachSubtable::parse(gpos_lt::CURSIVE_ATTACHMENT, &bytes).unwrap();
    let mut glyphs = vec![glyph(1, 600)];
    let mut slots = new_slots(1);
    let filter = MatchFilter::none();
    let var = VarCtx::none();
    let cx = LookupCx {
        gdef: None,
        filter: &filter,
        lookup_flag: 0,
        mark_filtering_set: None,
        ignore_zwj: true,
        var: &var,
        lookup_index: 0,
    };
    let mut att = Attach::new(Direction::Ltr, &mut slots);
    assert!(!apply_at(&sub, &mut glyphs, &mut att, &cx, 5));
    assert!(!apply_at(&sub, &mut glyphs, &mut att, &cx, 0));
}

#[test]
fn parse_only_accepts_attachment_lookup_types() {
    let bytes = simple_cursive();
    assert!(AttachSubtable::parse(gpos_lt::PAIR_ADJUSTMENT, &bytes).is_none());
    assert!(AttachSubtable::parse(gpos_lt::MARK_TO_BASE, &[0, 1]).is_none());
}

mod rules;
