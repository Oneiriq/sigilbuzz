//! Syllable reordering: the moves Khmer and Myanmar make before their
//! features and the pre-base moves the Universal Shaping Engine makes
//! after its basic features, with the cluster merges each move makes.

use alloc::vec::Vec;
use core::ops::Range;

use super::{Syllable, SyllableKind};
use crate::buffer::{ClusterLevel, Glyph, IndicPosition};
use crate::shape::merge_clusters;
use crate::tables::layout::skip_iter::MatchGlyph;
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
    // The glyphs the moves below pass over: pre-base matras and the
    // coeng pair travel to the syllable start, the kinzi triple to
    // just after the base.
    let pair = pre_cons_idx.filter(|&pc| pc >= syl_start && pc + 1 < syl_end);
    let kinzi = kinzi_idx.filter(|&kz| kz >= syl_start && kz + 2 < syl_end);
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

/// Reorder category tag: a halant (`H`).
const TAG_HALANT: u8 = 1;
/// Reorder category tag: a pre-base vowel sign or modifier (`VPre`,
/// `VMPre`), or the glyph `pref` substituted.
const TAG_PRE_BASE: u8 = 2;
/// Mask of the reorder category in the tag byte.
const TAG_CATEGORY: u8 = 0x0F;
/// Shift of the syllable serial in the tag byte.
const TAG_SERIAL_SHIFT: u32 = 4;

/// Tags each glyph with its syllable and reorder category, which the
/// Universal Shaping Engine reads after its basic features. HarfBuzz
/// keeps both in the glyph info (`syllable()`, `use_category()`), where
/// a ligature keeps its first component's and a multiple
/// substitution's outputs their source's; sigilbuzz keeps them in
/// [`Glyph::indic_position`], which GSUB carries the same way: the
/// syllable serial (1 to 15, then 1 again, so neighbors always differ)
/// in the high nibble, the category in the low one. Returns `false`,
/// tagging nothing, unless glyphs and code points are one to one.
pub(super) fn tag_syllables(
    glyphs: &mut [Glyph],
    codepoints: &[char],
    syllables: &[Syllable],
) -> bool {
    if glyphs.len() != codepoints.len() {
        return false;
    }
    let mut serial = 1u8;
    for syl in syllables {
        for i in syl.start..syl.end.min(glyphs.len()) {
            let ch = codepoints[i];
            let category = if use_category(ch) == UseCategory::H {
                TAG_HALANT
            } else if use_category(ch) == UseCategory::VPre
                || use_position(ch) == UsePosition::PreBase
            {
                TAG_PRE_BASE
            } else {
                0
            };
            glyphs[i].indic_position = (serial << TAG_SERIAL_SHIFT) | category;
        }
        serial = serial % 15 + 1;
    }
    true
}

/// The glyph range of each syllable: the runs of glyphs sharing a
/// serial (HarfBuzz's `foreach_syllable`).
fn syllable_ranges(glyphs: &[Glyph]) -> Vec<Range<usize>> {
    let serial = |g: &Glyph| g.indic_position >> TAG_SERIAL_SHIFT;
    let mut ranges = Vec::new();
    let mut start = 0;
    for i in 1..=glyphs.len() {
        if i == glyphs.len() || serial(&glyphs[i]) != serial(&glyphs[start]) {
            ranges.push(start..i);
            start = i;
        }
    }
    ranges
}

/// HarfBuzz's `record_pref_use`: in each syllable, the first glyph
/// `pref` substituted becomes a pre-base glyph, given the glyph ids
/// from before `pref`. Does nothing when `pref` changed the glyph
/// count, which this comparison cannot follow.
pub(super) fn record_pref(before: &[u32], glyphs: &mut [Glyph]) {
    if before.len() != glyphs.len() {
        return;
    }
    for range in syllable_ranges(glyphs) {
        if let Some(i) = range.into_iter().find(|&i| glyphs[i].glyph_id != before[i]) {
            let g = &mut glyphs[i];
            g.indic_position = (g.indic_position & !TAG_CATEGORY) | TAG_PRE_BASE;
        }
    }
}

/// The pre-base moves of HarfBuzz's `reorder_syllable_use`, run after
/// the basic features on glyphs [`tag_syllables`] tagged: in each
/// syllable, a pre-base glyph moves back to the start of the syllable,
/// or to just after the last halant before it that did not ligate,
/// merging the clusters it passes at the monotone `level`s. Only the
/// first glyph of a multiple substitution moves. Clears the tags.
///
/// HarfBuzz moves each glyph on its own, which costs time quadratic in
/// the number of pre-base glyphs one insertion point collects. Here the
/// moves to one insertion point are collected and applied together
/// (see [`move_to_insertion_point`]), so the pass stays linear.
pub(super) fn reorder_pre_base(glyphs: &mut [Glyph], level: ClusterLevel) {
    let mut moves: Vec<usize> = Vec::new();
    let mut scratch: Vec<Glyph> = Vec::new();
    for range in syllable_ranges(glyphs) {
        let mut j = range.start;
        moves.clear();
        for i in range {
            // Moves only ever touch glyphs before `i`, so these are the
            // glyph's own tags whether or not earlier moves ran yet.
            let m = MatchGlyph::from(&glyphs[i]);
            let category = glyphs[i].indic_position & TAG_CATEGORY;
            if category == TAG_HALANT && !m.is_ligated() {
                move_to_insertion_point(glyphs, j, &moves, level, &mut scratch);
                moves.clear();
                j = i + 1;
            } else if category == TAG_PRE_BASE && m.lig_comp() == 0 && j < i {
                moves.push(i);
            }
        }
        move_to_insertion_point(glyphs, j, &moves, level, &mut scratch);
    }
    for g in glyphs {
        g.indic_position = IndicPosition::Start as u8;
    }
}

/// Applies the moves one insertion point `j` collected, in one pass:
/// each glyph at `moves` (ascending, all after `j`) moves to `j` in
/// turn, so the last one ends up first, and the glyphs they pass shift
/// right. That is the order moving them one at a time leaves.
///
/// One at a time, each move also merges the clusters from `j` through
/// the moved glyph. The ranges all start at `j` and grow, and the
/// glyphs of a merged range share one cluster, so the moves inside
/// it do not change any cluster. On a run whose clusters rise or fall
/// monotonically, as they do at the monotone levels, merging the
/// widest range once leaves the same clusters as merging each range
/// in turn.
fn move_to_insertion_point(
    glyphs: &mut [Glyph],
    j: usize,
    moves: &[usize],
    level: ClusterLevel,
    scratch: &mut Vec<Glyph>,
) {
    let (Some(&first), Some(&last)) = (moves.first(), moves.last()) else {
        return;
    };
    let ascending = moves.windows(2).all(|w| w[0] < w[1]);
    if !ascending || first <= j || last >= glyphs.len() {
        return;
    }
    merge_clusters(glyphs, j, last + 1, level);
    let span = &mut glyphs[j..=last];
    scratch.clear();
    scratch.extend(moves.iter().rev().map(|&i| span[i - j]));
    let mut next_move = moves.iter().peekable();
    for (k, &g) in span.iter().enumerate() {
        if next_move.peek() == Some(&&(j + k)) {
            next_move.next();
        } else {
            scratch.push(g);
        }
    }
    // Ascending moves inside the span make the lengths match.
    if scratch.len() == span.len() {
        span.copy_from_slice(scratch);
    }
}
