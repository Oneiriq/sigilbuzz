//! Universal Shaping Engine (USE).
//!
//! The USE is Microsoft's generic complex-script shaper, the one
//! every SE-Asian, SE-Indic and archaic-South-Asian script that does
//! not fit Arabic or Indic2 runs through. Tai Tham, Buginese, New Tai
//! Lue, Cham, and Hanifi Rohingya are USE clients, and sigilbuzz also
//! runs its Myanmar and Old Hangul passes through this module. Khmer
//! has its own shaper (`crate::ot::khmer`), as in HarfBuzz, and
//! [`shape_khmer`] runs it. Each script gets an entry point below that
//! pairs its script-tag priority with a feature chain. A new script
//! needs its codepoints in the per-codepoint tables of
//! [`crate::unicode::use_category`] and an entry point here.
//!
//! # Pipeline
//!
//! 1. **Categorize** every codepoint in the run via
//!    [`use_category`](crate::unicode::use_category::use_category)
//!    and [`use_position`](crate::unicode::use_category::use_position).
//! 2. **Segment** into USE syllables. The grammar (simplified to the
//!    shape Khmer actually emits) is:
//!
//!    ```text
//!      R? (B | GB | IV) (H B)* VPre* VAbv* VBlw* VPst* M* FM*
//!    ```
//!
//!    Non-matching codepoints emit a one-wide Symbol/Broken syllable
//!    so the segmenter always makes progress.
//! 3. **Basic features**, on the logical order, applied via the GSUB
//!    dispatcher in the script's tag order. Order:
//!
//!    ```text
//!      locl -> ccmp -> nukt -> akhn -> rphf -> pref -> rkrf -> abvf
//!           -> blwf -> half -> pstf -> vatu -> cjct
//!    ```
//!
//! 4. **Reorder** each syllable in place, as HarfBuzz's `reorder_use`
//!    does after the basic features: every pre-base vowel sign (VPre),
//!    and the glyph `pref` substituted, moves to the start of the
//!    syllable or to just after the last halant before it.
//!
//!    Myanmar reorders before its features instead (its HarfBuzz
//!    shaper does), with the kinzi move.
//! 5. **Topographical features**, run after the reorder:
//!
//!    ```text
//!      abvs -> blws -> haln -> pres -> psts
//!    ```
//!
//! 6. **GPOS**: the generic pipeline in [`crate::shape`] runs the
//!    standard kern/mark/mkmk and `dist`. This module returns control
//!    to it after topographical GSUB.
//!
//! # Clusters
//!
//! Every reorder moves glyphs with their clusters. At the monotone
//! cluster levels a moved glyph and the glyphs it moved across then
//! share their smallest cluster, the `merge_clusters` calls of
//! HarfBuzz's Myanmar and USE reorderings. The other levels leave
//! the clusters out of order. Ligatures merge in the GSUB
//! dispatcher and graphemes before shaping starts, both by the same
//! level, so no syllable-wide merge happens here.

mod reorder;
mod scripts;
mod syllable;

use alloc::vec::Vec;

use reorder::{record_pref, record_rphf, reorder_pre_base, rphf_info, tag_syllables};
pub use scripts::{
    shape_balinese, shape_brahmi, shape_buginese, shape_cham, shape_hangul, shape_khojki,
    shape_lepcha, shape_limbu, shape_modi, shape_myanmar, shape_nko, shape_nko_in_context,
    shape_sharada, shape_sundanese, shape_tai_tham, shape_tirhuta,
};
pub(crate) use syllable::{segment_syllables, Syllable, SyllableKind};

use crate::buffer::{ClusterLevel, Glyph};
use crate::ot::syllabic::stage::{apply_stage, FeatureFlags, StageFeature};
use crate::shape::{apply_gsub_feature_in_scripts, JoinerTable, SyllabicGsub};
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;

/// Script-tag priority for USE GSUB / GPOS feature lookup.
///
/// Khmer fonts advertise their USE features under `khmr` (Indic2
/// tag) and the legacy `khmr` form is identical; HarfBuzz also
/// accepts `khm2` on fonts built against the 2005+ Indic2 revision.
/// DFLT falls through for fonts that register features only in the
/// default LangSys (rare for Khmer but cheap to probe).
pub const KHMER_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"khmr", *b"khm2", *b"DFLT"];

