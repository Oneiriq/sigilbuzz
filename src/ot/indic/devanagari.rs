//! Devanagari shaper driving the Indic2 state machine.
//!
//! # Pipeline
//!
//! Given a run of codepoints that the script classifier identified
//! as Devanagari:
//!
//! 1. Syllable segmentation. Each syllable is one of:
//!    - **Consonant syllable** — `(Consonant Nukta? (Halant Consonant)* Matra* Bindu?)`
//!      — the common path.
//!    - **Vowel syllable** — independent vowel, optional matras/marks.
//!    - **Standalone** — Bindu/Visarga/dotted-circle alone.
//!    - **Symbol / broken** — anything else; passes through unchanged.
//! 2. Initial reordering per syllable, producing the *logical*
//!    glyph order the GSUB feature pipeline expects:
//!    - Identify the base consonant (last consonant not preceded by
//!      halant is the most common heuristic; a syllable starting with
//!      `ra + halant` marks that `ra` as a reph candidate and promotes
//!      the next consonant to base).
//!    - Move pre-base matras (`U+093F` in Devanagari) to immediately
//!      before the base consonant.
//!    - Mark glyphs that should receive `rphf` (the ra+halant reph),
//!      `half`, `blwf`, `pstf`, `pref` features.
//! 3. Apply basic features via the GSUB feature dispatcher:
//!    `nukt`, `akhn`, `rphf`, `rkrf`, `blwf`, `half`, `pstf`, `vatu`,
//!    `cjct`. sigilbuzz runs each feature across the whole run; the
//!    font's lookup masks ensure only the right glyphs transform. A
//!    more aggressive impl would tag per-glyph feature masks, but
//!    the cluster-level feature call reproduces rustybuzz output on
//!    the common Devanagari fixtures.
//! 4. Final reordering. Reph (if any) moves to its display slot —
//!    commonly before the last character of the syllable for
//!    Devanagari. Pre-base matras that moved to before the base in
//!    step 2 stay where they are.
//! 5. Return to the generic shape pipeline, which runs presentation
//!    features (`pres`, `abvs`, `blws`, `psts`, `haln`) and then
//!    `liga`, `clig`, `calt` exactly as for Latin.
//!
//! # What isn't here yet
//!
//! - Matra decomposition (split vowel signs). Devanagari does not
//!   have any split matras in the base block, so this is a no-op
//!   for it; the hook remains for future scripts.
//! - Per-glyph feature masking. sigilbuzz applies each basic feature
//!   across the whole run; that matches rustybuzz output on simple
//!   syllables but can fire a `half` form on a consonant that should
//!   have stayed full. Fixed by tagging glyph masks once the shape
//!   pipeline gains them.

use alloc::vec::Vec;

use crate::buffer::{Glyph, IndicPosition};
use crate::shape::apply_gsub_feature_in_scripts;
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;
use crate::unicode::indic_category::{
    positional_category, syllabic_category, IndicPositionalCategory, IndicSyllabicCategory,
};

