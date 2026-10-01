//! Which GSUB features handle ZWJ and ZWNJ themselves, and which
//! match within one syllable.
//!
//! HarfBuzz's shapers register some features with `F_MANUAL_ZWJ` or
//! `F_MANUAL_ZWNJ` (`F_MANUAL_JOINERS` for both): the lookups of such a
//! feature see a joiner as an ordinary glyph instead of skipping it
//! (see [`Joiners`]). Arabic does it for its ligating features, since
//! a ZWJ there should also keep letters apart. The Indic-family
//! shapers do it for the features that form conjuncts, so ZWJ and ZWNJ
//! select half forms and block conjuncts the way the scripts' encoding
//! models say. Every other feature, including all the default ones,
//! skips joiners automatically.
//!
//! The syllable-based shapers also register the features that shape a
//! syllable with `F_PER_SYLLABLE`: their lookups only match glyphs of
//! the cursor's syllable (the skipping iterator's `per_syllable`), so
//! no conjunct or ligature forms across a syllable boundary. The
//! syllables are the ones the shaper records in
//! [`Glyph::syllable`](crate::Glyph::syllable).
//!
//! [`JoinerTable`] is the per-shaper list, from the feature table of
//! HarfBuzz's `hb-ot-shaper-arabic.cc` (the `mark` and `mkmk` GPOS
//! features, manual in every shaper, live with the GPOS stage). The
//! Indic, Khmer, Myanmar, and USE shapers apply every GSUB feature of
//! their runs with their own feature tables
//! (`crate::ot::indic::shaper`, `crate::ot::khmer`,
//! `crate::ot::myanmar`, `crate::ot::use_shaper`), so their segments
//! take the default table here. The first registration of a tag
//! decides its flags, and shapers register theirs before the default
//! features, so a shaper's flags win for the default tags it also
//! lists (`ccmp`, `liga`, ... in Arabic).

use crate::tables::layout::Joiners;

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
}

impl JoinerTable {
    /// The table for a segment: `arabic` when the Arabic joining pass
    /// runs for it.
    pub(crate) fn for_segment(arabic: bool) -> Self {
        if arabic {
            Self::Arabic
        } else {
            Self::Default
        }
    }

    /// The flags of feature `tag` in this shaper. No feature of these
    /// shapers matches per syllable.
    pub(crate) fn joiners(self, tag: [u8; 4]) -> FeatureFlags {
        FeatureFlags {
            joiners: self.manual_joiners(tag),
            per_syllable: false,
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
        };
        if manual {
            Joiners::MANUAL_ZWJ
        } else {
            Joiners::AUTO
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
            JoinerTable::Default.joiners(*b"liga").joiners,
            Joiners::AUTO
        );
        assert!(!JoinerTable::Arabic.joiners(*b"ccmp").per_syllable);
        assert!(!JoinerTable::Default.joiners(*b"ccmp").per_syllable);
    }

    #[test]
    fn segments_pick_their_shapers_table() {
        assert_eq!(JoinerTable::for_segment(true), JoinerTable::Arabic);
        // The Indic, Khmer, Myanmar, and USE shapers bring their own
        // tables.
        assert_eq!(JoinerTable::for_segment(false), JoinerTable::Default);
    }
}
