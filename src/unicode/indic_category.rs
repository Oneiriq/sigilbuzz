//! Indic_Syllabic_Category (ISC) and Indic_Positional_Category (IPC).
//!
//! The Indic shaper needs per-codepoint syllabic role (consonant,
//! vowel, matra, nukta, halant, ...) and matra positional role
//! (pre-base, post-base, above-base, below-base) to run its
//! state-machine-driven syllable segmentation and reordering.
//!
//! These two properties are published in the Unicode Character
//! Database (`IndicSyllabicCategory.txt`, `IndicPositionalCategory.txt`)
//! and are OSI-approved data. sigilbuzz carries a hand-curated excerpt
//! covering the scripts it can shape — Devanagari at M4, the rest as
//! tables land.
//!
//! # Layout
//!
//! Both categories are returned for any `char`; unknown codepoints
//! map to [`IndicSyllabicCategory::Other`] and
//! [`IndicPositionalCategory::NotApplicable`] respectively. The
//! shaper treats those as pass-through, so uncovered scripts flow
//! through the generic path unchanged.

/// Indic Syllabic Category — the role a codepoint plays inside an
/// Indic syllable. Variants mirror the UAX #44 `Indic_Syllabic_Category`
/// enumeration; only the values sigilbuzz uses today are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum IndicSyllabicCategory {
    /// Anything we have not tabulated — passes through untouched.
    Other,
    Bindu,
    Visarga,
    Avagraha,
    Nukta,
    Virama,
    PureKiller,
    InvisibleStacker,
    VowelIndependent,
    VowelDependent,
    Vowel,
    ConsonantPlaceholder,
    Consonant,
    ConsonantDead,
    ConsonantWithStacker,
    ConsonantPrefixed,
    ConsonantPreceding,
    ConsonantSucceeding,
    ConsonantSubjoined,
    ConsonantMedial,
    ConsonantFinal,
    ConsonantHeadLetter,
    ConsonantInitialPostfixed,
    ModifyingLetter,
    ToneLetter,
    ToneMark,
    GeminationMark,
    CantillationMark,
    RegisterShifter,
    SyllableModifier,
    ConsonantKiller,
    Number,
    BrahmiJoiningNumber,
    NumberJoiner,
    Joiner,
    NonJoiner,
    Symbol,
    SymbolLetter,
}

/// Indic Positional Category — where a mark sits relative to its
/// base consonant. Used by the Indic shaper to decide which feature
/// bucket a matra belongs in (pre-base, below-base, post-base...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum IndicPositionalCategory {
    /// Default for codepoints without a position annotation.
    NotApplicable,
    Right,
    Left,
    LeftAndRight,
    Top,
    Bottom,
    TopAndBottom,
    TopAndRight,
    TopAndLeft,
    TopAndLeftAndRight,
    BottomAndLeft,
    BottomAndRight,
    TopAndBottomAndRight,
    TopAndBottomAndLeft,
    Overstruck,
    VisualOrderLeft,
}

/// Returns the Indic Syllabic Category for a character.
///
/// Unknown codepoints return [`IndicSyllabicCategory::Other`].
#[must_use]
pub const fn syllabic_category(ch: char) -> IndicSyllabicCategory {
    let cp = ch as u32;
    match cp {
        // Devanagari (U+0900..U+097F). Derived from
        // IndicSyllabicCategory.txt in the Unicode Character Database.
        0x0900 | 0x0901 | 0x0902 => IndicSyllabicCategory::Bindu,
        0x0903 => IndicSyllabicCategory::Visarga,
        0x0904..=0x0914 => IndicSyllabicCategory::VowelIndependent,
        0x0915..=0x0939 => IndicSyllabicCategory::Consonant,
        0x093A => IndicSyllabicCategory::VowelDependent,
        0x093B => IndicSyllabicCategory::VowelDependent,
        0x093C => IndicSyllabicCategory::Nukta,
        0x093D => IndicSyllabicCategory::Avagraha,
        0x093E..=0x094C => IndicSyllabicCategory::VowelDependent,
        0x094D => IndicSyllabicCategory::Virama,
        0x094E | 0x094F => IndicSyllabicCategory::VowelDependent,
        0x0950 => IndicSyllabicCategory::Other, // OM
        0x0951..=0x0954 => IndicSyllabicCategory::CantillationMark,
        0x0955..=0x0957 => IndicSyllabicCategory::VowelDependent,
        0x0958..=0x095F => IndicSyllabicCategory::Consonant,
        0x0960 | 0x0961 => IndicSyllabicCategory::VowelIndependent,
        0x0962 | 0x0963 => IndicSyllabicCategory::VowelDependent,
        0x0964 | 0x0965 => IndicSyllabicCategory::Other, // Dandas
        0x0966..=0x096F => IndicSyllabicCategory::Number,
        0x0970 => IndicSyllabicCategory::Other, // Abbreviation sign
        0x0971 => IndicSyllabicCategory::ConsonantPlaceholder,
        0x0972..=0x097F => IndicSyllabicCategory::Consonant,

        // Zero-width joiner / non-joiner — shared across Indic scripts.
        0x200C => IndicSyllabicCategory::NonJoiner,
        0x200D => IndicSyllabicCategory::Joiner,

        // Dotted circle (U+25CC) sits in broken syllables as the
        // visible base for orphaned marks. Treat as placeholder.
        0x25CC => IndicSyllabicCategory::ConsonantPlaceholder,

        _ => IndicSyllabicCategory::Other,
    }
}

