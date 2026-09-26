//! Tests for the USE shaper: syllable segmentation, pre-base
//! reordering, cluster merging, and the Khmer feature tables.

use super::reorder::initial_reorder;
use super::*;
use alloc::vec;

fn cps(s: &str) -> Vec<char> {
    s.chars().collect()
}

fn fake_glyphs(n: usize) -> Vec<Glyph> {
    (0..n).map(|i| Glyph::new(i as u32 + 1, i as u32)).collect()
}

#[test]
fn single_consonant_is_one_syllable() {
    // ក U+1780: one base, one syllable.
    let cp = cps("\u{1780}");
    let syl = segment_syllables(&cp);
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].kind, SyllableKind::Consonant);
    assert_eq!(syl[0].start, 0);
    assert_eq!(syl[0].end, 1);
    assert_eq!(syl[0].base_index, Some(0));
}

#[test]
fn consonant_plus_post_base_matra_is_one_syllable() {
    // កា: ka + aa.
    let cp = cps("\u{1780}\u{17B6}");
    let syl = segment_syllables(&cp);
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].end, 2);
    assert_eq!(syl[0].base_index, Some(0));
}

#[test]
fn coeng_conjunct_keeps_last_base() {
    // ស្ត: sa + coeng + ta. One syllable, base is the ta at idx 2.
    let cp = cps("\u{179F}\u{17D2}\u{178F}");
    let syl = segment_syllables(&cp);
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].kind, SyllableKind::Consonant);
    assert_eq!(syl[0].end, 3);
    assert_eq!(syl[0].base_index, Some(2));
}

#[test]
fn pre_base_vowel_moves_before_base() {
    // កេ = ka + sign-e. Pre-base matra is typed after the base
    // but renders before it.
    let cp = cps("\u{1780}\u{17C1}");
    let mut glyphs = fake_glyphs(2);
    let original = glyphs.clone();
    let syls = segment_syllables(&cp);
    for s in &syls {
        initial_reorder(&cp, &mut glyphs, s);
    }
    assert_eq!(glyphs[0], original[1], "sign-e should sit first visually");
    assert_eq!(glyphs[1], original[0], "ka should sit second");
}

#[test]
fn post_base_vowel_stays_put() {
    // កា: sign-aa is post-base; no reorder.
    let cp = cps("\u{1780}\u{17B6}");
    let mut glyphs = fake_glyphs(2);
    let before = glyphs.clone();
    let syls = segment_syllables(&cp);
    for s in &syls {
        initial_reorder(&cp, &mut glyphs, s);
    }
    assert_eq!(glyphs, before);
}

#[test]
fn independent_vowel_is_vowel_syllable() {
    // ឣ U+17A3 historically independent vowel a. After our
    // table it is classed as B (base), still a single
    // syllable. Use U+17A5 (real IV) for the vowel path.
    let cp = cps("\u{17A5}");
    let syl = segment_syllables(&cp);
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].kind, SyllableKind::Vowel);
}

#[test]
fn khmer_digits_are_symbol_pass_through() {
    // ០១២ (Khmer digits 0,1,2): three Symbol syllables, one
    // per digit. Each keeps its own cluster id (not merged to
    // the first byte offset), matching rustybuzz.
    let cp = cps("\u{17E0}\u{17E1}\u{17E2}");
    let syl = segment_syllables(&cp);
    assert_eq!(syl.len(), 3);
    assert!(syl.iter().all(|s| s.kind == SyllableKind::Symbol));
}

#[test]
fn multi_syllable_run_segments_correctly() {
    // សួស្តី: SUS TI (hello). 6 codepoints, 2 syllables:
    //   សួ (sa + below-base u):                3 codepoints
    //   ស្តី (sa + coeng + ta + pre-base ii):   3 codepoints? No:
    //       ស 179F, ្ 17D2, ត 178F, ី 17B8: 4 cps
    // Input: 179F 17BD 179F 17D2 178F 17B8, six cps.
    // Hmm, សួ = sa(179F) + ua(17BD); ស្តី = sa(179F) + coeng(17D2)
    //       + ta(178F) + ii(17B8). Two syllables.
    let cp = cps("\u{179F}\u{17BD}\u{179F}\u{17D2}\u{178F}\u{17B8}");
    let syl = segment_syllables(&cp);
    assert_eq!(syl.len(), 2);
    assert_eq!(syl[0].end, 2);
    assert_eq!(syl[1].end, 6);
    assert_eq!(syl[1].base_index, Some(4)); // ta is visible base
}

#[test]
fn empty_input_produces_no_syllables() {
    assert!(segment_syllables(&[]).is_empty());
}

