//! GSUB stage 0: `rvrn`, and the language system's required feature
//! when the pipeline never applies its tag.
//!
//! HarfBuzz (`hb_ot_shape_collect_features` in `hb-ot-shape.cc`)
//! enables `rvrn` for every shaper, in a stage of its own before the
//! direction features. `rvrn` is how a variable font swaps glyphs by
//! region of the design space: the feature usually has no lookups of
//! its own, and a FeatureVariations record gives it some (see
//! [`crate::tables::layout::feature_variations`]).
//!
//! HarfBuzz (`hb_ot_map_builder_t::compile` in `hb-ot-map.cc`) also
//! runs a language system's required feature on every glyph. When the
//! shaper plan enables a feature with the same tag, the required
//! feature's lookups join that feature's stage; otherwise they run in
//! stage 0, with `rvrn`. A tag the caller turned off counts as not
//! enabled, so the required feature still runs, in stage 0.
//!
//! sigilbuzz covers the first case in two places. The default chain,
//! which the default, Arabic, Hebrew and Thai shapers run, merges a
//! required feature into its tag's lookups in
//! [`crate::ot::layout_select`]. The Indic, Khmer, Myanmar and Hangul
//! shapers and the Universal Shaping Engine add it to the stage of a
//! feature with its tag (`stage_lookups` in
//! [`crate::ot::syllabic::stage`]). Before a segment's first GSUB
//! lookup, [`apply_stage_zero`] runs `rvrn` unless the caller turned it
//! off, and the required feature when no pass of the segment's pipeline
//! will apply its tag or its tag is `rvrn`, as one stage: each lookup
//! once, in lookup-index order. Which passes run depends on the shaper
//! the pipeline picked for the segment ([`SegmentShaper`]), not on the
//! script alone: HarfBuzz sends an Indic or Myanmar script to the
//! default shaper when the script tag the font's GSUB picks is `DFLT`
//! or `latn` (or `mymr` for Myanmar), but not `dflt` (see
//! [`super::shaper::Shaper::for_run`]). A value the
//! caller gives `rvrn` picks the alternate of an AlternateSubst lookup,
//! as it would for any other feature, except in a lookup `rvrn` shares
//! with the required feature. There a value above 1 picks no alternate
//! at all, as in HarfBuzz (see [`stage_zero_alternate`]).
//!
//! HarfBuzz enables `rvrn` in GPOS too, where it joins the one GPOS
//! stage with the other features (see [`super::gpos`]).

use alloc::vec::Vec;

use super::gsub::{apply_gsub_stage, StageLookup};
use super::joiners::FeatureFlags;
use super::{feature_disabled, Feature, LookupBudget};
use crate::buffer::Glyph;
use crate::ot::indic::shaper::INDIC_FEATURES;
use crate::ot::myanmar::{MYANMAR_BASIC_FEATURES, MYANMAR_TOPOGRAPHICAL_FEATURES};
use crate::ot::use_shaper::{HANGUL_FEATURES, USE_BASIC_FEATURES, USE_TOPOGRAPHICAL_FEATURES};
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;

/// The feature of GSUB stage 0, which every segment runs first.
pub(super) const RVRN: [u8; 4] = *b"rvrn";

/// The default GSUB features every segment runs in either direction
/// (`common_features` in `hb-ot-shape.cc`, and `run_default_gsub`).
const COMMON_CHAIN: &[[u8; 4]] = &[*b"ccmp", *b"locl", *b"rlig"];

/// The default GSUB features of horizontal runs only
/// (`horizontal_features`). Vertical runs apply them only when the
/// caller turns them on.
const HORIZONTAL_CHAIN: &[[u8; 4]] = &[*b"liga", *b"clig", *b"calt", *b"rclt"];

/// The default GSUB feature of vertical runs. HarfBuzz enables no
/// `vrt2`, so it only applies when the caller turns it on.
const VERTICAL_CHAIN: &[[u8; 4]] = &[*b"vert"];

/// The features of the Arabic shaper that a later pass applies: the
/// joining forms, and `mset`, which only the Arabic shaper turns on.
const ARABIC_FEATURES: &[&[u8; 4]] = &[
    b"isol", b"fina", b"fin2", b"fin3", b"medi", b"med2", b"init", b"mset",
];

/// `locl` and `ccmp`, which the Indic shaper runs first.
const LOCL_CCMP: &[&[u8; 4]] = &[b"locl", b"ccmp"];

