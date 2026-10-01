//! The Universal Shaping Engine against HarfBuzz.
//!
//! Every expectation here is HarfBuzz 14.5.0's output (uharfbuzz 0.56.2,
//! `hb.shape` with no features, the direction given, script and language
//! guessed): glyph id, cluster, and glyph flags. The fonts are the Noto
//! fonts of `tests/fonts/`.
//!
//! HarfBuzz shapes these scripts with its USE shaper
//! (`hb-ot-shaper-use.cc`): the category table of
//! `hb-ot-shaper-use-table.hh`, the syllable machine of
//! `hb-ot-shaper-use-machine.rl`, its feature stages, its repha and
//! pre-base moves, and its dotted circles. Sinhala, Tibetan, N'Ko, and
//! Mongolian go there too (`hb_ot_shaper_categorize`), and a font whose
//! GSUB has only `DFLT` or `latn` lookups gets the default shaper.
//! sigilbuzz shaped these strings with a simpler syllable grammar and
//! category table, an earlier Indic pass for Sinhala, and shapers of
//! its own for Tibetan, N'Ko, and Mongolian, and each differed from
//! HarfBuzz before.

use std::time::{Duration, Instant};

use sigilbuzz::{shape, Blob, Buffer, ClusterLevel, Direction, Face, Font};

const TIRHUTA: &[u8] = include_bytes!("fonts/NotoSansTirhuta-Regular.ttf");
const SINHALA: &[u8] = include_bytes!("fonts/NotoSansSinhala-Regular.ttf");
const SHARADA: &[u8] = include_bytes!("fonts/NotoSansSharada-Regular.ttf");
const BALINESE: &[u8] = include_bytes!("fonts/NotoSansBalinese-Regular.ttf");
const CHAM: &[u8] = include_bytes!("fonts/NotoSansCham-Regular.ttf");
const KHOJKI: &[u8] = include_bytes!("fonts/NotoSansKhojki-Regular.ttf");
const MODI: &[u8] = include_bytes!("fonts/NotoSansModi-Regular.ttf");
const LEPCHA: &[u8] = include_bytes!("fonts/NotoSansLepcha-Regular.ttf");
const LIMBU: &[u8] = include_bytes!("fonts/NotoSansLimbu-Regular.ttf");
const SUNDANESE: &[u8] = include_bytes!("fonts/NotoSansSundanese-Regular.ttf");
const BUGINESE: &[u8] = include_bytes!("fonts/NotoSansBuginese-Regular.ttf");
const BRAHMI: &[u8] = include_bytes!("fonts/NotoSansBrahmi-Regular.ttf");
const TAI_THAM: &[u8] = include_bytes!("fonts/NotoSansTaiTham-Regular.ttf");
const TIBETAN: &[u8] = include_bytes!("fonts/NotoSerifTibetan-Regular.ttf");
const NKO: &[u8] = include_bytes!("fonts/NotoSansNKo-Regular.ttf");
const MONGOLIAN: &[u8] = include_bytes!("fonts/NotoSansMongolian-Regular.ttf");

/// One string, how to shape it, and HarfBuzz's glyph id, cluster, and
/// glyph flags for each glyph.
struct Case {
    font: &'static [u8],
    text: &'static str,
    level: ClusterLevel,
    direction: Direction,
    expected: &'static [(u32, u32, u32)],
}

fn shape_case(
    font: &[u8],
    text: &str,
    level: ClusterLevel,
    direction: Direction,
) -> Vec<(u32, u32, u32)> {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    buffer.set_cluster_level(level);
    shape(&font, &buffer, &[])
        .expect("shape")
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.cluster, g.flags.bits()))
        .collect()
}

fn check(cases: &[Case]) {
    for case in cases {
        let got = shape_case(case.font, case.text, case.level, case.direction);
        assert_eq!(got, case.expected, "{:?} at {:?}", case.text, case.level);
    }
}

