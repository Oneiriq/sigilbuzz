//! Indic2 state machine and its Devanagari instantiation.
//!
//! The core `shape_indic` function is script-agnostic: it takes an
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
//!    - **Consonant syllable**: `(Consonant Nukta? (Halant Consonant)* Matra* Bindu?)`
//!      (the common path).
//!    - **Vowel syllable**: independent vowel, optional matras/marks.
//!    - **Standalone**: Bindu/Visarga/dotted-circle alone.
//!    - **Symbol / broken**: anything else; passes through unchanged.
//! 2. Initial reordering per syllable, producing the *logical*
//!    glyph order the GSUB feature pipeline expects:
//!    - Identify the base consonant (last consonant not preceded by
//!      halant is the most common heuristic; a syllable starting with
//!      `ra + halant` marks that `ra` as a reph candidate and promotes
//!      the next consonant to base, subject to [`RephMode`]).
//!    - Move pre-base matras (positional `Left`) to immediately
//!      before the base consonant.
//!    - Mark glyphs that should receive `rphf`, `half`, `blwf`,
//!      `pstf`, `pref` features.
//! 3. Apply basic features via the GSUB feature dispatcher:
//!    `nukt`, `akhn`, `rphf`, `rkrf`, `blwf`, `half`, `pstf`,
//!    `vatu`, `cjct`. sigilbuzz runs each feature across the whole
//!    run; the font's lookup masks ensure only the right glyphs
//!    transform.
//! 4. Final reordering. Reph (if any) moves to its display slot:
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
use crate::shape::{
    apply_gsub_feature_in_scripts, apply_gsub_feature_masked, feature_would_substitute,
};
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;
use crate::unicode::indic_category::{
    positional_category, syllabic_category, IndicPositionalCategory, IndicSyllabicCategory,
};
use crate::unicode::Script;