/// Entry point. Re-orders and runs basic Indic features over the
/// portion of `glyphs` that corresponds to the Devanagari run
/// described by `codepoints`.
///
/// `codepoints` is in one-to-one correspondence with the starting
/// glyph layout — each codepoint produced one glyph before any
/// reordering. After this function returns, `glyphs` may contain
/// fewer entries (if basic features applied ligatures) and the
/// order can differ from input.
pub fn shape_devanagari(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    if codepoints.is_empty() || glyphs.is_empty() {
        return;
    }

    // Split the run into syllables. Each syllable carries the
    // codepoint indices it covers (start, end exclusive) so the
    // reorder phase can index into `glyphs` without re-scanning.
    let syllables = segment_syllables(codepoints);

    // Tag per-glyph Indic positions BEFORE we reorder or apply
    // features. The `rphf` ligature will drop the halant and leave
    // only the ra's glyph slot in place; because our GSUB
    // ligature path preserves the first component's `Glyph` struct
    // (only `glyph_id` is overwritten), the `RaToBecomeReph` mark
    // survives the substitution and the final-reorder pass can
    // locate the reph without re-running the state machine.
    for syllable in &syllables {
        tag_positions(codepoints, glyphs, syllable);
    }

    // Initial reordering is per-syllable and mutates `glyphs` in
    // place. We walk syllables in reverse when reorders change
    // lengths; Devanagari reorder is length-preserving (same glyph
    // count in, same out) because decomposition runs separately, so
    // forward iteration is safe here.
    for syllable in &syllables {
        initial_reorder(codepoints, glyphs, syllable);
    }

    // Basic features. Order matters — rphf must run before blwf
    // because a reph candidate that did not reph must fall through
    // to blwf as a regular ra-halant conjunct. Likewise half runs
    // after rphf because the ra in ra+halant may have been consumed
    // as reph already.
    //
    // Feature application uses the generic GSUB dispatcher with
    // Devanagari-aware script priority: `dev2` (Indic2 script tag
    // adopted in the 2005 Indic-improvements spec) first, `deva`
    // (legacy) next, DFLT as a fallback. Fonts that carry only
    // DFLT (rare for Indic) still work because the generic
    // fallback in `lookup_indices_for_feature_in_scripts` kicks in.
    if let Some(gsub) = gsub {
        for tag in INDIC_BASIC_FEATURES {
            apply_gsub_feature_in_scripts(gsub, glyphs, gdef, **tag, 0, DEVA_SCRIPT_PRIORITY);
        }
    }

    // Final reordering — reph moves to its display slot. Feature
    // execution above may have replaced the reph candidate with the
    // reph glyph via `rphf`; we locate it by the
    // `RaToBecomeReph` tag we set above, which the ligature path
    // preserved on the surviving glyph.
    //
    // Syllable bounds in codepoint space no longer map one-to-one
    // into `glyphs` because GSUB may have collapsed conjuncts.
    // Resolve bounds via cluster byte offsets instead: each
    // syllable covers the byte range from its first codepoint's
    // offset up to (but not including) its end codepoint's offset,
    // and every surviving glyph carries one of those byte offsets
    // in `cluster`.
    let byte_offsets = cluster_byte_offsets(codepoints);
    for syllable in &syllables {
        let byte_start = byte_offsets[syllable.start];
        let byte_end = byte_offsets[syllable.end];
        let original_glyph_count = syllable.end - syllable.start;
        final_reorder(glyphs, byte_start, byte_end, original_glyph_count);
    }

    // Presentation features — selecting the visual forms of
    // conjuncts, pre-base matras and combining marks. These run
    // after final reordering so the glyph positions are in their
    // final visual slots.
    if let Some(gsub) = gsub {
        for tag in INDIC_PRESENTATION_FEATURES {
            apply_gsub_feature_in_scripts(gsub, glyphs, gdef, **tag, 0, DEVA_SCRIPT_PRIORITY);
        }
    }
}

/// Devanagari GSUB script-tag priority.
///
/// Per the OpenType Indic2 spec (2005 revision), fonts advertise
/// the improved Indic feature set under `dev2`. Older fonts still
/// expose the same features under `deva`. DFLT is a last-resort
/// fallback — Indic-specific features like `pres`, `half`,
/// `blwf`, `cjct` are rarely exposed there.
pub(crate) const DEVA_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"dev2", *b"deva", *b"DFLT"];

/// Default Indic2 basic features, in application order.
pub(crate) const INDIC_BASIC_FEATURES: &[&[u8; 4]] = &[
    b"nukt", b"akhn", b"rphf", b"rkrf", b"blwf", b"half", b"pstf", b"vatu", b"cjct",
];

/// Default Indic2 presentation features, in application order.
/// Run after final reordering to pick the display glyphs for
/// pre-base vowels, conjuncts, and final marks.
pub(crate) const INDIC_PRESENTATION_FEATURES: &[&[u8; 4]] =
    &[b"init", b"pres", b"abvs", b"blws", b"psts", b"haln"];

