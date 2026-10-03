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
//! sigilbuzz merges a required feature into its tag's lookups in
//! [`crate::ot::layout_select`], which covers the first case, and a
//! required `rvrn` with it. Before a segment's first GSUB lookup,
//! [`apply_stage_zero`] runs `rvrn` unless the caller turned it off,
//! and the required feature when no pass of the segment's pipeline
//! will apply its tag, as one stage: each lookup once, in lookup-index
//! order. A value the caller gives `rvrn` picks the alternate of an
//! AlternateSubst lookup, as it would for any other feature.
//!
//! HarfBuzz enables `rvrn` in GPOS too, where it joins the one GPOS
//! stage with the other features (see [`super::gpos`]).

use alloc::vec::Vec;

use super::gsub::{apply_gsub_stage, StageLookup};
use super::joiners::FeatureFlags;
use super::{feature_disabled, Feature, LookupBudget};
use crate::buffer::Glyph;
use crate::ot::indic::indic_config_for;
use crate::ot::indic::shaper::INDIC_FEATURES;
use crate::ot::myanmar::{MYANMAR_BASIC_FEATURES, MYANMAR_TOPOGRAPHICAL_FEATURES};
use crate::ot::use_shaper::{HANGUL_FEATURES, USE_BASIC_FEATURES, USE_TOPOGRAPHICAL_FEATURES};
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;
use crate::unicode::Script;

/// The feature of GSUB stage 0, which every segment runs first.
pub(super) const RVRN: [u8; 4] = *b"rvrn";

/// The default GSUB chain every segment runs (`run_default_gsub`).
const DEFAULT_CHAIN: &[[u8; 4]] = &[
    *b"ccmp", *b"locl", *b"rlig", *b"liga", *b"clig", *b"calt", *b"rclt",
];

/// Extra default features of vertical runs.
const VERTICAL_CHAIN: &[[u8; 4]] = &[*b"vert", *b"vrt2"];

/// The features of the Arabic shaper that a later pass applies: the
/// joining forms, and `mset`, which only the Arabic shaper turns on.
const ARABIC_FEATURES: &[&[u8; 4]] = &[
    b"isol", b"fina", b"fin2", b"fin3", b"medi", b"med2", b"init", b"mset",
];

/// `locl` and `ccmp`, which the Indic shaper runs first.
const LOCL_CCMP: &[&[u8; 4]] = &[b"locl", b"ccmp"];

/// The tags this module counts as applied for Khmer.
const KHMER_TAGS: &[&[u8; 4]] = &[
    b"locl", b"ccmp", b"nukt", b"akhn", b"rphf", b"pref", b"rkrf", b"abvf", b"blwf", b"half",
    b"pstf", b"vatu", b"cjct", b"abvs", b"blws", b"haln", b"pres", b"psts",
];

/// What decides the GSUB tags one segment's pipeline applies.
pub(super) struct SegmentPlan<'a> {
    /// The segment's script.
    pub(super) script: Script,
    /// The buffer's dominant script, which gates the Hangul shaper.
    pub(super) dominant: Option<Script>,
    /// True when the Universal Shaping Engine shapes the segment.
    pub(super) use_shaper: bool,
    /// The segment's code points.
    pub(super) codepoints: &'a [char],
    /// True when the Arabic shaper's joining forms apply to the segment
    /// (Arabic and Syriac).
    pub(super) arabic: bool,
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
    /// mirroring the dispatch in [`super::shape`]. Complex shapers
    /// apply their features whatever the caller's overrides say.
    fn shaper_features(&self) -> &'static [&'static [&'static [u8; 4]]] {
        if self.use_shaper {
            return &[USE_BASIC_FEATURES, USE_TOPOGRAPHICAL_FEATURES];
        }
        let dominant = self.dominant == Some(self.script);
        match self.script {
            Script::Khmer => &[KHMER_TAGS],
            Script::Myanmar => &[MYANMAR_BASIC_FEATURES, MYANMAR_TOPOGRAPHICAL_FEATURES],
            Script::Hangul
                if dominant
                    && self
                        .codepoints
                        .iter()
                        .any(|&c| crate::unicode::is_hangul_jamo(c)) =>
            {
                &[HANGUL_FEATURES]
            }
            _ if self.arabic => &[ARABIC_FEATURES],
            _ => &[],
        }
    }

    /// True when some pass of the segment's pipeline applies `tag`.
    fn applies(&self, tag: [u8; 4]) -> bool {
        // An Indic script whose font has a `dev3`-style tag runs the
        // Universal Shaping Engine instead.
        let indic = !self.use_shaper
            && indic_config_for(self.script).is_some_and(|c| c.script != Script::Sinhala);
        if indic && (LOCL_CCMP.contains(&&tag) || INDIC_FEATURES.iter().any(|f| f.tag == tag)) {
            return true;
        }
        let default = DEFAULT_CHAIN.contains(&tag)
            || tag == RVRN
            || (self.vertical && VERTICAL_CHAIN.contains(&tag))
            || (self.backward && tag == *b"rtlm")
            || self.direction_features.contains(&tag);
        if default && !feature_disabled(self.features, tag) {
            return true;
        }
        if self.features.iter().any(|f| f.tag == tag && f.value != 0) {
            return true;
        }
        self.shaper_features()
            .iter()
            .any(|list| list.iter().any(|t| **t == tag))
    }
}

