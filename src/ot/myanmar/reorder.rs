//! The syllable reorder Myanmar makes before its features: pre-base
//! vowel signs to the start of the syllable and a kinzi to just after
//! the base, with the cluster merges each move makes.

use alloc::vec::Vec;

use super::category::{category, is_pre_base, Category};
use super::{Syllable, SyllableKind};
use crate::buffer::{ClusterLevel, Glyph};
use crate::shape::merge_clusters;

/// Initial reorder for one syllable. Moves every pre-base vowel sign
/// in the syllable to the syllable head, and a Myanmar kinzi to just
/// after the base. Length-preserving: glyph count and codepoint count
/// stay aligned.
///
/// Every move spans the glyphs between the moved one's old and new
/// slots. At the monotone cluster `level`s those glyphs share one
/// cluster, as HarfBuzz's `merge_clusters` before each Myanmar sort
/// step leaves them.
pub(super) fn initial_reorder(
    codepoints: &[char],
    glyphs: &mut [Glyph],
    syllable: &Syllable,
    level: ClusterLevel,
) {
    if !matches!(syllable.kind, SyllableKind::Consonant) {
        return;
    }
    let Some(base) = syllable.base_index else {
        return;
    };
    if syllable.end > glyphs.len()
        || syllable.end > codepoints.len()
        || !(syllable.start..syllable.end).contains(&base)
    {
        return;
    }

    // Gather pre-base matra indices that live after the base: those
    // are the ones that need to move. A pre-base sign sitting before
    // the base is already in position (unusual but legal for
    // broken-cluster repair).
    let mut to_move: Vec<usize> = Vec::new();
    for (offset, &ch) in codepoints[base + 1..syllable.end].iter().enumerate() {
        if matches!(category(ch), Category::VPre) || is_pre_base(ch) {
            to_move.push(base + 1 + offset);
        }
    }

    // Myanmar kinzi prefix: three codepoints at `kinzi_index`,
    // `kinzi_index + 1`, `kinzi_index + 2` (Nga + Asat + Virama).
    // rustybuzz's Myanmar reorder tags them POS_AFTER_MAIN so the
    // sort drops them after the base consonant. sigilbuzz replicates
    // the resulting glyph order here. After reorder, `rphf` fires on
    // the still-adjacent triple and collapses it to the font's kinzi
    // glyph, which naturally sits in the reph slot (immediately after
    // the base consonant).
    let kinzi_idx = syllable.kinzi_index;

    if to_move.is_empty() && kinzi_idx.is_none() {
        return;
    }

    // Rebuild the syllable slice in one pass so we handle the
    // multi-matra and halant-stack cases without index drift.
    //
    // Target layout (USE pre-base rule per MS USE spec):
    //
    //   [pre-base matras in logical order]
    //   [everything else, in original order]
    //
    // Pre-base matras move to the very start of the syllable, not
    // just before the base. This keeps halant stacks intact so GSUB
    // `blwf` / `pstf` can still see `halant + consonant` pairs
    // adjacent and collapse them into a single subscript glyph.
    //
    // `base` is used below as the anchor for Myanmar kinzi
    // placement: the kinzi triple gets injected immediately after
    // the base consonant in the rebuilt slice, matching rustybuzz's
    // POS_AFTER_MAIN semantics.
    let syl_start = syllable.start;
    let syl_end = syllable.end;
    // The glyphs the moves below pass over: pre-base matras travel to
    // the syllable start, the kinzi triple to just after the base.
    let kinzi = kinzi_idx.filter(|&kz| kz >= syl_start && kz + 2 < syl_end);
    let mut span = to_move.last().map(|&last| syl_start..last + 1);
    if let Some(kz) = kinzi {
        span = Some(span.map_or(kz..base + 1, |s| s.start.min(kz)..s.end.max(base + 1)));
    }
    if let Some(span) = span {
        merge_clusters(glyphs, span.start, span.end, level);
    }
    let original: Vec<Glyph> = glyphs[syl_start..syl_end].to_vec();
    let mut rebuilt: Vec<Glyph> = Vec::with_capacity(original.len());

    // Syllable-relative flags for glyphs consumed by the earlier
    // buckets, which the fall-through must not re-emit. A flag array
    // keeps the pass linear in the syllable length even when a
    // syllable carries thousands of pre-base signs.
    let mut consumed = alloc::vec![false; original.len()];

    // 1. Pre-base matras, in logical order.
    for &idx in &to_move {
        rebuilt.push(original[idx - syl_start]);
        consumed[idx - syl_start] = true;
    }
    // 2. Mark the kinzi triple as consumed so the fall-through
    //    doesn't re-emit them at the syllable head. We inject them
    //    right after the base consonant below.
    if let Some(kz) = kinzi {
        consumed[kz - syl_start..=kz + 2 - syl_start].fill(true);
    }
    // 3. Everything else, in original order, with the kinzi triple
    //    injected immediately after the base consonant.
    for (rel, &glyph) in original.iter().enumerate() {
        if consumed[rel] {
            continue;
        }
        rebuilt.push(glyph);
        if syl_start + rel == base {
            if let Some(kz) = kinzi {
                rebuilt.extend_from_slice(&original[kz - syl_start..=kz + 2 - syl_start]);
            }
        }
    }

    // Every glyph of the syllable is emitted exactly once for the
    // syllables the segmenter builds. If a future category table ever
    // breaks that, keep the syllable as it was instead of panicking.
    if rebuilt.len() == original.len() {
        glyphs[syl_start..syl_end].copy_from_slice(&rebuilt);
    }
}
