//! GSUB feature application: the default feature chain, merged
//! stages, the masked and script-priority entry points the complex
//! shapers call, and the feature-to-lookup resolution behind them.

use alloc::vec::Vec;

use super::gsub::{apply_gsub_lookup, apply_gsub_lookups_masked};
use super::joiners::FeatureFlags;
use super::{feature_disabled, Feature, JoinerTable, LookupBudget};
use crate::buffer::Glyph;
use crate::ot::arabic::JoiningForm;
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;
use crate::unicode::Script;

/// Runs the default GSUB feature chain and any user-enabled extras.
/// Order matches the spec: `ccmp` + `locl` -> `rlig` -> `liga` ->
/// `clig` -> `calt`, then `vrt2` / `vert` for vertical runs.
/// HarfBuzz's Latin fallback shaper turns the horizontal list on by
/// default; sigilbuzz follows suit. User-enabled features beyond that
/// list are dispatched afterwards, respecting their 1-indexed
/// alternate-selector value.
///
/// `early_features` is the part of `ccmp` + `locl` that has not run
/// yet (see [`early_default_features`]): the Arabic path and the
/// Myanmar pass run both first. HarfBuzz runs the two in one stage,
/// so their lookups interleave by lookup index. `table` is the joiner
/// handling of the segment's shaper (Arabic runs its ligating features
/// with manual ZWJ).
///
/// `hangul` says the buffer shapes with HarfBuzz's Hangul shaper, which
/// turns `calt` off whatever the caller asks (`override_features_hangul`:
/// Uniscribe does not apply it, and some CJK fonts put all their jamo
/// lookups there).
#[allow(clippy::too_many_arguments)]
pub(super) fn run_default_gsub(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    features: &[Feature],
    want_liga: bool,
    is_vertical: bool,
    script_priority: &[[u8; 4]],
    early_features: &[[u8; 4]],
    table: JoinerTable,
    hangul: bool,
    budget: &mut LookupBudget,
) {
    let merged = |glyphs: &mut Vec<Glyph>, tags: &[[u8; 4]], budget: &mut LookupBudget| {
        let priority = script_priority;
        apply_gsub_features_merged_budgeted(
            gsub, glyphs, gdef, features, tags, priority, table, budget,
        );
    };
    let single = |glyphs: &mut Vec<Glyph>, tag: [u8; 4], alt: u16, budget: &mut LookupBudget| {
        let joiners = table.joiners(tag);
        let priority = script_priority;
        apply_gsub_feature_budgeted(gsub, glyphs, gdef, tag, alt, priority, joiners, budget);
    };
    merged(glyphs, early_features, budget);
    if !feature_disabled(features, *b"rlig") {
        single(glyphs, *b"rlig", 0, budget);
    }
    if want_liga {
        single(glyphs, *b"liga", 0, budget);
    }
    if !feature_disabled(features, *b"clig") {
        single(glyphs, *b"clig", 0, budget);
    }
    // `calt` and `rclt` together: HarfBuzz's default horizontal
    // feature list enables both, and Mongolian fonts in particular
    // ship the same lookup set under both tags (calt for legacy,
    // rclt for required-contextual). Naively running each tag's
    // lookups in turn double-applies on those fonts.
    let contextual: &[[u8; 4]] = if hangul {
        &[*b"rclt"]
    } else {
        &[*b"calt", *b"rclt"]
    };
    merged(glyphs, contextual, budget);
    // Vertical writing: HarfBuzz auto-enables `vrt2` when the font
    // carries it, otherwise falls back to `vert`. The two tags
    // cannot be active together: `vrt2` (Vertical Alternates &
    // Rotation) is the superset, so prefer it.
    if is_vertical {
        let has_vrt2 = feature_present(gsub, *b"vrt2");
        if has_vrt2 && !feature_disabled(features, *b"vrt2") {
            single(glyphs, *b"vrt2", 0, budget);
        } else if !feature_disabled(features, *b"vert") {
            single(glyphs, *b"vert", 0, budget);
        }
    }
    for feat in features {
        if feat.value == 0 {
            continue;
        }
        if is_handled_gsub_tag(feat.tag) {
            continue;
        }
        let alternate_idx = (feat.value.saturating_sub(1)).min(u32::from(u16::MAX)) as u16;
        single(glyphs, feat.tag, alternate_idx, budget);
    }
}