/// Myanmar script-tag priority: `mym2` is the Indic2 (2012+) tag
/// that modern Noto / Padauk builds use; `mymr` is the legacy tag
/// that older fonts still carry.
pub const MYANMAR_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"mym2", *b"mymr", *b"DFLT"];

/// Thai script-tag priority.
pub const THAI_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"thai", *b"DFLT"];

/// Lao script-tag priority. The OpenType tag is `lao ` with a
/// trailing space. The 4-byte tag convention is padded that way.
pub const LAO_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"lao ", *b"DFLT"];

/// Hangul script-tag priority. Old Hangul fonts register their
/// `ljmo`/`vjmo`/`tjmo` features under `hang`; `jamo` is the legacy
/// tag that a few fonts still emit.
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

/// USE features up to the reorder (HarfBuzz's `collect_features_use`),
/// in order: the default glyph pre-processing group (`locl`, `ccmp`,
/// `nukt`, `akhn`), the reordering group (`rphf`, then `pref`), and the
/// orthographic unit shaping group (`rkrf` to `cjct`). Order matters:
/// `rphf` must run before `half` so the ra+halant that would otherwise
/// fold into a half-form is consumed as a reph first.
///
/// HarfBuzz's topographical `isol`/`init`/`medi`/`fina` only reach the
/// USE scripts with Arabic-style joining, which sigilbuzz shapes with
/// their own joining passes (N'Ko, Mongolian).
pub const USE_BASIC_FEATURES: &[&[u8; 4]] = &[
    b"locl", b"ccmp", b"nukt", b"akhn", b"rphf", b"pref", b"rkrf", b"abvf", b"blwf", b"half",
    b"pstf", b"vatu", b"cjct",
];

/// USE topographical features: run after basic substitutions have
/// collapsed conjuncts into display forms.
pub const USE_TOPOGRAPHICAL_FEATURES: &[&[u8; 4]] = &[b"abvs", b"blws", b"haln", b"pres", b"psts"];

/// Myanmar's features up to the basic ones: `locl` and `ccmp` before
/// the syllable reorder, then `rphf` (kinzi), `pref`, `blwf`, and
/// `pstf` after it (HarfBuzz's `myanmar_basic_features`).
pub const MYANMAR_BASIC_FEATURES: &[&[u8; 4]] =
    &[b"locl", b"ccmp", b"rphf", b"pref", b"blwf", b"pstf"];

/// Myanmar's other features, applied together once the syllables are
/// done (HarfBuzz's `myanmar_other_features`).
pub const MYANMAR_TOPOGRAPHICAL_FEATURES: &[&[u8; 4]] = &[b"pres", b"abvs", b"blws", b"psts"];

/// Hangul Old-Hangul features: the three positional jamo features
/// pick Leading/Vowel/Trailing variant shapes. HarfBuzz's Hangul
/// shaper adds only these to the default features, which run once,
/// in the default pass.
pub const HANGUL_FEATURES: &[&[u8; 4]] = &[b"ljmo", b"vjmo", b"tjmo"];

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

