//! Default-ignorable parity: sigilbuzz hides every character HarfBuzz
//! treats as default ignorable, the way HarfBuzz does.
//!
//! HarfBuzz (`hb-ot-shape.cc`) looks each default-ignorable code point
//! up in the font like any other character, lets GSUB and GPOS see
//! that glyph, and only after positioning zeroes its advance and
//! offset (`hb_ot_zero_width_default_ignorables`) and swaps it for the
//! space glyph (`hb_ot_hide_default_ignorables`), unless GSUB
//! substituted it. The set comes from `hb_unicode_funcs_t::
//! is_default_ignorable` in `hb-unicode.hh`, which is
//! Default_Ignorable_Code_Point minus the Hangul fillers and U+1BCA0
//! to U+1BCA3.
//!
//! Every case is compared with rustybuzz 0.20 (a port of HarfBuzz)
//! on glyph id, advance, and offset. Clusters are left out: these
//! buffers keep sigilbuzz's default cluster level, MONOTONE_CHARACTERS,
//! while rustybuzz defaults to MONOTONE_GRAPHEMES, which merges a
//! grapheme's clusters (`hb_form_clusters`).
//!
//! Known gaps, not covered here: HarfBuzz's lookup matcher also skips
//! default-ignorable glyphs inside a match (ZWJ within `liga` input, any
//! of them within context), so `f` ZWJ `i` still ligates and Amiri's
//! contextual beh forms still see each other across a ZWSP, soft
//! hyphen, ALM, or word joiner. sigilbuzz's matcher does not skip them.

use rustybuzz::Direction as RbDirection;
use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");
const DEVANAGARI: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Regular.ttf");
const KHMER: &[u8] = include_bytes!("fonts/NotoSansKhmer-Regular.ttf");
const MONGOLIAN: &[u8] = include_bytes!("fonts/NotoSansMongolian-Regular.ttf");

/// Shapes `text` with both engines and returns the mismatch report,
/// or `None` when they agree on every glyph.
fn diff(font_data: &[u8], text: &str, direction: Direction) -> Option<String> {
    let blob = Blob::new(font_data);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    let sig = shape(&font, &buffer, &[]).expect("sigilbuzz shape");

    let rb_face = rustybuzz::Face::from_slice(font_data, 0).expect("parse rustybuzz face");
    let mut rb_buf = rustybuzz::UnicodeBuffer::new();
    rb_buf.push_str(text);
    rb_buf.set_direction(match direction {
        Direction::Ltr => RbDirection::LeftToRight,
        Direction::Rtl => RbDirection::RightToLeft,
        Direction::Ttb => RbDirection::TopToBottom,
        Direction::Btt => RbDirection::BottomToTop,
    });
    let rb = rustybuzz::shape(&rb_face, &[], rb_buf);

    let ours: Vec<(u32, i32, i32, i32, i32)> = sig
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.x_advance, g.y_advance, g.x_offset, g.y_offset))
        .collect();
    let theirs: Vec<(u32, i32, i32, i32, i32)> = rb
        .glyph_infos()
        .iter()
        .zip(rb.glyph_positions())
        .map(|(i, p)| (i.glyph_id, p.x_advance, p.y_advance, p.x_offset, p.y_offset))
        .collect();
    (ours != theirs)
        .then(|| format!("{text:?} {direction:?}\n  sigilbuzz: {ours:?}\n  rustybuzz: {theirs:?}"))
}

fn assert_parity(font_data: &[u8], cases: &[(&str, Direction)]) {
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|&(text, direction)| diff(font_data, text, direction))
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn latin_default_ignorables_match_rustybuzz() {
    use Direction::Ltr;
    assert_parity(
        OPEN_SANS,
        &[
            ("a\u{00AD}b", Ltr),
            ("a\u{034F}b", Ltr),
            ("a\u{061C}b", Ltr),
            ("a\u{200B}b", Ltr),
            ("a\u{200C}b", Ltr),
            ("a\u{200D}b", Ltr),
            ("a\u{200E}b\u{200F}", Ltr),
            ("a\u{202A}b\u{202C}c\u{202E}", Ltr),
            ("a\u{2060}b\u{2064}c\u{2066}d\u{2069}", Ltr),
            ("a\u{FE0F}b\u{FE00}", Ltr),
            ("\u{FEFF}ab", Ltr),
            ("a\u{E0001}\u{E0041}b\u{E007F}", Ltr),
            ("a\u{E0100}b", Ltr),
            ("a\u{1D173}b", Ltr),
            ("a\u{FFF0}b", Ltr),
            ("f\u{200C}i", Ltr),
            // Not hidden: Hangul fillers and the shorthand format
            // controls are exceptions in HarfBuzz.
            ("a\u{115F}\u{1160}\u{3164}\u{FFA0}b", Ltr),
            ("a\u{1BCA0}b", Ltr),
        ],
    );
}