/// The tags the Khmer shaper applies: `locl` and `ccmp`, then
/// HarfBuzz's `khmer_features` ([`crate::ot::khmer::KHMER_FEATURES`]).
const KHMER_TAGS: &[&[u8; 4]] = &[
    b"locl", b"ccmp", b"pref", b"blwf", b"abvf", b"pstf", b"cfar", b"pres", b"abvs", b"blws",
    b"psts",
];

/// `liga`, which HarfBuzz's Indic and Khmer shapers turn off after the
/// caller's features (`override_features_indic`,
/// `override_features_khmer`), so it is off whatever the caller asks.
const LIGA: [u8; 4] = *b"liga";

/// `clig`, which HarfBuzz's Khmer shaper turns on after the caller's
/// features (`override_features_khmer`), so it is on whatever the
/// caller asks, in vertical text too.
const CLIG: [u8; 4] = *b"clig";

/// The shaper whose GSUB passes run for one segment, as
/// [`super::shape`] dispatches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SegmentShaper {
    /// The default chain alone: the default, Hebrew and Thai shapers,
    /// and the default shaper HarfBuzz picks for an Indic, Myanmar, or
    /// Universal Shaping Engine script whose GSUB script is `DFLT` or
    /// `latn` (or `mymr` for Myanmar). A `dflt` script keeps the
    /// script's own shaper.
    Default,
    /// The Arabic shaper's joining forms (Arabic and Syriac), then the
    /// default chain.
    Arabic,
    /// The Indic shaper.
    Indic,
    /// The Khmer shaper.
    Khmer,
    /// The Myanmar shaper.
    Myanmar,
    /// The Universal Shaping Engine.
    Use,
    /// The Hangul shaper's GSUB stage, which a Hangul segment of a
    /// Hangul buffer runs.
    Hangul,
}

/// What decides the GSUB tags one segment's pipeline applies.
pub(super) struct SegmentPlan<'a> {
    /// The shaper the pipeline runs for the segment.
    pub(super) shaper: SegmentShaper,
    /// True for vertical layout.
    pub(super) vertical: bool,
    /// True for backward (right-to-left or bottom-to-top) runs, which
    /// apply `rtlm`.
    pub(super) backward: bool,
    /// The direction features the run applies (`ltra`, `ltrm`, `rtla`).
    pub(super) direction_features: &'a [[u8; 4]],
    /// The caller's feature overrides.
    pub(super) features: &'a [Feature],
}

impl SegmentPlan<'_> {
    /// Feature lists of the complex shaper that runs for the segment,
    /// other than the Indic shaper's, and whether that shaper leaves out
    /// a feature the caller turned off. The syllable-based shapers do
    /// (`apply_stage` in [`crate::ot::syllabic::stage`]), as HarfBuzz's
    /// map drops a feature whose value is 0. The Arabic joining forms
    /// always run.
    fn shaper_features(&self) -> (&'static [&'static [&'static [u8; 4]]], bool) {
        match self.shaper {
            SegmentShaper::Use => (&[USE_BASIC_FEATURES, USE_TOPOGRAPHICAL_FEATURES], true),
            SegmentShaper::Khmer => (&[KHMER_TAGS], true),
            SegmentShaper::Myanmar => (
                &[MYANMAR_BASIC_FEATURES, MYANMAR_TOPOGRAPHICAL_FEATURES],
                true,
            ),
            SegmentShaper::Hangul => (&[HANGUL_FEATURES], true),
            SegmentShaper::Arabic => (&[ARABIC_FEATURES], false),
            SegmentShaper::Indic | SegmentShaper::Default => (&[], false),
        }
    }

    /// True when some pass of the segment's pipeline applies `tag`.
    fn applies(&self, tag: [u8; 4]) -> bool {
        let indic = self.shaper == SegmentShaper::Indic;
        if indic && (LOCL_CCMP.contains(&&tag) || INDIC_FEATURES.iter().any(|f| f.tag == tag)) {
            return !feature_disabled(self.features, tag);
        }
        // The overrides of HarfBuzz's Indic and Khmer shapers win over
        // the caller's features.
        let khmer = self.shaper == SegmentShaper::Khmer;
        if tag == LIGA && (indic || khmer) {
            return false;
        }
        if tag == CLIG && khmer {
            return true;
        }
        let chain = if self.vertical {
            VERTICAL_CHAIN
        } else {
            HORIZONTAL_CHAIN
        };
        let default = COMMON_CHAIN.contains(&tag)
            || chain.contains(&tag)
            || tag == RVRN
            || (self.backward && tag == *b"rtlm")
            || self.direction_features.contains(&tag);
        if default && !feature_disabled(self.features, tag) {
            return true;
        }
        if self.features.iter().any(|f| f.tag == tag && f.value != 0) {
            return true;
        }
        let (lists, honors_overrides) = self.shaper_features();
        lists.iter().any(|list| list.iter().any(|t| **t == tag))
            && !(honors_overrides && feature_disabled(self.features, tag))
    }
}

