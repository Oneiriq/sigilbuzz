//! The Khmer shaper against HarfBuzz.
//!
//! Every expectation here is HarfBuzz 14.5.0's output (uharfbuzz 0.56.2,
//! `hb.shape` with no features, LTR, script and language guessed) for
//! Noto Sans Khmer: glyph id, cluster, x advance, x offset, y offset.
//! rustybuzz 0.20 agrees on all of them.
//!
//! HarfBuzz shapes Khmer with its own shaper (`hb-ot-shaper-khmer.cc`).
//! It has its own syllable grammar, where a joiner only stays in a
//! syllable before a robat, an above-base vowel, or an X-group sign. It
//! reorders coeng + ro and pre-base vowel signs its own way, and it has
//! its own feature stages. Before this shaper, sigilbuzz ran Khmer
//! through the Universal Shaping Engine and got these cases wrong.

use sigilbuzz::{shape, Blob, Buffer, BufferFlags, ClusterLevel, Direction, Face, Font};

const KHMER: &[u8] = include_bytes!("fonts/NotoSansKhmer-Regular.ttf");

type Row = (u32, u32, i32, i32, i32);

fn shape_rows(text: &str, level: ClusterLevel, flags: BufferFlags) -> Vec<Row> {
    let blob = Blob::new(KHMER);
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

/// Glyph 25 is ka, 3 ZWJ or ZWNJ, 107 sign e, 360 the dotted circle,
/// 196 and 197 the pre-base forms of coeng + ro.
const JOINERS: &[(&str, &[Row])] = &[
    // ZWJ before a coeng ends the syllable: coeng, ro is a broken
    // cluster with a dotted circle, and pref still moves ro before it.
    (
        "\u{1780}\u{200D}\u{17D2}\u{179A}",
        &[
            (25, 0, 636, 0, 0),
            (3, 0, 0, 0, 0),
            (196, 0, 287, 0, 0),
            (360, 0, 635, 0, 0),
        ],
    ),
    // ZWNJ before sign e: the sign starts a broken cluster and moves
    // before its dotted circle.
    (
        "\u{1780}\u{200C}\u{17C1}",
        &[
            (25, 0, 636, 0, 0),
            (3, 3, 0, 0, 0),
            (107, 3, 288, 0, 0),
            (360, 3, 635, 0, 0),
        ],
    ),
    // ZWJ before sign e does the same.
    (
        "\u{1780}\u{200D}\u{17C1}",
        &[
            (25, 0, 636, 0, 0),
            (3, 0, 0, 0, 0),
            (107, 0, 288, 0, 0),
            (360, 0, 635, 0, 0),
        ],
    ),
    // ZWJ before robat stays in the syllable.
    (
        "\u{1780}\u{200D}\u{17CC}",
        &[(25, 0, 636, 0, 0), (3, 0, 0, 0, 0), (124, 0, 0, -22, -29)],
    ),
    // ZWNJ before nikahit stays in the syllable.
    (
        "\u{1780}\u{17B7}\u{200C}\u{17C6}",
        &[
            (25, 0, 636, 0, 0),
            (81, 0, 0, -23, -29),
            (3, 6, 0, 0, 0),
            (113, 6, 0, -23, 217),
        ],
    ),
];

const REORDERING: &[(&str, &[Row])] = &[
    // Coeng, ro moves to the syllable start, before the robat too.
    (
        "\u{1780}\u{17CC}\u{17D2}\u{179A}",
        &[
            (196, 0, 287, 0, 0),
            (25, 0, 636, 0, 0),
            (124, 0, 0, -22, -29),
        ],
    ),
    // Coeng, ro first, then coeng, ko: pref, then blwf.
    (
        "\u{1784}\u{17D2}\u{179A}\u{17D2}\u{1782}",
        &[(197, 0, 287, 0, 0), (29, 0, 635, 0, 0), (161, 0, 0, 0, -26)],
    ),
    // Coeng, ko first, then coeng, ro.
    (
        "\u{1784}\u{17D2}\u{1782}\u{17D2}\u{179A}",
        &[(197, 0, 287, 0, 0), (29, 0, 635, 0, 0), (161, 0, 0, 0, -26)],
    ),
    // Only one pre-base vowel sign per syllable. The second is a
    // broken cluster.
    (
        "\u{1780}\u{17C1}\u{17C1}",
        &[
            (107, 0, 288, 0, 0),
            (25, 0, 636, 0, 0),
            (107, 0, 288, 0, 0),
            (360, 0, 635, 0, 0),
        ],
    ),
    // The split vowel o: sign e moves first, and clig, in the last
    // stage, ligates ro with the right part.
    (
        "\u{179A}\u{17C4}\u{1791}\u{178A}",
        &[
            (107, 0, 288, 0, 0),
            (268, 0, 615, 0, 0),
            (43, 6, 599, 0, 0),
            (36, 9, 635, 0, 0),
        ],
    ),
    // Three coengs.
    (
        "\u{1780}\u{17D2}\u{1780}\u{17D2}\u{1780}\u{17D2}\u{1780}",
        &[
            (25, 0, 636, 0, 0),
            (159, 0, 0, -2, -26),
            (159, 0, 0, -2, -296),
            (159, 0, 0, -2, -566),
        ],
    ),
];

/// Merges at a syllable's edge reach the neighbor that shares the
/// edge glyph's cluster, as `merge_clusters` does in HarfBuzz.
const EDGE_MERGES: &[(&str, &[Row])] = &[
    (
        "\u{178A}\u{17BE}\u{17D2}\u{178A}\u{17CA}\u{17B6}",
        &[
            (107, 0, 288, 0, 0),
            (36, 0, 635, 0, 0),
            (85, 0, 0, -36, 30),
            (172, 0, 0, -1, -26),
            (360, 0, 635, 0, 0),
            (120, 0, 0, -20, -84),
            (80, 0, 288, 0, 0),
        ],
    ),
    (
        "\u{1781}\u{1791}\u{17B1}\u{17D2}\u{1789}\u{200D}",
        &[
            (26, 0, 635, 0, 0),
            (43, 3, 599, 0, 0),
            (77, 6, 645, 0, 0),
            (170, 6, 0, 0, 0),
            (3, 6, 0, 0, 0),
        ],
    ),
];

#[test]
fn joiners_break_or_keep_syllables_like_harfbuzz() {
    check(
        JOINERS,
        ClusterLevel::MonotoneGraphemes,
        BufferFlags::empty(),
    );
}

#[test]
fn coeng_ro_and_pre_base_vowels_reorder_like_harfbuzz() {
    check(
        REORDERING,
        ClusterLevel::MonotoneGraphemes,
        BufferFlags::empty(),
    );
}

#[test]
fn syllable_edge_merges_follow_harfbuzz() {
    check(
        EDGE_MERGES,
        ClusterLevel::MonotoneGraphemes,
        BufferFlags::empty(),
    );
}

#[test]
fn monotone_characters_clusters_follow_harfbuzz() {
    let cases: &[(&str, &[Row])] = &[
        (
            "\u{1780}\u{200D}\u{17D2}\u{179A}",
            &[
                (25, 0, 636, 0, 0),
                (3, 3, 0, 0, 0),
                (196, 6, 287, 0, 0),
                (360, 6, 635, 0, 0),
            ],
        ),
        (
            "\u{1784}\u{17D2}\u{179A}\u{17D2}\u{1782}",
            &[(197, 0, 287, 0, 0), (29, 0, 635, 0, 0), (161, 9, 0, 0, -26)],
        ),
        (
            "\u{1780}\u{17C1}\u{17C1}",
            &[
                (107, 0, 288, 0, 0),
                (25, 0, 636, 0, 0),
                (107, 6, 288, 0, 0),
                (360, 6, 635, 0, 0),
            ],
        ),
        (
            "\u{178A}\u{17BE}\u{17D2}\u{178A}\u{17CA}\u{17B6}",
            &[
                (107, 0, 288, 0, 0),
                (36, 0, 635, 0, 0),
                (85, 0, 0, -36, 30),
                (172, 6, 0, -1, -26),
                (360, 12, 635, 0, 0),
                (120, 12, 0, -20, -84),
                (80, 15, 288, 0, 0),
            ],
        ),
        (
            "\u{1780}\u{17D2}\u{1780}\u{17D2}\u{1780}\u{17D2}\u{1780}",
            &[
                (25, 0, 636, 0, 0),
                (159, 3, 0, -2, -26),
                (159, 9, 0, -2, -296),
                (159, 15, 0, -2, -566),
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
fn no_dotted_circle_flag_leaves_broken_clusters_bare() {
    let cases: &[(&str, &[Row])] = &[
        (
            "\u{1780}\u{200C}\u{17C1}",
            &[(25, 0, 636, 0, 0), (3, 3, 0, 0, 0), (107, 3, 288, 0, 0)],
        ),
        ("\u{17C1}", &[(107, 0, 288, 0, 0)]),
    ];
    check(
        cases,
        ClusterLevel::MonotoneGraphemes,
        BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE,
    );
}
