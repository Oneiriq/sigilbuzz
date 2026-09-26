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
    let mut rebuilt: Vec<Glyph> = Vec::with_capacity(original.len());

    // Syllable-relative flags for glyphs consumed by the earlier
    // buckets, which the fall-through must not re-emit. A flag array
    // keeps the pass linear in the syllable length even when a
    // syllable carries thousands of pre-base signs.
    let mut consumed = alloc::vec![false; original.len()];
    let kinzi = kinzi_idx.filter(|&kz| kz >= syl_start && kz + 2 < syl_end);

    // 1. Pre-base matras, in logical order.
    for &idx in &to_move {
        rebuilt.push(original[idx - syl_start]);
        consumed[idx - syl_start] = true;
    }
    // 2. Pre-base consonant pair (coeng + ra). Both glyphs move to
    //    the start of the syllable so the `pref` GSUB feature sees
    //    the pair adjacent AND the collapsed subscript-ra glyph
    //    already sits before the base.
    if let Some(pc) = pre_cons_idx {
        if pc >= syl_start && pc + 1 < syl_end {
            for rel in [pc - syl_start, pc + 1 - syl_start] {
                rebuilt.push(original[rel]);
                consumed[rel] = true;
            }
        }
    }
    // 3. Mark the kinzi triple as consumed so the fall-through
    //    doesn't re-emit them at the syllable head; we inject them
    //    right after the base consonant below.
    if let Some(kz) = kinzi {
        consumed[kz - syl_start..=kz + 2 - syl_start].fill(true);
    }
    // 4. Everything else, in original order, with the kinzi triple
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
pub(super) fn cluster_byte_offsets(codepoints: &[char]) -> Vec<u32> {
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
///
/// Syllables are consecutive, so their byte ranges are sorted and
/// disjoint and a glyph falls in at most one of them. Rewriting a
/// cluster to its syllable's start keeps it inside that range, so one
/// pass with a binary search per glyph gives the same result as
/// visiting every glyph once per syllable, in `O(n log s)` time.
pub(super) fn merge_syllable_clusters(
    glyphs: &mut [Glyph],
    syllables: &[Syllable],
    byte_offsets: &[u32],
) {
    let ranges: Vec<(u32, u32)> = syllables
        .iter()
        .filter(|syl| syl.end > syl.start)
        .filter_map(|syl| Some((*byte_offsets.get(syl.start)?, *byte_offsets.get(syl.end)?)))
        .collect();
    for g in glyphs {
        let after = ranges.partition_point(|&(start, _)| start <= g.cluster);
        let Some(&(start, end)) = after.checked_sub(1).and_then(|k| ranges.get(k)) else {
            continue;
        };
        if g.cluster < end {
            g.cluster = start;
        }
    }
}

/// Post-`pref` reorder. Walks one syllable and, for any position whose
/// glyph id changed under the `pref` feature AND whose original
/// codepoint was a [`UseCategory::CM`] sitting at
/// [`UsePosition::BelowBase`] (the textbook medial-ra), moves the
/// substituted glyph to the front of the syllable so it visually sits
/// before the base. Mirrors rustybuzz's `record_pref` ->
/// `reorder_syllable_use` pair, but only for the medial-ra case
/// (Cham). Length-preserving.
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
    if syllable.end > glyphs.len()
        || syllable.end > codepoints.len()
        || syllable.end > pre_ids.len()
        || !(syllable.start..syllable.end).contains(&base)
    {
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
    let mut rebuilt: Vec<Glyph> = Vec::with_capacity(original.len());
    let mut moved = alloc::vec![false; original.len()];

    // 1. Substituted pre-base forms in logical order.
    for &idx in &to_move {
        rebuilt.push(original[idx - syl_start]);
        moved[idx - syl_start] = true;
    }
    // 2. Everything else, in original order.
    for (rel, &glyph) in original.iter().enumerate() {
        if !moved[rel] {
            rebuilt.push(glyph);
        }
    }
    glyphs[syl_start..syl_end].copy_from_slice(&rebuilt);
}
