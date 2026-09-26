//! Thai and Lao SARA AM, the part of HarfBuzz's `preprocess_text_thai`
//! (`hb-ot-shaper-thai.cc`) that runs whether or not the font has Thai
//! GSUB.
//!
//! SARA AM (U+0E33, Lao U+0EB3) decomposes into NIKHAHIT (U+0E4D, Lao
//! U+0ECD) followed by SARA AA (U+0E32, Lao U+0EB2), both with the
//! SARA AM's cluster. The shaping pipeline splits it when it maps the
//! text to glyphs; [`preprocess`] then does what HarfBuzz does right
//! after its own split:
//!
//! - NIKHAHIT moves back over the above-base marks typed before the
//!   SARA AM (`<0E14, 0E4B, 0E33>` becomes `<0E14, 0E4D, 0E4B, 0E32>`),
//!   and the moved-over glyphs share one cluster at the monotone
//!   levels.
//! - Since NIKHAHIT is a combining mark, the decomposed glyphs join the
//!   cluster before them at the grapheme levels.

use super::cluster;
use crate::buffer::{ClusterLevel, Glyph};

/// NIKHAHIT, Thai or Lao.
const fn is_nikhahit(ch: char) -> bool {
    matches!(ch, '\u{0E4D}' | '\u{0ECD}')
}

/// HarfBuzz's `IS_ABOVE_BASE_MARK`: the Thai marks Uniscribe moves
/// NIKHAHIT over, and their Lao counterparts (Thai plus 0x80).
const fn is_above_base_mark(ch: char) -> bool {
    matches!(
        ch as u32 & !0x0080,
        0x0E31 | 0x0E34..=0x0E37 | 0x0E3B | 0x0E47..=0x0E4E
    )
}

/// True when `cps[i..i + 2]` is a split SARA AM: NIKHAHIT and the
/// SARA AA of the same script, sharing one cluster. Typed separately,
/// the two never share a cluster: SARA AA is a letter and starts its
/// own.
fn is_split_sara_am(cps: &[char], glyphs: &[Glyph], i: usize) -> bool {
    let (Some(&nikhahit), Some(&aa)) = (cps.get(i), cps.get(i + 1)) else {
        return false;
    };
    is_nikhahit(nikhahit)
        && aa as u32 == nikhahit as u32 - 0x1B
        && glyphs[i].cluster == glyphs[i + 1].cluster
}

/// Reorders and merges each split SARA AM in `cps` / `glyphs` /
/// `mirrored` (one entry per code point); see the module docs.
pub(super) fn preprocess(
    cps: &mut [char],
    glyphs: &mut [Glyph],
    mirrored: &mut [bool],
    level: ClusterLevel,
) {
    if glyphs.len() != cps.len() || mirrored.len() != cps.len() {
        return;
    }
    let mut i = 0;
    while i + 1 < cps.len() {
        if !is_split_sara_am(cps, glyphs, i) {
            i += 1;
            continue;
        }
        let end = i + 2;
        let mut start = i;
        while start > 0 && is_above_base_mark(cps[start - 1]) {
            start -= 1;
        }
        if start < i {
            // Move NIKHAHIT to the front of the marks it follows.
            cluster::merge_clusters(glyphs, start, end, level);
            cps[start..=i].rotate_right(1);
            glyphs[start..=i].rotate_right(1);
            mirrored[start..=i].rotate_right(1);
        }
        if start > 0 {
            cluster::merge_grapheme_clusters(glyphs, start - 1, end, level);
        }
        i = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Runs `preprocess` over `cps` whose clusters are `clusters`.
    fn run(cps: &str, clusters: &[u32], level: ClusterLevel) -> (Vec<char>, Vec<u32>) {
        let mut cps: Vec<char> = cps.chars().collect();
        let mut glyphs: Vec<Glyph> = clusters.iter().map(|&c| Glyph::new(0, c)).collect();
        let mut mirrored = alloc::vec![false; cps.len()];
        preprocess(&mut cps, &mut glyphs, &mut mirrored, level);
        (cps, glyphs.iter().map(|g| g.cluster).collect())
    }

    #[test]
    fn nikhahit_moves_over_above_base_marks() {
        // do dek, mai chattawa, then SARA AM split at cluster 6.
        let text = "\u{0E14}\u{0E4B}\u{0E4D}\u{0E32}";
        let moved: Vec<char> = "\u{0E14}\u{0E4D}\u{0E4B}\u{0E32}".chars().collect();
        for (level, clusters) in [
            (ClusterLevel::MonotoneCharacters, [0, 3, 3, 3]),
            (ClusterLevel::MonotoneGraphemes, [0, 0, 0, 0]),
            (ClusterLevel::Characters, [0, 6, 3, 6]),
            (ClusterLevel::Graphemes, [0, 0, 0, 0]),
        ] {
            let (cps, got) = run(text, &[0, 3, 6, 6], level);
            assert_eq!(cps, moved, "{level:?}");
            assert_eq!(got, clusters, "{level:?}");
        }
    }

    #[test]
    fn split_sara_am_joins_the_cluster_before_at_grapheme_levels() {
        let text = "\u{0E01}\u{0E4D}\u{0E32}";
        let (_, got) = run(text, &[0, 3, 3], ClusterLevel::MonotoneGraphemes);
        assert_eq!(got, [0, 0, 0]);
        let (_, got) = run(text, &[0, 3, 3], ClusterLevel::MonotoneCharacters);
        assert_eq!(got, [0, 3, 3]);
        // Lao works the same.
        let (_, got) = run(
            "\u{0E81}\u{0ECD}\u{0EB2}",
            &[0, 3, 3],
            ClusterLevel::Graphemes,
        );
        assert_eq!(got, [0, 0, 0]);
    }

    #[test]
    fn typed_nikhahit_and_sara_aa_are_left_alone() {
        let text = "\u{0E01}\u{0E4B}\u{0E4D}\u{0E32}";
        let (cps, got) = run(text, &[0, 3, 6, 9], ClusterLevel::MonotoneGraphemes);
        assert_eq!(cps, text.chars().collect::<Vec<_>>());
        assert_eq!(got, [0, 3, 6, 9]);
    }
}
