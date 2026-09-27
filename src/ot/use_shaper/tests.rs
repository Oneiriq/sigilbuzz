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
        initial_reorder(&cp, &mut glyphs, s, ClusterLevel::Characters);
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
        initial_reorder(&cp, &mut glyphs, s, ClusterLevel::Characters);
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
    // first, and at a monotone level both glyphs share the
    // syllable's head byte offset (0 for `fake_glyphs` which
    // mirrors a UTF-8 buffer where ka starts at byte 0). We
    // verify the glyph IDs moved (1 -> 0 by original mapping) so
    // the test still catches a reorder regression.
    let cp = cps("\u{1780}\u{17C1}");
    let mut glyphs = fake_glyphs(2);
    let original = glyphs.clone();
    shape_khmer(
        None,
        None,
        &cp,
        &mut glyphs,
        ClusterLevel::MonotoneCharacters,
    );
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
        initial_reorder(&cp, &mut glyphs, s, ClusterLevel::Characters);
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
        initial_reorder(&cp, &mut glyphs, s, ClusterLevel::Characters);
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
            b"half", b"pstf", b"vatu", b"cjct",
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
fn reordered_vowel_merges_clusters_at_monotone_levels_only() {
    // កេ: the pre-base sign-e moves in front of ka. HarfBuzz's Khmer
    // shaper merges the clusters it moves across first, so at the
    // monotone levels both glyphs carry ka's offset; at the others the
    // sign-e keeps its own (out-of-order) cluster.
    let cp = cps("\u{1780}\u{17C1}");
    for (level, clusters) in [
        (ClusterLevel::MonotoneCharacters, [0, 0]),
        (ClusterLevel::MonotoneGraphemes, [0, 0]),
        (ClusterLevel::Characters, [3, 0]),
        (ClusterLevel::Graphemes, [3, 0]),
    ] {
        let mut glyphs = vec![Glyph::new(10, 0), Glyph::new(20, 3)];
        shape_khmer(None, None, &cp, &mut glyphs, level);
        let got: Vec<u32> = glyphs.iter().map(|g| g.cluster).collect();
        assert_eq!(got, clusters, "{level:?}");
        // Glyph IDs confirm the reorder happened (20 was at cluster
        // 3, now it is first).
        assert_eq!(glyphs[0].glyph_id, 20);
        assert_eq!(glyphs[1].glyph_id, 10);
    }
}

#[test]
fn every_move_merges_the_span_it_crosses() {
    // ស្តេ: sa, coeng, ta, sign-e. The sign-e moves to the syllable
    // head across all three, so all four share a cluster.
    let cp = cps("\u{179F}\u{17D2}\u{178F}\u{17C1}");
    let mut glyphs = fake_glyphs(4);
    for s in &segment_syllables(&cp) {
        initial_reorder(&cp, &mut glyphs, s, ClusterLevel::MonotoneCharacters);
    }
    let got: Vec<u32> = glyphs.iter().map(|g| g.cluster).collect();
    assert_eq!(got, [0, 0, 0, 0]);
    // Two pre-base signs before a trailing aa: the aa is not crossed.
    let cp = cps("\u{1780}\u{17C1}\u{17C2}\u{17B6}");
    let mut glyphs = fake_glyphs(4);
    for s in &segment_syllables(&cp) {
        initial_reorder(&cp, &mut glyphs, s, ClusterLevel::MonotoneCharacters);
    }
    let got: Vec<u32> = glyphs.iter().map(|g| g.cluster).collect();
    assert_eq!(got, [0, 0, 0, 3]);
}

fn tagged(text: &str) -> Vec<Glyph> {
    let cp = cps(text);
    let mut glyphs = fake_glyphs(cp.len());
    assert!(reorder::tag_syllables(
        &mut glyphs,
        &cp,
        &segment_syllables(&cp)
    ));
    glyphs
}

