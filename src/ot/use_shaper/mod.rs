//! The Universal Shaping Engine (USE), following HarfBuzz's
//! (`hb-ot-shaper-use.cc`).
//!
//! The USE is Microsoft's generic complex-script shaper. HarfBuzz
//! sends it every script `hb_ot_shaper_categorize` does not give a
//! shaper of its own: Sinhala, Balinese, Cham, Tai Tham, the
//! Brahmi-family historical scripts, and many more. A run goes through
//! these steps:
//!
//! 1. Every character gets its USE category from the table generated
//!    from the Unicode Character Database (`hb_use_get_category`,
//!    see the `category` module), and the syllable machine of
//!    `hb-ot-shaper-use-machine.rl` splits the run into clusters (the
//!    `machine` module). Each cluster is unsafe to break.
//!    `setup_rphf_mask` lets `rphf` apply to the start of each
//!    cluster. When the font has the `isol`, `init`, `medi`, or `fina`
//!    features, each character gets the mask of its Arabic-style
//!    joining form in the scripts that join that way (N'Ko, Mongolian),
//!    and the clusters of the other scripts join each other
//!    (`setup_topographical_masks`).
//! 2. `locl`, `ccmp`, `nukt`, and `akhn` run as one stage, one cluster
//!    at a time (`USE_FEATURES` keeps HarfBuzz's flags).
//! 3. `rphf` runs, and the glyph it substitutes becomes a repha
//!    (`record_rphf_use`). `pref` runs, and the glyph it substitutes
//!    becomes a pre-base vowel sign (`record_pref_use`).
//! 4. `rkrf`, `abvf`, `blwf`, `half`, `pstf`, `vatu`, and `cjct` run
//!    as one stage, one cluster at a time.
//! 5. Broken clusters get a dotted circle, after a leading repha. In
//!    each cluster a repha moves toward the end and the pre-base vowel
//!    signs move to the start (`reorder_use`). Each move merges the
//!    clusters it passes.
//! 6. `isol`, `init`, `medi`, and `fina` run as one stage, on the
//!    glyphs of their masks.
//! 7. `abvs`, `blws`, `haln`, `pres`, and `psts` run as one stage with
//!    the default features HarfBuzz puts in the same stage (`rlig`,
//!    `calt`, `clig`, `liga`, `rclt`, or `vert` in vertical text) and
//!    the caller's features.
//!
//! GPOS runs in the generic pipeline in [`crate::shape`].
//!
//! HarfBuzz checks the vowel constraints of
//! `hb-ot-shaper-vowel-constraints.cc` before all of this, before
//! normalization (`preprocess_text_use`). sigilbuzz does not insert
//! those dotted circles yet. Their place is the preprocessing in
//! [`crate::shape`], next to the Thai and Hangul preprocessing.
//!
//! Khmer and Myanmar have shapers of their own (`crate::ot::khmer`,
//! `crate::ot::myanmar`), as in HarfBuzz, and [`shape_khmer`] and
//! [`shape_myanmar`] run them.

mod category;
mod machine;
mod reorder;
mod scripts;
#[rustfmt::skip]
mod table;

use alloc::vec::Vec;

pub use crate::ot::myanmar::{
    shape_myanmar, MYANMAR_BASIC_FEATURES, MYANMAR_SCRIPT_PRIORITY, MYANMAR_TOPOGRAPHICAL_FEATURES,
};
pub use scripts::{
    shape_balinese, shape_brahmi, shape_buginese, shape_cham, shape_hangul, shape_khojki,
    shape_lepcha, shape_limbu, shape_modi, shape_nko, shape_nko_in_context, shape_sharada,
    shape_sundanese, shape_tai_tham, shape_tirhuta,
};
pub(crate) use scripts::{shape_script, shape_use, UseScript};

use self::machine::{find_syllables, syllable};
use crate::buffer::{ClusterLevel, Glyph};
use crate::ot::arabic::JoiningForm;
use crate::ot::syllabic::stage::{
    add_user_features, apply_alternate_feature, apply_stage, has_feature, FeatureFlags as F,
    MapFeature, StageFeature, GLOBAL_MASK,
};
use crate::ot::syllabic::{insert_dotted_circles, setup_syllables, DottedCircle, GlyphInfo};
use crate::shape::{Feature, SyllabicGsub};
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;

/// Script-tag priority for USE GSUB / GPOS feature lookup.
///
/// Khmer fonts advertise their features under `khmr`, and HarfBuzz
/// also accepts `khm2` on fonts built against the 2005+ Indic2
/// revision. DFLT falls through for fonts that register features only
/// in the default LangSys (rare for Khmer but cheap to probe).
pub const KHMER_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"khmr", *b"khm2", *b"DFLT"];

/// Thai script-tag priority.
pub const THAI_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"thai", *b"DFLT"];

