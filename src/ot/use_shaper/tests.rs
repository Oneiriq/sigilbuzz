//! Tests for the Universal Shaping Engine's masks, records, and
//! reordering, and for the Khmer entry point.

use super::category::{category, B, H, R, VPRE};
use super::reorder::{
    record_pref, record_rphf, reorder, setup_rphf_mask, setup_topographical_masks,
};
use super::*;
use crate::ot::syllabic::syllable_ranges;
use crate::tables::layout::skip_iter::{match_prop, MatchGlyph};
use alloc::{format, vec};

fn cps(s: &str) -> Vec<char> {
    s.chars().collect()
}

fn fake_glyphs(n: usize) -> Vec<Glyph> {
    (0..n).map(|i| Glyph::new(i as u32 + 1, i as u32)).collect()
}

/// The shaper state of `text` after `setup_syllables_use`, and one
/// glyph per character (glyph id `k + 1`, cluster `k`).
fn setup(text: &str) -> (Vec<Glyph>, Vec<GlyphInfo>) {
    let cp = cps(text);
    let mut info: Vec<GlyphInfo> = cp
        .iter()
        .map(|&c| GlyphInfo {
            category: category(c),
            ..GlyphInfo::default()
        })
        .collect();
    find_syllables(&cp, &mut info);
    (fake_glyphs(cp.len()), info)
}

fn ids_and_clusters(glyphs: &[Glyph]) -> (Vec<u32>, Vec<u32>) {
    (
        glyphs.iter().map(|g| g.glyph_id).collect(),
        glyphs.iter().map(|g| g.cluster).collect(),
    )
}

#[test]
fn feature_lists_follow_harfbuzz() {
    assert_eq!(
        USE_BASIC_FEATURES,
        &[
            b"locl", b"ccmp", b"nukt", b"akhn", b"rphf", b"pref", b"rkrf", b"abvf", b"blwf",
            b"half", b"pstf", b"vatu", b"cjct",
        ]
    );
    assert_eq!(
        USE_TOPOGRAPHICAL_FEATURES,
        &[b"isol", b"init", b"medi", b"fina", b"abvs", b"blws", b"haln", b"pres", b"psts"]
    );
    let tags: Vec<&[u8; 4]> = USE_FEATURES.iter().map(|f| &f.tag).collect();
    let listed: Vec<&[u8; 4]> = USE_BASIC_FEATURES
        .iter()
        .chain(USE_TOPOGRAPHICAL_FEATURES)
        .copied()
        .collect();
    assert_eq!(tags, listed);
    // Only `rphf` and the joining forms are masked, and only the
    // features up to `cjct` match one cluster at a time.
    for (i, f) in USE_FEATURES.iter().enumerate() {
        let masked = f.tag == *b"rphf" || TOPOGRAPHICAL.contains(&i);
        assert_eq!(!f.flags.contains(F::GLOBAL), masked, "{:?}", f.tag);
        assert_eq!(
            f.flags.contains(F::PER_SYLLABLE),
            i < BASIC.end,
            "{:?}",
            f.tag
        );
    }
}

#[test]
fn script_priority_starts_with_khmr() {
    assert_eq!(KHMER_SCRIPT_PRIORITY[0], *b"khmr");
}

#[test]
fn rphf_mask_covers_the_start_of_each_cluster() {
    // Tirhuta ra, virama, ka, virama, kha: `rphf` may apply to the
    // first three glyphs only.
    let (_, mut info) = setup("\u{114A9}\u{114C2}\u{1148F}\u{114C2}\u{11490}");
    setup_rphf_mask(&mut info, 1);
    let mask: Vec<u32> = info.iter().map(|i| i.mask).collect();
    assert_eq!(mask, [1, 1, 1, 0, 0]);
    // A cluster that starts with an encoded repha: the repha only.
    let mut info = vec![
        GlyphInfo {
            category: R,
            syllable: 0x12,
            ..GlyphInfo::default()
        },
        GlyphInfo {
            category: B,
            syllable: 0x12,
            ..GlyphInfo::default()
        },
    ];
    setup_rphf_mask(&mut info, 1);
    assert_eq!((info[0].mask, info[1].mask), (1, 0));
}

#[test]
fn record_rphf_marks_the_substituted_glyph_under_the_mask() {
    let (_, mut info) = setup("\u{114A9}\u{114C2}\u{1148F}\u{114C2}\u{11490}");
    setup_rphf_mask(&mut info, 1);
    // A substitution past the masked glyphs is not a repha.
    info[3].substituted = true;
    record_rphf(&mut info, 1);
    assert_ne!(info[3].category, R);
    info[1].substituted = true;
    record_rphf(&mut info, 1);
    assert_eq!(info[1].category, R);
}

