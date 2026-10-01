//! Unsafe-to-concatenate flags of the lookup fast paths and of the
//! kerning tables, checked against HarfBuzz.
//!
//! HarfBuzz tries a ligature set of two or more ligatures, and a
//! context or chained context rule set of five or more rules, through
//! a fast path (`LigatureSet::apply`, `RuleSet::apply` and
//! `ChainRuleSet::apply`). It reads the glyphs after the cursor first
//! and marks the cursor through the glyph that ruled a rule out, and
//! once a rule matches it marks from where the match left the cursor.
//! A `kern` or `kerx` table the shaping plan applies marks the whole
//! run (`KerxTable::apply`), kerning or not.
//!
//! Every expectation is HarfBuzz 14.5.0's output (through uharfbuzz
//! 0.56.2) with `HB_BUFFER_FLAG_PRODUCE_UNSAFE_TO_CONCAT` at
//! `HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES`: glyph id, cluster (a
//! UTF-8 byte offset) and glyph flags, in output order.

use sigilbuzz::{shape, Blob, Buffer, BufferFlags, ClusterLevel, Direction, Face, Font};

const MODI: &[u8] = include_bytes!("fonts/NotoSansModi-Regular.ttf");
const SHARADA: &[u8] = include_bytes!("fonts/NotoSansSharada-Regular.ttf");
const TAI_THAM: &[u8] = include_bytes!("fonts/NotoSansTaiTham-Regular.ttf");
const KHMER: &[u8] = include_bytes!("fonts/NotoSansKhmer-Regular.ttf");
const MYANMAR: &[u8] = include_bytes!("fonts/NotoSansMyanmar-Regular.ttf");
const TELUGU: &[u8] = include_bytes!("fonts/NotoSansTelugu-Regular.ttf");
const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");

/// Shapes `text` left to right with unsafe-to-concat flags on and
/// returns (glyph id, cluster, flag bits) per glyph.
fn flags(font: &[u8], text: &str) -> Vec<(u32, u32, u32)> {
    flags_in(font, text, Direction::Ltr)
}

/// [`flags`] in `direction`.
fn flags_in(font: &[u8], text: &str, direction: Direction) -> Vec<(u32, u32, u32)> {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).expect("face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    buffer.set_cluster_level(ClusterLevel::MonotoneGraphemes);
    buffer.set_flags(BufferFlags::PRODUCE_UNSAFE_TO_CONCAT);
    let run = shape(&font, &buffer, &[]).expect("shape");
    run.glyphs
        .iter()
        .map(|g| (g.glyph_id, g.cluster, g.flags.bits()))
        .collect()
}

#[test]
fn a_chained_rule_passed_over_marks_the_glyph_that_ruled_it_out() {
    // Noto Sans Modi's `pres` lookup 11 is a class-based chained
    // context with more than four rules for ra. Rules the glyph after
    // ra rules out are passed over, so ra through that glyph (the
    // independent vowel o) is unsafe to concatenate. A full match of
    // those rules fails in its input and marks nothing.
    assert_eq!(
        flags(
            MODI,
            "\u{11611}\u{11608}\u{11629}\u{11628}\u{1160C}\u{11621}"
        ),
        [
            (35, 0, 2),
            (26, 4, 2),
            (59, 8, 2),
            (156, 12, 3),
            (30, 16, 2),
            (51, 20, 2)
        ]
    );
}

#[test]
fn a_ligature_marks_from_the_end_of_its_match() {
    // Noto Sans Sharada's `akhn` set for nya passes over nya + u
    // before nya + uu ligates. HarfBuzz marks from where the ligature
    // left the cursor, past its last component, so nothing is marked.
    assert_eq!(flags(SHARADA, "\u{1119A}\u{111B7}"), [(107, 0, 0)]);
}

#[test]
fn a_matched_rule_marks_from_the_end_of_its_match() {
    // Ligatures and rules passed over before a match mark nothing
    // before the end of the match.
    assert_eq!(
        flags(
            TAI_THAM,
            "\u{1A3F}\u{1A4A}\u{1A58}\u{1A31}\u{1A4C}\u{1A5E}\u{1A40}\u{1A6E}"
        ),
        [
            (549, 0, 0),
            (555, 3, 2),
            (678, 3, 2),
            (531, 9, 2),
            (560, 12, 0),
            (688, 12, 0),
            (358, 18, 0)
        ]
    );
    assert_eq!(
        flags(KHMER, "\u{1796}\u{17C5}\u{17B6}\u{1786}"),
        [
            (107, 0, 0),
            (261, 0, 0),
            (360, 0, 0),
            (80, 0, 0),
            (31, 9, 2)
        ]
    );
    assert_eq!(
        flags(TELUGU, "\u{0C16}\u{0C48}\u{0C40}"),
        [(327, 0, 0), (62, 0, 0)]
    );
}

#[test]
fn a_rule_matched_on_the_last_glyph_marks_from_the_end_of_its_match() {
    // Noto Sans Myanmar's `blws` lookup 72 is a class-based chained
    // context with more than four rules for the dotted circle, which
    // ends the run. Rules that need a glyph after it are passed over,
    // and a later rule with only a backtrack matches. HarfBuzz marks
    // from where the match left the cursor, the end of the run, so
    // nothing is marked.
    assert_eq!(
        flags(MYANMAR, "\u{1031}\u{FE00}"),
        [(547, 0, 0), (388, 0, 0)]
    );
    assert_eq!(
        flags(MYANMAR, "\u{25CC}\u{1031}\u{FE00}"),
        [(547, 0, 0), (388, 0, 0)]
    );
    assert_eq!(
        flags(MYANMAR, "\u{AA78}\u{1084}"),
        [(170, 0, 0), (118, 0, 0), (388, 0, 0)]
    );
}

/// Open Sans with its GPOS table renamed, so only its `kern` table can
/// kern.
fn open_sans_without_gpos() -> Vec<u8> {
    let mut data = OPEN_SANS.to_vec();
    let tables = usize::from(u16::from_be_bytes([data[4], data[5]]));
    let rec = (0..tables)
        .map(|i| 12 + 16 * i)
        .find(|&rec| &data[rec..rec + 4] == b"GPOS")
        .expect("GPOS table");
    data[rec..rec + 4].copy_from_slice(b"GPOX");
    data
}

#[test]
fn an_applied_kern_table_marks_the_whole_run() {
    // GPOS has no `vkrn`, so HarfBuzz applies the legacy `kern` table
    // to vertical text, which it does not kern, and marks every glyph.
    assert_eq!(
        flags_in(OPEN_SANS, "office", Direction::Ttb),
        [
            (82, 0, 2),
            (73, 1, 2),
            (73, 2, 2),
            (76, 3, 2),
            (70, 4, 2),
            (72, 5, 2)
        ]
    );
    let font = open_sans_without_gpos();
    assert_eq!(
        flags(&font, "AVATAR"),
        [
            (36, 0, 2),
            (57, 1, 3),
            (36, 2, 3),
            (55, 3, 3),
            (36, 4, 3),
            (53, 5, 2)
        ]
    );
    // HarfBuzz's Thai shaper never applies the legacy table.
    assert_eq!(flags(&font, "\u{0E01}\u{0E02}"), [(0, 0, 0), (0, 3, 0)]);
}
