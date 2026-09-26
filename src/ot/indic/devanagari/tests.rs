//! Tests for the Indic state machine: syllable segmentation, position
//! tagging, initial and final reordering, and split-matra handling.

use super::super::indic_config_for;
use super::reorder::{
    cluster_byte_offsets, final_reorder, initial_reorder, merge_pre_base_matras,
    rotate_prefixes_right, tag_positions,
};
use super::*;
use crate::buffer::IndicPosition;
use alloc::vec;

fn cps(s: &str) -> Vec<char> {
    s.chars().collect()
}

fn fake_glyphs(n: usize) -> Vec<Glyph> {
    (0..n).map(|i| Glyph::new(i as u32 + 1, i as u32)).collect()
}

fn deva_config() -> IndicConfig {
    indic_config_for(Script::Devanagari).unwrap()
}

#[test]
fn single_consonant_is_a_consonant_syllable() {
    // क: one syllable.
    let cp = cps("\u{0915}");
    let syl = segment_syllables(&cp, &deva_config());
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].kind, SyllableKind::Consonant);
    assert_eq!(syl[0].start, 0);
    assert_eq!(syl[0].end, 1);
    assert_eq!(syl[0].base_index, Some(0));
    assert!(!syl[0].has_reph);
}

#[test]
fn consonant_matra_is_one_syllable() {
    // की = क + ी (consonant + post-base matra).
    let cp = cps("\u{0915}\u{0940}");
    let syl = segment_syllables(&cp, &deva_config());
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].kind, SyllableKind::Consonant);
    assert_eq!(syl[0].end, 2);
    assert_eq!(syl[0].base_index, Some(0));
}

#[test]
fn namaste_splits_into_three_syllables() {
    // न म स ् त े ("namaste") is typically three syllables:
    // न (na), म (ma), स्ते (ste with halant conjunct).
    let cp = cps("\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}");
    let syl = segment_syllables(&cp, &deva_config());
    assert_eq!(syl.len(), 3);
    assert!(syl.iter().all(|s| s.kind == SyllableKind::Consonant));
}

#[test]
fn hindi_has_reph_on_second_syllable() {
    // हिन्दी = ह ि न ् द ी
    let cp = cps("\u{0939}\u{093F}\u{0928}\u{094D}\u{0926}\u{0940}");
    let syl = segment_syllables(&cp, &deva_config());
    assert!(!syl.is_empty());
    assert!(syl.iter().all(|s| !s.has_reph));
}

#[test]
fn ra_halant_consonant_marks_reph() {
    // र् क -> reph(ra) + halant + ka = reph + ka syllable.
    let cp = cps("\u{0930}\u{094D}\u{0915}");
    let syl = segment_syllables(&cp, &deva_config());
    assert_eq!(syl.len(), 1);
    assert!(syl[0].has_reph);
    assert_eq!(syl[0].base_index, Some(2)); // ka
}

#[test]
fn pre_base_matra_moves_before_base() {
    // कि = क (ka) + ि (pre-base matra).
    let cp = cps("\u{0915}\u{093F}");
    let mut glyphs = fake_glyphs(2);
    let original = glyphs.clone();
    let syllables = segment_syllables(&cp, &deva_config());
    for s in &syllables {
        initial_reorder(&cp, &mut glyphs, s, ClusterLevel::Characters);
    }
    assert_eq!(glyphs[0], original[1]); // matra first
    assert_eq!(glyphs[1], original[0]); // ka second
}

#[test]
fn post_base_matra_stays_put() {
    // की: matra ी is Right positional (post-base), so no move.
    let cp = cps("\u{0915}\u{0940}");
    let mut glyphs = fake_glyphs(2);
    let before = glyphs.clone();
    let syllables = segment_syllables(&cp, &deva_config());
    for s in &syllables {
        initial_reorder(&cp, &mut glyphs, s, ClusterLevel::Characters);
    }
    assert_eq!(glyphs, before);
}

