//! The Indic entry points, and sigilbuzz's earlier Indic pass.
//!
//! [`shape_indic`] and [`shape_devanagari`] run the port of HarfBuzz's
//! Indic shaper (`crate::ot::indic::shaper`) for the nine scripts it
//! covers. Sinhala still takes the earlier pass described below, which
//! the rest of this module implements. It takes an [`IndicConfig`]
//! that captures the per-script virama, ra, reph position, reph mode,
//! and GSUB script-tag priority, and runs a HarfBuzz-style reorder and
//! feature pipeline over the ISC and IPC tables in
//! [`crate::unicode::indic_category`].
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
//! - Split matras (e.g. Tamil U+0BCA `O = e + aa`) decompose in the
//!   shaper's normalization, with the Indic shaper's hooks, so this
//!   module only ever sees their components.
//! - Only `half` runs with a per-glyph mask. The other basic features
//!   run across the whole run and rely on the font's lookups to touch
//!   only the right glyphs.

mod reorder;
mod syllable;

use alloc::vec::Vec;

use reorder::{
    code_point_clusters, compute_half_mask, final_reorder_all, initial_reorder, tag_positions,
};
pub(crate) use syllable::{segment_syllables, Syllable, SyllableKind};

use super::{IndicConfig, RephMode, RephPosition};
use crate::buffer::{ClusterLevel, Glyph};
use crate::shape::{
    apply_gsub_feature_in_scripts, apply_gsub_feature_masked, apply_locl_ccmp_if_length_preserving,
    JoinerTable,
};
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;
use crate::unicode::Script;

/// Script-agnostic Indic entry point. Re-orders and runs the Indic
/// features over the portion of `glyphs` that corresponds to the
/// Indic run described by `codepoints`, using `config` for per-script
/// behavior.
///
/// `codepoints` is in one-to-one correspondence with the starting
/// glyph layout: each codepoint produced one glyph before any
/// reordering. After this function returns, `glyphs` may contain
/// fewer entries (if basic features applied ligatures) and the
/// order can differ from input.
///
/// The nine scripts of HarfBuzz's Indic shaper run through its port
/// (`crate::ot::indic::shaper`), every GSUB feature of the run
/// included, the default ones too. The virama glyph comes from the run
/// itself, and broken clusters get no dotted circle here. Shaping
/// through [`crate::shape`] adds both from the font. Sinhala, which
/// HarfBuzz sends to the Universal Shaping Engine, keeps sigilbuzz's
/// earlier Indic pass.
pub fn shape_indic(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    config: &IndicConfig,
    level: ClusterLevel,
) {
    if config.script == Script::Sinhala {
        shape_indic_legacy(gsub, gdef, codepoints, glyphs, config, level);
        return;
    }
    let virama_glyph = codepoints
        .iter()
        .zip(glyphs.iter())
        .find(|(&c, _)| c as u32 == config.virama)
        .map(|(_, g)| g.glyph_id as u16);
    let run = super::shaper::IndicRun {
        gsub,
        gdef,
        level,
        features: &[],
        vertical: false,
        dotted_circle: None,
        virama_glyph,
    };
    super::shaper::shape(&run, config, codepoints, glyphs);
}

/// sigilbuzz's earlier Indic pass, which Sinhala still runs through.
pub(crate) fn shape_indic_legacy(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    config: &IndicConfig,
    level: ClusterLevel,
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

    // The joiner handling of HarfBuzz's shaper for the script: the
    // Indic shaper's features take ZWJ and ZWNJ as ordinary glyphs;
    // Sinhala goes to the Universal Shaping Engine instead.
    let table = if config.script == Script::Sinhala {
        JoinerTable::Use
    } else {
        JoinerTable::Indic
    };
    let prio = config.script_priority;

    // HarfBuzz runs `locl` and `ccmp` as one stage before initial
    // reordering. Everything below indexes glyphs by code point, so a
    // font whose `ccmp` changes the glyph count gets `locl` as the
    // first basic feature and `ccmp` last instead.
    let early = gsub.is_some_and(|gsub| {
        apply_locl_ccmp_if_length_preserving(gsub, glyphs, gdef, config.script_priority, table)
    });

    // Initial reordering is per-syllable and mutates `glyphs` in
    // place. Indic reorder is length-preserving (same glyph count
    // in, same out) because decomposition runs separately, so
    // forward iteration is safe here.
    for syllable in &syllables {
        initial_reorder(codepoints, glyphs, syllable, level);
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
            let joiners = table.joiners(*b"locl");
            apply_gsub_feature_in_scripts(gsub, glyphs, gdef, *b"locl", 0, prio, joiners);
        }
        for &&tag in INDIC_BASIC_FEATURES {
            let joiners = table.joiners(tag);
            if tag == *b"half" {
                apply_gsub_feature_masked(gsub, glyphs, gdef, tag, prio, &half_mask, joiners);
            } else {
                apply_gsub_feature_in_scripts(gsub, glyphs, gdef, tag, 0, prio, joiners);
            }
        }
    }

    // Final reordering: reph moves to its display slot. Feature
    // execution above may have replaced the reph candidate with the
    // reph glyph via `rphf`; we locate it by the
    // `RaToBecomeReph` tag we set above, which the ligature path
    // preserved on the surviving glyph.
    // Pre-base matras merge their clusters with the base first.
    final_reorder_all(glyphs, &syllables, &byte_offsets, config, level);

    // Presentation features.
    if let Some(gsub) = gsub {
        for &&tag in INDIC_PRESENTATION_FEATURES {
            let joiners = table.joiners(tag);
            apply_gsub_feature_in_scripts(gsub, glyphs, gdef, tag, 0, prio, joiners);
        }
        if !early {
            let joiners = table.joiners(*b"ccmp");
            apply_gsub_feature_in_scripts(gsub, glyphs, gdef, *b"ccmp", 0, prio, joiners);
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
    level: ClusterLevel,
) {
    // `indic_config_for` has a Devanagari entry, so the early return
    // never fires.
    let Some(config) = super::indic_config_for(Script::Devanagari) else {
        return;
    };
    shape_indic(gsub, gdef, codepoints, glyphs, &config, level);
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
