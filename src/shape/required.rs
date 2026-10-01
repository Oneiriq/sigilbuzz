//! Language system required features whose tag the pipeline never
//! applies.
//!
//! HarfBuzz (`hb_ot_map_builder_t::compile` in `hb-ot-map.cc`) runs a
//! language system's required feature on every glyph. When the shaper
//! plan enables a feature with the same tag, the required feature's
//! lookups join that feature's stage; otherwise they run in stage 0,
//! before any other GSUB lookup. A tag the caller turned off counts as
//! not enabled, so the required feature still runs, in stage 0.
//!
//! sigilbuzz merges a required feature into its tag's lookups in
//! [`crate::ot::layout_select`], which covers the first case. This
//! module covers the second: before a segment's first GSUB lookup,
//! [`apply_unscheduled`] runs the required feature when no pass of
//! the segment's pipeline will apply its tag.

use super::joiners::FeatureFlags;
use super::{apply_gsub_lookup, feature_disabled, Feature, LookupBudget};
use crate::buffer::Glyph;
use crate::ot::indic::indic_config_for;
use crate::ot::indic::shaper::INDIC_FEATURES;
use crate::ot::myanmar::{MYANMAR_BASIC_FEATURES, MYANMAR_TOPOGRAPHICAL_FEATURES};
use crate::ot::use_shaper::{HANGUL_FEATURES, USE_BASIC_FEATURES, USE_TOPOGRAPHICAL_FEATURES};
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;
use crate::unicode::Script;

/// The default GSUB chain every segment runs (`run_default_gsub`).
const DEFAULT_CHAIN: &[[u8; 4]] = &[
    *b"ccmp", *b"locl", *b"rlig", *b"liga", *b"clig", *b"calt", *b"rclt",
];

/// Extra default features of vertical runs.
const VERTICAL_CHAIN: &[[u8; 4]] = &[*b"vert", *b"vrt2"];

/// The joining-form features of the Arabic path.
const POSITIONAL: &[&[u8; 4]] = &[b"isol", b"init", b"medi", b"fina"];

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
    /// True when the Arabic joining pass runs for the segment.
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
            Script::Arabic if self.arabic => &[POSITIONAL],
            _ => &[],
        }
    }

    /// True when some pass of the segment's pipeline applies `tag`.
    fn applies(&self, tag: [u8; 4]) -> bool {
        let indic = indic_config_for(self.script).is_some_and(|c| c.script != Script::Sinhala);
        if indic && (LOCL_CCMP.contains(&&tag) || INDIC_FEATURES.iter().any(|f| f.tag == tag)) {
            return true;
        }
        let default = DEFAULT_CHAIN.contains(&tag)
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

/// Runs the required feature of the language system the segment
/// selects, when its tag is one `plan` never applies. Call before the
/// segment's first GSUB lookup. The lookups spend `budget`, the one
/// the whole [`super::shape`] call shares.
pub(super) fn apply_unscheduled(
    gsub: &Gsub<'_>,
    glyphs: &mut alloc::vec::Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    script_priority: &[[u8; 4]],
    plan: &SegmentPlan<'_>,
    budget: &mut LookupBudget,
) {
    if glyphs.is_empty() {
        return;
    }
    let Some((tag, lookups)) = crate::ot::layout_select::required_feature(
        gsub.script_list(),
        gsub.feature_list(),
        gsub.language_tags(),
        script_priority,
    ) else {
        return;
    };
    if plan.applies(tag) {
        return;
    }
    for lookup in lookups {
        apply_gsub_lookup(gsub, lookup, glyphs, gdef, 0, FeatureFlags::AUTO, budget);
    }
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