/// Applies several features' lookups as one pass: each lookup once,
/// in ascending lookup-index order, the order the GSUB LookupList
/// walks them. HarfBuzz runs features that share a stage this way
/// (`ccmp` with `locl`, `calt` with `rclt`), so a font whose lookups
/// for one feature must interleave with another's keeps its intended
/// order. Tags the caller disabled with a zero-valued [`Feature`] are
/// skipped. A lookup shared by several of the features skips joiners
/// only where all of them do (HarfBuzz merges the flags that way).
///
/// Runs under a fresh [`LookupBudget`] for this one pass.
pub(crate) fn apply_gsub_features_merged(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    features: &[Feature],
    tags: &[[u8; 4]],
    script_priority: &[[u8; 4]],
    table: JoinerTable,
) {
    let mut budget = LookupBudget::for_run(glyphs);
    apply_gsub_features_merged_budgeted(
        gsub,
        glyphs,
        gdef,
        features,
        tags,
        script_priority,
        table,
        &mut budget,
    );
}

/// [`apply_gsub_features_merged`] under a caller-owned budget, so
/// [`shape`](super::shape) can share one budget across every lookup
/// it applies.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_gsub_features_merged_budgeted(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    features: &[Feature],
    tags: &[[u8; 4]],
    script_priority: &[[u8; 4]],
    table: JoinerTable,
    budget: &mut LookupBudget,
) {
    let mut lookups: Vec<(u16, FeatureFlags)> = Vec::new();
    for &tag in tags {
        if feature_disabled(features, tag) {
            continue;
        }
        let joiners = table.joiners(tag);
        for index in
            lookup_indices_for_feature_in_scripts(gsub, tag, script_priority).unwrap_or_default()
        {
            match lookups.iter_mut().find(|(i, _)| *i == index) {
                Some((_, j)) => *j = j.and(joiners),
                None => lookups.push((index, joiners)),
            }
        }
    }
    lookups.sort_unstable_by_key(|&(index, _)| index);
    for (lookup_idx, joiners) in lookups {
        apply_gsub_lookup(gsub, lookup_idx, glyphs, gdef, 0, joiners, budget);
    }
}

/// The part of `ccmp` + `locl` the default GSUB pass still has to run
/// for a segment. The Arabic path and the Myanmar pass run both ahead
/// of their own features, and running either again would apply its
/// lookups twice. The Indic, Khmer, Hangul, and USE shapers run every
/// feature themselves, so the default pass does not run for them.
pub(super) fn early_default_features(arabic_ran: bool, script: Script) -> &'static [[u8; 4]] {
    const CCMP_LOCL: &[[u8; 4]] = &[*b"ccmp", *b"locl"];
    if arabic_ran || script == Script::Myanmar {
        &[]
    } else {
        CCMP_LOCL
    }
}

/// Applies `locl` and `ccmp` as one stage, as the Myanmar pass does
/// before anything else, when that keeps one glyph per code point. The
/// pass indexes its glyphs by code point, so a length-changing `ccmp`
/// has to wait until after its reorder. Returns whether the stage ran.
/// `table` is the shaper's joiner handling.
pub(crate) fn apply_locl_ccmp_if_length_preserving(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    script_priority: &[[u8; 4]],
    table: JoinerTable,
) -> bool {
    let mut trial = glyphs.clone();
    apply_gsub_features_merged(
        gsub,
        &mut trial,
        gdef,
        &[],
        &[*b"locl", *b"ccmp"],
        script_priority,
        table,
    );
    if trial.len() != glyphs.len() {
        return false;
    }
    *glyphs = trial;
    true
}

/// GSUB feature tags that `shape()` already dispatches by name,
/// so the user-override walk should skip them rather than
/// double-apply.
fn is_handled_gsub_tag(tag: [u8; 4]) -> bool {
    matches!(
        &tag,
        b"liga"
            | b"kern"
            | b"ccmp"
            | b"locl"
            | b"rlig"
            | b"clig"
            | b"calt"
            | b"rclt"
            | b"vert"
            | b"vrt2"
    )
}

/// Returns `true` when the GSUB default-LangSys advertises the named
/// feature tag. Used by the vertical-writing dispatcher to decide
/// between `vrt2` (preferred if present) and `vert` (fallback).
fn feature_present(gsub: &Gsub<'_>, tag: [u8; 4]) -> bool {
    // Vertical-writing probe runs before we know the script. Use the
    // DFLT -> first script order to match the previous behavior.
    // Arabic fonts do not ship vert/vrt2, so this choice is not
    // observable in practice.
    lookup_indices_for_feature_in_scripts(gsub, tag, &[*b"DFLT"]).is_some_and(|v| !v.is_empty())
}

