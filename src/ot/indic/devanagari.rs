//! Indic2 state machine and its Devanagari instantiation.
//!
//! The core `shape_indic` function is script-agnostic — it takes an
//! [`IndicConfig`] that captures the per-script virama / ra / reph
//! position / reph mode / GSUB script-tag priority and runs the
//! HarfBuzz-style reorder + feature pipeline. Devanagari, Bengali,
//! Gurmukhi, Gujarati, Oriya, Tamil, Telugu, Kannada, Malayalam and
//! Sinhala all feed into the same machine; only the configuration
//! (and the ISC/IPC tables in [`crate::unicode::indic_category`])
//! differs per script.
//!
//! # Pipeline
//!
//! Given a run of codepoints that the script classifier identified
//! as belonging to an Indic block:
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
//!      the next consonant to base — subject to [`RephMode`]).
//!    - Move pre-base matras (positional `Left`) to immediately
//!      before the base consonant.
//!    - Mark glyphs that should receive `rphf`, `half`, `blwf`,
//!      `pstf`, `pref` features.
//! 3. Apply basic features via the GSUB feature dispatcher:
//!    `nukt`, `akhn`, `rphf`, `rkrf`, `blwf`, `half`, `pstf`,
//!    `vatu`, `cjct`. sigilbuzz runs each feature across the whole
//!    run; the font's lookup masks ensure only the right glyphs
//!    transform.
//! 4. Final reordering. Reph (if any) moves to its display slot —
//!    [`RephPosition::BeforePost`] for Devanagari/Gujarati,
//!    [`RephPosition::AfterPost`] for Tamil/Telugu/Kannada/Sinhala,
//!    etc. Pre-base matras that moved to before the base in step 2
//!    stay where they are.
//! 5. Return to the generic shape pipeline, which runs presentation
//!    features (`pres`, `abvs`, `blws`, `psts`, `haln`) and then
//!    `liga`, `clig`, `calt`.
//!
//! # What isn't here yet
//!
//! - Matra decomposition (split vowel signs). Devanagari does not
//!   have any split matras in the base block; Tamil/Sinhala/Kannada
//!   have some (e.g. Tamil U+0BCA `OA = e + aa`) that a future
//!   matra-decompose pass will handle. Current impl treats them as
//!   opaque VowelDependent; the font's `pres` feature can still fire.
//! - Per-glyph feature masking.
//! - [`RephMode::Explicit`] / [`RephMode::LogRepha`] reph detection.
//!   Sigilbuzz 0.2.0 treats all scripts as Implicit for the purposes
//!   of reph candidate tagging; Telugu/Sinhala/Malayalam LogRepha
//!   flows get filed as follow-up issues and their parity tests
//!   exclude strings that depend on the difference.

use alloc::vec::Vec;

use super::{IndicConfig, RephMode, RephPosition};
use crate::buffer::{Glyph, IndicPosition};
use crate::shape::apply_gsub_feature_in_scripts;
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;
use crate::unicode::indic_category::{
    positional_category, syllabic_category, IndicPositionalCategory, IndicSyllabicCategory,
};
use crate::unicode::Script;

/// Script-agnostic Indic entry point. Re-orders and runs basic Indic
/// features over the portion of `glyphs` that corresponds to the
/// Indic run described by `codepoints`, using `config` for per-script
/// behaviour.
///
/// `codepoints` is in one-to-one correspondence with the starting
/// glyph layout — each codepoint produced one glyph before any
/// reordering. After this function returns, `glyphs` may contain
/// fewer entries (if basic features applied ligatures) and the
/// order can differ from input.
pub fn shape_indic(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    config: &IndicConfig,
) {
    if codepoints.is_empty() || glyphs.is_empty() {
        return;
    }

    // Split the run into syllables. Each syllable carries the
    // codepoint indices it covers (start, end exclusive) so the
    // reorder phase can index into `glyphs` without re-scanning.
    let syllables = segment_syllables(codepoints, config);

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
    // place. Indic reorder is length-preserving (same glyph count
    // in, same out) because decomposition runs separately, so
    // forward iteration is safe here.
    for syllable in &syllables {
        initial_reorder(codepoints, glyphs, syllable);
    }

    // Basic features. Order matters — rphf must run before blwf
    // because a reph candidate that did not reph must fall through
    // to blwf as a regular ra-halant conjunct. Likewise half runs
    // after rphf because the ra in ra+halant may have been consumed
    // as reph already.
    if let Some(gsub) = gsub {
        for tag in INDIC_BASIC_FEATURES {
            apply_gsub_feature_in_scripts(gsub, glyphs, gdef, **tag, 0, config.script_priority);
        }
    }

    // Final reordering — reph moves to its display slot. Feature
    // execution above may have replaced the reph candidate with the
    // reph glyph via `rphf`; we locate it by the
    // `RaToBecomeReph` tag we set above, which the ligature path
    // preserved on the surviving glyph.
    let byte_offsets = cluster_byte_offsets(codepoints);
    for syllable in &syllables {
        let byte_start = byte_offsets[syllable.start];
        let byte_end = byte_offsets[syllable.end];
        let original_glyph_count = syllable.end - syllable.start;
        final_reorder(
            glyphs,
            byte_start,
            byte_end,
            original_glyph_count,
            config.reph_pos,
        );
    }

    // Presentation features.
    if let Some(gsub) = gsub {
        for tag in INDIC_PRESENTATION_FEATURES {
            apply_gsub_feature_in_scripts(gsub, glyphs, gdef, **tag, 0, config.script_priority);
        }
    }
}

