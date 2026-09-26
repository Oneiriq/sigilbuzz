//! Cluster forming and merging, gated by the buffer's
//! [`ClusterLevel`] the way HarfBuzz gates them.
//!
//! HarfBuzz (`hb-buffer.hh`, `hb-buffer.cc`, `hb-ot-shape.cc`) has two
//! merge primitives:
//!
//! - `merge_clusters`, used wherever shaping would otherwise leave
//!   clusters out of order (ligatures, reordered vowel signs, deleted
//!   glyphs, reversed graphemes). It acts only at the monotone levels.
//! - `merge_grapheme_clusters`, used where characters join their
//!   grapheme (`hb_form_clusters`, decomposed Thai and Hangul vowels).
//!   It acts only at the grapheme levels.
//!
//! Both run the same `merge_clusters_impl`: the range takes its
//! smallest cluster, and the merge spreads over neighbors that shared
//! a cluster with either end. sigilbuzz shapes a flat glyph vector
//! where HarfBuzz splits its buffer into an input and an output half
//! while it substitutes, so the in/out variants (`merge_out_clusters`
//! and friends) all come down to the same flat merge here.
//!
//! HarfBuzz also marks the range unsafe to break when a level skips a
//! merge; sigilbuzz produces no glyph flags, so that part has no
//! counterpart.

use alloc::vec::Vec;

use crate::buffer::{ClusterLevel, Glyph};
use crate::unicode::general_category::{
    general_category_class, is_extended_pictographic, GeneralCategoryClass,
};

/// `merge_clusters_impl`: `glyphs[start..end]` takes its smallest
/// cluster, extended over neighbors that shared a cluster with an end
/// of the range whose cluster changes.
fn merge_impl(glyphs: &mut [Glyph], mut start: usize, mut end: usize) {
    let end_limit = glyphs.len();
    if end > end_limit || end <= start + 1 {
        return;
    }
    let Some(cluster) = glyphs[start..end].iter().map(|g| g.cluster).min() else {
        return;
    };
    if cluster != glyphs[end - 1].cluster {
        while end < end_limit && glyphs[end - 1].cluster == glyphs[end].cluster {
            end += 1;
        }
    }
    if cluster != glyphs[start].cluster {
        while start > 0 && glyphs[start - 1].cluster == glyphs[start].cluster {
            start -= 1;
        }
    }
    for g in &mut glyphs[start..end] {
        g.cluster = cluster;
    }
}

/// HarfBuzz's `hb_buffer_t::merge_clusters` (and `merge_out_clusters`)
/// for `glyphs[start..end]`: merges only at the monotone levels.
pub(crate) fn merge_clusters(glyphs: &mut [Glyph], start: usize, end: usize, level: ClusterLevel) {
    if level.is_monotone() {
        merge_impl(glyphs, start, end);
    }
}

/// HarfBuzz's `merge_grapheme_clusters` (and
/// `merge_out_grapheme_clusters`) for `glyphs[start..end]`: merges only
/// at the grapheme levels.
pub(crate) fn merge_grapheme_clusters(
    glyphs: &mut [Glyph],
    start: usize,
    end: usize,
    level: ClusterLevel,
) {
    if level.is_graphemes() {
        merge_impl(glyphs, start, end);
    }
}

/// True for a character of General_Category Mn, Mc, or Me, HarfBuzz's
/// `_hb_glyph_info_is_unicode_mark`.
pub(super) fn is_unicode_mark(ch: char) -> bool {
    general_category_class(ch) == Some(GeneralCategoryClass::Mark)
}

const fn is_regional_indicator(ch: char) -> bool {
    matches!(ch as u32, 0x1F1E6..=0x1F1FF)
}

/// True at each index of `cps` that continues the grapheme before it,
/// the continuation bit HarfBuzz's `hb_set_unicode_props` sets: marks,
/// emoji modifiers, the second regional indicator of a pair, ZWJ and an
/// Extended_Pictographic character right after it, the halfwidth
/// katakana voiced sound marks, and tag characters. ZWNJ, although
/// Other_Grapheme_Extend, is left out on purpose, as in HarfBuzz.
pub(super) fn continuations(cps: &[char]) -> Vec<bool> {
    let mut cont = alloc::vec![false; cps.len()];
    let mut i = 0;
    while i < cps.len() {
        let c = cps[i];
        let cp = c as u32;
        if cp >= 0x80 && is_unicode_mark(c) {
            cont[i] = true;
        } else if (0x1F3FB..=0x1F3FF).contains(&cp) {
            // Emoji modifiers.
            cont[i] = true;
        } else if is_regional_indicator(c) {
            if i > 0 && is_regional_indicator(cps[i - 1]) && !cont[i - 1] {
                cont[i] = true;
            }
        } else if c == '\u{200D}' {
            cont[i] = true;
            if cps.get(i + 1).is_some_and(|&n| is_extended_pictographic(n)) {
                i += 1;
                cont[i] = true;
            }
        } else if matches!(cp, 0xFF9E..=0xFF9F | 0xE0020..=0xE007F) {
            cont[i] = true;
        }
        i += 1;
    }
    cont
}

/// The grapheme ranges of a run whose continuation bits are `cont`:
/// each range is a character and the continuations after it
/// (HarfBuzz's `foreach_grapheme`).
pub(super) fn graphemes(cont: &[bool]) -> impl Iterator<Item = core::ops::Range<usize>> + '_ {
    let mut start = 0;
    core::iter::from_fn(move || {
        if start >= cont.len() {
            return None;
        }
        let mut end = start + 1;
        while end < cont.len() && cont[end] {
            end += 1;
        }
        let range = start..end;
        start = end;
        Some(range)
    })
}

