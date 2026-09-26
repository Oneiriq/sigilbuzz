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

use super::{apply_gsub_lookup, feature_disabled, Feature};
use crate::buffer::Glyph;
use crate::ot::indic::devanagari::{INDIC_BASIC_FEATURES, INDIC_PRESENTATION_FEATURES};
use crate::ot::indic::indic_config_for;
use crate::ot::tibetan::TIBT_FEATURES;
use crate::ot::use_shaper::{
    HANGUL_FEATURES, MYANMAR_BASIC_FEATURES, MYANMAR_TOPOGRAPHICAL_FEATURES, THAI_LAO_FEATURES,
    USE_BASIC_FEATURES, USE_TOPOGRAPHICAL_FEATURES,
};
use crate::tables::gdef::Gdef;
use crate::tables::layout::Joiners;
use crate::tables::Gsub;
use crate::unicode::Script;

/// The default GSUB chain every segment runs (`run_default_gsub`).
const DEFAULT_CHAIN: &[[u8; 4]] = &[
    *b"ccmp", *b"locl", *b"rlig", *b"liga", *b"clig", *b"calt", *b"rclt",
];

/// Extra default features of vertical runs.
const VERTICAL_CHAIN: &[[u8; 4]] = &[*b"vert", *b"vrt2"];

/// The joining-form features of the Arabic, Mongolian, and N'Ko paths.
const POSITIONAL: &[&[u8; 4]] = &[b"isol", b"init", b"medi", b"fina"];

/// `locl` and `ccmp`, which the Indic, Mongolian, and N'Ko shapers run first.
const LOCL_CCMP: &[&[u8; 4]] = &[b"locl", b"ccmp"];

/// What decides the GSUB tags one segment's pipeline applies.
pub(super) struct SegmentPlan<'a> {
    /// The segment's script.
    pub(super) script: Script,
    /// The buffer's dominant script, which gates the Tibetan,
    /// Mongolian, and Hangul shapers.
    pub(super) dominant: Option<Script>,
    /// The segment's code points.
    pub(super) codepoints: &'a [char],
    /// True when the Arabic joining pass runs for the segment.
    pub(super) arabic: bool,
    /// True for vertical layout.
    pub(super) vertical: bool,
    /// True for backward (right-to-left or bottom-to-top) runs, which
    /// apply `rtlm`.
    pub(super) backward: bool,
    /// The caller's feature overrides.
    pub(super) features: &'a [Feature],
}

impl SegmentPlan<'_> {
    /// Feature lists of the complex shaper that runs for the segment,
    /// mirroring the dispatch in [`super::shape`]. Complex shapers
    /// apply their features whatever the caller's overrides say.
    fn shaper_features(&self) -> &'static [&'static [&'static [u8; 4]]] {
        if indic_config_for(self.script).is_some() {
            return &[LOCL_CCMP, INDIC_BASIC_FEATURES, INDIC_PRESENTATION_FEATURES];
        }
        let dominant = self.dominant == Some(self.script);
        match self.script {
            Script::Khmer
            | Script::Buginese
            | Script::TaiTham
            | Script::Balinese
            | Script::Sundanese
            | Script::Lepcha
            | Script::Limbu
            | Script::Cham
            | Script::Brahmi
            | Script::Sharada
            | Script::Khojki
            | Script::Tirhuta
            | Script::Modi => &[USE_BASIC_FEATURES, USE_TOPOGRAPHICAL_FEATURES],
            Script::Myanmar => &[MYANMAR_BASIC_FEATURES, MYANMAR_TOPOGRAPHICAL_FEATURES],
            Script::Thai | Script::Lao => &[THAI_LAO_FEATURES],
            Script::NKo => &[LOCL_CCMP, POSITIONAL],
            Script::Tibetan if dominant => &[TIBT_FEATURES],
            Script::Mongolian if dominant => &[LOCL_CCMP, POSITIONAL],
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
        let default = DEFAULT_CHAIN.contains(&tag)
            || (self.vertical && VERTICAL_CHAIN.contains(&tag))
            || (self.backward && tag == *b"rtlm");
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
/// segment's first GSUB lookup.
pub(super) fn apply_unscheduled(
    gsub: &Gsub<'_>,
    glyphs: &mut alloc::vec::Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    script_priority: &[[u8; 4]],
    plan: &SegmentPlan<'_>,
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
        apply_gsub_lookup(gsub, lookup, glyphs, gdef, 0, Joiners::AUTO);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan<'a>(script: Script, features: &'a [Feature], cps: &'a [char]) -> SegmentPlan<'a> {
        SegmentPlan {
            script,
            dominant: Some(script),
            codepoints: cps,
            arabic: script == Script::Arabic,
            vertical: false,
            backward: false,
            features,
        }
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