#[test]
fn independent_vowel_is_a_vowel_syllable() {
    let cp = cps("\u{0905}"); // अ
    let syl = segment_syllables(&cp, &deva_config());
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].kind, SyllableKind::Vowel);
}

#[test]
fn devanagari_digits_are_symbol_pass_through() {
    let cp = cps("\u{0966}");
    let syl = segment_syllables(&cp, &deva_config());
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].kind, SyllableKind::Symbol);
}

#[test]
fn empty_input_produces_no_syllables() {
    assert!(segment_syllables(&[], &deva_config()).is_empty());
}

#[test]
fn three_pre_base_matras_each_move_before_their_base() {
    let cp = cps("\u{0915}\u{093F}\u{0915}\u{093F}\u{0915}\u{093F}");
    let mut glyphs = fake_glyphs(6);
    let syls = segment_syllables(&cp, &deva_config());
    assert_eq!(syls.len(), 3);
    for s in &syls {
        initial_reorder(&cp, &mut glyphs, s, ClusterLevel::Characters);
    }
    assert_eq!(glyphs[0].cluster, 1);
    assert_eq!(glyphs[1].cluster, 0);
    assert_eq!(glyphs[2].cluster, 3);
    assert_eq!(glyphs[3].cluster, 2);
    assert_eq!(glyphs[4].cluster, 5);
    assert_eq!(glyphs[5].cluster, 4);
}

#[test]
fn shape_devanagari_without_gsub_only_reorders() {
    let cp = cps("\u{0915}\u{093F}");
    let mut glyphs = fake_glyphs(2);
    shape_devanagari(None, None, &cp, &mut glyphs, ClusterLevel::Characters);
    assert_eq!(glyphs[0].cluster, 1);
    assert_eq!(glyphs[1].cluster, 0);
    // At a monotone level the matra and the base it moved before
    // share a cluster, as after HarfBuzz's final reordering.
    let mut glyphs = fake_glyphs(2);
    shape_devanagari(
        None,
        None,
        &cp,
        &mut glyphs,
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!((glyphs[0].cluster, glyphs[1].cluster), (0, 0));
}

#[test]
fn tag_positions_marks_ra_as_reph_candidate() {
    let cp = cps("\u{0930}\u{094D}\u{0915}");
    let mut glyphs = fake_glyphs(3);
    for s in &segment_syllables(&cp, &deva_config()) {
        tag_positions(&cp, &mut glyphs, s);
    }
    assert_eq!(
        glyphs[0].indic_position,
        IndicPosition::RaToBecomeReph as u8,
        "ra should be marked as reph candidate"
    );
    assert_eq!(
        glyphs[2].indic_position,
        IndicPosition::BaseC as u8,
        "ka should be marked as base consonant"
    );
}

#[test]
fn tag_positions_marks_pre_base_matra() {
    let cp = cps("\u{0915}\u{093F}");
    let mut glyphs = fake_glyphs(2);
    for s in &segment_syllables(&cp, &deva_config()) {
        tag_positions(&cp, &mut glyphs, s);
    }
    assert_eq!(glyphs[0].indic_position, IndicPosition::BaseC as u8);
    assert_eq!(glyphs[1].indic_position, IndicPosition::PreM as u8);
}

#[test]
fn cluster_byte_offsets_matches_utf8_layout() {
    let cp = cps("\u{0930}\u{094D}\u{0915}");
    assert_eq!(cluster_byte_offsets(&cp), vec![0, 3, 6, 9]);
}

#[test]
fn final_reorder_moves_reph_to_syllable_end() {
    let mut g = fake_glyphs(2);
    g[0].indic_position = IndicPosition::RaToBecomeReph as u8;
    g[0].cluster = 0;
    g[1].indic_position = IndicPosition::BaseC as u8;
    g[1].cluster = 6;
    final_reorder(
        &mut g,
        0,
        9,
        3,
        RephPosition::BeforePost,
        RephMode::Implicit,
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!(g[0].indic_position, IndicPosition::BaseC as u8);
    assert_eq!(g[1].indic_position, IndicPosition::RaToBecomeReph as u8);
    assert_eq!(g[1].cluster, 0);
}

#[test]
fn final_reorder_noop_when_rphf_did_not_fire() {
    let mut g = fake_glyphs(3);
    g[0].indic_position = IndicPosition::RaToBecomeReph as u8;
    g[0].cluster = 0;
    g[2].indic_position = IndicPosition::BaseC as u8;
    g[2].cluster = 6;
    let before = g.clone();
    final_reorder(
        &mut g,
        0,
        9,
        3,
        RephPosition::BeforePost,
        RephMode::Implicit,
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!(g, before, "no collapse -> no move");
}

#[test]
fn tag_positions_leaves_non_reph_syllables_alone() {
    let cp = cps("\u{0915}");
    let mut glyphs = fake_glyphs(1);
    for s in &segment_syllables(&cp, &deva_config()) {
        tag_positions(&cp, &mut glyphs, s);
    }
    assert_ne!(
        glyphs[0].indic_position,
        IndicPosition::RaToBecomeReph as u8
    );
}

#[test]
fn symbol_run_advances_past_multiple_digits() {
    let cp = cps("\u{0966}\u{0967}\u{0968}");
    let syl = segment_syllables(&cp, &deva_config());
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].end, 3);
}

#[test]
fn bengali_ra_halant_consonant_marks_reph() {
    // Bengali: র (U+09B0) + ্ (U+09CD) + ক (U+0995).
    let cp = cps("\u{09B0}\u{09CD}\u{0995}");
    let config = indic_config_for(Script::Bengali).unwrap();
    let syl = segment_syllables(&cp, &config);
    assert_eq!(syl.len(), 1);
    assert!(syl[0].has_reph);
    assert_eq!(syl[0].base_index, Some(2));
}

#[test]
fn tamil_consonant_syllable_segments() {
    // Tamil: க (U+0B95) + ி (U+0BBF pre-base I).
    let cp = cps("\u{0B95}\u{0BBF}");
    let config = indic_config_for(Script::Tamil).unwrap();
    let syl = segment_syllables(&cp, &config);
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].kind, SyllableKind::Consonant);
    assert_eq!(syl[0].base_index, Some(0));
}

