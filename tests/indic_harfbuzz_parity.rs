//! The Indic shaper against HarfBuzz: ZWJ and ZWNJ around reph,
//! half forms, and pre-base matras, and Kannada's Ra,H,ZWJ.
//!
//! Every expectation here is HarfBuzz 14.5.0's output (uharfbuzz 0.56.2,
//! `hb.shape` with no features, LTR, script and language guessed) with
//! the Noto Sans fonts under `tests/fonts`: glyph id, cluster, x
//! advance, x offset, y offset. rustybuzz 0.20 agrees on all of them.
//!
//! HarfBuzz's Indic shaper (`hb-ot-shaper-indic.cc`) reads joiners in
//! its syllable grammar, in `initial_reordering_consonant_syllable`
//! (a joiner after Ra,H blocks an implicit reph, a ZWJ after a halant
//! stops the base search, a ZWNJ turns `half` off), and in
//! `final_reordering_syllable_indic` (a pre-base matra does not move
//! after a halant a ZWJ follows, and a reph or pre-base consonant moves
//! past a joiner after a halant). For Kannada it shapes Ra,H,ZWJ at the
//! start of a syllable as Ra,ZWJ,H, so it forms no reph.

use sigilbuzz::{shape, Blob, Buffer, ClusterLevel, Direction, Face, Font};

const DEVANAGARI: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Regular.ttf");
const BENGALI: &[u8] = include_bytes!("fonts/NotoSansBengali-Regular.ttf");
const MALAYALAM: &[u8] = include_bytes!("fonts/NotoSansMalayalam-Regular.ttf");
const TELUGU: &[u8] = include_bytes!("fonts/NotoSansTelugu-Regular.ttf");
const KANNADA: &[u8] = include_bytes!("fonts/NotoSansKannada-Regular.ttf");

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

#[test]
fn devanagari_joiners_follow_harfbuzz() {
    let cases: &[(&str, &[Row])] = &[
        // Ra,H,ka: a reph.
        (
            "\u{0930}\u{094D}\u{0915}",
            &[(56, 0, 768, 0, 0), (506, 0, 0, -221, 0)],
        ),
        // Ra,H,ZWJ,ka: the ZWJ blocks the reph and asks for the
        // eyelash Ra.
        (
            "\u{0930}\u{094D}\u{200D}\u{0915}",
            &[(507, 0, 379, 0, 0), (56, 9, 768, 0, 0)],
        ),
        // Ra,H,ZWNJ,ka: the ZWNJ ends the syllable after the halant.
        (
            "\u{0930}\u{094D}\u{200C}\u{0915}",
            &[
                (82, 0, 409, 0, 0),
                (103, 0, 0, 0, 0),
                (3, 6, 0, 0, 0),
                (56, 9, 768, 0, 0),
            ],
        ),
        // Ra,ZWJ,H,ka.
        (
            "\u{0930}\u{200D}\u{094D}\u{0915}",
            &[
                (82, 0, 409, 0, 0),
                (3, 0, 0, 0, 0),
                (103, 0, 0, 0, 0),
                (56, 9, 768, 0, 0),
            ],
        ),
        // Ka,H,ZWJ,ssa,i: the half form stays, and the matra does not
        // move after a halant that a ZWJ follows.
        (
            "\u{0915}\u{094D}\u{200D}\u{0937}\u{093F}",
            &[
                (555, 0, 259, 0, 0),
                (232, 0, 550, 0, 0),
                (3, 0, 0, 0, 0),
                (86, 0, 578, 0, 0),
            ],
        ),
        // Ka,H,ZWNJ,ssa,i: the matra stays with ssa.
        (
            "\u{0915}\u{094D}\u{200C}\u{0937}\u{093F}",
            &[
                (56, 0, 768, 0, 0),
                (103, 0, 0, -221, 0),
                (3, 6, 0, 0, 0),
                (546, 9, 259, 0, 0),
                (86, 9, 578, 0, 0),
            ],
        ),
        // Ka,ZWJ,H,ssa: the base search goes past a ZWJ before a
        // halant.
        (
            "\u{0915}\u{200D}\u{094D}\u{0937}",
            &[
                (56, 0, 768, 0, 0),
                (3, 0, 0, 0, 0),
                (103, 0, 0, -221, 0),
                (86, 9, 578, 0, 0),
            ],
        ),
        // Ra,H,ka,H,ssa,i: reph and matra.
        (
            "\u{0930}\u{094D}\u{0915}\u{094D}\u{0937}\u{093F}",
            &[(596, 0, 259, 0, 0), (90, 0, 717, 0, 0), (785, 0, 0, 0, 0)],
        ),
        // Ra,H,ZWJ,ka,i.
        (
            "\u{0930}\u{094D}\u{200D}\u{0915}\u{093F}",
            &[(552, 0, 259, 0, 0), (507, 0, 379, 0, 0), (56, 0, 768, 0, 0)],
        ),
        // Ka,H,Ra,ZWJ.
        (
            "\u{0915}\u{094D}\u{0930}\u{200D}",
            &[(295, 0, 768, 0, 0), (3, 0, 0, 0, 0)],
        ),
        // Ta,H,Ra,H,ka: rakaar and the half form.
        (
            "\u{0924}\u{094D}\u{0930}\u{094D}\u{0915}",
            &[(367, 0, 382, 0, 0), (56, 12, 768, 0, 0)],
        ),
    ];
    check(DEVANAGARI, cases, ClusterLevel::MonotoneGraphemes);
}

