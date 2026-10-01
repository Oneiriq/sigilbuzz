//! HarfBuzz's vowel constraints: a dotted circle inside a vowel and
//! vowel sign sequence that would read as another vowel.
//!
//! HarfBuzz's Indic and Universal Shaping Engine shapers run
//! `_hb_preprocess_text_vowel_constraints`
//! (`hb-ot-shaper-vowel-constraints.cc`) as their `preprocess_text`, with
//! the sequences of the buffer's script, unless the buffer has
//! `DO_NOT_INSERT_DOTTED_CIRCLE`. The circle copies the glyph info of the
//! character after it, so it takes that character's cluster, and in a
//! font without GDEF glyph classes it is a mark when that character is a
//! nonspacing mark.
//!
//! Every expectation here is HarfBuzz 14.5.0's output (uharfbuzz 0.56.2,
//! `hb.shape` with no features, LTR, UTF-8 clusters, script guessed
//! unless the case sets it) with the fonts under `tests/fonts`: glyph id,
//! cluster, glyph flags, x advance, x offset, y offset, at the three
//! cluster levels, and at the default level with
//! `DO_NOT_INSERT_DOTTED_CIRCLE`.

use sigilbuzz::{
    shape, Blob, Buffer, BufferFlags, ClusterLevel, Direction, Face, Font, UnicodeScript,
};

const DEVANAGARI: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Regular.ttf");
const BENGALI: &[u8] = include_bytes!("fonts/NotoSansBengali-Regular.ttf");
const GURMUKHI: &[u8] = include_bytes!("fonts/NotoSansGurmukhi-Regular.ttf");
const GUJARATI: &[u8] = include_bytes!("fonts/NotoSansGujarati-Regular.ttf");
const ORIYA: &[u8] = include_bytes!("fonts/NotoSansOriya-Regular.ttf");
const TAMIL: &[u8] = include_bytes!("fonts/NotoSansTamil-Regular.ttf");
const TELUGU: &[u8] = include_bytes!("fonts/NotoSansTelugu-Regular.ttf");
const KANNADA: &[u8] = include_bytes!("fonts/NotoSansKannada-Regular.ttf");
const MALAYALAM: &[u8] = include_bytes!("fonts/NotoSansMalayalam-Regular.ttf");
const SINHALA: &[u8] = include_bytes!("fonts/NotoSansSinhala-Regular.ttf");
const BRAHMI: &[u8] = include_bytes!("fonts/NotoSansBrahmi-Regular.ttf");
const KHOJKI: &[u8] = include_bytes!("fonts/NotoSansKhojki-Regular.ttf");
const TIRHUTA: &[u8] = include_bytes!("fonts/NotoSansTirhuta-Regular.ttf");
const MODI: &[u8] = include_bytes!("fonts/NotoSansModi-Regular.ttf");
const DEVANAGARI_NO_GDEF: &[u8] = include_bytes!("fonts/NotoSansDevanagari-NoGDEF-Subset.ttf");

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
    flags: BufferFlags,
) -> Vec<Row> {
    let blob = Blob::new(font_data);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(Direction::Ltr);
    buffer.set_cluster_level(level);
    buffer.set_script(script);
    buffer.set_flags(flags);
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
    /// At the default level with `DO_NOT_INSERT_DOTTED_CIRCLE`.
    without_circles: &'static [Row],
}

fn check(cases: &[Case]) {
    for case in cases {
        for (level, expected) in LEVELS.into_iter().zip(case.expected) {
            assert_eq!(
                shape_rows(
                    case.font,
                    case.text,
                    case.script,
                    level,
                    BufferFlags::empty()
                ),
                expected,
                "{:?} at {level:?}",
                case.text
            );
        }
        assert_eq!(
            shape_rows(
                case.font,
                case.text,
                case.script,
                ClusterLevel::MonotoneGraphemes,
                BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE,
            ),
            case.without_circles,
            "{:?} without dotted circles",
            case.text
        );
    }
}

