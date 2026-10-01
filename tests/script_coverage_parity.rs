//! Scripts that HarfBuzz gives a shaper of its own, against HarfBuzz.
//!
//! Every expectation here is HarfBuzz 14.5.0's output (uharfbuzz 0.56.2,
//! `hb.shape` with no features, the direction given, script and language
//! guessed, monotone grapheme clusters): glyph id, cluster, x advance,
//! x and y offset, and glyph flags. The fonts are the subsets of Noto
//! fonts in `tests/fonts/` (see the README there).
//!
//! `hb_ot_shaper_categorize` (`hb-ot-shaper.hh`) sends Syriac to the
//! Arabic shaper (`hb-ot-shaper-arabic.cc`) and Javanese, Chakma,
//! Khudawadi, Takri, Adlam, and the other scripts of its list to the
//! Universal Shaping Engine (`hb-ot-shaper-use.cc`). sigilbuzz had no
//! `UnicodeScript` for them before and shaped them with the default
//! shaper, so they got no syllables, no reordering, no joining forms,
//! and no vowel constraints.

use std::time::{Duration, Instant};

use sigilbuzz::{
    script_of, shape, Blob, Buffer, ClusterLevel, Direction, Face, Font, UnicodeScript,
};

const JAVANESE: &[u8] = include_bytes!("fonts/NotoSansJavanese-Subset.ttf");
const CHAKMA: &[u8] = include_bytes!("fonts/NotoSansChakma-Subset.ttf");
const KHUDAWADI: &[u8] = include_bytes!("fonts/NotoSansKhudawadi-Subset.ttf");
const TAKRI: &[u8] = include_bytes!("fonts/NotoSansTakri-Subset.ttf");
const SYRIAC: &[u8] = include_bytes!("fonts/NotoSansSyriac-Subset.ttf");
const ADLAM: &[u8] = include_bytes!("fonts/NotoSansAdlam-Subset.ttf");

/// One string, its direction, and HarfBuzz's glyph id, cluster, x
/// advance, x offset, y offset, and glyph flags for each glyph.
struct Case {
    font: &'static [u8],
    text: &'static str,
    direction: Direction,
    expected: &'static [(u32, u32, i32, i32, i32, u32)],
}

