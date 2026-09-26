//! Per-syllable reordering: position tagging, initial reordering, the
//! `half` feature mask, final reph placement, and the cluster
//! bookkeeping around them.

use alloc::vec::Vec;

use super::{IndicConfig, RephMode, RephPosition, Syllable, SyllableKind};
use crate::buffer::{ClusterLevel, Glyph, IndicPosition};
use crate::shape::{feature_would_substitute, merge_clusters};
use crate::tables::gdef::Gdef;
use crate::tables::layout::Joiners;
use crate::tables::Gsub;
use crate::unicode::indic_category::{
    positional_category, syllabic_category, IndicPositionalCategory, IndicSyllabicCategory,
};

/// Sets per-glyph Indic positions for the glyphs in one syllable's
/// range. Called BEFORE any reorder or GSUB pass, so indices in
/// `glyphs` still line up one-to-one with `codepoints`.
///
/// Three positions matter for the final reorder pass:
///
/// - [`IndicPosition::RaToBecomeReph`] on the leading `ra` of a
///   `ra + halant + ...` syllable. The `rphf` ligature will turn the
///   ra-halant pair into a reph glyph; the ligature path preserves
///   the first component's `Glyph` struct, so the tag survives.
/// - [`IndicPosition::BaseC`] on the base consonant.
/// - [`IndicPosition::PreM`] on pre-base matra glyphs.
///
/// Other glyphs keep the default [`IndicPosition::Start`].
pub(super) fn tag_positions(codepoints: &[char], glyphs: &mut [Glyph], syllable: &Syllable) {
    if !matches!(syllable.kind, SyllableKind::Consonant) {
        return;
    }
    if syllable.end > glyphs.len() || syllable.end > codepoints.len() {
        return;
    }

    // Mark reph candidate. Only valid when the syllable
    // starts with ra + halant AND has a base consonant after,
    // caught at segmentation via `has_reph`.
    if syllable.has_reph {
        glyphs[syllable.start].indic_position = IndicPosition::RaToBecomeReph as u8;
    }

    // Mark the base consonant.
    if let Some(base) = syllable.base_index {
        if base < glyphs.len() {
            glyphs[base].indic_position = IndicPosition::BaseC as u8;
        }
    }

    // Mark pre-base matras. These live logically after the base
    // consonant but visually before it; the `initial_reorder` pass
    // will physically move them. Tagging survives that reorder
    // because the tag is on the `Glyph`, not the slot.
    for (idx, &ch) in codepoints
        .iter()
        .enumerate()
        .take(syllable.end)
        .skip(syllable.start)
    {
        if positional_category(ch) == IndicPositionalCategory::Left
            && syllabic_category(ch) == IndicSyllabicCategory::VowelDependent
        {
            glyphs[idx].indic_position = IndicPosition::PreM as u8;
        }
    }
}

/// Initial reordering for one syllable.
///
/// The main transformation is moving pre-base matras (positional
/// category `Left`) from after the base consonant to immediately
/// before it. That puts the glyph run into the logical order the
/// GSUB basic features and the final reordering step expect.
///
/// Glyphs from the base on that the moves displaced then share
/// clusters at the monotone cluster `level`s, HarfBuzz's
/// `merge_clusters` over each permutation cycle past the base; what
/// ends up before the base merges in final reordering instead.
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

    // Collect pre-base matra indices inside the syllable, excluding
    // the base and anything preceding it.
    let Some(after_base) = codepoints.get(base + 1..syllable.end) else {
        return;
    };
    let Some(base_rel) = base.checked_sub(syllable.start) else {
        return;
    };
    let mut to_move: Vec<usize> = Vec::new();
    for (offset, &ch) in after_base.iter().enumerate() {
        if positional_category(ch) == IndicPositionalCategory::Left
            && syllabic_category(ch) == IndicSyllabicCategory::VowelDependent
        {
            to_move.push(base + 1 + offset);
        }
    }
    let Some(&last_move) = to_move.last() else {
        return;
    };

    // Take each pre-base matra, from the last one back, and splice it
    // in just before the base: the glyph at the matra index moves to
    // `base` and the glyphs in between shift right by one. The reph
    // stays leftmost and the matra slots in after the reph's halant,
    // i.e. before the base still. The moves are prefix rotations of
    // the slots from the base on, relative to `base`, largest first.
    //
    // `order[k]` is the index the glyph now at `syllable.start + k`
    // came from; the rotations run on it, and the glyphs follow.
    let mut order: Vec<usize> = (syllable.start..syllable.end).collect();
    let rotations: Vec<usize> = to_move.iter().rev().map(|&idx| idx - base).collect();
    let Some(moved) = order.get_mut(base_rel..=last_move - syllable.start) else {
        return;
    };
    rotate_prefixes_right(moved, &rotations);
    let Some(reordered) = order
        .iter()
        .map(|&from| glyphs.get(from).copied())
        .collect::<Option<Vec<Glyph>>>()
    else {
        return;
    };
    glyphs[syllable.start..syllable.end].copy_from_slice(&reordered);
    if let Some(new_base) = order.iter().position(|&from| from == base) {
        let new_base = syllable.start + new_base;
        merge_displaced_after_base(glyphs, &order, syllable.start, new_base, level);
    }
}

