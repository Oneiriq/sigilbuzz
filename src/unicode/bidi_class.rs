//! Per-codepoint Bidi_Class table (UAX #9 section 3.1).
//!
//! The table in `bidi_class_table.rs` is generated from Unicode 17.0.0
//! `DerivedBidiClass.txt` by `tests/unicode_table_gen.rs`. It covers
//! every code point: the listed values, and for the code points the
//! file does not list, the defaults its `@missing` lines give (R or AL
//! in the blocks set aside for right-to-left scripts, ET in the
//! Currency Symbols block, L everywhere else). Unassigned default
//! ignorables and noncharacters are listed as BN.

use super::bidi_class_table::BIDI_CLASSES;
use crate::buffer::Direction;

/// UAX #9 Bidi_Class: full set of categories the algorithm
/// distinguishes. The variants exactly match the names used in the
/// UAX #9 rule pseudocode (`L`, `R`, `AL`, `EN`, ...) so cross-checking
/// against the spec stays mechanical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BidiClass {
    // Strong types.
    /// Left-to-right (`L`): Latin, Greek, Cyrillic, CJK, ...
    L,
    /// Right-to-left (`R`): Hebrew and other non-Arabic RTL letters.
    R,
    /// Arabic letter (`AL`): Arabic, Syriac, Thaana, ...
    Al,
    // Weak types.
    /// European number (`EN`): ASCII and other European digits.
    En,
    /// European number separator (`ES`): plus and minus signs.
    Es,
    /// European number terminator (`ET`): currency, percent, degree.
    Et,
    /// Arabic number (`AN`): Arabic-Indic digits and separators.
    An,
    /// Common number separator (`CS`): comma, period, colon, slash.
    Cs,
    /// Nonspacing mark (`NSM`): takes the class of its base.
    Nsm,
    /// Boundary neutral (`BN`): controls and default ignorables.
    Bn,
    // Neutral types.
    /// Paragraph separator (`B`).
    B,
    /// Segment separator (`S`): tab and the like.
    S,
    /// Whitespace (`WS`).
    Ws,
    /// Other neutral (`ON`): punctuation, symbols, brackets.
    On,
    // Explicit formatting.
    /// Left-to-right embedding (`LRE`, U+202A).
    Lre,
    /// Left-to-right override (`LRO`, U+202D).
    Lro,
    /// Right-to-left embedding (`RLE`, U+202B).
    Rle,
    /// Right-to-left override (`RLO`, U+202E).
    Rlo,
    /// Pop directional formatting (`PDF`, U+202C).
    Pdf,
    /// Left-to-right isolate (`LRI`, U+2066).
    Lri,
    /// Right-to-left isolate (`RLI`, U+2067).
    Rli,
    /// First strong isolate (`FSI`, U+2068).
    Fsi,
    /// Pop directional isolate (`PDI`, U+2069).
    Pdi,
}

impl BidiClass {
    /// True for the three strong directional categories (L / R / AL).
    #[must_use]
    pub const fn is_strong(self) -> bool {
        matches!(self, Self::L | Self::R | Self::Al)
    }

    /// True for the categories that participate as RTL strong types
    /// in the implicit-level pass (R, AL).
    #[must_use]
    pub const fn is_rtl_strong(self) -> bool {
        matches!(self, Self::R | Self::Al)
    }

    /// True for the explicit-formatting characters consumed by X1-X8.
    #[must_use]
    pub const fn is_explicit(self) -> bool {
        matches!(
            self,
            Self::Lre
                | Self::Lro
                | Self::Rle
                | Self::Rlo
                | Self::Pdf
                | Self::Lri
                | Self::Rli
                | Self::Fsi
                | Self::Pdi
        )
    }

    /// True for the isolate-initiator subset (LRI / RLI / FSI).
    #[must_use]
    pub const fn is_isolate_initiator(self) -> bool {
        matches!(self, Self::Lri | Self::Rli | Self::Fsi)
    }

    /// True for codepoints that L1 resets back to the paragraph level.
    #[must_use]
    pub const fn is_l1_reset(self) -> bool {
        matches!(
            self,
            Self::B | Self::S | Self::Ws | Self::Fsi | Self::Lri | Self::Rli | Self::Pdi
        )
    }
}