fn ids_and_clusters(glyphs: &[Glyph]) -> (Vec<u32>, Vec<u32>) {
    (
        glyphs.iter().map(|g| g.glyph_id).collect(),
        glyphs.iter().map(|g| g.cluster).collect(),
    )
}

#[test]
fn use_reorder_moves_pre_base_signs_by_cluster_level() {
    // Balinese ka, taling: the vowel sign moves in front of the base,
    // merging clusters only at the monotone levels.
    for (level, clusters) in [
        (ClusterLevel::MonotoneGraphemes, [0, 0]),
        (ClusterLevel::MonotoneCharacters, [0, 0]),
        (ClusterLevel::Characters, [1, 0]),
        (ClusterLevel::Graphemes, [1, 0]),
    ] {
        let mut glyphs = tagged("\u{1B13}\u{1B3E}");
        reorder::reorder_pre_base(&mut glyphs, level);
        let expected = (vec![2, 1], clusters.to_vec());
        assert_eq!(ids_and_clusters(&glyphs), expected, "{level:?}");
        assert!(glyphs.iter().all(|g| g.indic_position == 0));
    }
}

#[test]
fn use_reorder_stops_after_the_last_unligated_halant() {
    // ka, adeg adeg, ta, taling: the sign moves back only to the halant.
    let mut glyphs = tagged("\u{1B13}\u{1B44}\u{1B22}\u{1B3E}");
    reorder::reorder_pre_base(&mut glyphs, ClusterLevel::MonotoneCharacters);
    let expected = (vec![1, 2, 4, 3], vec![0, 1, 2, 2]);
    assert_eq!(ids_and_clusters(&glyphs), expected);
    // A halant that ligated no longer stops it.
    let mut glyphs = tagged("\u{1B13}\u{1B44}\u{1B22}\u{1B3E}");
    glyphs[1].unicode_props |= crate::tables::layout::skip_iter::match_prop::LIGATED;
    reorder::reorder_pre_base(&mut glyphs, ClusterLevel::MonotoneCharacters);
    let expected = (vec![4, 1, 2, 3], vec![0, 0, 0, 0]);
    assert_eq!(ids_and_clusters(&glyphs), expected);
}

#[test]
fn use_reorder_moves_the_glyph_pref_substituted() {
    // Cham ka, medial ra: pref substitutes the medial, which then moves
    // like a pre-base vowel sign (`record_pref_use`).
    let mut glyphs = tagged("\u{AA06}\u{AA34}");
    let before: Vec<u32> = glyphs.iter().map(|g| g.glyph_id).collect();
    glyphs[1].glyph_id = 20;
    reorder::record_pref(&before, &mut glyphs);
    reorder::reorder_pre_base(&mut glyphs, ClusterLevel::MonotoneCharacters);
    assert_eq!(ids_and_clusters(&glyphs), (vec![20, 1], vec![0, 0]));
}