/// Applies a sequence of prefix rotations to `items`. For each `r` in
/// `rotations`, in order, the item at index `r` moves to index 0 and
/// the items at `0..r` shift right by one.
///
/// `rotations` must be strictly decreasing and every entry must be
/// below `items.len()`. Anything else leaves `items` unchanged.
///
/// Applying the rotations one at a time costs `O(len * rotations)`,
/// which a syllable with thousands of pre-base matras turns into a
/// hang. This version computes the same permutation in linear time.
/// While a rotation index `r` is at least the number of items already
/// moved to the front in the current pass, it picks the untouched item
/// at index `r - moved`. Once a rotation index falls inside the moved
/// prefix, every later one does too, so the rest of the rotations run
/// again on that prefix alone.
pub(super) fn rotate_prefixes_right<T: Copy>(items: &mut [T], rotations: &[usize]) {
    let valid = rotations.windows(2).all(|w| w[0] > w[1])
        && !rotations.first().is_some_and(|&r| r >= items.len());
    if !valid {
        return;
    }
    let mut len = items.len();
    let mut rest = rotations;
    let mut scratch: Vec<T> = Vec::with_capacity(len);
    while !rest.is_empty() {
        let Some(work) = items.get_mut(..len) else {
            return;
        };
        // Rotations that pick from the untouched items in this pass.
        let picks = rest
            .iter()
            .enumerate()
            .take_while(|&(moved, &r)| r >= moved)
            .count();
        // Item picked by rotation `t` sits at `rest[t] - t`. Those
        // indices strictly decrease with `t`, and the last pick lands
        // at the front of the result.
        scratch.clear();
        for (t, &r) in rest[..picks].iter().enumerate().rev() {
            scratch.push(work[r - t]);
        }
        let mut picked = rest[..picks]
            .iter()
            .enumerate()
            .rev()
            .map(|(t, &r)| r - t)
            .peekable();
        for (i, &item) in work.iter().enumerate() {
            if picked.peek() == Some(&i) {
                picked.next();
            } else {
                scratch.push(item);
            }
        }
        // Every index of `work` was pushed exactly once, so the lengths
        // match. The guard only keeps a broken invariant from panicking.
        if scratch.len() != work.len() {
            return;
        }
        work.copy_from_slice(&scratch);
        len = picks;
        rest = &rest[picks..];
    }
}

/// HarfBuzz's cluster merge at the end of Indic initial reordering:
/// for every permutation cycle that reaches a slot at or after the
/// base (`base`, after the moves), the slots from the base (or the
/// cycle's first slot, if later) through the cycle's last slot merge.
/// `order[k]` is the index the glyph now at `start + k` came from.
/// A syllable longer than 127 merges everything from the base on.
fn merge_displaced_after_base(
    glyphs: &mut [Glyph],
    order: &[usize],
    start: usize,
    base: usize,
    level: ClusterLevel,
) {
    let end = start + order.len();
    if order.len() > 127 {
        merge_clusters(glyphs, base, end, level);
        return;
    }
    let mut visited = alloc::vec![false; order.len()];
    for i in base..end {
        if visited[i - start] {
            continue;
        }
        let (mut min, mut max) = (i, i);
        let mut j = order[i - start];
        while j != i {
            min = min.min(j);
            max = max.max(j);
            visited[j - start] = true;
            j = order[j - start];
        }
        merge_clusters(glyphs, base.max(min), max + 1, level);
    }
}

