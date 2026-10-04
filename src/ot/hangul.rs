//! The GSUB side of HarfBuzz's Hangul shaper (`hb-ot-shaper-hangul.cc`).
//!
//! HarfBuzz runs every GSUB feature of a Hangul buffer as one stage,
//! each lookup once in lookup-index order: `ljmo`, `vjmo`, and `tjmo`
//! on the jamo the preprocessing gave each feature to
//! (`setup_masks_hangul`), the default features, and the caller's.
//! `calt` applies to every glyph but jamo (`override_features_hangul`
//! gives it a mask and `setup_masks_hangul` clears it on jamo, because
//! some fonts put all their jamo lookups in `calt`). Vertical text gets
//! `vert` instead of the horizontal defaults, and `calt` only when the
//! caller turns it on, still off jamo.
//!
//! The preprocessing itself (composition, decomposition, and the tone
//! marks) runs on the characters, before normalization, in
//! `crate::shape`.

use alloc::vec::Vec;

use super::syllabic::stage::{
    add_user_features, apply_alternate_feature, apply_stage, user_disabled, FeatureFlags as F,
    MapFeature, StageFeature, GLOBAL_MASK,
};
use super::syllabic::GlyphInfo;
use crate::buffer::Glyph;
use crate::shape::{Feature, SyllabicGsub};
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;

/// Hangul script tags, in the order HarfBuzz tries them.
pub(crate) const HANGUL_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"hang"];

/// HarfBuzz's `hangul_features`, added with no flags: each applies only
/// to the jamo the preprocessing marked for it.
pub(crate) const HANGUL_FEATURES: [MapFeature; 3] = [
    MapFeature::new(b"ljmo", F::NONE),
    MapFeature::new(b"vjmo", F::NONE),
    MapFeature::new(b"tjmo", F::NONE),
];

/// The default GSUB features of the Hangul shaper's stage for
/// horizontal text (`common_features` and `horizontal_features` in
/// `hb-ot-shape.cc`). `calt` is masked (see [`CALT`]).
const DEFAULT_HORIZONTAL: [MapFeature; 7] = [
    MapFeature::new(b"ccmp", F::GLOBAL),
    MapFeature::new(b"locl", F::GLOBAL),
    MapFeature::new(b"rlig", F::GLOBAL),
    MapFeature::new(b"calt", F::NONE),
    MapFeature::new(b"clig", F::GLOBAL),
    MapFeature::new(b"liga", F::GLOBAL),
    MapFeature::new(b"rclt", F::GLOBAL),
];

/// The default GSUB features of the stage for vertical text.
const DEFAULT_VERTICAL: [MapFeature; 4] = [
    MapFeature::new(b"ccmp", F::GLOBAL),
    MapFeature::new(b"locl", F::GLOBAL),
    MapFeature::new(b"rlig", F::GLOBAL),
    MapFeature::new(b"vert", F::GLOBAL),
];

/// Mask bits: one per jamo feature, in [`crate::shape`]'s jamo
/// numbering (1 `ljmo`, 2 `vjmo`, 3 `tjmo`), and one for `calt`.
const fn jamo_bit(feature: u8) -> u32 {
    1 << feature
}
const CALT: u32 = 1 << 4;

/// What the Hangul shaper needs besides the glyphs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HangulRun<'a> {
    /// The font's GSUB, if any.
    pub(crate) gsub: Option<&'a Gsub<'a>>,
    /// The font's GDEF, if any.
    pub(crate) gdef: Option<&'a Gdef<'a>>,
    /// The caller's feature overrides.
    pub(crate) features: &'a [Feature],
    /// True for vertical text.
    pub(crate) vertical: bool,
}

/// The jamo feature of a character (HarfBuzz's
/// `hangul_shaping_feature`): none, `ljmo`, `vjmo`, or `tjmo`.
pub(crate) mod jamo {
    /// No jamo feature.
    pub(crate) const NONE: u8 = 0;
    /// `ljmo`.
    pub(crate) const LJMO: u8 = 1;
    /// `vjmo`.
    pub(crate) const VJMO: u8 = 2;
    /// `tjmo`.
    pub(crate) const TJMO: u8 = 3;
}

/// A leading jamo (`isL`).
pub(crate) const fn is_l(ch: char) -> bool {
    matches!(ch as u32, 0x1100..=0x115F | 0xA960..=0xA97C)
}

/// A vowel jamo (`isV`).
pub(crate) const fn is_v(ch: char) -> bool {
    matches!(ch as u32, 0x1160..=0x11A7 | 0xD7B0..=0xD7C6)
}

