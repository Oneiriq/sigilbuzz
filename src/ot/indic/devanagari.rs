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
//! # Known limitations
//!
//! - Split matras (e.g. Tamil U+0BCA `O = e + aa`) are decomposed
//!   before cmap by [`super::split_matra_decompose`], so this module
//!   only ever sees their components.
//! - Only `half` runs with a per-glyph mask. The other basic features
//!   run across the whole run and rely on the font's lookups to touch
//!   only the right glyphs.

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
    final_reorder_all(glyphs, &syllables, &byte_offsets, config);

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
    // `indic_config_for` has a Devanagari entry, so the early return
    // never fires.
    let Some(config) = super::indic_config_for(Script::Devanagari) else {
        return;
    };
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
    // the following base consonant as the syllable's base. An
    // Explicit `ra + halant + ZWJ` head needs no special step: the
    // (C H)+ loop below consumes the ZWJ that follows a halant.
    if logrepha_prefix {
        i += 1;
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
    let Some(after_base) = codepoints.get(base + 1..syllable.end) else {
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

    // Each pre-base matra, taken from the last one back, is rotated
    // to the base position: the glyph at the matra index moves to
    // `base` and the glyphs in between shift right by one. The
    // rotations are relative to `base`, largest first.
    let rotations: Vec<usize> = to_move.iter().rev().map(|&idx| idx - base).collect();
    rotate_prefixes_right(&mut glyphs[base..=last_move], &rotations);
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
fn rotate_prefixes_right<T: Copy>(items: &mut [T], rotations: &[usize]) {
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
/// Cluster metadata on the moved reph is rewritten to the
/// syllable's base cluster so byte offsets attributed to the reph
/// match HarfBuzz's behavior (`merge_clusters` in rustybuzz).
///
/// Returns the inclusive index range whose glyphs were moved and
/// given the syllable's cluster, or `None` when nothing moved.
fn final_reorder_members(
    glyphs: &mut [Glyph],
    syllable_glyphs: &[usize],
    original_glyph_count: usize,
    reph_pos: RephPosition,
    reph_mode: RephMode,
) -> Option<(usize, usize)> {
    let (&first_in_syllable, &last_in_syllable) =
        (syllable_glyphs.first()?, syllable_glyphs.last()?);
    if syllable_glyphs.len() < 2 {
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

    if target == reph_idx {
        return None; // Nothing to move past.
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
    Some((reph_idx, target))
}

/// Runs the final reorder for every syllable, in order, in time
/// linear in the glyph count.
///
/// A glyph belongs to the syllable whose byte range holds its
/// cluster. Rather than rescanning every glyph once per syllable, the
/// glyph indices are bucketed by syllable up front. A reorder rewrites
/// the cluster of every glyph between the reph and its target to the
/// syllable's own cluster, so those glyphs can no longer belong to a
/// later syllable. `owner` records that, and each bucket is filtered by
/// it before use. The result is the same as filtering all glyphs by
/// byte range just before each syllable is reordered.
fn final_reorder_all(
    glyphs: &mut [Glyph],
    syllables: &[Syllable],
    byte_offsets: &[u32],
    config: &IndicConfig,
) {
    const NO_OWNER: usize = usize::MAX;
    let byte_range = |s: &Syllable| -> (u32, u32) {
        let start = byte_offsets.get(s.start).copied().unwrap_or(u32::MAX);
        let end = byte_offsets.get(s.end).copied().unwrap_or(start);
        (start, end)
    };
    let starts: Vec<u32> = syllables.iter().map(|s| byte_range(s).0).collect();

    // Syllables are consecutive, so their byte ranges are sorted and
    // disjoint. The owner is the last syllable starting at or before
    // the cluster, if its range reaches the cluster.
    let mut owner: Vec<usize> = glyphs
        .iter()
        .map(|g| {
            let Some(k) = starts.partition_point(|&s| s <= g.cluster).checked_sub(1) else {
                return NO_OWNER;
            };
            match syllables.get(k) {
                Some(s) if g.cluster < byte_range(s).1 => k,
                _ => NO_OWNER,
            }
        })
        .collect();

    // Bucket glyph indices by owner, ascending within each bucket.
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
    for (i, &k) in owner.iter().enumerate() {
        if k != NO_OWNER {
            bucketed[fill[k]] = i;
            fill[k] += 1;
        }
    }

    let mut members: Vec<usize> = Vec::new();
    for (k, syllable) in syllables.iter().enumerate() {
        members.clear();
        members.extend(
            bucketed[bucket_start[k]..bucket_start[k + 1]]
                .iter()
                .copied()
                .filter(|&i| owner[i] == k),
        );
        let original_glyph_count = syllable.end - syllable.start;
        if let Some((from, to)) = final_reorder_members(
            glyphs,
            &members,
            original_glyph_count,
            config.reph_pos,
            config.reph_mode,
        ) {
            for slot in &mut owner[from..=to] {
                *slot = k;
            }
        }
    }
}

/// Test entry point: reorders the syllable whose glyph clusters fall
/// in `[byte_start, byte_end)`.
#[cfg(test)]
fn final_reorder(
    glyphs: &mut [Glyph],
    byte_start: u32,
    byte_end: u32,
    original_glyph_count: usize,
    reph_pos: RephPosition,
    reph_mode: RephMode,
) {
    let syllable_glyphs: Vec<usize> = glyphs
        .iter()
        .enumerate()
        .filter(|(_, g)| g.cluster >= byte_start && g.cluster < byte_end)
        .map(|(i, _)| i)
        .collect();
    final_reorder_members(
        glyphs,
        &syllable_glyphs,
        original_glyph_count,
        reph_pos,
        reph_mode,
    );
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
        // tag a reph candidate. Only ra+virama+ZWJ does.
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

    /// Small deterministic generator for the differential tests.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, bound: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as usize) % bound.max(1)
        }
    }

    /// The one-rotation-at-a-time loop that `rotate_prefixes_right`
    /// replaces, kept as the reference.
    fn rotate_prefixes_one_by_one(items: &mut [u32], rotations: &[usize]) {
        for &r in rotations {
            let item = items[r];
            for j in (0..r).rev() {
                items[j + 1] = items[j];
            }
            items[0] = item;
        }
    }

    #[test]
    fn rotate_prefixes_right_matches_one_by_one_rotation() {
        let mut rng = Lcg(7);
        for _ in 0..3000 {
            let len = 1 + rng.below(24);
            let density = 1 + rng.below(4);
            let mut rotations: Vec<usize> = (1..len).filter(|_| rng.below(density) == 0).collect();
            rotations.reverse();
            let mut expected: Vec<u32> = (0..len as u32).collect();
            rotate_prefixes_one_by_one(&mut expected, &rotations);
            let mut got: Vec<u32> = (0..len as u32).collect();
            rotate_prefixes_right(&mut got, &rotations);
            assert_eq!(got, expected, "len {len} rotations {rotations:?}");
        }
    }

    #[test]
    fn rotate_prefixes_right_ignores_invalid_rotations() {
        let mut items = [1u32, 2, 3];
        rotate_prefixes_right(&mut items, &[3]);
        rotate_prefixes_right(&mut items, &[1, 2]);
        assert_eq!(items, [1, 2, 3]);
    }

    #[test]
    fn final_reorder_all_matches_per_syllable_scan() {
        let positions = [
            IndicPosition::Start,
            IndicPosition::RaToBecomeReph,
            IndicPosition::BaseC,
            IndicPosition::Smvd,
        ];
        let reph_positions = [
            RephPosition::AfterMain,
            RephPosition::BeforeSub,
            RephPosition::AfterSub,
            RephPosition::BeforePost,
            RephPosition::AfterPost,
        ];
        let reph_modes = [RephMode::Implicit, RephMode::Explicit, RephMode::LogRepha];
        let mut rng = Lcg(11);
        for _ in 0..3000 {
            // Consecutive syllables over `n` three-byte codepoints.
            let n = 1 + rng.below(12);
            let mut syllables = Vec::new();
            let mut start = 0;
            while start < n {
                let end = (start + 1 + rng.below(4)).min(n);
                syllables.push(Syllable {
                    kind: SyllableKind::Consonant,
                    start,
                    end,
                    base_index: Some(start),
                    has_reph: false,
                });
                start = end;
            }
            let byte_offsets: Vec<u32> = (0..=n as u32).map(|i| i * 3).collect();
            // Glyphs with arbitrary clusters, including interleaved
            // syllables and clusters past the end of the run.
            let glyph_count = rng.below(16);
            let glyphs: Vec<Glyph> = (0..glyph_count)
                .map(|i| {
                    let mut g = Glyph::new(i as u32, rng.below(3 * n + 4) as u32);
                    g.indic_position = positions[rng.below(positions.len())] as u8;
                    g
                })
                .collect();
            let mut config = deva_config();
            config.reph_pos = reph_positions[rng.below(reph_positions.len())];
            config.reph_mode = reph_modes[rng.below(reph_modes.len())];

            let mut expected = glyphs.clone();
            for s in &syllables {
                final_reorder(
                    &mut expected,
                    byte_offsets[s.start],
                    byte_offsets[s.end],
                    s.end - s.start,
                    config.reph_pos,
                    config.reph_mode,
                );
            }
            let mut got = glyphs;
            final_reorder_all(&mut got, &syllables, &byte_offsets, &config);
            assert_eq!(got, expected);
        }
    }

    #[test]
    fn long_run_of_pre_base_matras_reorders_in_linear_time() {
        // One consonant followed by 200000 pre-base matras is a single
        // consonant syllable. Moving the matras one rotation at a time
        // cost about 2e10 glyph copies.
        const N: usize = 200_000;
        let mut cp = vec!['\u{0915}'];
        cp.extend(core::iter::repeat('\u{093F}').take(N));
        let mut glyphs = fake_glyphs(cp.len());
        for s in &segment_syllables(&cp, &deva_config()) {
            initial_reorder(&cp, &mut glyphs, s);
        }
        let mut ids: Vec<u32> = glyphs.iter().map(|g| g.glyph_id).collect();
        ids.sort_unstable();
        assert!(ids.iter().copied().eq(1..=cp.len() as u32));
    }

    #[test]
    fn many_syllables_final_reorder_in_linear_time() {
        // 200000 one-consonant syllables. Scanning every glyph once per
        // syllable cost about 4e10 cluster comparisons.
        const N: usize = 200_000;
        let cp = vec!['\u{0915}'; N];
        let mut glyphs: Vec<Glyph> = (0..N).map(|i| Glyph::new(1, (i * 3) as u32)).collect();
        shape_indic(None, None, &cp, &mut glyphs, &deva_config());
        assert_eq!(glyphs.len(), N);
        assert_eq!(glyphs[N - 1].cluster, ((N - 1) * 3) as u32);
    }
}
