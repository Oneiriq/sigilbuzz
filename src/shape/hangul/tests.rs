//! Hangul preprocessing tests with a made-up font: which characters it
//! maps and which it draws with no advance.

use super::*;
use alloc::vec;

/// Runs the preprocessing over `text` at `level`, with a font that maps
/// every character but those in `missing` and draws those in `zero`
/// with no advance. Returns the characters, clusters, and features.
fn run(
    text: &str,
    missing: &[char],
    zero: &[char],
    circle: bool,
    level: ClusterLevel,
) -> (Vec<char>, Vec<u32>, Vec<u8>) {
    let mut cps: Vec<char> = text.chars().collect();
    let mut glyphs: Vec<Glyph> = text
        .char_indices()
        .map(|(i, _)| Glyph::new(0, i as u32))
        .collect();
    let mut mirrored = vec![false; cps.len()];
    let has_glyph = |c: char| !missing.contains(&c);
    let zero_width = |c: char| zero.contains(&c);
    let font = HangulFont {
        has_glyph: &has_glyph,
        zero_width: &zero_width,
        dotted_circle: circle,
    };
    let features = preprocess(&mut cps, &mut glyphs, &mut mirrored, &font, level);
    let clusters = glyphs.iter().map(|g| g.cluster).collect();
    (cps, clusters, features)
}

const CHARS: ClusterLevel = ClusterLevel::Characters;
const MONO: ClusterLevel = ClusterLevel::MonotoneCharacters;
const GRAPH: ClusterLevel = ClusterLevel::MonotoneGraphemes;

#[test]
fn modern_jamo_compose_when_the_font_has_the_syllable() {
    // Kiyeok, a, kiyeok: GAG.
    let (cps, clusters, f) = run("\u{1100}\u{1161}\u{11A8}", &[], &[], true, MONO);
    assert_eq!(cps, ['\u{AC01}']);
    assert_eq!(clusters, [0]);
    assert_eq!(f, [jamo::NONE]);
    // Without the syllable the jamo stay, with their features, and
    // share a cluster at the grapheme levels only.
    let missing = ['\u{AC01}'];
    let (cps, clusters, f) = run("\u{1100}\u{1161}\u{11A8}", &missing, &[], true, MONO);
    assert_eq!(cps, ['\u{1100}', '\u{1161}', '\u{11A8}']);
    assert_eq!(clusters, [0, 3, 6]);
    assert_eq!(f, [jamo::LJMO, jamo::VJMO, jamo::TJMO]);
    let (_, clusters, _) = run("\u{1100}\u{1161}\u{11A8}", &missing, &[], true, GRAPH);
    assert_eq!(clusters, [0, 0, 0]);
}

#[test]
fn old_jamo_keep_their_features() {
    // Old Hangul choseong ssangkiyeok-like A960 never composes.
    let (cps, _, f) = run("\u{A960}\u{1161}", &[], &[], true, MONO);
    assert_eq!(cps, ['\u{A960}', '\u{1161}']);
    assert_eq!(f, [jamo::LJMO, jamo::VJMO]);
    // A lone vowel jamo is no syllable.
    let (_, _, f) = run("\u{1161}", &[], &[], true, MONO);
    assert_eq!(f, [jamo::NONE]);
}

#[test]
fn syllables_decompose_when_the_font_lacks_them() {
    let (cps, clusters, f) = run("\u{AC01}", &['\u{AC01}'], &[], true, GRAPH);
    assert_eq!(cps, ['\u{1100}', '\u{1161}', '\u{11A8}']);
    assert_eq!(clusters, [0, 0, 0]);
    assert_eq!(f, [jamo::LJMO, jamo::VJMO, jamo::TJMO]);
    // LV with a trailing jamo that cannot join it: the LV decomposes
    // and takes the jamo along.
    let (cps, _, f) = run("\u{AC00}\u{11C3}", &[], &[], true, MONO);
    assert_eq!(cps, ['\u{1100}', '\u{1161}', '\u{11C3}']);
    assert_eq!(f, [jamo::LJMO, jamo::VJMO, jamo::TJMO]);
    // LV and a modern trailing jamo compose.
    let (cps, clusters, _) = run("\u{AC00}\u{11A8}", &[], &[], true, MONO);
    assert_eq!(cps, ['\u{AC01}']);
    assert_eq!(clusters, [0]);
}

