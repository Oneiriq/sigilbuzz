//! The Khmer shaper, following HarfBuzz's (`hb-ot-shaper-khmer.cc`).
//!
//! HarfBuzz gives Khmer a shaper of its own, outside the Universal
//! Shaping Engine:
//!
//! 1. Every character gets its Khmer category from the Indic-family
//!    table (`set_khmer_properties`), and the syllable machine of
//!    `hb-ot-shaper-khmer-machine.rl` splits the run into consonant
//!    syllables, broken clusters, and other characters
//!    (`setup_syllables_khmer`).
//! 2. Broken clusters get a dotted circle, and each consonant syllable
//!    (and broken cluster) is reordered before any lookup runs
//!    (`reorder_khmer`). The glyphs after the first get the `blwf`,
//!    `abvf`, and `pstf` masks. A coeng + ro pair moves to the start of
//!    the syllable with the `pref` mask, and the glyphs after it get
//!    `cfar`. A pre-base vowel sign moves to the start too. Each move
//!    merges the clusters it passes.
//! 3. `locl`, `ccmp`, and the basic features `pref`, `blwf`, `abvf`,
//!    `pstf`, and `cfar` run as one stage, each masked and matching
//!    one syllable at a time ([`KHMER_FEATURES`] keeps HarfBuzz's
//!    flags).
//! 4. The other features `pres`, `abvs`, `blws`, and `psts` run as one
//!    stage with the default features HarfBuzz puts in the same stage
//!    (`rlig`, `calt`, `clig`, `rclt`, or `vert` in vertical text) and
//!    the caller's features. HarfBuzz turns `liga` off for Khmer and
//!    `clig` on (`override_features_khmer`).

mod machine;

use alloc::vec::Vec;

use super::syllabic::stage::{
    add_user_features, apply_alternate_feature, apply_stage, has_feature, FeatureFlags as F,
    MapFeature, StageFeature, GLOBAL_MASK,
};
use super::syllabic::{
    cat, categories, insert_dotted_circles, merge_clusters, set_syllables, syllable_ranges,
    DottedCircle, GlyphInfo,
};
use crate::buffer::{ClusterLevel, Glyph};
use crate::shape::{Feature, SyllabicGsub};
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;

/// Khmer script tags, in the order HarfBuzz tries them.
pub(crate) const KHMER_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"khmr"];

/// HarfBuzz's `khmer_features`, with their flags: the basic features
/// (masked, manual joiners, per syllable) and the other features
/// (global, manual joiners).
pub(crate) const KHMER_FEATURES: [MapFeature; 9] = [
    MapFeature::new(b"pref", F::MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"blwf", F::MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"abvf", F::MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"pstf", F::MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"cfar", F::MANUAL_JOINERS.union(F::PER_SYLLABLE)),
    MapFeature::new(b"pres", F::GLOBAL_MANUAL_JOINERS),
    MapFeature::new(b"abvs", F::GLOBAL_MANUAL_JOINERS),
    MapFeature::new(b"blws", F::GLOBAL_MANUAL_JOINERS),
    MapFeature::new(b"psts", F::GLOBAL_MANUAL_JOINERS),
];

/// How many of [`KHMER_FEATURES`] are basic features.
const KHMER_BASIC_FEATURES: usize = 5;

/// `locl` and `ccmp`, which the Khmer shaper enables with
/// `F_PER_SYLLABLE` in the basic stage (`enable_feature` adds
/// `F_GLOBAL`).
const KHMER_EARLY_FEATURES: [MapFeature; 2] = [
    MapFeature::new(b"locl", F::GLOBAL.union(F::PER_SYLLABLE)),
    MapFeature::new(b"ccmp", F::GLOBAL.union(F::PER_SYLLABLE)),
];

/// The default GSUB features HarfBuzz runs in the Khmer shaper's last
/// stage, for horizontal and for vertical text (`common_features`,
/// `horizontal_features`, and `vert` in `hb-ot-shape.cc`). `liga` is
/// off, and `ccmp` and `locl` ran earlier.
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

/// The mask bit of feature `i` of [`KHMER_FEATURES`].
const fn bit(i: usize) -> u32 {
    1 << i
}
const PREF: u32 = bit(0);
const BLWF: u32 = bit(1);
const ABVF: u32 = bit(2);
const PSTF: u32 = bit(3);
const CFAR: u32 = bit(4);

/// Khmer syllable types (`khmer_syllable_type_t`).
pub(crate) mod syllable {
    /// A consonant syllable.
    pub(crate) const CONSONANT: u8 = 0;
    /// A broken cluster.
    pub(crate) const BROKEN: u8 = 1;
    /// A character outside any syllable.
    pub(crate) const NON_KHMER: u8 = 2;
}