/// Runs GSUB stage 0 of the segment: `rvrn` unless the caller turned
/// it off, and the required feature of the language system the segment
/// selects when its tag is `rvrn` or one `plan` never applies. Call
/// before the segment's first GSUB lookup. The lookups spend `budget`,
/// the one the whole [`super::shape`] call shares.
pub(super) fn apply_stage_zero(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    script_priority: &[[u8; 4]],
    plan: &SegmentPlan<'_>,
    budget: &mut LookupBudget,
) {
    if glyphs.is_empty() {
        return;
    }
    let features = gsub.features();
    // A required `rvrn` runs here too, in the stage of its tag.
    let required = crate::ot::layout_select::required_feature(
        gsub.script_list(),
        &features,
        gsub.language_tags(),
        script_priority,
    )
    .filter(|&(tag, _)| tag == RVRN || !plan.applies(tag))
    .map(|(_, lookups)| lookups)
    .unwrap_or_default();
    let rvrn = if feature_disabled(plan.features, RVRN) {
        Vec::new()
    } else {
        crate::ot::layout_select::listed_feature_lookups(
            gsub.script_list(),
            &features,
            gsub.language_tags(),
            RVRN,
            script_priority,
        )
    };
    if required.is_empty() && rvrn.is_empty() {
        return;
    }
    let rvrn_alternate = rvrn_alternate(plan.features);
    let mut stage: Vec<StageLookup> = required
        .iter()
        .chain(rvrn.iter().filter(|index| !required.contains(index)))
        .map(|&index| StageLookup {
            index,
            flags: FeatureFlags::AUTO,
            alternate: stage_zero_alternate(
                required.contains(&index),
                rvrn.contains(&index),
                rvrn_alternate,
            ),
            masked: false,
        })
        .collect();
    stage.sort_unstable_by_key(|l| l.index);
    apply_gsub_stage(gsub, &stage, glyphs, gdef, None, budget);
}

/// The alternate an AlternateSubst lookup of stage 0 picks, given
/// whether the required feature and `rvrn` have it: `rvrn_alternate`
/// (see [`rvrn_alternate`]) for a lookup of `rvrn` alone, the first
/// alternate for one of the required feature alone, and none for one
/// they share while the caller picks an alternate past the first.
///
/// HarfBuzz runs the required feature with the global mask bit, and
/// `rvrn` with it too unless the caller gives `rvrn` a value above 1,
/// which takes mask bits of its own. A lookup the two share runs once
/// with both masks OR-ed together, and AlternateSubst reads its
/// alternate index from that mask from `rvrn`'s lowest bit up, the
/// global bit included. The index then overruns the alternate set, so
/// the lookup substitutes nothing.
fn stage_zero_alternate(in_required: bool, in_rvrn: bool, rvrn_alternate: u16) -> u16 {
    match (in_required, in_rvrn) {
        (true, true) if rvrn_alternate > 0 => NO_ALTERNATE,
        (true, _) => 0,
        (false, _) => rvrn_alternate,
    }
}

/// An alternate index past every AlternateSet, which holds at most
/// 65,535 glyphs, so the lookup substitutes nothing.
const NO_ALTERNATE: u16 = u16::MAX;

