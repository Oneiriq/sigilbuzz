//! The Myanmar shaper, following HarfBuzz's
//! (`hb-ot-shaper-myanmar.cc`).
//!
//! 1. Every character gets its Myanmar category from the Indic-family
//!    table (`set_myanmar_properties`), and the syllable machine of
//!    `hb-ot-shaper-myanmar-machine.rl` splits the run into consonant
//!    syllables, broken clusters, and other characters. Each syllable
//!    is unsafe to break (`setup_syllables_myanmar`).
//! 2. `locl` and `ccmp` run as one stage, per syllable, on the logical
//!    order.
//! 3. Broken clusters get a dotted circle, and each consonant syllable
//!    and broken cluster is reordered (`reorder_myanmar`, see the
//!    `reorder` module): a kinzi goes after the base, a medial ra and
//!    the glyphs before the base before it, pre-base vowels to the
//!    front. The sort merges the clusters each move passes.
//! 4. The basic features `rphf`, `pref`, `blwf`, and `pstf` run one
//!    stage each, per syllable, with manual ZWJ.
//! 5. The other features `pres`, `abvs`, `blws`, and `psts` run as one
//!    stage, with manual ZWJ, along with the default features HarfBuzz
//!    puts in the same stage (`rlig`, `calt`, `clig`, `liga`, `rclt`, or
//!    `vert` in vertical text) and the caller's features.
//!
//! Myanmar uses no feature masks: every feature applies to every glyph.

mod machine;
mod reorder;
mod sort;

use alloc::vec::Vec;

use super::syllabic::stage::{
    add_user_features, apply_alternate_feature, apply_stage, FeatureFlags as F, MapFeature,
    StageFeature, GLOBAL_MASK,
};
use super::syllabic::{
    cat, categories, insert_dotted_circles, set_syllables, setup_syllables, syllable_ranges,
    DottedCircle, GlyphInfo,
};
use crate::buffer::{ClusterLevel, Glyph};
use crate::shape::{Feature, SyllabicGsub};
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;

/// Myanmar script-tag priority: `mym2` is the tag of fonts made for
/// the Myanmar shaping model, and `mymr` the older tag, which sends a
/// font to HarfBuzz's default shaper.
pub const MYANMAR_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"mym2", *b"mymr", *b"DFLT"];

/// The Myanmar features up to the basic ones: `locl` and `ccmp` before
/// the syllable reorder, then `rphf` (kinzi), `pref`, `blwf`, and
/// `pstf` after it, one stage each (HarfBuzz's
/// `myanmar_basic_features`).
pub const MYANMAR_BASIC_FEATURES: &[&[u8; 4]] =
    &[b"locl", b"ccmp", b"rphf", b"pref", b"blwf", b"pstf"];

/// The other Myanmar features, applied as one stage once the syllables
/// are done (HarfBuzz's `myanmar_other_features`).
pub const MYANMAR_TOPOGRAPHICAL_FEATURES: &[&[u8; 4]] = &[b"pres", b"abvs", b"blws", b"psts"];

/// `locl` and `ccmp`, which `collect_features_myanmar` enables with
/// `F_PER_SYLLABLE` before the reorder (`enable_feature` adds
/// `F_GLOBAL`).
const EARLY_FEATURES: [MapFeature; 2] = [
    MapFeature::new(b"locl", F::GLOBAL.union(F::PER_SYLLABLE)),
    MapFeature::new(b"ccmp", F::GLOBAL.union(F::PER_SYLLABLE)),
];

/// The basic features, with `F_MANUAL_ZWJ | F_PER_SYLLABLE`.
const BASIC_FEATURES: [MapFeature; 4] = [
    MapFeature::new(b"rphf", BASIC),
    MapFeature::new(b"pref", BASIC),
    MapFeature::new(b"blwf", BASIC),
    MapFeature::new(b"pstf", BASIC),
];
const BASIC: F = F::GLOBAL.union(F::MANUAL_ZWJ).union(F::PER_SYLLABLE);

/// The other features, with `F_MANUAL_ZWJ`.
const OTHER_FEATURES: [MapFeature; 4] = [
    MapFeature::new(b"pres", OTHER),
    MapFeature::new(b"abvs", OTHER),
    MapFeature::new(b"blws", OTHER),
    MapFeature::new(b"psts", OTHER),
];
const OTHER: F = F::GLOBAL.union(F::MANUAL_ZWJ);

/// The default GSUB features HarfBuzz runs in the Myanmar shaper's last
/// stage, for horizontal and for vertical text (`common_features`,
/// `horizontal_features`, and `vert` in `hb-ot-shape.cc`). `ccmp` and
/// `locl` ran earlier.
const DEFAULT_HORIZONTAL: [MapFeature; 5] = [
    MapFeature::new(b"rlig", F::GLOBAL),
    MapFeature::new(b"calt", F::GLOBAL),
    MapFeature::new(b"clig", F::GLOBAL),
    MapFeature::new(b"liga", F::GLOBAL),
    MapFeature::new(b"rclt", F::GLOBAL),
];
const DEFAULT_VERTICAL: [MapFeature; 2] = [
    MapFeature::new(b"rlig", F::GLOBAL),
    MapFeature::new(b"vert", F::GLOBAL),
];

