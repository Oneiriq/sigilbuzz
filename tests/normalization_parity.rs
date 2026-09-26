//! Normalization parity with rustybuzz on decomposed and
//! non-canonically ordered input.
//!
//! Shaping normalizes the text against the font the way HarfBuzz's
//! `_hb_ot_shape_normalize` does: decomposed input recomposes into the
//! precomposed glyphs a font has (Open Sans's Latin and Vietnamese
//! letters, Amiri's alef with hamza), and marks typed in any order sort
//! into canonical order with HarfBuzz's modified combining classes
//! (Hebrew points in the SBL order, Arabic shadda first) before GSUB
//! and GPOS see them. Each case must match rustybuzz glyph for glyph,
//! position for position.
//!
//! Clusters are checked on the cases where every glyph's cluster is
//! decided by normalization itself (a composite takes the smallest
//! cluster of what it spans). Where a mark is left uncomposed, HarfBuzz
//! merges it into its base's grapheme cluster before normalizing
//! (`hb_form_clusters`), which sigilbuzz does not do yet.

use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
const NOTO_HEBREW: &[u8] = include_bytes!("fonts/NotoSansHebrew-Regular.ttf");
const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");

/// `(glyph id, x advance, y advance, x offset, y offset)` per glyph,
/// and the clusters.
type Shaped = (Vec<(u32, i32, i32, i32, i32)>, Vec<u32>);

fn sigilbuzz_shape(font_bytes: &[u8], text: &str, rtl: bool) -> Shaped {
    let blob = Blob::new(font_bytes);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(if rtl { Direction::Rtl } else { Direction::Ltr });
    let run = shape(&font, &buffer, &[]).expect("shape");
    (
        run.glyphs
            .iter()
            .map(|g| (g.glyph_id, g.x_advance, g.y_advance, g.x_offset, g.y_offset))
            .collect(),
        run.glyphs.iter().map(|g| g.cluster).collect(),
    )
}

fn rustybuzz_shape(font_bytes: &[u8], text: &str) -> Shaped {
    let face = rustybuzz::Face::from_slice(font_bytes, 0).expect("parse face");
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    let out = rustybuzz::shape(&face, &[], buffer);
    (
        out.glyph_infos()
            .iter()
            .zip(out.glyph_positions())
            .map(|(i, p)| (i.glyph_id, p.x_advance, p.y_advance, p.x_offset, p.y_offset))
            .collect(),
        out.glyph_infos().iter().map(|i| i.cluster).collect(),
    )
}

fn assert_parity(font: &[u8], text: &str, rtl: bool, check_clusters: bool) {
    let (sig, sig_clusters) = sigilbuzz_shape(font, text, rtl);
    let (rb, rb_clusters) = rustybuzz_shape(font, text);
    assert_eq!(sig, rb, "positions of {text:?}");
    if check_clusters {
        assert_eq!(sig_clusters, rb_clusters, "clusters of {text:?}");
    }
}

#[test]
fn decomposed_latin_recomposes_like_rustybuzz() {
    for text in [
        "e\u{0301}",
        "cafe\u{0301}",
        "A\u{030A}ngstro\u{0308}m",
        "n\u{0303}o",
        // Vietnamese stacked marks, canonical and non-canonical order.
        "Vie\u{0323}\u{0302}t",
        "Vie\u{0302}\u{0323}t",
        "o\u{0302}\u{0301}",
        "a\u{0306}\u{0303}",
        "u\u{031B}\u{0309}",
        "Tie\u{0302}\u{0301}ng Vie\u{0323}\u{0302}t",
    ] {
        assert_parity(OPEN_SANS, text, false, true);
    }
}

#[test]
fn precomposed_input_the_font_lacks_decomposes_like_rustybuzz() {
    // U+212B ANGSTROM SIGN is a singleton decomposition of U+00C5.
    assert_parity(OPEN_SANS, "\u{212B}", false, true);
    // A mark with no composite stays a separate glyph.
    assert_parity(OPEN_SANS, "q\u{0301}", false, false);
}

#[test]
fn hebrew_points_in_any_order_match_rustybuzz() {
    for text in [
        // Shin with shin dot and qamats, typed in both orders.
        "\u{05E9}\u{05C1}\u{05B8}\u{05DC}\u{05D5}\u{05B9}\u{05DD}",
        "\u{05E9}\u{05B8}\u{05C1}\u{05DC}\u{05D5}\u{05B9}\u{05DD}",
        // Bet with dagesh and sheva, typed sheva first.
        "\u{05D1}\u{05B0}\u{05BC}\u{05E8}\u{05B5}\u{05D0}\u{05E9}\u{05B4}\u{05C1}\u{05D9}\u{05EA}",
        // Patah, sheva, and meteg: the Hebrew mark hook.
        "\u{05D0}\u{05B7}\u{05B0}\u{05BD}",
    ] {
        assert_parity(NOTO_HEBREW, text, true, false);
    }
}

#[test]
fn arabic_marks_in_any_order_match_rustybuzz() {
    for text in [
        // Fatha typed before shadda: shadda sorts first.
        "\u{0628}\u{064E}\u{0651}\u{0627}\u{0628}",
        "\u{0645}\u{064F}\u{0651}\u{062D}\u{0645}\u{064E}\u{0651}\u{062F}",
        // Kasra before shadda.
        "\u{0628}\u{0650}\u{0651}",
        // Alef, fatha, hamza above: the hamza moves forward and
        // composes with the alef.
        "\u{0627}\u{064E}\u{0654}\u{0628}",
    ] {
        assert_parity(AMIRI, text, true, false);
    }
}
