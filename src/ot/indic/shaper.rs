//! The Indic shaper, following HarfBuzz's (`hb-ot-shaper-indic.cc`):
//! the character table and syllable machine it shares with Khmer,
//! `locl` and `ccmp` per syllable, initial reordering (see the
//! `initial` module), the basic features one stage each, final
//! reordering (see the `final_reorder` module), and the other
//! features in one stage with the default ones.

use alloc::vec::Vec;

use super::machine::{self, syllable};
use super::IndicConfig;
use crate::buffer::{ClusterLevel, Glyph};
use crate::ot::syllabic::stage::{
    add_user_features, apply_alternate_feature, apply_stage, has_feature, user_disabled,
    FeatureFlags as F, MapFeature, StageFeature, GLOBAL_MASK,
};
use crate::ot::syllabic::{
    cat, categories, insert_dotted_circles, pos, set_syllables, setup_syllables, syllable_ranges,
    DottedCircle, GlyphInfo,
};
use crate::shape::{feature_would_substitute, Feature, SyllabicGsub};
use crate::tables::gdef::Gdef;
use crate::tables::layout::skip_iter::MatchGlyph;
use crate::tables::layout::Joiners;
use crate::tables::Gsub;
use crate::unicode::Script;

/// HarfBuzz's `indic_features`, with their flags. The first
/// [`INDIC_BASIC_FEATURES`] run one stage each after initial
/// reordering, the rest as one stage after final reordering.
pub(crate) const INDIC_FEATURES: [MapFeature; 17] = [
    MapFeature::new(b"nukt", F::GLOBAL_MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"akhn", F::GLOBAL_MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"rphf", F::MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"rkrf", F::GLOBAL_MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"pref", F::MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"blwf", F::MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"abvf", F::MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"half", F::MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"pstf", F::MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"vatu", F::GLOBAL_MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"cjct", F::GLOBAL_MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"init", F::MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"pres", F::GLOBAL_MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"abvs", F::GLOBAL_MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"blws", F::GLOBAL_MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"psts", F::GLOBAL_MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"haln", F::GLOBAL_MANUAL_JOINERS.union(F::PER_SYLLABLE)),
];

/// How many of [`INDIC_FEATURES`] are basic features.
pub(crate) const INDIC_BASIC_FEATURES: usize = 11;

/// `locl` and `ccmp`, which the Indic shaper enables with
/// `F_PER_SYLLABLE` before initial reordering.
const INDIC_EARLY_FEATURES: [MapFeature; 2] = [
    MapFeature::new(b"locl", F::GLOBAL.union(F::PER_SYLLABLE)),
    MapFeature::new(b"ccmp", F::GLOBAL.union(F::PER_SYLLABLE)),
];

/// The default GSUB features HarfBuzz runs in the Indic shaper's last
/// stage (`common_features` and `horizontal_features`, or `vert`, in
/// `hb-ot-shape.cc`). `override_features_indic` turns `liga` off, and
/// `ccmp` and `locl` ran first.
const DEFAULT_HORIZONTAL: [MapFeature; 4] = [
    MapFeature::new(b"rlig", F::GLOBAL),
    MapFeature::new(b"calt", F::GLOBAL),
    MapFeature::new(b"clig", F::GLOBAL),
    MapFeature::new(b"rclt", F::GLOBAL),
];
const DEFAULT_VERTICAL: [MapFeature; 2] = [
    MapFeature::new(b"rlig", F::GLOBAL),
    MapFeature::new(b"vert", F::GLOBAL),
];

/// The mask bit of feature `i` of [`INDIC_FEATURES`].
const fn bit(i: usize) -> u32 {
    1 << i
}
pub(super) const RPHF: u32 = bit(2);
pub(super) const PREF: u32 = bit(4);
pub(super) const BLWF: u32 = bit(5);
pub(super) const ABVF: u32 = bit(6);
pub(super) const HALF: u32 = bit(7);
pub(super) const PSTF: u32 = bit(8);
pub(super) const INIT: u32 = bit(11);

/// What the Indic shaper needs besides the glyphs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct IndicRun<'a> {
    /// The font's GSUB, if any.
    pub(crate) gsub: Option<&'a Gsub<'a>>,
    /// The font's GDEF, if any.
    pub(crate) gdef: Option<&'a Gdef<'a>>,
    /// The buffer's cluster level.
    pub(crate) level: ClusterLevel,
    /// The caller's feature overrides.
    pub(crate) features: &'a [Feature],
    /// True for vertical text.
    pub(crate) vertical: bool,
    /// The dotted circle glyph for broken clusters, or `None` when the
    /// font has none or the buffer asks for no dotted circles.
    pub(crate) dotted_circle: Option<u16>,
    /// The font's glyph for the script's virama, or `None`.
    pub(crate) virama_glyph: Option<u16>,
}

