//! The Indic entry points.
//!
//! [`shape_indic`] and [`shape_devanagari`] run the port of HarfBuzz's
//! Indic shaper (`crate::ot::indic::shaper`) for the nine scripts it
//! covers. Sinhala, which HarfBuzz shapes with the Universal Shaping
//! Engine, runs that shaper (`crate::ot::use_shaper`).

use alloc::vec::Vec;

use super::IndicConfig;
use crate::buffer::{ClusterLevel, Glyph};
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;
use crate::unicode::Script;

/// Script-agnostic Indic entry point. Shapes the Indic run described
/// by `codepoints`, using `config` for per-script behavior.
///
/// `codepoints` is in one-to-one correspondence with the starting
/// glyph layout: each codepoint produced one glyph before any
/// reordering. After this function returns, `glyphs` may contain
/// fewer entries (if basic features applied ligatures) and the
/// order can differ from input.
///
/// The nine scripts of HarfBuzz's Indic shaper run through its port
/// (`crate::ot::indic::shaper`), every GSUB feature of the run
/// included, the default ones too. The virama glyph comes from the run
/// itself, and broken clusters get no dotted circle here. Shaping
/// through [`crate::shape`] adds both from the font. Sinhala runs
/// through the Universal Shaping Engine, as in HarfBuzz, with the same
/// coverage of features.
pub fn shape_indic(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    config: &IndicConfig,
    level: ClusterLevel,
) {
    if config.script == Script::Sinhala {
        crate::ot::use_shaper::shape_use(
            gsub,
            gdef,
            codepoints,
            glyphs,
            config.script_priority,
            level,
        );
        return;
    }
    let virama_glyph = codepoints
        .iter()
        .zip(glyphs.iter())
        .find(|(&c, _)| c as u32 == config.virama)
        .map(|(_, g)| g.glyph_id as u16);
    let run = super::shaper::IndicRun {
        gsub,
        gdef,
        level,
        features: &[],
        vertical: false,
        dotted_circle: None,
        virama_glyph,
    };
    super::shaper::shape(&run, config, codepoints, glyphs);
}

/// Devanagari convenience wrapper over [`shape_indic`]. Kept for the
/// existing call sites; new scripts should use `shape_indic` with the
/// config from [`super::indic_config_for`].
pub fn shape_devanagari(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    // `indic_config_for` has a Devanagari entry, so the early return
    // never fires.
    let Some(config) = super::indic_config_for(Script::Devanagari) else {
        return;
    };
    shape_indic(gsub, gdef, codepoints, glyphs, &config, level);
}
