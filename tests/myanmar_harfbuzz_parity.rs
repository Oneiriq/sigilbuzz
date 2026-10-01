//! The Myanmar shaper against HarfBuzz.
//!
//! Every expectation here is HarfBuzz 14.5.0's output (uharfbuzz 0.56.2,
//! `hb.shape` with no features, script and language guessed) for Noto
//! Sans Myanmar, at the cluster levels `MonotoneGraphemes`,
//! `MonotoneCharacters`, and `Characters` in that order. Each glyph
//! reads `id=cluster+x_advance,y_advance@x_offset,y_offset#flags`.
//!
//! HarfBuzz shapes Myanmar with its own shaper (`hb-ot-shaper-myanmar.cc`)
//! and syllable machine (`hb-ot-shaper-myanmar-machine.rl`): `locl` and
//! `ccmp` per syllable, then dotted circles and the syllable reorder
//! (kinzi after the base, medial ra and pre-base vowels before it, marks
//! after a below-base vowel before it, a run of pre-base vowels flipped),
//! the basic features one stage each, and the other features with the
//! default ones. sigilbuzz shaped Myanmar with a simpler grammar and
//! reorder before, and got about half of these strings wrong.

use std::time::{Duration, Instant};

use sigilbuzz::{shape, Blob, Buffer, ClusterLevel, Direction, Face, Font};

const MYANMAR: &[u8] = include_bytes!("fonts/NotoSansMyanmar-Regular.ttf");

/// The cluster levels of [`Case::expected`], in order.
const LEVELS: [ClusterLevel; 3] = [
    ClusterLevel::MonotoneGraphemes,
    ClusterLevel::MonotoneCharacters,
    ClusterLevel::Characters,
];