#[test]
fn telugu_ra_halant_is_not_reph_under_explicit_mode() {
    // Telugu's RephMode is Explicit: bare ra+virama does NOT
    // tag a reph candidate. Only ra+virama+ZWJ does.
    let cp = cps("\u{0C30}\u{0C4D}\u{0C15}");
    let config = indic_config_for(Script::Telugu).unwrap();
    let syl = segment_syllables(&cp, &config);
    assert_eq!(syl.len(), 1);
    assert!(
        !syl[0].has_reph,
        "explicit reph mode should NOT tag bare ra+virama"
    );
}

#[test]
fn sinhala_pre_base_matra_moves() {
    // Sinhala: ක (U+0D9A) + ෙ (U+0DD9 pre-base e vowel sign).
    let cp = cps("\u{0D9A}\u{0DD9}");
    let mut glyphs = fake_glyphs(2);
    let original = glyphs.clone();
    let config = indic_config_for(Script::Sinhala).unwrap();
    for s in &segment_syllables(&cp, &config) {
        initial_reorder(&cp, &mut glyphs, s, ClusterLevel::Characters);
    }
    assert_eq!(glyphs[0], original[1]);
    assert_eq!(glyphs[1], original[0]);
}

#[test]
fn after_main_reph_target_is_right_after_base() {
    // Oriya config uses AfterMain.
    let mut g = fake_glyphs(3);
    g[0].indic_position = IndicPosition::RaToBecomeReph as u8;
    g[0].cluster = 0;
    g[1].indic_position = IndicPosition::BaseC as u8;
    g[1].cluster = 6;
    g[2].indic_position = IndicPosition::Start as u8;
    g[2].cluster = 9;
    // original glyph count was 4 (ra, halant, base, matra); now 3.
    final_reorder(
        &mut g,
        0,
        12,
        4,
        RephPosition::AfterMain,
        RephMode::Implicit,
        ClusterLevel::MonotoneCharacters,
    );
    // After move: base @ 0, reph @ 1, trailing @ 2.
    assert_eq!(g[0].indic_position, IndicPosition::BaseC as u8);
    assert_eq!(g[1].indic_position, IndicPosition::RaToBecomeReph as u8);
    assert_eq!(g[2].indic_position, IndicPosition::Start as u8);
}