/// Maps a paragraph-direction tag to its strong-class equivalent for
/// the higher-level API.
#[must_use]
pub const fn strong_for_direction(dir: Direction) -> BidiClass {
    match dir {
        Direction::Rtl => BidiClass::R,
        _ => BidiClass::L,
    }
}
/// Returns the UAX #9 Bidi_Class of `ch`, from the Unicode Character
/// Database.
///
/// ```
/// use sigilbuzz::{bidi_class, BidiClass};
///
/// assert_eq!(bidi_class('a'), BidiClass::L);
/// assert_eq!(bidi_class('\u{05D0}'), BidiClass::R);
/// assert_eq!(bidi_class('\u{0901}'), BidiClass::Nsm);
/// // Unassigned, in the Hebrew block.
/// assert_eq!(bidi_class('\u{05FF}'), BidiClass::R);
/// ```
#[must_use]
pub const fn bidi_class(ch: char) -> BidiClass {
    let cp = ch as u32;
    let table = BIDI_CLASSES;
    let (mut lo, mut hi) = (0, table.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        // `mid < hi <= table.len()`, so the index is in range.
        let (first, last, class) = table[mid];
        if cp < first {
            hi = mid;
        } else if cp > last {
            lo = mid + 1;
        } else {
            return class;
        }
    }
    BidiClass::L
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_letter_is_l() {
        assert_eq!(bidi_class('A'), BidiClass::L);
        assert_eq!(bidi_class('z'), BidiClass::L);
    }

    #[test]
    fn hebrew_letter_is_r() {
        assert_eq!(bidi_class('\u{05D0}'), BidiClass::R); // alef
        assert_eq!(bidi_class('\u{05EA}'), BidiClass::R); // tav
    }

    #[test]
    fn arabic_letter_is_al() {
        assert_eq!(bidi_class('\u{0627}'), BidiClass::Al); // alef
        assert_eq!(bidi_class('\u{0645}'), BidiClass::Al); // mim
    }

    #[test]
    fn ascii_digit_is_en() {
        assert_eq!(bidi_class('0'), BidiClass::En);
        assert_eq!(bidi_class('9'), BidiClass::En);
    }

    #[test]
    fn arabic_digit_is_an() {
        assert_eq!(bidi_class('\u{0660}'), BidiClass::An);
        assert_eq!(bidi_class('\u{0669}'), BidiClass::An);
    }

    #[test]
    fn separators_classified() {
        assert_eq!(bidi_class('+'), BidiClass::Es);
        assert_eq!(bidi_class('-'), BidiClass::Es);
        assert_eq!(bidi_class(','), BidiClass::Cs);
        assert_eq!(bidi_class('.'), BidiClass::Cs);
        assert_eq!(bidi_class(':'), BidiClass::Cs);
        assert_eq!(bidi_class('/'), BidiClass::Cs);
    }

    #[test]
    fn terminators_classified() {
        assert_eq!(bidi_class('$'), BidiClass::Et);
        assert_eq!(bidi_class('%'), BidiClass::Et);
        assert_eq!(bidi_class('#'), BidiClass::Et);
    }

    #[test]
    fn whitespace_and_paragraph() {
        assert_eq!(bidi_class(' '), BidiClass::Ws);
        assert_eq!(bidi_class('\t'), BidiClass::S);
        assert_eq!(bidi_class('\n'), BidiClass::B);
    }

    #[test]
    fn explicit_format_characters() {
        assert_eq!(bidi_class('\u{202A}'), BidiClass::Lre);
        assert_eq!(bidi_class('\u{202B}'), BidiClass::Rle);
        assert_eq!(bidi_class('\u{202C}'), BidiClass::Pdf);
        assert_eq!(bidi_class('\u{202D}'), BidiClass::Lro);
        assert_eq!(bidi_class('\u{202E}'), BidiClass::Rlo);
        assert_eq!(bidi_class('\u{2066}'), BidiClass::Lri);
        assert_eq!(bidi_class('\u{2067}'), BidiClass::Rli);
        assert_eq!(bidi_class('\u{2068}'), BidiClass::Fsi);
        assert_eq!(bidi_class('\u{2069}'), BidiClass::Pdi);
    }

    #[test]
    fn lrm_rlm_alm_strong() {
        assert_eq!(bidi_class('\u{200E}'), BidiClass::L); // LRM
        assert_eq!(bidi_class('\u{200F}'), BidiClass::R); // RLM
        assert_eq!(bidi_class('\u{061C}'), BidiClass::Al); // ALM
    }

    #[test]
    fn combining_marks_are_nsm() {
        assert_eq!(bidi_class('\u{0301}'), BidiClass::Nsm); // acute
        assert_eq!(bidi_class('\u{064E}'), BidiClass::Nsm); // fatha
    }

    #[test]
    fn classes_follow_the_ucd() {
        // Values the old hand-picked table had wrong.
        assert_eq!(bidi_class('*'), BidiClass::On);
        assert_eq!(bidi_class('\u{06F1}'), BidiClass::En); // EXTENDED ARABIC-INDIC ONE
        assert_eq!(bidi_class('\u{0901}'), BidiClass::Nsm); // DEVANAGARI CANDRABINDU
        assert_eq!(bidi_class('\u{0800}'), BidiClass::R); // SAMARITAN ALAF
        assert_eq!(bidi_class('\u{02B0}'), BidiClass::L); // MODIFIER SMALL H
        assert_eq!(bidi_class('\u{2603}'), BidiClass::On); // SNOWMAN
                                                           // Unassigned code points take their block's default.
        assert_eq!(bidi_class('\u{05FF}'), BidiClass::R);
        assert_eq!(bidi_class('\u{07BF}'), BidiClass::Al);
        assert_eq!(bidi_class('\u{20CF}'), BidiClass::Et);
        assert_eq!(bidi_class('\u{2065}'), BidiClass::Bn);
        assert_eq!(bidi_class('\u{FDD0}'), BidiClass::Bn); // noncharacter
        assert_eq!(bidi_class('\u{E0080}'), BidiClass::Bn);
        assert_eq!(bidi_class('\u{50000}'), BidiClass::L);
        assert_eq!(bidi_class('\u{10FFFF}'), BidiClass::Bn);
        assert_eq!(bidi_class('\0'), BidiClass::Bn);
    }

    #[test]
    fn lookup_works_in_const_context() {
        const ALEF: BidiClass = bidi_class('\u{05D0}');
        assert_eq!(ALEF, BidiClass::R);
    }

    #[test]
    fn brackets_are_on() {
        assert_eq!(bidi_class('('), BidiClass::On);
        assert_eq!(bidi_class(')'), BidiClass::On);
        assert_eq!(bidi_class('['), BidiClass::On);
        assert_eq!(bidi_class(']'), BidiClass::On);
    }

    #[test]
    fn strong_predicate_matches_strong_classes() {
        assert!(BidiClass::L.is_strong());
        assert!(BidiClass::R.is_strong());
        assert!(BidiClass::Al.is_strong());
        assert!(!BidiClass::En.is_strong());
        assert!(!BidiClass::On.is_strong());
    }
}