#[test]
fn vowel_sequences_get_a_dotted_circle_as_in_harfbuzz() {
    check(&[
        // A, AA.
        Case {
            font: DEVANAGARI,
            text: "\u{0905}\u{093E}",
            script: None,
            expected: [
                &[
                    (5, 0, 0, 764, 0, 0),
                    (789, 0, 0, 510, 0, 0),
                    (31, 0, 0, 259, 0, 0),
                ],
                &[
                    (5, 0, 0, 764, 0, 0),
                    (789, 3, 1, 510, 0, 0),
                    (31, 3, 1, 259, 0, 0),
                ],
                &[
                    (5, 0, 0, 764, 0, 0),
                    (789, 3, 1, 510, 0, 0),
                    (31, 3, 1, 259, 0, 0),
                ],
            ],
            without_circles: &[(5, 0, 0, 764, 0, 0), (31, 0, 0, 259, 0, 0)],
        },
        // RA, VIRAMA, I.
        Case {
            font: DEVANAGARI,
            text: "\u{0930}\u{094D}\u{0907}",
            script: None,
            expected: [
                &[
                    (789, 0, 0, 510, 0, 0),
                    (506, 0, 0, 0, 6, 0),
                    (7, 0, 0, 491, 0, 0),
                ],
                &[
                    (789, 0, 0, 510, 0, 0),
                    (506, 0, 0, 0, 6, 0),
                    (7, 0, 0, 491, 0, 0),
                ],
                &[
                    (789, 6, 1, 510, 0, 0),
                    (506, 0, 0, 0, 6, 0),
                    (7, 6, 0, 491, 0, 0),
                ],
            ],
            without_circles: &[(7, 0, 0, 491, 0, 0), (506, 0, 0, 0, 2, 0)],
        },
        // A, AA, AA.
        Case {
            font: DEVANAGARI,
            text: "\u{0905}\u{093E}\u{093E}",
            script: None,
            expected: [
                &[
                    (5, 0, 0, 764, 0, 0),
                    (789, 0, 0, 510, 0, 0),
                    (31, 0, 0, 259, 0, 0),
                    (31, 0, 0, 259, 0, 0),
                ],
                &[
                    (5, 0, 0, 764, 0, 0),
                    (789, 3, 1, 510, 0, 0),
                    (31, 3, 1, 259, 0, 0),
                    (31, 6, 1, 259, 0, 0),
                ],
                &[
                    (5, 0, 0, 764, 0, 0),
                    (789, 3, 1, 510, 0, 0),
                    (31, 3, 1, 259, 0, 0),
                    (31, 6, 1, 259, 0, 0),
                ],
            ],
            without_circles: &[
                (5, 0, 0, 764, 0, 0),
                (31, 0, 0, 259, 0, 0),
                (31, 0, 0, 259, 0, 0),
            ],
        },
        // KA, AA, E.
        Case {
            font: DEVANAGARI,
            text: "\u{0915}\u{0906}\u{0947}",
            script: None,
            expected: [
                &[
                    (56, 0, 0, 768, 0, 0),
                    (6, 3, 0, 1022, 0, 0),
                    (789, 3, 0, 510, 0, 0),
                    (40, 3, 0, 0, 6, 0),
                ],
                &[
                    (56, 0, 0, 768, 0, 0),
                    (6, 3, 0, 1022, 0, 0),
                    (789, 6, 1, 510, 0, 0),
                    (40, 6, 1, 0, 6, 0),
                ],
                &[
                    (56, 0, 0, 768, 0, 0),
                    (6, 3, 0, 1022, 0, 0),
                    (789, 6, 1, 510, 0, 0),
                    (40, 6, 1, 0, 6, 0),
                ],
            ],
            without_circles: &[
                (56, 0, 0, 768, 0, 0),
                (6, 3, 0, 1022, 0, 0),
                (40, 3, 0, 0, 0, 0),
            ],
        },
        // a Latin buffer.
        Case {
            font: DEVANAGARI,
            text: "a\u{0905}\u{093E}",
            script: None,
            expected: [
                &[
                    (0, 0, 0, 600, 0, 0),
                    (5, 1, 0, 764, 0, 0),
                    (31, 1, 0, 259, 0, 0),
                ],
                &[
                    (0, 0, 0, 600, 0, 0),
                    (5, 1, 0, 764, 0, 0),
                    (31, 4, 1, 259, 0, 0),
                ],
                &[
                    (0, 0, 0, 600, 0, 0),
                    (5, 1, 0, 764, 0, 0),
                    (31, 4, 1, 259, 0, 0),
                ],
            ],
            without_circles: &[
                (0, 0, 0, 600, 0, 0),
                (5, 1, 0, 764, 0, 0),
                (31, 1, 0, 259, 0, 0),
            ],
        },
        // script set.
        Case {
            font: DEVANAGARI,
            text: "a\u{0905}\u{093E}",
            script: Some(UnicodeScript::Devanagari),
            expected: [
                &[
                    (0, 0, 0, 600, 0, 0),
                    (5, 1, 0, 764, 0, 0),
                    (789, 1, 0, 510, 0, 0),
                    (31, 1, 0, 259, 0, 0),
                ],
                &[
                    (0, 0, 0, 600, 0, 0),
                    (5, 1, 0, 764, 0, 0),
                    (789, 4, 1, 510, 0, 0),
                    (31, 4, 1, 259, 0, 0),
                ],
                &[
                    (0, 0, 0, 600, 0, 0),
                    (5, 1, 0, 764, 0, 0),
                    (789, 4, 1, 510, 0, 0),
                    (31, 4, 1, 259, 0, 0),
                ],
            ],
            without_circles: &[
                (0, 0, 0, 600, 0, 0),
                (5, 1, 0, 764, 0, 0),
                (31, 1, 0, 259, 0, 0),
            ],
        },
        // A, AA.
        Case {
            font: BENGALI,
            text: "\u{0985}\u{09BE}",
            script: None,
            expected: [
                &[
                    (8, 0, 0, 893, 0, 0),
                    (661, 0, 0, 510, 0, 0),
                    (54, 0, 0, 266, 0, 0),
                ],
                &[
                    (8, 0, 0, 893, 0, 0),
                    (661, 3, 1, 510, 0, 0),
                    (54, 3, 1, 266, 0, 0),
                ],
                &[
                    (8, 0, 0, 893, 0, 0),
                    (661, 3, 1, 510, 0, 0),
                    (54, 3, 1, 266, 0, 0),
                ],
            ],
            without_circles: &[(8, 0, 0, 893, 0, 0), (54, 0, 0, 266, 0, 0)],
        },
        // IRI, I.
        Case {
            font: GURMUKHI,
            text: "\u{0A72}\u{0A3F}",
            script: None,
            expected: [
                &[
                    (79, 0, 0, 557, 0, 0),
                    (52, 0, 0, 259, 0, 0),
                    (302, 0, 0, 566, 0, 0),
                ],
                &[
                    (79, 0, 0, 557, 0, 0),
                    (52, 3, 1, 259, 0, 0),
                    (302, 3, 1, 566, 0, 0),
                ],
                &[
                    (79, 0, 0, 557, 0, 0),
                    (52, 3, 1, 259, 0, 0),
                    (302, 3, 1, 566, 0, 0),
                ],
            ],
            without_circles: &[(52, 0, 0, 259, 0, 0), (79, 0, 0, 557, 0, 0)],
        },
        // A, AA, CANDRA E.
        Case {
            font: GUJARATI,
            text: "\u{0A85}\u{0ABE}\u{0AC5}",
            script: None,
            expected: [
                &[
                    (7, 0, 0, 883, 0, 0),
                    (756, 0, 0, 510, 0, 0),
                    (64, 0, 0, 0, 0, 0),
                    (57, 0, 0, 265, 0, 0),
                ],
                &[
                    (7, 0, 0, 883, 0, 0),
                    (756, 3, 1, 510, 0, 0),
                    (64, 3, 1, 0, 0, 0),
                    (57, 3, 1, 265, 0, 0),
                ],
                &[
                    (7, 0, 0, 883, 0, 0),
                    (756, 3, 1, 510, 0, 0),
                    (64, 6, 1, 0, 0, 0),
                    (57, 3, 1, 265, 0, 0),
                ],
            ],
            without_circles: &[
                (7, 0, 0, 883, 0, 0),
                (64, 0, 0, 0, 0, 0),
                (57, 0, 0, 265, 0, 0),
            ],
        },
        // CANDRA E, AA.
        Case {
            font: GUJARATI,
            text: "\u{0AC5}\u{0ABE}",
            script: None,
            expected: [
                &[
                    (756, 0, 0, 510, 0, 0),
                    (64, 0, 0, 0, 0, 0),
                    (756, 0, 0, 510, 0, 0),
                    (57, 0, 0, 265, 0, 0),
                ],
                &[
                    (756, 0, 0, 510, 0, 0),
                    (64, 0, 0, 0, 0, 0),
                    (756, 3, 1, 510, 0, 0),
                    (57, 3, 1, 265, 0, 0),
                ],
                &[
                    (756, 0, 0, 510, 0, 0),
                    (64, 0, 0, 0, 0, 0),
                    (756, 3, 1, 510, 0, 0),
                    (57, 3, 1, 265, 0, 0),
                ],
            ],
            without_circles: &[(64, 0, 0, 0, 0, 0), (57, 0, 0, 265, 0, 0)],
        },
        // O, AU LENGTH MARK.
        Case {
            font: ORIYA,
            text: "\u{0B13}\u{0B57}",
            script: None,
            expected: [
                &[
                    (18, 0, 0, 700, 0, 0),
                    (105, 0, 0, 800, 0, 0),
                    (76, 0, 0, 201, 0, 0),
                ],
                &[
                    (18, 0, 0, 700, 0, 0),
                    (105, 3, 1, 800, 0, 0),
                    (76, 3, 1, 201, 0, 0),
                ],
                &[
                    (18, 0, 0, 700, 0, 0),
                    (105, 3, 1, 800, 0, 0),
                    (76, 3, 1, 201, 0, 0),
                ],
            ],
            without_circles: &[(18, 0, 0, 700, 0, 0), (76, 0, 0, 201, 0, 0)],
        },
        // A, UU.
        Case {
            font: TAMIL,
            text: "\u{0B85}\u{0BC2}",
            script: None,
            expected: [
                &[
                    (6, 0, 0, 1121, 0, 0),
                    (243, 0, 0, 562, 0, 0),
                    (45, 0, 0, 844, 0, 0),
                ],
                &[
                    (6, 0, 0, 1121, 0, 0),
                    (243, 3, 1, 562, 0, 0),
                    (45, 3, 1, 844, 0, 0),
                ],
                &[
                    (6, 0, 0, 1121, 0, 0),
                    (243, 3, 1, 562, 0, 0),
                    (45, 3, 1, 844, 0, 0),
                ],
            ],
            without_circles: &[(6, 0, 0, 1121, 0, 0), (45, 0, 0, 844, 0, 0)],
        },
        // O, LENGTH MARK.
        Case {
            font: TELUGU,
            text: "\u{0C12}\u{0C55}",
            script: None,
            expected: [
                &[
                    (20, 0, 0, 731, 0, 0),
                    (672, 0, 0, 578, 0, 0),
                    (74, 0, 0, 0, 0, 0),
                ],
                &[
                    (20, 0, 0, 731, 0, 0),
                    (672, 3, 1, 578, 0, 0),
                    (74, 3, 1, 0, 0, 0),
                ],
                &[
                    (20, 0, 0, 731, 0, 0),
                    (672, 3, 1, 578, 0, 0),
                    (74, 3, 1, 0, 0, 0),
                ],
            ],
            without_circles: &[(20, 0, 0, 731, 0, 0), (74, 0, 0, 0, 0, 0)],
        },
        // O, LENGTH MARK, TILDE OVERLAY.
        Case {
            font: TELUGU,
            text: "\u{0C12}\u{0C55}\u{0334}",
            script: None,
            expected: [
                &[
                    (20, 0, 0, 731, 0, 0),
                    (0, 0, 0, 600, 0, 0),
                    (672, 0, 0, 578, 0, 0),
                    (74, 0, 0, 0, 0, 0),
                ],
                &[
                    (20, 0, 0, 731, 0, 0),
                    (0, 3, 1, 600, 0, 0),
                    (672, 3, 1, 578, 0, 0),
                    (74, 3, 1, 0, 0, 0),
                ],
                &[
                    (20, 0, 0, 731, 0, 0),
                    (0, 6, 1, 600, 0, 0),
                    (672, 3, 1, 578, 0, 0),
                    (74, 3, 1, 0, 0, 0),
                ],
            ],
            without_circles: &[
                (20, 0, 0, 731, 0, 0),
                (0, 0, 0, 600, 0, 0),
                (74, 0, 0, 0, 0, 0),
            ],
        },
        // U, AA.
        Case {
            font: KANNADA,
            text: "\u{0C89}\u{0CBE}",
            script: None,
            expected: [
                &[
                    (13, 0, 0, 1222, 0, 0),
                    (480, 0, 0, 561, 0, 0),
                    (60, 0, 0, 449, 0, 0),
                ],
                &[
                    (13, 0, 0, 1222, 0, 0),
                    (480, 3, 1, 561, 0, 0),
                    (60, 3, 1, 449, 0, 0),
                ],
                &[
                    (13, 0, 0, 1222, 0, 0),
                    (480, 3, 1, 561, 0, 0),
                    (60, 3, 1, 449, 0, 0),
                ],
            ],
            without_circles: &[(13, 0, 0, 1222, 0, 0), (60, 0, 0, 449, 0, 0)],
        },
        // O, AA.
        Case {
            font: MALAYALAM,
            text: "\u{0D12}\u{0D3E}",
            script: None,
            expected: [
                &[
                    (18, 0, 0, 757, 0, 0),
                    (345, 0, 0, 562, 0, 0),
                    (60, 0, 0, 504, 0, 0),
                ],
                &[
                    (18, 0, 0, 757, 0, 0),
                    (345, 3, 1, 562, 0, 0),
                    (60, 3, 1, 504, 0, 0),
                ],
                &[
                    (18, 0, 0, 757, 0, 0),
                    (345, 3, 1, 562, 0, 0),
                    (60, 3, 1, 504, 0, 0),
                ],
            ],
            without_circles: &[(18, 0, 0, 757, 0, 0), (60, 0, 0, 504, 0, 0)],
        },
        // AYANNA, AELA-PILLA.
        Case {
            font: SINHALA,
            text: "\u{0D85}\u{0DCF}",
            script: None,
            expected: [
                &[
                    (6, 0, 0, 708, 0, 0),
                    (643, 0, 0, 622, 0, 0),
                    (66, 0, 0, 343, 0, 0),
                ],
                &[
                    (6, 0, 0, 708, 0, 0),
                    (643, 3, 1, 622, 0, 0),
                    (66, 3, 1, 343, 0, 0),
                ],
                &[
                    (6, 0, 0, 708, 0, 0),
                    (643, 3, 1, 622, 0, 0),
                    (66, 3, 1, 343, 0, 0),
                ],
            ],
            without_circles: &[(6, 0, 0, 708, 0, 0), (66, 0, 0, 343, 0, 0)],
        },
        // A, AA.
        Case {
            font: BRAHMI,
            text: "\u{11005}\u{11038}",
            script: None,
            expected: [
                &[
                    (10, 0, 0, 593, 0, 0),
                    (227, 0, 0, 594, 0, 0),
                    (61, 0, 0, 0, -294, 0),
                ],
                &[
                    (10, 0, 0, 593, 0, 0),
                    (227, 4, 1, 594, 0, 0),
                    (61, 4, 1, 0, -294, 0),
                ],
                &[
                    (10, 0, 0, 593, 0, 0),
                    (227, 4, 1, 594, 0, 0),
                    (61, 4, 1, 0, -294, 0),
                ],
            ],
            without_circles: &[(10, 0, 0, 593, 0, 0), (61, 0, 0, 0, 0, 0)],
        },
        // SHORT I, II.
        Case {
            font: KHOJKI,
            text: "\u{11240}\u{1122E}",
            script: None,
            expected: [
                &[
                    (0, 0, 0, 600, 0, 0),
                    (7, 0, 0, 594, 0, 0),
                    (144, 0, 0, 225, 0, 0),
                ],
                &[
                    (0, 0, 0, 600, 0, 0),
                    (7, 4, 1, 594, 0, 0),
                    (144, 4, 1, 225, 0, 0),
                ],
                &[
                    (0, 0, 0, 600, 0, 0),
                    (7, 4, 1, 594, 0, 0),
                    (144, 4, 1, 225, 0, 0),
                ],
            ],
            without_circles: &[(0, 0, 0, 600, 0, 0), (144, 0, 0, 225, 0, 0)],
        },
        // A, AA.
        Case {
            font: TIRHUTA,
            text: "\u{11481}\u{114B0}",
            script: None,
            expected: [
                &[
                    (11, 0, 0, 732, 0, 0),
                    (7, 0, 0, 594, 0, 0),
                    (58, 0, 0, 266, 0, 0),
                ],
                &[
                    (11, 0, 0, 732, 0, 0),
                    (7, 4, 1, 594, 0, 0),
                    (58, 4, 1, 266, 0, 0),
                ],
                &[
                    (11, 0, 0, 732, 0, 0),
                    (7, 4, 1, 594, 0, 0),
                    (58, 4, 1, 266, 0, 0),
                ],
            ],
            without_circles: &[(11, 0, 0, 732, 0, 0), (58, 0, 0, 266, 0, 0)],
        },
        // A, E.
        Case {
            font: MODI,
            text: "\u{11600}\u{11639}",
            script: None,
            expected: [
                &[
                    (18, 0, 0, 756, 0, 0),
                    (7, 0, 0, 594, 0, 0),
                    (75, 0, 0, 0, -165, -78),
                ],
                &[
                    (18, 0, 0, 756, 0, 0),
                    (7, 4, 1, 594, 0, 0),
                    (75, 4, 1, 0, -165, -78),
                ],
                &[
                    (18, 0, 0, 756, 0, 0),
                    (7, 4, 1, 594, 0, 0),
                    (75, 4, 1, 0, -165, -78),
                ],
            ],
            without_circles: &[(18, 0, 0, 756, 0, 0), (75, 0, 0, 0, 0, 0)],
        },
    ]);
}

