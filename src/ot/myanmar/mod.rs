//! sigilbuzz's Myanmar pass.
//!
//! HarfBuzz gives Myanmar a shaper of its own
//! (`hb-ot-shaper-myanmar.cc`). sigilbuzz follows its feature order
//! (`collect_features_myanmar`) over a simpler syllable grammar (the
//! `syllable` module): `locl` and `ccmp` on the logical order, the
//! syllable reorder (medial ra and pre-base vowels in front of the
//! base, kinzi after it, see the `reorder` module), the basic features
//! `rphf`, `pref`, `blwf`, and `pstf` one at a time, then `pres`,
//! `abvs`, `blws`, and `psts` together. The default features follow
//! in the generic pass.

mod category;
mod reorder;
mod syllable;

use alloc::vec::Vec;

pub(crate) use category::{category, Category};
use reorder::initial_reorder;
pub(crate) use syllable::{segment_syllables, Syllable, SyllableKind};

use crate::buffer::{ClusterLevel, Glyph};
use crate::shape::{
    apply_gsub_feature_in_scripts, apply_gsub_features_merged,
    apply_locl_ccmp_if_length_preserving, JoinerTable,
};
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;

/// Myanmar script-tag priority: `mym2` is the Indic2 (2012+) tag
/// that modern Noto and Padauk builds use, and `mymr` is the legacy
/// tag that older fonts still carry.
pub const MYANMAR_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"mym2", *b"mymr", *b"DFLT"];

/// Myanmar's features up to the basic ones: `locl` and `ccmp` before
/// the syllable reorder, then `rphf` (kinzi), `pref`, `blwf`, and
/// `pstf` after it (HarfBuzz's `myanmar_basic_features`).
pub const MYANMAR_BASIC_FEATURES: &[&[u8; 4]] =
    &[b"locl", b"ccmp", b"rphf", b"pref", b"blwf", b"pstf"];

/// Myanmar's other features, applied together once the syllables are
/// done (HarfBuzz's `myanmar_other_features`).
pub const MYANMAR_TOPOGRAPHICAL_FEATURES: &[&[u8; 4]] = &[b"pres", b"abvs", b"blws", b"psts"];

/// Entry point for Myanmar runs, in the order of HarfBuzz's Myanmar
/// shaper (`collect_features_myanmar`): `locl` and `ccmp` on the
/// logical order, the syllable reorder (medial ra and pre-base vowels
/// in front of the base, kinzi after it), the basic features `rphf`,
/// `pref`, `blwf`, and `pstf` one at a time, then `pres`, `abvs`,
/// `blws`, and `psts` together. The default features follow in the
/// generic pass.
pub fn shape_myanmar(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    if codepoints.is_empty() || glyphs.is_empty() {
        return;
    }
    let table = JoinerTable::Myanmar;
    let syllables = segment_syllables(codepoints);
    // Per-syllable features match within these (HarfBuzz's syllable()).
    let numbers = syllables.iter().map(|s| (s.start, s.end, s.kind as u8));
    crate::shape::number_syllables(glyphs, numbers, level);
    // `locl` and `ccmp` see the logical order, as one stage, before the
    // reorder (`collect_features_myanmar`). The reorder indexes glyphs
    // by code point, so a length-changing `ccmp` waits until after it.
    let early = gsub.is_some_and(|gsub| {
        apply_locl_ccmp_if_length_preserving(gsub, glyphs, gdef, MYANMAR_SCRIPT_PRIORITY, table)
    });
    for syllable in &syllables {
        initial_reorder(codepoints, glyphs, syllable, level);
    }
    let Some(gsub) = gsub else {
        return;
    };
    if !early {
        let locl_ccmp = [*b"locl", *b"ccmp"];
        apply_gsub_features_merged(
            gsub,
            glyphs,
            gdef,
            &[],
            &locl_ccmp,
            MYANMAR_SCRIPT_PRIORITY,
            table,
        );
    }
    // The basic features, one stage each.
    for tag in &MYANMAR_BASIC_FEATURES[2..] {
        let joiners = table.joiners(**tag);
        apply_gsub_feature_in_scripts(
            gsub,
            glyphs,
            gdef,
            **tag,
            0,
            MYANMAR_SCRIPT_PRIORITY,
            joiners,
        );
    }
    // The other features, as one stage.
    let other: Vec<[u8; 4]> = MYANMAR_TOPOGRAPHICAL_FEATURES.iter().map(|t| **t).collect();
    apply_gsub_features_merged(
        gsub,
        glyphs,
        gdef,
        &[],
        &other,
        MYANMAR_SCRIPT_PRIORITY,
        table,
    );
}

#[cfg(test)]
mod tests;