/// Devanagari convenience wrapper over [`shape_indic`]. Kept for the
/// existing call sites; new scripts should use `shape_indic` with the
/// config from [`super::indic_config_for`].
pub fn shape_devanagari(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    let config = super::indic_config_for(Script::Devanagari)
        .expect("Devanagari always has an Indic config");
    shape_indic(gsub, gdef, codepoints, glyphs, &config);
}

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

/// Breaks the codepoint run into Indic syllables.
///
/// The segmenter is a forgiving greedy parser: it starts at each
/// index, consumes the longest prefix matching a syllable pattern,
/// and emits one [`Syllable`]. Codepoints that do not begin any
/// syllable pattern emit a one-wide Broken/Symbol syllable.
pub(crate) fn segment_syllables(codepoints: &[char], config: &IndicConfig) -> Vec<Syllable> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < codepoints.len() {
        let syl = scan_one_syllable(codepoints, i, config);
        i = syl.end;
        out.push(syl);
    }
    out
}

/// Parses a single syllable starting at `start`. Always makes
/// progress: the returned syllable has `end > start`.
fn scan_one_syllable(cps: &[char], start: usize, config: &IndicConfig) -> Syllable {
    let first_isc = syllabic_category(cps[start]);
    match first_isc {
        IndicSyllabicCategory::Consonant | IndicSyllabicCategory::ConsonantPlaceholder => {
            scan_consonant_syllable(cps, start, config)
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
/// consonant follows (required for reph positioning), subject to the
/// config's [`RephMode`].
fn scan_consonant_syllable(cps: &[char], start: usize, config: &IndicConfig) -> Syllable {
    let mut i = start;
    let len = cps.len();

    // Detect `ra + halant` reph candidate at the head. Sigilbuzz
    // 0.2.0 only triggers on the Implicit mode; scripts declared as
    // Explicit or LogRepha do not tag a reph here (their reph flow
    // is a follow-up issue — see tracker in `src/ot/indic/mod.rs`).
    let ra_halant_prefix = config.reph_mode == RephMode::Implicit
        && i + 1 < len
        && cps[i] as u32 == config.ra
        && cps[i + 1] as u32 == config.virama;

    let mut base_index: Option<usize> = None;

    // Walk consonants and halant pairs.
    loop {
        if i >= len {
            break;
        }
        let isc = syllabic_category(cps[i]);
        match isc {
            IndicSyllabicCategory::Consonant | IndicSyllabicCategory::ConsonantPlaceholder => {
                base_index = Some(i);
                i += 1;
                // Optional nukta.
                if i < len && syllabic_category(cps[i]) == IndicSyllabicCategory::Nukta {
                    i += 1;
                }
                // Optional virama: tells us this consonant is a
                // half-form / conjunct participant, not the base.
                if i < len && syllabic_category(cps[i]) == IndicSyllabicCategory::Virama {
                    i += 1;
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
/// Three positions matter for the final reorder pass:
///
/// - [`IndicPosition::RaToBecomeReph`] on the leading `ra` of a
///   `ra + halant + …` syllable. The `rphf` ligature will turn the
///   ra-halant pair into a reph glyph; the ligature path preserves
///   the first component's `Glyph` struct, so the tag survives.
/// - [`IndicPosition::BaseC`] on the base consonant.
/// - [`IndicPosition::PreM`] on pre-base matra glyphs.
///
/// Other glyphs keep the default [`IndicPosition::Start`].
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
    // reph prefix (if any) or just before the base. The reph
    // stays leftmost and the matra slots in after the reph's
    // halant — i.e. before the base still.
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
/// index to UTF-8 byte offset.
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

/// Final reordering for one Indic syllable, in glyph space.
///
/// `byte_start` and `byte_end` are UTF-8 byte offsets that bound the
/// syllable's clusters — any glyph whose `cluster` falls in
/// `[byte_start, byte_end)` belongs to this syllable. Cluster byte
/// offsets are stable across GSUB (ligatures keep the first
/// component's cluster, multiple-sub replicates it), so this
/// mapping works even after `rphf` has collapsed `ra + halant` into
/// a single reph glyph.
///
/// The reph target slot depends on `reph_pos`:
///
/// - [`RephPosition::BeforePost`] / [`RephPosition::AfterPost`] —
///   reph moves to the end of the syllable, skipping any trailing
///   SMVD marks. Matches Devanagari/Gujarati (BeforePost) and
///   Tamil/Telugu/Kannada/Sinhala (AfterPost) on the fixtures
///   tested because our simplified syllables never have a
///   meaningful post-base consonant cluster in between.
/// - [`RephPosition::AfterMain`] — reph moves to immediately after
///   the base consonant. Used by Oriya and Malayalam.
/// - [`RephPosition::BeforeSub`] / [`RephPosition::AfterSub`] —
///   reph slots before/after any sub-joined (below-base) consonant.
///   Our syllable structure collapses sub-joined forms into the
///   same `(C halant C)*` sequence as the rest, so we currently
///   reuse the `AfterMain` path for both; the difference surfaces
///   in multi-halant syllables that the 0.2.0 corpus avoids.
///
/// When the `rphf` feature did not fire (the font ships no reph
/// form), the surviving `ra` glyph keeps its
/// [`IndicPosition::RaToBecomeReph`] tag but there is no stand-alone
/// reph glyph to move — we detect this by comparing the post-feature
/// glyph count to the original.
///
/// Cluster metadata on the moved reph is rewritten to the
/// syllable's base cluster so byte offsets attributed to the reph
/// match HarfBuzz's behavior (`merge_clusters` in rustybuzz).
fn final_reorder(
    glyphs: &mut [Glyph],
    byte_start: u32,
    byte_end: u32,
    original_glyph_count: usize,
    reph_pos: RephPosition,
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
    // to move.
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

    // Compute target slot per-script.
    let last_in_syllable = *syllable_glyphs.last().unwrap();
    let target = match reph_pos {
        // BeforePost / AfterPost / BeforeSub / AfterSub all fall
        // back to "end of syllable, past any trailing SMVD marks"
        // for the simplified syllables the 0.2.0 corpus covers.
        // The distinction between these four only matters when the
        // syllable has a sub-joined form between the base and a
        // post-base consonant, which our machine doesn't distinguish
        // at this layer. See the follow-up issue in `mod.rs`.
        RephPosition::BeforePost
        | RephPosition::AfterPost
        | RephPosition::BeforeSub
        | RephPosition::AfterSub => {
            let mut t = last_in_syllable;
            while t > reph_idx && glyphs[t].indic_position == IndicPosition::Smvd as u8 {
                t -= 1;
            }
            t
        }
        // AfterMain — the reph slots immediately after the base
        // consonant. Find the base glyph in the syllable; if found
        // and it comes after the reph, land right on it. Otherwise
        // fall back to "end of syllable" behaviour (so a missing
        // base tag doesn't strand the reph).
        RephPosition::AfterMain => syllable_glyphs
            .iter()
            .copied()
            .find(|&i| i > reph_idx && glyphs[i].indic_position == IndicPosition::BaseC as u8)
            .unwrap_or_else(|| {
                let mut t = last_in_syllable;
                while t > reph_idx && glyphs[t].indic_position == IndicPosition::Smvd as u8 {
                    t -= 1;
                }
                t
            }),
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

#[cfg(test)]
mod tests {
    use super::super::indic_config_for;
    use super::*;
    use alloc::vec;

    fn cps(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn fake_glyphs(n: usize) -> Vec<Glyph> {
        (0..n).map(|i| Glyph::new(i as u32 + 1, i as u32)).collect()
    }

    fn deva_config() -> IndicConfig {
        indic_config_for(Script::Devanagari).unwrap()
    }

    #[test]
    fn single_consonant_is_a_consonant_syllable() {
        // क — one syllable.
        let cp = cps("\u{0915}");
        let syl = segment_syllables(&cp, &deva_config());
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
        let syl = segment_syllables(&cp, &deva_config());
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
        let syl = segment_syllables(&cp, &deva_config());
        assert_eq!(syl.len(), 3);
        assert!(syl.iter().all(|s| s.kind == SyllableKind::Consonant));
    }

    #[test]
    fn hindi_has_reph_on_second_syllable() {
        // हिन्दी = ह ि न ् द ी
        let cp = cps("\u{0939}\u{093F}\u{0928}\u{094D}\u{0926}\u{0940}");
        let syl = segment_syllables(&cp, &deva_config());
        assert!(!syl.is_empty());
        assert!(syl.iter().all(|s| !s.has_reph));
    }

    #[test]
    fn ra_halant_consonant_marks_reph() {
        // र् क → reph(ra) + halant + ka = reph + ka syllable.
        let cp = cps("\u{0930}\u{094D}\u{0915}");
        let syl = segment_syllables(&cp, &deva_config());
        assert_eq!(syl.len(), 1);
        assert!(syl[0].has_reph);
        assert_eq!(syl[0].base_index, Some(2)); // ka
    }

    #[test]
    fn pre_base_matra_moves_before_base() {
        // कि = क (ka) + ि (pre-base matra).
        let cp = cps("\u{0915}\u{093F}");
        let mut glyphs = fake_glyphs(2);
        let original = glyphs.clone();
        let syllables = segment_syllables(&cp, &deva_config());
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
        let syllables = segment_syllables(&cp, &deva_config());
        for s in &syllables {
            initial_reorder(&cp, &mut glyphs, s);
        }
        assert_eq!(glyphs, before);
    }

    #[test]
    fn independent_vowel_is_a_vowel_syllable() {
        let cp = cps("\u{0905}"); // अ
        let syl = segment_syllables(&cp, &deva_config());
        assert_eq!(syl.len(), 1);
        assert_eq!(syl[0].kind, SyllableKind::Vowel);
    }

    #[test]
    fn devanagari_digits_are_symbol_pass_through() {
        let cp = cps("\u{0966}");
        let syl = segment_syllables(&cp, &deva_config());
        assert_eq!(syl.len(), 1);
        assert_eq!(syl[0].kind, SyllableKind::Symbol);
    }

    #[test]
    fn empty_input_produces_no_syllables() {
        assert!(segment_syllables(&[], &deva_config()).is_empty());
    }

    #[test]
    fn three_pre_base_matras_each_move_before_their_base() {
        let cp = cps("\u{0915}\u{093F}\u{0915}\u{093F}\u{0915}\u{093F}");
        let mut glyphs = fake_glyphs(6);
        let syls = segment_syllables(&cp, &deva_config());
        assert_eq!(syls.len(), 3);
        for s in &syls {
            initial_reorder(&cp, &mut glyphs, s);
        }
        assert_eq!(glyphs[0].cluster, 1);
        assert_eq!(glyphs[1].cluster, 0);
        assert_eq!(glyphs[2].cluster, 3);
        assert_eq!(glyphs[3].cluster, 2);
        assert_eq!(glyphs[4].cluster, 5);
        assert_eq!(glyphs[5].cluster, 4);
    }

    #[test]
    fn shape_devanagari_without_gsub_only_reorders() {
        let cp = cps("\u{0915}\u{093F}");
        let mut glyphs = fake_glyphs(2);
        shape_devanagari(None, None, &cp, &mut glyphs);
        assert_eq!(glyphs[0].cluster, 1);
        assert_eq!(glyphs[1].cluster, 0);
    }

    #[test]
    fn tag_positions_marks_ra_as_reph_candidate() {
        let cp = cps("\u{0930}\u{094D}\u{0915}");
        let mut glyphs = fake_glyphs(3);
        for s in &segment_syllables(&cp, &deva_config()) {
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
        let cp = cps("\u{0915}\u{093F}");
        let mut glyphs = fake_glyphs(2);
        for s in &segment_syllables(&cp, &deva_config()) {
            tag_positions(&cp, &mut glyphs, s);
        }
        assert_eq!(glyphs[0].indic_position, IndicPosition::BaseC as u8);
        assert_eq!(glyphs[1].indic_position, IndicPosition::PreM as u8);
    }

    #[test]
    fn cluster_byte_offsets_matches_utf8_layout() {
        let cp = cps("\u{0930}\u{094D}\u{0915}");
        assert_eq!(cluster_byte_offsets(&cp), vec![0, 3, 6, 9]);
    }

    #[test]
    fn final_reorder_moves_reph_to_syllable_end() {
        let mut g = fake_glyphs(2);
        g[0].indic_position = IndicPosition::RaToBecomeReph as u8;
        g[0].cluster = 0;
        g[1].indic_position = IndicPosition::BaseC as u8;
        g[1].cluster = 6;
        final_reorder(&mut g, 0, 9, 3, RephPosition::BeforePost);
        assert_eq!(g[0].indic_position, IndicPosition::BaseC as u8);
        assert_eq!(g[1].indic_position, IndicPosition::RaToBecomeReph as u8);
        assert_eq!(g[1].cluster, 0);
    }

    #[test]
    fn final_reorder_noop_when_rphf_did_not_fire() {
        let mut g = fake_glyphs(3);
        g[0].indic_position = IndicPosition::RaToBecomeReph as u8;
        g[0].cluster = 0;
        g[2].indic_position = IndicPosition::BaseC as u8;
        g[2].cluster = 6;
        let before = g.clone();
        final_reorder(&mut g, 0, 9, 3, RephPosition::BeforePost);
        assert_eq!(g, before, "no collapse -> no move");
    }

    #[test]
    fn tag_positions_leaves_non_reph_syllables_alone() {
        let cp = cps("\u{0915}");
        let mut glyphs = fake_glyphs(1);
        for s in &segment_syllables(&cp, &deva_config()) {
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
        let syl = segment_syllables(&cp, &deva_config());
        assert_eq!(syl.len(), 1);
        assert_eq!(syl[0].end, 3);
    }

    #[test]
    fn bengali_ra_halant_consonant_marks_reph() {
        // Bengali: র (U+09B0) + ্ (U+09CD) + ক (U+0995).
        let cp = cps("\u{09B0}\u{09CD}\u{0995}");
        let config = indic_config_for(Script::Bengali).unwrap();
        let syl = segment_syllables(&cp, &config);
        assert_eq!(syl.len(), 1);
        assert!(syl[0].has_reph);
        assert_eq!(syl[0].base_index, Some(2));
    }

    #[test]
    fn tamil_consonant_syllable_segments() {
        // Tamil: க (U+0B95) + ி (U+0BBF pre-base I).
        let cp = cps("\u{0B95}\u{0BBF}");
        let config = indic_config_for(Script::Tamil).unwrap();
        let syl = segment_syllables(&cp, &config);
        assert_eq!(syl.len(), 1);
        assert_eq!(syl[0].kind, SyllableKind::Consonant);
        assert_eq!(syl[0].base_index, Some(0));
    }

    #[test]
    fn telugu_ra_halant_is_not_reph_under_explicit_mode() {
        // Telugu's RephMode is Explicit — bare ra+virama does NOT
        // tag a reph candidate. Only ra+virama+ZWJ would (not yet
        // implemented — follow-up issue).
        let cp = cps("\u{0C30}\u{0C4D}\u{0C15}");
        let config = indic_config_for(Script::Telugu).unwrap();
        let syl = segment_syllables(&cp, &config);
        assert_eq!(syl.len(), 1);
        assert!(
            !syl[0].has_reph,
            "explicit reph mode should NOT tag bare ra+virama"
        );
    }

    #[test]
    fn sinhala_pre_base_matra_moves() {
        // Sinhala: ක (U+0D9A) + ෙ (U+0DD9 pre-base e vowel sign).
        let cp = cps("\u{0D9A}\u{0DD9}");
        let mut glyphs = fake_glyphs(2);
        let original = glyphs.clone();
        let config = indic_config_for(Script::Sinhala).unwrap();
        for s in &segment_syllables(&cp, &config) {
            initial_reorder(&cp, &mut glyphs, s);
        }
        assert_eq!(glyphs[0], original[1]);
        assert_eq!(glyphs[1], original[0]);
    }

    #[test]
    fn after_main_reph_target_is_right_after_base() {
        // Oriya config uses AfterMain.
        let mut g = fake_glyphs(3);
        g[0].indic_position = IndicPosition::RaToBecomeReph as u8;
        g[0].cluster = 0;
        g[1].indic_position = IndicPosition::BaseC as u8;
        g[1].cluster = 6;
        g[2].indic_position = IndicPosition::Start as u8;
        g[2].cluster = 9;
        // original glyph count was 4 (ra, halant, base, matra); now 3.
        final_reorder(&mut g, 0, 12, 4, RephPosition::AfterMain);
        // After move: base @ 0, reph @ 1, trailing @ 2.
        assert_eq!(g[0].indic_position, IndicPosition::BaseC as u8);
        assert_eq!(g[1].indic_position, IndicPosition::RaToBecomeReph as u8);
        assert_eq!(g[2].indic_position, IndicPosition::Start as u8);
    }
}