#[test]
fn bengali_joiners_follow_harfbuzz() {
    let cases: &[(&str, &[Row])] = &[
        // Ra,H,Ya: ya-phala.
        (
            "\u{09B0}\u{09CD}\u{09AF}",
            &[(45, 0, 626, 0, 0), (131, 0, 0, 0, 0)],
        ),
        // Ra,H,ZWJ,Ya.
        (
            "\u{09B0}\u{09CD}\u{200D}\u{09AF}",
            &[
                (46, 0, 596, 0, 0),
                (65, 0, 0, 0, 0),
                (3, 0, 0, 0, 0),
                (45, 9, 626, 0, 0),
            ],
        ),
        // Ka,H,ZWJ,ssa,i.
        (
            "\u{0995}\u{09CD}\u{200D}\u{09B7}\u{09BF}",
            &[
                (495, 0, 266, 0, 0),
                (134, 0, 682, 0, 0),
                (3, 0, 0, 0, 0),
                (49, 0, 633, 0, 0),
            ],
        ),
        // Ka,H,ZWNJ,ssa,e.
        (
            "\u{0995}\u{09CD}\u{200C}\u{09B7}\u{09C7}",
            &[
                (20, 0, 807, 0, 0),
                (65, 0, 0, -220, 0),
                (3, 6, 0, 0, 0),
                (61, 9, 346, 0, 0),
                (49, 9, 633, 0, 0),
            ],
        ),
    ];
    check(BENGALI, cases, ClusterLevel::MonotoneGraphemes);
}

#[test]
fn malayalam_and_telugu_joiners_follow_harfbuzz() {
    let malayalam: &[(&str, &[Row])] = &[
        // Ka,H,ZWJ: a chillu.
        ("\u{0D15}\u{0D4D}\u{200D}", &[(117, 0, 1206, 0, 0)]),
        // Ka,H,ka,H,ZWNJ,ka.
        (
            "\u{0D15}\u{0D4D}\u{0D15}\u{0D4D}\u{200C}\u{0D15}",
            &[
                (159, 0, 1506, 0, 0),
                (73, 0, 0, 0, 0),
                (3, 12, 0, 0, 0),
                (21, 15, 1038, 0, 0),
            ],
        ),
        // Dot reph,ka,e.
        (
            "\u{0D4E}\u{0D15}\u{0D46}",
            &[(67, 0, 715, 0, 0), (21, 0, 1038, 0, 0), (74, 0, 0, -232, 0)],
        ),
    ];
    check(MALAYALAM, malayalam, ClusterLevel::MonotoneGraphemes);
    let telugu: &[(&str, &[Row])] = &[
        // Ra,H,ZWJ,ka: Telugu forms a reph only when asked.
        (
            "\u{0C30}\u{0C4D}\u{200D}\u{0C15}",
            &[(23, 0, 522, 0, 0), (611, 0, 565, 0, 0)],
        ),
        (
            "\u{0C30}\u{0C4D}\u{0C15}",
            &[(49, 0, 580, 0, 0), (470, 0, 483, 0, 0)],
        ),
        // Ka,H,ZWNJ,ssa.
        (
            "\u{0C15}\u{0C4D}\u{200C}\u{0C37}",
            &[(102, 0, 522, 0, 0), (3, 6, 0, 0, 0), (56, 9, 713, 0, 0)],
        ),
    ];
    check(TELUGU, telugu, ClusterLevel::MonotoneGraphemes);
}