#[test]
fn use_clusters_match_harfbuzz() {
    // The categories of the generated table and the clusters of the
    // syllable machine: pre-base signs move, signs without a base get
    // a dotted circle, and a cluster keeps its finals and modifiers.
    check(&[
        // Ra, virama, candrabindu: the candrabindu stays in the cluster.
        Case {
            font: TIRHUTA,
            text: "\u{114A9}\u{114C2}\u{11481}",
            level: ClusterLevel::Characters,
            direction: Direction::Ltr,
            expected: &[(51, 0, 0), (76, 4, 1), (11, 8, 1)],
        },
        // Ya, sign i: the pre-base sign moves in front.
        Case {
            font: TIRHUTA,
            text: "\u{1149F}\u{114B1}",
            level: ClusterLevel::Characters,
            direction: Direction::Ltr,
            expected: &[(59, 4, 1), (41, 0, 0)],
        },
        // Ka, sign i, sign candrabindu.
        Case {
            font: TIRHUTA,
            text: "\u{1148F}\u{114B1}\u{114BF}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(59, 0, 0), (230, 0, 0), (25, 0, 0), (203, 0, 0)],
        },
        // Ka, ZWNJ, va.
        Case {
            font: SHARADA,
            text: "\u{1118C}\u{200C}\u{111AB}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(16, 0, 0), (3, 4, 1), (47, 7, 0)],
        },
        // Sa, sign upadhmaniya: no base for the sign.
        Case {
            font: SHARADA,
            text: "\u{111AC}\u{111C3}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(48, 0, 0), (198, 4, 0), (206, 4, 0)],
        },
        // Ka with two vowel signs: the second one is a broken cluster.
        Case {
            font: BALINESE,
            text: "\u{1B16}\u{1B3B}\u{1B3D}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[
                (244, 0, 0),
                (63, 0, 0),
                (131, 0, 0),
                (233, 0, 0),
                (71, 0, 0),
                (58, 0, 0),
            ],
        },
        // Ka, final ng: the final stays in the cluster.
        Case {
            font: CHAM,
            text: "\u{AA06}\u{AA42}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(21, 0, 0), (58, 3, 0)],
        },
        // Ka, sign virama.
        Case {
            font: KHOJKI,
            text: "\u{11207}\u{11235}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(106, 0, 0), (150, 0, 0)],
        },
        // Ka, sign virama.
        Case {
            font: MODI,
            text: "\u{11605}\u{1163F}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(23, 0, 0), (81, 0, 0)],
        },
        // Ra, virama, independent a.
        Case {
            font: MODI,
            text: "\u{11628}\u{1163F}\u{11600}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(18, 0, 0), (148, 0, 0)],
        },
        // Ga, sign nyin-do: a pre-base vowel modifier.
        Case {
            font: LEPCHA,
            text: "\u{1C07}\u{1C35}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(127, 0, 0), (8, 0, 0)],
        },
        // A small final, then a vowel sign: two broken clusters.
        Case {
            font: LIMBU,
            text: "\u{1930}\u{1926}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(78, 0, 0), (45, 0, 0), (78, 0, 0), (39, 0, 0)],
        },
        // Independent vowel e, vowel sign panaelaeng.
        Case {
            font: SUNDANESE,
            text: "\u{1B87}\u{1BA6}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(49, 0, 0), (14, 0, 0)],
        },
        // Ga, vowel sign ae, vowel sign e.
        Case {
            font: BUGINESE,
            text: "\u{1A02}\u{1A1B}\u{1A19}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(3, 0, 0), (39, 0, 0), (37, 0, 0), (34, 0, 0)],
        },
        // Independent a, vowel sign i.
        Case {
            font: BRAHMI,
            text: "\u{11003}\u{11039}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(8, 0, 0), (227, 0, 0), (62, 0, 0)],
        },
        // Number one, number joiner, number two.
        Case {
            font: BRAHMI,
            text: "\u{11052}\u{1107F}\u{11053}",
            level: ClusterLevel::Characters,
            direction: Direction::Ltr,
            expected: &[(83, 0, 0), (183, 4, 1), (84, 8, 1)],
        },
    ]);
}

