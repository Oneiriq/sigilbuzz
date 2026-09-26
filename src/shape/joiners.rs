//! Which GSUB features handle ZWJ and ZWNJ themselves.
//!
//! HarfBuzz's shapers register some features with `F_MANUAL_ZWJ` or
//! `F_MANUAL_ZWNJ` (`F_MANUAL_JOINERS` for both): the lookups of such a
//! feature see a joiner as an ordinary glyph instead of skipping it
//! (see [`Joiners`]). Arabic does it for its ligating features, since
//! a ZWJ there should also keep letters apart; the Indic-family
//! shapers do it for the features that form conjuncts, so ZWJ and ZWNJ
//! select half forms and block conjuncts the way the scripts' encoding
//! models say. Every other feature, including all the default ones,
//! skips joiners automatically.
//!
//! [`JoinerTable`] is the per-shaper list, from the feature tables of
//! HarfBuzz's `hb-ot-shaper-arabic.cc`, `-indic.cc`, `-khmer.cc`,
//! `-myanmar.cc` and `-use.cc` (the `mark` and `mkmk` GPOS features,
//! manual in every shaper, live with the GPOS stage). The first
//! registration of a tag decides its flags, and shapers register
//! theirs before the default features, so a shaper's flags win for
//! the default tags it also lists (`ccmp`, `liga`, ... in Arabic).

use crate::tables::layout::Joiners;
use crate::unicode::Script;

/// The feature table of the HarfBuzz shaper a run's GSUB features
/// follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JoinerTable {
    /// The default shaper, and the Thai, Lao, Hebrew and Hangul
    /// shapers: every feature skips joiners automatically.
    Default,
    /// The Arabic shaper: manual ZWJ for its ligating features.
    Arabic,
    /// The Indic shaper: manual joiners for every Indic feature.
    Indic,
    /// The Khmer shaper: manual joiners for every Khmer feature.
    Khmer,
    /// The Myanmar shaper: manual ZWJ for its basic and presentation
    /// features.
    Myanmar,
    /// The Universal Shaping Engine: manual ZWJ for every feature but
    /// the pre-processing (`locl`, `ccmp`, `nukt`) and topographical
    /// (`isol`, `init`, `medi`, `fina`) ones.
    Use,
}

impl JoinerTable {
    /// The table for a segment of `script`: `arabic` when the Arabic
    /// joining pass runs for it, `dominant` the buffer's script (the
    /// Tibetan and Mongolian shapers only run for their own buffers).
    pub(crate) fn for_segment(script: Script, arabic: bool, dominant: Option<Script>) -> Self {
        if arabic {
            return Self::Arabic;
        }
        match script {
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
            Script::Tibetan | Script::Mongolian if dominant == Some(script) => Self::Use,
            Script::Sinhala
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
            _ => Self::Default,
        }
    }

    /// The joiner handling of feature `tag` in this shaper.
    pub(crate) fn joiners(self, tag: [u8; 4]) -> Joiners {
        let manual = match self {
            Self::Default => false,
            Self::Arabic => matches!(
                &tag,
                b"ccmp"
                    | b"locl"
                    | b"isol"
                    | b"fina"
                    | b"fin2"
                    | b"fin3"
                    | b"medi"
                    | b"med2"
                    | b"init"
                    | b"rlig"
                    | b"calt"
                    | b"liga"
                    | b"clig"
                    | b"mset"
            ),
            Self::Indic => matches!(
                &tag,
                b"nukt"
                    | b"akhn"
                    | b"rphf"
                    | b"rkrf"
                    | b"pref"
                    | b"blwf"
                    | b"abvf"
                    | b"half"
                    | b"pstf"
                    | b"vatu"
                    | b"cjct"
                    | b"init"
                    | b"pres"
                    | b"abvs"
                    | b"blws"
                    | b"psts"
                    | b"haln"
            ),
            Self::Khmer => matches!(
                &tag,
                b"pref"
                    | b"blwf"
                    | b"abvf"
                    | b"pstf"
                    | b"cfar"
                    | b"pres"
                    | b"abvs"
                    | b"blws"
                    | b"psts"
            ),
            Self::Myanmar => matches!(
                &tag,
                b"rphf" | b"pref" | b"blwf" | b"pstf" | b"pres" | b"abvs" | b"blws" | b"psts"
            ),
            Self::Use => matches!(
                &tag,
                b"akhn"
                    | b"rphf"
                    | b"pref"
                    | b"rkrf"
                    | b"abvf"
                    | b"blwf"
                    | b"half"
                    | b"pstf"
                    | b"vatu"
                    | b"cjct"
                    | b"abvs"
                    | b"blws"
                    | b"haln"
                    | b"pres"
                    | b"psts"
            ),
        };
        match (manual, self) {
            (false, _) => Joiners::AUTO,
            (true, Self::Indic | Self::Khmer) => Joiners::MANUAL,
            (true, _) => Joiners::MANUAL_ZWJ,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapers_mark_their_features_manual() {
        assert_eq!(JoinerTable::Arabic.joiners(*b"liga"), Joiners::MANUAL_ZWJ);
        assert_eq!(JoinerTable::Arabic.joiners(*b"rclt"), Joiners::AUTO);
        assert_eq!(JoinerTable::Indic.joiners(*b"half"), Joiners::MANUAL);
        assert_eq!(JoinerTable::Indic.joiners(*b"ccmp"), Joiners::AUTO);
        assert_eq!(JoinerTable::Khmer.joiners(*b"pres"), Joiners::MANUAL);
        assert_eq!(JoinerTable::Myanmar.joiners(*b"blwf"), Joiners::MANUAL_ZWJ);
        assert_eq!(JoinerTable::Use.joiners(*b"pres"), Joiners::MANUAL_ZWJ);
        assert_eq!(JoinerTable::Use.joiners(*b"isol"), Joiners::AUTO);
        assert_eq!(JoinerTable::Default.joiners(*b"liga"), Joiners::AUTO);
    }

    #[test]
    fn segments_pick_their_shapers_table() {
        let t = JoinerTable::for_segment;
        assert_eq!(t(Script::Arabic, true, None), JoinerTable::Arabic);
        assert_eq!(t(Script::Devanagari, false, None), JoinerTable::Indic);
        assert_eq!(t(Script::Sinhala, false, None), JoinerTable::Use);
        let mong = Some(Script::Mongolian);
        assert_eq!(t(Script::Mongolian, false, mong), JoinerTable::Use);
        assert_eq!(t(Script::Mongolian, false, None), JoinerTable::Default);
        assert_eq!(t(Script::Latin, false, None), JoinerTable::Default);
    }
}
