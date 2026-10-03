//! GSUB feature application: the default feature chain, merged
//! stages, the masked and script-priority entry points the complex
//! shapers call, and the feature-to-lookup resolution behind them.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::arabic_joining::Action;
use super::gsub::{apply_gsub_lookup, apply_gsub_lookups_masked, apply_gsub_stage, StageLookup};
use super::joiners::FeatureFlags;
use super::{feature_disabled, feature_enabled, Feature, JoinerTable, LookupBudget};
use crate::buffer::Glyph;
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;

/// Which shaper's default GSUB stages a segment runs.
#[derive(Debug, Clone, Copy)]
pub(super) enum DefaultShaper<'a> {
    /// HarfBuzz's default, Hebrew and Thai shapers, which add no stage
    /// of their own: every default feature runs in one stage with the
    /// direction features (`direction`), `rtlm` on the glyphs `rtlm`
    /// marks (backward runs), and the caller's features.
    Plain {
        direction: &'a [[u8; 4]],
        rtlm: Option<&'a [bool]>,
    },
    /// HarfBuzz's Arabic shaper after its joining features
    /// (`collect_features_arabic`): `rlig`, then `calt`, then `liga`,
    /// `clig`, `mset`, the other default features and the caller's in
    /// one stage. The shaper turns `rlig`, `calt`, `liga`, `clig` and
    /// `mset` on in both directions.
    Arabic,
}

/// What the default GSUB stages of one segment read besides the run.
pub(super) struct DefaultGsub<'a> {
    /// The caller's features.
    pub(super) features: &'a [Feature],
    /// Vertical layout.
    pub(super) vertical: bool,
    /// The segment's script tags, in the order the table tries them.
    pub(super) script_priority: &'a [[u8; 4]],
    /// The joiner handling of the segment's shaper (Arabic runs its
    /// ligating features with manual ZWJ).
    pub(super) table: JoinerTable,
    /// Whether `calt` applies. It is off for vertical text of a buffer
    /// HarfBuzz shapes with its Hangul shaper, whose vertical features
    /// leave it out whatever the caller asks. In horizontal text the
    /// Hangul shaper keeps `calt` off jamo only
    /// (`override_features_hangul` and `setup_masks_hangul`), and no
    /// jamo reach these stages.
    pub(super) calt: bool,
    /// The shaper whose stages run.
    pub(super) shaper: DefaultShaper<'a>,
}

/// Runs the default GSUB features and the caller's features in the
/// stages HarfBuzz builds for them (`hb_ot_shape_collect_features` in
/// `hb-ot-shape.cc`): `ccmp`, `locl` and `rlig` everywhere, `calt`,
/// `clig`, `liga` and `rclt` in horizontal text and `vert` in vertical
/// text (a caller can turn any of them on or off in either direction,
/// and `vrt2` only runs when asked for), and the caller's other
/// features with their 1-indexed alternate value. A stage applies the
/// lookups of all its features once each, in lookup-index order (see
/// [`apply_feature_stage`]).
pub(super) fn run_default_gsub(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    d: &DefaultGsub<'_>,
    budget: &mut LookupBudget,
) {
    let features = d.features;
    let horizontal = !d.vertical;
    // A feature HarfBuzz enables for one direction only: on by default
    // in that direction unless the caller turns it off, and in the
    // other direction only when the caller turns it on.
    let default_on = |tag: [u8; 4], direction_matches: bool| {
        if direction_matches {
            !feature_disabled(features, tag)
        } else {
            feature_enabled(features, tag)
        }
    };
    let feature = |tag: [u8; 4], masked: bool| StageFeature {
        tag,
        flags: d.table.joiners(tag),
        alternate: 0,
        masked,
    };
    let mut stage: Vec<StageFeature> = Vec::new();
    let mut mask = None;
    let arabic = matches!(d.shaper, DefaultShaper::Arabic);
    match d.shaper {
        DefaultShaper::Plain { direction, rtlm } => {
            stage.extend(direction.iter().map(|&tag| feature(tag, false)));
            if rtlm.is_some() {
                stage.push(feature(*b"rtlm", true));
                mask = rtlm;
            }
            stage.extend([feature(*b"ccmp", false), feature(*b"locl", false)]);
            stage.push(feature(*b"rlig", false));
        }
        DefaultShaper::Arabic => {
            for tag in [*b"rlig", *b"calt"] {
                let one = [feature(tag, false)];
                let priority = d.script_priority;
                apply_feature_stage(gsub, glyphs, gdef, features, priority, &one, None, budget);
            }
        }
    }
    for tag in [*b"liga", *b"clig"] {
        if arabic || default_on(tag, horizontal) {
            stage.push(feature(tag, false));
        }
    }
    if arabic {
        stage.push(feature(*b"mset", false));
    } else if d.calt && default_on(*b"calt", horizontal) {
        stage.push(feature(*b"calt", false));
    }
    if default_on(*b"rclt", horizontal) {
        stage.push(feature(*b"rclt", false));
    }
    // Vertical text gets `vert`, which the lookup selection finds in
    // any script the font lists it under (`F_GLOBAL_SEARCH`).
    if default_on(*b"vert", d.vertical) {
        stage.push(feature(*b"vert", false));
    }
    for f in features {
        if f.value == 0 || is_handled_gsub_tag(f.tag) {
            continue;
        }
        stage.push(StageFeature {
            alternate: f.value.saturating_sub(1).min(u32::from(u16::MAX)) as u16,
            ..feature(f.tag, false)
        });
    }
    let priority = d.script_priority;
    apply_feature_stage(gsub, glyphs, gdef, features, priority, &stage, mask, budget);
}