/// Lao script-tag priority. The OpenType tag is `lao ` with a
/// trailing space. The 4-byte tag convention is padded that way.
pub const LAO_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"lao ", *b"DFLT"];

/// Hangul script-tag priority. Old Hangul fonts register their
/// `ljmo`/`vjmo`/`tjmo` features under `hang`, and `jamo` is the
/// legacy tag that a few fonts still emit.
pub const HANGUL_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"hang", *b"jamo", *b"DFLT"];

/// N'Ko script tag: `nko ` (trailing space) is the canonical
/// OpenType tag for N'Ko. No v2 form.
pub const NKO_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"nko ", *b"DFLT"];

/// Buginese (Lontara) script tag.
pub const BUGINESE_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"bugi", *b"DFLT"];

/// Tai Tham (Lanna) script tag.
pub const TAI_THAM_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"lana", *b"DFLT"];

/// Balinese script tag: `bali` is the only OT tag in current use.
pub const BALINESE_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"bali", *b"DFLT"];

/// Sundanese script tag.
pub const SUNDANESE_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"sund", *b"DFLT"];

/// Lepcha script tag.
pub const LEPCHA_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"lepc", *b"DFLT"];

/// Limbu script tag.
pub const LIMBU_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"limb", *b"DFLT"];

/// Cham script tag.
pub const CHAM_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"cham", *b"DFLT"];

/// Brahmi script tag: `brah` is the only OT tag in current use.
pub const BRAHMI_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"brah", *b"DFLT"];

/// Sharada script tag.
pub const SHARADA_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"shrd", *b"DFLT"];

/// Khojki script tag.
pub const KHOJKI_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"khoj", *b"DFLT"];

/// Tirhuta script tag.
pub const TIRHUTA_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"tirh", *b"DFLT"];

/// Modi script tag.
pub const MODI_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"modi", *b"DFLT"];

/// The USE features up to the reorder (HarfBuzz's
/// `collect_features_use`), in order: the default glyph
/// pre-processing group (`locl`, `ccmp`, `nukt`, `akhn`), the
/// reordering group (`rphf`, then `pref`), and the orthographic unit
/// shaping group (`rkrf` to `cjct`).
pub const USE_BASIC_FEATURES: &[&[u8; 4]] = &[
    b"locl", b"ccmp", b"nukt", b"akhn", b"rphf", b"pref", b"rkrf", b"abvf", b"blwf", b"half",
    b"pstf", b"vatu", b"cjct",
];

/// The USE features after the reorder: the joining forms
/// (`use_topographical_features`), then the standard typographic
/// presentation features (`use_other_features`).
pub const USE_TOPOGRAPHICAL_FEATURES: &[&[u8; 4]] = &[
    b"isol", b"init", b"medi", b"fina", b"abvs", b"blws", b"haln", b"pres", b"psts",
];

/// HarfBuzz's USE features with their flags, in the order
/// `collect_features_use` adds them. The stages split after `akhn`,
/// `rphf`, `pref`, `cjct`, and `fina`.
pub(crate) const USE_FEATURES: [MapFeature; 22] = [
    MapFeature::new(b"locl", F::GLOBAL.union(F::PER_SYLLABLE)),
    MapFeature::new(b"ccmp", F::GLOBAL.union(F::PER_SYLLABLE)),
    MapFeature::new(b"nukt", F::GLOBAL.union(F::PER_SYLLABLE)),
    MapFeature::new(b"akhn", MANUAL_ZWJ_PER_SYLLABLE.union(F::GLOBAL)),
    MapFeature::new(b"rphf", MANUAL_ZWJ_PER_SYLLABLE),
    MapFeature::new(b"pref", MANUAL_ZWJ_PER_SYLLABLE.union(F::GLOBAL)),
    MapFeature::new(b"rkrf", MANUAL_ZWJ_PER_SYLLABLE.union(F::GLOBAL)),
    MapFeature::new(b"abvf", MANUAL_ZWJ_PER_SYLLABLE.union(F::GLOBAL)),
    MapFeature::new(b"blwf", MANUAL_ZWJ_PER_SYLLABLE.union(F::GLOBAL)),
    MapFeature::new(b"half", MANUAL_ZWJ_PER_SYLLABLE.union(F::GLOBAL)),
    MapFeature::new(b"pstf", MANUAL_ZWJ_PER_SYLLABLE.union(F::GLOBAL)),
    MapFeature::new(b"vatu", MANUAL_ZWJ_PER_SYLLABLE.union(F::GLOBAL)),
    MapFeature::new(b"cjct", MANUAL_ZWJ_PER_SYLLABLE.union(F::GLOBAL)),
    MapFeature::new(b"isol", F::NONE),
    MapFeature::new(b"init", F::NONE),
    MapFeature::new(b"medi", F::NONE),
    MapFeature::new(b"fina", F::NONE),
    MapFeature::new(b"abvs", F::GLOBAL.union(F::MANUAL_ZWJ)),
    MapFeature::new(b"blws", F::GLOBAL.union(F::MANUAL_ZWJ)),
    MapFeature::new(b"haln", F::GLOBAL.union(F::MANUAL_ZWJ)),
    MapFeature::new(b"pres", F::GLOBAL.union(F::MANUAL_ZWJ)),
    MapFeature::new(b"psts", F::GLOBAL.union(F::MANUAL_ZWJ)),
];