#[test]
fn tone_mark_moves_in_front_of_its_syllable() {
    let (cps, clusters, _) = run("\u{AC00}\u{302E}", &[], &[], true, MONO);
    assert_eq!(cps, ['\u{302E}', '\u{AC00}']);
    assert_eq!(clusters, [0, 0]);
    // Characters level: the move keeps each cluster.
    let (cps, clusters, _) = run("\u{AC00}\u{302E}", &[], &[], true, CHARS);
    assert_eq!(cps, ['\u{302E}', '\u{AC00}']);
    assert_eq!(clusters, [3, 0]);
    // A whole jamo syllable.
    let missing = ['\u{AC01}'];
    let (cps, _, f) = run(
        "\u{1100}\u{1161}\u{11A8}\u{302F}",
        &missing,
        &[],
        true,
        MONO,
    );
    assert_eq!(cps, ['\u{302F}', '\u{1100}', '\u{1161}', '\u{11A8}']);
    assert_eq!(f, [jamo::NONE, jamo::LJMO, jamo::VJMO, jamo::TJMO]);
}

#[test]
fn zero_width_tone_mark_stays_to_overstrike() {
    let (cps, clusters, _) = run("\u{AC00}\u{302E}", &[], &['\u{302E}'], true, MONO);
    assert_eq!(cps, ['\u{AC00}', '\u{302E}']);
    assert_eq!(clusters, [0, 3]);
}

#[test]
fn tone_mark_without_a_syllable_gets_a_dotted_circle() {
    let (cps, clusters, _) = run("\u{302E}", &[], &[], true, MONO);
    assert_eq!(cps, ['\u{302E}', '\u{25CC}']);
    assert_eq!(clusters, [0, 0]);
    // A zero-width one goes after the circle.
    let (cps, _, _) = run("\u{302E}", &[], &['\u{302E}'], true, MONO);
    assert_eq!(cps, ['\u{25CC}', '\u{302E}']);
    // A second tone mark has no syllable either.
    let (cps, _, _) = run("\u{AC00}\u{302E}\u{302F}", &[], &[], true, MONO);
    assert_eq!(cps, ['\u{302E}', '\u{AC00}', '\u{302F}', '\u{25CC}']);
    // A lone leading jamo is no syllable.
    let (cps, _, _) = run("\u{1100}\u{302E}", &[], &[], true, MONO);
    assert_eq!(cps, ['\u{1100}', '\u{302E}', '\u{25CC}']);
    // Without a dotted circle the mark stays alone.
    let (cps, _, _) = run("\u{302E}", &[], &[], false, MONO);
    assert_eq!(cps, ['\u{302E}']);
}

#[test]
fn feature_reading_matches_preprocessing() {
    let cps: Vec<char> = "\u{1100}\u{1161}\u{11A8}\u{1100}\u{1100}\u{1161}\u{1161}"
        .chars()
        .collect();
    assert_eq!(
        jamo_features(&cps),
        [
            jamo::LJMO,
            jamo::VJMO,
            jamo::TJMO,
            jamo::NONE,
            jamo::LJMO,
            jamo::VJMO,
            jamo::NONE
        ]
    );
}

#[test]
fn long_runs_stay_linear() {
    // 100000 syllables each with a tone mark, and 100000 broken tone
    // marks.
    let text: alloc::string::String = core::iter::repeat("\u{AC00}\u{302E}")
        .take(100_000)
        .chain(core::iter::repeat("\u{302F}").take(100_000))
        .collect();
    let (cps, _, _) = run(&text, &[], &[], true, GRAPH);
    assert_eq!(cps.len(), 400_000);
    assert_eq!(cps[0], '\u{302E}');
    assert_eq!(cps[399_999], '\u{25CC}');
}
