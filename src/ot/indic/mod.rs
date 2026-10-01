//! Indic2 reordering shaper.
//!
//! Indic scripts (Devanagari, Bengali, Gurmukhi, Gujarati, Oriya,
//! Tamil, Telugu, Kannada, Malayalam, Sinhala) are written
//! phonetically but laid out with a richer set of positional rules
//! than Latin. A pre-base vowel sign is typed after its consonant
//! but must render before it; `ra + halant` at the start of a
//! syllable becomes a "reph" that renders above the last consonant;
//! conjunct consonants (half-forms, below-base forms, post-base
//! forms) are selected by font-declared GSUB features that run in
//! a specific order.
//!
//! The nine scripts of HarfBuzz's Indic shaper (Devanagari through
//! Malayalam) run through a port of it (`hb-ot-shaper-indic.cc`, in
//! the `shaper`, `initial`, and `final_reorder` submodules):
//!
//! ```text
//!   1. Classify each character (hb_indic_get_categories) and split the
//!      run into syllables with HarfBuzz's Indic syllable grammar.
//!   2. Apply `locl` and `ccmp`, one syllable at a time.
//!   3. Initial reordering: consonant positions from the font's
//!      `blwf`, `vatu`, `pstf`, and `pref`, dotted circles for broken
//!      clusters, the base consonant and reph of each syllable (ZWJ
//!      and ZWNJ decide both), the syllable sorted by position, and
//!      the feature masks.
//!   4. The basic features `nukt` to `cjct`, one stage each, masked
//!      and one syllable at a time.
//!   5. Final reordering: the base again, pre-base matras next to it,
//!      the reph to its script's position, pre-base-reordering
//!      consonants, and `init` on a word-initial matra.
//!   6. `init`, `pres`, `abvs`, `blws`, `psts`, and `haln` in one stage
//!      with the default features HarfBuzz puts there.
//! ```
//!
//! Per-script behavior is in [`IndicConfig`]: script-tag priority
//! (which `<scr>`/`<scr2>` pair to look up features under), reph
//! positioning, and the reph-detection mode (implicit ra+halant,
//! explicit ra+halant+ZWJ, or a LogRepha encoded glyph). HarfBuzz's
//! other two per-script settings live in the shaper: every one of the
//! nine scripts has an old spec, and Telugu and Kannada apply `blwf`
//! after the base only.
//!
//! Sinhala, which HarfBuzz shapes with the Universal Shaping Engine,
//! runs that shaper ([`crate::ot::use_shaper`]). Its config here only
//! gives [`shape_indic`] its script tags.

pub mod devanagari;
mod final_reorder;
mod initial;
mod machine;
pub(crate) mod shaper;

pub use devanagari::{shape_devanagari, shape_indic};

use crate::unicode::Script;

/// Where the reph (reordered `ra + halant` form) should end up
/// relative to the other glyphs of the syllable.
///
/// Mirrors the `ot_position_t` slot the reph should occupy after
/// reordering. The names match HarfBuzz's (`reph_position_t` in
/// `hb-ot-shaper-indic.cc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RephPosition {
    /// Just after the main (base) consonant.
    AfterMain,
    /// Before any sub-joined (below-base) consonant forms.
    BeforeSub,
    /// After any sub-joined (below-base) consonant forms.
    AfterSub,
    /// Before any post-base consonant/matra. Default Devanagari slot.
    BeforePost,
    /// After any post-base consonant/matra. Tamil/Telugu/Kannada/Sinhala slot.
    AfterPost,
}

/// How the shaper recognizes that a syllable has a reph to move.
///
/// [`RephMode::Implicit`]: ra + halant at the head is enough.
/// [`RephMode::Explicit`]: ra + halant + ZWJ is required.
/// [`RephMode::LogRepha`]: the encoded Repha character (e.g.
/// Malayalam U+0D4E) is emitted ahead of the base and reordered as
/// if it were a reph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RephMode {
    /// Reph formed out of initial Ra,H sequence.
    Implicit,
    /// Reph formed out of initial Ra,H,ZWJ sequence.
    Explicit,
    /// Encoded Repha character (Malayalam U+0D4E etc.) reordered in place.
    LogRepha,
}