/// Applies every GSUB lookup reachable via the named feature tag
/// to the glyph run in place, walking the supplied script-tag
/// priority list (`arab > DFLT` for Arabic, `dev2 > deva > DFLT` for
/// Devanagari, plain `DFLT` otherwise). The Indic shaper needs the
/// list because Devanagari fonts expose their reordering features
/// under `deva`/`dev2` and leave DFLT with only the "universal"
/// subset. The table picks the first of those tags it lists, then
/// `DFLT`, `dflt` or `latn`, and takes the feature from that script's
/// language system alone (see [`crate::ot::layout_select`]).
/// `joiners` are the feature's flags: its ZWJ/ZWNJ handling and whether
/// it matches within one syllable (see [`JoinerTable`]).
///
/// Supports every GSUB lookup type:
///
/// - 1: Single substitution (`smcp`, `vert`, `salt`, `ss01`...)
/// - 2: Multiple substitution (`ccmp` decomposition, some scripts)
/// - 3: Alternate substitution (`salt`, `swsh`, `aalt`). The
///   alternate index comes from the feature `value` (1-indexed,
///   clamped into the alternate set)
/// - 4: Ligature substitution (`liga`, `dlig`, `rlig`)
/// - 5, 6: Context and chained context substitution (`calt`,
///   `clig`, `init`, `medi`, `fina`, `isol`) with recursive nested
///   lookups
/// - 8: Reverse chained single substitution
///
/// Extension (type 7) wrappers are unwrapped to the inner type.
/// Unknown lookup types are silently skipped so callers can enable
/// forward-compatible features without the run erroring out.
///
/// Runs under a fresh [`LookupBudget`] for this one feature.
pub(crate) fn apply_gsub_feature_in_scripts(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    tag: [u8; 4],
    alternate_index: u16,
    script_priority: &[[u8; 4]],
    joiners: impl Into<FeatureFlags>,
) {
    let mut budget = LookupBudget::for_run(glyphs);
    apply_gsub_feature_budgeted(
        gsub,
        glyphs,
        gdef,
        tag,
        alternate_index,
        script_priority,
        joiners.into(),
        &mut budget,
    );
}

/// [`apply_gsub_feature_in_scripts`] under a caller-owned budget, so
/// [`shape`](super::shape) can share one budget across every lookup
/// it applies.
#[allow(clippy::too_many_arguments)]
fn apply_gsub_feature_budgeted(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    tag: [u8; 4],
    alternate_index: u16,
    script_priority: &[[u8; 4]],
    flags: FeatureFlags,
    budget: &mut LookupBudget,
) {
    if glyphs.is_empty() {
        return;
    }

    let Some(lookup_indices) = lookup_indices_for_feature_in_scripts(gsub, tag, script_priority)
    else {
        return;
    };
    if lookup_indices.is_empty() {
        return;
    }

    for lookup_idx in lookup_indices {
        apply_gsub_lookup(
            gsub,
            lookup_idx,
            glyphs,
            gdef,
            alternate_index,
            flags,
            budget,
        );
    }
}

/// Applies a single feature's lookups only at glyph positions where
/// `mask[i]` is true, HarfBuzz's per-glyph feature mask. Used by the
/// Indic shaper to gate `half` off on consonants whose post-halant
/// partner is already going to be consumed by `blwf`, and by the
/// joining and mirroring passes. The mask moves with its glyph
/// through the feature's lookups, and every input glyph a rule
/// matches must have the feature on (see
/// [`apply_gsub_lookups_masked`]). Runs under a fresh
/// [`LookupBudget`] for this one feature.
pub(crate) fn apply_gsub_feature_masked(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    tag: [u8; 4],
    script_priority: &[[u8; 4]],
    mask: &[bool],
    joiners: impl Into<FeatureFlags>,
) {
    if glyphs.is_empty() {
        return;
    }
    let Some(lookup_indices) = lookup_indices_for_feature_in_scripts(gsub, tag, script_priority)
    else {
        return;
    };
    if lookup_indices.is_empty() {
        return;
    }
    let mut budget = LookupBudget::for_run(glyphs);
    apply_gsub_lookups_masked(
        gsub,
        &lookup_indices,
        glyphs,
        gdef,
        mask,
        joiners.into(),
        &mut budget,
    );
}