/// HarfBuzz's `indic_shape_plan_t`: the per-run facts reordering reads.
pub(super) struct Plan<'a> {
    pub(super) config: IndicConfig,
    /// The GSUB script tag is an old (pre-`dev2`) one.
    pub(super) is_old_spec: bool,
    /// `BLWF_MODE_POST_ONLY`: `blwf` only after the base.
    pub(super) blwf_post_only: bool,
    pub(super) has_rphf: bool,
    pub(super) has_pref: bool,
    pub(super) virama_glyph: Option<u16>,
    pub(super) level: ClusterLevel,
    runner: Option<SyllabicGsub<'a>>,
    gdef: Option<&'a Gdef<'a>>,
    features: &'a [Feature],
    /// Glyph ids and the verdicts of `consonant_position_from_face`.
    positions: Vec<(u32, u8)>,
}

impl Plan<'_> {
    /// HarfBuzz's `hb_indic_would_substitute_feature_t::would_substitute`
    /// for feature `tag` over `glyphs`.
    pub(super) fn would_substitute(&self, tag: [u8; 4], glyphs: &[u32]) -> bool {
        let Some(runner) = self.runner.as_ref() else {
            return false;
        };
        if user_disabled(self.features, tag) {
            return false;
        }
        let ids: Vec<u16> = glyphs.iter().map(|&g| g as u16).collect();
        let prio = self.config.script_priority;
        feature_would_substitute(runner.gsub(), self.gdef, tag, prio, &ids, Joiners::MANUAL)
    }

    /// HarfBuzz's `consonant_position_from_face`.
    fn consonant_position(&mut self, consonant: u32) -> u8 {
        if let Some(&(_, p)) = self.positions.iter().find(|(g, _)| *g == consonant) {
            return p;
        }
        let virama = self.virama_glyph.map_or(0, u32::from);
        let pair = |a: u32, b: u32| [a, b];
        let (vc, cv) = (pair(virama, consonant), pair(consonant, virama));
        let p = if self.would_substitute(*b"blwf", &vc)
            || self.would_substitute(*b"blwf", &cv)
            || self.would_substitute(*b"vatu", &vc)
            || self.would_substitute(*b"vatu", &cv)
        {
            pos::BELOW_C
        } else if self.would_substitute(*b"pstf", &vc)
            || self.would_substitute(*b"pstf", &cv)
            || self.would_substitute(*b"pref", &vc)
            || self.would_substitute(*b"pref", &cv)
        {
            pos::POST_C
        } else {
            pos::BASE_C
        };
        self.positions.push((consonant, p));
        p
    }

    /// The script the run shapes as.
    pub(super) fn script(&self) -> Script {
        self.config.script
    }
}

/// True when the glyph ligated (HarfBuzz's `_hb_glyph_info_ligated`).
pub(super) fn ligated(g: &Glyph) -> bool {
    MatchGlyph::from(g).is_ligated()
}

/// HarfBuzz's `_hb_glyph_info_ligated_and_didnt_multiply`.
pub(super) fn ligated_and_didnt_multiply(g: &Glyph) -> bool {
    let m = MatchGlyph::from(g);
    m.is_ligated() && !m.is_multiplied()
}

/// HarfBuzz's `is_one_of`: false for a ligated glyph, otherwise whether
/// the glyph's category is one of `cats`.
pub(super) fn is_one_of(g: &Glyph, info: &GlyphInfo, cats: &[u8]) -> bool {
    !ligated(g) && cats.contains(&info.category)
}

/// `CONSONANT_FLAGS_INDIC`.
pub(super) const CONSONANTS: [u8; 7] = [
    cat::C,
    cat::CS,
    cat::RA,
    cat::CM,
    cat::V,
    cat::PLACEHOLDER,
    cat::DOTTEDCIRCLE,
];

