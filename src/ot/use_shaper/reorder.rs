//! Syllable reordering, the initial pre-base moves and the post-`pref`
//! medial move, and the cluster bookkeeping around it.

use alloc::vec::Vec;

use super::{Syllable, SyllableKind};
use crate::buffer::Glyph;
use crate::unicode::use_category::{use_category, use_position, UseCategory, UsePosition};

/// Initial reorder for one syllable. Moves every pre-base vowel sign
/// in the syllable to sit immediately before the base consonant, and
/// promotes pre-base consonant pairs (Khmer `coeng + ra`) to the
/// syllable head so the `pref` GSUB feature sees them adjacent AND
/// their output glyph naturally sits before the base.
/// Length-preserving: glyph count and codepoint count stay aligned.
pub(super) fn initial_reorder(codepoints: &[char], glyphs: &mut [Glyph], syllable: &Syllable) {
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

/// Returns a length-`codepoints.len() + 1` array mapping each code
/// point to the cluster its glyph carries, with an open end. Read
/// before any reordering or GSUB, while glyphs are one per code point,
/// these are the run's real UTF-8 offsets, right for a segment that
/// does not start the text and for decomposed vowels whose parts share
/// a cluster. Falls back to offsets counted from the code points when
/// the glyphs are not one per code point.
pub(super) fn code_point_clusters(codepoints: &[char], glyphs: &[Glyph]) -> Vec<u32> {
    if glyphs.len() != codepoints.len() {
        return cluster_byte_offsets(codepoints);
    }
    glyphs
        .iter()
        .map(|g| g.cluster)
        .chain(core::iter::once(u32::MAX))
        .collect()
}

/// Returns a length-`codepoints.len() + 1` array mapping codepoint
/// index to UTF-8 byte offset. `out[i]` is the byte offset of the
/// i'th codepoint; `out[len]` is the total byte length.
fn cluster_byte_offsets(codepoints: &[char]) -> Vec<u32> {
    let mut out = Vec::with_capacity(codepoints.len() + 1);
    let mut byte = 0u32;
    for &c in codepoints {
        out.push(byte);
        byte = byte.saturating_add(c.len_utf8() as u32);
    }
    out.push(byte);
    out
}

/// Merges all cluster byte offsets that belong to a syllable to the
/// minimum offset in that syllable's byte range. Matches HarfBuzz /
/// rustybuzz behavior. Downstream callers see one cluster id per
/// syllable (the byte offset of the first codepoint) even when GSUB
/// substitutions have collapsed glyphs inside the syllable.
pub(super) fn merge_syllable_clusters(
    glyphs: &mut [Glyph],
    syllables: &[Syllable],
    byte_offsets: &[u32],
) {
    for syl in syllables {
        if syl.end == syl.start {
            continue;
        }
        let byte_start = byte_offsets[syl.start];
        let byte_end = byte_offsets[syl.end];
        for g in glyphs.iter_mut() {
            if g.cluster >= byte_start && g.cluster < byte_end {
                g.cluster = byte_start;
            }
        }
    }
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
