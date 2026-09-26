//! Fallback positioning parity with rustybuzz.
//!
//! Space characters a font does not map are drawn with the space glyph
//! and given the width of their kind, as HarfBuzz's
//! `_hb_ot_shape_fallback_spaces` does: an em fraction for the em
//! spaces, a digit's width for U+2007 FIGURE SPACE, a period's for
//! U+2008 PUNCTUATION SPACE, half a space for U+202F NARROW NO-BREAK
//! SPACE.

use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
const NOTO_DEVANAGARI: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Regular.ttf");
const RUBIK: &[u8] = include_bytes!("fixtures/rubik_vf.ttf");

/// `(glyph id, cluster, x advance, y advance, x offset, y offset)`.
type Row = (u32, u32, i32, i32, i32, i32);

fn sigilbuzz_rows(font_bytes: &[u8], text: &str, direction: Direction) -> Vec<Row> {
    let blob = Blob::new(font_bytes);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    shape(&font, &buffer, &[])
        .expect("shape")
        .glyphs
        .iter()
        .map(|g| {
            (
                g.glyph_id,
                g.cluster,
                g.x_advance,
                g.y_advance,
                g.x_offset,
                g.y_offset,
            )
        })
        .collect()
}

fn rustybuzz_rows(font_bytes: &[u8], text: &str, direction: rustybuzz::Direction) -> Vec<Row> {
    let face = rustybuzz::Face::from_slice(font_bytes, 0).expect("parse face");
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    buffer.set_direction(direction);
    let out = rustybuzz::shape(&face, &[], buffer);
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| {
            (
                i.glyph_id,
                i.cluster,
                p.x_advance,
                p.y_advance,
                p.x_offset,
                p.y_offset,
            )
        })
        .collect()
}

fn assert_ltr_parity(font: &[u8], text: &str) {
    assert_eq!(
        sigilbuzz_rows(font, text, Direction::Ltr),
        rustybuzz_rows(font, text, rustybuzz::Direction::LeftToRight),
        "{text:?}"
    );
}

#[test]
fn missing_space_characters_take_their_fallback_widths() {
    // Open Sans maps U+2000..U+200A but not these three.
    assert_ltr_parity(OPEN_SANS, "a\u{202F}b\u{205F}c\u{3000}d");
    // Noto Sans Devanagari maps none of the special spaces, but has
    // digits and a period for the figure and punctuation spaces.
    for text in [
        "\u{0915}\u{2000}\u{0915}\u{2001}\u{0915}\u{2002}\u{0915}\u{2003}\u{0915}",
        "\u{0915}\u{2004}\u{0915}\u{2005}\u{0915}\u{2006}\u{0915}\u{2009}\u{0915}\u{200A}",
        "\u{0967}\u{2007}\u{0968}\u{2008}\u{0969}",
        "\u{0915}\u{202F}\u{0915}\u{205F}\u{0915}\u{3000}\u{0915}",
    ] {
        assert_ltr_parity(NOTO_DEVANAGARI, text);
    }
}

#[test]
fn a_fallback_space_draws_with_the_space_glyph() {
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).expect("parse face");
    let space = face.cmap().expect("cmap").glyph_id(' ').expect("space");
    let upem = i32::from(face.head().expect("head").units_per_em);
    let rows = sigilbuzz_rows(OPEN_SANS, "\u{3000}\u{205F}", Direction::Ltr);
    assert_eq!(rows[0].0, u32::from(space));
    // IDEOGRAPHIC SPACE is an em, MEDIUM MATHEMATICAL SPACE 4/18 em.
    assert_eq!(rows[0].2, upem);
    assert_eq!(rows[1].2, upem * 4 / 18);
}

#[test]
fn figure_and_punctuation_spaces_follow_the_font_at_default_coords() {
    // Rubik is variable; at the default instance the widths come from
    // hmtx like everything else.
    assert_ltr_parity(RUBIK, "1\u{2007}2\u{2008}3\u{2009}4");
}
