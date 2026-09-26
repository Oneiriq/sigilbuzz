//! Cluster-level parity: every place shaping forms or merges clusters
//! follows the buffer's cluster level the way HarfBuzz does.
//!
//! Each case is shaped at MONOTONE_GRAPHEMES, MONOTONE_CHARACTERS, and
//! CHARACTERS by sigilbuzz and by rustybuzz 0.20 (a port of HarfBuzz),
//! and glyph ids, clusters, advances, and offsets must agree. The cases
//! cover grapheme forming (marks, variation selectors, ZWJ emoji,
//! regional indicators, tag sequences), ligatures, Arabic and Hebrew
//! marks, Indic pre-base matras and repha, Khmer and Myanmar
//! reordering, USE `pref`, Thai and Lao SARA AM, Old Hangul jamo, and
//! text in a direction that is not its script's native one.
//!
//! rustybuzz predates HarfBuzz's fourth level, GRAPHEMES, so that one
//! is pinned against its definition instead: grapheme forming as at
//! MONOTONE_GRAPHEMES, no monotone merges, as at CHARACTERS.
//!
//! One known difference is left out: Sinhala split vowel signs
//! (U+0DDC..U+0DDE) that sigilbuzz splits before the cmap lookup
//! come out with the second part's cluster unmerged at
//! MONOTONE_CHARACTERS, where rustybuzz merges it.

use rustybuzz::{BufferClusterLevel, Direction as RbDirection};
use sigilbuzz::{shape, Blob, Buffer, ClusterLevel, Direction, Face, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");
const HEBREW: &[u8] = include_bytes!("fonts/NotoSansHebrew-Regular.ttf");
const DEVANAGARI: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Regular.ttf");
const KHMER: &[u8] = include_bytes!("fonts/NotoSansKhmer-Regular.ttf");
const MYANMAR: &[u8] = include_bytes!("fonts/NotoSansMyanmar-Regular.ttf");
const THAI: &[u8] = include_bytes!("fonts/NotoSansThai-Regular.ttf");
const LAO: &[u8] = include_bytes!("fonts/NotoSansLao-Regular.ttf");
const CHAM: &[u8] = include_bytes!("fonts/NotoSansCham-Regular.ttf");
const OLD_HANGUL: &[u8] = include_bytes!("fonts/NotoSansOldHangul-Subset.ttf");

type Row = (u32, u32, i32, i32, i32, i32);

const LEVELS: [(ClusterLevel, BufferClusterLevel); 3] = [
    (
        ClusterLevel::MonotoneGraphemes,
        BufferClusterLevel::MonotoneGraphemes,
    ),
    (
        ClusterLevel::MonotoneCharacters,
        BufferClusterLevel::MonotoneCharacters,
    ),
    (ClusterLevel::Characters, BufferClusterLevel::Characters),
];

fn sigilbuzz_rows(data: &[u8], text: &str, direction: Direction, level: ClusterLevel) -> Vec<Row> {
    let blob = Blob::new(data);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    buffer.set_cluster_level(level);
    shape(&font, &buffer, &[])
        .expect("sigilbuzz shape")
        .glyphs
        .iter()
        .map(|g| {
            (
                g.glyph_id,
                g.cluster,
                g.x_advance,
                g.y_advance,
                g.x_offset,
                g.y_offset,
            )
        })
        .collect()
}

fn rustybuzz_rows(
    data: &[u8],
    text: &str,
    direction: Direction,
    level: BufferClusterLevel,
) -> Vec<Row> {
    let face = rustybuzz::Face::from_slice(data, 0).expect("parse rustybuzz face");
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_direction(match direction {
        Direction::Ltr => RbDirection::LeftToRight,
        Direction::Rtl => RbDirection::RightToLeft,
        Direction::Ttb => RbDirection::TopToBottom,
        Direction::Btt => RbDirection::BottomToTop,
    });
    buffer.set_cluster_level(level);
    let out = rustybuzz::shape(&face, &[], buffer);
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| {
            (
                i.glyph_id,
                i.cluster,
                p.x_advance,
                p.y_advance,
                p.x_offset,
                p.y_offset,
            )
        })
        .collect()
}