/// Builds a per-glyph mask for the `half` feature.
///
/// Default is `true` (fire `half` everywhere, matching the pre-mask
/// behavior). A position is flipped to `false` when:
///
/// 1. It sits in a consonant syllable immediately before a `virama +
///    consonant` pair, AND
/// 2. The font's `blwf` feature would substitute that
///    `virama + consonant` pair.
///
/// That's the sigilbuzz-sized equivalent of HarfBuzz's
/// `consonant_position_from_face` check: when the post-halant
/// consonant is blwf-eligible, HarfBuzz tags it `POS_BELOW_C` and the
/// base moves back to the pre-halant consonant. Under the standard
/// Indic mask scheme the pre-halant positions then no longer get the
/// HALF mask bit, so `half` can't fire on them. We encode the same
/// decision directly: the mask keeps `half` off at those positions.
///
/// Glyph count has not yet shrunk when this runs (we call it before
/// any GSUB feature fires), so the mask indexes match the final
/// `glyphs` slice one-to-one.
pub(super) fn compute_half_mask(
    gsub: &Gsub<'_>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &[Glyph],
    config: &IndicConfig,
    syllables: &[Syllable],
) -> Vec<bool> {
    let mut mask = alloc::vec![true; glyphs.len()];
    if codepoints.len() != glyphs.len() {
        // Decomposition expanded the glyph stream past the codepoint
        // run or vice versa; fall back to fire-everywhere to avoid
        // mis-indexing. Parity tests catch the over-fire case when it
        // matters.
        return mask;
    }
    // Per-shape memoization of the dry-run check. Indic corpora reuse
    // the same handful of `(halant_glyph, c2_glyph)` pairs across
    // syllables (every "ष" + virama + "ट" triple maps to the same
    // glyph IDs), so caching the verdict turns the worst-case cost from
    // `O(N_pairs * 6 * LookupCost)` into `O(N_unique_pairs * 6 * LookupCost)`.
    // The cache is stack-local (no cross-shape state), so determinism
    // is preserved.
    let mut cache: Vec<((u16, u16), bool)> = Vec::new();
    let mut eligible_for = |halant_glyph: u16, c2_glyph: u16| -> bool {
        let key = (halant_glyph, c2_glyph);
        if let Some(&(_, hit)) = cache.iter().find(|(k, _)| *k == key) {
            return hit;
        }
        let hit = [*b"blwf", *b"pstf", *b"abvf"].iter().any(|&tag| {
            let prio = config.script_priority;
            let joiners = Joiners::MANUAL;
            feature_would_substitute(gsub, gdef, tag, prio, &[halant_glyph, c2_glyph], joiners)
                || feature_would_substitute(
                    gsub,
                    gdef,
                    tag,
                    prio,
                    &[c2_glyph, halant_glyph],
                    joiners,
                )
        });
        cache.push((key, hit));
        hit
    };
    for syllable in syllables {
        if !matches!(syllable.kind, SyllableKind::Consonant) {
            continue;
        }
        // Walk the syllable looking for `C + H + C` triples and check
        // the trailing pair against `blwf`. For each eligible triple
        // the pre-halant consonant and its halant get masked off.
        let mut i = syllable.start;
        while i + 2 < syllable.end {
            let (c1, h, c2) = (codepoints[i], codepoints[i + 1], codepoints[i + 2]);
            if !is_consonant(c1) {
                i += 1;
                continue;
            }
            if h as u32 != config.virama {
                i += 1;
                continue;
            }
            if !is_consonant(c2) {
                i += 1;
                continue;
            }
            let halant_glyph = glyphs[i + 1].glyph_id as u16;
            let c2_glyph = glyphs[i + 2].glyph_id as u16;
            // A post-halant consonant is below-base / post-base
            // eligible when any of the position-establishing features
            // would substitute it. Fonts vary: Noto Sans Devanagari
            // exposes `blwf` for its below-base consonants, Noto Sans
            // Gurmukhi uses `pstf` for the yakash (ya after halant),
            // and `pref` carries pre-base-reordering Ra. Check all
            // three both in new-spec (H+C) and old-spec (C+H) order,
            // matching `consonant_position_from_face` in rustybuzz.
            if eligible_for(halant_glyph, c2_glyph) {
                mask[i] = false;
                mask[i + 1] = false;
            }
            i += 1;
        }
    }
    mask
}

fn is_consonant(ch: char) -> bool {
    matches!(
        syllabic_category(ch),
        IndicSyllabicCategory::Consonant | IndicSyllabicCategory::ConsonantPlaceholder
    )
}