#[test]
fn record_pref_marks_the_first_substituted_glyph() {
    // Cham ka, medial ra: `pref` substitutes the medial, which then
    // moves like a pre-base vowel sign.
    let (mut glyphs, mut info) = setup("\u{AA06}\u{AA34}");
    info[1].substituted = true;
    record_pref(&mut info);
    assert_eq!(info[1].category, VPRE);
    reorder(&mut glyphs, &mut info, ClusterLevel::MonotoneCharacters);
    assert_eq!(ids_and_clusters(&glyphs), (vec![2, 1], vec![0, 0]));
}

#[test]
fn pre_base_signs_move_by_cluster_level() {
    // Balinese ka, taling: the vowel sign moves in front of the base,
    // merging clusters only at the monotone levels.
    for (level, clusters) in [
        (ClusterLevel::MonotoneGraphemes, [0, 0]),
        (ClusterLevel::MonotoneCharacters, [0, 0]),
        (ClusterLevel::Characters, [1, 0]),
        (ClusterLevel::Graphemes, [1, 0]),
    ] {
        let (mut glyphs, mut info) = setup("\u{1B13}\u{1B3E}");
        reorder(&mut glyphs, &mut info, level);
        let expected = (vec![2, 1], clusters.to_vec());
        assert_eq!(ids_and_clusters(&glyphs), expected, "{level:?}");
    }
}

#[test]
fn pre_base_signs_stop_after_the_last_unligated_halant() {
    // Ka, adeg adeg, ta, taling: the sign moves back only to the halant.
    let (mut glyphs, mut info) = setup("\u{1B13}\u{1B44}\u{1B22}\u{1B3E}");
    reorder(&mut glyphs, &mut info, ClusterLevel::MonotoneCharacters);
    assert_eq!(
        ids_and_clusters(&glyphs),
        (vec![1, 2, 4, 3], vec![0, 1, 2, 2])
    );
    // A halant that ligated no longer stops it.
    let (mut glyphs, mut info) = setup("\u{1B13}\u{1B44}\u{1B22}\u{1B3E}");
    glyphs[1].unicode_props |= match_prop::LIGATED;
    reorder(&mut glyphs, &mut info, ClusterLevel::MonotoneCharacters);
    assert_eq!(
        ids_and_clusters(&glyphs),
        (vec![4, 1, 2, 3], vec![0, 0, 0, 0])
    );
}

#[test]
fn repha_moves_before_the_first_halant_or_mark() {
    // Ra, virama, ka, virama, kha with the ra and virama formed into a
    // repha: it moves after ka, before the virama.
    let (mut glyphs, mut info) = setup("\u{114A9}\u{114C2}\u{1148F}\u{114C2}\u{11490}");
    glyphs.remove(1);
    info.remove(1);
    info[0].category = R;
    reorder(&mut glyphs, &mut info, ClusterLevel::MonotoneCharacters);
    assert_eq!(
        ids_and_clusters(&glyphs),
        (vec![3, 1, 4, 5], vec![0, 0, 3, 4])
    );
    // With nothing after the base, it moves to the end.
    let (mut glyphs, mut info) = setup("\u{114A9}\u{114C2}\u{1148F}");
    glyphs.remove(1);
    info.remove(1);
    info[0].category = R;
    reorder(&mut glyphs, &mut info, ClusterLevel::Characters);
    assert_eq!(ids_and_clusters(&glyphs), (vec![3, 1], vec![2, 0]));
}

#[test]
fn clusters_join_their_neighbors_for_the_topographical_features() {
    // Three clusters, a word joiner, and one more cluster: init, medi,
    // fina, nothing, isol.
    let (_, mut info) = setup("\u{1B13}\u{1B13}\u{1B13}\u{2060}\u{1B13}");
    setup_topographical_masks(&mut info, TOPOGRAPHICAL_BITS);
    let masks: Vec<u32> = info.iter().map(|i| i.mask).collect();
    let [isol, init, medi, fina] = TOPOGRAPHICAL_BITS;
    assert_eq!(masks, [init, medi, fina, 0, isol]);
    // A font without the features changes nothing.
    let (_, mut info) = setup("\u{1B13}\u{1B13}");
    setup_topographical_masks(&mut info, [0; 4]);
    assert!(info.iter().all(|i| i.mask == 0));
}

#[test]
fn broken_clusters_get_a_dotted_circle_after_the_basic_features() {
    // A lone taling and a ka: the circle takes the taling's cluster and
    // the taling moves in front of it.
    let cp = cps("\u{1B3E}\u{1B13}");
    let mut glyphs = fake_glyphs(2);
    let run = UseRun {
        gsub: None,
        gdef: None,
        script_priority: BALINESE_SCRIPT_PRIORITY,
        level: ClusterLevel::MonotoneCharacters,
        features: &[],
        vertical: false,
        dotted_circle: Some(99),
        joining: None,
    };
    shape(&run, &cp, &mut glyphs);
    assert_eq!(ids_and_clusters(&glyphs), (vec![1, 99, 2], vec![0, 0, 1]));
}