/// Asserts parity at every level rustybuzz has, for each case.
fn assert_parity(data: &[u8], direction: Direction, texts: &[&str]) {
    let mut failures = Vec::new();
    for &text in texts {
        for (ours, theirs) in LEVELS {
            let a = sigilbuzz_rows(data, text, direction, ours);
            let b = rustybuzz_rows(data, text, direction, theirs);
            if a != b {
                failures.push(format!(
                    "{text:?} {direction:?} {ours:?}\n  sigilbuzz: {a:?}\n  rustybuzz: {b:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn clusters(data: &[u8], text: &str, direction: Direction, level: ClusterLevel) -> Vec<u32> {
    sigilbuzz_rows(data, text, direction, level)
        .iter()
        .map(|r| r.1)
        .collect()
}

#[test]
fn latin_marks_ligatures_and_emoji_sequences() {
    assert_parity(
        OPEN_SANS,
        Direction::Ltr,
        &[
            // Base and mark pairs with no precomposed form, so
            // HarfBuzz's normalizer leaves them as they are.
            "x\u{0301}",
            "x\u{0301}\u{0302}y",
            "q\u{0308}\u{0301}ffi",
            "office",
            "fix\u{0301}",
            "a\u{200D}b",
            "a\u{200C}b",
            "A\u{FE0F}B",
            "x\u{1F3FB}y",
            "\u{1F468}\u{200D}\u{1F469}",
            "\u{1F1EB}\u{1F1F7}\u{1F1E9}",
            "\u{1F3F4}\u{E0067}\u{E0062}\u{E007F}",
            "\u{FF76}\u{FF9E}",
            "\u{0301}a",
            "Hello, world!",
        ],
    );
}

#[test]
fn arabic_and_hebrew_marks() {
    assert_parity(
        AMIRI,
        Direction::Rtl,
        &[
            "\u{0644}\u{0627}",
            "\u{0644}\u{064E}\u{0627}",
            "\u{0628}\u{0650}\u{0633}\u{0652}\u{0645}\u{0650}",
            "\u{0627}\u{0644}\u{0644}\u{0651}\u{064E}\u{0647}",
            "\u{0628}\u{200D}",
        ],
    );
    assert_parity(
        HEBREW,
        Direction::Rtl,
        &[
            "\u{05E9}\u{05C1}\u{05B8}\u{05DC}\u{05D5}\u{05B9}\u{05DD}",
            "\u{05D1}\u{05BC}\u{05B0}\u{05E8}\u{05B5}\u{05D0}",
        ],
    );
}

#[test]
fn indic_reordering_merges() {
    assert_parity(
        DEVANAGARI,
        Direction::Ltr,
        &[
            "\u{0915}\u{093F}",
            "\u{0915}\u{200D}\u{093F}",
            "\u{0930}\u{094D}\u{0915}",
            "\u{0930}\u{094D}\u{0915}\u{093F}",
            "\u{0939}\u{093F}\u{0928}\u{094D}\u{0926}\u{0940}",
            "\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}",
            "\u{0915} \u{093F}",
        ],
    );
}

#[test]
fn khmer_myanmar_and_use_reordering_merges() {
    assert_parity(
        KHMER,
        Direction::Ltr,
        &[
            "\u{1780}\u{17C1}",
            "\u{179F}\u{17D2}\u{178F}\u{17C1}",
            "\u{1780}\u{17D2}\u{179A}\u{17C1}",
            "\u{1780}\u{17C4}",
            "\u{1780}\u{17C1}\u{1781}\u{17D2}\u{1780}\u{17B6}",
        ],
    );
    assert_parity(
        MYANMAR,
        Direction::Ltr,
        &[
            "\u{1000}\u{1031}",
            "\u{1000}\u{102C}",
            "\u{1004}\u{103A}\u{1039}\u{1000}\u{1031}",
        ],
    );
    assert_parity(
        CHAM,
        Direction::Ltr,
        &["\u{AA06}\u{AA34}", "\u{AA06}\u{AA29}", "\u{AA06}\u{AA43}"],
    );
}

#[test]
fn thai_and_lao_sara_am() {
    assert_parity(
        THAI,
        Direction::Ltr,
        &[
            "\u{0E01}\u{0E33}",
            "\u{0E19}\u{0E49}\u{0E33}",
            "\u{0E14}\u{0E4B}\u{0E33}",
            "\u{0E01}\u{0E48}\u{0E32}",
            "\u{0E01}\u{0E33}\u{0E01}",
            "\u{0E33}",
        ],
    );
    assert_parity(
        LAO,
        Direction::Ltr,
        &["\u{0E81}\u{0EB3}", "\u{0E81}\u{0EC8}\u{0EB3}"],
    );
}

#[test]
fn old_hangul_jamo_sequences() {
    assert_parity(
        OLD_HANGUL,
        Direction::Ltr,
        &["\u{1100}\u{1161}\u{D7CB}", "\u{A960}\u{1161}"],
    );
}

#[test]
fn non_native_directions_reverse_graphemes() {
    // Arabic and Hebrew given left to right, Latin right to left: the
    // graphemes are reversed, and at MONOTONE_CHARACTERS each reversed
    // grapheme's clusters merge.
    assert_parity(
        AMIRI,
        Direction::Ltr,
        &[
            "\u{0628}\u{0650}\u{0633}\u{0652}\u{0645}\u{0650}",
            "\u{0644}\u{0627}",
        ],
    );
    assert_parity(
        HEBREW,
        Direction::Ltr,
        &["\u{05E9}\u{05C1}\u{05B8}\u{05DC}\u{05D5}\u{05B9}\u{05DD}"],
    );
    assert_parity(
        OPEN_SANS,
        Direction::Rtl,
        &["x\u{0301}fi", "q\u{0308}\u{0301}y"],
    );
}

#[test]
fn graphemes_level_forms_graphemes_without_monotone_merges() {
    let g = ClusterLevel::Graphemes;
    // Marks join their base, as at MONOTONE_GRAPHEMES.
    assert_eq!(
        clusters(OPEN_SANS, "e\u{0301}\u{0302}x", Direction::Ltr, g),
        clusters(
            OPEN_SANS,
            "e\u{0301}\u{0302}x",
            Direction::Ltr,
            ClusterLevel::MonotoneGraphemes
        )
    );
    // A ligature keeps its first component's cluster, as at CHARACTERS.
    assert_eq!(
        clusters(OPEN_SANS, "office", Direction::Ltr, g),
        clusters(
            OPEN_SANS,
            "office",
            Direction::Ltr,
            ClusterLevel::Characters
        )
    );
    // A pre-base matra joins its consonant's grapheme; no reordering
    // merge is needed for that.
    assert_eq!(
        clusters(DEVANAGARI, "\u{0915}\u{093F}", Direction::Ltr, g),
        [0, 0]
    );
    // Khmer sa, coeng, ta, sign-e: graphemes <sa coeng> and
    // <ta sign-e>. The sign-e moves to the front with its grapheme's
    // cluster and nothing merges across the move, so the clusters come
    // out of order; MONOTONE_GRAPHEMES merges the whole span instead.
    let khmer = "\u{179F}\u{17D2}\u{178F}\u{17C1}";
    let out_of_order = clusters(KHMER, khmer, Direction::Ltr, g);
    assert_eq!(out_of_order.first(), Some(&6));
    assert!(out_of_order.contains(&0));
    assert!(clusters(
        KHMER,
        khmer,
        Direction::Ltr,
        ClusterLevel::MonotoneGraphemes
    )
    .iter()
    .all(|&c| c == 0));
    // SARA AM and Old Hangul jamo merge at every grapheme level.
    assert_eq!(
        clusters(THAI, "\u{0E01}\u{0E33}", Direction::Ltr, g),
        [0, 0, 0]
    );
    assert_eq!(
        clusters(OLD_HANGUL, "\u{A960}\u{1161}", Direction::Ltr, g),
        [0, 0]
    );
}

#[test]
fn the_default_level_is_monotone_characters() {
    // A base and a mark that do not compose (normalization would fold
    // "e\u{0301}" into one glyph at every level).
    let text = "q\u{0301}";
    let blob = Blob::new(OPEN_SANS);
    let font = Font::new(Face::parse(&blob, 0).expect("face"), 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    let got: Vec<u32> = shape(&font, &buffer, &[])
        .expect("shape")
        .glyphs
        .iter()
        .map(|g| g.cluster)
        .collect();
    assert_eq!(
        got,
        clusters(
            OPEN_SANS,
            text,
            Direction::Ltr,
            ClusterLevel::MonotoneCharacters
        )
    );
    assert_eq!(got, [0, 1]);
}