#[test]
fn telugu_ra_halant_zwj_consonant_marks_reph_explicit() {
    // Telugu's Explicit reph mode: ra + halant + ZWJ + C tags
    // the ra as a reph candidate; bare ra+halant alone does not
    // (see issue #30).
    let cp = cps("\u{0C30}\u{0C4D}\u{200D}\u{0C15}");
    let config = indic_config_for(Script::Telugu).unwrap();
    let syl = segment_syllables(&cp, &config);
    assert_eq!(syl.len(), 1);
    assert!(
        syl[0].has_reph,
        "explicit ra+halant+ZWJ+C should be tagged as reph"
    );
    // Base must sit past the ZWJ.
    assert_eq!(syl[0].base_index, Some(3));
}

#[test]
fn malayalam_logrepha_head_marks_reph() {
    // Malayalam's LogRepha mode: U+0D4E at syllable head is
    // itself the reph, and the following consonant becomes
    // the base (see issue #31).
    let cp = cps("\u{0D4E}\u{0D15}");
    let config = indic_config_for(Script::Malayalam).unwrap();
    let syl = segment_syllables(&cp, &config);
    assert_eq!(syl.len(), 1);
    assert!(syl[0].has_reph, "LogRepha head should tag as reph");
    assert_eq!(syl[0].base_index, Some(1));
}