/// `hb_form_clusters`: at the grapheme levels, each grapheme of the
/// run takes one cluster. `cont` holds the continuation bits of
/// `glyphs`, one per glyph.
pub(super) fn form_clusters(glyphs: &mut [Glyph], cont: &[bool], level: ClusterLevel) {
    if glyphs.len() != cont.len() || !cont.contains(&true) {
        return;
    }
    for range in graphemes(cont) {
        merge_grapheme_clusters(glyphs, range.start, range.end, level);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(clusters: &[u32]) -> Vec<Glyph> {
        clusters.iter().map(|&c| Glyph::new(0, c)).collect()
    }

    fn clusters(glyphs: &[Glyph]) -> Vec<u32> {
        glyphs.iter().map(|g| g.cluster).collect()
    }

    #[test]
    fn merge_takes_the_smallest_cluster_and_spreads_to_equal_neighbors() {
        // [2, 4) holds 7 and 4: the glyph before it shared the start's
        // old cluster 7, so it joins.
        let mut g = run(&[0, 7, 7, 4, 9]);
        merge_clusters(&mut g, 2, 4, ClusterLevel::MonotoneGraphemes);
        assert_eq!(clusters(&g), [0, 4, 4, 4, 9]);
        // [1, 3) holds 2 and 7: the glyph after it shared the end's old
        // cluster 7, so it joins.
        let mut g = run(&[0, 2, 7, 7, 9]);
        merge_clusters(&mut g, 1, 3, ClusterLevel::MonotoneCharacters);
        assert_eq!(clusters(&g), [0, 2, 2, 2, 9]);
        // An end that already has the smallest cluster does not spread.
        let mut g = run(&[0, 5, 3, 3, 9]);
        merge_clusters(&mut g, 1, 3, ClusterLevel::MonotoneCharacters);
        assert_eq!(clusters(&g), [0, 3, 3, 3, 9]);
    }

    #[test]
    fn merges_follow_the_level() {
        for level in [
            ClusterLevel::MonotoneGraphemes,
            ClusterLevel::MonotoneCharacters,
            ClusterLevel::Characters,
            ClusterLevel::Graphemes,
        ] {
            let mut g = run(&[4, 2]);
            merge_clusters(&mut g, 0, 2, level);
            let monotone = clusters(&g);
            let mut g = run(&[4, 2]);
            merge_grapheme_clusters(&mut g, 0, 2, level);
            let graphemes = clusters(&g);
            let expect = |on: bool| if on { [2, 2] } else { [4, 2] };
            assert_eq!(monotone, expect(level.is_monotone()), "{level:?}");
            assert_eq!(graphemes, expect(level.is_graphemes()), "{level:?}");
        }
    }

    #[test]
    fn continuations_follow_harfbuzz() {
        let cont = |s: &str| continuations(&s.chars().collect::<Vec<_>>());
        // Marks, variation selectors, tag characters.
        assert_eq!(cont("e\u{0301}\u{FE0F}"), [false, true, true]);
        assert_eq!(cont("\u{1F3F4}\u{E0067}\u{E007F}"), [false, true, true]);
        // ZWJ and the pictograph after it; ZWNJ is left alone.
        assert_eq!(
            cont("\u{1F468}\u{200D}\u{1F469}a\u{200C}b"),
            [false, true, true, false, false, false]
        );
        // Emoji modifier, halfwidth katakana sound mark.
        assert_eq!(cont("\u{1F44D}\u{1F3FD}"), [false, true]);
        assert_eq!(cont("\u{FF76}\u{FF9E}"), [false, true]);
        // Regional indicators pair up.
        assert_eq!(
            cont("\u{1F1EB}\u{1F1F7}\u{1F1E9}\u{1F1EA}\u{1F1EF}"),
            [false, true, false, true, false]
        );
        // Letters, spaces, Thai sara am (a letter) start graphemes.
        assert_eq!(cont("a \u{0E01}\u{0E33}"), [false, false, false, false]);
    }

    #[test]
    fn form_clusters_merges_graphemes_at_grapheme_levels_only() {
        let cps: Vec<char> = "e\u{0301}\u{0302}x\u{200D}".chars().collect();
        let cont = continuations(&cps);
        for (level, expect) in [
            (ClusterLevel::MonotoneGraphemes, [0, 0, 0, 5, 5]),
            (ClusterLevel::Graphemes, [0, 0, 0, 5, 5]),
            (ClusterLevel::MonotoneCharacters, [0, 1, 3, 5, 6]),
            (ClusterLevel::Characters, [0, 1, 3, 5, 6]),
        ] {
            let mut g = run(&[0, 1, 3, 5, 6]);
            form_clusters(&mut g, &cont, level);
            assert_eq!(clusters(&g), expect, "{level:?}");
        }
    }

    #[test]
    fn graphemes_split_on_starters() {
        let got: Vec<_> = graphemes(&[false, true, false, false, true, true]).collect();
        assert_eq!(got, [0..2, 2..3, 3..6]);
        assert_eq!(graphemes(&[]).count(), 0);
        // A leading continuation still opens a grapheme.
        let got: Vec<_> = graphemes(&[true, true, false]).collect();
        assert_eq!(got, [0..2, 2..3]);
    }
}