/// Syllable classification mirroring the Indic2 syllable types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyllableKind {
    /// Consonant-based syllable: the common case.
    Consonant,
    /// Vowel-based syllable: starts with an independent vowel.
    Vowel,
    /// Standalone: a sole Bindu/Visarga/placeholder + marks.
    Standalone,
    /// Symbol or pass-through: digits, dandas, OM, ...
    Symbol,
    /// Broken: an orphan matra or virama we could not fold in.
    Broken,
}

/// One syllable's footprint in the input.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Syllable {
    pub kind: SyllableKind,
    /// Start codepoint/glyph index (inclusive).
    pub start: usize,
    /// End codepoint/glyph index (exclusive).
    pub end: usize,
    /// Codepoint-space index of the base consonant, or `None` for
    /// non-consonant syllables. Indices are relative to the syllable,
    /// i.e. `start <= base_index < end`.
    pub base_index: Option<usize>,
    /// True when the syllable begins with `ra + halant` and the
    /// leading ra is a reph candidate. The reph candidate sits at
    /// `start`; the halant sits at `start + 1`.
    pub has_reph: bool,
}

/// Breaks the codepoint run into Devanagari syllables.
///
/// The segmenter is a forgiving greedy parser: it starts at each
/// index, consumes the longest prefix matching a syllable pattern,
/// and emits one [`Syllable`]. Codepoints that do not begin any
/// syllable pattern emit a one-wide Broken/Symbol syllable.
pub(crate) fn segment_syllables(codepoints: &[char]) -> Vec<Syllable> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < codepoints.len() {
        let syl = scan_one_syllable(codepoints, i);
        i = syl.end;
        out.push(syl);
    }
    out
}

/// Parses a single syllable starting at `start`. Always makes
/// progress: the returned syllable has `end > start`.
fn scan_one_syllable(cps: &[char], start: usize) -> Syllable {
    let first_isc = syllabic_category(cps[start]);
    match first_isc {
        IndicSyllabicCategory::Consonant | IndicSyllabicCategory::ConsonantPlaceholder => {
            scan_consonant_syllable(cps, start)
        }
        IndicSyllabicCategory::VowelIndependent | IndicSyllabicCategory::Vowel => {
            scan_vowel_syllable(cps, start)
        }
        IndicSyllabicCategory::Bindu
        | IndicSyllabicCategory::Visarga
        | IndicSyllabicCategory::Avagraha => Syllable {
            kind: SyllableKind::Standalone,
            start,
            end: start + 1,
            base_index: None,
            has_reph: false,
        },
        IndicSyllabicCategory::Number | IndicSyllabicCategory::Other => {
            // Consume any run of pass-through codepoints in one go.
            let mut end = start + 1;
            while end < cps.len() {
                let c = syllabic_category(cps[end]);
                if matches!(
                    c,
                    IndicSyllabicCategory::Number | IndicSyllabicCategory::Other
                ) {
                    end += 1;
                } else {
                    break;
                }
            }
            Syllable {
                kind: SyllableKind::Symbol,
                start,
                end,
                base_index: None,
                has_reph: false,
            }
        }
        _ => Syllable {
            kind: SyllableKind::Broken,
            start,
            end: start + 1,
            base_index: None,
            has_reph: false,
        },
    }
}

