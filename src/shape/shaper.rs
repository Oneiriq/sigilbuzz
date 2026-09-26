//! The HarfBuzz shaper a script runs through, and the properties of
//! each shaper that the shaping stages consult.
//!
//! HarfBuzz picks one shaper per run from its script
//! (`hb_ot_shaper_categorize` in `hb-ot-shaper.hh`) and reads fixed
//! properties off it (`hb_ot_shaper_t`): the normalization mode, the
//! decompose and compose overrides and the mark reordering hook, when
//! mark advances are zeroed, and whether marks get fallback positions
//! when the font cannot position them. The values here follow the
//! shaper descriptors of HarfBuzz 14.5.0 (`hb-ot-shaper-*.cc`).
//!
//! HarfBuzz sends the Indic scripts to the default shaper when the font
//! only has `DFLT` or `latn` lookups, and Myanmar also when it only has
//! the pre-spec `mymr` tag; sigilbuzz runs its Indic and Myanmar shapers
//! regardless, so those scripts always map to their own shaper here.

use crate::unicode::Script;

/// A HarfBuzz shaper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Shaper {
    /// The default shaper (Latin, Greek, Cyrillic, Han, and every
    /// script without a dedicated one), also used for vertical Arabic.
    Default,
    /// Arabic joining shaper.
    Arabic,
    /// Hebrew shaper.
    Hebrew,
    /// Thai and Lao shaper.
    Thai,
    /// Hangul shaper.
    Hangul,
    /// Indic shaper (Devanagari through Malayalam; Sinhala uses USE).
    Indic,
    /// Khmer shaper.
    Khmer,
    /// Myanmar shaper.
    Myanmar,
    /// Universal Shaping Engine.
    Use,
}

/// How the normalizer treats a run (`hb_ot_shape_normalization_mode_t`).
/// HarfBuzz's `AUTO` mode resolves to [`Self::ComposedDiacritics`]
/// whether or not the font has GPOS mark positioning, so it is not a
/// separate variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NormalizationMode {
    /// Keep every character the font maps; decompose only the ones it
    /// does not. Nothing recomposes.
    None,
    /// Decompose whatever the font supports decomposed.
    #[allow(dead_code)] // no HarfBuzz 14.5.0 shaper asks for it
    Decomposed,
    /// Keep characters the font maps, decompose clusters with marks, and
    /// recompose base and mark pairs the font has a composite for.
    ComposedDiacritics,
    /// Like [`Self::ComposedDiacritics`], but decompose every character
    /// first even when the font maps it.
    ComposedDiacriticsNoShortCircuit,
}

/// When mark advances are zeroed (`hb_ot_shape_zero_width_marks_type_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MarkZeroing {
    /// Never.
    None,
    /// Before GPOS.
    Early,
    /// After all positioning.
    Late,
}

impl Shaper {
    /// The shaper HarfBuzz runs `script` through. Arabic only uses the
    /// Arabic shaper for horizontal runs.
    pub(super) fn for_script(script: Script, horizontal: bool) -> Self {
        match script {
            Script::Arabic if horizontal => Self::Arabic,
            Script::Hebrew => Self::Hebrew,
            Script::Thai | Script::Lao => Self::Thai,
            Script::Hangul => Self::Hangul,
            Script::Devanagari
            | Script::Bengali
            | Script::Gurmukhi
            | Script::Gujarati
            | Script::Oriya
            | Script::Tamil
            | Script::Telugu
            | Script::Kannada
            | Script::Malayalam => Self::Indic,
            Script::Khmer => Self::Khmer,
            Script::Myanmar => Self::Myanmar,
            Script::Sinhala
            | Script::Tibetan
            | Script::Mongolian
            | Script::NKo
            | Script::Buginese
            | Script::TaiTham
            | Script::Balinese
            | Script::Sundanese
            | Script::Lepcha
            | Script::Limbu
            | Script::Cham
            | Script::Brahmi
            | Script::Sharada
            | Script::Khojki
            | Script::Tirhuta
            | Script::Modi => Self::Use,
            Script::Arabic
            | Script::Latin
            | Script::Greek
            | Script::Cyrillic
            | Script::Han
            | Script::Other => Self::Default,
        }
    }

    /// The shaper's `normalization_preference`, with `AUTO` resolved.
    pub(super) const fn normalization_mode(self) -> NormalizationMode {
        match self {
            Self::Hangul => NormalizationMode::None,
            Self::Indic | Self::Khmer | Self::Myanmar | Self::Use => {
                NormalizationMode::ComposedDiacriticsNoShortCircuit
            }
            Self::Default | Self::Arabic | Self::Hebrew | Self::Thai => {
                NormalizationMode::ComposedDiacritics
            }
        }
    }

    /// The shaper's `zero_width_marks`.
    pub(super) const fn mark_zeroing(self) -> MarkZeroing {
        match self {
            Self::Indic | Self::Khmer | Self::Hangul => MarkZeroing::None,
            Self::Myanmar | Self::Use => MarkZeroing::Early,
            Self::Default | Self::Arabic | Self::Hebrew | Self::Thai => MarkZeroing::Late,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_map_to_harfbuzz_shapers() {
        assert_eq!(Shaper::for_script(Script::Arabic, true), Shaper::Arabic);
        assert_eq!(Shaper::for_script(Script::Arabic, false), Shaper::Default);
        assert_eq!(Shaper::for_script(Script::Sinhala, true), Shaper::Use);
        assert_eq!(Shaper::for_script(Script::Lao, true), Shaper::Thai);
        assert_eq!(Shaper::for_script(Script::Tamil, true), Shaper::Indic);
        assert_eq!(Shaper::for_script(Script::Latin, true), Shaper::Default);
    }

    #[test]
    fn shaper_properties_follow_harfbuzz() {
        assert_eq!(Shaper::Hangul.normalization_mode(), NormalizationMode::None);
        assert_eq!(
            Shaper::Use.normalization_mode(),
            NormalizationMode::ComposedDiacriticsNoShortCircuit
        );
        assert_eq!(
            Shaper::Hebrew.normalization_mode(),
            NormalizationMode::ComposedDiacritics
        );
        assert_eq!(Shaper::Hangul.mark_zeroing(), MarkZeroing::None);
        assert_eq!(Shaper::Use.mark_zeroing(), MarkZeroing::Early);
        assert_eq!(Shaper::Thai.mark_zeroing(), MarkZeroing::Late);
    }
}
