//! Indic fonts with the newest script tags (`dev3`, `bng3`, ...),
//! against HarfBuzz.
//!
//! HarfBuzz tries the tag ending in 3 before the one ending in 2 and
//! the old one (`hb_ot_all_tags_from_script` in `hb-ot-tag.cc`), and
//! `hb_ot_shaper_categorize` (`hb-ot-shaper.hh`) sends an Indic script
//! whose chosen GSUB script tag ends in 3 to the Universal Shaping
//! Engine. Every expectation here is HarfBuzz 14.5.0's output
//! (uharfbuzz 0.56.2, `hb.shape` with no features, script and language
//! guessed, monotone grapheme clusters): glyph id, cluster, x advance,
//! x and y offset, and glyph flags. `NotoSansDevanagari-Dev3-Subset.ttf`
//! is a subset of Noto Sans Devanagari whose `dev2` script records are
//! renamed `dev3` (see `tests/fonts/README.md`).

use sigilbuzz::{shape, Blob, Buffer, ClusterLevel, Direction, Face, Font};

const DEVANAGARI_DEV3: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Dev3-Subset.ttf");
const DEVANAGARI: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Regular.ttf");

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

#[test]
fn dev3_fonts_take_the_universal_shaping_engine() {
    // The Universal Shaping Engine forms a final half form of sa with
    // the virama, gives each of two vowel signs its own dotted circle,
    // places the repha with the vowel sign o, and puts candrabindu after
    // visarga in a broken cluster of its own. HarfBuzz's Indic shaper
    // shapes each of these differently, and so did sigilbuzz, which
    // stopped at `dev2` and picked the font's `deva` lookups.
    for case in &[
        Case {
            font: DEVANAGARI_DEV3,
            text: "\u{0938}\u{094D}",
            direction: Direction::Ltr,
            expected: &[(38, 0, 389, 0, 0, 0)],
        },
        Case {
            font: DEVANAGARI_DEV3,
            text: "\u{0941}\u{0947}",
            direction: Direction::Ltr,
            expected: &[
                (197, 0, 510, 0, 0, 0),
                (6, 0, 0, -9, -18, 0),
                (197, 0, 510, 0, 0, 0),
                (7, 0, 0, 6, 0, 0),
            ],
        },
        Case {
            font: DEVANAGARI_DEV3,
            text: "\u{0930}\u{094D}\u{0915}\u{094B}",
            direction: Direction::Ltr,
            expected: &[
                (9, 0, 768, 0, 0, 0),
                (98, 0, 0, -221, 0, 0),
                (8, 0, 259, 0, 0, 0),
            ],
        },
        Case {
            font: DEVANAGARI_DEV3,
            text: "\u{0915}\u{0903}\u{0901}",
            direction: Direction::Ltr,
            expected: &[
                (9, 0, 768, 0, 0, 0),
                (22, 0, 202, 0, 0, 0),
                (197, 0, 510, 0, 0, 0),
                (21, 0, 0, 10, 0, 0),
            ],
        },
        Case {
            font: DEVANAGARI_DEV3,
            text: "\u{0915}\u{093F}",
            direction: Direction::Ltr,
            expected: &[(109, 0, 259, 0, 0, 0), (9, 0, 768, 0, 0, 0)],
        },
        Case {
            font: DEVANAGARI_DEV3,
            text: "\u{0905}\u{093E}",
            direction: Direction::Ltr,
            expected: &[
                (2, 0, 764, 0, 0, 0),
                (197, 0, 510, 0, 0, 0),
                (3, 0, 259, 0, 0, 0),
            ],
        },
    ] {
        let got = shape_text(case.font, case.text, case.direction);
        assert_eq!(got, case.expected, "{:?}", case.text);
    }
}

#[test]
fn dev2_fonts_keep_the_indic_shaper() {
    // The same strings in the full font, which has `dev2` lookups and
    // no `dev3` ones: one dotted circle for both vowel signs, and sa
    // with a visible virama.
    assert_eq!(
        shape_text(DEVANAGARI, "\u{0938}\u{094D}", Direction::Ltr),
        [(87, 0, 676, 0, 0, 0), (103, 0, 0, 0, 0, 0)]
    );
    assert_eq!(
        shape_text(DEVANAGARI, "\u{0941}\u{0947}", Direction::Ltr),
        [
            (789, 0, 510, 0, 0, 0),
            (34, 0, 0, -9, -18, 0),
            (40, 0, 0, 6, 0, 0)
        ]
    );
}