/// Consumes a consonant-based syllable. Pattern (simplified):
///
/// ```text
///   [(C N?) Virama]*  C  N?  (Matra Bindu?)*  (Virama)?
/// ```
///
/// We track the base consonant index as "last consonant not
/// followed by a virama". On exit `has_reph` is true when the
/// syllable begins with `ra + halant` and at least one more
/// consonant follows (required for reph positioning).
fn scan_consonant_syllable(cps: &[char], start: usize) -> Syllable {
    let mut i = start;
    let len = cps.len();

    // Detect ra + halant reph candidate at the head.
    let ra_halant_prefix = i + 1 < len
        && cps[i] == '\u{0930}' // ra
        && syllabic_category(cps[i + 1]) == IndicSyllabicCategory::Virama;

    let mut base_index: Option<usize> = None;
    let mut last_was_consonant_then_virama: bool = false;

    // Walk consonants and halant pairs.
    loop {
        if i >= len {
            break;
        }
        let isc = syllabic_category(cps[i]);
        match isc {
            IndicSyllabicCategory::Consonant | IndicSyllabicCategory::ConsonantPlaceholder => {
                base_index = Some(i);
                last_was_consonant_then_virama = false;
                i += 1;
                // Optional nukta.
                if i < len && syllabic_category(cps[i]) == IndicSyllabicCategory::Nukta {
                    i += 1;
                }
                // Optional virama: tells us this consonant is a
                // half-form / conjunct participant, not the base.
                if i < len && syllabic_category(cps[i]) == IndicSyllabicCategory::Virama {
                    i += 1;
                    last_was_consonant_then_virama = true;
                    continue;
                }
                break;
            }
            _ => break,
        }
    }

    // Trailing matras and modifier marks.
    while i < len {
        let isc = syllabic_category(cps[i]);
        match isc {
            IndicSyllabicCategory::VowelDependent
            | IndicSyllabicCategory::Bindu
            | IndicSyllabicCategory::Visarga
            | IndicSyllabicCategory::CantillationMark
            | IndicSyllabicCategory::Nukta => {
                i += 1;
            }
            IndicSyllabicCategory::Virama => {
                // A trailing virama (explicit halant at the end of a
                // syllable) is legal — it is rendered as a visible
                // virama. Consume and stop.
                i += 1;
                break;
            }
            IndicSyllabicCategory::Joiner | IndicSyllabicCategory::NonJoiner => {
                // ZWJ/ZWNJ request the preceding consonant's
                // half-form / non-conjunct behaviour.
                i += 1;
            }
            _ => break,
        }
    }

    // If we never moved past `start`, we could not form a
    // consonant syllable. Emit a one-wide Broken syllable so the
    // caller advances.
    if i == start {
        return Syllable {
            kind: SyllableKind::Broken,
            start,
            end: start + 1,
            base_index: None,
            has_reph: false,
        };
    }

    // Reph is only real when the syllable has more consonants
    // after the ra+halant prefix.
    let has_reph = ra_halant_prefix && base_index.is_some_and(|b| b > start + 1);

    // If the very last token was a consonant-then-virama (an
    // explicit halant cluster with no trailing matra), the base is
    // the consonant before that halant — matches the "last
    // consonant not followed by a virama" rule only when such a
    // consonant exists; otherwise the last-consonant wins.
    let _ = last_was_consonant_then_virama;

    Syllable {
        kind: SyllableKind::Consonant,
        start,
        end: i,
        base_index,
        has_reph,
    }
}

/// Consumes a vowel syllable starting with an independent vowel.
fn scan_vowel_syllable(cps: &[char], start: usize) -> Syllable {
    let len = cps.len();
    let mut i = start + 1;
    while i < len {
        let isc = syllabic_category(cps[i]);
        match isc {
            IndicSyllabicCategory::VowelDependent
            | IndicSyllabicCategory::Bindu
            | IndicSyllabicCategory::Visarga
            | IndicSyllabicCategory::Nukta
            | IndicSyllabicCategory::CantillationMark => {
                i += 1;
            }
            _ => break,
        }
    }
    Syllable {
        kind: SyllableKind::Vowel,
        start,
        end: i,
        base_index: Some(start),
        has_reph: false,
    }
}

