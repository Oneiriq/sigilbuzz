//! `Buffer::guess_segment_properties` against HarfBuzz.
//!
//! HarfBuzz shapes a buffer with one script, which
//! `hb_buffer_guess_segment_properties` takes from the first character
//! that is not Common or Inherited. After the same call, sigilbuzz
//! shapes a buffer that mixes scripts with that one shaper too. Without
//! it, sigilbuzz shapes each script run with its own shaper.
//!
//! Every expectation is HarfBuzz 14.5.0's output (through uharfbuzz
//! 0.56.2) after `guess_segment_properties`, at the MONOTONE_GRAPHEMES
//! cluster level: glyph id, cluster (a UTF-8 byte offset), x advance,
//! and x and y offset.

use sigilbuzz::{shape, Blob, Buffer, ClusterLevel, Face, Font, UnicodeScript};

const SINHALA: &[u8] = include_bytes!("fonts/NotoSansSinhala-Regular.ttf");
const TELUGU: &[u8] = include_bytes!("fonts/NotoSansTelugu-Regular.ttf");
const KHOJKI: &[u8] = include_bytes!("fonts/NotoSansKhojki-Regular.ttf");

type Row = (u32, u32, i32, i32, i32);

fn rows(font: &[u8], text: &str, guess: bool) -> Vec<Row> {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).expect("face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_cluster_level(ClusterLevel::MonotoneGraphemes);
    if guess {
        buffer.guess_segment_properties();
    }
    let run = shape(&font, &buffer, &[]).expect("shape");
    run.glyphs
        .iter()
        .map(|g| (g.glyph_id, g.cluster, g.x_advance, g.x_offset, g.y_offset))
        .collect()
}

#[test]
fn a_mixed_buffer_shapes_with_the_script_of_its_first_letter() {
    // Latin first, so HarfBuzz shapes the Sinhala letter and its
    // pre-base vowel sign with the default shaper: the sign stays after
    // the letter and gets no syllable.
    let text = "a\u{0D91}\u{0DD9}";
    assert_eq!(
        rows(SINHALA, text, true),
        [(0, 0, 600, 0, 0), (18, 1, 814, 0, 0), (74, 1, 631, 0, 0)]
    );
    // Without the guess, the Sinhala run shapes with the Universal
    // Shaping Engine, which moves the sign in front of the letter.
    assert_eq!(
        rows(SINHALA, text, false),
        [(0, 0, 600, 0, 0), (74, 1, 631, 0, 0), (18, 1, 814, 0, 0)]
    );
}

#[test]
fn a_latin_buffer_inserts_no_vowel_constraint_circle() {
    // Telugu vowel sign i followed by the length mark is a vowel
    // constraint sequence, but only the Indic shaper checks those.
    assert_eq!(
        rows(TELUGU, "a\u{0C3F}\u{0C55}", true),
        [(0, 0, 600, 0, 0), (61, 0, 0, 0, 0), (74, 0, 0, 0, 0)]
    );
}

#[test]
fn a_latin_buffer_positions_marks_with_the_latin_lookups() {
    // In a Latin buffer HarfBuzz looks GPOS features up under the
    // Latin script tags, and the Khojki mark gets no offset. Shaped as a
    // Khojki run of its own, it would.
    assert_eq!(
        rows(KHOJKI, "a\u{11200}\u{11231}", true),
        [(0, 0, 600, 0, 0), (99, 1, 918, 0, 0), (147, 1, 0, 0, 0)]
    );
}

#[test]
fn the_guess_keeps_what_the_caller_set() {
    let mut buffer = Buffer::new();
    buffer.push_str("a\u{0D91}");
    buffer.set_script(Some(UnicodeScript::Sinhala));
    buffer.guess_segment_properties();
    assert_eq!(buffer.script(), Some(UnicodeScript::Sinhala));
    // Digits and spaces are Common, so the first letter decides.
    let mut buffer = Buffer::new();
    buffer.push_str("12 \u{0D91}a");
    buffer.guess_segment_properties();
    assert_eq!(buffer.script(), Some(UnicodeScript::Sinhala));
}