/// `F_MANUAL_ZWJ | F_PER_SYLLABLE`.
const MANUAL_ZWJ_PER_SYLLABLE: F = F::MANUAL_ZWJ.union(F::PER_SYLLABLE);

/// The stages of [`USE_FEATURES`]: the ranges of features each runs.
const EARLY: core::ops::Range<usize> = 0..4;
const RPHF_STAGE: core::ops::Range<usize> = 4..5;
const PREF_STAGE: core::ops::Range<usize> = 5..6;
const BASIC: core::ops::Range<usize> = 6..13;
const TOPOGRAPHICAL: core::ops::Range<usize> = 13..17;
const OTHER: core::ops::Range<usize> = 17..22;

/// The `rphf` mask bit.
const RPHF: u32 = 1;
/// The mask bits of `isol`, `init`, `medi`, and `fina`.
const TOPOGRAPHICAL_BITS: [u32; 4] = [1 << 1, 1 << 2, 1 << 3, 1 << 4];

/// The default GSUB features HarfBuzz runs in the USE shaper's last
/// stage, for horizontal and for vertical text (`common_features`,
/// `horizontal_features`, and `vert` in `hb-ot-shape.cc`). `ccmp` and
/// `locl` ran in the first stage.
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

/// What the USE shaper needs besides the glyphs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct UseRun<'a> {
    /// The font's GSUB, if any.
    pub(crate) gsub: Option<&'a Gsub<'a>>,
    /// The font's GDEF, if any.
    pub(crate) gdef: Option<&'a Gdef<'a>>,
    /// The script tags the run's lookups try.
    pub(crate) script_priority: &'a [[u8; 4]],
    /// The buffer's cluster level.
    pub(crate) level: ClusterLevel,
    /// The caller's feature overrides.
    pub(crate) features: &'a [Feature],
    /// True for vertical text.
    pub(crate) vertical: bool,
    /// The dotted circle glyph for broken clusters, or `None` when the
    /// font has none or the buffer asks for no dotted circles.
    pub(crate) dotted_circle: Option<u16>,
    /// The joining form of each character, for a script with
    /// Arabic-style joining (HarfBuzz's `has_arabic_joining`), whose
    /// forms pick the topographical features. `None` for the other
    /// scripts, whose clusters join each other instead.
    pub(crate) joining: Option<&'a [JoiningForm]>,
}