/// Shapes one Indic run: `codepoints` and `glyphs` are one to one on
/// entry. Runs every GSUB feature of the run, the default ones
/// included.
pub(crate) fn shape(
    run: &IndicRun<'_>,
    config: &IndicConfig,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    if codepoints.len() != glyphs.len() || glyphs.is_empty() {
        return;
    }
    let mut info: Vec<GlyphInfo> = codepoints
        .iter()
        .map(|&c| {
            let (category, position) = categories(c);
            GlyphInfo {
                category,
                position,
                word_char: crate::ot::syllabic::is_word_char(c),
                ..GlyphInfo::default()
            }
        })
        .collect();
    let cats: Vec<u8> = info.iter().map(|g| g.category).collect();
    set_syllables(&mut info, &machine::find_syllables(&cats));
    setup_syllables(glyphs, &info, run.level);

    let prio = config.script_priority;
    let runner = run
        .gsub
        .map(|gsub| SyllabicGsub::new(gsub, run.gdef, glyphs));
    let has = |tag: &[u8; 4]| {
        runner
            .as_ref()
            .is_some_and(|r| has_feature(r, run.features, *tag, prio))
    };
    let is_old_spec = !run
        .gsub
        .and_then(|g| crate::ot::layout_select::chosen_script(g.script_list(), prio))
        .is_some_and(|tag| tag[3] == b'2');
    let mut plan = Plan {
        config: *config,
        is_old_spec,
        blwf_post_only: matches!(config.script, Script::Telugu | Script::Kannada),
        has_rphf: has(b"rphf"),
        has_pref: has(b"pref"),
        virama_glyph: run.virama_glyph.filter(|&g| g != 0),
        level: run.level,
        runner,
        gdef: run.gdef,
        features: run.features,
        positions: Vec::new(),
    };

    // `locl` and `ccmp`, per syllable, before initial reordering.
    let early: Vec<StageFeature> = INDIC_EARLY_FEATURES
        .iter()
        .map(|&f| StageFeature::of(f, GLOBAL_MASK))
        .collect();
    if let Some(runner) = plan.runner.as_mut() {
        apply_stage(runner, prio, &early, run.features, glyphs, &mut info);
    }

    // `initial_reordering_indic`.
    if plan.virama_glyph.is_some() {
        for (g, i) in glyphs.iter().zip(info.iter_mut()) {
            if i.position == pos::BASE_C {
                i.position = plan.consonant_position(g.glyph_id);
            }
        }
    }
    if let Some(circle) = run.dotted_circle {
        let spec = DottedCircle {
            broken: syllable::BROKEN,
            category: cat::DOTTEDCIRCLE,
            position: pos::END,
            repha: Some(cat::REPHA),
        };
        insert_dotted_circles(glyphs, &mut info, spec, circle);
    }
    for range in syllable_ranges(&info) {
        let kind = info
            .get(range.start)
            .map_or(syllable::NON_INDIC, |g| g.syllable_type());
        if !matches!(kind, syllable::SYMBOL | syllable::NON_INDIC) {
            super::initial::reorder_syllable(&mut plan, glyphs, &mut info, range);
        }
    }

    // The basic features, one stage each.
    for (i, &f) in INDIC_FEATURES[..INDIC_BASIC_FEATURES].iter().enumerate() {
        if let Some(runner) = plan.runner.as_mut() {
            let stage = [StageFeature::of(f, bit(i))];
            apply_stage(runner, prio, &stage, run.features, glyphs, &mut info);
        }
    }

    // `final_reordering_indic`.
    for range in syllable_ranges(&info) {
        super::final_reorder::reorder_syllable(&plan, glyphs, &mut info, range);
    }

    // The other features, with the defaults and the caller's features.
    let Some(runner) = plan.runner.as_mut() else {
        return;
    };
    let defaults: &[MapFeature] = if run.vertical {
        &DEFAULT_VERTICAL
    } else {
        &DEFAULT_HORIZONTAL
    };
    let mut other: Vec<StageFeature> = INDIC_FEATURES[INDIC_BASIC_FEATURES..]
        .iter()
        .enumerate()
        .map(|(i, &f)| StageFeature::of(f, bit(INDIC_BASIC_FEATURES + i)))
        .chain(defaults.iter().map(|&f| StageFeature::of(f, GLOBAL_MASK)))
        .collect();
    let alternates = add_user_features(&mut other, run.features, early_or_off);
    apply_stage(runner, prio, &other, run.features, glyphs, &mut info);
    for (tag, value) in alternates {
        apply_alternate_feature(runner, prio, tag, value, glyphs);
    }
}

/// Tags the caller's features cannot add to the last stage: those of
/// the earlier stages, and `liga`, which the Indic shaper turns off.
fn early_or_off(tag: [u8; 4]) -> bool {
    tag == *b"liga"
        || INDIC_FEATURES[..INDIC_BASIC_FEATURES]
            .iter()
            .chain(&INDIC_EARLY_FEATURES)
            .any(|f| f.tag == tag)
}

#[cfg(test)]
mod tests;
