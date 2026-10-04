//! Script runs from the Unicode Script property, leading marks with no
//! script, and symbols in an Arabic `stch` word, against HarfBuzz.
//!
//! Every expectation here is HarfBuzz 14.5.0's output (uharfbuzz
//! 0.56.2, `hb.shape` with no features, script and direction guessed,
//! then the direction given if any, HarfBuzz's default cluster level):
//! glyph id, cluster (UTF-8 byte offset), advance along the direction,
//! and x and y offset. The fonts are the fixtures in `tests/fixtures/`
//! and the Noto fonts in `tests/fonts/` (see the READMEs there).
//!
//! Before 0.24.0 the older `UnicodeScript` buckets (Latin through Modi)
//! took the code points of their Unicode blocks, so a text whose first
//! letter sat outside those blocks shaped as `Other` under `DFLT`:
//! Latin Extended Additional lost the `latn` kerning, Arabic
//! Extended-A and the Arabic mathematical letters shaped left to right
//! with no joining, and the Mongolian Supplement, Devanagari Extended,
//! and Sinhala Archaic Numbers lost their shapers. Text with no
//! script-bearing character took the direction of its first strong
//! character, so Arabic marks before a tatweel were reversed, where
//! HarfBuzz's invalid script is left to right. And the `stch` stretch
//! ended its word at a symbol, which HarfBuzz counts into the word.

use sigilbuzz::{script_of, shape, Blob, Buffer, Direction, Face, Font, UnicodeScript};

const RUBIK: &[u8] = include_bytes!("fixtures/rubik_vf.ttf");
const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");
const MONGOLIAN: &[u8] = include_bytes!("fonts/NotoSansMongolian-Regular.ttf");
const DEVANAGARI: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Regular.ttf");
const SINHALA: &[u8] = include_bytes!("fonts/NotoSansSinhala-Regular.ttf");
const SYRIAC: &[u8] = include_bytes!("fonts/NotoSansSyriac-Subset.ttf");

/// Glyph id, cluster, advance along the direction, x offset, y offset.
type Row = (u32, u32, i32, i32, i32);

/// One string, the direction set after guessing (`None` keeps the
/// guessed one), and HarfBuzz's glyphs.
struct Case {
    font: &'static [u8],
    text: &'static str,
    direction: Option<Direction>,
    expected: &'static [Row],
}

fn shape_text(font: &[u8], text: &str, direction: Option<Direction>) -> Vec<Row> {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    if let Some(direction) = direction {
        buffer.set_direction(direction);
    }
    let vertical = !buffer.direction().is_horizontal();
    shape(&font, &buffer, &[])
        .expect("shape")
        .glyphs
        .iter()
        .map(|g| {
            let advance = if vertical { g.y_advance } else { g.x_advance };
            (g.glyph_id, g.cluster, advance, g.x_offset, g.y_offset)
        })
        .collect()
}

fn check(cases: &[Case]) {
    for case in cases {
        let got = shape_text(case.font, case.text, case.direction);
        assert_eq!(got, case.expected, "{:?}", case.text);
    }
}

#[test]
fn latin_outside_the_basic_blocks_gets_latin_features() {
    // A first letter from Latin Extended Additional (Vietnamese and
    // Welsh), and the fl ligature from Alphabetic Presentation Forms:
    // the run is Latin, so `kern` under `latn` applies.
    for c in ['\u{1E85}', '\u{1EF3}', '\u{FB02}'] {
        assert_eq!(script_of(c), UnicodeScript::Latin, "{c:?}");
    }
    check(&[
        Case {
            font: RUBIK,
            text: "\u{1E85} fi",
            direction: None,
            expected: &[
                (249, 0, 763, 0, 0),
                (928, 3, 238, 0, 0),
                (264, 4, 495, 0, 0),
            ],
        },
        Case {
            font: RUBIK,
            text: "\u{1E85}AVa",
            direction: None,
            expected: &[
                (249, 0, 768, 0, 0),
                (1, 3, 604, 0, 0),
                (113, 4, 583, 0, 0),
                (129, 5, 536, 0, 0),
            ],
        },
        Case {
            font: RUBIK,
            text: "\u{1EF3} AV",
            direction: None,
            expected: &[
                (256, 0, 505, 0, 0),
                (928, 3, 231, 0, 0),
                (1, 4, 604, 0, 0),
                (113, 5, 638, 0, 0),
            ],
        },
        Case {
            font: RUBIK,
            text: "\u{FB02} AV",
            direction: None,
            expected: &[
                (265, 0, 555, 0, 0),
                (928, 3, 231, 0, 0),
                (1, 4, 604, 0, 0),
                (113, 5, 638, 0, 0),
            ],
        },
    ]);
}