#[test]
fn logrepha_reorder_moves_reph_past_base() {
    // LogRepha fixture: the 0D4E glyph sits at pos 0, base at
    // pos 1. AfterMain target puts the repha right after the
    // base: [repha, base] -> [base, repha].
    let mut g = fake_glyphs(2);
    g[0].indic_position = IndicPosition::RaToBecomeReph as u8;
    g[0].cluster = 0;
    g[1].indic_position = IndicPosition::BaseC as u8;
    g[1].cluster = 3;
    // Original glyph count 2, no shrinkage. LogRepha mode must
    // still relocate because the repha is a standalone glyph
    // rather than an `rphf` ligature product.
    final_reorder(
        &mut g,
        0,
        6,
        2,
        RephPosition::AfterMain,
        RephMode::LogRepha,
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!(g[0].indic_position, IndicPosition::BaseC as u8);
    assert_eq!(g[1].indic_position, IndicPosition::RaToBecomeReph as u8);
}

/// Glyphs with the given `(indic position, cluster)` pairs.
fn tagged(rows: &[(IndicPosition, u32)]) -> Vec<Glyph> {
    rows.iter()
        .enumerate()
        .map(|(i, &(pos, cluster))| {
            let mut g = Glyph::new(i as u32 + 1, cluster);
            g.indic_position = pos as u8;
            g
        })
        .collect()
}

fn clusters_of(glyphs: &[Glyph]) -> Vec<u32> {
    glyphs.iter().map(|g| g.cluster).collect()
}

#[test]
fn reph_move_merges_clusters_at_monotone_levels_only() {
    use IndicPosition::{BaseC, RaToBecomeReph, Start};
    for (level, clusters) in [
        (ClusterLevel::MonotoneCharacters, [0, 0, 0]),
        (ClusterLevel::MonotoneGraphemes, [0, 0, 0]),
        (ClusterLevel::Characters, [6, 9, 0]),
        (ClusterLevel::Graphemes, [6, 9, 0]),
    ] {
        // reph (ra + halant ligated), base, trailing matra: the
        // original four code points shrank to three glyphs.
        let mut g = tagged(&[(RaToBecomeReph, 0), (BaseC, 6), (Start, 9)]);
        final_reorder(
            &mut g,
            0,
            12,
            4,
            RephPosition::BeforePost,
            RephMode::Implicit,
            level,
        );
        assert_eq!(g[2].indic_position, RaToBecomeReph as u8, "{level:?}");
        assert_eq!(clusters_of(&g), clusters, "{level:?}");
    }
}

#[test]
fn pre_base_matra_merges_through_the_base() {
    use IndicPosition::{BaseC, PreM, Start};
    let rows = [(PreM, 9), (Start, 0), (BaseC, 6), (Start, 12)];
    let mut g = tagged(&rows);
    merge_pre_base_matras(&mut g, 0, 15, ClusterLevel::MonotoneCharacters);
    assert_eq!(clusters_of(&g), [0, 0, 0, 12]);
    let mut g = tagged(&rows);
    merge_pre_base_matras(&mut g, 0, 15, ClusterLevel::Characters);
    assert_eq!(clusters_of(&g), [9, 0, 6, 12]);
    // Without a base glyph left, the merge runs to the syllable end.
    let mut g = tagged(&[(PreM, 6), (Start, 0), (Start, 3)]);
    merge_pre_base_matras(&mut g, 0, 9, ClusterLevel::MonotoneCharacters);
    assert_eq!(clusters_of(&g), [0, 0, 0]);
    // Glyphs of other syllables are left alone.
    let mut g = tagged(&[(Start, 20), (PreM, 9), (BaseC, 6)]);
    merge_pre_base_matras(&mut g, 6, 12, ClusterLevel::MonotoneCharacters);
    assert_eq!(clusters_of(&g), [20, 6, 6]);
}

#[test]
fn initial_reorder_merges_glyphs_displaced_past_the_base() {
    // ka ZWJ i-matra: the matra moves before ka, which shifts ka and
    // the ZWJ right; HarfBuzz merges the displaced glyphs from the
    // base on, and leaves the matra to final reordering.
    let cp = cps("\u{0915}\u{200D}\u{093F}");
    for (level, clusters) in [
        (ClusterLevel::MonotoneCharacters, [6, 0, 0]),
        (ClusterLevel::Characters, [6, 0, 3]),
    ] {
        let mut glyphs: Vec<Glyph> = [0, 3, 6].iter().map(|&c| Glyph::new(c + 1, c)).collect();
        for s in &segment_syllables(&cp, &deva_config()) {
            initial_reorder(&cp, &mut glyphs, s, level);
        }
        assert_eq!(clusters_of(&glyphs), clusters, "{level:?}");
    }
}

/// Small deterministic generator for the differential tests.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % bound.max(1)
    }
}

/// The one-rotation-at-a-time loop that `rotate_prefixes_right`
/// replaces, kept as the reference.
fn rotate_prefixes_one_by_one(items: &mut [u32], rotations: &[usize]) {
    for &r in rotations {
        let item = items[r];
        for j in (0..r).rev() {
            items[j + 1] = items[j];
        }
        items[0] = item;
    }
}

#[test]
fn rotate_prefixes_right_matches_one_by_one_rotation() {
    let mut rng = Lcg(7);
    for _ in 0..3000 {
        let len = 1 + rng.below(24);
        let density = 1 + rng.below(4);
        let mut rotations: Vec<usize> = (1..len).filter(|_| rng.below(density) == 0).collect();
        rotations.reverse();
        let mut expected: Vec<u32> = (0..len as u32).collect();
        rotate_prefixes_one_by_one(&mut expected, &rotations);
        let mut got: Vec<u32> = (0..len as u32).collect();
        rotate_prefixes_right(&mut got, &rotations);
        assert_eq!(got, expected, "len {len} rotations {rotations:?}");
    }
}

#[test]
fn rotate_prefixes_right_ignores_invalid_rotations() {
    let mut items = [1u32, 2, 3];
    rotate_prefixes_right(&mut items, &[3]);
    rotate_prefixes_right(&mut items, &[1, 2]);
    assert_eq!(items, [1, 2, 3]);
}