#[test]
fn arabic_joiners_and_marks_match_rustybuzz() {
    use Direction::Rtl;
    assert_parity(
        AMIRI,
        &[
            ("\u{0644}\u{200D}\u{0627}", Rtl),
            ("\u{0628}\u{200C}\u{0628}", Rtl),
            ("\u{0628}\u{200D}", Rtl),
            ("\u{200D}\u{0628}", Rtl),
            ("\u{0627}\u{00AD}\u{0627}", Rtl),
            ("\u{0627}\u{061C}\u{0628}", Rtl),
            ("\u{0628}\u{200E}\u{0627}", Rtl),
        ],
    );
}

#[test]
fn devanagari_joiners_match_rustybuzz() {
    use Direction::Ltr;
    assert_parity(
        DEVANAGARI,
        &[
            // The ZWNJ-after-halant and eyelash-ra (ra halant ZWJ)
            // sequences still differ, in the Indic shaper's joiner
            // handling rather than in hiding.
            ("\u{0915}\u{094D}\u{200D}\u{0937}", Ltr),
            ("\u{0915}\u{094D}\u{200D}", Ltr),
            ("\u{0915}\u{200D}\u{093F}", Ltr),
            ("\u{0915}\u{00AD}\u{0916}", Ltr),
        ],
    );
}

#[test]
fn khmer_inherent_vowels_are_hidden() {
    use Direction::Ltr;
    assert_parity(
        KHMER,
        &[("\u{1780}\u{17B4}", Ltr), ("\u{1780}\u{17B5}\u{1781}", Ltr)],
    );
}

#[test]
fn mongolian_free_variation_selectors_match_rustybuzz() {
    use Direction::Ltr;
    assert_parity(
        MONGOLIAN,
        &[
            ("\u{1820}\u{180B}\u{1821}", Ltr),
            ("\u{1821}\u{180C}", Ltr),
            ("\u{1820}\u{180E}\u{1821}", Ltr),
            ("\u{1820}\u{180F}\u{1821}", Ltr),
        ],
    );
}

#[test]
fn vertical_ignorables_lose_their_vertical_advance() {
    // Only the ignorables are compared: sigilbuzz does not apply
    // vertical glyph origins yet, so the letters' offsets differ.
    let blob = Blob::new(MONGOLIAN);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);
    let rb_face = rustybuzz::Face::from_slice(MONGOLIAN, 0).expect("parse rustybuzz face");
    let text = "\u{1820}\u{200B}\u{1821}\u{00AD}\u{1822}\u{2060}";
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(Direction::Ttb);
    let ours = shape(&font, &buffer, &[]).expect("sigilbuzz shape");
    let mut rb_buf = rustybuzz::UnicodeBuffer::new();
    rb_buf.push_str(text);
    rb_buf.set_direction(RbDirection::TopToBottom);
    let theirs = rustybuzz::shape(&rb_face, &[], rb_buf);
    assert_eq!(ours.len(), theirs.len());
    for i in [1, 3, 5] {
        let (g, info, pos) = (
            &ours.glyphs[i],
            &theirs.glyph_infos()[i],
            &theirs.glyph_positions()[i],
        );
        assert_eq!(
            (g.glyph_id, g.x_advance, g.y_advance, g.y_offset),
            (info.glyph_id, pos.x_advance, pos.y_advance, pos.y_offset),
            "glyph {i}"
        );
        assert_eq!(g.y_advance, 0);
    }
}