/// Returns a length-`codepoints.len() + 1` array mapping each code
/// point to the cluster its glyph carries, with an open end. The
/// clusters are the run's real UTF-8 offsets, so they hold for a
/// segment that does not start the text and for code points that
/// share a cluster (split matras). Falls back to offsets counted from
/// the code points when the glyphs are no longer one per code point.
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
/// index to UTF-8 byte offset.
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

/// Final reordering for one Indic syllable, in glyph space.
///
/// `syllable_glyphs` lists, in ascending order, the indices of the
/// glyphs whose `cluster` falls in the syllable's UTF-8 byte range.
/// Cluster byte offsets are stable across GSUB (ligatures keep the
/// first component's cluster, multiple-sub replicates it), so this
/// mapping works even after `rphf` has collapsed `ra + halant` into
/// a single reph glyph.
///
/// The reph target slot is selected per-script:
///
/// - [`RephPosition::BeforePost`]: Devanagari/Gujarati. Reph lands
///   just before any post-base matra, i.e. immediately after the
///   base consonant (and below-base forms, if any). For our
///   consonant-syllable structure that collapses to "end of syllable
///   past trailing SMVD marks", which is what the Devanagari
///   ground-truth tests exercise.
/// - [`RephPosition::AfterPost`]: Tamil, Telugu, Kannada, Sinhala.
///   Reph lands after any post-base matra, i.e. at the very end of
///   the syllable excluding trailing SMVD marks.
/// - [`RephPosition::AfterMain`]: Oriya/Malayalam. Reph lands
///   immediately after the base consonant (BaseC tag) and before
///   any post-base matra / mark.
/// - [`RephPosition::BeforeSub`]: Gurmukhi. Reph lands before any
///   sub-joined consonant. With sigilbuzz's flat `(C halant C)*`
///   syllable structure the sub-joined form, once generated by
///   `blwf`/`pstf`, sits to the right of the base consonant, so
///   the "after BaseC but before everything else" target matches
///   the AfterMain walker in practice.
/// - [`RephPosition::AfterSub`]: Bengali. Reph lands after any
///   sub-joined consonant. Equivalent to "end of syllable past
///   trailing SMVD" when no post-base matra follows.
///
/// When the `rphf` feature did not fire (the font ships no reph
/// form), the surviving `ra` glyph keeps its
/// [`IndicPosition::RaToBecomeReph`] tag but there is no stand-alone
/// reph glyph to move. We detect this by comparing the post-feature
/// glyph count to the original.
///
/// Before the reph moves, it and the glyphs it passes over share one
/// cluster at the monotone cluster `level`s, HarfBuzz's
/// `merge_clusters (start, new_reph_pos + 1)`; the other levels move
/// it with its own cluster.
///
/// Returns the inclusive index range whose glyphs were moved, or
/// `None` when nothing moved.
fn final_reorder_members(
    glyphs: &mut [Glyph],
    syllable_glyphs: &[usize],
    original_glyph_count: usize,
    reph_pos: RephPosition,
    reph_mode: RephMode,
    level: ClusterLevel,
) -> Option<(usize, usize)> {
    let (&first_in_syllable, &last_in_syllable) =
        (syllable_glyphs.first()?, syllable_glyphs.last()?);
    if syllable_glyphs.len() < 2 || last_in_syllable >= glyphs.len() {
        return None;
    }

    // For Implicit / Explicit modes the ra+halant pair collapses to
    // a single reph glyph via `rphf`; if the glyph count did NOT
    // shrink, the font has no reph form and there is nothing to
    // relocate. [`RephMode::LogRepha`] is different: the logrepha
    // is encoded as its own codepoint with its own glyph, so we
    // always run the reorder regardless of glyph-count shrinkage.
    if reph_mode != RephMode::LogRepha && syllable_glyphs.len() >= original_glyph_count {
        return None;
    }

    // Find the reph within this syllable.
    let &reph_idx = syllable_glyphs
        .iter()
        .find(|&&i| glyphs[i].indic_position == IndicPosition::RaToBecomeReph as u8)?;

    if reph_idx != first_in_syllable {
        // Already moved, nothing to do.
        return None;
    }

    // Compute target slot per-script.
    // Walker A: "end of syllable past trailing SMVD marks". Drops
    // vedic / cantillation marks off the tail so the reph sits just
    // before them rather than visually at the very end. Used by
    // BeforePost / AfterPost / AfterSub.
    let end_past_smvd = || {
        let mut t = last_in_syllable;
        while t > reph_idx && glyphs[t].indic_position == IndicPosition::Smvd as u8 {
            t -= 1;
        }
        t
    };
    // Walker B: "immediately after the base consonant". Lands on
    // the BaseC tag if found after the reph, else falls back to A
    // so we don't strand the reph on an unresolved syllable. Used by
    // AfterMain and BeforeSub (the latter because, with our flat
    // syllable structure, a sub-joined form sits right after the
    // base and "before it" and "right on the base glyph" collapse
    // to the same slot).
    let on_base_or_end = || {
        syllable_glyphs
            .iter()
            .copied()
            .find(|&i| i > reph_idx && glyphs[i].indic_position == IndicPosition::BaseC as u8)
            .unwrap_or_else(end_past_smvd)
    };
    let target = match reph_pos {
        RephPosition::BeforePost | RephPosition::AfterPost | RephPosition::AfterSub => {
            end_past_smvd()
        }
        RephPosition::AfterMain | RephPosition::BeforeSub => on_base_or_end(),
    };

    if target <= reph_idx {
        return None; // Nothing to move past.
    }

    // Move `glyphs[reph_idx]` to `target` by shifting the slots
    // between them left by one, after merging the range the reph
    // passes over as HarfBuzz does.
    merge_clusters(glyphs, reph_idx, target + 1, level);
    glyphs[reph_idx..=target].rotate_left(1);
    Some((reph_idx, target))
}

