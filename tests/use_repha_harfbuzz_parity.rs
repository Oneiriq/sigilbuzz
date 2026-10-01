//! The Universal Shaping Engine's repha against HarfBuzz.
//!
//! Every expectation here is HarfBuzz 14.5.0's output (uharfbuzz 0.56.2,
//! `hb.shape` with no features, LTR, script and language guessed) with
//! Noto Sans Tirhuta and Noto Sans Modi: glyph id, cluster, x advance,
//! x offset, y offset. rustybuzz 0.20 agrees on all of them.
//!
//! HarfBuzz's USE shaper (`hb-ot-shaper-use.cc`) lets `rphf` apply only
//! to the first glyphs of a syllable (`setup_rphf_mask`), makes the
//! glyph it substitutes a repha (`record_rphf_use`), and after the basic
//! features moves that repha toward the end of the syllable, to just
//! before the first vowel sign, medial, final, or halant that did not
//! ligate, or to the end (`reorder_syllable_use`). Before, sigilbuzz
//! left the repha where `rphf` formed it.

use sigilbuzz::{shape, Blob, Buffer, ClusterLevel, Direction, Face, Font};

const TIRHUTA: &[u8] = include_bytes!("fonts/NotoSansTirhuta-Regular.ttf");
const MODI: &[u8] = include_bytes!("fonts/NotoSansModi-Regular.ttf");

type Row = (u32, u32, i32, i32, i32);

fn shape_rows(font_data: &[u8], text: &str, level: ClusterLevel) -> Vec<Row> {
    let blob = Blob::new(font_data);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(Direction::Ltr);
    buffer.set_cluster_level(level);
    shape(&font, &buffer, &[])
        .expect("shape")
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.cluster, g.x_advance, g.x_offset, g.y_offset))
        .collect()
}

fn check(font_data: &[u8], cases: &[(&str, &[Row])], level: ClusterLevel) {
    for &(text, expected) in cases {
        assert_eq!(
            shape_rows(font_data, text, level),
            expected,
            "{text:?} at {level:?}"
        );
    }
}

/// Glyph 134 is the Tirhuta repha, 25 ka, 76 the virama, 67 sign e.
const TIRHUTA_CASES: &[(&str, &[Row])] = &[
    // Ra,H,ka: the repha moves after ka.
    (
        "\u{114A9}\u{114C2}\u{1148F}",
        &[(25, 0, 807, 0, 0), (134, 0, 0, -367, -8)],
    ),
    // Ra,H,ka,e: the repha stops before the vowel sign, which then
    // moves to the front.
    (
        "\u{114A9}\u{114C2}\u{1148F}\u{114B9}",
        &[
            (67, 0, 346, 0, 0),
            (25, 0, 807, 0, 0),
            (134, 0, 0, -367, -8),
        ],
    ),
    // Ra,H,ka,H,kha: the repha stops before the halant.
    (
        "\u{114A9}\u{114C2}\u{1148F}\u{114C2}\u{11490}",
        &[
            (25, 0, 807, 0, 0),
            (134, 0, 0, -367, -8),
            (76, 0, 0, -247, 0),
            (26, 16, 677, 0, 0),
        ],
    ),
    (
        "\u{114A9}\u{114C2}\u{1148F}\u{114C2}",
        &[
            (25, 0, 807, 0, 0),
            (134, 0, 0, -367, -8),
            (76, 0, 0, -247, 0),
        ],
    ),
    // Ra,H,ka,visarga: the repha stops before the final mark.
    (
        "\u{114A9}\u{114C2}\u{1148F}\u{114C0}",
        &[
            (25, 0, 1017, 0, 0),
            (134, 0, 0, -577, -8),
            (231, 0, 0, -30, 15),
        ],
    ),
    (
        "\u{114A9}\u{114C2}\u{1148F}\u{114B2}\u{114C0}",
        &[
            (25, 0, 807, 0, 0),
            (203, 0, 0, 0, 0),
            (60, 0, 586, 0, 0),
            (221, 0, 0, -486, 0),
            (231, 0, 0, -19, -12),
        ],
    ),
    // `rphf` only applies at the start of a syllable.
    (
        "\u{1148F}\u{114A9}\u{114C2}\u{1148F}",
        &[
            (25, 0, 807, 0, 0),
            (25, 4, 807, 0, 0),
            (134, 4, 0, -367, -8),
        ],
    ),
];

#[test]
fn tirhuta_repha_moves_like_harfbuzz() {
    check(TIRHUTA, TIRHUTA_CASES, ClusterLevel::MonotoneGraphemes);
}

#[test]
fn modi_repha_moves_like_harfbuzz() {
    // Glyph 148 is the Modi repha and 32 ka.
    let cases: &[(&str, &[Row])] = &[
        (
            "\u{11628}\u{1163F}\u{1160E}",
            &[(32, 0, 655, 0, 0), (148, 0, 0, 3, 4)],
        ),
        (
            "\u{11628}\u{1163F}\u{1160E}\u{11631}",
            &[(32, 0, 655, 0, 0), (148, 0, 0, 3, 4), (67, 0, 240, 0, 0)],
        ),
        // Ra,H,ka,H,kha: the half form ligated, so the repha goes to
        // the end.
        (
            "\u{11628}\u{1163F}\u{1160E}\u{1163F}\u{1160F}",
            &[(173, 0, 550, 0, 0), (33, 0, 694, 0, 0), (148, 0, 0, 54, 4)],
        ),
        (
            "\u{11628}\u{1163F}\u{1160E}\u{11630}",
            &[(32, 0, 655, 0, 0), (148, 0, 0, 3, 4), (66, 0, 240, 0, 0)],
        ),
    ];
    check(MODI, cases, ClusterLevel::MonotoneGraphemes);
}

#[test]
fn repha_move_merges_clusters_like_harfbuzz() {
    // At MONOTONE_CHARACTERS the repha shares its cluster only with the
    // glyphs it passes.
    let cases: &[(&str, &[Row])] = &[
        (
            "\u{114A9}\u{114C2}\u{1148F}\u{114C2}\u{11490}",
            &[
                (25, 0, 807, 0, 0),
                (134, 0, 0, -367, -8),
                (76, 12, 0, -247, 0),
                (26, 16, 677, 0, 0),
            ],
        ),
        (
            "\u{114A9}\u{114C2}\u{1148F}\u{114C0}",
            &[
                (25, 0, 1017, 0, 0),
                (134, 0, 0, -577, -8),
                (231, 12, 0, -30, 15),
            ],
        ),
    ];
    check(TIRHUTA, cases, ClusterLevel::MonotoneCharacters);
}