/// HarfBuzz's `reorder_syllable_use` loop, one move and one merge at a
/// time, as the reference for the batched pass.
fn reorder_one_by_one(glyphs: &mut [Glyph], info: &mut [GlyphInfo], level: ClusterLevel) {
    let halant = |g: &Glyph, i: &GlyphInfo| {
        matches!(i.category, H | super::category::HVM | super::category::IS)
            && !MatchGlyph::from(g).is_ligated()
    };
    for range in syllable_ranges(info) {
        let (start, end) = (range.start, range.end);
        if info[start].category == R && end - start > 1 {
            for i in start + 1..end {
                let post = matches!(
                    info[i].category,
                    VPRE | super::category::VPST | super::category::VABV | super::category::VBLW
                ) || halant(&glyphs[i], &info[i]);
                if post || i == end - 1 {
                    let i = if post { i - 1 } else { i };
                    crate::shape::merge_clusters(glyphs, start, i + 1, level);
                    glyphs[start..=i].rotate_left(1);
                    info[start..=i].rotate_left(1);
                    break;
                }
            }
        }
        let mut j = start;
        for i in start..end {
            if halant(&glyphs[i], &info[i]) {
                j = i + 1;
            } else if info[i].category == VPRE
                && MatchGlyph::from(&glyphs[i]).lig_comp() == 0
                && j < i
            {
                crate::shape::merge_clusters(glyphs, j, i + 1, level);
                glyphs[j..=i].rotate_right(1);
                info[j..=i].rotate_right(1);
            }
        }
    }
}

#[test]
fn batched_reorder_matches_moving_one_glyph_at_a_time() {
    // Balinese ka, adeg adeg, taling, and a post-base sign, in every
    // mix up to six long, with rising and falling clusters, and with
    // the first glyph taken as a repha or not.
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
                for (falling, repha) in [(false, false), (true, false), (false, true), (true, true)]
                {
                    let (mut glyphs, mut info) = setup(&text);
                    if falling {
                        let n = glyphs.len() as u32;
                        for (k, g) in glyphs.iter_mut().enumerate() {
                            g.cluster = 3 * (n - k as u32);
                        }
                    }
                    if repha {
                        info[0].category = R;
                    }
                    let (mut expected, mut expected_info) = (glyphs.clone(), info.clone());
                    reorder_one_by_one(&mut expected, &mut expected_info, level);
                    reorder(&mut glyphs, &mut info, level);
                    let note = format!("{text:?} {level:?} falling {falling} repha {repha}");
                    assert_eq!(glyphs, expected, "{note}");
                    assert_eq!(info, expected_info, "{note}");
                }
            }
        }
    }
}

#[test]
fn long_run_of_pre_base_signs_reorders_in_linear_time() {
    // Balinese ka followed by 200000 taling is one cluster whose signs
    // all move to its start. Moving them one at a time cost about
    // 2e10 glyph copies.
    const N: usize = 200_000;
    let mut text = alloc::string::String::from("\u{1B13}");
    text.extend(core::iter::repeat('\u{1B3E}').take(N));
    let (mut glyphs, mut info) = setup(&text);
    reorder(&mut glyphs, &mut info, ClusterLevel::MonotoneCharacters);
    // Each sign moves in front of the ones before it, so they end up
    // reversed, then the base.
    assert_eq!(glyphs.len(), N + 1);
    assert_eq!(glyphs[0].glyph_id, N as u32 + 1);
    assert_eq!(glyphs[N - 1].glyph_id, 2);
    assert_eq!(glyphs[N].glyph_id, 1);
    assert!(glyphs.iter().all(|g| g.cluster == 0));
}

#[test]
fn shape_khmer_without_gsub_only_reorders() {
    // Ka, sign e: the sign moves in front, and both glyphs take the
    // syllable's cluster at a monotone level.
    let cp = cps("\u{1780}\u{17C1}");
    let mut glyphs = fake_glyphs(2);
    shape_khmer(
        None,
        None,
        &cp,
        &mut glyphs,
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!(ids_and_clusters(&glyphs), (vec![2, 1], vec![0, 0]));
}

#[test]
fn khmer_reordered_vowel_merges_clusters_at_monotone_levels_only() {
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
        assert_eq!(glyphs[0].glyph_id, 20);
        assert_eq!(glyphs[1].glyph_id, 10);
    }
}

#[test]
fn khmer_long_runs_stay_linear() {
    // Khmer ka followed by 200000 sign e: one pre-base sign joins the
    // consonant syllable, every other is a broken cluster.
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
    // 200000 Khmer digits are 200000 one-wide syllables.
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