#[test]
fn sinhala_shapes_with_the_universal_shaping_engine() {
    // Two or more pre-base vowel signs move in front of the base in
    // turn, which the earlier Indic pass got wrong.
    check(&[
        // Ka and two kombuva: both move in front of the base.
        Case {
            font: SINHALA,
            text: "\u{0D9A}\u{0DD9}\u{0DD9}",
            level: ClusterLevel::Characters,
            direction: Direction::Ltr,
            expected: &[(74, 6, 1), (74, 3, 1), (24, 0, 0)],
        },
        // Ka, kombuva deka, kombuva.
        Case {
            font: SINHALA,
            text: "\u{0D9A}\u{0DDB}\u{0DD9}",
            level: ClusterLevel::Characters,
            direction: Direction::Ltr,
            expected: &[(74, 6, 1), (76, 3, 1), (24, 0, 0)],
        },
        // Ka with yansaya, and kombuva.
        Case {
            font: SINHALA,
            text: "\u{0D9A}\u{0DCA}\u{200D}\u{0DBA}\u{0DD9}",
            level: ClusterLevel::Characters,
            direction: Direction::Ltr,
            expected: &[(74, 12, 1), (24, 0, 0), (128, 3, 1)],
        },
        // Ka with rakaransaya, and two kombuva.
        Case {
            font: SINHALA,
            text: "\u{0D9A}\u{0DCA}\u{200D}\u{0DBB}\u{0DD9}\u{0DD9}",
            level: ClusterLevel::Characters,
            direction: Direction::Ltr,
            expected: &[(74, 15, 1), (74, 12, 1), (24, 0, 0), (133, 3, 1)],
        },
        // Repaya on ka, and kombuva.
        Case {
            font: SINHALA,
            text: "\u{0DBB}\u{0DCA}\u{200D}\u{0D9A}\u{0DD9}",
            level: ClusterLevel::Characters,
            direction: Direction::Ltr,
            expected: &[(74, 12, 1), (372, 9, 1)],
        },
        // Touching ka and ssa.
        Case {
            font: SINHALA,
            text: "\u{0D9A}\u{200D}\u{0DCA}\u{0DC2}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(547, 0, 0), (60, 9, 1)],
        },
        // Ka, al-lakuna, ZWNJ, ka: the ZWNJ ends the first cluster.
        Case {
            font: SINHALA,
            text: "\u{0D9A}\u{0DCA}\u{200C}\u{0D9A}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(186, 0, 0), (3, 6, 1), (24, 9, 0)],
        },
        // A lone kombuva gets a dotted circle.
        Case {
            font: SINHALA,
            text: "\u{0DD9}\u{0D82}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(74, 0, 0), (643, 0, 0), (4, 0, 0)],
        },
    ]);
}

#[test]
fn tibetan_nko_and_mongolian_shape_with_the_universal_shaping_engine() {
    // N'Ko and Mongolian also take the joining forms of their letters.
    check(&[
        // Ga, vowel sign vocalic ll.
        Case {
            font: TIBETAN,
            text: "\u{0F42}\u{0F79}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(243, 0, 0), (1347, 0, 0)],
        },
        // Kha, dha.
        Case {
            font: TIBETAN,
            text: "\u{0F41}\u{0F52}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(7, 0, 0), (23, 3, 0)],
        },
        // A trailing ZWNJ is a broken cluster.
        Case {
            font: NKO,
            text: "\u{07E2}\u{07CF}\u{200C}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Rtl,
            expected: &[(3, 4, 1), (82, 2, 1), (141, 0, 0)],
        },
        // A ZWNJ between letters.
        Case {
            font: MONGOLIAN,
            text: "\u{185D}\u{200C}\u{1856}\u{1829}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(367, 0, 0), (1489, 3, 1), (333, 6, 0), (81, 9, 1)],
        },
    ]);
}

#[test]
fn a_font_without_lookups_for_the_script_gets_the_default_shaper() {
    // Noto Sans Tai Tham lists its lookups under DFLT only.
    check(&[
        // The font has DFLT lookups only, so the default shaper runs.
        Case {
            font: TAI_THAM,
            text: "\u{1A37}\u{1A6F}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(256, 0, 0)],
        },
        // Ka, vowel sign e: no reorder with the default shaper.
        Case {
            font: TAI_THAM,
            text: "\u{1A20}\u{1A6E}",
            level: ClusterLevel::MonotoneGraphemes,
            direction: Direction::Ltr,
            expected: &[(6, 0, 0)],
        },
    ]);
}

/// Shapes `text` with `font`, returning the glyph count and the time.
fn timed(font: &[u8], text: &str) -> (usize, Duration) {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).expect("face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    let start = Instant::now();
    let glyphs = shape(&font, &buffer, &[]).expect("shape").len();
    (glyphs, start.elapsed())
}

#[test]
fn long_runs_of_signs_and_joiners_shape_in_linear_time() {
    // One Balinese cluster of a base and 20,000 pre-base signs, whose
    // signs all move, and 20,000 ZWNJ and mark pairs, which the
    // syllable machine reads past, each against a run of 20,000 bases.
    const N: usize = 20_000;
    let (_, plain) = timed(BALINESE, &"\u{1B13}".repeat(N));
    let budget = plain * 20 + Duration::from_secs(2);
    let signs = format!("\u{1B13}{}", "\u{1B3E}".repeat(N));
    let (glyphs, took) = timed(BALINESE, &signs);
    assert_eq!(glyphs, N + 1);
    assert!(took < budget, "pre-base signs: {took:?}, budget {budget:?}");
    let pairs = format!("\u{1B13}{}", "\u{200C}\u{1B36}".repeat(N));
    let (glyphs, took) = timed(BALINESE, &pairs);
    assert_eq!(glyphs, 2 * N + 1);
    assert!(
        took < budget,
        "ZWNJ and mark pairs: {took:?}, budget {budget:?}"
    );
}
