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
