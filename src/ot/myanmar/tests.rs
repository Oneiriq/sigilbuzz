//! Tests for the Myanmar pass: syllable segmentation, the reorder,
//! and its cluster merges.

use super::*;
use alloc::vec;

fn cps(s: &str) -> Vec<char> {
    s.chars().collect()
}

fn fake_glyphs(n: usize) -> Vec<Glyph> {
    (0..n).map(|i| Glyph::new(i as u32 + 1, i as u32)).collect()
}

fn reordered(text: &str, level: ClusterLevel) -> Vec<Glyph> {
    let cp = cps(text);
    let mut glyphs = fake_glyphs(cp.len());
    for s in &segment_syllables(&cp) {
        initial_reorder(&cp, &mut glyphs, s, level);
    }
    glyphs
}

fn ids(glyphs: &[Glyph]) -> Vec<u32> {
    glyphs.iter().map(|g| g.glyph_id).collect()
}

fn clusters(glyphs: &[Glyph]) -> Vec<u32> {
    glyphs.iter().map(|g| g.cluster).collect()
}

#[test]
fn consonants_take_their_signs_and_stacks() {
    // Ka: one base, one syllable.
    let syl = segment_syllables(&cps("\u{1000}"));
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].kind, SyllableKind::Consonant);
    assert_eq!(
        (syl[0].start, syl[0].end, syl[0].base_index),
        (0, 1, Some(0))
    );
    // Ka, aa.
    let syl = segment_syllables(&cps("\u{1000}\u{102C}"));
    assert_eq!((syl.len(), syl[0].end), (1, 2));
    // Ka, virama, kha: the last consonant is the base.
    let syl = segment_syllables(&cps("\u{1000}\u{1039}\u{1001}"));
    assert_eq!(syl.len(), 1);
    assert_eq!((syl[0].end, syl[0].base_index), (3, Some(2)));
    // Ka, anusvara: the final mark stays in the syllable.
    let syl = segment_syllables(&cps("\u{1000}\u{1036}"));
    assert_eq!((syl.len(), syl[0].end), (1, 2));
}

#[test]
fn vowels_digits_and_broken_syllables() {
    // An independent vowel.
    let syl = segment_syllables(&cps("\u{1021}"));
    assert_eq!(syl[0].kind, SyllableKind::Vowel);
    // Three digits are three syllables.
    let syl = segment_syllables(&cps("\u{1040}\u{1041}\u{1042}"));
    assert_eq!(syl.len(), 3);
    assert!(syl.iter().all(|s| s.kind == SyllableKind::Symbol));
    // A leading vowel sign is broken, and the scan advances past it.
    let syl = segment_syllables(&cps("\u{102C}\u{1000}"));
    assert_eq!(syl.len(), 2);
    assert_eq!((syl[0].kind, syl[0].end), (SyllableKind::Broken, 1));
    assert!(segment_syllables(&[]).is_empty());
}

#[test]
fn pre_base_vowel_moves_to_the_syllable_start() {
    // Ka, sign e: the sign e moves in front, merging clusters only at
    // the monotone levels.
    let g = reordered("\u{1000}\u{1031}", ClusterLevel::Characters);
    assert_eq!((ids(&g), clusters(&g)), (vec![2, 1], vec![1, 0]));
    let g = reordered("\u{1000}\u{1031}", ClusterLevel::MonotoneCharacters);
    assert_eq!((ids(&g), clusters(&g)), (vec![2, 1], vec![0, 0]));
    // Ka, aa: nothing moves.
    let g = reordered("\u{1000}\u{102C}", ClusterLevel::Characters);
    assert_eq!(g, fake_glyphs(2));
    // Ka, virama, kha, sign e: the sign moves to the very start, and
    // every glyph it passes shares its cluster.
    let g = reordered(
        "\u{1000}\u{1039}\u{1001}\u{1031}",
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!(
        (ids(&g), clusters(&g)),
        (vec![4, 1, 2, 3], vec![0, 0, 0, 0])
    );
    // Two signs e before an aa keep their order, and the aa is not
    // crossed.
    let g = reordered(
        "\u{1000}\u{1031}\u{1031}\u{102C}",
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!(
        (ids(&g), clusters(&g)),
        (vec![2, 3, 1, 4], vec![0, 0, 0, 3])
    );
}

#[test]
fn kinzi_moves_after_the_base() {
    // Nga, asat, virama, ka: the kinzi goes after the ka.
    let g = reordered(
        "\u{1004}\u{103A}\u{1039}\u{1000}",
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!(
        (ids(&g), clusters(&g)),
        (vec![4, 1, 2, 3], vec![0, 0, 0, 0])
    );
}

#[test]
fn shape_without_gsub_only_reorders() {
    let cp = cps("\u{1000}\u{1031}");
    let mut glyphs = fake_glyphs(2);
    shape_myanmar(
        None,
        None,
        &cp,
        &mut glyphs,
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!((ids(&glyphs), clusters(&glyphs)), (vec![2, 1], vec![0, 0]));
}

#[test]
fn long_run_of_pre_base_signs_reorders_in_linear_time() {
    // Ka followed by 200000 signs e is one syllable whose signs all
    // move to its start.
    const N: usize = 200_000;
    let mut cp = vec!['\u{1000}'];
    cp.extend(core::iter::repeat('\u{1031}').take(N));
    let mut glyphs = fake_glyphs(cp.len());
    shape_myanmar(
        None,
        None,
        &cp,
        &mut glyphs,
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!(glyphs.len(), N + 1);
    assert_eq!(glyphs[0].glyph_id, 2);
    assert_eq!(glyphs[N].glyph_id, 1);
}
