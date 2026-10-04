//! Hangul in a buffer of another script, and other scripts in a Hangul
//! buffer.
//!
//! HarfBuzz shapes a buffer with the one shaper its script picks
//! (`hb_ot_shaper_categorize`), so Hangul text in a Latin or Han buffer
//! goes through the default shaper: the whole buffer normalizes with it
//! (a syllable followed by a mark decomposes into jamo), and the Hangul
//! preprocessing and jamo features do not run. In a Hangul buffer the
//! Hangul shaper normalizes everything with its own mode, which composes
//! nothing, and applies `calt` to every glyph but jamo
//! (`override_features_hangul` and `setup_masks_hangul` in
//! `hb-ot-shaper-hangul.cc`). A Hangul tone mark after another script's
//! letter normalizes and clusters with that letter.
//!
//! Every expectation here is HarfBuzz 14.5.0's output (uharfbuzz 0.56.2,
//! `hb.shape` with no features, LTR, UTF-8 clusters, script guessed
//! unless the case sets it): glyph id, cluster, glyph flags, x advance,
//! x offset, y offset, at the three cluster levels.
//! `NotoSansKR-Calt-Subset.ttf` adds a `calt` lookup that turns `a` into
//! `c` and U+1100 into U+1101 (see `tests/fonts/README.md`).

use sigilbuzz::{shape, Blob, Buffer, ClusterLevel, Direction, Face, Feature, Font, UnicodeScript};

const CALT: &[u8] = include_bytes!("fonts/NotoSansKR-Calt-Subset.ttf");
const TONE: &[u8] = include_bytes!("fonts/NotoSansKR-HangulTone-Subset.ttf");
const OLD_HANGUL: &[u8] = include_bytes!("fonts/NotoSansOldHangul-Subset.ttf");

/// Glyph id, cluster, glyph flags, x advance, x offset, y offset.
type Row = (u32, u32, u32, i32, i32, i32);

const LEVELS: [ClusterLevel; 3] = [
    ClusterLevel::MonotoneGraphemes,
    ClusterLevel::MonotoneCharacters,
    ClusterLevel::Characters,
];

fn shape_rows(
    font_data: &[u8],
    text: &str,
    script: Option<UnicodeScript>,
    level: ClusterLevel,
) -> Vec<Row> {
    let blob = Blob::new(font_data);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(Direction::Ltr);
    buffer.set_cluster_level(level);
    buffer.set_script(script);
    shape(&font, &buffer, &[])
        .expect("shape")
        .glyphs
        .iter()
        .map(|g| {
            (
                g.glyph_id,
                g.cluster,
                g.flags.bits(),
                g.x_advance,
                g.x_offset,
                g.y_offset,
            )
        })
        .collect()
}