/// Applies the four Arabic positional features (`isol`, `init`,
/// `medi`, `fina`), each restricted to the glyph positions whose
/// [`JoiningForm`] matches. The forms slice stays aligned with the
/// glyph run because we call this before any `ccmp`/`rlig`/`liga`
/// substitution has shrunk or expanded the stream (see the call
/// site in [`shape`](super::shape)).
pub(super) fn apply_arabic_positional_features(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    forms: &[JoiningForm],
    budget: &mut LookupBudget,
) {
    for (form, tag) in [
        (JoiningForm::Isol, *b"isol"),
        (JoiningForm::Init, *b"init"),
        (JoiningForm::Medi, *b"medi"),
        (JoiningForm::Fina, *b"fina"),
    ] {
        let Some(lookup_indices) =
            lookup_indices_for_feature_in_scripts(gsub, tag, &[*b"arab", *b"DFLT"])
        else {
            continue;
        };
        if lookup_indices.is_empty() {
            continue;
        }
        let mask: Vec<bool> = forms.iter().map(|&f| f == form).collect();
        let joiners = JoinerTable::Arabic.joiners(tag);
        apply_gsub_lookups_masked(gsub, &lookup_indices, glyphs, gdef, &mask, joiners, budget);
    }
}

/// Sorted lookup indices GSUB feature `tag` selects for a run whose
/// candidate script tags are `script_priority`. The table picks one
/// script and one language system, the language system from the
/// view's language tags (see [`Gsub::with_language_tags`]), as
/// HarfBuzz does. The walk lives in [`crate::ot::layout_select`],
/// shared with GPOS.
fn lookup_indices_for_feature_in_scripts(
    gsub: &Gsub<'_>,
    tag: [u8; 4],
    script_priority: &[[u8; 4]],
) -> Option<Vec<u16>> {
    crate::ot::layout_select::feature_lookup_indices(
        gsub.script_list(),
        gsub.feature_list(),
        gsub.language_tags(),
        tag,
        script_priority,
    )
}

/// Asks "would feature `tag`'s lookups substitute starting at the
/// head of this glyph sequence?" Used by the Indic shaper's base
/// finder to tag post-halant consonants as below-base (POS_BELOW_C)
/// when the font's `blwf` feature contains a substitution that would
/// consume `virama + consonant` (new-spec) or `consonant + virama`
/// (old-spec). Mirrors HarfBuzz's `consonant_position_from_face`.
///
/// The implementation is a dry-run: copy the candidate glyph slice
/// into a throw-away buffer, run the feature's lookups over it, and
/// report whether any glyph id changed or any glyph was removed.
/// That handles ligature subtables, chaining contexts whose nested
/// lookups ligate, and single subtables uniformly. It is considerably
/// more expensive than poking at individual subtable types, but we
/// only call it per-syllable during Indic initial reordering, so the
/// cost is bounded. The scratch glyphs carry no joiners, so the
/// feature's joiner handling (`joiners`) only matters for fidelity.
pub(crate) fn feature_would_substitute(
    gsub: &Gsub<'_>,
    gdef: Option<&Gdef<'_>>,
    tag: [u8; 4],
    script_priority: &[[u8; 4]],
    glyph_ids: &[u16],
    joiners: impl Into<FeatureFlags>,
) -> bool {
    if glyph_ids.is_empty() {
        return false;
    }
    // Build a throw-away glyph slice: cluster values don't matter,
    // only glyph ids survive the dry run. Start clusters at 0 so a
    // ligature merge collapses them to 0 deterministically.
    let mut scratch: Vec<Glyph> = glyph_ids
        .iter()
        .map(|&id| Glyph::new(u32::from(id), 0))
        .collect();
    let before: Vec<u32> = scratch.iter().map(|g| g.glyph_id).collect();
    apply_gsub_feature_in_scripts(gsub, &mut scratch, gdef, tag, 0, script_priority, joiners);
    if scratch.len() != before.len() {
        return true;
    }
    for (a, b) in scratch
        .iter()
        .map(|g| g.glyph_id)
        .zip(before.iter().copied())
    {
        if a != b {
            return true;
        }
    }
    false
}
