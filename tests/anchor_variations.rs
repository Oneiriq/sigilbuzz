//! Variable mark anchors: Rubik Variable (`wght` axis) ships
//! AnchorFormat3 anchors whose device offsets point at GDEF
//! VariationIndex rows, so the attachment points of its combining
//! marks move with the weight. These tests shape marked text at
//! several weights and compare every glyph's advance and offset with
//! HarfBuzz 14.5.0 at the same axis value, in both directions.
//!
//! `tests/fixtures/rubik_variable_shaping.expected` holds HarfBuzz's
//! output; `tests/tools/variable_shaping_expected.py` regenerates it.
//! rustybuzz rounds the anchor deltas half away from zero, where
//! HarfBuzz rounds halves up, so it is not the reference here.
//!
//! Clusters are left out of the comparison: HarfBuzz merges a mark's
//! cluster into its base's, which sigilbuzz does not do yet.

use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const RUBIK: &[u8] = include_bytes!("fixtures/rubik_vf.ttf");
const EXPECTED: &str = include_str!("fixtures/rubik_variable_shaping.expected");

/// `(glyph_id, x_advance, y_advance, x_offset, y_offset)`.
type Pos = (u32, i32, i32, i32, i32);

/// Marked text Rubik covers, with the direction to shape it in.
/// The Latin and Cyrillic pairs have no precomposed form, so neither
/// engine's normalizer changes the glyph stream.
const CASES: &[(&str, bool)] = &[
    // q + acute, x + diaeresis, k + circumflex.
    ("q\u{0301}x\u{0308}k\u{0302}", false),
    // zhe + acute, ef + diaeresis.
    ("\u{0436}\u{0301}\u{0444}\u{0308}", false),
    // bet + kamatz, shin + shin dot + kamatz, lamed, vav + holam, mem.
    (
        "\u{05D1}\u{05B8}\u{05E9}\u{05C1}\u{05B8}\u{05DC}\u{05D5}\u{05B9}\u{05DD}",
        true,
    ),
    // bereshit: dagesh, sheva, tsere, shin dot, hiriq.
    (
        "\u{05D1}\u{05BC}\u{05B0}\u{05E8}\u{05B5}\u{05D0}\u{05E9}\u{05C1}\u{05B4}\u{05D9}\u{05EA}",
        true,
    ),
];

/// Normalized (fvar + avar) coordinates for a user-space weight.
fn coords_for(face: &Face<'_>, wght: f32) -> Vec<f32> {
    let fvar = face.fvar().unwrap().expect("rubik has fvar");
    let normalized = fvar.normalize_coords(&[wght]);
    match face.avar().unwrap() {
        Some(avar) => avar.remap_all(&normalized),
        None => normalized,
    }
}

fn sigilbuzz_positions(wght: Option<f32>, text: &str, rtl: bool) -> Vec<Pos> {
    let blob = Blob::new(RUBIK);
    let face = Face::parse(&blob, 0).unwrap();
    let coords = wght.map(|w| coords_for(&face, w)).unwrap_or_default();
    let font = Font::new(face, 1000.0).with_coords(&coords);
    let mut buffer = Buffer::new();
    buffer.set_direction(if rtl { Direction::Rtl } else { Direction::Ltr });
    buffer.push_str(text);
    let run = shape(&font, &buffer, &[]).unwrap();
    run.glyphs
        .iter()
        .map(|g| (g.glyph_id, g.x_advance, g.y_advance, g.x_offset, g.y_offset))
        .collect()
}

/// HarfBuzz's positions for `text` at `wght`, from the expected file.
fn harfbuzz_positions(wght: f32, text: &str, rtl: bool) -> Vec<Pos> {
    let cps: Vec<String> = text.chars().map(|c| format!("{:04X}", c as u32)).collect();
    let key = format!(
        "anchors {wght} {} {}",
        if rtl { "rtl" } else { "ltr" },
        cps.join(",")
    );
    let line = EXPECTED
        .lines()
        .find(|l| {
            l.strip_prefix(&key)
                .is_some_and(|rest| rest.starts_with(' '))
        })
        .unwrap_or_else(|| panic!("no HarfBuzz record for {key}"));
    line[key.len()..]
        .split_whitespace()
        .map(|g| {
            let v: Vec<i64> = g.split(',').map(|n| n.parse().unwrap()).collect();
            (
                v[0] as u32,
                v[1] as i32,
                v[2] as i32,
                v[3] as i32,
                v[4] as i32,
            )
        })
        .collect()
}

#[test]
fn marks_match_harfbuzz_across_the_weight_axis() {
    for &wght in &[300.0f32, 450.0, 600.0, 750.0, 900.0] {
        for &(text, rtl) in CASES {
            assert_eq!(
                sigilbuzz_positions(Some(wght), text, rtl),
                harfbuzz_positions(wght, text, rtl),
                "wght={wght} rtl={rtl} {text:?}"
            );
        }
    }
}

#[test]
fn anchor_deltas_actually_move_the_marks() {
    // In an RTL run a mark's x offset is base anchor minus mark anchor
    // plus the mark's own (zero) advance: no base advance enters it. A
    // change between weights therefore comes from the anchors' device
    // deltas alone, not from HVAR moving the base.
    let text = "\u{05D1}\u{05B8}";
    let light = sigilbuzz_positions(Some(300.0), text, true);
    let heavy = sigilbuzz_positions(Some(900.0), text, true);
    let (light_mark, heavy_mark) = (light[0], heavy[0]);
    assert_eq!(light_mark.0, heavy_mark.0, "same mark glyph");
    assert_eq!(light_mark.1, 0);
    assert_ne!(
        light_mark.3, heavy_mark.3,
        "kamatz x offset must follow the weight axis"
    );
}

#[test]
fn default_coords_equal_the_static_instance() {
    // wght=300 is Rubik's default: normalized coords are all zero, so
    // every anchor delta is zero and the output equals shaping without
    // coords at all.
    for &(text, rtl) in CASES {
        assert_eq!(
            sigilbuzz_positions(Some(300.0), text, rtl),
            sigilbuzz_positions(None, text, rtl),
            "{text:?}"
        );
    }
}