#[test]
fn shape_khmer_without_gsub_only_reorders() {
    // កេ: reorder, no GSUB. After reorder the sign-e sits
    // first; after the cluster-merge pass both glyphs share the
    // syllable's head byte offset (0 for `fake_glyphs` which
    // mirrors a UTF-8 buffer where ka starts at byte 0). We
    // verify the glyph IDs moved (1 -> 0 by original mapping) so
    // the test still catches a reorder regression.
    let cp = cps("\u{1780}\u{17C1}");
    let mut glyphs = fake_glyphs(2);
    let original = glyphs.clone();
    shape_khmer(None, None, &cp, &mut glyphs);
    assert_eq!(glyphs[0].glyph_id, original[1].glyph_id);
    assert_eq!(glyphs[1].glyph_id, original[0].glyph_id);
    // Cluster merge: both glyphs carry cluster 0 (syllable
    // start) after shaping.
    assert_eq!(glyphs[0].cluster, 0);
    assert_eq!(glyphs[1].cluster, 0);
}

#[test]
fn pre_base_with_coeng_moves_matra_to_syllable_head() {
    // ស្តេ = sa + coeng + ta + sign-e.
    //
    // The USE pre-base rule moves the sign-e to the very front
    // of the syllable, not just before the base. Keeping
    // `coeng + ta` adjacent is what lets the GSUB `blwf`
    // feature collapse them into a single subscript-ta glyph
    // in a later pass, matching rustybuzz output.
    //
    // Codepoint indices: sa=0, coeng=1, ta=2, sign-e=3.
    // fake_glyphs(4) uses index as cluster, so post-reorder we
    // expect clusters [3, 0, 1, 2].
    let cp = cps("\u{179F}\u{17D2}\u{178F}\u{17C1}");
    let mut glyphs = fake_glyphs(4);
    let syls = segment_syllables(&cp);
    assert_eq!(syls.len(), 1);
    assert_eq!(syls[0].base_index, Some(2));
    for s in &syls {
        initial_reorder(&cp, &mut glyphs, s);
    }
    assert_eq!(glyphs[0].cluster, 3); // sign-e
    assert_eq!(glyphs[1].cluster, 0); // sa
    assert_eq!(glyphs[2].cluster, 1); // coeng
    assert_eq!(glyphs[3].cluster, 2); // ta
}

#[test]
fn broken_leading_matra_advances_one_codepoint() {
    // Leading matra with no base: broken cluster. The
    // segmenter must still advance so the outer loop ends.
    let cp = cps("\u{17B6}\u{1780}");
    let syl = segment_syllables(&cp);
    assert_eq!(syl.len(), 2);
    assert_eq!(syl[0].kind, SyllableKind::Broken);
    assert_eq!(syl[0].end, 1);
}

#[test]
fn syllable_with_final_mark_absorbs_nikahit() {
    // កំ = ka + nikahit (final mark, U+17C6). One syllable.
    let cp = cps("\u{1780}\u{17C6}");
    let syl = segment_syllables(&cp);
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].end, 2);
}

#[test]
fn multiple_pre_base_matras_all_move() {
    // Synthetic: ka + VPre + VPre + aa. Rare but legal. Both
    // pre-base signs end up before the base in typing order.
    let cp = cps("\u{1780}\u{17C1}\u{17C2}\u{17B6}");
    let mut glyphs = fake_glyphs(4);
    let syls = segment_syllables(&cp);
    for s in &syls {
        initial_reorder(&cp, &mut glyphs, s);
    }
    // After reorder: VPre, VPre, ka, aa.
    assert_eq!(glyphs[0].cluster, 1);
    assert_eq!(glyphs[1].cluster, 2);
    assert_eq!(glyphs[2].cluster, 0);
    assert_eq!(glyphs[3].cluster, 3);
}

#[test]
fn feature_lists_are_deterministic() {
    // Compile-time check: the const slice of features is the
    // one the state machine dispatches, in the order the MS spec
    // specifies. Assertion is about order so downstream reviewers
    // can eyeball the slice instead of re-deriving it.
    assert_eq!(
        USE_BASIC_FEATURES,
        &[
            b"locl", b"ccmp", b"nukt", b"akhn", b"rphf", b"pref", b"rkrf", b"abvf", b"blwf",
            b"half", b"pstf", b"vatu", b"cjct", b"isol",
        ]
    );
    assert_eq!(
        USE_TOPOGRAPHICAL_FEATURES,
        &[b"abvs", b"blws", b"haln", b"pres", b"psts"]
    );
}

#[test]
fn script_priority_starts_with_khmr() {
    assert_eq!(KHMER_SCRIPT_PRIORITY[0], *b"khmr");
}

#[test]
fn cluster_merge_collapses_syllable_to_head_offset() {
    // កេ: pre-base reorder followed by the cluster-merge pass
    // leaves every glyph in the syllable carrying the head
    // byte offset (0 here: ka is first in the UTF-8 stream).
    // Matches HarfBuzz / rustybuzz behavior so callers see one
    // cluster id per syllable.
    let cp = cps("\u{1780}\u{17C1}");
    let mut glyphs = vec![Glyph::new(10, 0), Glyph::new(20, 3)];
    shape_khmer(None, None, &cp, &mut glyphs);
    assert_eq!(glyphs[0].cluster, 0);
    assert_eq!(glyphs[1].cluster, 0);
    // Glyph IDs confirm reorder happened (20 was at cluster 3,
    // now it is first).
    assert_eq!(glyphs[0].glyph_id, 20);
    assert_eq!(glyphs[1].glyph_id, 10);
}