#[test]
fn circle_before_a_nonspacing_mark_is_a_mark_without_gdef() {
    // DEVANAGARI LETTER A, VOWEL SIGN CANDRA E in a font without GDEF:
    // HarfBuzz synthesizes the glyph classes from the General_Category
    // the circle copied from U+0945, so the circle is a mark, and the
    // vowel sign attaches to the letter A past it.
    let expected: [&[Row]; 3] = [
        &[
            (2, 0, 0, 764, 0, 0),
            (5, 0, 0, 510, 0, 0),
            (3, 0, 0, 0, -497, 0),
        ],
        &[
            (2, 0, 0, 764, 0, 0),
            (5, 3, 1, 510, 0, 0),
            (3, 3, 1, 0, -497, 0),
        ],
        &[
            (2, 0, 0, 764, 0, 0),
            (5, 3, 1, 510, 0, 0),
            (3, 3, 1, 0, -497, 0),
        ],
    ];
    for (level, expected) in LEVELS.into_iter().zip(expected) {
        let rows = shape_rows(
            DEVANAGARI_NO_GDEF,
            "\u{0905}\u{0945}",
            None,
            level,
            BufferFlags::empty(),
        );
        assert_eq!(rows, expected, "{level:?}");
    }
    // A typed circle is a symbol, so a base glyph that takes the mark.
    let rows = shape_rows(
        DEVANAGARI_NO_GDEF,
        "\u{0905}\u{25CC}\u{0945}",
        None,
        ClusterLevel::MonotoneGraphemes,
        BufferFlags::empty(),
    );
    assert_eq!(
        rows,
        [
            (2, 0, 0, 764, 0, 0),
            (5, 3, 0, 510, 0, 0),
            (3, 3, 0, 0, 10, 0)
        ]
    );
}

#[test]
fn long_vowel_runs_shape_in_linear_time() {
    // 4,000 Devanagari A, AA pairs, each of which takes a circle.
    let text = "\u{0905}\u{093E}".repeat(4_000);
    let start = std::time::Instant::now();
    let rows = shape_rows(
        DEVANAGARI,
        &text,
        None,
        ClusterLevel::MonotoneGraphemes,
        BufferFlags::empty(),
    );
    assert_eq!(rows.len(), 12_000);
    assert!(
        start.elapsed() < std::time::Duration::from_secs(20),
        "took {:?}",
        start.elapsed()
    );
}