/// Runs GSUB stage 0 of the segment: `rvrn` unless the caller turned
/// it off, and the required feature of the language system the segment
/// selects when its tag is one `plan` never applies. Call before the
/// segment's first GSUB lookup. The lookups spend `budget`, the one
/// the whole [`super::shape`] call shares.
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
    let required = crate::ot::layout_select::required_feature(
        gsub.script_list(),
        &features,
        gsub.language_tags(),
        script_priority,
    )
    .filter(|&(tag, _)| !plan.applies(tag))
    .map(|(_, lookups)| lookups)
    .unwrap_or_default();
    let rvrn = if feature_disabled(plan.features, RVRN) {
        Vec::new()
    } else {
        crate::ot::layout_select::feature_lookup_indices(
            gsub.script_list(),
            &features,
            gsub.language_tags(),
            RVRN,
            script_priority,
        )
        .unwrap_or_default()
    };
    if required.is_empty() && rvrn.is_empty() {
        return;
    }
    // The caller's `rvrn` value picks the glyph an AlternateSubst lookup
    // of `rvrn` substitutes, 1 for the first alternate, as for any
    // feature. The required feature always picks the first.
    let rvrn_alternate = rvrn_alternate(plan.features);
    let mut stage: Vec<StageLookup> = required
        .into_iter()
        .filter(|index| !rvrn.contains(index))
        .map(|index| (index, 0))
        .chain(rvrn.iter().map(|&index| (index, rvrn_alternate)))
        .map(|(index, alternate)| StageLookup {
            index,
            flags: FeatureFlags::AUTO,
            alternate,
            masked: false,
        })
        .collect();
    stage.sort_unstable_by_key(|l| l.index);
    apply_gsub_stage(gsub, &stage, glyphs, gdef, None, budget);
}

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

    fn plan<'a>(script: Script, features: &'a [Feature], cps: &'a [char]) -> SegmentPlan<'a> {
        SegmentPlan {
            script,
            dominant: Some(script),
            use_shaper: script.is_use(),
            codepoints: cps,
            arabic: script == Script::Arabic,
            vertical: false,
            backward: false,
            direction_features: super::super::rotate::direction_features(crate::Direction::Ltr),
            features,
        }
    }

    #[test]
    fn direction_features_count_as_applied() {
        let p = plan(Script::Latin, &[], &[]);
        assert!(p.applies(*b"ltra"));
        assert!(!p.applies(*b"rtla"));
    }

    #[test]
    fn default_chain_tags_apply_unless_disabled() {
        let p = plan(Script::Latin, &[], &[]);
        assert!(p.applies(*b"liga"));
        assert!(p.applies(*b"ccmp"));
        assert!(!p.applies(*b"onum"));
        assert!(!p.applies(*b"init"));
        assert!(!p.applies(*b"vert"));
        let off = [Feature {
            tag: *b"liga",
            value: 0,
        }];
        assert!(!plan(Script::Latin, &off, &[]).applies(*b"liga"));
        let on = [Feature {
            tag: *b"onum",
            value: 1,
        }];
        assert!(plan(Script::Latin, &on, &[]).applies(*b"onum"));
    }

    #[test]
    fn rvrn_counts_as_applied_unless_disabled() {
        assert!(plan(Script::Latin, &[], &[]).applies(RVRN));
        assert!(plan(Script::Devanagari, &[], &[]).applies(RVRN));
        let off = [Feature {
            tag: RVRN,
            value: 0,
        }];
        assert!(!plan(Script::Latin, &off, &[]).applies(RVRN));
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
    fn complex_shaper_tags_apply() {
        assert!(plan(Script::Arabic, &[], &[]).applies(*b"init"));
        assert!(plan(Script::Devanagari, &[], &[]).applies(*b"rphf"));
        assert!(plan(Script::Khmer, &[], &[]).applies(*b"pref"));
        assert!(plan(Script::Thai, &[], &[]).applies(*b"liga"));
        assert!(!plan(Script::Latin, &[], &[]).applies(*b"rphf"));
        // The Universal Shaping Engine, unless the font sends the
        // script to the default shaper.
        assert!(plan(Script::Sinhala, &[], &[]).applies(*b"rphf"));
        let generic = SegmentPlan {
            use_shaper: false,
            ..plan(Script::Sinhala, &[], &[])
        };
        assert!(!generic.applies(*b"rphf"));
        assert!(generic.applies(*b"liga"));
        // The Hangul shaper only runs for jamo.
        assert!(!plan(Script::Hangul, &[], &['\u{AC00}']).applies(*b"ljmo"));
        assert!(plan(Script::Hangul, &[], &['\u{1100}']).applies(*b"ljmo"));
        // Complex shapers ignore the caller's overrides.
        let off = [Feature {
            tag: *b"locl",
            value: 0,
        }];
        assert!(plan(Script::Devanagari, &off, &[]).applies(*b"locl"));
        assert!(!plan(Script::Latin, &off, &[]).applies(*b"locl"));
    }
}