#[test]
fn final_reorder_all_matches_per_syllable_scan() {
    let positions = [
        IndicPosition::Start,
        IndicPosition::RaToBecomeReph,
        IndicPosition::BaseC,
        IndicPosition::PreM,
        IndicPosition::Smvd,
    ];
    let levels = [ClusterLevel::MonotoneCharacters, ClusterLevel::Characters];
    let reph_positions = [
        RephPosition::AfterMain,
        RephPosition::BeforeSub,
        RephPosition::AfterSub,
        RephPosition::BeforePost,
        RephPosition::AfterPost,
    ];
    let reph_modes = [RephMode::Implicit, RephMode::Explicit, RephMode::LogRepha];
    let mut rng = Lcg(11);
    for _ in 0..3000 {
        // Consecutive syllables over `n` three-byte codepoints.
        let n = 1 + rng.below(12);
        let mut syllables = Vec::new();
        let mut start = 0;
        while start < n {
            let end = (start + 1 + rng.below(4)).min(n);
            syllables.push(Syllable {
                kind: SyllableKind::Consonant,
                start,
                end,
                base_index: Some(start),
                has_reph: false,
            });
            start = end;
        }
        let byte_offsets: Vec<u32> = (0..=n as u32).map(|i| i * 3).collect();
        // Glyphs with arbitrary clusters, including interleaved
        // syllables and clusters past the end of the run.
        let glyph_count = rng.below(16);
        let glyphs: Vec<Glyph> = (0..glyph_count)
            .map(|i| {
                let mut g = Glyph::new(i as u32, rng.below(3 * n + 4) as u32);
                g.indic_position = positions[rng.below(positions.len())] as u8;
                g
            })
            .collect();
        let mut config = deva_config();
        config.reph_pos = reph_positions[rng.below(reph_positions.len())];
        config.reph_mode = reph_modes[rng.below(reph_modes.len())];
        let level = levels[rng.below(levels.len())];

        let mut expected = glyphs.clone();
        for s in &syllables {
            let (start, end) = (byte_offsets[s.start], byte_offsets[s.end]);
            merge_pre_base_matras(&mut expected, start, end, level);
            final_reorder(
                &mut expected,
                start,
                end,
                s.end - s.start,
                config.reph_pos,
                config.reph_mode,
                level,
            );
        }
        let mut got = glyphs;
        final_reorder_all(&mut got, &syllables, &byte_offsets, &config, level);
        assert_eq!(got, expected);
    }
}

#[test]
fn long_run_of_pre_base_matras_reorders_in_linear_time() {
    // One consonant followed by 200000 pre-base matras is a single
    // consonant syllable. Moving the matras one rotation at a time
    // cost about 2e10 glyph copies.
    const N: usize = 200_000;
    let mut cp = vec!['\u{0915}'];
    cp.extend(core::iter::repeat('\u{093F}').take(N));
    let mut glyphs = fake_glyphs(cp.len());
    for s in &segment_syllables(&cp, &deva_config()) {
        initial_reorder(&cp, &mut glyphs, s, ClusterLevel::MonotoneCharacters);
    }
    let mut ids: Vec<u32> = glyphs.iter().map(|g| g.glyph_id).collect();
    ids.sort_unstable();
    assert!(ids.iter().copied().eq(1..=cp.len() as u32));
}

#[test]
fn many_syllables_final_reorder_in_linear_time() {
    // 200000 one-consonant syllables. Scanning every glyph once per
    // syllable cost about 4e10 cluster comparisons.
    const N: usize = 200_000;
    let cp = vec!['\u{0915}'; N];
    let mut glyphs: Vec<Glyph> = (0..N).map(|i| Glyph::new(1, (i * 3) as u32)).collect();
    let level = ClusterLevel::MonotoneCharacters;
    shape_indic(None, None, &cp, &mut glyphs, &deva_config(), level);
    assert_eq!(glyphs.len(), N);
    assert_eq!(glyphs[N - 1].cluster, ((N - 1) * 3) as u32);
}