/// HarfBuzz's final-reordering merge for pre-base matras: when a
/// matra sits before the base consonant of the syllable whose glyphs
/// are `syllable_glyphs` (ascending indices), the glyphs from the
/// first such matra through the base share one cluster at the
/// monotone cluster `level`s (`merge_clusters (i, hb_min (end, base +
/// 1))`). Only the unbroken run of indices from the first glyph on
/// counts. Without a tagged base glyph left, the merge runs to the
/// syllable's end, as HarfBuzz's does when it loses track of the base.
fn merge_pre_base_matra_members(
    glyphs: &mut [Glyph],
    syllable_glyphs: &[usize],
    level: ClusterLevel,
) {
    let Some(&start) = syllable_glyphs.first() else {
        return;
    };
    let run = syllable_glyphs
        .iter()
        .enumerate()
        .take_while(|&(k, &i)| i == start + k)
        .count();
    let Some(syllable) = glyphs.get(start..start + run) else {
        return;
    };
    let base = syllable
        .iter()
        .position(|g| g.indic_position == IndicPosition::BaseC as u8)
        .map_or(syllable.len(), |b| b + 1);
    if let Some(matra) = syllable[..base.min(syllable.len())]
        .iter()
        .position(|g| g.indic_position == IndicPosition::PreM as u8)
    {
        merge_clusters(glyphs, start + matra, start + base, level);
    }
}