/// Generic USE shaping entry point, used by Old-Hangul and the
/// Universal Shaping Engine scripts. Takes the script-priority table
/// and the (basic, topographical) feature slices as parameters so each
/// script can supply its own set. The syllable segmenter and pre-base
/// reorder are script-agnostic: they run off the [`UseCategory`] /
/// [`UsePosition`] tables which already encode per-script positional
/// rules.
///
/// As in HarfBuzz's USE shaper, the basic features see the logical
/// order: the pre-base vowel signs, and the glyph `pref` substitutes in
/// each syllable, move in front of their base only after them
/// (`reorder_prebase`; Old Hangul has nothing to reorder, so it passes
/// `false`).
///
/// `level` is the buffer's cluster level, which decides whether
/// reordered glyphs merge clusters.
///
/// `table` is the joiner handling of the HarfBuzz shaper the script
/// maps to (USE, or the default shaper for Hangul).
///
/// [`UseCategory`]: crate::unicode::use_category::UseCategory
/// [`UsePosition`]: crate::unicode::use_category::UsePosition
#[allow(clippy::too_many_arguments)]
pub(crate) fn shape_use(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    script_priority: &[[u8; 4]],
    basic_features: &[&[u8; 4]],
    topographical_features: &[&[u8; 4]],
    reorder_prebase: bool,
    level: ClusterLevel,
    table: JoinerTable,
) {
    if codepoints.is_empty() || glyphs.is_empty() {
        return;
    }

    // 1. Segment.
    let syllables = segment_syllables(codepoints);

    // 2. Basic features, on the logical order. The glyphs carry their
    //    syllable and reorder category through GSUB. `rphf` applies
    //    only to the first glyphs of each syllable and marks the glyph
    //    it substitutes as a repha (HarfBuzz's `setup_rphf_mask` and
    //    `record_rphf_use`), and `pref` marks the first glyph it
    //    substitutes in each syllable as pre-base (`record_pref_use`).
    let reorder = reorder_prebase && tag_syllables(glyphs, codepoints, &syllables);
    if let Some(gsub) = gsub {
        for tag in basic_features {
            if reorder && **tag == *b"rphf" {
                apply_rphf(gsub, gdef, glyphs, script_priority);
                continue;
            }
            let pref = reorder && **tag == *b"pref";
            let before: Vec<u32> = if pref {
                glyphs.iter().map(|g| g.glyph_id).collect()
            } else {
                Vec::new()
            };
            let joiners = table.joiners(**tag);
            apply_gsub_feature_in_scripts(gsub, glyphs, gdef, **tag, 0, script_priority, joiners);
            if pref {
                record_pref(&before, glyphs);
            }
        }
    }

    // 3. The reorder, after the basic features (`reorder_use`).
    if reorder {
        reorder_pre_base(glyphs, level);
    }

    // 4. Topographical features.
    if let Some(gsub) = gsub {
        for tag in topographical_features {
            let joiners = table.joiners(**tag);
            apply_gsub_feature_in_scripts(gsub, glyphs, gdef, **tag, 0, script_priority, joiners);
        }
    }
}

/// HarfBuzz's flags for the USE `rphf` feature (`collect_features_use`).
pub(crate) const USE_RPHF_FLAGS: FeatureFlags =
    FeatureFlags::MANUAL_ZWJ.union(FeatureFlags::PER_SYLLABLE);

/// HarfBuzz's USE `rphf` stage (`collect_features_use`): `rphf`,
/// applied only to the glyphs `setup_rphf_mask` marked, and then
/// `record_rphf_use`, which makes the glyph it substituted a repha.
/// HarfBuzz clears the substitution flags before the stage, so only
/// what `rphf` itself substituted counts.
///
/// HarfBuzz flags `rphf` `F_MANUAL_ZWJ | F_PER_SYLLABLE`
/// ([`USE_RPHF_FLAGS`]). The per-syllable part is left out here:
/// sigilbuzz's USE syllables come from a simpler grammar than
/// HarfBuzz's `hb-ot-shaper-use-machine.rl`, and matching inside them
/// would cut a lookup's context where HarfBuzz does not.
fn apply_rphf(
    gsub: &Gsub<'_>,
    gdef: Option<&Gdef<'_>>,
    glyphs: &mut Vec<Glyph>,
    script_priority: &[[u8; 4]],
) {
    const RPHF: u32 = 1;
    let mut info = rphf_info(glyphs, RPHF);
    let mut runner = SyllabicGsub::new(gsub, gdef, glyphs);
    let feature = StageFeature {
        tag: *b"rphf",
        mask: RPHF,
        flags: USE_RPHF_FLAGS.without(FeatureFlags::PER_SYLLABLE),
    };
    apply_stage(
        &mut runner,
        script_priority,
        &[feature],
        &[],
        glyphs,
        &mut info,
    );
    record_rphf(glyphs, &info);
}

#[cfg(test)]
mod tests;