#[test]
fn arabic_extended_a_and_the_math_letters_join_right_to_left() {
    // Beh with three dots below (Arabic Extended-A) and the
    // mathematical initial jeem first: the buffer is Arabic, so it is
    // right to left and the letters join.
    assert_eq!(script_of('\u{08A0}'), UnicodeScript::Arabic);
    assert_eq!(script_of('\u{1EE06}'), UnicodeScript::Arabic);
    check(&[
        Case {
            font: AMIRI,
            text: "\u{08A0}\u{0628}\u{0644}",
            direction: None,
            expected: &[
                (1837, 5, 644, 0, 0),
                (2570, 3, 168, 0, 0),
                (5723, 0, 247, 0, 0),
            ],
        },
        Case {
            font: AMIRI,
            text: "\u{0628}\u{08A0}\u{0628}",
            direction: None,
            expected: &[
                (1589, 5, 883, 0, 0),
                (5735, 2, 244, 0, 0),
                (3958, 0, 233, 0, 0),
            ],
        },
        Case {
            font: AMIRI,
            text: "\u{1EE06} \u{0628}\u{0644}",
            direction: None,
            expected: &[
                (1837, 7, 644, 0, 0),
                (1611, 5, 190, 0, 0),
                (1, 4, 292, 0, 0),
                (1067, 0, 537, 0, 0),
            ],
        },
    ]);
}

#[test]
fn the_mongolian_supplement_takes_the_mongolian_forms() {
    // A Mongolian Supplement birga first: the letters after it take
    // their joining forms, horizontally and vertically.
    assert_eq!(script_of('\u{11662}'), UnicodeScript::Mongolian);
    check(&[
        Case {
            font: MONGOLIAN,
            text: "\u{11662}\u{1855}\u{1800}",
            direction: None,
            expected: &[
                (1526, 0, 656, 0, 0),
                (325, 4, 496, 0, 0),
                (1523, 7, 534, 0, 0),
            ],
        },
        Case {
            font: MONGOLIAN,
            text: "\u{11662}\u{1855}\u{1800}",
            direction: Some(Direction::Ttb),
            expected: &[
                (1526, 0, -1000, -328, -880),
                (325, 4, -1000, -248, -880),
                (1523, 7, -1000, -267, -880),
            ],
        },
        Case {
            font: MONGOLIAN,
            text: "\u{11667} \u{185F}\u{1891}\u{1834}",
            direction: None,
            expected: &[
                (1531, 0, 534, 0, 0),
                (1597, 4, 260, 0, 0),
                (384, 5, 353, 0, 0),
                (579, 8, 507, 0, 0),
                (159, 11, 780, 0, 0),
            ],
        },
    ]);
}

#[test]
fn devanagari_and_sinhala_extensions_take_their_shapers() {
    // A combining Devanagari digit (Devanagari Extended) with no base
    // gets a dotted circle from the Indic shaper; a Devanagari
    // Extended-A head mark first leaves the next syllable to the Indic
    // shaper; a Sinhala archaic number first leaves the split vowel
    // signs after it to the Universal Shaping Engine.
    assert_eq!(script_of('\u{A8E1}'), UnicodeScript::Devanagari);
    assert_eq!(script_of('\u{11B04}'), UnicodeScript::Devanagari);
    assert_eq!(script_of('\u{111E2}'), UnicodeScript::Sinhala);
    check(&[
        Case {
            font: DEVANAGARI,
            text: "\u{A8E1}\u{090D}\u{097F}",
            direction: None,
            expected: &[
                (789, 0, 510, 0, 0),
                (182, 0, 0, 0, -347),
                (20, 3, 553, 0, 0),
                (95, 6, 571, 0, 0),
            ],
        },
        Case {
            font: DEVANAGARI,
            text: "\u{11B04} \u{0918}\u{0906}\u{0908}",
            direction: None,
            expected: &[
                (215, 0, 717, 0, 0),
                (3, 4, 260, 0, 0),
                (59, 5, 591, 0, 0),
                (6, 8, 1022, 0, 0),
                (7, 11, 491, 0, 0),
                (506, 11, 0, 2, 0),
            ],
        },
        Case {
            font: SINHALA,
            text: "\u{111E2}\u{0DEA}\u{0DDA}",
            direction: None,
            expected: &[
                (92, 0, 1160, 0, 0),
                (74, 4, 631, 0, 0),
                (85, 4, 790, 0, 0),
                (65, 4, 0, 0, 0),
            ],
        },
        Case {
            font: SINHALA,
            text: "\u{111EB}\u{0D8E}\u{0DDB}",
            direction: None,
            expected: &[
                (101, 0, 974, 0, 0),
                (76, 4, 1262, 0, 0),
                (15, 4, 1705, 0, 0),
            ],
        },
    ]);
}