/// What the Khmer shaper needs besides the glyphs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct KhmerRun<'a> {
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

/// Shapes one Khmer run: `codepoints` and `glyphs` are one to one on
/// entry. Runs every GSUB feature of the run, the default ones
/// included.
pub(crate) fn shape(run: &KhmerRun<'_>, codepoints: &[char], glyphs: &mut Vec<Glyph>) {
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

    let prio = KHMER_SCRIPT_PRIORITY;
    let mut runner = run
        .gsub
        .map(|gsub| SyllabicGsub::new(gsub, run.gdef, glyphs));
    let masks = Masks {
        cfar: runner
            .as_ref()
            .is_some_and(|r| has_feature(r, run.features, *b"cfar", prio)),
    };
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
            .map_or(syllable::NON_KHMER, |g| g.syllable_type());
        if matches!(kind, syllable::CONSONANT | syllable::BROKEN) {
            reorder_consonant_syllable(glyphs, &mut info, range, masks, run.level);
        }
    }

    let Some(runner) = runner.as_mut() else {
        return;
    };
    let basic: Vec<StageFeature> = KHMER_EARLY_FEATURES
        .iter()
        .map(|&f| StageFeature::of(f, GLOBAL_MASK))
        .chain(
            KHMER_FEATURES[..KHMER_BASIC_FEATURES]
                .iter()
                .enumerate()
                .map(|(i, &f)| StageFeature::of(f, bit(i))),
        )
        .collect();
    apply_stage(runner, prio, &basic, run.features, glyphs, &mut info);

    // HarfBuzz clears the syllables before the last stage.
    let defaults: &[MapFeature] = if run.vertical {
        &DEFAULT_VERTICAL
    } else {
        &DEFAULT_HORIZONTAL
    };
    let mut other: Vec<StageFeature> = KHMER_FEATURES[KHMER_BASIC_FEATURES..]
        .iter()
        .chain(defaults)
        .map(|&f| StageFeature::of(f, GLOBAL_MASK))
        .collect();
    let alternates = add_user_features(&mut other, run.features, early_or_off);
    apply_stage(runner, prio, &other, run.features, glyphs, &mut info);
    for (tag, value) in alternates {
        apply_alternate_feature(runner, prio, tag, value, glyphs);
    }
}

/// Which optional masks the font has features for.
#[derive(Debug, Clone, Copy)]
struct Masks {
    cfar: bool,
}

/// HarfBuzz's `reorder_consonant_syllable` for the syllable at
/// `range`.
fn reorder_consonant_syllable(
    glyphs: &mut [Glyph],
    info: &mut [GlyphInfo],
    range: core::ops::Range<usize>,
    masks: Masks,
    level: ClusterLevel,
) {
    let (start, end) = (range.start, range.end);
    if end > glyphs.len() || end > info.len() || start >= end {
        return;
    }
    for g in &mut info[start + 1..end] {
        g.mask |= BLWF | ABVF | PSTF;
    }
    let mut num_coengs = 0;
    let mut i = start + 1;
    while i < end {
        if info[i].category == cat::H && num_coengs <= 2 && i + 1 < end {
            num_coengs += 1;
            if info[i + 1].category == cat::RA {
                info[i].mask |= PREF;
                info[i + 1].mask |= PREF;
                // Move the coeng, ro pair to the start.
                merge_clusters(glyphs, start, i + 2, level);
                glyphs[start..i + 2].rotate_right(2);
                info[start..i + 2].rotate_right(2);
                if masks.cfar {
                    for g in &mut info[i + 2..end] {
                        g.mask |= CFAR;
                    }
                }
                num_coengs = 2;
            }
        } else if info[i].category == cat::VPRE {
            // Move the pre-base vowel sign to the start.
            merge_clusters(glyphs, start, i + 1, level);
            glyphs[start..=i].rotate_right(1);
            info[start..=i].rotate_right(1);
        }
        i += 1;
    }
}

/// Tags the caller's features cannot add to the last stage: those of
/// the basic stage, and `liga`, which the Khmer shaper turns off.
fn early_or_off(tag: [u8; 4]) -> bool {
    tag == *b"liga"
        || KHMER_FEATURES[..KHMER_BASIC_FEATURES]
            .iter()
            .chain(&KHMER_EARLY_FEATURES)
            .any(|f| f.tag == tag)
}

#[cfg(test)]
mod tests;