struct Case {
    font: &'static [u8],
    text: &'static str,
    script: Option<UnicodeScript>,
    /// At the three levels of [`LEVELS`].
    expected: [&'static [Row]; 3],
}

#[test]
fn hangul_mixed_with_other_scripts_shapes_as_in_harfbuzz() {
    let cases = [
        // A Hangul buffer applies `calt` to the Latin letter.
        Case {
            font: CALT,
            text: "\u{AC00}a",
            script: None,
            expected: [
                &[(14, 0, 0, 920, 0, 0), (4, 3, 0, 510, 0, 0)],
                &[(14, 0, 0, 920, 0, 0), (4, 3, 0, 510, 0, 0)],
                &[(14, 0, 0, 920, 0, 0), (4, 3, 0, 510, 0, 0)],
            ],
        },
        // A Hangul buffer normalizes with the Hangul shaper, which composes nothing.
        Case {
            font: CALT,
            text: "\u{AC00} a\u{0301}",
            script: None,
            expected: [
                &[
                    (14, 0, 0, 920, 0, 0),
                    (1, 3, 0, 224, 0, 0),
                    (4, 4, 0, 510, 0, 0),
                    (6, 4, 0, 0, -555, 224),
                ],
                &[
                    (14, 0, 0, 920, 0, 0),
                    (1, 3, 0, 224, 0, 0),
                    (4, 4, 0, 510, 0, 0),
                    (6, 5, 1, 0, -555, 224),
                ],
                &[
                    (14, 0, 0, 920, 0, 0),
                    (1, 3, 0, 224, 0, 0),
                    (4, 4, 0, 510, 0, 0),
                    (6, 5, 1, 0, -555, 224),
                ],
            ],
        },
        // but keeps `calt` off jamo.
        Case {
            font: CALT,
            text: "\u{AC00}\u{1100}",
            script: None,
            expected: [
                &[(14, 0, 0, 920, 0, 0), (7, 3, 0, 920, 0, 0)],
                &[(14, 0, 0, 920, 0, 0), (7, 3, 0, 920, 0, 0)],
                &[(14, 0, 0, 920, 0, 0), (7, 3, 0, 920, 0, 0)],
            ],
        },
        // A Latin buffer applies `calt` to jamo too.
        Case {
            font: CALT,
            text: "a\u{1100}",
            script: None,
            expected: [
                &[(4, 0, 0, 510, 0, 0), (8, 1, 0, 920, 0, 0)],
                &[(4, 0, 0, 510, 0, 0), (8, 1, 0, 920, 0, 0)],
                &[(4, 0, 0, 510, 0, 0), (8, 1, 0, 920, 0, 0)],
            ],
        },
        // A Latin buffer decomposes a syllable with a tone mark and moves nothing.
        Case {
            font: CALT,
            text: "a \u{AC00}\u{302E}",
            script: None,
            expected: [
                &[
                    (4, 0, 0, 510, 0, 0),
                    (1, 1, 0, 224, 0, 0),
                    (8, 2, 0, 920, 0, 0),
                    (9, 2, 0, 920, 0, 0),
                    (12, 2, 0, 0, -585, 0),
                ],
                &[
                    (4, 0, 0, 510, 0, 0),
                    (1, 1, 0, 224, 0, 0),
                    (8, 2, 0, 920, 0, 0),
                    (9, 2, 0, 920, 0, 0),
                    (12, 5, 1, 0, -585, 0),
                ],
                &[
                    (4, 0, 0, 510, 0, 0),
                    (1, 1, 0, 224, 0, 0),
                    (8, 2, 0, 920, 0, 0),
                    (9, 2, 0, 920, 0, 0),
                    (12, 5, 1, 0, -585, 0),
                ],
            ],
        },
        // So does a Han buffer.
        Case {
            font: CALT,
            text: "\u{4E2D}\u{AC00}\u{302E}",
            script: None,
            expected: [
                &[
                    (13, 0, 0, 1000, 0, 0),
                    (8, 3, 0, 920, 0, 0),
                    (9, 3, 0, 920, 0, 0),
                    (12, 3, 0, 0, -585, 0),
                ],
                &[
                    (13, 0, 0, 1000, 0, 0),
                    (8, 3, 0, 920, 0, 0),
                    (9, 3, 0, 920, 0, 0),
                    (12, 6, 1, 0, -585, 0),
                ],
                &[
                    (13, 0, 0, 1000, 0, 0),
                    (8, 3, 0, 920, 0, 0),
                    (9, 3, 0, 920, 0, 0),
                    (12, 6, 1, 0, -585, 0),
                ],
            ],
        },
        // The syllable and tone mark of a Latin buffer.
        Case {
            font: TONE,
            text: "Hi \u{AC00}\u{302E}",
            script: None,
            expected: [
                &[
                    (0, 0, 0, 1000, 0, 0),
                    (0, 1, 0, 1000, 0, 0),
                    (1, 2, 0, 224, 0, 0),
                    (3, 3, 0, 920, 0, 0),
                    (9, 3, 0, 920, 0, 0),
                    (17, 3, 0, 250, 0, 0),
                ],
                &[
                    (0, 0, 0, 1000, 0, 0),
                    (0, 1, 0, 1000, 0, 0),
                    (1, 2, 0, 224, 0, 0),
                    (3, 3, 0, 920, 0, 0),
                    (9, 3, 0, 920, 0, 0),
                    (17, 6, 1, 250, 0, 0),
                ],
                &[
                    (0, 0, 0, 1000, 0, 0),
                    (0, 1, 0, 1000, 0, 0),
                    (1, 2, 0, 224, 0, 0),
                    (3, 3, 0, 920, 0, 0),
                    (9, 3, 0, 920, 0, 0),
                    (17, 6, 1, 250, 0, 0),
                ],
            ],
        },
        // A tone mark sorts with the marks of a Latin letter.
        Case {
            font: TONE,
            text: "e\u{0301}\u{302E}",
            script: None,
            expected: [
                &[
                    (0, 0, 0, 1000, 0, 0),
                    (17, 0, 0, 250, 0, 0),
                    (2, 0, 0, 0, 0, 0),
                ],
                &[
                    (0, 0, 0, 1000, 0, 0),
                    (17, 1, 1, 250, 0, 0),
                    (2, 1, 1, 0, 0, 0),
                ],
                &[
                    (0, 0, 0, 1000, 0, 0),
                    (17, 3, 1, 250, 0, 0),
                    (2, 1, 1, 0, 0, 0),
                ],
            ],
        },
        // In a Hangul buffer the tone mark moves.
        Case {
            font: TONE,
            text: "\u{AC00}\u{302E} Hi",
            script: None,
            expected: [
                &[
                    (17, 0, 0, 250, 0, 0),
                    (20, 0, 0, 920, 0, 0),
                    (1, 6, 0, 224, 0, 0),
                    (0, 7, 0, 1000, 0, 0),
                    (0, 8, 0, 1000, 0, 0),
                ],
                &[
                    (17, 0, 0, 250, 0, 0),
                    (20, 0, 0, 920, 0, 0),
                    (1, 6, 0, 224, 0, 0),
                    (0, 7, 0, 1000, 0, 0),
                    (0, 8, 0, 1000, 0, 0),
                ],
                &[
                    (17, 3, 1, 250, 0, 0),
                    (20, 0, 0, 920, 0, 0),
                    (1, 6, 0, 224, 0, 0),
                    (0, 7, 0, 1000, 0, 0),
                    (0, 8, 0, 1000, 0, 0),
                ],
            ],
        },
        // The script set to Latin.
        Case {
            font: TONE,
            text: "\u{AC00}\u{302E}",
            script: Some(UnicodeScript::Latin),
            expected: [
                &[
                    (3, 0, 0, 920, 0, 0),
                    (9, 0, 0, 920, 0, 0),
                    (17, 0, 0, 250, 0, 0),
                ],
                &[
                    (3, 0, 0, 920, 0, 0),
                    (9, 0, 0, 920, 0, 0),
                    (17, 3, 1, 250, 0, 0),
                ],
                &[
                    (3, 0, 0, 920, 0, 0),
                    (9, 0, 0, 920, 0, 0),
                    (17, 3, 1, 250, 0, 0),
                ],
            ],
        },
        // The script set to Hangul.
        Case {
            font: TONE,
            text: "Hi \u{AC00}\u{302E}",
            script: Some(UnicodeScript::Hangul),
            expected: [
                &[
                    (0, 0, 0, 1000, 0, 0),
                    (0, 1, 0, 1000, 0, 0),
                    (1, 2, 0, 224, 0, 0),
                    (17, 3, 0, 250, 0, 0),
                    (20, 3, 0, 920, 0, 0),
                ],
                &[
                    (0, 0, 0, 1000, 0, 0),
                    (0, 1, 0, 1000, 0, 0),
                    (1, 2, 0, 224, 0, 0),
                    (17, 3, 0, 250, 0, 0),
                    (20, 3, 0, 920, 0, 0),
                ],
                &[
                    (0, 0, 0, 1000, 0, 0),
                    (0, 1, 0, 1000, 0, 0),
                    (1, 2, 0, 224, 0, 0),
                    (17, 6, 1, 250, 0, 0),
                    (20, 3, 0, 920, 0, 0),
                ],
            ],
        },
        // A tone mark joins the cluster of a ligature.
        Case {
            font: OLD_HANGUL,
            text: "fi\u{302E}",
            script: None,
            expected: [
                &[(431, 0, 0, 524, 0, 0), (0, 0, 0, 1000, 0, 0)],
                &[(431, 0, 0, 524, 0, 0), (0, 2, 1, 1000, 0, 0)],
                &[(431, 0, 0, 524, 0, 0), (0, 2, 1, 1000, 0, 0)],
            ],
        },
        // A syllable with a mark decomposes.
        Case {
            font: OLD_HANGUL,
            text: "a\u{AC00}\u{0301}",
            script: None,
            expected: [
                &[
                    (28, 0, 0, 536, 0, 0),
                    (54, 1, 0, 920, 0, 0),
                    (151, 1, 0, 920, 0, 0),
                    (0, 1, 0, 0, 0, 0),
                ],
                &[
                    (28, 0, 0, 536, 0, 0),
                    (54, 1, 0, 920, 0, 0),
                    (151, 1, 0, 920, 0, 0),
                    (0, 4, 1, 0, 0, 0),
                ],
                &[
                    (28, 0, 0, 536, 0, 0),
                    (54, 1, 0, 920, 0, 0),
                    (151, 1, 0, 920, 0, 0),
                    (0, 4, 1, 0, 0, 0),
                ],
            ],
        },
        // Jamo get no jamo features.
        Case {
            font: OLD_HANGUL,
            text: "Hi \u{1100}\u{1161}",
            script: None,
            expected: [
                &[
                    (9, 0, 0, 698, 0, 0),
                    (36, 1, 0, 245, 0, 0),
                    (1, 2, 0, 220, 0, 0),
                    (54, 3, 0, 920, 0, 0),
                    (151, 6, 0, 920, 0, 0),
                ],
                &[
                    (9, 0, 0, 698, 0, 0),
                    (36, 1, 0, 245, 0, 0),
                    (1, 2, 0, 220, 0, 0),
                    (54, 3, 0, 920, 0, 0),
                    (151, 6, 0, 920, 0, 0),
                ],
                &[
                    (9, 0, 0, 698, 0, 0),
                    (36, 1, 0, 245, 0, 0),
                    (1, 2, 0, 220, 0, 0),
                    (54, 3, 0, 920, 0, 0),
                    (151, 6, 0, 920, 0, 0),
                ],
            ],
        },
    ];
    for case in &cases {
        for (level, expected) in LEVELS.into_iter().zip(case.expected) {
            assert_eq!(
                shape_rows(case.font, case.text, case.script, level),
                expected,
                "{:?} ({:?}) at {level:?}",
                case.text,
                case.script
            );
        }
    }
}

/// Glyph id, cluster, y advance, x offset, y offset.
type VerticalRow = (u32, u32, i32, i32, i32);

/// The [`VerticalRow`]s of `text` in vertical text with `features`.
/// With `guess`, the buffer guesses its script; without, sigilbuzz
/// splits the text into script runs.
fn vertical_rows(text: &str, guess: bool, features: &[Feature]) -> Vec<VerticalRow> {
    let blob = Blob::new(CALT);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    if guess {
        buffer.guess_segment_properties();
    }
    buffer.set_direction(Direction::Ttb);
    buffer.set_cluster_level(ClusterLevel::MonotoneGraphemes);
    shape(&font, &buffer, features)
        .expect("shape")
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.cluster, g.y_advance, g.x_offset, g.y_offset))
        .collect()
}

