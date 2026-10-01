//! Hangul tone marks against HarfBuzz.
//!
//! Every expectation here is HarfBuzz 14.5.0's output (uharfbuzz 0.56.2,
//! `hb.shape` with no features, LTR, script and language guessed) with
//! `tests/fonts/NotoSansKR-HangulTone-Subset.ttf`: glyph id, cluster,
//! x advance, x offset, y offset. rustybuzz 0.20 agrees on all of them.
//!
//! HarfBuzz's Hangul shaper (`preprocess_text_hangul` in
//! `hb-ot-shaper-hangul.cc`) moves a tone mark (U+302E, U+302F) that
//! follows a syllable in front of it, merging their clusters, and gives
//! a tone mark with no syllable before it a dotted circle to sit on.
//! A syllable is a precomposed one or a jamo sequence that starts with
//! a leading and a vowel jamo. The tone mark keeps its advance: the
//! Hangul shaper zeroes no mark advances. Before, sigilbuzz left the
//! tone mark where it was typed, with no dotted circle.
//!
//! Glyph 17 is U+302E, 18 U+302F, and 16 the dotted circle.

use sigilbuzz::{shape, Blob, Buffer, BufferFlags, ClusterLevel, Direction, Face, Font};

const FONT: &[u8] = include_bytes!("fonts/NotoSansKR-HangulTone-Subset.ttf");

type Row = (u32, u32, i32, i32, i32);

fn shape_rows(text: &str, level: ClusterLevel, flags: BufferFlags) -> Vec<Row> {
    let blob = Blob::new(FONT);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(Direction::Ltr);
    buffer.set_cluster_level(level);
    buffer.set_flags(flags);
    shape(&font, &buffer, &[])
        .expect("shape")
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.cluster, g.x_advance, g.x_offset, g.y_offset))
        .collect()
}

fn check(cases: &[(&str, &[Row])], level: ClusterLevel, flags: BufferFlags) {
    for &(text, expected) in cases {
        assert_eq!(
            shape_rows(text, level, flags),
            expected,
            "{text:?} at {level:?}"
        );
    }
}

#[test]
fn tone_marks_move_in_front_of_their_syllable() {
    let cases: &[(&str, &[Row])] = &[
        // After a precomposed syllable.
        (
            "\u{AC00}\u{302E}",
            &[(17, 0, 250, 0, 0), (20, 0, 920, 0, 0)],
        ),
        (
            "\u{AC01}\u{302F}",
            &[(18, 0, 250, 0, 0), (21, 0, 920, 0, 0)],
        ),
        // After jamo that compose.
        (
            "\u{1100}\u{1161}\u{11A8}\u{302E}",
            &[(17, 0, 250, 0, 0), (21, 0, 920, 0, 0)],
        ),
        // After jamo with no precomposed form: in front of all three.
        (
            "\u{1100}\u{1161}\u{11C3}\u{302E}",
            &[
                (17, 0, 250, 0, 0),
                (28, 0, 920, 0, 0),
                (56, 0, 0, 0, 0),
                (67, 0, 0, 0, 0),
            ],
        ),
        (
            "\u{A960}\u{1161}\u{302F}",
            &[(18, 0, 250, 0, 0), (48, 0, 920, 0, 0), (60, 0, 0, 0, 0)],
        ),
        // An LV decomposed for a trailing jamo that cannot join it.
        (
            "\u{AC00}\u{11C3}\u{302E}",
            &[
                (17, 0, 250, 0, 0),
                (28, 0, 920, 0, 0),
                (56, 0, 0, 0, 0),
                (67, 0, 0, 0, 0),
            ],
        ),
        // Two syllables, each with a tone mark.
        (
            "\u{AC1C}\u{302E}\u{1101}\u{1162}\u{302F}",
            &[
                (17, 0, 250, 0, 0),
                (22, 0, 920, 0, 0),
                (18, 6, 250, 0, 0),
                (43, 6, 920, 0, 0),
                (61, 6, 0, 0, 0),
            ],
        ),
    ];
    check(cases, ClusterLevel::MonotoneGraphemes, BufferFlags::empty());
}

#[test]
fn tone_marks_without_a_syllable_get_a_dotted_circle() {
    let cases: &[(&str, &[Row])] = &[
        ("\u{302E}", &[(17, 0, 250, 0, 0), (16, 0, 1000, 0, 0)]),
        // A lone leading jamo is no syllable.
        (
            "\u{1100}\u{302E}",
            &[(3, 0, 920, 0, 0), (17, 0, 250, 0, 0), (16, 0, 1000, 0, 0)],
        ),
        // The second tone mark has no syllable left.
        (
            "\u{AC00}\u{302E}\u{302F}",
            &[
                (17, 0, 250, 0, 0),
                (20, 0, 920, 0, 0),
                (18, 0, 250, 0, 0),
                (16, 0, 1000, 0, 0),
            ],
        ),
        (
            "\u{AC00} \u{302E}",
            &[
                (20, 0, 920, 0, 0),
                (1, 3, 224, 0, 0),
                (17, 3, 250, 0, 0),
                (16, 3, 1000, 0, 0),
            ],
        ),
        // The dotted circle takes the tone mark's combining class, so
        // the acute accent sorts after both.
        (
            "\u{AC00}\u{0301}\u{302E}",
            &[
                (20, 0, 920, 0, 0),
                (17, 0, 250, 0, 0),
                (16, 0, 1000, 0, 0),
                (2, 0, 0, 0, 0),
            ],
        ),
    ];
    check(cases, ClusterLevel::MonotoneGraphemes, BufferFlags::empty());
}

#[test]
fn tone_mark_clusters_at_monotone_characters() {
    let cases: &[(&str, &[Row])] = &[
        (
            "\u{AC00}\u{302E}",
            &[(17, 0, 250, 0, 0), (20, 0, 920, 0, 0)],
        ),
        (
            "\u{1100}\u{302E}",
            &[(3, 0, 920, 0, 0), (17, 3, 250, 0, 0), (16, 3, 1000, 0, 0)],
        ),
        (
            "\u{AC00}\u{302E}\u{302F}",
            &[
                (17, 0, 250, 0, 0),
                (20, 0, 920, 0, 0),
                (18, 6, 250, 0, 0),
                (16, 6, 1000, 0, 0),
            ],
        ),
        (
            "\u{AC00}\u{0301}\u{302E}",
            &[
                (20, 0, 920, 0, 0),
                (17, 3, 250, 0, 0),
                (16, 3, 1000, 0, 0),
                (2, 3, 0, 0, 0),
            ],
        ),
    ];
    check(
        cases,
        ClusterLevel::MonotoneCharacters,
        BufferFlags::empty(),
    );
}

#[test]
fn no_dotted_circle_flag_leaves_the_tone_mark_alone() {
    let cases: &[(&str, &[Row])] = &[
        (
            "\u{AC00}\u{302E}",
            &[(17, 0, 250, 0, 0), (20, 0, 920, 0, 0)],
        ),
        ("\u{302E}", &[(17, 0, 250, 0, 0)]),
    ];
    check(
        cases,
        ClusterLevel::MonotoneGraphemes,
        BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE,
    );
}
