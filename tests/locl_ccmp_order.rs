//! Where `locl` and `ccmp` run relative to a complex shaper's own
//! stages.
//!
//! HarfBuzz runs `locl` and `ccmp` together, first: the Indic shaper
//! before initial reordering (`collect_features_indic`), and the USE
//! shaper, which HarfBuzz uses for Mongolian and N'Ko, before the
//! joining features `isol`/`init`/`medi`/`fina` (`collect_features_use`).
//! sigilbuzz used to run them after those stages, and to run `ccmp`
//! twice for N'Ko.
//!
//! The vendored fonts' own `locl` and `ccmp` lookups do not care about
//! that order, so each test renames another feature of a real font to
//! `locl` or `ccmp` (a four-byte patch of its FeatureList record) and
//! compares the patched font's output with rustybuzz 0.20.

use rustybuzz::Direction as RbDirection;
use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const DEVANAGARI: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Regular.ttf");
const MONGOLIAN: &[u8] = include_bytes!("fonts/NotoSansMongolian-Regular.ttf");
const NKO: &[u8] = include_bytes!("fonts/NotoSansNKo-Regular.ttf");

fn u16_at(data: &[u8], at: usize) -> usize {
    usize::from(u16::from_be_bytes([data[at], data[at + 1]]))
}

/// `font` with every GSUB FeatureList record tagged `from` retagged
/// `to`.
fn retag(font: &[u8], from: &[u8; 4], to: &[u8; 4]) -> Vec<u8> {
    let mut data = font.to_vec();
    let tables = u16_at(&data, 4);
    let gsub = (0..tables)
        .map(|i| 12 + 16 * i)
        .find(|&rec| &data[rec..rec + 4] == b"GSUB")
        .map(|rec| u32::from_be_bytes(data[rec + 8..rec + 12].try_into().unwrap()) as usize)
        .expect("GSUB table");
    let feature_list = gsub + u16_at(&data, gsub + 6);
    let mut hits = 0;
    for i in 0..u16_at(&data, feature_list) {
        let rec = feature_list + 2 + 6 * i;
        if &data[rec..rec + 4] == from {
            data[rec..rec + 4].copy_from_slice(to);
            hits += 1;
        }
    }
    assert!(hits > 0, "no {from:?} feature to retag");
    data
}

type Row = (u32, i32, i32, i32);

fn sigilbuzz_rows(data: &[u8], text: &str, direction: Direction) -> Vec<Row> {
    let blob = Blob::new(data);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    shape(&font, &buffer, &[])
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

fn assert_parity(data: &[u8], texts: &[&str], direction: Direction) {
    let failures: Vec<String> = texts
        .iter()
        .filter_map(|text| {
            let ours = sigilbuzz_rows(data, text, direction);
            let theirs = rustybuzz_rows(data, text, direction);
            (ours != theirs)
                .then(|| format!("{text:?}\n  sigilbuzz: {ours:?}\n  rustybuzz: {theirs:?}"))
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn indic_locl_runs_before_initial_reordering() {
    // Noto Sans Devanagari's `pres` picks the width of the i-matra
    // from the consonant after it, which only matches once initial
    // reordering has moved the matra in front. As `locl`, HarfBuzz
    // runs it before that move.
    let patched = retag(DEVANAGARI, b"pres", b"locl");
    assert_parity(
        &patched,
        &["\u{0915}\u{093F}", "\u{0916}\u{093F} \u{092E}\u{093F}"],
        Direction::Ltr,
    );
}

#[test]
fn mongolian_locl_runs_before_the_joining_features() {
    // Noto Sans Mongolian's `init` lookups, run as `locl`, turn every
    // letter into its initial form when they come before the other
    // joining features, and only the first letter when they come
    // after.
    let patched = retag(MONGOLIAN, b"init", b"locl");
    assert_parity(
        &patched,
        &[
            "\u{1820}\u{1821}\u{1822}",
            "\u{182A}\u{1820}\u{182D}\u{1820}",
        ],
        Direction::Ltr,
    );
}

#[test]
fn nko_locl_runs_before_the_joining_features() {
    // Noto Sans NKo's `aalt` swaps letters for alternates the joining
    // lookups do not cover. As `locl` it runs first in HarfBuzz.
    let patched = retag(NKO, b"aalt", b"locl");
    assert_parity(
        &patched,
        &["\u{07CA}\u{07CB}\u{07CC}", "\u{07D3}\u{07CA}\u{07DE}"],
        Direction::Rtl,
    );
}

#[test]
fn nko_with_combining_marks_matches() {
    assert_parity(
        NKO,
        &["\u{07CA}\u{07F2}\u{07CB}", "\u{07D3}\u{07EB}\u{07CA}"],
        Direction::Rtl,
    );
}