/// Runs the final reorder, and the pre-base matra merge before it, for
/// every syllable, in order, in time linear in the glyph count.
///
/// A glyph belongs to the syllable whose byte range holds its
/// cluster. Rather than rescanning every glyph once per syllable, the
/// glyph indices are bucketed by syllable up front. A reorder moves
/// only glyphs of its own syllable, and the cluster merges give them
/// clusters from that syllable's range, so those glyphs can no longer
/// belong to a later syllable. `owner` records that, and each bucket
/// is filtered by it before use. The result is the same as filtering
/// all glyphs by byte range just before each syllable is reordered.
pub(super) fn final_reorder_all(
    glyphs: &mut [Glyph],
    syllables: &[Syllable],
    byte_offsets: &[u32],
    config: &IndicConfig,
    level: ClusterLevel,
) {
    const NO_OWNER: usize = usize::MAX;
    let byte_range = |s: &Syllable| -> (u32, u32) {
        let start = byte_offsets.get(s.start).copied().unwrap_or(u32::MAX);
        let end = byte_offsets.get(s.end).copied().unwrap_or(start);
        (start, end)
    };
    // The syllables whose byte ranges hold any cluster, by range
    // start. Syllables are consecutive, so for text in logical order
    // their ranges are sorted and disjoint already; text shaped in
    // reversed grapheme order (see `crate::shape`) runs its clusters
    // downward, which leaves most ranges empty.
    let mut ranges: Vec<(u32, u32, usize)> = syllables
        .iter()
        .enumerate()
        .map(|(k, s)| {
            let (start, end) = byte_range(s);
            (start, end, k)
        })
        .filter(|&(start, end, _)| start < end)
        .collect();
    ranges.sort_unstable();

    // Glyphs are tracked by their index before any reorder (their id
    // here): `pos[id]` is where the glyph is now and `at[i]` which
    // glyph is at index `i`. The owner is the syllable with the last
    // range starting at or before the glyph's cluster, if that range
    // reaches the cluster.
    let mut owner: Vec<usize> = glyphs
        .iter()
        .map(|g| {
            let Some(r) = ranges
                .partition_point(|&(s, _, _)| s <= g.cluster)
                .checked_sub(1)
            else {
                return NO_OWNER;
            };
            match ranges.get(r) {
                Some(&(_, end, k)) if g.cluster < end => k,
                _ => NO_OWNER,
            }
        })
        .collect();
    let mut pos: Vec<usize> = (0..glyphs.len()).collect();
    let mut at: Vec<usize> = pos.clone();

    // Bucket glyph ids by owner, ascending within each bucket.
    let mut bucket_start = alloc::vec![0usize; syllables.len() + 1];
    for &k in &owner {
        if k != NO_OWNER {
            bucket_start[k + 1] += 1;
        }
    }
    for k in 0..syllables.len() {
        bucket_start[k + 1] += bucket_start[k];
    }
    let mut fill = bucket_start.clone();
    let mut bucketed = alloc::vec![0usize; bucket_start[syllables.len()]];
    for (id, &k) in owner.iter().enumerate() {
        if k != NO_OWNER {
            bucketed[fill[k]] = id;
            fill[k] += 1;
        }
    }

    let mut members: Vec<usize> = Vec::new();
    for (k, syllable) in syllables.iter().enumerate() {
        // A reorder moves one glyph of its own syllable past others,
        // which keep their order, so a later syllable's glyphs stay in
        // ascending order.
        members.clear();
        members.extend(
            bucketed[bucket_start[k]..bucket_start[k + 1]]
                .iter()
                .filter(|&&id| owner[id] == k)
                .map(|&id| pos[id]),
        );
        merge_pre_base_matra_members(glyphs, &members, level);
        let original_glyph_count = syllable.end - syllable.start;
        let Some((from, to)) = final_reorder_members(
            glyphs,
            &members,
            original_glyph_count,
            config.reph_pos,
            config.reph_mode,
            level,
        ) else {
            continue;
        };
        at[from..=to].rotate_left(1);
        for (i, &id) in at.iter().enumerate().take(to + 1).skip(from) {
            pos[id] = i;
            if level.is_monotone() {
                // The merge gave the moved glyphs a cluster no later
                // syllable's range holds. Otherwise they kept theirs.
                owner[id] = k;
            }
        }
    }
}

/// The glyph indices whose clusters fall in `[byte_start, byte_end)`.
#[cfg(test)]
fn glyphs_in_range(glyphs: &[Glyph], byte_start: u32, byte_end: u32) -> Vec<usize> {
    glyphs
        .iter()
        .enumerate()
        .filter(|(_, g)| g.cluster >= byte_start && g.cluster < byte_end)
        .map(|(i, _)| i)
        .collect()
}

/// Test entry point: reorders the syllable whose glyph clusters fall
/// in `[byte_start, byte_end)`.
#[cfg(test)]
pub(super) fn final_reorder(
    glyphs: &mut [Glyph],
    byte_start: u32,
    byte_end: u32,
    original_glyph_count: usize,
    reph_pos: RephPosition,
    reph_mode: RephMode,
    level: ClusterLevel,
) {
    let members = glyphs_in_range(glyphs, byte_start, byte_end);
    final_reorder_members(
        glyphs,
        &members,
        original_glyph_count,
        reph_pos,
        reph_mode,
        level,
    );
}

/// Test entry point: the pre-base matra merge for the syllable whose
/// glyph clusters fall in `[byte_start, byte_end)`.
#[cfg(test)]
pub(super) fn merge_pre_base_matras(
    glyphs: &mut [Glyph],
    byte_start: u32,
    byte_end: u32,
    level: ClusterLevel,
) {
    let members = glyphs_in_range(glyphs, byte_start, byte_end);
    merge_pre_base_matra_members(glyphs, &members, level);
}