/// Per-script tuning that the Indic state machine consults.
///
/// Populated from rustybuzz's `INDIC_CONFIGS` at 0.20.1. Each Indic
/// script has exactly one config; [`indic_config_for`] resolves
/// a [`Script`] to its config.
#[derive(Debug, Clone, Copy)]
pub struct IndicConfig {
    /// The script this config belongs to.
    pub script: Script,
    /// Virama codepoint, for ra+halant detection.
    pub virama: u32,
    /// The script's "ra" consonant codepoint. The state machine
    /// recognizes a reph candidate when the syllable opens with
    /// `ra + virama` (and the reph mode allows it).
    pub ra: u32,
    /// Reph display slot after `rphf` fires.
    pub reph_pos: RephPosition,
    /// How the shaper decides a syllable has a reph.
    pub reph_mode: RephMode,
    /// Script-tag priority for GSUB/GPOS feature lookup: the Indic3
    /// tag (`dev3`, `bng3`, ...), the Indic2 tag (`dev2`, `bng2`, ...),
    /// the legacy Indic1 tag, and `DFLT` as a fallback.
    pub script_priority: &'static [[u8; 4]],
}

// Script-tag priority tables, in HarfBuzz's order
// (`hb_ot_all_tags_from_script` in `hb-ot-tag.cc`). Each Indic script
// has an Indic3 tag (`dev3`, `bng3`, ...), an Indic2 tag (`dev2`,
// `bng2`, ...), and a legacy Indic1 tag (`deva`, `beng`, ...). DFLT is
// a last-resort fallback. A font that has the Indic3 tag gets the
// Universal Shaping Engine (`Shaper::for_run`). Sinhala is the only
// one that never received a newer tag, so its priority only lists
// `sinh`.
pub(crate) const DEVA_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"dev3", *b"dev2", *b"deva", *b"DFLT"];
pub(crate) const BENG_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"bng3", *b"bng2", *b"beng", *b"DFLT"];
pub(crate) const GURU_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"gur3", *b"gur2", *b"guru", *b"DFLT"];
pub(crate) const GUJR_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"gjr3", *b"gjr2", *b"gujr", *b"DFLT"];
pub(crate) const ORYA_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"ory3", *b"ory2", *b"orya", *b"DFLT"];
pub(crate) const TAML_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"tml3", *b"tml2", *b"taml", *b"DFLT"];
pub(crate) const TELU_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"tel3", *b"tel2", *b"telu", *b"DFLT"];
pub(crate) const KNDA_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"knd3", *b"knd2", *b"knda", *b"DFLT"];
pub(crate) const MLYM_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"mlm3", *b"mlm2", *b"mlym", *b"DFLT"];
pub(crate) const SINH_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"sinh", *b"DFLT"];