/// A trailing jamo (`isT`).
pub(crate) const fn is_t(ch: char) -> bool {
    matches!(ch as u32, 0x11A8..=0x11FF | 0xD7CB..=0xD7FB)
}

/// True for a leading, vowel, or trailing jamo.
const fn is_jamo(ch: char) -> bool {
    is_l(ch) || is_v(ch) || is_t(ch)
}

/// The jamo features of `cps` read off the characters alone: every
/// leading jamo followed by a vowel jamo starts a syllable that did not
/// compose, with an optional trailing jamo. It gives the features the
/// Hangul preprocessing gives, except after a precomposed syllable the
/// font lacks that decomposed in front of a trailing jamo, which the
/// preprocessing leaves without a feature.
pub(crate) fn jamo_features(cps: &[char]) -> Vec<u8> {
    let mut out = alloc::vec![jamo::NONE; cps.len()];
    let mut i = 0;
    while i + 1 < cps.len() {
        if is_l(cps[i]) && is_v(cps[i + 1]) {
            out[i] = jamo::LJMO;
            out[i + 1] = jamo::VJMO;
            if cps.get(i + 2).is_some_and(|&c| is_t(c)) {
                out[i + 2] = jamo::TJMO;
                i += 3;
            } else {
                i += 2;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Runs the Hangul stage over one run: `codepoints`, `jamo` (each
/// character's jamo feature, 0 for none), and `glyphs` are one to one.
pub(crate) fn shape(
    run: &HangulRun<'_>,
    codepoints: &[char],
    jamo: &[u8],
    glyphs: &mut Vec<Glyph>,
) {
    let Some(gsub) = run.gsub else {
        return;
    };
    if codepoints.len() != glyphs.len() || jamo.len() != glyphs.len() || glyphs.is_empty() {
        return;
    }
    let mut info: Vec<GlyphInfo> = codepoints
        .iter()
        .zip(jamo)
        .map(|(&c, &f)| GlyphInfo {
            mask: if f == 0 { 0 } else { jamo_bit(f) } | if is_jamo(c) { 0 } else { CALT },
            ..GlyphInfo::default()
        })
        .collect();
    let mut runner = SyllabicGsub::new(gsub, run.gdef, glyphs);
    let defaults: &[MapFeature] = if run.vertical {
        &DEFAULT_VERTICAL
    } else {
        &DEFAULT_HORIZONTAL
    };
    let mut stage: Vec<StageFeature> = HANGUL_FEATURES
        .iter()
        .enumerate()
        .map(|(i, &f)| StageFeature::of(f, jamo_bit(i as u8 + 1)))
        .chain(defaults.iter().map(|&f| {
            let bit = if f.tag == *b"calt" { CALT } else { GLOBAL_MASK };
            StageFeature::of(f, bit)
        }))
        .collect();
    // In vertical text HarfBuzz's map only has the `calt` of
    // `override_features_hangul`, which has no value, so no glyph gets
    // its mask bit unless the caller turns `calt` on. Then every glyph
    // but the jamo gets it, as in horizontal text.
    let calt_on = run
        .features
        .iter()
        .any(|f| f.tag == *b"calt" && f.value != 0)
        && !user_disabled(run.features, *b"calt");
    if run.vertical && calt_on {
        stage.push(StageFeature::of(MapFeature::new(b"calt", F::NONE), CALT));
    }
    let excluded = if run.vertical {
        |tag: [u8; 4]| tag == *b"calt"
    } else {
        |_: [u8; 4]| false
    };
    let prio = HANGUL_SCRIPT_PRIORITY;
    let alternates = add_user_features(&mut stage, run.features, excluded);
    apply_stage(&mut runner, prio, &stage, run.features, glyphs, &mut info);
    for (tag, value) in alternates {
        apply_alternate_feature(&mut runner, prio, tag, value, glyphs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calt_stays_off_jamo() {
        assert!(is_jamo('\u{1100}'));
        assert!(is_jamo('\u{11A8}'));
        assert!(is_jamo('\u{D7B0}'));
        assert!(!is_jamo('\u{AC00}'));
        assert!(!is_jamo('\u{302E}'));
    }

    #[test]
    fn feature_table_matches_harfbuzz() {
        let tags: Vec<[u8; 4]> = HANGUL_FEATURES.iter().map(|f| f.tag).collect();
        assert_eq!(tags, [*b"ljmo", *b"vjmo", *b"tjmo"]);
        assert!(HANGUL_FEATURES.iter().all(|f| f.flags == F::NONE));
        assert_eq!(StageFeature::of(HANGUL_FEATURES[1], jamo_bit(2)).mask, 4);
    }
}