/// The alternate an AlternateSubst lookup of `rvrn` picks: the last
/// value the caller gave `rvrn`, less 1, and the first alternate when
/// the caller gave none.
fn rvrn_alternate(features: &[Feature]) -> u16 {
    features
        .iter()
        .rev()
        .find(|f| f.tag == RVRN)
        .map_or(0, |f| {
            f.value.saturating_sub(1).min(u32::from(u16::MAX)) as u16
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use SegmentShaper as S;

    fn plan(shaper: SegmentShaper, features: &[Feature]) -> SegmentPlan<'_> {
        SegmentPlan {
            shaper,
            vertical: false,
            backward: false,
            direction_features: super::super::rotate::direction_features(crate::Direction::Ltr),
            features,
        }
    }

    fn off(tag: &[u8; 4]) -> Feature {
        Feature {
            tag: *tag,
            value: 0,
        }
    }

    fn on(tag: &[u8; 4]) -> Feature {
        Feature {
            tag: *tag,
            value: 1,
        }
    }

    #[test]
    fn direction_features_count_as_applied() {
        let p = plan(S::Default, &[]);
        assert!(p.applies(*b"ltra"));
        assert!(!p.applies(*b"rtla"));
    }

    #[test]
    fn default_chain_tags_apply_unless_disabled() {
        let p = plan(S::Default, &[]);
        assert!(p.applies(*b"liga"));
        assert!(p.applies(*b"ccmp"));
        assert!(!p.applies(*b"onum"));
        assert!(!p.applies(*b"init"));
        assert!(!p.applies(*b"vert"));
        assert!(!plan(S::Default, &[off(b"liga")]).applies(*b"liga"));
        assert!(plan(S::Default, &[on(b"onum")]).applies(*b"onum"));
    }

    #[test]
    fn vertical_runs_apply_vert_and_no_horizontal_default() {
        // `horizontal_features` and `vrt2` only apply in vertical text
        // when the caller turns them on, so a required feature with
        // their tag runs in stage 0 there.
        fn vertical(shaper: SegmentShaper, features: &[Feature]) -> SegmentPlan<'_> {
            SegmentPlan {
                vertical: true,
                ..plan(shaper, features)
            }
        }
        let liga_on = [on(b"liga")];
        for shaper in [S::Default, S::Indic, S::Myanmar, S::Use, S::Hangul] {
            let p = vertical(shaper, &[]);
            assert!(p.applies(*b"vert"), "{shaper:?}");
            assert!(p.applies(*b"ccmp"), "{shaper:?}");
            assert!(p.applies(*b"rlig"), "{shaper:?}");
            for tag in [b"liga", b"clig", b"calt", b"rclt", b"vrt2"] {
                assert!(!p.applies(*tag), "{shaper:?} {tag:?}");
            }
        }
        assert!(vertical(S::Default, &liga_on).applies(*b"liga"));
        assert!(!vertical(S::Indic, &liga_on).applies(*b"liga"));
    }

    #[test]
    fn rvrn_counts_as_applied_unless_disabled() {
        assert!(plan(S::Default, &[]).applies(RVRN));
        assert!(plan(S::Indic, &[]).applies(RVRN));
        assert!(!plan(S::Default, &[off(b"rvrn")]).applies(RVRN));
    }

    #[test]
    fn the_last_rvrn_value_picks_the_alternate() {
        let rvrn = |value| Feature { tag: RVRN, value };
        assert_eq!(rvrn_alternate(&[]), 0);
        assert_eq!(rvrn_alternate(&[rvrn(1)]), 0);
        assert_eq!(rvrn_alternate(&[rvrn(3)]), 2);
        let liga = Feature {
            tag: *b"liga",
            value: 5,
        };
        assert_eq!(rvrn_alternate(&[rvrn(3), liga, rvrn(2)]), 1);
        assert_eq!(rvrn_alternate(&[liga]), 0);
        assert_eq!(rvrn_alternate(&[rvrn(u32::MAX)]), u16::MAX);
    }

    #[test]
    fn a_lookup_shared_with_the_required_feature_takes_no_later_alternate() {
        // `rvrn` alone: the caller's alternate.
        assert_eq!(stage_zero_alternate(false, true, 0), 0);
        assert_eq!(stage_zero_alternate(false, true, 2), 2);
        // The required feature alone: the first alternate.
        assert_eq!(stage_zero_alternate(true, false, 0), 0);
        assert_eq!(stage_zero_alternate(true, false, 2), 0);
        // Both: the first alternate, or none past it.
        assert_eq!(stage_zero_alternate(true, true, 0), 0);
        assert_eq!(stage_zero_alternate(true, true, 1), NO_ALTERNATE);
        assert_eq!(stage_zero_alternate(true, true, u16::MAX), NO_ALTERNATE);
    }

    #[test]
    fn complex_shaper_tags_apply() {
        assert!(plan(S::Arabic, &[]).applies(*b"init"));
        assert!(plan(S::Indic, &[]).applies(*b"rphf"));
        assert!(plan(S::Khmer, &[]).applies(*b"pref"));
        assert!(plan(S::Myanmar, &[]).applies(*b"pref"));
        assert!(plan(S::Use, &[]).applies(*b"rphf"));
        assert!(plan(S::Hangul, &[]).applies(*b"ljmo"));
        assert!(plan(S::Default, &[]).applies(*b"liga"));
        assert!(!plan(S::Default, &[]).applies(*b"rphf"));
        assert!(!plan(S::Default, &[]).applies(*b"init"));
    }

    #[test]
    fn the_default_shaper_applies_no_syllabic_feature() {
        // HarfBuzz sends an Indic, Myanmar, or Universal Shaping Engine
        // script whose font only has `DFLT` or `latn` lookups (or `mymr`
        // for Myanmar) to the default shaper. That shaper
        // runs `liga` and none of the syllabic features, so a required
        // `liga` joins the default chain's `liga`, and a required `rphf`
        // or `pref` runs in stage 0.
        let default = plan(S::Default, &[]);
        assert!(default.applies(LIGA));
        for tag in [b"rphf", b"pref", b"blwf", b"pres", b"ljmo"] {
            assert!(!default.applies(*tag), "{tag:?}");
        }
    }

    #[test]
    fn the_syllabic_shapers_leave_out_what_the_caller_turns_off() {
        // HarfBuzz's map drops a feature whose value is 0, so a required
        // feature with its tag runs in stage 0.
        assert!(!plan(S::Indic, &[off(b"locl")]).applies(*b"locl"));
        assert!(!plan(S::Default, &[off(b"locl")]).applies(*b"locl"));
        assert!(!plan(S::Indic, &[off(b"rphf")]).applies(*b"rphf"));
        assert!(!plan(S::Khmer, &[off(b"cfar")]).applies(*b"cfar"));
        assert!(!plan(S::Use, &[off(b"rphf")]).applies(*b"rphf"));
        assert!(!plan(S::Myanmar, &[off(b"pref")]).applies(*b"pref"));
        assert!(!plan(S::Hangul, &[off(b"ljmo")]).applies(*b"ljmo"));
        // The Arabic joining forms run whatever the caller says.
        assert!(plan(S::Arabic, &[off(b"init")]).applies(*b"init"));
    }

    #[test]
    fn khmer_applies_harfbuzz_khmer_features() {
        let khmer = plan(S::Khmer, &[]);
        for tag in [b"locl", b"ccmp", b"pstf", b"cfar", b"psts"] {
            assert!(khmer.applies(*tag), "{tag:?}");
        }
        // Indic features HarfBuzz's Khmer shaper does not have.
        for tag in [b"rphf", b"half", b"akhn", b"nukt"] {
            assert!(!khmer.applies(*tag), "{tag:?}");
        }
        let tags: Vec<[u8; 4]> = KHMER_TAGS.iter().map(|t| **t).collect();
        let mut expected = alloc::vec![*b"locl", *b"ccmp"];
        expected.extend(crate::ot::khmer::KHMER_FEATURES.iter().map(|f| f.tag));
        assert_eq!(tags, expected);
    }

    #[test]
    fn indic_and_khmer_never_apply_liga() {
        for shaper in [S::Indic, S::Khmer] {
            assert!(!plan(shaper, &[]).applies(LIGA), "{shaper:?}");
            assert!(!plan(shaper, &[on(b"liga")]).applies(LIGA), "{shaper:?}");
        }
        for shaper in [S::Use, S::Myanmar, S::Default] {
            assert!(plan(shaper, &[]).applies(LIGA), "{shaper:?}");
        }
    }

    #[test]
    fn khmer_always_applies_clig() {
        // `override_features_khmer` turns `clig` on after the caller's
        // features, in vertical text too.
        let clig_off = [off(b"clig")];
        assert!(plan(S::Khmer, &[]).applies(CLIG));
        assert!(plan(S::Khmer, &clig_off).applies(CLIG));
        let vertical = SegmentPlan {
            vertical: true,
            ..plan(S::Khmer, &clig_off)
        };
        assert!(vertical.applies(CLIG));
        // Other shapers leave `clig` to the caller.
        for shaper in [S::Indic, S::Myanmar, S::Use, S::Default] {
            assert!(plan(shaper, &[]).applies(CLIG), "{shaper:?}");
            assert!(!plan(shaper, &[off(b"clig")]).applies(CLIG), "{shaper:?}");
        }
    }
}