#[test]
fn leading_arabic_marks_before_a_tatweel_sort_left_to_right() {
    // Damma and fathatan, or kasra and shadda, then a tatweel: every
    // character is Common or Inherited, so HarfBuzz's buffer keeps an
    // invalid script, which is left to right, and the marks sort by
    // combining class (fathatan before damma, shadda before kasra).
    // An explicit right-to-left direction reverses the graphemes.
    assert_eq!(script_of('\u{0640}'), UnicodeScript::Other);
    check(&[
        Case {
            font: AMIRI,
            text: "\u{064F}\u{064B}\u{0640}",
            direction: None,
            expected: &[(1438, 0, 0, 0, 0), (1441, 0, 0, 0, 0), (5477, 4, 185, 0, 0)],
        },
        Case {
            font: AMIRI,
            text: "\u{0650}\u{0651}\u{0640}",
            direction: None,
            expected: &[(97, 0, 0, 0, 0), (96, 0, 0, 0, 0), (5477, 4, 185, 0, 0)],
        },
        Case {
            font: AMIRI,
            text: "\u{064F}\u{064B}\u{0640}",
            direction: Some(Direction::Rtl),
            expected: &[
                (5477, 4, 185, 0, 0),
                (1438, 0, 0, 0, 0),
                (1441, 0, 0, -92, -112),
            ],
        },
    ]);
}

#[test]
fn a_symbol_counts_into_the_stch_word() {
    // The abbreviation mark stretches over the rest of its word. A
    // math (Sm) or currency (Sc) symbol in the word counts into its
    // width, so the two repeating tiles (55) come 4 times each; an
    // exclamation mark (Po) ends the word, and they come once each.
    // The font has no glyph for the symbols: their .notdef counts all
    // the same.
    check(&[
        Case {
            font: SYRIAC,
            text: "\u{070F}\u{0712}+\u{0713}\u{0715}",
            direction: Some(Direction::Rtl),
            expected: &[
                (18, 7, 525, 0, 0),
                (16, 5, 718, 0, 0),
                (0, 4, 600, 0, 0),
                (9, 2, 958, 0, 0),
                (53, 0, 0, -2804, 0),
                (55, 0, 0, -2478, 0),
                (55, 0, 0, -2276, 0),
                (55, 0, 0, -2074, 0),
                (55, 0, 0, -1872, 0),
                (54, 0, 0, -1637, 0),
                (55, 0, 0, -1167, 0),
                (55, 0, 0, -965, 0),
                (55, 0, 0, -763, 0),
                (55, 0, 0, -561, 0),
                (52, 0, 0, -326, 0),
            ],
        },
        Case {
            font: SYRIAC,
            text: "\u{070F}\u{0712}\u{20AC}\u{0713}\u{0715}",
            direction: Some(Direction::Rtl),
            expected: &[
                (18, 9, 525, 0, 0),
                (16, 7, 718, 0, 0),
                (0, 4, 600, 0, 0),
                (9, 2, 958, 0, 0),
                (53, 0, 0, -2804, 0),
                (55, 0, 0, -2478, 0),
                (55, 0, 0, -2276, 0),
                (55, 0, 0, -2074, 0),
                (55, 0, 0, -1872, 0),
                (54, 0, 0, -1637, 0),
                (55, 0, 0, -1167, 0),
                (55, 0, 0, -965, 0),
                (55, 0, 0, -763, 0),
                (55, 0, 0, -561, 0),
                (52, 0, 0, -326, 0),
            ],
        },
        Case {
            font: SYRIAC,
            text: "\u{070F}\u{0712}!\u{0713}\u{0715}",
            direction: Some(Direction::Rtl),
            expected: &[
                (18, 7, 525, 0, 0),
                (16, 5, 718, 0, 0),
                (0, 4, 600, 0, 0),
                (9, 2, 958, 0, 0),
                (53, 0, 0, -1674, 0),
                (55, 0, 0, -1348, 0),
                (54, 0, 0, -1113, 0),
                (55, 0, 0, -643, 0),
                (52, 0, 0, -408, 0),
            ],
        },
    ]);
}