#[test]
fn vertical_hangul_applies_calt_when_the_caller_turns_it_on() {
    // In vertical text HarfBuzz's Hangul shaper has only the `calt` of
    // `override_features_hangul`, which has no value: it applies to no
    // glyph, unless the caller turns `calt` on. Then it applies to every
    // glyph but jamo, as in horizontal text, so `a` turns into `c`
    // (glyph 4) and U+1100 stays as it is. sigilbuzz used to ignore the
    // caller's `calt` there.
    let on = [Feature {
        tag: *b"calt",
        value: 1,
    }];
    let cases: [(&str, &[Feature], [VerticalRow; 2]); 6] = [
        (
            "\u{AC00}a",
            &[],
            [(14, 0, -1448, -460, -1099), (2, 3, -1448, -281, -996)],
        ),
        (
            "\u{AC00}a",
            &on,
            [(14, 0, -1448, -460, -1099), (4, 3, -1448, -255, -996)],
        ),
        (
            "\u{1100}a",
            &[],
            [(7, 0, -1448, -460, -1299), (2, 3, -1448, -281, -996)],
        ),
        (
            "\u{1100}a",
            &on,
            [(7, 0, -1448, -460, -1299), (4, 3, -1448, -255, -996)],
        ),
        // A Latin buffer applies the caller's `calt` too.
        (
            "a\u{AC00}",
            &[],
            [(2, 0, -1448, -281, -996), (14, 1, -1448, -460, -1099)],
        ),
        (
            "a\u{AC00}",
            &on,
            [(4, 0, -1448, -255, -996), (14, 1, -1448, -460, -1099)],
        ),
    ];
    for (text, features, expected) in cases {
        for guess in [true, false] {
            assert_eq!(
                vertical_rows(text, guess, features),
                expected,
                "{text:?} {features:?} guess={guess}"
            );
        }
    }
}
