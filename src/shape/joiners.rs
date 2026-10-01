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
//! The Myanmar shaper also registers the features that shape a
//! syllable with `F_PER_SYLLABLE`: their lookups only match glyphs of
//! the cursor's syllable (the skipping iterator's `per_syllable`), so
//! no conjunct or ligature forms across a syllable boundary. The
//! syllables are the ones the shaper records in
//! [`Glyph::syllable`](crate::Glyph::syllable).
//!
//! [`JoinerTable`] is the per-shaper list, from the feature tables of
//! HarfBuzz's `hb-ot-shaper-arabic.cc` and `-myanmar.cc` (the `mark`
//! and `mkmk` GPOS features, manual in every shaper, live with the GPOS
//! stage). The Indic, Khmer, and USE shapers apply every GSUB feature
//! of their runs with their own feature tables
//! (`crate::ot::indic::shaper`, `crate::ot::khmer`,
//! `crate::ot::use_shaper`), so their segments take the default table
//! here. The first
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
    /// The Myanmar shaper: manual ZWJ for its basic and presentation
    /// features.
    Myanmar,
}

impl JoinerTable {
    /// The table for a segment of `script`: `arabic` when the Arabic
    /// joining pass runs for it.
    pub(crate) fn for_segment(script: Script, arabic: bool) -> Self {
        if arabic {
            Self::Arabic
        } else if script == Script::Myanmar {
            Self::Myanmar
        } else {
            Self::Default
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
    /// Myanmar `locl`, `ccmp`, and the basic features.
    fn per_syllable(self, tag: [u8; 4]) -> bool {
        match self {
            Self::Default | Self::Arabic => false,
            Self::Myanmar => matches!(
                &tag,
                b"locl" | b"ccmp" | b"rphf" | b"pref" | b"blwf" | b"pstf"
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
            Self::Myanmar => matches!(
                &tag,
                b"rphf" | b"pref" | b"blwf" | b"pstf" | b"pres" | b"abvs" | b"blws" | b"psts"
            ),
        };
        match (manual, self) {
            (false, _) => Joiners::AUTO,
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
            JoinerTable::Myanmar.joiners(*b"blwf").joiners,
            Joiners::MANUAL_ZWJ
        );
        assert_eq!(
            JoinerTable::Default.joiners(*b"liga").joiners,
            Joiners::AUTO
        );
    }

    #[test]
    fn syllabic_shapers_mark_their_syllable_features_per_syllable() {
        let per = |t: JoinerTable, tag: &[u8; 4]| t.joiners(*tag).per_syllable;
        // hb-ot-shaper-myanmar.cc: locl, ccmp and the basic features.
        assert!(per(JoinerTable::Myanmar, b"rphf") && per(JoinerTable::Myanmar, b"pstf"));
        assert!(!per(JoinerTable::Myanmar, b"pres") && !per(JoinerTable::Myanmar, b"blws"));
        assert!(!per(JoinerTable::Arabic, b"ccmp") && !per(JoinerTable::Default, b"ccmp"));
    }

    #[test]
    fn segments_pick_their_shapers_table() {
        let t = JoinerTable::for_segment;
        assert_eq!(t(Script::Arabic, true), JoinerTable::Arabic);
        assert_eq!(t(Script::Myanmar, false), JoinerTable::Myanmar);
        // The Indic, Khmer, and USE shapers bring their own tables.
        assert_eq!(t(Script::Devanagari, false), JoinerTable::Default);
        assert_eq!(t(Script::Khmer, false), JoinerTable::Default);
        assert_eq!(t(Script::Sinhala, false), JoinerTable::Default);
        assert_eq!(t(Script::Mongolian, false), JoinerTable::Default);
        assert_eq!(t(Script::Latin, false), JoinerTable::Default);
    }
}
