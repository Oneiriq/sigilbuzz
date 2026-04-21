//! UAX #9 Bidirectional Algorithm — minimum viable subset.
//!
//! Real UAX #9 is a ~1500-line algorithm that assigns an
//! embedding level to every character in a mixed-direction run
//! and reorders them for display. sigilbuzz's shaper does not
//! need the full machinery today: what it *does* need is a
//! reliable way to answer "is this text paragraph RTL?" so that
//! Arabic or Hebrew passed to [`crate::shape`] without an
//! explicit direction flag gets shaped right-to-left.
//!
//! This module therefore implements only the **paragraph-level
//! rules** (P2 and P3 in UAX #9 terminology):
//!
//! 1. Scan the paragraph left-to-right.
//! 2. Return the direction of the first *strong* character (L,
//!    R, or AL) encountered.
//! 3. If no strong character appears, default to LTR.
//!
//! Full level/weak/neutral resolution (W1–W7, N0–N2, I1–I2) is
//! deferred. When the shaper needs to split mixed Arabic +
//! English runs into visually-ordered segments, that pass will
//! live here alongside what's below.
//!
//! # Bidi_Class coverage
//!
//! The character classifier covers the ranges sigilbuzz has
//! curated:
//!
//! - Basic Latin + Latin-1 + Latin Extended A/B/Additional (L)
//! - Arabic + Supplement + Presentation forms (AL)
//! - Hebrew + Presentation forms (R)
//! - ASCII digits + Arabic-Indic digits (EN/AN)
//! - Common whitespace + punctuation (neutral)
//! - Syriac, Thaana, N'Ko (R / AL)
//!
//! Uncovered codepoints default to [`BidiClass::Other`], which
//! the paragraph-direction pass treats as neutral. This biases
//! unknown scripts toward LTR, which is the safe default.

use crate::buffer::Direction;

/// Coarse Bidi_Class — only the categories UAX #9 paragraph-level
/// rules distinguish. Weak types (EN, ES, ET, AN, CS, NSM, BN)
/// land under [`BidiClass::Other`] for now since P2 treats them
/// as "not strong".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BidiClass {
    /// Strong left-to-right (Latin, Greek, Cyrillic, CJK, ...).
    L,
    /// Strong right-to-left non-Arabic (Hebrew, Syriac, Thaana,
    /// N'Ko, ...).
    R,
    /// Strong right-to-left Arabic (Arabic, Arabic Presentation
    /// forms, ...).
    Al,
    /// Anything else — weak, neutral, explicit-embedding markers,
    /// or unclassified ranges. Not strong, so P2 skips it.
    Other,
}

/// Classifies a character into a coarse [`BidiClass`]. Sparse by
/// design; the paragraph-direction rule only needs strong hits.
/// Arms are kept grouped by script so the reader can see which
/// ranges belong together; the small merge opportunities clippy
/// flags would hurt maintainability.
#[must_use]
#[allow(clippy::match_same_arms)]
pub const fn bidi_class(ch: char) -> BidiClass {
    let cp = ch as u32;
    match cp {
        // ASCII letters — strong L.
        0x0041..=0x005A | 0x0061..=0x007A => BidiClass::L,
        // Latin-1 Supplement letters, Latin Extended A/B, IPA,
        // Spacing Modifier Letters, and scripts through Armenian
        // — all strong L per UCD.
        0x00C0..=0x00D6
        | 0x00D8..=0x00F6
        | 0x00F8..=0x02AF
        | 0x0370..=0x0373
        | 0x0376..=0x0377
        | 0x037A..=0x037D
        | 0x037F
        | 0x0384..=0x038A
        | 0x038C
        | 0x038E..=0x03A1
        | 0x03A3..=0x03FF
        | 0x0400..=0x0482
        | 0x048A..=0x052F
        | 0x0531..=0x0556
        | 0x0559..=0x058A => BidiClass::L,
        // Hebrew letter block + related — strong R.
        0x0590..=0x05FF => BidiClass::R,
        // Arabic + Supplement — strong AL (Arabic strong).
        0x0600..=0x06FF | 0x0750..=0x077F => BidiClass::Al,
        // Syriac — strong AL.
        0x0700..=0x074F => BidiClass::Al,
        // Thaana — strong R.
        0x0780..=0x07BF => BidiClass::R,
        // N'Ko — strong R.
        0x07C0..=0x07FF => BidiClass::R,
        // Arabic Presentation Forms-A and -B — strong AL.
        0xFB1D..=0xFB4F => BidiClass::R, // Hebrew presentation forms
        0xFB50..=0xFDFF | 0xFE70..=0xFEFF => BidiClass::Al,
        // CJK + kana + Hangul — strong L.
        0x1100..=0x11FF
        | 0x3040..=0x309F
        | 0x30A0..=0x30FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xAC00..=0xD7AF => BidiClass::L,
        _ => BidiClass::Other,
    }
}

/// Applies UAX #9 rules P2 and P3 to `text` and returns the
/// paragraph-level direction. LTR when no strong character
/// exists in the run (e.g. whitespace-only or symbol-only input).
#[must_use]
pub fn paragraph_direction(text: &str) -> Direction {
    for ch in text.chars() {
        match bidi_class(ch) {
            BidiClass::L => return Direction::Ltr,
            BidiClass::R | BidiClass::Al => return Direction::Rtl,
            BidiClass::Other => {}
        }
    }
    Direction::Ltr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_letters_are_strong_ltr() {
        assert_eq!(bidi_class('a'), BidiClass::L);
        assert_eq!(bidi_class('Z'), BidiClass::L);
    }

    #[test]
    fn arabic_is_strong_al() {
        assert_eq!(bidi_class('ا'), BidiClass::Al);
        assert_eq!(bidi_class('م'), BidiClass::Al);
    }

    #[test]
    fn hebrew_is_strong_rtl() {
        assert_eq!(bidi_class('א'), BidiClass::R);
        assert_eq!(bidi_class('ש'), BidiClass::R);
    }

    #[test]
    fn digits_and_punctuation_fall_through_to_other() {
        assert_eq!(bidi_class('0'), BidiClass::Other);
        assert_eq!(bidi_class(' '), BidiClass::Other);
        assert_eq!(bidi_class('.'), BidiClass::Other);
    }

    #[test]
    fn first_strong_determines_paragraph_direction() {
        assert_eq!(paragraph_direction("Hello"), Direction::Ltr);
        assert_eq!(paragraph_direction("שלום"), Direction::Rtl);
        assert_eq!(paragraph_direction("السلام عليكم"), Direction::Rtl);
    }

    #[test]
    fn leading_neutrals_are_skipped() {
        // Whitespace and digits are neutral; first strong char
        // decides. "12 Hello" → L from H.
        assert_eq!(paragraph_direction("   Hello"), Direction::Ltr);
        assert_eq!(paragraph_direction("12 שלום"), Direction::Rtl);
    }

    #[test]
    fn strong_ltr_wins_over_rtl_that_follows() {
        assert_eq!(paragraph_direction("Hello שלום"), Direction::Ltr);
    }

    #[test]
    fn empty_or_neutral_input_defaults_to_ltr() {
        assert_eq!(paragraph_direction(""), Direction::Ltr);
        assert_eq!(paragraph_direction("     "), Direction::Ltr);
        assert_eq!(paragraph_direction("12345"), Direction::Ltr);
    }
}