/// Returns the [`IndicConfig`] for an Indic script, or `None` for
/// non-Indic scripts.
///
/// The reph-position/mode table follows rustybuzz's `INDIC_CONFIGS`
/// at 0.20.1, which is the current ground truth for HarfBuzz's
/// Indic2 shaper defaults.
#[must_use]
pub const fn indic_config_for(script: Script) -> Option<IndicConfig> {
    match script {
        Script::Devanagari => Some(IndicConfig {
            script,
            virama: 0x094D,
            ra: 0x0930,
            reph_pos: RephPosition::BeforePost,
            reph_mode: RephMode::Implicit,
            script_priority: DEVA_SCRIPT_PRIORITY,
        }),
        Script::Bengali => Some(IndicConfig {
            script,
            virama: 0x09CD,
            ra: 0x09B0,
            reph_pos: RephPosition::AfterSub,
            reph_mode: RephMode::Implicit,
            script_priority: BENG_SCRIPT_PRIORITY,
        }),
        Script::Gurmukhi => Some(IndicConfig {
            script,
            virama: 0x0A4D,
            ra: 0x0A30,
            reph_pos: RephPosition::BeforeSub,
            reph_mode: RephMode::Implicit,
            script_priority: GURU_SCRIPT_PRIORITY,
        }),
        Script::Gujarati => Some(IndicConfig {
            script,
            virama: 0x0ACD,
            ra: 0x0AB0,
            reph_pos: RephPosition::BeforePost,
            reph_mode: RephMode::Implicit,
            script_priority: GUJR_SCRIPT_PRIORITY,
        }),
        Script::Oriya => Some(IndicConfig {
            script,
            virama: 0x0B4D,
            ra: 0x0B30,
            reph_pos: RephPosition::AfterMain,
            reph_mode: RephMode::Implicit,
            script_priority: ORYA_SCRIPT_PRIORITY,
        }),
        Script::Tamil => Some(IndicConfig {
            script,
            virama: 0x0BCD,
            ra: 0x0BB0,
            reph_pos: RephPosition::AfterPost,
            reph_mode: RephMode::Implicit,
            script_priority: TAML_SCRIPT_PRIORITY,
        }),
        Script::Telugu => Some(IndicConfig {
            script,
            virama: 0x0C4D,
            ra: 0x0C30,
            reph_pos: RephPosition::AfterPost,
            reph_mode: RephMode::Explicit,
            script_priority: TELU_SCRIPT_PRIORITY,
        }),
        Script::Kannada => Some(IndicConfig {
            script,
            virama: 0x0CCD,
            ra: 0x0CB0,
            reph_pos: RephPosition::AfterPost,
            reph_mode: RephMode::Implicit,
            script_priority: KNDA_SCRIPT_PRIORITY,
        }),
        Script::Malayalam => Some(IndicConfig {
            script,
            virama: 0x0D4D,
            ra: 0x0D30,
            reph_pos: RephPosition::AfterMain,
            reph_mode: RephMode::LogRepha,
            script_priority: MLYM_SCRIPT_PRIORITY,
        }),
        Script::Sinhala => Some(IndicConfig {
            script,
            virama: 0x0DCA,
            // Sinhala reph mode is Explicit, so ra + virama forms a reph
            // only when a ZWJ follows.
            ra: 0x0DBB,
            reph_pos: RephPosition::AfterPost,
            reph_mode: RephMode::Explicit,
            script_priority: SINH_SCRIPT_PRIORITY,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_indic_script_has_a_config() {
        for script in [
            Script::Devanagari,
            Script::Bengali,
            Script::Gurmukhi,
            Script::Gujarati,
            Script::Oriya,
            Script::Tamil,
            Script::Telugu,
            Script::Kannada,
            Script::Malayalam,
            Script::Sinhala,
        ] {
            assert!(
                indic_config_for(script).is_some(),
                "missing config for {script:?}"
            );
        }
    }

    #[test]
    fn non_indic_scripts_have_no_config() {
        for script in [
            Script::Latin,
            Script::Arabic,
            Script::Hebrew,
            Script::Han,
            Script::Other,
        ] {
            assert!(indic_config_for(script).is_none(), "unexpected {script:?}");
        }
    }

    #[test]
    fn devanagari_config_matches_known_values() {
        let c = indic_config_for(Script::Devanagari).unwrap();
        assert_eq!(c.virama, 0x094D);
        assert_eq!(c.reph_pos, RephPosition::BeforePost);
        assert_eq!(c.reph_mode, RephMode::Implicit);
        assert_eq!(c.script_priority, &[*b"dev3", *b"dev2", *b"deva", *b"DFLT"]);
    }

    #[test]
    fn sinhala_priority_has_no_indic2_tag() {
        let c = indic_config_for(Script::Sinhala).unwrap();
        assert_eq!(c.script_priority, &[*b"sinh", *b"DFLT"]);
    }

    #[test]
    fn telugu_uses_explicit_reph_mode() {
        let c = indic_config_for(Script::Telugu).unwrap();
        assert_eq!(c.reph_mode, RephMode::Explicit);
    }

    #[test]
    fn malayalam_uses_logrepha_mode() {
        let c = indic_config_for(Script::Malayalam).unwrap();
        assert_eq!(c.reph_mode, RephMode::LogRepha);
    }
}