/// Returns the Indic Positional Category for a character.
///
/// Unknown codepoints return [`IndicPositionalCategory::NotApplicable`].
#[must_use]
pub const fn positional_category(ch: char) -> IndicPositionalCategory {
    let cp = ch as u32;
    match cp {
        // Devanagari matras and marks.
        0x093A => IndicPositionalCategory::Top,
        0x093B => IndicPositionalCategory::Right,
        0x093C => IndicPositionalCategory::Bottom,
        0x093E => IndicPositionalCategory::Right,
        0x093F => IndicPositionalCategory::Left,
        0x0940 => IndicPositionalCategory::Right,
        0x0941 | 0x0942 => IndicPositionalCategory::Bottom,
        0x0943 | 0x0944 => IndicPositionalCategory::Bottom,
        0x0945..=0x0948 => IndicPositionalCategory::Top,
        0x0949 | 0x094A => IndicPositionalCategory::Right,
        0x094B | 0x094C => IndicPositionalCategory::Right,
        0x094D => IndicPositionalCategory::Bottom,
        0x094E => IndicPositionalCategory::Left,
        0x094F => IndicPositionalCategory::Right,
        0x0951 => IndicPositionalCategory::Top,
        0x0952 => IndicPositionalCategory::Bottom,
        0x0953 | 0x0954 => IndicPositionalCategory::Top,
        0x0955 => IndicPositionalCategory::Top,
        0x0956 | 0x0957 => IndicPositionalCategory::Bottom,
        0x0962 | 0x0963 => IndicPositionalCategory::Bottom,

        _ => IndicPositionalCategory::NotApplicable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devanagari_consonants_classify_as_consonant() {
        // क U+0915 .. ह U+0939 are consonants.
        assert_eq!(syllabic_category('\u{0915}'), IndicSyllabicCategory::Consonant);
        assert_eq!(syllabic_category('\u{0924}'), IndicSyllabicCategory::Consonant); // त
        assert_eq!(syllabic_category('\u{0939}'), IndicSyllabicCategory::Consonant);
    }

    #[test]
    fn devanagari_halant_classifies_as_virama() {
        assert_eq!(syllabic_category('\u{094D}'), IndicSyllabicCategory::Virama);
    }

    #[test]
    fn devanagari_vowel_signs_classify_as_vowel_dependent() {
        // Vowel sign I (matra) is a dependent vowel.
        assert_eq!(
            syllabic_category('\u{093F}'),
            IndicSyllabicCategory::VowelDependent
        );
        // Vowel sign AA is a dependent vowel.
        assert_eq!(
            syllabic_category('\u{093E}'),
            IndicSyllabicCategory::VowelDependent
        );
    }

    #[test]
    fn pre_base_matra_has_left_positional_category() {
        // U+093F DEVANAGARI VOWEL SIGN I is visually pre-base.
        assert_eq!(
            positional_category('\u{093F}'),
            IndicPositionalCategory::Left
        );
    }

    #[test]
    fn halant_has_bottom_positional_category() {
        assert_eq!(
            positional_category('\u{094D}'),
            IndicPositionalCategory::Bottom
        );
    }

    #[test]
    fn ascii_and_unknown_scripts_pass_through() {
        assert_eq!(syllabic_category('A'), IndicSyllabicCategory::Other);
        assert_eq!(
            positional_category('A'),
            IndicPositionalCategory::NotApplicable
        );
        // Thai — no table yet.
        assert_eq!(syllabic_category('\u{0E01}'), IndicSyllabicCategory::Other);
    }

    #[test]
    fn zwj_and_zwnj_have_dedicated_categories() {
        assert_eq!(syllabic_category('\u{200C}'), IndicSyllabicCategory::NonJoiner);
        assert_eq!(syllabic_category('\u{200D}'), IndicSyllabicCategory::Joiner);
    }

    #[test]
    fn dotted_circle_is_a_consonant_placeholder() {
        assert_eq!(
            syllabic_category('\u{25CC}'),
            IndicSyllabicCategory::ConsonantPlaceholder
        );
    }
}
