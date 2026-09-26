//! Per-syllable reordering: position tagging, initial reordering, the
//! `half` feature mask, final reph placement, and the cluster
//! bookkeeping around them.

use alloc::vec::Vec;

use super::{IndicConfig, RephMode, RephPosition, Syllable, SyllableKind};
use crate::buffer::{Glyph, IndicPosition};
use crate::shape::feature_would_substitute;
use crate::tables::gdef::Gdef;
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

    // Collect pre-base matra indices inside the syllable, excluding
    // the base and anything preceding it.
    let mut to_move: Vec<usize> = Vec::new();
    for (offset, &ch) in codepoints[base + 1..syllable.end].iter().enumerate() {
        if positional_category(ch) == IndicPositionalCategory::Left
            && syllabic_category(ch) == IndicSyllabicCategory::VowelDependent
        {
            to_move.push(base + 1 + offset);
        }
    }
    if to_move.is_empty() {
        return;
    }

    // Take each pre-base matra and splice it in just before the
    // reph prefix (if any) or just before the base. The reph
    // stays leftmost and the matra slots in after the reph's
    // halant, i.e. before the base still.
    let insertion_point = base;

    // Move in reverse so later indices remain valid while we drain.
    for &idx in to_move.iter().rev() {
        let glyph = glyphs[idx];
        // Shift glyphs[insertion_point..idx] right by one.
        for j in (insertion_point..idx).rev() {
            glyphs[j + 1] = glyphs[j];
        }
        glyphs[insertion_point] = glyph;
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
        let hit = [*b"blwf", *b"pstf", *b"abvf"].iter().any(|tag| {
            feature_would_substitute(
                gsub,
                gdef,
                *tag,
                config.script_priority,
                &[halant_glyph, c2_glyph],
            ) || feature_would_substitute(
                gsub,
                gdef,
                *tag,
                config.script_priority,
                &[c2_glyph, halant_glyph],
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
/// `byte_start` and `byte_end` are UTF-8 byte offsets that bound the
/// syllable's clusters: any glyph whose `cluster` falls in
/// `[byte_start, byte_end)` belongs to this syllable. Cluster byte
/// offsets are stable across GSUB (ligatures keep the first
/// component's cluster, multiple-sub replicates it), so this
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
/// Cluster metadata on the moved reph is rewritten to the
/// syllable's base cluster so byte offsets attributed to the reph
/// match HarfBuzz's behavior (`merge_clusters` in rustybuzz).
pub(super) fn final_reorder(
    glyphs: &mut [Glyph],
    byte_start: u32,
    byte_end: u32,
    original_glyph_count: usize,
    reph_pos: RephPosition,
    reph_mode: RephMode,
) {
    // Collect glyph indices that belong to this syllable.
    let syllable_glyphs: Vec<usize> = glyphs
        .iter()
        .enumerate()
        .filter(|(_, g)| g.cluster >= byte_start && g.cluster < byte_end)
        .map(|(i, _)| i)
        .collect();
    if syllable_glyphs.len() < 2 {
        return;
    }

    // For Implicit / Explicit modes the ra+halant pair collapses to
    // a single reph glyph via `rphf`; if the glyph count did NOT
    // shrink, the font has no reph form and there is nothing to
    // relocate. [`RephMode::LogRepha`] is different: the logrepha
    // is encoded as its own codepoint with its own glyph, so we
    // always run the reorder regardless of glyph-count shrinkage.
    if reph_mode != RephMode::LogRepha && syllable_glyphs.len() >= original_glyph_count {
        return;
    }

    // Find the reph within this syllable.
    let Some(&reph_idx) = syllable_glyphs
        .iter()
        .find(|&&i| glyphs[i].indic_position == IndicPosition::RaToBecomeReph as u8)
    else {
        return;
    };

    let first_in_syllable = *syllable_glyphs.first().unwrap();
    if reph_idx != first_in_syllable {
        // Already moved, nothing to do.
        return;
    }

    // Compute target slot per-script.
    let last_in_syllable = *syllable_glyphs.last().unwrap();
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

    if target == reph_idx {
        return; // Nothing to move past.
    }

    // Move `glyphs[reph_idx]` to `target` by shifting the slots
    // between them left by one. HarfBuzz's `merge_clusters(start,
    // new_reph_pos + 1)` collapses the range the reph passes over
    // into the minimum cluster; for a reph syllable the first
    // surviving glyph's cluster is that minimum, so we overwrite
    // every cluster in the range with it.
    let base_cluster = glyphs[first_in_syllable].cluster;
    let mut reph = glyphs[reph_idx];
    reph.cluster = base_cluster;
    for i in reph_idx..target {
        glyphs[i] = glyphs[i + 1];
        glyphs[i].cluster = base_cluster;
    }
    glyphs[target] = reph;
}
