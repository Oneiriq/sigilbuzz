//! Script and language system selection parity with rustybuzz.
//!
//! HarfBuzz picks one script per table (the run's own script tags,
//! then `DFLT`, `dflt`, `latn`) and one language system in it, and
//! takes every feature from that language system alone. A feature the
//! language system lacks is simply off, even when `DFLT` or another
//! script has it. The fonts below have such gaps:
//!
//! - Rubik lists `kern` under GPOS `DFLT`, `latn`, `hebr`, and `cyrl`,
//!   but not under `arab`, so Arabic text is not kerned.
//! - Noto Sans Lepcha lists `kern` under GPOS `DFLT` only, not under
//!   `lepc`.
//! - Noto Sans Telugu lists `kern` under GPOS `tel2` only, not `telu`.
//!
//! Each case compares glyph ids, advances, and offsets.

use rustybuzz::Direction as RbDirection;
use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font, Language};

const RUBIK: &[u8] = include_bytes!("fixtures/rubik_vf.ttf");
const LEPCHA: &[u8] = include_bytes!("fonts/NotoSansLepcha-Regular.ttf");
const TELUGU: &[u8] = include_bytes!("fonts/NotoSansTelugu-Regular.ttf");
const DEVANAGARI: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Regular.ttf");
const OLD_HANGUL: &[u8] = include_bytes!("fonts/NotoSansOldHangul-Subset.ttf");

// Clusters are left out: the Indic shaper does not merge syllable
// clusters the way HarfBuzz does yet, which is unrelated to feature
// selection.
type Row = (u32, i32, i32, i32);

fn sigilbuzz_rows(data: &[u8], text: &str, direction: Direction, lang: Option<&str>) -> Vec<Row> {
    let blob = Blob::new(data);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    buffer.set_language(lang.and_then(Language::new));
    shape(&font, &buffer, &[])
        .expect("sigilbuzz shape")
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.x_advance, g.x_offset, g.y_offset))
        .collect()
}

fn rustybuzz_rows(data: &[u8], text: &str, direction: Direction, lang: Option<&str>) -> Vec<Row> {
    let face = rustybuzz::Face::from_slice(data, 0).expect("parse rustybuzz face");
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_direction(match direction {
        Direction::Rtl => RbDirection::RightToLeft,
        _ => RbDirection::LeftToRight,
    });
    if let Some(lang) = lang {
        buffer.set_language(lang.parse().expect("language"));
    }
    let out = rustybuzz::shape(&face, &[], buffer);
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| (i.glyph_id, p.x_advance, p.x_offset, p.y_offset))
        .collect()
}

fn assert_parity(data: &[u8], cases: &[(&str, Direction, Option<&str>)]) {
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|&(text, direction, lang)| {
            let ours = sigilbuzz_rows(data, text, direction, lang);
            let theirs = rustybuzz_rows(data, text, direction, lang);
            (ours != theirs).then(|| {
                format!("{text:?} {lang:?}\n  sigilbuzz: {ours:?}\n  rustybuzz: {theirs:?}")
            })
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn arabic_in_rubik_takes_features_from_arab_only() {
    use Direction::Rtl;
    assert_parity(
        RUBIK,
        &[
            ("\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}", Rtl, None),
            (
                "\u{0644}\u{0627} \u{0643}\u{062A}\u{0627}\u{0628}",
                Rtl,
                None,
            ),
            ("\u{0627}\u{0648}\u{062F}\u{0631}\u{0632}", Rtl, None),
        ],
    );
}

#[test]
fn latin_hebrew_and_cyrillic_in_rubik_match() {
    use Direction::{Ltr, Rtl};
    assert_parity(
        RUBIK,
        &[
            ("AVATAR Tokyo", Ltr, None),
            ("AVATAR Tokyo", Ltr, Some("tr")),
            ("\u{05E9}\u{05DC}\u{05D5}\u{05DD}", Rtl, None),
            ("\u{0410}\u{0412}\u{0422}\u{041E}", Ltr, None),
        ],
    );
}

#[test]
fn lepcha_takes_features_from_lepc_only() {
    use Direction::Ltr;
    assert_parity(
        LEPCHA,
        &[
            ("\u{1C00}\u{1C01}\u{1C02}\u{1C03}", Ltr, None),
            ("\u{1C04}\u{1C24}\u{1C05}\u{1C26}\u{1C0A}", Ltr, None),
            ("\u{1C1A}\u{1C1B}\u{1C1C}\u{1C1D}\u{1C1E}", Ltr, None),
        ],
    );
}

#[test]
fn telugu_takes_features_from_tel2() {
    use Direction::Ltr;
    assert_parity(
        TELUGU,
        &[
            (
                "\u{0C24}\u{0C46}\u{0C32}\u{0C41}\u{0C17}\u{0C41}",
                Ltr,
                None,
            ),
            ("\u{0C15}\u{0C4D}\u{0C37}", Ltr, None),
        ],
    );
}

#[test]
fn devanagari_language_systems_match() {
    use Direction::Ltr;
    let text = "\u{0936}\u{094D}\u{0930}\u{0940} \u{0932}\u{0915}\u{094D}\u{0937}";
    assert_parity(
        DEVANAGARI,
        &[
            (text, Ltr, None),
            (text, Ltr, Some("mr")),
            (text, Ltr, Some("ne")),
            (text, Ltr, Some("sa")),
        ],
    );
}

#[test]
fn hangul_font_with_many_scripts_matches() {
    use Direction::Ltr;
    assert_parity(OLD_HANGUL, &[("\u{1100}\u{1161}\u{11A8}", Ltr, None)]);
}