fn shape_text(
    font: &[u8],
    text: &str,
    direction: Direction,
) -> Vec<(u32, u32, i32, i32, i32, u32)> {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    buffer.set_cluster_level(ClusterLevel::MonotoneGraphemes);
    shape(&font, &buffer, &[])
        .expect("shape")
        .glyphs
        .iter()
        .map(|g| {
            (
                g.glyph_id,
                g.cluster,
                g.x_advance,
                g.x_offset,
                g.y_offset,
                g.flags.bits(),
            )
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
fn javanese_shapes_with_the_universal_shaping_engine() {
    // Ka with cakra, which the font's `pref` substitutes in place, so
    // the cakra is a pre-base glyph that moves (`record_pref_use`). Then
    // ka with the pre-base vowel sign taling, ka with pangkon and ta, a
    // taling with no base, which gets a dotted circle, and a word.
    assert_eq!(script_of('\u{A98F}'), UnicodeScript::Javanese);
    check(&[
        Case {
            font: JAVANESE,
            text: "\u{A98F}\u{A9BF}",
            direction: Direction::Ltr,
            expected: &[(40, 0, 1465, 0, 0, 0)],
        },
        Case {
            font: JAVANESE,
            text: "\u{A98F}\u{A9BA}",
            direction: Direction::Ltr,
            expected: &[(24, 0, 677, 0, 0, 0), (10, 0, 1221, 0, 0, 0)],
        },
        Case {
            font: JAVANESE,
            text: "\u{A98F}\u{A9C0}\u{A9A0}",
            direction: Direction::Ltr,
            expected: &[(10, 0, 1221, 0, 0, 0), (67, 0, 0, -1, 0, 0)],
        },
        Case {
            font: JAVANESE,
            text: "\u{A9BA}",
            direction: Direction::Ltr,
            expected: &[(24, 0, 677, 0, 0, 0), (4, 0, 594, 0, 0, 0)],
        },
        Case {
            font: JAVANESE,
            text: "\u{A986}\u{A9BF}\u{A9A1}",
            direction: Direction::Ltr,
            expected: &[
                (36, 0, 238, 0, 0, 0),
                (9, 0, 1207, 0, 0, 0),
                (12, 6, 967, 0, 0, 0),
            ],
        },
        Case {
            font: JAVANESE,
            text: "\u{A9B2}\u{A9BA}\u{A9B4}\u{A9AB}\u{A981}",
            direction: Direction::Ltr,
            expected: &[
                (24, 0, 677, 0, 0, 0),
                (16, 0, 1195, 0, 0, 0),
                (17, 0, 413, 0, 0, 0),
                (14, 9, 915, 0, 0, 0),
                (6, 9, 0, -321, 10, 0),
            ],
        },
    ]);
}

#[test]
fn chakma_shapes_with_the_universal_shaping_engine() {
    // Ka with virama and ka, ka with maayyaa (the invisible stacker)
    // and ta, ka with the pre-base vowel sign e, a vowel sign with no
    // base, and a word.
    check(&[
        Case {
            font: CHAKMA,
            text: "\u{11107}\u{11133}\u{11107}",
            direction: Direction::Ltr,
            expected: &[(19, 0, 988, 0, 0, 0), (46, 0, 0, -256, 0, 0)],
        },
        Case {
            font: CHAKMA,
            text: "\u{11107}\u{11134}\u{11116}",
            direction: Direction::Ltr,
            expected: &[
                (19, 0, 988, 0, 0, 0),
                (44, 0, 0, -271, -60, 0),
                (21, 8, 1045, 0, 0, 0),
            ],
        },
        Case {
            font: CHAKMA,
            text: "\u{11107}\u{1112C}",
            direction: Direction::Ltr,
            expected: &[(27, 0, 379, 0, 0, 0), (19, 0, 988, 0, 0, 0)],
        },
        Case {
            font: CHAKMA,
            text: "\u{11127}",
            direction: Direction::Ltr,
            expected: &[(4, 0, 599, 0, 0, 0), (25, 0, 0, 204, -74, 0)],
        },
        Case {
            font: CHAKMA,
            text: "\u{11103}\u{11107}\u{11127}\u{11100}",
            direction: Direction::Ltr,
            expected: &[
                (18, 0, 1021, 0, 0, 0),
                (19, 4, 988, 0, 0, 0),
                (25, 4, 0, 100, -127, 0),
                (5, 4, 0, -275, -145, 0),
            ],
        },
    ]);
}

#[test]
fn khudawadi_and_takri_get_their_vowel_constraints() {
    // `hb-ot-shaper-vowel-constraints.cc` lists Khudawadi a with vowel
    // sign aa, and Takri a with vowel sign aa and u with vowel sign u:
    // HarfBuzz puts a dotted circle before the sign. The others are a
    // vowel sign, a virama conjunct, a nukta, and Takri's pre-base sign
    // i.
    check(&[
        Case {
            font: KHUDAWADI,
            text: "\u{112B0}\u{112E0}",
            direction: Direction::Ltr,
            expected: &[
                (5, 0, 996, 0, 0, 0),
                (4, 0, 594, 0, 0, 0),
                (11, 0, 263, 0, 0, 0),
            ],
        },
        Case {
            font: KHUDAWADI,
            text: "\u{112BA}\u{112E1}",
            direction: Direction::Ltr,
            expected: &[(12, 0, 262, 0, 0, 0), (6, 0, 767, 0, 0, 0)],
        },
        Case {
            font: KHUDAWADI,
            text: "\u{112BA}\u{112EA}\u{112C0}",
            direction: Direction::Ltr,
            expected: &[
                (6, 0, 767, 0, 0, 0),
                (16, 0, 0, -377, 0, 0),
                (7, 8, 578, 0, 0, 0),
            ],
        },
        Case {
            font: KHUDAWADI,
            text: "\u{112BA}\u{112E9}",
            direction: Direction::Ltr,
            expected: &[(6, 0, 767, 0, 0, 0), (15, 0, 0, -377, -40, 0)],
        },
        Case {
            font: TAKRI,
            text: "\u{11680}\u{116AD}",
            direction: Direction::Ltr,
            expected: &[
                (5, 0, 703, 0, 0, 0),
                (2, 0, 594, 0, 0, 0),
                (12, 0, 0, -59, -36, 0),
            ],
        },
        Case {
            font: TAKRI,
            text: "\u{11686}\u{116B2}",
            direction: Direction::Ltr,
            expected: &[
                (6, 0, 439, 0, 0, 0),
                (2, 0, 594, 0, 0, 0),
                (15, 0, 0, -297, 0, 0),
            ],
        },
        Case {
            font: TAKRI,
            text: "\u{1168A}\u{116AE}",
            direction: Direction::Ltr,
            expected: &[(14, 0, 304, 0, 0, 0), (7, 0, 830, 107, 0, 0)],
        },
        Case {
            font: TAKRI,
            text: "\u{1168A}\u{116B6}\u{116A2}",
            direction: Direction::Ltr,
            expected: &[
                (7, 0, 723, 0, 0, 0),
                (17, 0, 0, -166, 0, 0),
                (9, 8, 659, 0, 0, 1),
            ],
        },
    ]);
}

#[test]
fn syriac_takes_the_arabic_shaper_joining_forms() {
    // Alaph after a joining letter is final, after waw it takes `fin2`,
    // after dalath `fin3`, and between two letters `med2` (HarfBuzz's
    // `arabic_state_table`). The abbreviation mark stretches over the
    // rest of its word with the font's `stch` tiles (`apply_stch`).
    // The tatweel, Common in `Scripts.txt`, stays in the Syriac run.
    check(&[
        Case {
            font: SYRIAC,
            text: "\u{0712}\u{0710}",
            direction: Direction::Rtl,
            expected: &[(45, 2, 986, 0, 0, 1), (12, 0, 655, -75, 0, 0)],
        },
        Case {
            font: SYRIAC,
            text: "\u{0718}\u{0710}",
            direction: Direction::Rtl,
            expected: &[(6, 2, 930, 0, 0, 0), (19, 0, 610, 0, 0, 0)],
        },
        Case {
            font: SYRIAC,
            text: "\u{0715}\u{0710}",
            direction: Direction::Rtl,
            expected: &[(5, 2, 930, 0, 0, 0), (17, 0, 539, 0, 0, 0)],
        },
        Case {
            font: SYRIAC,
            text: "\u{0712}\u{0710}\u{0712}",
            direction: Direction::Rtl,
            expected: &[
                (9, 4, 958, 0, 0, 1),
                (47, 2, 986, 0, 0, 1),
                (12, 0, 655, -75, 0, 0),
            ],
        },
        Case {
            font: SYRIAC,
            text: "\u{070F}\u{0712}\u{0713}\u{0715}",
            direction: Direction::Rtl,
            expected: &[
                (18, 6, 525, 0, 0, 1),
                (15, 4, 571, 0, 0, 1),
                (12, 2, 655, -75, 0, 1),
                (53, 0, 0, -1752, 0, 0),
                (55, 0, 0, -1426, 0, 0),
                (55, 0, 0, -1346, 0, 0),
                (54, 0, 0, -1111, 0, 0),
                (55, 0, 0, -641, 0, 0),
                (55, 0, 0, -561, 0, 0),
                (52, 0, 0, -326, 0, 0),
            ],
        },
        Case {
            font: SYRIAC,
            text: "\u{0712}\u{0730}\u{0713}",
            direction: Direction::Rtl,
            expected: &[
                (14, 4, 945, 0, 0, 1),
                (37, 0, 0, 266, -23, 0),
                (12, 0, 655, -75, 0, 0),
            ],
        },
        Case {
            font: SYRIAC,
            text: "\u{0640}\u{0712}",
            direction: Direction::Rtl,
            expected: &[(10, 2, 968, 0, 0, 1), (35, 0, 379, 0, 0, 0)],
        },
        Case {
            font: SYRIAC,
            text: "\u{0712}\u{0640}\u{0712}",
            direction: Direction::Rtl,
            expected: &[
                (10, 4, 968, 0, 0, 1),
                (35, 2, 379, 0, 0, 1),
                (12, 0, 730, 0, 0, 0),
            ],
        },
    ]);
}

#[test]
fn adlam_joins_in_the_universal_shaping_engine() {
    // Adlam is right to left and joins like Arabic, and HarfBuzz gives
    // it the Universal Shaping Engine with Arabic-style joining forms
    // (`has_arabic_joining`).
    check(&[
        Case {
            font: ADLAM,
            text: "\u{1E900}\u{1E902}\u{1E904}",
            direction: Direction::Rtl,
            expected: &[
                (6, 8, 768, 0, 0, 1),
                (12, 4, 816, 0, 0, 1),
                (3, 0, 715, 0, 0, 0),
            ],
        },
        Case {
            font: ADLAM,
            text: "\u{1E922}\u{1E944}\u{1E924}",
            direction: Direction::Rtl,
            expected: &[
                (33, 8, 606, 0, 0, 1),
                (17, 0, 0, 90, -15, 0),
                (15, 0, 669, 0, 0, 0),
            ],
        },
        Case {
            font: ADLAM,
            text: "\u{1E900}\u{1E94B}",
            direction: Direction::Rtl,
            expected: &[(19, 4, 142, 0, 0, 0), (1, 0, 715, 0, 0, 0)],
        },
    ]);
}

#[test]
fn long_runs_of_the_new_scripts_stay_fast() {
    // Every Syriac abbreviation mark stretches over its word, and each
    // Javanese syllable reorders. Both stay linear in the text length.
    let syriac = "\u{070F}\u{0712}\u{0713}\u{0715} ".repeat(4_000);
    let javanese = "\u{A98F}\u{A9BA}\u{A9BF}\u{A9C0}\u{A9A0} ".repeat(4_000);
    let start = Instant::now();
    let glyphs = shape_text(SYRIAC, &syriac, Direction::Rtl);
    assert!(glyphs.len() >= 20_000);
    let glyphs = shape_text(JAVANESE, &javanese, Direction::Ltr);
    assert!(glyphs.len() >= 20_000);
    assert!(start.elapsed() < Duration::from_secs(20));
}