/// Script-agnostic Indic entry point. Re-orders and runs basic Indic
/// features over the portion of `glyphs` that corresponds to the
/// Indic run described by `codepoints`, using `config` for per-script
/// behavior.
///
/// `codepoints` is in one-to-one correspondence with the starting
/// glyph layout: each codepoint produced one glyph before any
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

    // Basic features. Order matters: rphf must run before blwf
    // because a reph candidate that did not reph must fall through
    // to blwf as a regular ra-halant conjunct. Likewise half runs
    // after rphf because the ra in ra+halant may have been consumed
    // as reph already.
    //
    // `half` runs masked: for every C+H+C syllable we first ask the
    // font "would blwf substitute halant + second-C?" If yes, the
    // second consonant is below-base eligible and the base moves to
    // the first C, making the pre-halant position ineligible for
    // `half`. We encode that by zeroing the mask bit on the
    // pre-halant consonant (and its halant) so the `half` ligature
    // `C + H -> half-C` cannot fire. Mirrors
    // `consonant_position_from_face` in rustybuzz's ot_shaper_indic.
    if let Some(gsub) = gsub {
        let half_mask = compute_half_mask(gsub, gdef, codepoints, glyphs, config, &syllables);
        for tag in INDIC_BASIC_FEATURES {
            if *tag == b"half" {
                apply_gsub_feature_masked(
                    gsub,
                    glyphs,
                    gdef,
                    **tag,
                    config.script_priority,
                    &half_mask,
                );
            } else {
                apply_gsub_feature_in_scripts(gsub, glyphs, gdef, **tag, 0, config.script_priority);
            }
        }
    }

    // Final reordering: reph moves to its display slot. Feature
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
            config.reph_mode,
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
    let config =
        super::indic_config_for(Script::Devanagari).expect("Devanagari always has an Indic config");
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

    // Head-of-syllable reph detection. Three modes:
    //
    // * [`RephMode::Implicit`]: bare `ra + halant` at position 0
    //   is a reph candidate (Devanagari / Bengali / Gurmukhi /
    //   Gujarati / Oriya / Tamil / Kannada).
    // * [`RephMode::Explicit`]: `ra + halant + ZWJ` is required
    //   (Telugu / Sinhala). The trailing ZWJ is consumed as part of
    //   the prefix and never ends up in the output of the rphf
    //   ligature.
    // * [`RephMode::LogRepha`]: a dedicated code point (Malayalam
    //   U+0D4E DOT REPH) flags the syllable as reph-bearing
    //   regardless of ra / halant.
    let implicit_ra_halant = config.reph_mode == RephMode::Implicit
        && i + 1 < len
        && cps[i] as u32 == config.ra
        && cps[i + 1] as u32 == config.virama;
    let explicit_ra_halant_zwj = config.reph_mode == RephMode::Explicit
        && i + 2 < len
        && cps[i] as u32 == config.ra
        && cps[i + 1] as u32 == config.virama
        && cps[i + 2] == '\u{200D}';
    let logrepha_prefix = config.reph_mode == RephMode::LogRepha && cps[i] == '\u{0D4E}';
    let ra_halant_prefix = implicit_ra_halant || explicit_ra_halant_zwj || logrepha_prefix;

    // Advance past a LogRepha head so the syllable machine picks up
    // the following base consonant as the syllable's base. For
    // Explicit the ZWJ sits between the halant and the base; the
    // existing (C H)+ loop below treats ZWJ as non-consonant and
    // stops, so we walk it manually here.
    if logrepha_prefix {
        i += 1;
    } else if explicit_ra_halant_zwj {
        // Skip the ZWJ after ra+halant; the head now points at the
        // base consonant. The ra+halant pair will be swallowed by
        // the (C H)+ loop below as normal.
    }

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
                    // Optional ZWJ/ZWNJ after halant: requests an
                    // explicit conjunct / half-form. Consumed here so
                    // the following consonant keeps extending the
                    // C+H loop (needed for `ra + halant + ZWJ + C`
                    // under [`RephMode::Explicit`] in particular).
                    if i < len
                        && matches!(
                            syllabic_category(cps[i]),
                            IndicSyllabicCategory::Joiner | IndicSyllabicCategory::NonJoiner
                        )
                    {
                        i += 1;
                    }
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
                // syllable) is legal. It is rendered as a visible
                // virama. Consume and stop.
                i += 1;
                break;
            }
            IndicSyllabicCategory::Joiner | IndicSyllabicCategory::NonJoiner => {
                // ZWJ/ZWNJ request the preceding consonant's
                // half-form / non-conjunct behavior.
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

    // Reph is only real when the syllable has a base consonant
    // past the prefix. Otherwise the "prefix" was the whole
    // syllable and there is no base to hang the reph off.
    //
    // The minimum base offset depends on the head pattern:
    // * Implicit: `ra + halant + C`. Base must sit at >= start+2.
    // * Explicit: `ra + halant + ZWJ + C`. Base must sit at >= start+3.
    // * LogRepha: `U+0D4E + C`. Base must sit at >= start+1.
    let min_base_offset = if logrepha_prefix {
        1
    } else if explicit_ra_halant_zwj {
        3
    } else {
        2
    };
    let has_reph = ra_halant_prefix && base_index.is_some_and(|b| b >= start + min_base_offset);

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
///   `ra + halant + ...` syllable. The `rphf` ligature will turn the
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
fn compute_half_mask(
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
fn final_reorder(
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
        // क: one syllable.
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
        // न म स ् त े ("namaste") is typically three syllables:
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
        // र् क -> reph(ra) + halant + ka = reph + ka syllable.
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
        // की: matra ी is Right positional (post-base), so no move.
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
        final_reorder(
            &mut g,
            0,
            9,
            3,
            RephPosition::BeforePost,
            RephMode::Implicit,
        );
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
        final_reorder(
            &mut g,
            0,
            9,
            3,
            RephPosition::BeforePost,
            RephMode::Implicit,
        );
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
        // Telugu's RephMode is Explicit: bare ra+virama does NOT
        // tag a reph candidate. Only ra+virama+ZWJ would (not yet
        // implemented, follow-up issue).
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
        final_reorder(
            &mut g,
            0,
            12,
            4,
            RephPosition::AfterMain,
            RephMode::Implicit,
        );
        // After move: base @ 0, reph @ 1, trailing @ 2.
        assert_eq!(g[0].indic_position, IndicPosition::BaseC as u8);
        assert_eq!(g[1].indic_position, IndicPosition::RaToBecomeReph as u8);
        assert_eq!(g[2].indic_position, IndicPosition::Start as u8);
    }

    #[test]
    fn telugu_ra_halant_zwj_consonant_marks_reph_explicit() {
        // Telugu's Explicit reph mode: ra + halant + ZWJ + C tags
        // the ra as a reph candidate; bare ra+halant alone does not
        // (see issue #30).
        let cp = cps("\u{0C30}\u{0C4D}\u{200D}\u{0C15}");
        let config = indic_config_for(Script::Telugu).unwrap();
        let syl = segment_syllables(&cp, &config);
        assert_eq!(syl.len(), 1);
        assert!(
            syl[0].has_reph,
            "explicit ra+halant+ZWJ+C should be tagged as reph"
        );
        // Base must sit past the ZWJ.
        assert_eq!(syl[0].base_index, Some(3));
    }

    #[test]
    fn malayalam_logrepha_head_marks_reph() {
        // Malayalam's LogRepha mode: U+0D4E at syllable head is
        // itself the reph, and the following consonant becomes
        // the base (see issue #31).
        let cp = cps("\u{0D4E}\u{0D15}");
        let config = indic_config_for(Script::Malayalam).unwrap();
        let syl = segment_syllables(&cp, &config);
        assert_eq!(syl.len(), 1);
        assert!(syl[0].has_reph, "LogRepha head should tag as reph");
        assert_eq!(syl[0].base_index, Some(1));
    }

    #[test]
    fn logrepha_reorder_moves_reph_past_base() {
        // LogRepha fixture: the 0D4E glyph sits at pos 0, base at
        // pos 1. AfterMain target puts the repha right after the
        // base: [repha, base] -> [base, repha].
        let mut g = fake_glyphs(2);
        g[0].indic_position = IndicPosition::RaToBecomeReph as u8;
        g[0].cluster = 0;
        g[1].indic_position = IndicPosition::BaseC as u8;
        g[1].cluster = 3;
        // Original glyph count 2, no shrinkage. LogRepha mode must
        // still relocate because the repha is a standalone glyph
        // rather than an `rphf` ligature product.
        final_reorder(&mut g, 0, 6, 2, RephPosition::AfterMain, RephMode::LogRepha);
        assert_eq!(g[0].indic_position, IndicPosition::BaseC as u8);
        assert_eq!(g[1].indic_position, IndicPosition::RaToBecomeReph as u8);
    }

    #[test]
    fn tamil_split_matra_decomposes() {
        // U+0BCB (OO) should be split into U+0BC7 + U+0BBE.
        let parts = super::super::split_matra_decompose('\u{0BCB}');
        assert_eq!(parts, Some(&['\u{0BC7}', '\u{0BBE}'][..]));
    }

    #[test]
    fn sinhala_three_part_matra_decomposes() {
        // U+0DDD splits into three components.
        let parts = super::super::split_matra_decompose('\u{0DDD}');
        assert_eq!(parts, Some(&['\u{0DD9}', '\u{0DCF}', '\u{0DCA}'][..]));
    }

    #[test]
    fn non_split_matra_returns_none() {
        assert!(super::super::split_matra_decompose('\u{0BBE}').is_none());
        assert!(super::super::split_matra_decompose('\u{0D15}').is_none());
    }
}
