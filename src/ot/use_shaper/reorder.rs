//! Syllable reordering: the initial pre-base moves and the post-`pref`
//! medial move, with the cluster merges each move makes.

use alloc::vec::Vec;

use super::{Syllable, SyllableKind};
use crate::buffer::{ClusterLevel, Glyph};
use crate::shape::merge_clusters;
use crate::unicode::use_category::{use_category, use_position, UseCategory, UsePosition};

/// Initial reorder for one syllable. Moves every pre-base vowel sign
/// in the syllable to sit immediately before the base consonant, and
/// promotes pre-base consonant pairs (Khmer `coeng + ra`) to the
/// syllable head so the `pref` GSUB feature sees them adjacent AND
/// their output glyph naturally sits before the base.
/// Length-preserving: glyph count and codepoint count stay aligned.
///
/// Every move spans the glyphs between the moved one's old and new
/// slots; at the monotone cluster `level`s those glyphs share one
/// cluster, as HarfBuzz's `merge_clusters` before each Khmer move
/// (`reorder_consonant_syllable`) and each Myanmar sort step leaves
/// them.
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
    if syllable.end > glyphs.len() || syllable.end > codepoints.len() {
        return;
    }

    // Gather pre-base matra indices that live after the base: those
    // are the ones that need to move. A pre-base sign sitting before
    // the base is already in position (unusual but legal for
    // broken-cluster repair).
    let mut to_move: Vec<usize> = Vec::new();
    for (offset, &ch) in codepoints[base + 1..syllable.end].iter().enumerate() {
        if matches!(use_category(ch), UseCategory::VPre) || use_position(ch) == UsePosition::PreBase
        {
            to_move.push(base + 1 + offset);
        }
    }

    // Pre-base consonant pair indices (Khmer `coeng + ra` = the two
    // codepoints at `pre_base_cons_index` and that + 1). These
    // move to the start of the syllable, BEFORE the pre-base
    // matras, so the visual order ends up as
    // `[matras, pre-base cons pair, everything else, base, ...]`.
    let pre_cons_idx = syllable.pre_base_cons_index;

    // Myanmar kinzi prefix: three codepoints at `kinzi_index`,
    // `kinzi_index + 1`, `kinzi_index + 2` (Nga + Asat + Virama).
    // rustybuzz's Myanmar reorder tags them POS_AFTER_MAIN so the
    // sort drops them after the base consonant; sigilbuzz replicates
    // the resulting glyph order here. After reorder, `rphf` fires on
    // the still-adjacent triple and collapses it to the font's kinzi
    // glyph, which naturally sits in the reph slot (immediately after
    // the base consonant).
    let kinzi_idx = syllable.kinzi_index;

    if to_move.is_empty() && pre_cons_idx.is_none() && kinzi_idx.is_none() {
        return;
    }

    // Rebuild the syllable slice in one pass so we handle the
    // multi-matra and coeng-stack cases without index drift.
    //
    // Target layout (USE pre-base rule per MS USE spec):
    //
    //   [pre-base matras in logical order]
    //   [everything else, in original order]
    //
    // Pre-base matras move to the very start of the syllable, not
    // just before the base. This keeps coeng stacks intact so GSUB
    // `blwf` / `pstf` can still see `halant + consonant` pairs
    // adjacent and collapse them into a single subscript glyph.
    //
    // For `sa + coeng + ta + sign-e` the result is
    // `[sign-e, sa, coeng, ta]`. The subsequent `blwf` pass sees
    // `coeng + ta` still adjacent and collapses to a single
    // subscript-ta glyph, matching rustybuzz.
    //
    // `base` is used below as the anchor for Myanmar kinzi
    // placement: the kinzi triple gets injected immediately after
    // the base consonant in the rebuilt slice, matching rustybuzz's
    // POS_AFTER_MAIN semantics.
    let syl_start = syllable.start;
    let syl_end = syllable.end;
    // The glyphs the moves below pass over: pre-base matras and the
    // coeng pair travel to the syllable start, the kinzi triple to
    // just after the base.
    let pair = pre_cons_idx.filter(|&pc| pc >= syl_start && pc + 1 < syl_end);
    let kinzi = kinzi_idx.filter(|&kz| kz + 2 < syl_end);
    let mut span = to_move.last().map(|&last| syl_start..last + 1);
    if let Some(pc) = pair {
        span = Some(syl_start..span.map_or(pc + 2, |s| s.end.max(pc + 2)));
    }
    if let Some(kz) = kinzi {
        span = Some(span.map_or(kz..base + 1, |s| s.start.min(kz)..s.end.max(base + 1)));
    }
    if let Some(span) = span {
        merge_clusters(glyphs, span.start, span.end, level);
    }
    let original: Vec<Glyph> = glyphs[syl_start..syl_end].to_vec();
    let mut rebuilt: Vec<Glyph> = Vec::with_capacity(syl_end - syl_start);

    // Set of indices whose glyphs are consumed by the earlier
    // buckets and must not be re-emitted by the fall-through.
    let mut consumed: Vec<usize> = Vec::new();

    // 1. Pre-base matras, in logical order.
    for &idx in &to_move {
        rebuilt.push(original[idx - syl_start]);
        consumed.push(idx);
    }
    // 2. Pre-base consonant pair (coeng + ra). Both glyphs move to
    //    the start of the syllable so the `pref` GSUB feature sees
    //    the pair adjacent AND the collapsed subscript-ra glyph
    //    already sits before the base.
    if let Some(pc) = pre_cons_idx {
        if pc >= syl_start && pc + 1 < syl_end {
            rebuilt.push(original[pc - syl_start]);
            rebuilt.push(original[pc + 1 - syl_start]);
            consumed.push(pc);
            consumed.push(pc + 1);
        }
    }
    // 3. Mark the kinzi triple as consumed so the fall-through
    //    doesn't re-emit them at the syllable head; we inject them
    //    right after the base consonant below.
    if let Some(kz) = kinzi_idx {
        if kz + 2 < syl_end {
            consumed.push(kz);
            consumed.push(kz + 1);
            consumed.push(kz + 2);
        }
    }
    // 4. Everything else, in original order, with the kinzi triple
    //    injected immediately after the base consonant.
    for idx in syl_start..syl_end {
        if consumed.contains(&idx) {
            continue;
        }
        rebuilt.push(original[idx - syl_start]);
        if idx == base {
            if let Some(kz) = kinzi_idx {
                if kz + 2 < syl_end {
                    rebuilt.push(original[kz - syl_start]);
                    rebuilt.push(original[kz + 1 - syl_start]);
                    rebuilt.push(original[kz + 2 - syl_start]);
                }
            }
        }
    }

    debug_assert_eq!(rebuilt.len(), syl_end - syl_start);
    glyphs[syl_start..syl_end].copy_from_slice(&rebuilt);
}