/// Myanmar syllable types (`myanmar_syllable_type_t`).
pub(crate) mod syllable {
    /// A consonant syllable.
    pub(crate) const CONSONANT: u8 = 0;
    /// A broken cluster.
    pub(crate) const BROKEN: u8 = 1;
    /// A character outside any syllable.
    pub(crate) const NON_MYANMAR: u8 = 2;
}

/// What the Myanmar shaper needs besides the glyphs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MyanmarRun<'a> {
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
}

/// Shapes one Myanmar run: `codepoints` and `glyphs` are one to one on
/// entry. Runs every GSUB feature of the run, the default ones
/// included.
pub(crate) fn shape(run: &MyanmarRun<'_>, codepoints: &[char], glyphs: &mut Vec<Glyph>) {
    if codepoints.len() != glyphs.len() || glyphs.is_empty() {
        return;
    }
    let mut info: Vec<GlyphInfo> = codepoints
        .iter()
        .map(|&c| GlyphInfo {
            category: categories(c).0,
            ..GlyphInfo::default()
        })
        .collect();
    let cats: Vec<u8> = info.iter().map(|g| g.category).collect();
    set_syllables(&mut info, &machine::find_syllables(&cats));
    setup_syllables(glyphs, &info, run.level);

    let prio = MYANMAR_SCRIPT_PRIORITY;
    let mut runner = run
        .gsub
        .map(|gsub| SyllabicGsub::new(gsub, run.gdef, glyphs));
    if let Some(runner) = runner.as_mut() {
        let early = EARLY_FEATURES.map(|f| StageFeature::of(f, GLOBAL_MASK));
        apply_stage(runner, prio, &early, run.features, glyphs, &mut info);
    }

    if let Some(circle) = run.dotted_circle {
        let spec = DottedCircle {
            broken: syllable::BROKEN,
            category: cat::DOTTEDCIRCLE,
            position: 0,
            repha: None,
        };
        insert_dotted_circles(glyphs, &mut info, spec, circle);
    }
    for range in syllable_ranges(&info) {
        let kind = info
            .get(range.start)
            .map_or(syllable::NON_MYANMAR, |g| g.syllable_type());
        if matches!(kind, syllable::CONSONANT | syllable::BROKEN) {
            reorder::reorder_consonant_syllable(
                glyphs,
                &mut info,
                range.start,
                range.end,
                run.level,
            );
        }
    }

    let Some(runner) = runner.as_mut() else {
        return;
    };
    for feature in BASIC_FEATURES {
        let stage = [StageFeature::of(feature, GLOBAL_MASK)];
        apply_stage(runner, prio, &stage, run.features, glyphs, &mut info);
    }

    // HarfBuzz clears the syllables before the last stage, whose
    // features do not match per syllable.
    let defaults: &[MapFeature] = if run.vertical {
        &DEFAULT_VERTICAL
    } else {
        &DEFAULT_HORIZONTAL
    };
    let mut other: Vec<StageFeature> = OTHER_FEATURES
        .iter()
        .chain(defaults)
        .map(|&f| StageFeature::of(f, GLOBAL_MASK))
        .collect();
    let alternates = add_user_features(&mut other, run.features, earlier);
    apply_stage(runner, prio, &other, run.features, glyphs, &mut info);
    for (tag, value) in alternates {
        apply_alternate_feature(runner, prio, tag, value, glyphs);
    }
}

/// Tags the caller's features cannot add to the last stage: those of
/// the earlier stages.
fn earlier(tag: [u8; 4]) -> bool {
    EARLY_FEATURES
        .iter()
        .chain(&BASIC_FEATURES)
        .any(|f| f.tag == tag)
}

/// Entry point: shapes one Myanmar run with the Myanmar shaper, which
/// follows HarfBuzz's. `codepoints` is in one-to-one correspondence
/// with `glyphs` on entry. After the call `glyphs` may be shorter (GSUB
/// collapses) and reordered. Clusters track back to original byte
/// offsets so the caller can map glyphs to input. A reordered glyph
/// shares one cluster with the glyphs it moved across at the monotone
/// cluster `level`s, as in HarfBuzz's Myanmar shaper.
///
/// Every GSUB feature of the run runs here, the default ones (`rlig`,
/// `calt`, `clig`, `liga`, `rclt`) included, since HarfBuzz runs them
/// in the Myanmar shaper's last stage. Broken clusters get no dotted
/// circle here. Shaping through [`crate::shape`] adds them.
pub fn shape_myanmar(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    let run = MyanmarRun {
        gsub,
        gdef,
        level,
        features: &[],
        vertical: false,
        dotted_circle: None,
    };
    shape(&run, codepoints, glyphs);
}

#[cfg(test)]
mod tests;