/// One feature of a GSUB stage.
#[derive(Debug, Clone, Copy)]
struct StageFeature {
    tag: [u8; 4],
    flags: FeatureFlags,
    /// The alternate an AlternateSubst lookup of the feature picks.
    alternate: u16,
    /// The feature is on only where the stage's mask says.
    masked: bool,
}

/// Applies the lookups of `stage`'s features as one stage, as
/// HarfBuzz's map builder merges a stage (`hb_ot_map_builder_t::compile`
/// in `hb-ot-map.cc`): each lookup once, in ascending lookup-index
/// order. A lookup several features share skips joiners only where all
/// of them do, and applies on every glyph when any of them does.
/// Features the caller turned off with a zero-valued [`Feature`] are
/// left out.
#[allow(clippy::too_many_arguments)]
fn apply_feature_stage(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    features: &[Feature],
    script_priority: &[[u8; 4]],
    stage: &[StageFeature],
    mask: Option<&[bool]>,
    budget: &mut LookupBudget,
) {
    let mut lookups: BTreeMap<u16, StageLookup> = BTreeMap::new();
    for f in stage {
        if feature_disabled(features, f.tag) {
            continue;
        }
        for index in
            lookup_indices_for_feature_in_scripts(gsub, f.tag, script_priority).unwrap_or_default()
        {
            lookups
                .entry(index)
                .and_modify(|l| {
                    l.flags = l.flags.and(f.flags);
                    l.masked &= f.masked;
                })
                .or_insert(StageLookup {
                    index,
                    flags: f.flags,
                    alternate: f.alternate,
                    masked: f.masked,
                });
        }
    }
    let lookups: Vec<StageLookup> = lookups.into_values().collect();
    apply_gsub_stage(gsub, &lookups, glyphs, gdef, mask, budget);
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
/// Runs under a caller-owned budget, so [`shape`](super::shape) can
/// share one budget across every lookup it applies.
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
    let stage: Vec<StageFeature> = tags
        .iter()
        .map(|&tag| StageFeature {
            tag,
            flags: table.joiners(tag),
            alternate: 0,
            masked: false,
        })
        .collect();
    apply_feature_stage(
        gsub,
        glyphs,
        gdef,
        features,
        script_priority,
        &stage,
        None,
        budget,
    );
}

/// GSUB feature tags that `shape()` already dispatches by name,
/// so the user-override walk should skip them rather than
/// double-apply. `rvrn` runs in GSUB stage 0 (see
/// [`super::required::apply_stage_zero`]).
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
            | b"rvrn"
    )
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

/// Applies `stch`, the Arabic shaper's first feature, and records the
/// glyphs its multiple substitutions produced as stretch tiles
/// (`record_stch` in `hb-ot-shaper-arabic.cc`). Returns whether it
/// recorded any, so the stretch runs after positioning (see the `stch`
/// module).
pub(super) fn apply_stch(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    features: &[Feature],
    script_priority: &[[u8; 4]],
    budget: &mut LookupBudget,
) -> bool {
    let tag = *b"stch";
    let lookups = lookup_indices_for_feature_in_scripts(gsub, tag, script_priority);
    if feature_disabled(features, tag) || lookups.as_ref().map_or(true, Vec::is_empty) {
        return false;
    }
    for index in lookups.unwrap_or_default() {
        apply_gsub_lookup(gsub, index, glyphs, gdef, 0, FeatureFlags::AUTO, budget);
    }
    super::arabic_joining::record_stch(glyphs)
}

/// Applies the Arabic shaper's joining features (`isol`, `fina`,
/// `fin2`, `fin3`, `medi`, `med2`, `init`), one stage each in that
/// order, as HarfBuzz's `collect_features_arabic` adds them. Each runs
/// on the glyphs whose joining action (see the `arabic_joining`
/// module, which stashes it in each glyph so it follows the glyph
/// through `ccmp` and the other substitutions) is its own. The
/// features come from the segment's script tags, `script_priority`.
pub(super) fn apply_arabic_positional_features(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    script_priority: &[[u8; 4]],
    budget: &mut LookupBudget,
) {
    for (action, tag) in Action::FEATURES {
        let Some(lookup_indices) =
            lookup_indices_for_feature_in_scripts(gsub, tag, script_priority)
        else {
            continue;
        };
        if lookup_indices.is_empty() {
            continue;
        }
        let mask: Vec<bool> = glyphs.iter().map(|g| action.is_on(g)).collect();
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
        &gsub.features(),
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
