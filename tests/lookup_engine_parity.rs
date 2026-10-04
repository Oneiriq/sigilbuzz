//! GSUB and GPOS lookup application checked against HarfBuzz.
//!
//! Every expectation here is HarfBuzz 14.5.0's output (through
//! uharfbuzz 0.56.2) for the same font, text, and buffer settings:
//! glyph ids, clusters (UTF-8 byte offsets), advances, and offsets in
//! font units.

use sigilbuzz::{shape, Blob, Buffer, ClusterLevel, Direction, Face, Font};

const LEPCHA: &[u8] = include_bytes!("fonts/NotoSansLepcha-Regular.ttf");

/// One glyph: id, cluster, x advance, x offset.
type Out = (u32, u32, i32, i32);

fn shaped(font: &[u8], text: &str, level: ClusterLevel) -> Vec<Out> {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).expect("face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(Direction::Ltr);
    buffer.set_cluster_level(level);
    let run = shape(&font, &buffer, &[]).expect("shape");
    run.glyphs
        .iter()
        .map(|g| (g.glyph_id, g.cluster, g.x_advance, g.x_offset))
        .collect()
}

#[test]
fn an_empty_multiple_substitution_sequence_deletes_its_glyph() {
    // Noto Sans Lepcha's `psts` rules delete the vowel sign i (U+1C26)
    // before a final consonant sign through a MultipleSubst with an
    // empty sequence, which HarfBuzz's `Sequence::apply` turns into
    // `delete_glyph`. The glyph used to stay.
    let text = "\u{1C00}\u{1C26}\u{1C2D}";
    assert_eq!(
        shaped(LEPCHA, text, ClusterLevel::MonotoneCharacters),
        [(1, 0, 602, 0), (115, 6, 0, -272), (124, 6, 354, 0)]
    );
}

const SINHALA: &[u8] = include_bytes!("fonts/NotoSansSinhala-Regular.ttf");

#[test]
fn per_syllable_features_do_not_form_conjuncts_across_syllables() {
    // HarfBuzz's USE shaper, which shapes Sinhala, registers its basic
    // features with `F_PER_SYLLABLE`, so the glyphs of the syllable
    // la + ee + ZWJ and those of ra + virama + ha + vocalic r stay
    // apart. Without per-syllable matching a ligature formed across
    // the boundary, swallowing the ZWJ and the ra + virama.
    let text = "\u{0D9B}\u{0DCA}\u{0DBD}\u{0DDA}\u{200D}\u{0DBB}\u{0DCA}\u{0DC4}\u{0DD8}";
    assert_eq!(
        shaped(SINHALA, text, ClusterLevel::MonotoneCharacters),
        [
            (187, 0, 878, 0),
            (74, 6, 631, 0),
            (219, 6, 852, 0),
            (3, 12, 0, 0),
            (218, 15, 702, 0),
            (62, 21, 946, 0),
            (73, 24, 445, 0),
        ]
    );
}

const RUBIK: &[u8] = include_bytes!("fixtures/rubik_vf.ttf");
const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");

/// Glyph id, cluster, x offset and y offset.
fn marks(font: &[u8], text: &str) -> Vec<(u32, u32, i32, i32)> {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).expect("face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(Direction::Ltr);
    let run = shape(&font, &buffer, &[]).expect("shape");
    run.glyphs
        .iter()
        .map(|g| (g.glyph_id, g.cluster, g.x_offset, g.y_offset))
        .collect()
}

#[test]
fn a_cgj_that_blocked_mark_reordering_stays_hidden_from_gsub() {
    // hb-ot-shape-normalize.cc: a COMBINING GRAPHEME JOINER is hidden
    // (GSUB sees it) unless the marks around it were in order anyway,
    // in which case `_hb_glyph_info_unhide` lets GSUB skip it. Rubik's
    // mark lookups pick the acute's form from the mark after it.
    //
    // Acute (230) then cedilla (202): the CGJ kept them from being
    // reordered, so it stays hidden and the acute keeps its own form.
    // At the default grapheme level the marks and the CGJ share the
    // cluster of the base.
    assert_eq!(
        marks(RUBIK, "f\u{0301}\u{034F}\u{0327}"),
        [
            (162, 0, 0, 0),
            (1126, 0, -280, 190),
            (928, 0, 0, 0),
            (1137, 0, -340, 0)
        ]
    );
    // Cedilla then acute were in order: the CGJ is skipped and the
    // acute takes the form it has after a cedilla.
    assert_eq!(
        marks(RUBIK, "f\u{0327}\u{034F}\u{0301}"),
        [
            (162, 0, 0, 0),
            (1154, 0, -340, 0),
            (928, 0, 0, 0),
            (1145, 0, -308, 10)
        ]
    );
    // Before a base the CGJ blocks nothing either: "f", CGJ, "i"
    // ligates.
    assert_eq!(
        marks(OPEN_SANS, "f\u{034F}i"),
        [(564, 0, 0, 0), (3, 0, 0, 0)]
    );
}

const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");

#[test]
fn a_gpos_context_subtable_that_does_not_match_leaves_the_next_its_turn() {
    // HarfBuzz tries a lookup's subtables in order until one applies
    // (hb_ot_layout_lookup_accelerator_t::apply). sigilbuzz stopped at
    // the first chained context subtable that did not match, so
    // Amiri's kerning lookups never reached their later subtables: the
    // teh marbuta here kept its unkerned advance of 587.
    let blob = Blob::new(AMIRI);
    let face = Face::parse(&blob, 0).expect("face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str("\u{0643}\u{0629}\u{0643}\u{0651}\u{0623}\u{062E}\u{0651}");
    buffer.set_direction(Direction::Rtl);
    let run = shape(&font, &buffer, &[]).expect("shape");
    let got: Vec<Out> = run
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.cluster, g.x_advance, g.x_offset))
        .collect();
    assert_eq!(
        got,
        [
            (97, 10, 0, 134),
            (62, 10, 661, 0),
            (4263, 8, 261, 0),
            (97, 4, 0, -9),
            (4173, 4, 343, 0),
            (4366, 2, 336, 0),
            (4330, 0, 674, 0),
        ]
    );
}