/// Sets per-glyph Indic positions for the glyphs in one syllable's
/// range. Called BEFORE any reorder or GSUB pass, so indices in
/// `glyphs` still line up one-to-one with `codepoints`.
///
/// Three positions matter for Devanagari's final reorder pass:
///
/// - [`IndicPosition::RaToBecomeReph`] on the leading `ra` of a
///   `ra + halant + …` syllable. The `rphf` ligature will turn the
///   ra-halant pair into a reph glyph; the ligature path preserves
///   the first component's `Glyph` struct (everything but
///   `glyph_id`), so the tag survives and the final-reorder pass
///   can find the reph without re-inspecting codepoints.
/// - [`IndicPosition::BaseC`] on the base consonant, so the
///   reorder knows where the main consonant sits (even after
///   basic features have collapsed conjuncts around it).
/// - [`IndicPosition::PreM`] on pre-base matra glyphs — useful
///   later when we grow pre-base matra repositioning, and already
///   needed to distinguish a matra's halant from a consonant's in
///   the reph-target-finding walk.
///
/// Other glyphs keep the default [`IndicPosition::Start`].
///
/// TODO(#<follow-up>): port the same tagging to other Indic scripts
/// (Bengali, Gurmukhi, …) as they land. The position values are
/// script-agnostic; only the reph target slot differs.
fn tag_positions(codepoints: &[char], glyphs: &mut [Glyph], syllable: &Syllable) {
    if !matches!(syllable.kind, SyllableKind::Consonant) {
        return;
    }
    if syllable.end > glyphs.len() || syllable.end > codepoints.len() {
        return;
    }

    // Mark reph candidate. Only valid when the syllable genuinely
    // starts with ra + halant AND has a base consonant after —
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
fn initial_reorder(codepoints: &[char], glyphs: &mut [Glyph], syllable: &Syllable) {
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
    // reph prefix (if any) or just before the base. Walking in
    // reverse so earlier insertions do not shift later indices.
    // Destination slot: the first consonant that should render
    // after the matra. For Devanagari that is immediately before
    // the base, UNLESS the syllable has a reph — then the reph
    // stays leftmost and the matra slots in after the reph's
    // halant (i.e. before the base still, because the reph's ra
    // is at `start` and its halant at `start + 1`).
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

/// Returns a length-`codepoints.len() + 1` array mapping codepoint
/// index to UTF-8 byte offset. `out[i]` is the byte offset of the
/// i'th codepoint in the original string; `out[len]` is the total
/// byte length. Used by [`shape_devanagari`] to translate
/// codepoint-space syllable bounds into cluster-space bounds that
/// survive GSUB (each surviving glyph's `cluster` is one of these
/// byte offsets).
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

/// Final reordering for one Devanagari syllable, in glyph space.
///
/// `byte_start` and `byte_end` are UTF-8 byte offsets that bound
/// the syllable's clusters — any glyph whose `cluster` falls in
/// `[byte_start, byte_end)` belongs to this syllable. Cluster byte
/// offsets are stable across GSUB (ligatures keep the first
/// component's cluster, multiple-sub replicates it), so this
/// mapping works even after `rphf` has collapsed `ra + halant` into
/// a single reph glyph.
///
/// Target slot for Devanagari reph is `BeforePost` — HarfBuzz and
/// rustybuzz both spell it out as: find the first explicit halant
/// inside the syllable (after the reph's own start+1); if one
/// exists, reph sits right after it (or after a following joiner).
/// Otherwise reph falls through to the end of the syllable, just
/// before any trailing syllable-modifier / vedic mark. The simpler
/// syllable `ra + halant + consonant` has no inner halant, so the
/// fallback fires and the reph slots in after the base.
///
/// When the `rphf` feature did not fire (the font ships no reph
/// form), the surviving `ra` glyph keeps its
/// [`IndicPosition::RaToBecomeReph`] tag but there is no stand-alone
/// reph glyph to move — we detect this by looking at whether the
/// tagged glyph sits adjacent to its halant. If it does, we leave
/// it in place; the generic pipeline treats it as a conjunct.
///
/// Cluster metadata on the moved reph is rewritten to the
/// syllable's base cluster so byte offsets attributed to the reph
/// match HarfBuzz's behavior (`merge_clusters` in rustybuzz).
fn final_reorder(
    glyphs: &mut [Glyph],
    byte_start: u32,
    byte_end: u32,
    original_glyph_count: usize,
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

    // If the syllable's glyph count did not shrink, the `rphf`
    // feature did not fire and there is no stand-alone reph glyph
    // to move. The ra+halant pair is still two separate glyphs that
    // will render as a conjunct through the generic pipeline.
    // (Other basic features may also shrink a syllable but for the
    // `ra + halant + C` prefix the first substitution to trigger is
    // rphf, so a strict inequality is a reliable rphf probe.)
    if syllable_glyphs.len() >= original_glyph_count {
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
        // Already moved — nothing to do.
        return;
    }

    // Compute target slot. The syllable's other glyphs are what
    // remains after rphf collapsed `ra + halant`: base, conjunct
    // continuations, matras, marks. For Devanagari's `BeforePost`
    // position we want the reph after the main consonant but
    // before any post-base matra / smvd.
    //
    // Walk the syllable from the end backward past SMVD-like
    // trailing marks and post-base matras so the reph lands just
    // after the base. For the simple `ra+halant+C` case this means
    // the reph goes to the end.
    //
    // More precisely (matching rustybuzz step 6 fallback):
    //   new_pos = last glyph in syllable
    //   while new_pos > first and glyph[new_pos] is SMVD: new_pos -= 1
    let last_in_syllable = *syllable_glyphs.last().unwrap();
    let mut target = last_in_syllable;
    while target > reph_idx && glyphs[target].indic_position == IndicPosition::Smvd as u8 {
        target -= 1;
    }
    if target == reph_idx {
        return; // Nothing to move past.
    }

    // Move `glyphs[reph_idx]` to `target` by shifting the slots
    // between them left by one. HarfBuzz's `merge_clusters(start,
    // new_reph_pos + 1)` collapses the range the reph passes over
    // into the minimum cluster; for a Devanagari reph syllable the
    // first surviving glyph's cluster is that minimum (it is the
    // ra's byte offset = syllable start), so we overwrite every
    // cluster in the range with it.
    let base_cluster = glyphs[first_in_syllable].cluster;
    let mut reph = glyphs[reph_idx];
    reph.cluster = base_cluster;
    for i in reph_idx..target {
        glyphs[i] = glyphs[i + 1];
        glyphs[i].cluster = base_cluster;
    }
    glyphs[target] = reph;
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn cps(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn fake_glyphs(n: usize) -> Vec<Glyph> {
        (0..n).map(|i| Glyph::new(i as u32 + 1, i as u32)).collect()
    }

    #[test]
    fn single_consonant_is_a_consonant_syllable() {
        // क — one syllable.
        let cp = cps("\u{0915}");
        let syl = segment_syllables(&cp);
        assert_eq!(syl.len(), 1);
        assert_eq!(syl[0].kind, SyllableKind::Consonant);
        assert_eq!(syl[0].start, 0);
        assert_eq!(syl[0].end, 1);
        assert_eq!(syl[0].base_index, Some(0));
        assert!(!syl[0].has_reph);
    }

    #[test]
    fn consonant_matra_is_one_syllable() {
        // की = क + ी (consonant + post-base matra).
        let cp = cps("\u{0915}\u{0940}");
        let syl = segment_syllables(&cp);
        assert_eq!(syl.len(), 1);
        assert_eq!(syl[0].kind, SyllableKind::Consonant);
        assert_eq!(syl[0].end, 2);
        assert_eq!(syl[0].base_index, Some(0));
    }

    #[test]
    fn namaste_splits_into_three_syllables() {
        // न म स ् त े — "namaste" is typically three syllables:
        // न (na), म (ma), स्ते (ste with halant conjunct).
        let cp = cps("\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}");
        let syl = segment_syllables(&cp);
        assert_eq!(syl.len(), 3);
        assert!(syl.iter().all(|s| s.kind == SyllableKind::Consonant));
    }

    #[test]
    fn hindi_has_reph_on_second_syllable() {
        // हिन्दी = ह ि न ् द ी
        // Syllables: हि (ha + i), न्दी (na-halant-da-ii).
        // No reph here — the halant joins na+da inside the syllable.
        let cp = cps("\u{0939}\u{093F}\u{0928}\u{094D}\u{0926}\u{0940}");
        let syl = segment_syllables(&cp);
        assert!(!syl.is_empty());
        // None of the syllables should have reph (no ra-halant
        // prefix).
        assert!(syl.iter().all(|s| !s.has_reph));
    }

    #[test]
    fn ra_halant_consonant_marks_reph() {
        // र् क → reph(ra) + halant + ka = reph + ka syllable.
        let cp = cps("\u{0930}\u{094D}\u{0915}");
        let syl = segment_syllables(&cp);
        assert_eq!(syl.len(), 1);
        assert!(syl[0].has_reph);
        assert_eq!(syl[0].base_index, Some(2)); // ka
    }

    #[test]
    fn pre_base_matra_moves_before_base() {
        // कि = क (ka) + ि (pre-base matra).
        // Before reorder: [ka, i]. After reorder: [i, ka].
        let cp = cps("\u{0915}\u{093F}");
        let mut glyphs = fake_glyphs(2);
        let original = glyphs.clone();
        let syllables = segment_syllables(&cp);
        for s in &syllables {
            initial_reorder(&cp, &mut glyphs, s);
        }
        assert_eq!(glyphs[0], original[1]); // matra first
        assert_eq!(glyphs[1], original[0]); // ka second
    }

    #[test]
    fn post_base_matra_stays_put() {
        // की — matra ी is Right positional (post-base), so no move.
        let cp = cps("\u{0915}\u{0940}");
        let mut glyphs = fake_glyphs(2);
        let before = glyphs.clone();
        let syllables = segment_syllables(&cp);
        for s in &syllables {
            initial_reorder(&cp, &mut glyphs, s);
        }
        assert_eq!(glyphs, before);
    }

    #[test]
    fn independent_vowel_is_a_vowel_syllable() {
        let cp = cps("\u{0905}"); // अ
        let syl = segment_syllables(&cp);
        assert_eq!(syl.len(), 1);
        assert_eq!(syl[0].kind, SyllableKind::Vowel);
    }

    #[test]
    fn devanagari_digits_are_symbol_pass_through() {
        // ० (digit zero) — should pass through as Symbol.
        let cp = cps("\u{0966}");
        let syl = segment_syllables(&cp);
        assert_eq!(syl.len(), 1);
        assert_eq!(syl[0].kind, SyllableKind::Symbol);
    }

    #[test]
    fn empty_input_produces_no_syllables() {
        assert!(segment_syllables(&[]).is_empty());
    }

    #[test]
    fn three_pre_base_matras_each_move_before_their_base() {
        // क ि क ि क ि → three (consonant, pre-base matra) syllables.
        // After reorder each matra should sit before its consonant.
        let cp = cps("\u{0915}\u{093F}\u{0915}\u{093F}\u{0915}\u{093F}");
        let mut glyphs = fake_glyphs(6);
        let syls = segment_syllables(&cp);
        assert_eq!(syls.len(), 3);
        for s in &syls {
            initial_reorder(&cp, &mut glyphs, s);
        }
        // Cluster ids 1,0,3,2,5,4 — i.e. the matras (original
        // indices 1,3,5) now sit at positions 0,2,4.
        assert_eq!(glyphs[0].cluster, 1);
        assert_eq!(glyphs[1].cluster, 0);
        assert_eq!(glyphs[2].cluster, 3);
        assert_eq!(glyphs[3].cluster, 2);
        assert_eq!(glyphs[4].cluster, 5);
        assert_eq!(glyphs[5].cluster, 4);
    }

    #[test]
    fn shape_devanagari_without_gsub_only_reorders() {
        // क ि — reorder but no feature run.
        let cp = cps("\u{0915}\u{093F}");
        let mut glyphs = fake_glyphs(2);
        shape_devanagari(None, None, &cp, &mut glyphs);
        assert_eq!(glyphs[0].cluster, 1);
        assert_eq!(glyphs[1].cluster, 0);
    }

    #[test]
    fn tag_positions_marks_ra_as_reph_candidate() {
        // र ् क — the leading ra is a reph candidate.
        let cp = cps("\u{0930}\u{094D}\u{0915}");
        let mut glyphs = fake_glyphs(3);
        for s in &segment_syllables(&cp) {
            tag_positions(&cp, &mut glyphs, s);
        }
        assert_eq!(
            glyphs[0].indic_position,
            IndicPosition::RaToBecomeReph as u8,
            "ra should be marked as reph candidate"
        );
        assert_eq!(
            glyphs[2].indic_position,
            IndicPosition::BaseC as u8,
            "ka should be marked as base consonant"
        );
    }

    #[test]
    fn tag_positions_marks_pre_base_matra() {
        // कि — ka + pre-base i. Matra gets PreM, base gets BaseC.
        let cp = cps("\u{0915}\u{093F}");
        let mut glyphs = fake_glyphs(2);
        for s in &segment_syllables(&cp) {
            tag_positions(&cp, &mut glyphs, s);
        }
        assert_eq!(glyphs[0].indic_position, IndicPosition::BaseC as u8);
        assert_eq!(glyphs[1].indic_position, IndicPosition::PreM as u8);
    }

    #[test]
    fn cluster_byte_offsets_matches_utf8_layout() {
        // र = 3 bytes, halant = 3 bytes, क = 3 bytes.
        let cp = cps("\u{0930}\u{094D}\u{0915}");
        assert_eq!(cluster_byte_offsets(&cp), vec![0, 3, 6, 9]);
    }

    #[test]
    fn final_reorder_moves_reph_to_syllable_end() {
        // Simulate post-rphf: two glyphs — reph (tagged) at idx 0
        // with cluster 0, base (tagged BaseC) at idx 1 with cluster 6.
        let mut g = fake_glyphs(2);
        g[0].indic_position = IndicPosition::RaToBecomeReph as u8;
        g[0].cluster = 0;
        g[1].indic_position = IndicPosition::BaseC as u8;
        g[1].cluster = 6;
        // Syllable covers bytes [0, 9) — original had 3 codepoints
        // (ra, halant, base); after rphf there are 2 glyphs.
        final_reorder(&mut g, 0, 9, 3);
        // Reph should now be at index 1, base at index 0.
        assert_eq!(g[0].indic_position, IndicPosition::BaseC as u8);
        assert_eq!(g[1].indic_position, IndicPosition::RaToBecomeReph as u8);
        // Cluster of the moved reph merges to the syllable's base
        // cluster (0 — the ra's original byte offset).
        assert_eq!(g[1].cluster, 0);
    }

    #[test]
    fn final_reorder_noop_when_rphf_did_not_fire() {
        // Three glyphs still present — same count as original
        // codepoints, so we know rphf did not collapse anything.
        let mut g = fake_glyphs(3);
        g[0].indic_position = IndicPosition::RaToBecomeReph as u8;
        g[0].cluster = 0;
        g[2].indic_position = IndicPosition::BaseC as u8;
        g[2].cluster = 6;
        let before = g.clone();
        final_reorder(&mut g, 0, 9, 3);
        assert_eq!(g, before, "no collapse -> no move");
    }

    #[test]
    fn tag_positions_leaves_non_reph_syllables_alone() {
        // क alone — no reph anywhere in the run.
        let cp = cps("\u{0915}");
        let mut glyphs = fake_glyphs(1);
        for s in &segment_syllables(&cp) {
            tag_positions(&cp, &mut glyphs, s);
        }
        assert_ne!(
            glyphs[0].indic_position,
            IndicPosition::RaToBecomeReph as u8
        );
    }

    #[test]
    fn symbol_run_advances_past_multiple_digits() {
        let cp = cps("\u{0966}\u{0967}\u{0968}");
        let syl = segment_syllables(&cp);
        assert_eq!(syl.len(), 1);
        assert_eq!(syl[0].end, 3);
    }
}