/// Post-`pref` reorder. Walks one syllable and, for any position whose
/// glyph id changed under the `pref` feature AND whose original
/// codepoint was a [`UseCategory::CM`] sitting at
/// [`UsePosition::BelowBase`] (the textbook medial-ra), moves the
/// substituted glyph to the front of the syllable so it visually sits
/// before the base. Mirrors rustybuzz's `record_pref` ->
/// `reorder_syllable_use` pair, but only for the medial-ra case the
/// 0.8.0 corpus exercises (Cham). Length-preserving.
pub(super) fn pref_reorder(
    codepoints: &[char],
    glyphs: &mut [Glyph],
    syllable: &Syllable,
    pre_ids: &[u32],
    level: ClusterLevel,
) {
    if !matches!(syllable.kind, SyllableKind::Consonant) {
        return;
    }
    let Some(base) = syllable.base_index else {
        return;
    };
    if syllable.end > glyphs.len() || syllable.end > codepoints.len() {
        return;
    }

    // Find positions in (base, end) whose glyph id changed under
    // `pref` and whose original codepoint was a below-base CM.
    let mut to_move: Vec<usize> = Vec::new();
    for idx in (base + 1)..syllable.end {
        if glyphs[idx].glyph_id == pre_ids[idx] {
            continue;
        }
        let ch = codepoints[idx];
        if use_category(ch) != UseCategory::CM {
            continue;
        }
        if use_position(ch) != UsePosition::BelowBase {
            continue;
        }
        to_move.push(idx);
    }
    if to_move.is_empty() {
        return;
    }

    let syl_start = syllable.start;
    let syl_end = syllable.end;
    // The moved forms share one cluster with what they pass over (at
    // the monotone levels), as in HarfBuzz's `reorder_syllable_use`.
    if let Some(&last) = to_move.last() {
        merge_clusters(glyphs, syl_start, last + 1, level);
    }
    let original: Vec<Glyph> = glyphs[syl_start..syl_end].to_vec();
    let mut rebuilt: Vec<Glyph> = Vec::with_capacity(syl_end - syl_start);

    // 1. Substituted pre-base forms in logical order.
    for &idx in &to_move {
        rebuilt.push(original[idx - syl_start]);
    }
    // 2. Everything else, in original order.
    for idx in syl_start..syl_end {
        if to_move.contains(&idx) {
            continue;
        }
        rebuilt.push(original[idx - syl_start]);
    }
    debug_assert_eq!(rebuilt.len(), syl_end - syl_start);
    glyphs[syl_start..syl_end].copy_from_slice(&rebuilt);
}