/// One string and HarfBuzz's glyphs for it at each of [`LEVELS`].
struct Case {
    text: &'static str,
    expected: [&'static str; 3],
}

fn shape_text(text: &str, level: ClusterLevel, direction: Direction) -> String {
    shape_with(MYANMAR, text, level, direction)
}

fn shape_with(font: &[u8], text: &str, level: ClusterLevel, direction: Direction) -> String {
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
        .map(|g| {
            format!(
                "{}={}+{},{}@{},{}#{}",
                g.glyph_id,
                g.cluster,
                g.x_advance,
                g.y_advance,
                g.x_offset,
                g.y_offset,
                g.flags.bits()
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn check(cases: &[Case]) {
    for case in cases {
        for (level, expected) in LEVELS.iter().zip(case.expected) {
            assert_eq!(
                shape_text(case.text, *level, Direction::Ltr),
                expected,
                "{:?} at {level:?}",
                case.text
            );
        }
    }
}

#[test]
fn kinzi_goes_after_the_base() {
    check(KINZI);
}

#[test]
fn medials_follow_the_grammar_and_medial_ra_goes_first() {
    check(MEDIALS);
}

#[test]
fn pre_base_vowels_move_and_flip() {
    check(PRE_BASE);
}

#[test]
fn stacked_consonants_shape_as_one_syllable() {
    check(STACKS);
}

#[test]
fn asat_anusvara_dot_below_visarga_and_tones_take_their_positions() {
    check(SIGNS);
}

#[test]
fn joiners_stay_in_their_syllables() {
    check(JOINERS);
}

#[test]
fn broken_clusters_get_a_dotted_circle() {
    check(BROKEN);
}

#[test]
fn digits_and_extended_blocks_shape_like_harfbuzz() {
    check(DIGITS_AND_EXTENSIONS);
}

#[test]
fn vertical_text_runs_vert_in_the_last_stage() {
    for &(text, expected) in VERTICAL {
        assert_eq!(
            shape_text(text, ClusterLevel::MonotoneGraphemes, Direction::Ttb),
            expected,
            "{text:?}"
        );
    }
}

#[test]
fn a_mymr_font_gets_the_default_shaper() {
    // The font with its `mym2` script tags renamed `mymr`, the tag of
    // fonts made before Myanmar's shaping model. HarfBuzz gives such a
    // font the default shaper: no reorder and no dotted circle, and its
    // `ccmp` ligates medials ra, wa and ha. sigilbuzz skipped `ccmp`
    // there before.
    let mut font = MYANMAR.to_vec();
    for i in 0..font.len().saturating_sub(3) {
        if &font[i..i + 4] == b"mym2" {
            font[i + 3] = b'r';
        }
    }
    let cases = [
        (
            "\u{1000}\u{103C}\u{103D}\u{103E}",
            [
                "4=0+1124,0@0,0#0|405=0+229,0@0,0#0",
                "4=0+1124,0@0,0#0|405=3+229,0@0,0#1",
            ],
        ),
        (
            "\u{1000}\u{1031}",
            [
                "4=0+1124,0@0,0#0|372=0+618,0@0,0#0",
                "4=0+1124,0@0,0#0|372=3+618,0@0,0#1",
            ],
        ),
        ("\u{1031}", ["372=0+618,0@0,0#0", "372=0+618,0@0,0#0"]),
    ];
    let levels = [ClusterLevel::MonotoneGraphemes, ClusterLevel::Characters];
    for (text, expected) in cases {
        for (level, expected) in levels.into_iter().zip(expected) {
            assert_eq!(
                shape_with(&font, text, level, Direction::Ltr),
                expected,
                "{text:?} at {level:?}"
            );
        }
    }
}

/// Shapes `text`, returning the glyph count and the time.
fn timed(text: &str) -> (usize, Duration) {
    let blob = Blob::new(MYANMAR);
    let face = Face::parse(&blob, 0).expect("face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_cluster_level(ClusterLevel::MonotoneCharacters);
    let start = Instant::now();
    let glyphs = shape(&font, &buffer, &[]).expect("shape").len();
    (glyphs, start.elapsed())
}

#[test]
fn long_syllables_shape_in_linear_time() {
    // One syllable of ka, 10,000 asats, and 10,000 signs e: every sign
    // moves past every asat in HarfBuzz's insertion sort. Then 10,000
    // stacked consonants, and 10,000 ZWJ, which the syllable machine
    // reads past. Each is timed against a run of 20,000 consonants.
    const N: usize = 10_000;
    let (_, plain) = timed(&"\u{1000}".repeat(2 * N));
    let budget = plain * 20 + Duration::from_secs(2);
    let signs = format!("\u{1000}{}{}", "\u{103A}".repeat(N), "\u{1031}".repeat(N));
    let (glyphs, took) = timed(&signs);
    assert!(glyphs > N);
    assert!(
        took < budget,
        "asats and signs e: {took:?}, budget {budget:?}"
    );
    let stack = format!("\u{1000}{}", "\u{1039}\u{1001}".repeat(N));
    let (_, took) = timed(&stack);
    assert!(
        took < budget,
        "stacked consonants: {took:?}, budget {budget:?}"
    );
    let joiners = format!("\u{1000}{}\u{1031}", "\u{200D}".repeat(N));
    let (glyphs, took) = timed(&joiners);
    assert_eq!(glyphs, N + 3);
    assert!(took < budget, "joiners: {took:?}, budget {budget:?}");
}

const KINZI: &[Case] = &[
    // Nga, asat, virama, ka.
    Case {
        text: "\u{1004}\u{103A}\u{1039}\u{1000}",
        expected: [
            "4=0+1124,0@0,0#0|189=0+0,0@-1,0#0",
            "4=0+1124,0@0,0#0|189=0+0,0@-1,0#0",
            "4=9+1124,0@0,0#1|189=0+0,0@-1,0#0",
        ],
    },
    // Kinzi, ka, sign e.
    Case {
        text: "\u{1004}\u{103A}\u{1039}\u{1000}\u{1031}",
        expected: [
            "372=0+618,0@0,0#0|4=0+1124,0@0,0#0|189=0+0,0@-1,0#0",
            "372=0+618,0@0,0#0|4=0+1124,0@0,0#0|189=0+0,0@-1,0#0",
            "372=12+618,0@0,0#1|4=9+1124,0@0,0#1|189=0+0,0@-1,0#0",
        ],
    },
    // Ra kinzi, ka, sign i.
    Case {
        text: "\u{101B}\u{103A}\u{1039}\u{1000}\u{102D}",
        expected: [
            "4=0+1124,0@0,0#0|191=0+0,0@-27,0#0",
            "4=0+1124,0@0,0#0|191=0+0,0@-27,0#0",
            "4=9+1124,0@0,0#1|191=0+0,0@-27,0#0",
        ],
    },
    // Mingalaba.
    Case {
        text: "\u{1019}\u{1004}\u{103A}\u{1039}\u{1002}\u{101C}\u{102C}\u{1015}\u{102B}",
        expected: [
            "29=0+676,0@0,0#0|6=3+668,0@0,0#1|189=3+0,0@-4,0#1|32=15+1126,0@0,0#1|368=15+455,0@0,0#1|25=21+676,0@0,0#1|367=21+267,0@0,0#1",
            "29=0+676,0@0,0#0|6=3+668,0@0,0#1|189=3+0,0@-4,0#1|32=15+1126,0@0,0#1|368=18+455,0@0,0#1|25=21+676,0@0,0#1|367=24+267,0@0,0#1",
            "29=0+676,0@0,0#0|6=12+668,0@0,0#1|189=3+0,0@-4,0#0|32=15+1126,0@0,0#1|368=18+455,0@0,0#1|25=21+676,0@0,0#1|367=24+267,0@0,0#1",
        ],
    },
    // A lone kinzi is a broken cluster.
    Case {
        text: "\u{1004}\u{103A}\u{1039}",
        expected: [
            "388=0+594,0@0,0#0|189=0+0,0@35,-42#0",
            "388=0+594,0@0,0#0|189=0+0,0@35,-42#0",
            "388=0+594,0@0,0#0|189=0+0,0@35,-42#0",
        ],
    },
];

const MEDIALS: &[Case] = &[
    // Ka, medial ya, ra, wa, ha.
    Case {
        text: "\u{1000}\u{103B}\u{103C}\u{103D}\u{103E}",
        expected: [
            "406=0+229,0@0,0#0|4=0+1124,0@0,0#0|382=0+257,0@0,0#0",
            "406=0+229,0@0,0#0|4=0+1124,0@0,0#0|382=0+257,0@0,0#0",
            "406=6+229,0@0,0#1|4=0+1124,0@0,0#0|382=3+257,0@0,0#1",
        ],
    },
    // Ka, medial ra, wa, sign e, sign i.
    Case {
        text: "\u{1000}\u{103C}\u{103D}\u{1031}\u{102D}",
        expected: [
            "372=0+618,0@0,0#0|197=0+229,0@0,0#0|4=0+1124,0@0,0#0|369=0+0,0@-27,0#0",
            "372=0+618,0@0,0#0|197=0+229,0@0,0#0|4=0+1124,0@0,0#0|369=12+0,0@-27,0#1",
            "372=9+618,0@0,0#1|197=3+229,0@0,0#1|4=0+1124,0@0,0#0|369=12+0,0@-27,0#1",
        ],
    },
    // Medial ra after wa starts a broken cluster.
    Case {
        text: "\u{1000}\u{103D}\u{103C}",
        expected: [
            "4=0+1124,0@0,0#0|48=0+0,0@-4,0#0|47=0+229,0@0,0#0|388=0+594,0@0,0#0",
            "4=0+1124,0@0,0#0|48=3+0,0@-4,0#1|47=6+229,0@0,0#1|388=6+594,0@0,0#1",
            "4=0+1124,0@0,0#0|48=3+0,0@-4,0#1|47=6+229,0@0,0#1|388=6+594,0@0,0#1",
        ],
    },
    // Medials with asats.
    Case {
        text: "\u{1000}\u{103B}\u{103A}\u{103C}\u{103D}\u{103E}\u{103A}",
        expected: [
            "406=0+229,0@0,0#0|4=0+1124,0@0,0#0|382=0+257,0@0,0#0|381=0+0,0@176,0#0|381=0+0,0@176,0#0",
            "406=0+229,0@0,0#0|4=0+1124,0@0,0#0|382=0+257,0@0,0#0|381=0+0,0@176,0#0|381=18+0,0@176,0#1",
            "406=9+229,0@0,0#1|4=0+1124,0@0,0#0|382=3+257,0@0,0#1|381=6+0,0@176,0#1|381=18+0,0@176,0#1",
        ],
    },
    // Mon medials.
    Case {
        text: "\u{1000}\u{105E}\u{1060}",
        expected: [
            "4=0+1124,0@0,0#0|315=0+0,0@-10,-24#0|317=0+0,0@5,-436#0",
            "4=0+1124,0@0,0#0|315=3+0,0@-10,-24#1|317=6+0,0@5,-436#1",
            "4=0+1124,0@0,0#0|315=3+0,0@-10,-24#1|317=6+0,0@5,-436#1",
        ],
    },
];

const PRE_BASE: &[Case] = &[
    // Ka, sign e.
    Case {
        text: "\u{1000}\u{1031}",
        expected: [
            "372=0+618,0@0,0#0|4=0+1124,0@0,0#0",
            "372=0+618,0@0,0#0|4=0+1124,0@0,0#0",
            "372=3+618,0@0,0#1|4=0+1124,0@0,0#0",
        ],
    },
    // Two signs e flip.
    Case {
        text: "\u{1000}\u{1031}\u{1031}",
        expected: [
            "372=0+618,0@0,0#0|372=0+618,0@0,0#0|4=0+1124,0@0,0#0",
            "372=0+618,0@0,0#0|372=0+618,0@0,0#0|4=0+1124,0@0,0#0",
            "372=6+618,0@0,0#1|372=3+618,0@0,0#1|4=0+1124,0@0,0#0",
        ],
    },
    // A selector stays after its sign e.
    Case {
        text: "\u{1000}\u{1031}\u{FE00}\u{1031}",
        expected: [
            "372=0+618,0@0,0#0|547=0+618,0@0,0#0|4=0+1124,0@0,0#0",
            "372=0+618,0@0,0#0|547=0+618,0@0,0#0|4=0+1124,0@0,0#0",
            "372=9+618,0@0,0#1|547=3+618,0@0,0#1|4=0+1124,0@0,0#0",
        ],
    },
    // Shan sign e and sign e.
    Case {
        text: "\u{1000}\u{1084}\u{1031}",
        expected: [
            "372=0+618,0@0,0#0|118=0+0,0@0,0#0|4=0+1124,0@0,0#0",
            "372=0+618,0@0,0#0|118=0+0,0@0,0#0|4=0+1124,0@0,0#0",
            "372=6+618,0@0,0#1|118=3+0,0@0,0#1|4=0+1124,0@0,0#0",
        ],
    },
    // A full syllable.
    Case {
        text: "\u{1000}\u{103B}\u{1031}\u{1031}\u{102D}\u{102F}\u{1036}\u{102C}\u{103A}",
        expected: [
            "372=0+618,0@0,0#0|372=0+618,0@0,0#0|4=0+1124,0@0,0#0|382=0+257,0@0,0#0|181=0+0,0@245,0#0|210=0+261,0@0,0#0|610=0+0,0@0,0#0|368=0+455,0@0,0#0|381=0+0,0@-36,0#0",
            "372=0+618,0@0,0#0|372=0+618,0@0,0#0|4=0+1124,0@0,0#0|382=0+257,0@0,0#0|181=12+0,0@245,0#1|210=12+261,0@0,0#1|610=12+0,0@0,0#1|368=21+455,0@0,0#1|381=24+0,0@-36,0#1",
            "372=9+618,0@0,0#1|372=6+618,0@0,0#1|4=0+1124,0@0,0#0|382=3+257,0@0,0#1|181=12+0,0@245,0#1|210=15+261,0@0,0#1|610=15+0,0@0,0#1|368=21+455,0@0,0#1|381=24+0,0@-36,0#1",
        ],
    },
];

const STACKS: &[Case] = &[
    // Ka, virama, kha, sign e.
    Case {
        text: "\u{1000}\u{1039}\u{1001}\u{1031}",
        expected: [
            "372=0+618,0@0,0#0|4=0+1124,0@0,0#0|213=0+0,0@-9,-24#0",
            "372=0+618,0@0,0#0|4=0+1124,0@0,0#0|213=0+0,0@-9,-24#0",
            "372=9+618,0@0,0#1|4=0+1124,0@0,0#0|213=3+0,0@-9,-24#1",
        ],
    },
    // Two stacked consonants and medial ya.
    Case {
        text: "\u{1000}\u{1039}\u{1001}\u{1039}\u{1002}\u{103B}",
        expected: [
            "4=0+1124,0@0,0#0|214=0+0,0@-9,-24#0|216=0+0,0@-9,-436#0|364=0+257,0@0,0#0",
            "4=0+1124,0@0,0#0|214=3+0,0@-9,-24#1|216=9+0,0@-9,-436#1|364=15+257,0@0,0#1",
            "4=0+1124,0@0,0#0|214=3+0,0@-9,-24#1|216=9+0,0@-9,-436#1|364=15+257,0@0,0#1",
        ],
    },
    // Thuu, kakkhang.
    Case {
        text: "\u{101E}\u{1030}\u{1000}\u{1039}\u{1001}\u{1036}",
        expected: [
            "34=0+1127,0@0,0#0|360=0+0,0@-21,0#0|4=6+1124,0@0,0#0|213=6+0,0@-9,-24#0|377=6+0,0@-27,0#0",
            "34=0+1127,0@0,0#0|360=3+0,0@-21,0#1|4=6+1124,0@0,0#0|213=9+0,0@-9,-24#1|377=15+0,0@-27,0#1",
            "34=0+1127,0@0,0#0|360=3+0,0@-21,0#1|4=6+1124,0@0,0#0|213=9+0,0@-9,-24#1|377=15+0,0@-27,0#1",
        ],
    },
    // A stacked independent vowel.
    Case {
        text: "\u{1000}\u{1039}\u{1021}",
        expected: [
            "4=0+1124,0@0,0#0|289=0+0,0@-235,0#0",
            "4=0+1124,0@0,0#0|289=3+0,0@-235,0#1",
            "4=0+1124,0@0,0#0|289=3+0,0@-235,0#1",
        ],
    },
];

const SIGNS: &[Case] = &[
    // Ka, asat.
    Case {
        text: "\u{1000}\u{103A}",
        expected: [
            "4=0+1124,0@0,0#0|381=0+0,0@-27,0#0",
            "4=0+1124,0@0,0#0|381=3+0,0@-27,0#1",
            "4=0+1124,0@0,0#0|381=3+0,0@-27,0#1",
        ],
    },
    // Ka, sign e, sign aa, asat.
    Case {
        text: "\u{1000}\u{1031}\u{102C}\u{103A}",
        expected: [
            "372=0+618,0@0,0#0|4=0+1124,0@0,0#0|368=0+455,0@0,0#0|381=0+0,0@-36,0#0",
            "372=0+618,0@0,0#0|4=0+1124,0@0,0#0|368=6+455,0@0,0#1|381=9+0,0@-36,0#1",
            "372=3+618,0@0,0#1|4=0+1124,0@0,0#0|368=6+455,0@0,0#1|381=9+0,0@-36,0#1",
        ],
    },
    // Sign u, anusvara, dot below, visarga.
    Case {
        text: "\u{1000}\u{102F}\u{1036}\u{1037}\u{1038}",
        expected: [
            "4=0+1124,0@0,0#0|377=0+0,0@-27,0#0|398=0+0,0@-140,0#0|379=0+346,0@0,0#0",
            "4=0+1124,0@0,0#0|377=3+0,0@-27,0#1|398=3+0,0@-140,0#1|379=12+346,0@0,0#1",
            "4=0+1124,0@0,0#0|377=6+0,0@-27,0#1|398=3+0,0@-140,0#1|379=12+346,0@0,0#1",
        ],
    },
    // Marks after a sign u sort before it.
    Case {
        text: "\u{1000}\u{102F}\u{1032}\u{1036}",
        expected: [
            "4=0+1124,0@0,0#0|373=0+0,0@-27,20#0|377=0+0,0@-27,0#0|209=0+0,0@-5,0#0",
            "4=0+1124,0@0,0#0|373=3+0,0@-27,20#1|377=3+0,0@-27,0#1|209=3+0,0@-5,0#1",
            "4=0+1124,0@0,0#0|373=6+0,0@-27,20#1|377=9+0,0@-27,0#1|209=3+0,0@-5,0#1",
        ],
    },
    // A Pwo tone with signs.
    Case {
        text: "\u{1000}\u{1063}\u{1036}\u{1037}\u{103A}",
        expected: [
            "4=0+1124,0@0,0#0|85=0+661,0@0,0#0|377=0+0,0@-22,0#0|378=0+0,0@210,0#0|381=0+0,0@-22,0#0",
            "4=0+1124,0@0,0#0|85=3+661,0@0,0#1|377=6+0,0@-22,0#1|378=9+0,0@210,0#1|381=12+0,0@-22,0#1",
            "4=0+1124,0@0,0#0|85=3+661,0@0,0#1|377=6+0,0@-22,0#1|378=9+0,0@210,0#1|381=12+0,0@-22,0#1",
        ],
    },
    // A second visarga is broken.
    Case {
        text: "\u{1000}\u{1038}\u{1038}",
        expected: [
            "4=0+1124,0@0,0#0|379=0+346,0@0,0#0|379=0+346,0@0,0#0",
            "4=0+1124,0@0,0#0|379=3+346,0@0,0#1|379=6+346,0@0,0#1",
            "4=0+1124,0@0,0#0|379=3+346,0@0,0#1|379=6+346,0@0,0#1",
        ],
    },
];

const JOINERS: &[Case] = &[
    // ZWJ before sign e.
    Case {
        text: "\u{1000}\u{200D}\u{1031}",
        expected: [
            "4=0+1124,0@0,0#0|3=0+0,0@0,0#0|372=0+618,0@0,0#0|388=0+594,0@0,0#0",
            "4=0+1124,0@0,0#0|3=3+0,0@0,0#1|372=6+618,0@0,0#1|388=6+594,0@0,0#1",
            "4=0+1124,0@0,0#0|3=3+0,0@0,0#1|372=6+618,0@0,0#1|388=6+594,0@0,0#1",
        ],
    },
    // ZWNJ after sign e.
    Case {
        text: "\u{1000}\u{1031}\u{200C}",
        expected: [
            "372=0+618,0@0,0#0|4=0+1124,0@0,0#0|3=6+0,0@0,0#1",
            "372=0+618,0@0,0#0|4=0+1124,0@0,0#0|3=6+0,0@0,0#1",
            "372=3+618,0@0,0#1|4=0+1124,0@0,0#0|3=6+0,0@0,0#1",
        ],
    },
    // A lone ZWJ.
    Case {
        text: "\u{200D}",
        expected: ["3=0+0,0@0,0#0", "3=0+0,0@0,0#0", "3=0+0,0@0,0#0"],
    },
    // Asat, ZWJ, sign e.
    Case {
        text: "\u{1000}\u{103A}\u{200D}\u{1031}",
        expected: [
            "4=0+1124,0@0,0#0|381=0+0,0@-27,0#0|3=0+0,0@0,0#0|372=0+618,0@0,0#0|388=0+594,0@0,0#0",
            "4=0+1124,0@0,0#0|381=3+0,0@-27,0#1|3=6+0,0@0,0#1|372=9+618,0@0,0#1|388=9+594,0@0,0#1",
            "4=0+1124,0@0,0#0|381=3+0,0@-27,0#1|3=6+0,0@0,0#1|372=9+618,0@0,0#1|388=9+594,0@0,0#1",
        ],
    },
];

const BROKEN: &[Case] = &[
    // Sign e alone.
    Case {
        text: "\u{1031}",
        expected: [
            "372=0+618,0@0,0#0|388=0+594,0@0,0#0",
            "372=0+618,0@0,0#0|388=0+594,0@0,0#0",
            "372=0+618,0@0,0#0|388=0+594,0@0,0#0",
        ],
    },
    // Medial ra and sign e alone.
    Case {
        text: "\u{103C}\u{1031}",
        expected: [
            "372=0+618,0@0,0#0|47=0+229,0@0,0#0|388=0+594,0@0,0#0",
            "372=0+618,0@0,0#0|47=0+229,0@0,0#0|388=0+594,0@0,0#0",
            "372=3+618,0@0,0#1|47=0+229,0@0,0#0|388=0+594,0@0,0#0",
        ],
    },
    // A broken cluster before a syllable.
    Case {
        text: "\u{1031}\u{1000}\u{1031}",
        expected: [
            "372=0+618,0@0,0#0|388=0+594,0@0,0#0|372=3+618,0@0,0#0|4=3+1124,0@0,0#0",
            "372=0+618,0@0,0#0|388=0+594,0@0,0#0|372=3+618,0@0,0#0|4=3+1124,0@0,0#0",
            "372=0+618,0@0,0#0|388=0+594,0@0,0#0|372=6+618,0@0,0#1|4=3+1124,0@0,0#0",
        ],
    },
    // A typed dotted circle is a base.
    Case {
        text: "\u{25CC}\u{1031}\u{103C}",
        expected: [
            "372=0+618,0@0,0#0|388=0+594,0@0,0#0|47=0+229,0@0,0#0|388=0+594,0@0,0#0",
            "372=0+618,0@0,0#0|388=0+594,0@0,0#0|47=6+229,0@0,0#1|388=6+594,0@0,0#1",
            "372=3+618,0@0,0#1|388=0+594,0@0,0#0|47=6+229,0@0,0#1|388=6+594,0@0,0#1",
        ],
    },
    // Locative is a consonant.
    Case {
        text: "\u{104E}\u{1031}\u{102C}",
        expected: [
            "372=0+618,0@0,0#0|64=0+659,0@0,0#0|368=0+455,0@0,0#0",
            "372=0+618,0@0,0#0|64=0+659,0@0,0#0|368=6+455,0@0,0#1",
            "372=3+618,0@0,0#1|64=0+659,0@0,0#0|368=6+455,0@0,0#1",
        ],
    },
];

const DIGITS_AND_EXTENSIONS: &[Case] = &[
    // Digits and sign e.
    Case {
        text: "\u{1040}\u{1041}\u{1031}",
        expected: [
            "50=0+652,0@0,0#0|372=3+618,0@0,0#0|51=3+623,0@0,0#0",
            "50=0+652,0@0,0#0|372=3+618,0@0,0#0|51=3+623,0@0,0#0",
            "50=0+652,0@0,0#0|372=6+618,0@0,0#1|51=3+623,0@0,0#0",
        ],
    },
    // Shan digit, asat.
    Case {
        text: "\u{1090}\u{103A}",
        expected: [
            "130=0+761,0@0,0#0|381=0+0,0@0,0#0",
            "130=0+761,0@0,0#0|381=3+0,0@0,0#1",
            "130=0+761,0@0,0#0|381=3+0,0@0,0#1",
        ],
    },
    // Myanmar Extended-B digit, anusvara.
    Case {
        text: "\u{A9F0}\u{1036}",
        expected: [
            "519=0+761,0@0,0#0|377=0+0,0@0,0#0",
            "519=0+761,0@0,0#0|377=3+0,0@0,0#1",
            "519=0+761,0@0,0#0|377=3+0,0@0,0#1",
        ],
    },
    // Myanmar Extended-A consonant with Shan signs.
    Case {
        text: "\u{AA60}\u{1031}\u{1083}\u{1038}",
        expected: [
            "372=0+618,0@0,0#0|146=0+1125,0@0,0#0|117=0+0,0@0,0#0|379=0+346,0@0,0#0",
            "372=0+618,0@0,0#0|146=0+1125,0@0,0#0|117=6+0,0@0,0#1|379=9+346,0@0,0#1",
            "372=3+618,0@0,0#1|146=0+1125,0@0,0#0|117=6+0,0@0,0#1|379=9+346,0@0,0#1",
        ],
    },
    // Myanmar Extended-A tones.
    Case {
        text: "\u{AA71}\u{AA7B}\u{AA7C}",
        expected: [
            "163=0+1122,0@0,0#0|173=0+346,0@0,0#0|499=0+0,0@0,0#0",
            "163=0+1122,0@0,0#0|173=3+346,0@0,0#1|499=6+0,0@0,0#1",
            "163=0+1122,0@0,0#0|173=3+346,0@0,0#1|499=6+0,0@0,0#1",
        ],
    },
    // Myanmar Extended-B consonant and sign.
    Case {
        text: "\u{A9E0}\u{1031}\u{A9E5}",
        expected: [
            "372=0+618,0@0,0#0|503=0+676,0@0,0#0|508=0+0,0@-44,0#0",
            "372=0+618,0@0,0#0|503=0+676,0@0,0#0|508=6+0,0@-44,0#1",
            "372=3+618,0@0,0#1|503=0+676,0@0,0#0|508=6+0,0@-44,0#1",
        ],
    },
    // Shan medial wa, sign e, tone.
    Case {
        text: "\u{1075}\u{1082}\u{1084}\u{1087}",
        expected: [
            "118=0+0,0@0,0#0|103=0+667,0@0,0#0|116=0+0,0@-85,0#0|121=0+347,0@0,0#0",
            "118=0+0,0@0,0#0|103=0+667,0@0,0#0|116=0+0,0@-85,0#0|121=9+347,0@0,0#1",
            "118=6+0,0@0,0#1|103=0+667,0@0,0#0|116=3+0,0@-85,0#1|121=9+347,0@0,0#1",
        ],
    },
];

const VERTICAL: &[(&str, &str)] = &[
    (
        "\u{1000}\u{1031}\u{102C}",
        "372=0+0,-2184@-309,-1360#0|4=0+0,-2184@-562,-1360#0|368=0+0,-2184@-227,-1360#0",
    ),
    (
        "\u{1004}\u{103A}\u{1039}\u{1000}\u{103C}",
        "200=0+0,-2184@-114,-1360#0|4=0+0,-2184@-562,-1360#0|189=0+0,0@561,824#0",
    ),
];