#[test]
fn long_run_of_pre_base_signs_reorders_in_linear_time() {
    // Khmer ka followed by 200000 sign-e. HarfBuzz's Khmer grammar
    // takes one pre-base sign into the consonant syllable, which moves
    // in front of ka. Every other sign is a broken cluster of its own.
    // A scan or reorder that revisited the run per syllable would cost
    // about 4e10 steps.
    const N: usize = 200_000;
    let mut cp = vec!['\u{1780}'];
    cp.extend(core::iter::repeat('\u{17C1}').take(N));
    let mut glyphs = fake_glyphs(cp.len());
    shape_khmer(
        None,
        None,
        &cp,
        &mut glyphs,
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!(glyphs.len(), N + 1);
    assert_eq!(glyphs[0].glyph_id, 2);
    assert_eq!(glyphs[1].glyph_id, 1);
    assert_eq!(glyphs[N].glyph_id, N as u32 + 1);
}

#[test]
fn many_syllables_merge_clusters_in_linear_time() {
    // 200000 Khmer digits are 200000 one-wide syllables. Visiting
    // every glyph once per syllable cost about 4e10 comparisons.
    const N: usize = 200_000;
    let cp = vec!['\u{17E0}'; N];
    let mut glyphs: Vec<Glyph> = (0..N).map(|i| Glyph::new(1, (i * 3) as u32)).collect();
    shape_khmer(
        None,
        None,
        &cp,
        &mut glyphs,
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!(glyphs[N - 1].cluster, ((N - 1) * 3) as u32);
}

#[test]
fn long_run_of_use_pre_base_signs_reorders_in_linear_time() {
    // Balinese ka followed by 200000 taling is one syllable whose
    // signs all move to its start. Moving them one at a time cost
    // about 2e10 glyph copies.
    const N: usize = 200_000;
    let mut text = alloc::string::String::from("\u{1B13}");
    text.extend(core::iter::repeat('\u{1B3E}').take(N));
    let mut glyphs = tagged(&text);
    reorder::reorder_pre_base(&mut glyphs, ClusterLevel::MonotoneCharacters);
    // Each sign moves in front of the ones before it, so they end up
    // reversed, then the base.
    assert_eq!(glyphs.len(), N + 1);
    assert_eq!(glyphs[0].glyph_id, N as u32 + 1);
    assert_eq!(glyphs[N - 1].glyph_id, 2);
    assert_eq!(glyphs[N].glyph_id, 1);
    assert!(glyphs.iter().all(|g| g.cluster == 0));
}

/// HarfBuzz's `reorder_syllable_use` loop, one move and one merge at a
/// time, as the reference for the batched pass. Reads the tags
/// `reorder::tag_syllables` writes (serial in the high nibble, 1 for a
/// halant and 2 for a pre-base glyph in the low one).
fn reorder_one_by_one(glyphs: &mut [Glyph], level: ClusterLevel) {
    let serial = |g: &Glyph| g.indic_position >> 4;
    let mut start = 0;
    while start < glyphs.len() {
        let end = (start..glyphs.len())
            .find(|&i| serial(&glyphs[i]) != serial(&glyphs[start]))
            .unwrap_or(glyphs.len());
        let mut j = start;
        for i in start..end {
            let m = crate::tables::layout::skip_iter::MatchGlyph::from(&glyphs[i]);
            let category = glyphs[i].indic_position & 0x0F;
            if category == 1 && !m.is_ligated() {
                j = i + 1;
            } else if category == 2 && m.lig_comp() == 0 && j < i {
                crate::shape::merge_clusters(glyphs, j, i + 1, level);
                glyphs[j..=i].rotate_right(1);
            }
        }
        start = end;
    }
    for g in glyphs.iter_mut() {
        g.indic_position = 0;
    }
}

#[test]
fn use_reorder_matches_moving_one_glyph_at_a_time() {
    // Balinese ka, adeg adeg, taling, and a post-base sign, in every
    // mix up to six long, with rising and falling clusters.
    let alphabet = ['\u{1B13}', '\u{1B44}', '\u{1B3E}', '\u{1B38}'];
    let levels = [
        ClusterLevel::MonotoneGraphemes,
        ClusterLevel::MonotoneCharacters,
        ClusterLevel::Characters,
    ];
    for len in 1..=6u32 {
        for code in 0..4u32.pow(len) {
            let text: alloc::string::String = (0..len)
                .map(|k| alphabet[(code / 4u32.pow(k) % 4) as usize])
                .collect();
            for level in levels {
                for falling in [false, true] {
                    let mut glyphs = tagged(&text);
                    if falling {
                        let n = glyphs.len() as u32;
                        for (k, g) in glyphs.iter_mut().enumerate() {
                            g.cluster = 3 * (n - k as u32);
                        }
                    }
                    let mut expected = glyphs.clone();
                    reorder_one_by_one(&mut expected, level);
                    reorder::reorder_pre_base(&mut glyphs, level);
                    assert_eq!(glyphs, expected, "{text:?} {level:?} falling {falling}");
                }
            }
        }
    }
}
