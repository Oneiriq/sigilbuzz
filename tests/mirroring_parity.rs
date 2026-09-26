//! Bidi mirroring parity: in a right-to-left run HarfBuzz draws
//! `(` as `)` (`hb_ot_rotate_chars`), replacing each character that
//! has a `Bidi_Mirroring_Glyph` the font maps, and applies the font's
//! `rtlm` feature to every other glyph of the run.
//!
//! Each case compares glyph ids, advances, and offsets with rustybuzz
//! 0.20. Amiri has an `rtlm` feature, which covers the mirrored
//! mathematical symbols that have no mirror character.

use rustybuzz::Direction as RbDirection;
use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Feature, Font};

const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");
const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
const HEBREW: &[u8] = include_bytes!("fonts/NotoSansHebrew-Regular.ttf");

type Row = (u32, i32, i32, i32);

fn sigilbuzz_rows(data: &[u8], text: &str, direction: Direction, features: &[Feature]) -> Vec<Row> {
    let blob = Blob::new(data);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    shape(&font, &buffer, features)
        .expect("sigilbuzz shape")
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.x_advance, g.x_offset, g.y_offset))
        .collect()
}

fn rustybuzz_rows(data: &[u8], text: &str, direction: Direction) -> Vec<Row> {
    let face = rustybuzz::Face::from_slice(data, 0).expect("parse rustybuzz face");
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_direction(match direction {
        Direction::Rtl => RbDirection::RightToLeft,
        _ => RbDirection::LeftToRight,
    });
    let out = rustybuzz::shape(&face, &[], buffer);
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| (i.glyph_id, p.x_advance, p.x_offset, p.y_offset))
        .collect()
}

fn assert_parity(data: &[u8], cases: &[(&str, Direction)]) {
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|&(text, direction)| {
            let ours = sigilbuzz_rows(data, text, direction, &[]);
            let theirs = rustybuzz_rows(data, text, direction);
            (ours != theirs).then(|| {
                format!("{text:?} {direction:?}\n  sigilbuzz: {ours:?}\n  rustybuzz: {theirs:?}")
            })
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn arabic_brackets_are_mirrored() {
    use Direction::Rtl;
    assert_parity(
        AMIRI,
        &[
            ("(\u{0628}\u{0628})", Rtl),
            ("[\u{0628}] {\u{0628}}", Rtl),
            ("<\u{0628}> \u{00AB}\u{0628}\u{00BB}", Rtl),
            ("\u{0628} (1 < 2)", Rtl),
        ],
    );
}

#[test]
fn rtlm_covers_symbols_without_a_mirror_character() {
    use Direction::Rtl;
    assert_parity(
        AMIRI,
        &[
            ("\u{2211}\u{0628}", Rtl),
            ("\u{221A}\u{0628}", Rtl),
            ("\u{222B}\u{0628} \u{2208} \u{2282}", Rtl),
        ],
    );
}

#[test]
fn hebrew_and_latin_brackets_are_mirrored_in_rtl() {
    use Direction::{Ltr, Rtl};
    assert_parity(HEBREW, &[("(\u{05E9}\u{05DC}\u{05D5}\u{05DD})", Rtl)]);
    assert_parity(
        OPEN_SANS,
        &[("(abc) [x]", Rtl), ("(abc) [x]", Ltr), ("a < b", Rtl)],
    );
}

#[test]
fn rtl_parenthesis_draws_the_closing_glyph() {
    let ltr = sigilbuzz_rows(OPEN_SANS, ")", Direction::Ltr, &[]);
    let rtl = sigilbuzz_rows(OPEN_SANS, "(", Direction::Rtl, &[]);
    assert_eq!(ltr, rtl);
}

#[test]
fn rtlm_can_be_turned_off() {
    let off = [Feature {
        tag: *b"rtlm",
        value: 0,
    }];
    let text = "\u{221A}\u{0628}";
    let with = sigilbuzz_rows(AMIRI, text, Direction::Rtl, &[]);
    let without = sigilbuzz_rows(AMIRI, text, Direction::Rtl, &off);
    assert_ne!(with, without, "Amiri's rtlm mirrors the square root");
}
