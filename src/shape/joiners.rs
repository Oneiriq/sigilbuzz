//! Which GSUB features handle ZWJ and ZWNJ themselves, and which
//! match within one syllable.
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
//! The Indic, Khmer, Myanmar, and USE shapers also register the
//! features that shape a syllable with `F_PER_SYLLABLE`: their lookups
//! only match glyphs of the cursor's syllable (the skipping iterator's
//! `per_syllable`), so no conjunct or ligature forms across a syllable
//! boundary. The syllables are the ones the shapers record in
//! [`Glyph::syllable`](crate::Glyph::syllable).
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

/// A GSUB feature's flags in its shaper's feature table: HarfBuzz's
/// `F_MANUAL_ZWJ` / `F_MANUAL_ZWNJ` ([`Joiners`]) and
/// `F_PER_SYLLABLE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FeatureFlags {
    /// How the feature's lookups treat ZWJ and ZWNJ.
    pub(crate) joiners: Joiners,
    /// The feature's lookups match within the cursor's syllable only.
    pub(crate) per_syllable: bool,
}

impl FeatureFlags {
    /// Automatic joiners, any syllable: the default features' flags.
    pub(crate) const AUTO: Self = Self {
        joiners: Joiners::AUTO,
        per_syllable: false,
    };

    /// The flags of a lookup that several features of one stage share,
    /// as HarfBuzz's map merges them: joiners are skipped only where
    /// every feature skips them, and the first feature's per-syllable
    /// setting stays.
    pub(crate) const fn and(self, other: Self) -> Self {
        Self {
            joiners: self.joiners.and(other.joiners),
            per_syllable: self.per_syllable,
        }
    }
}

impl From<Joiners> for FeatureFlags {
    fn from(joiners: Joiners) -> Self {
        Self {
            joiners,
            per_syllable: false,
        }
    }
}

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

    /// The flags of feature `tag` in this shaper: its joiner handling
    /// and whether it matches within one syllable.
    pub(crate) fn joiners(self, tag: [u8; 4]) -> FeatureFlags {
        FeatureFlags {
            joiners: self.manual_joiners(tag),
            per_syllable: self.per_syllable(tag),
        }
    }

    /// Whether feature `tag` is registered with `F_PER_SYLLABLE`: in
    /// the Indic shaper `locl`, `ccmp`, and every Indic feature; in
    /// Khmer `locl`, `ccmp`, and the basic features before `pres`; in
    /// Myanmar `locl`, `ccmp`, and the basic features; in USE every
    /// feature up to its reorder.
    fn per_syllable(self, tag: [u8; 4]) -> bool {
        match self {
            Self::Default | Self::Arabic => false,
            Self::Indic => matches!(
                &tag,
                b"locl"
                    | b"ccmp"
                    | b"nukt"
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
                b"locl" | b"ccmp" | b"pref" | b"blwf" | b"abvf" | b"pstf" | b"cfar"
            ),
            Self::Myanmar => matches!(
                &tag,
                b"locl" | b"ccmp" | b"rphf" | b"pref" | b"blwf" | b"pstf"
            ),
            Self::Use => matches!(
                &tag,
                b"locl"
                    | b"ccmp"
                    | b"nukt"
                    | b"akhn"
                    | b"rphf"
                    | b"pref"
                    | b"rkrf"
                    | b"abvf"
                    | b"blwf"
                    | b"half"
                    | b"pstf"
                    | b"vatu"
                    | b"cjct"
            ),
        }
    }

    /// The joiner handling of feature `tag` in this shaper.
    fn manual_joiners(self, tag: [u8; 4]) -> Joiners {
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
        assert_eq!(
            JoinerTable::Arabic.joiners(*b"liga").joiners,
            Joiners::MANUAL_ZWJ
        );
        assert_eq!(JoinerTable::Arabic.joiners(*b"rclt").joiners, Joiners::AUTO);
        assert_eq!(
            JoinerTable::Indic.joiners(*b"half").joiners,
            Joiners::MANUAL
        );
        assert_eq!(JoinerTable::Indic.joiners(*b"ccmp").joiners, Joiners::AUTO);
        assert_eq!(
            JoinerTable::Khmer.joiners(*b"pres").joiners,
            Joiners::MANUAL
        );
        assert_eq!(
            JoinerTable::Myanmar.joiners(*b"blwf").joiners,
            Joiners::MANUAL_ZWJ
        );
        assert_eq!(
            JoinerTable::Use.joiners(*b"pres").joiners,
            Joiners::MANUAL_ZWJ
        );
        assert_eq!(JoinerTable::Use.joiners(*b"isol").joiners, Joiners::AUTO);
        assert_eq!(
            JoinerTable::Default.joiners(*b"liga").joiners,
            Joiners::AUTO
        );
    }

    #[test]
    fn syllabic_shapers_mark_their_syllable_features_per_syllable() {
        let per = |t: JoinerTable, tag: &[u8; 4]| t.joiners(*tag).per_syllable;
        // hb-ot-shaper-indic.cc: locl, ccmp and every Indic feature.
        assert!(per(JoinerTable::Indic, b"ccmp") && per(JoinerTable::Indic, b"half"));
        assert!(per(JoinerTable::Indic, b"pres") && per(JoinerTable::Indic, b"haln"));
        assert!(!per(JoinerTable::Indic, b"calt") && !per(JoinerTable::Indic, b"liga"));
        // hb-ot-shaper-khmer.cc: the basic features, not the others.
        assert!(per(JoinerTable::Khmer, b"locl") && per(JoinerTable::Khmer, b"cfar"));
        assert!(!per(JoinerTable::Khmer, b"pres") && !per(JoinerTable::Khmer, b"psts"));
        // hb-ot-shaper-myanmar.cc: likewise.
        assert!(per(JoinerTable::Myanmar, b"rphf") && per(JoinerTable::Myanmar, b"pstf"));
        assert!(!per(JoinerTable::Myanmar, b"pres") && !per(JoinerTable::Myanmar, b"blws"));
        // hb-ot-shaper-use.cc: up to the reorder.
        assert!(per(JoinerTable::Use, b"nukt") && per(JoinerTable::Use, b"cjct"));
        assert!(!per(JoinerTable::Use, b"isol") && !per(JoinerTable::Use, b"pres"));
        assert!(!per(JoinerTable::Arabic, b"ccmp") && !per(JoinerTable::Default, b"ccmp"));
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