#[test]
fn kannada_ra_halant_zwj_forms_no_reph() {
    let cases: &[(&str, &[Row])] = &[
        // Ra,H,ka: a reph.
        (
            "\u{0CB0}\u{0CCD}\u{0C95}",
            &[(23, 0, 574, 0, 0), (93, 0, 567, 0, 0)],
        ),
        // Ra,H,ZWJ,ka shapes as Ra,ZWJ,H,ka: no reph.
        (
            "\u{0CB0}\u{0CCD}\u{200D}\u{0C95}",
            &[(49, 0, 651, 0, 0), (3, 0, 0, 0, 0), (96, 0, 175, 0, 0)],
        ),
        (
            "\u{0CB0}\u{0CCD}\u{200D}\u{0C95}\u{0CBF}",
            &[(230, 0, 651, 0, 0), (3, 0, 0, 0, 0), (96, 0, 175, 0, 0)],
        ),
        (
            "\u{0CB0}\u{200D}\u{0CCD}\u{0C95}",
            &[(49, 0, 651, 0, 0), (3, 0, 0, 0, 0), (96, 0, 175, 0, 0)],
        ),
        (
            "\u{0CB0}\u{0CCD}\u{200D}",
            &[(194, 0, 964, 0, 0), (3, 0, 0, 0, 0)],
        ),
        // Only at the start of a syllable.
        (
            "\u{0C95}\u{0CCD}\u{0CB0}\u{0CCD}\u{200D}\u{0C95}",
            &[
                (168, 0, 887, 0, 0),
                (194, 6, 964, 0, 0),
                (23, 15, 574, 0, 0),
            ],
        ),
    ];
    check(KANNADA, cases, ClusterLevel::MonotoneGraphemes);
}

#[test]
fn kannada_swap_merges_the_halant_and_zwj_clusters() {
    // HarfBuzz merges the clusters of the halant and the ZWJ it swaps.
    let cases: &[(&str, &[Row])] = &[
        (
            "\u{0CB0}\u{0CCD}\u{200D}\u{0C95}",
            &[(49, 0, 651, 0, 0), (3, 3, 0, 0, 0), (96, 3, 175, 0, 0)],
        ),
        (
            "\u{0CB0}\u{200D}\u{0CCD}\u{0C95}",
            &[(49, 0, 651, 0, 0), (3, 3, 0, 0, 0), (96, 6, 175, 0, 0)],
        ),
    ];
    check(KANNADA, cases, ClusterLevel::MonotoneCharacters);
}

#[test]
fn devanagari_stress_signs_take_the_script_of_their_letter() {
    // U+0951..U+0954 are Inherited in Scripts.txt. Alone they give
    // HarfBuzz no script, so the default shaper inserts no dotted
    // circle. After a Bengali letter they are Bengali, and the mark
    // positions on the letter.
    let cases: &[(&str, &[Row])] = &[
        ("\u{0954}", &[(111, 0, 0, 0, 0)]),
        (
            "\u{0954}\u{25CC}",
            &[(111, 0, 0, 0, 0), (789, 3, 510, 0, 0)],
        ),
        (
            "\u{0951}\u{0915}",
            &[
                (789, 0, 510, 0, 0),
                (108, 0, 0, -170, 0),
                (56, 3, 768, 0, 0),
            ],
        ),
        (
            "\u{0915}\u{0951}",
            &[(56, 0, 768, 0, 0), (108, 0, 0, -221, 0)],
        ),
    ];
    check(DEVANAGARI, cases, ClusterLevel::MonotoneGraphemes);
    let cases: &[(&str, &[Row])] = &[
        (
            "\u{0995}\u{0951}",
            &[(20, 0, 807, 0, 0), (639, 0, 0, -99, 323)],
        ),
        (
            "\u{0995}\u{0952}\u{09BE}",
            &[
                (20, 0, 807, 0, 0),
                (640, 0, 0, -99, -313),
                (661, 0, 510, 0, 0),
                (54, 0, 266, 0, 0),
            ],
        ),
    ];
    check(BENGALI, cases, ClusterLevel::MonotoneGraphemes);
}

#[test]
fn an_indic_script_without_lookups_in_the_font_gets_the_default_shaper() {
    // hb_ot_shaper_categorize: Noto Sans Devanagari has no Bengali
    // script, so GSUB picks DFLT for Bengali text and HarfBuzz shapes it
    // with the default shaper. A leading stress sign gets no dotted
    // circle, and at the character level the stress sign keeps its own
    // cluster.
    let cases: &[(&str, &[Row])] = &[
        ("\u{0951}\u{0995}", &[(108, 0, 0, 0, 0), (0, 3, 600, 0, 0)]),
        ("\u{0995}\u{0951}", &[(0, 0, 600, 0, 0), (108, 0, 0, 0, 0)]),
    ];
    check(DEVANAGARI, cases, ClusterLevel::MonotoneGraphemes);
    let cases: &[(&str, &[Row])] = &[
        ("\u{0995}\u{0951}", &[(0, 0, 600, 0, 0), (108, 3, 0, 0, 0)]),
        (
            "\u{0995}\u{0951}\u{0951}",
            &[(0, 0, 600, 0, 0), (108, 3, 0, 0, 0), (108, 6, 0, 0, 0)],
        ),
    ];
    check(DEVANAGARI, cases, ClusterLevel::MonotoneCharacters);
}
