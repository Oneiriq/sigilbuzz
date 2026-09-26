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

mod reorder;
mod syllable;

use alloc::vec::Vec;

use reorder::{
    code_point_clusters, compute_half_mask, final_reorder, initial_reorder, tag_positions,
};
pub(crate) use syllable::{segment_syllables, Syllable, SyllableKind};

use super::{IndicConfig, RephMode, RephPosition};
use crate::buffer::Glyph;
use crate::shape::{
    apply_gsub_feature_in_scripts, apply_gsub_feature_masked, apply_locl_ccmp_if_length_preserving,
};
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;
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
    // The cluster each code point starts, for final reordering, read
    // while glyphs are still one per code point.
    let byte_offsets = code_point_clusters(codepoints, glyphs);

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

    // HarfBuzz runs `locl` and `ccmp` as one stage before initial
    // reordering. Everything below indexes glyphs by code point, so a
    // font whose `ccmp` changes the glyph count gets `locl` as the
    // first basic feature and `ccmp` last instead.
    let early = gsub.is_some_and(|gsub| {
        apply_locl_ccmp_if_length_preserving(gsub, glyphs, gdef, config.script_priority)
    });

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
        if !early {
            apply_gsub_feature_in_scripts(gsub, glyphs, gdef, *b"locl", 0, config.script_priority);
        }
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
        if !early {
            apply_gsub_feature_in_scripts(gsub, glyphs, gdef, *b"ccmp", 0, config.script_priority);
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

/// Default Indic2 basic features, in application order. `locl` and
/// `ccmp` run before them, ahead of initial reordering as in HarfBuzz,
/// so language-specific forms (Marathi, Nepali) are in place before
/// conjunct formation.
pub(crate) const INDIC_BASIC_FEATURES: &[&[u8; 4]] = &[
    b"nukt", b"akhn", b"rphf", b"rkrf", b"blwf", b"half", b"pstf", b"vatu", b"cjct",
];

/// Default Indic2 presentation features, in application order.
/// Run after final reordering to pick the display glyphs for
/// pre-base vowels, conjuncts, and final marks.
pub(crate) const INDIC_PRESENTATION_FEATURES: &[&[u8; 4]] =
    &[b"init", b"pres", b"abvs", b"blws", b"psts", b"haln"];

#[cfg(test)]
mod tests;