/// Shapes one run with the Universal Shaping Engine: `codepoints` and
/// `glyphs` are one to one on entry. Runs every GSUB feature of the
/// run, the default ones included.
pub(crate) fn shape(run: &UseRun<'_>, codepoints: &[char], glyphs: &mut Vec<Glyph>) {
    if codepoints.len() != glyphs.len() || glyphs.is_empty() {
        return;
    }
    // `setup_masks_use` and `setup_syllables_use`.
    let mut info: Vec<GlyphInfo> = codepoints
        .iter()
        .map(|&c| GlyphInfo {
            category: category::category(c),
            ..GlyphInfo::default()
        })
        .collect();
    find_syllables(codepoints, &mut info);
    setup_syllables(glyphs, &info, run.level);

    let prio = run.script_priority;
    let mut runner = run
        .gsub
        .map(|gsub| SyllabicGsub::new(gsub, run.gdef, glyphs));
    let has = |tag: [u8; 4]| {
        runner
            .as_ref()
            .is_some_and(|r| has_feature(r, run.features, tag, prio))
    };
    let has_rphf = has(*b"rphf");
    if has_rphf {
        reorder::setup_rphf_mask(&mut info, RPHF);
    }
    // A topographical feature the caller turned on everywhere is
    // global, so it gets no mask of its own.
    let mut masks = [0u32; 4];
    for (k, f) in USE_FEATURES[TOPOGRAPHICAL].iter().enumerate() {
        if has(f.tag) && !user_enabled(run.features, f.tag) {
            masks[k] = TOPOGRAPHICAL_BITS[k];
        }
    }
    match run.joining {
        // `setup_masks_arabic_plan`.
        Some(forms) => {
            for (g, &form) in info.iter_mut().zip(forms) {
                let k = match form {
                    JoiningForm::Isol => 0,
                    JoiningForm::Init => 1,
                    JoiningForm::Medi => 2,
                    JoiningForm::Fina => 3,
                    JoiningForm::None => continue,
                };
                g.mask |= masks[k];
            }
        }
        None => reorder::setup_topographical_masks(&mut info, masks),
    }

    if let Some(runner) = runner.as_mut() {
        let stage = |range: core::ops::Range<usize>, bit: u32| -> Vec<StageFeature> {
            USE_FEATURES[range]
                .iter()
                .map(|&f| StageFeature::of(f, bit))
                .collect()
        };
        apply_stage(
            runner,
            prio,
            &stage(EARLY, 0),
            run.features,
            glyphs,
            &mut info,
        );
        clear_substitution_flags(&mut info);
        let rphf = stage(RPHF_STAGE, RPHF);
        apply_stage(runner, prio, &rphf, run.features, glyphs, &mut info);
        if has_rphf {
            reorder::record_rphf(&mut info, RPHF);
        }
        clear_substitution_flags(&mut info);
        let pref = stage(PREF_STAGE, 0);
        apply_stage(runner, prio, &pref, run.features, glyphs, &mut info);
        reorder::record_pref(&mut info);
        apply_stage(
            runner,
            prio,
            &stage(BASIC, 0),
            run.features,
            glyphs,
            &mut info,
        );
    }

    // `reorder_use`.
    if let Some(circle) = run.dotted_circle {
        let spec = DottedCircle {
            broken: syllable::BROKEN,
            category: category::B,
            position: 0,
            repha: Some(category::R),
        };
        insert_dotted_circles(glyphs, &mut info, spec, circle);
    }
    reorder::reorder(glyphs, &mut info, run.level);

    let Some(runner) = runner.as_mut() else {
        return;
    };
    let topographical: Vec<StageFeature> = USE_FEATURES[TOPOGRAPHICAL]
        .iter()
        .zip(TOPOGRAPHICAL_BITS)
        .map(|(&f, bit)| {
            if user_enabled(run.features, f.tag) {
                StageFeature::of(MapFeature::new(&f.tag, F::GLOBAL), GLOBAL_MASK)
            } else {
                StageFeature::of(f, bit)
            }
        })
        .collect();
    apply_stage(
        runner,
        prio,
        &topographical,
        run.features,
        glyphs,
        &mut info,
    );

    let defaults: &[MapFeature] = if run.vertical {
        &DEFAULT_VERTICAL
    } else {
        &DEFAULT_HORIZONTAL
    };
    let mut other: Vec<StageFeature> = USE_FEATURES[OTHER]
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

/// HarfBuzz's `_hb_clear_substitution_flags`.
fn clear_substitution_flags(info: &mut [GlyphInfo]) {
    for g in info {
        g.substituted = false;
    }
}

/// True when the caller turned `tag` on for the whole run.
fn user_enabled(features: &[Feature], tag: [u8; 4]) -> bool {
    features
        .iter()
        .rev()
        .find(|f| f.tag == tag)
        .is_some_and(|f| f.value != 0)
}

/// Tags the caller's features cannot add to the last stage: those of
/// the earlier stages.
fn earlier(tag: [u8; 4]) -> bool {
    USE_FEATURES[..OTHER.start].iter().any(|f| f.tag == tag)
}

/// Entry point: shapes one Khmer run with the Khmer shaper
/// (`crate::ot::khmer`), which follows HarfBuzz's. `codepoints` is in
/// one-to-one correspondence with `glyphs` on entry. After the call
/// `glyphs` may be shorter (GSUB collapses) and reordered. Clusters
/// track back to original byte offsets so the caller can map glyphs
/// to input. A reordered glyph shares one cluster with the glyphs it
/// moved across at the monotone cluster `level`s, as in HarfBuzz's
/// Khmer shaper.
///
/// Every GSUB feature of the run runs here, the default ones
/// (`rlig`, `calt`, `clig`, `rclt`) included, since HarfBuzz runs
/// them in the Khmer shaper's last stage. Broken clusters get no
/// dotted circle here. Shaping through [`crate::shape`] adds them.
pub fn shape_khmer(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    let run = crate::ot::khmer::KhmerRun {
        gsub,
        gdef,
        level,
        features: &[],
        vertical: false,
        dotted_circle: None,
    };
    crate::ot::khmer::shape(&run, codepoints, glyphs);
}

/// Hangul Old-Hangul features: the three positional jamo features
/// pick Leading/Vowel/Trailing variant shapes. HarfBuzz's Hangul
/// shaper adds only these to the default features and runs them all
/// in one stage with the defaults (see [`shape_hangul`]).
pub const HANGUL_FEATURES: &[&[u8; 4]] = &[b"ljmo", b"vjmo", b"tjmo"];

#[cfg(test)]
mod tests;
