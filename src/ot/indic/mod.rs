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
//! The HarfBuzz implementation of the Indic2 shaper is the
//! reference; sigilbuzz follows the same phase structure:
//!
//! ```text
//!   1. Segment the buffer into syllables.
//!   2. For each syllable: initial reordering.
//!      - Matra decomposition.
//!      - Classify consonants (half-form, below-base, post-base, ...).
//!      - Reorder pre-base matras to before the base consonant.
//!      - Mark `ra + halant` as reph and move to the reordering slot.
//!   3. Apply basic features in order: `nukt`, `akhn`, `rphf`,
//!      `blwf`, `half`, `pstf`, `vatu`, `cjct`.
//!   4. Final reordering.
//!      - Reph to its font-declared position.
//!      - Pre-base matras to their visual slot.
//!   5. Apply presentation features: `init`, `pres`, `abvs`,
//!      `blws`, `psts`, `haln`. (These run through the generic
//!      GSUB pass after the Indic pipeline returns.)
//! ```
//!
//! The state machine itself is script-agnostic. Per-script behavior
//! is concentrated in [`IndicConfig`]: script-tag priority (which
//! `<scr>`/`<scr2>` pair to look up features under), reph
//! positioning (where the reph lands after `rphf` collapses the
//! ra+halant pair), and the reph-detection mode (implicit
//! ra+halant, explicit ra+halant+ZWJ, or a LogRepha encoded glyph).
//!
//! The configuration table matches rustybuzz's `INDIC_CONFIGS` for
//! all nine Indic scripts sigilbuzz ships at 0.2.0.

pub mod devanagari;

pub use devanagari::{shape_devanagari, shape_indic};

use crate::unicode::Script;

/// Where the reph (reordered `ra + halant` form) should end up
/// relative to the other glyphs of the syllable.
///
/// Mirrors the `ot_position_t` slot the reph should occupy after
/// reordering. HarfBuzz uses these names; we keep them verbatim so
/// a future port of the richer reorder (which actually consults
/// the full per-glyph positional tagging) drops in without
/// renaming.
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
///
/// Only `Implicit` fires a reorder in sigilbuzz 0.2.0. `Explicit`
/// and `LogRepha` currently fall through to the generic Implicit
/// path for scripts where that produces the same output on the
/// tested corpus; scripts that need explicit behavior are flagged
/// as follow-up issues.
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
    /// Script-tag priority for GSUB/GPOS feature lookup. First tag
    /// is the Indic2 tag (`dev2`, `bng2`, ...); second is the legacy
    /// Indic1 tag; last is always `DFLT` as a fallback.
    pub script_priority: &'static [[u8; 4]],
}

// Script-tag priority tables. Each Indic script has an Indic2 tag
// (`dev2`, `bng2`, ...) and a legacy Indic1 tag (`deva`, `beng`, ...);
// DFLT is a last-resort fallback. Sinhala is the only Indic script
// that never received an Indic2 tag, so its priority only lists `sinh`.
pub(crate) const DEVA_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"dev2", *b"deva", *b"DFLT"];
pub(crate) const BENG_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"bng2", *b"beng", *b"DFLT"];
pub(crate) const GURU_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"gur2", *b"guru", *b"DFLT"];
pub(crate) const GUJR_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"gjr2", *b"gujr", *b"DFLT"];
pub(crate) const ORYA_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"ory2", *b"orya", *b"DFLT"];
pub(crate) const TAML_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"tml2", *b"taml", *b"DFLT"];
pub(crate) const TELU_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"tel2", *b"telu", *b"DFLT"];
pub(crate) const KNDA_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"knd2", *b"knda", *b"DFLT"];
pub(crate) const MLYM_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"mlm2", *b"mlym", *b"DFLT"];
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
            // Sinhala's "ra" is U+0DBB; but Sinhala reph mode is Explicit
            // (requires a following ZWJ). sigilbuzz's M2 Implicit path
            // won't trigger on bare ra+virama anyway, so this field is
            // informational for Sinhala until the Explicit mode lands.
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
        assert_eq!(c.script_priority, &[*b"dev2", *b"deva", *b"DFLT"]);
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
