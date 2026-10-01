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
//! The scripts of the Universal Shaping Engine go to the default shaper
//! in such a font, as in HarfBuzz ([`Shaper::for_run`]).

use crate::tables::Gsub;
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

    /// The shaper that normalizes a segment of a script that maps to
    /// `self` in a buffer HarfBuzz shapes with `buffer`.
    ///
    /// HarfBuzz normalizes the whole buffer with the buffer's shaper.
    /// sigilbuzz runs each segment's own shaper, so each segment
    /// normalizes with it, except where sigilbuzz shapes the segment as
    /// the buffer's shaper does: the Hangul shaper only runs for a
    /// Hangul buffer, and in a Hangul buffer the text of scripts with no
    /// shaper of their own goes through the Hangul shaper's features.
    pub(super) fn normalizer_for(self, buffer: Self) -> Self {
        match (self, buffer) {
            (Self::Hangul, _) | (Self::Default, Self::Hangul) => buffer,
            _ => self,
        }
    }

    /// [`Self::for_script`] for a run whose lookups try the script tags
    /// `script_priority` in the font's `gsub`: HarfBuzz gives a script
    /// of the Universal Shaping Engine the default shaper when the
    /// script tag GSUB picks is `DFLT` or `latn`
    /// (`hb_ot_shaper_categorize`).
    pub(super) fn for_run(
        script: Script,
        horizontal: bool,
        gsub: Option<&Gsub<'_>>,
        script_priority: &[[u8; 4]],
    ) -> Self {
        let shaper = Self::for_script(script, horizontal);
        let generic = gsub
            .and_then(|g| crate::ot::layout_select::chosen_script(g.script_list(), script_priority))
            .is_some_and(|tag| tag == *b"DFLT" || tag == *b"latn");
        if shaper == Self::Use && generic {
            Self::Default
        } else {
            shaper
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

    /// The shaper's `fallback_position`: whether marks the font cannot
    /// position get fallback positions from their combining classes.
    pub(super) const fn fallback_position(self) -> bool {
        matches!(
            self,
            Self::Default | Self::Arabic | Self::Hebrew | Self::Hangul
        )
    }

    /// The shaper's `gpos_tag`: GPOS only applies when the font's GPOS
    /// has a script of this tag (HarfBuzz disables it otherwise).
    pub(super) const fn gpos_tag(self) -> Option<[u8; 4]> {
        match self {
            Self::Hebrew => Some(*b"hebr"),
            _ => None,
        }
    }

    /// Whether the shaper's `preprocess_text` runs the vowel constraints
    /// (`_hb_preprocess_text_vowel_constraints`, see the
    /// `vowel_constraints` module).
    pub(super) const fn vowel_constraints(self) -> bool {
        match self {
            Self::Indic | Self::Use => true,
            Self::Default
            | Self::Arabic
            | Self::Hebrew
            | Self::Thai
            | Self::Hangul
            | Self::Khmer
            | Self::Myanmar => false,
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
        assert!(Shaper::Hangul.fallback_position());
        assert!(!Shaper::Thai.fallback_position());
        assert!(!Shaper::Indic.fallback_position());
        assert_eq!(Shaper::Hebrew.gpos_tag(), Some(*b"hebr"));
        assert_eq!(Shaper::Arabic.gpos_tag(), None);
        assert_eq!(Shaper::Hangul.mark_zeroing(), MarkZeroing::None);
        assert_eq!(Shaper::Use.mark_zeroing(), MarkZeroing::Early);
        assert_eq!(Shaper::Thai.mark_zeroing(), MarkZeroing::Late);
        assert_eq!(
            Shaper::Hangul.normalizer_for(Shaper::Default),
            Shaper::Default
        );
        assert_eq!(
            Shaper::Default.normalizer_for(Shaper::Hangul),
            Shaper::Hangul
        );
        assert_eq!(
            Shaper::Hangul.normalizer_for(Shaper::Hangul),
            Shaper::Hangul
        );
        assert_eq!(Shaper::Indic.normalizer_for(Shaper::Hangul), Shaper::Indic);
        assert_eq!(
            Shaper::Default.normalizer_for(Shaper::Indic),
            Shaper::Default
        );
        assert!(Shaper::Indic.vowel_constraints());
        assert!(Shaper::Use.vowel_constraints());
        assert!(!Shaper::Khmer.vowel_constraints());
        assert!(!Shaper::Myanmar.vowel_constraints());
    }
}
