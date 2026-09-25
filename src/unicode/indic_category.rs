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
//! covering the scripts it can shape: the full Indic family at 0.2.0.
//!
//! # Layout
//!
//! Both categories are returned for any `char`; unknown codepoints
//! map to [`IndicSyllabicCategory::Other`] and
//! [`IndicPositionalCategory::NotApplicable`] respectively. The
//! shaper treats those as pass-through, so uncovered scripts flow
//! through the generic path unchanged.
//!
//! Each Unicode block has its own pair of match arms below. The
//! script-agnostic classifiers (zero-width joiner / non-joiner,
//! dotted circle) are shared at the top of each function.
//!
//! Ranges are cross-referenced with the Unicode Character Database
//! and rustybuzz's `ot_shaper_indic_table.rs`. Simplified here
//! compared to the full UCD: sigilbuzz collapses several matra
//! positional sub-categories (`VPre`, `VPst`, `VBlw`, `VAbv`) into
//! the coarser positional `Left`/`Right`/`Bottom`/`Top` set that
//! the state machine actually consumes.

/// Indic Syllabic Category: the role a codepoint plays inside an
/// Indic syllable. Variants mirror the UAX #44 `Indic_Syllabic_Category`
/// enumeration; only the values sigilbuzz uses today are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum IndicSyllabicCategory {
    /// Anything we have not tabulated: passes through untouched.
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

/// Indic Positional Category: where a mark sits relative to its
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

    // Script-agnostic classifiers that apply across every Indic block.
    match cp {
        0x25CC => return IndicSyllabicCategory::ConsonantPlaceholder,
        0x200C => return IndicSyllabicCategory::NonJoiner,
        0x200D => return IndicSyllabicCategory::Joiner,
        _ => {}
    }

    match cp {
        0x0900..=0x097F => devanagari_syllabic(cp),
        0x0980..=0x09FF => bengali_syllabic(cp),
        0x0A00..=0x0A7F => gurmukhi_syllabic(cp),
        0x0A80..=0x0AFF => gujarati_syllabic(cp),
        0x0B00..=0x0B7F => oriya_syllabic(cp),
        0x0B80..=0x0BFF => tamil_syllabic(cp),
        0x0C00..=0x0C7F => telugu_syllabic(cp),
        0x0C80..=0x0CFF => kannada_syllabic(cp),
        0x0D00..=0x0D7F => malayalam_syllabic(cp),
        0x0D80..=0x0DFF => sinhala_syllabic(cp),
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
        0x0900..=0x097F => devanagari_positional(cp),
        0x0980..=0x09FF => bengali_positional(cp),
        0x0A00..=0x0A7F => gurmukhi_positional(cp),
        0x0A80..=0x0AFF => gujarati_positional(cp),
        0x0B00..=0x0B7F => oriya_positional(cp),
        0x0B80..=0x0BFF => tamil_positional(cp),
        0x0C00..=0x0C7F => telugu_positional(cp),
        0x0C80..=0x0CFF => kannada_positional(cp),
        0x0D00..=0x0D7F => malayalam_positional(cp),
        0x0D80..=0x0DFF => sinhala_positional(cp),
        _ => IndicPositionalCategory::NotApplicable,
    }
}

// -----------------------------------------------------------------
// Devanagari (U+0900..U+097F)
// -----------------------------------------------------------------

const fn devanagari_syllabic(cp: u32) -> IndicSyllabicCategory {
    match cp {
        0x0900..=0x0902 => IndicSyllabicCategory::Bindu,
        0x0903 => IndicSyllabicCategory::Visarga,
        0x0904..=0x0914 | 0x0960 | 0x0961 => IndicSyllabicCategory::VowelIndependent,
        0x0915..=0x0939 | 0x0958..=0x095F | 0x0972..=0x097F => IndicSyllabicCategory::Consonant,
        0x093A | 0x093B => IndicSyllabicCategory::VowelDependent,
        0x093C => IndicSyllabicCategory::Nukta,
        0x093D => IndicSyllabicCategory::Avagraha,
        0x093E..=0x094C | 0x094E | 0x094F | 0x0955..=0x0957 | 0x0962 | 0x0963 => {
            IndicSyllabicCategory::VowelDependent
        }
        0x094D => IndicSyllabicCategory::Virama,
        0x0951..=0x0954 => IndicSyllabicCategory::CantillationMark,
        0x0966..=0x096F => IndicSyllabicCategory::Number,
        // OM (0x0950), dandas (0x0964..=0x0965), abbreviation sign
        // (0x0970) pass through as "Other": no reorder, no feature.
        // 0x0971 (high spacing dot) is a consonant placeholder.
        0x0971 => IndicSyllabicCategory::ConsonantPlaceholder,
        _ => IndicSyllabicCategory::Other,
    }
}

const fn devanagari_positional(cp: u32) -> IndicPositionalCategory {
    match cp {
        0x093A | 0x0945..=0x0948 | 0x0951 | 0x0953 | 0x0954 | 0x0955 => {
            IndicPositionalCategory::Top
        }
        0x093F | 0x094E => IndicPositionalCategory::Left,
        0x093B | 0x093E | 0x0940 | 0x0949 | 0x094A | 0x094B | 0x094C | 0x094F => {
            IndicPositionalCategory::Right
        }
        0x093C | 0x0941 | 0x0942 | 0x0943 | 0x0944 | 0x094D | 0x0952 | 0x0956 | 0x0957 | 0x0962
        | 0x0963 => IndicPositionalCategory::Bottom,
        _ => IndicPositionalCategory::NotApplicable,
    }
}

// -----------------------------------------------------------------
// Bengali (U+0980..U+09FF)
// -----------------------------------------------------------------

const fn bengali_syllabic(cp: u32) -> IndicSyllabicCategory {
    match cp {
        0x0980 => IndicSyllabicCategory::ConsonantPlaceholder,
        0x0981 | 0x0982 => IndicSyllabicCategory::Bindu,
        0x0983 => IndicSyllabicCategory::Visarga,
        0x0985..=0x0994 | 0x09E0 | 0x09E1 => IndicSyllabicCategory::VowelIndependent,
        0x0995..=0x09B9 | 0x09DC | 0x09DD | 0x09DF | 0x09F0 | 0x09F1 => {
            IndicSyllabicCategory::Consonant
        }
        0x09BC => IndicSyllabicCategory::Nukta,
        0x09BD => IndicSyllabicCategory::Avagraha,
        0x09BE..=0x09CC | 0x09D7 | 0x09E2 | 0x09E3 => IndicSyllabicCategory::VowelDependent,
        0x09CD => IndicSyllabicCategory::Virama,
        0x09E6..=0x09EF => IndicSyllabicCategory::Number,
        _ => IndicSyllabicCategory::Other,
    }
}

const fn bengali_positional(cp: u32) -> IndicPositionalCategory {
    match cp {
        // Vowel sign I (pre-base), candra e (pre-base).
        0x09BF | 0x09C7 | 0x09C8 => IndicPositionalCategory::Left,
        // Post-base matras: AA, aa-extender, O, AU.
        0x09BE | 0x09CB | 0x09CC | 0x09D7 => IndicPositionalCategory::Right,
        // Above: reserved candrabindu-style marks we don't annotate here.
        // Below-base: halant, nukta, u/uu/r/rr vocal.
        0x09BC | 0x09C1 | 0x09C2 | 0x09C3 | 0x09C4 | 0x09CD | 0x09E2 | 0x09E3 => {
            IndicPositionalCategory::Bottom
        }
        _ => IndicPositionalCategory::NotApplicable,
    }
}

// -----------------------------------------------------------------
// Gurmukhi (U+0A00..U+0A7F)
// -----------------------------------------------------------------

const fn gurmukhi_syllabic(cp: u32) -> IndicSyllabicCategory {
    match cp {
        0x0A01 | 0x0A02 | 0x0A70 | 0x0A71 => IndicSyllabicCategory::Bindu,
        0x0A03 => IndicSyllabicCategory::Visarga,
        0x0A05..=0x0A14 => IndicSyllabicCategory::VowelIndependent,
        0x0A15..=0x0A39 | 0x0A59..=0x0A5E => IndicSyllabicCategory::Consonant,
        0x0A3C => IndicSyllabicCategory::Nukta,
        0x0A3E..=0x0A4C | 0x0A51 => IndicSyllabicCategory::VowelDependent,
        0x0A4D => IndicSyllabicCategory::Virama,
        0x0A66..=0x0A6F => IndicSyllabicCategory::Number,
        0x0A75 => IndicSyllabicCategory::ConsonantMedial,
        _ => IndicSyllabicCategory::Other,
    }
}

const fn gurmukhi_positional(cp: u32) -> IndicPositionalCategory {
    match cp {
        0x0A3F => IndicPositionalCategory::Left,
        0x0A3E | 0x0A40 => IndicPositionalCategory::Right,
        0x0A41 | 0x0A42 | 0x0A3C | 0x0A4D | 0x0A75 => IndicPositionalCategory::Bottom,
        0x0A47 | 0x0A48 | 0x0A4B | 0x0A4C | 0x0A51 | 0x0A70 | 0x0A71 => {
            IndicPositionalCategory::Top
        }
        _ => IndicPositionalCategory::NotApplicable,
    }
}

// -----------------------------------------------------------------
// Gujarati (U+0A80..U+0AFF)
// -----------------------------------------------------------------

const fn gujarati_syllabic(cp: u32) -> IndicSyllabicCategory {
    match cp {
        0x0A81 | 0x0A82 => IndicSyllabicCategory::Bindu,
        0x0A83 => IndicSyllabicCategory::Visarga,
        0x0A85..=0x0A94 | 0x0AE0 | 0x0AE1 => IndicSyllabicCategory::VowelIndependent,
        0x0A95..=0x0AB9 | 0x0AF9 => IndicSyllabicCategory::Consonant,
        0x0ABC => IndicSyllabicCategory::Nukta,
        0x0ABD => IndicSyllabicCategory::Avagraha,
        0x0ABE..=0x0ACC | 0x0AE2 | 0x0AE3 => IndicSyllabicCategory::VowelDependent,
        0x0ACD => IndicSyllabicCategory::Virama,
        0x0AE6..=0x0AEF => IndicSyllabicCategory::Number,
        0x0AFA..=0x0AFF => IndicSyllabicCategory::CantillationMark,
        _ => IndicSyllabicCategory::Other,
    }
}

const fn gujarati_positional(cp: u32) -> IndicPositionalCategory {
    match cp {
        0x0ABF => IndicPositionalCategory::Left,
        0x0ABE | 0x0AC9 | 0x0ACB | 0x0ACC => IndicPositionalCategory::Right,
        0x0ABC | 0x0AC1 | 0x0AC2 | 0x0AC3 | 0x0AC4 | 0x0ACD | 0x0AE2 | 0x0AE3 => {
            IndicPositionalCategory::Bottom
        }
        0x0AC0 | 0x0AC5 | 0x0AC7 | 0x0AC8 | 0x0ACA => IndicPositionalCategory::Top,
        _ => IndicPositionalCategory::NotApplicable,
    }
}

// -----------------------------------------------------------------
// Oriya (U+0B00..U+0B7F)
// -----------------------------------------------------------------

const fn oriya_syllabic(cp: u32) -> IndicSyllabicCategory {
    match cp {
        0x0B01 | 0x0B02 => IndicSyllabicCategory::Bindu,
        0x0B03 => IndicSyllabicCategory::Visarga,
        0x0B05..=0x0B14 | 0x0B60 | 0x0B61 => IndicSyllabicCategory::VowelIndependent,
        0x0B15..=0x0B39 | 0x0B5C | 0x0B5D | 0x0B5F | 0x0B71 => IndicSyllabicCategory::Consonant,
        0x0B3C => IndicSyllabicCategory::Nukta,
        0x0B3D => IndicSyllabicCategory::Avagraha,
        0x0B3E..=0x0B4C | 0x0B55..=0x0B57 | 0x0B62 | 0x0B63 => {
            IndicSyllabicCategory::VowelDependent
        }
        0x0B4D => IndicSyllabicCategory::Virama,
        0x0B66..=0x0B6F => IndicSyllabicCategory::Number,
        _ => IndicSyllabicCategory::Other,
    }
}

const fn oriya_positional(cp: u32) -> IndicPositionalCategory {
    match cp {
        // Vowel sign I is visually pre-base.
        0x0B47 => IndicPositionalCategory::Left,
        // Post-base matras: AA, O, AU, and the aa/u extenders.
        0x0B3E | 0x0B40 | 0x0B4B | 0x0B4C | 0x0B57 => IndicPositionalCategory::Right,
        0x0B3C | 0x0B41 | 0x0B42 | 0x0B43 | 0x0B44 | 0x0B4D | 0x0B62 | 0x0B63 => {
            IndicPositionalCategory::Bottom
        }
        0x0B3F | 0x0B48 | 0x0B55 | 0x0B56 => IndicPositionalCategory::Top,
        _ => IndicPositionalCategory::NotApplicable,
    }
}

// -----------------------------------------------------------------
// Tamil (U+0B80..U+0BFF)
// -----------------------------------------------------------------

const fn tamil_syllabic(cp: u32) -> IndicSyllabicCategory {
    match cp {
        0x0B82 => IndicSyllabicCategory::Bindu,
        0x0B83 => IndicSyllabicCategory::ModifyingLetter,
        0x0B85..=0x0B94 => IndicSyllabicCategory::VowelIndependent,
        0x0B95..=0x0BB9 => IndicSyllabicCategory::Consonant,
        0x0BBE..=0x0BCC | 0x0BD7 => IndicSyllabicCategory::VowelDependent,
        0x0BCD => IndicSyllabicCategory::Virama,
        0x0BE6..=0x0BEF => IndicSyllabicCategory::Number,
        _ => IndicSyllabicCategory::Other,
    }
}

const fn tamil_positional(cp: u32) -> IndicPositionalCategory {
    match cp {
        // Vowel signs AA/I/U/UU sit to the right of the base.
        0x0BBE..=0x0BC2 | 0x0BD7 => IndicPositionalCategory::Right,
        // E/EE/AI are pre-base.
        0x0BC6..=0x0BC8 => IndicPositionalCategory::Left,
        // O/OO/AU are two-part matras (left + right).
        0x0BCA..=0x0BCC => IndicPositionalCategory::LeftAndRight,
        0x0BCD => IndicPositionalCategory::Top,
        _ => IndicPositionalCategory::NotApplicable,
    }
}

// -----------------------------------------------------------------
// Telugu (U+0C00..U+0C7F)
// -----------------------------------------------------------------

const fn telugu_syllabic(cp: u32) -> IndicSyllabicCategory {
    match cp {
        0x0C00..=0x0C02 | 0x0C04 => IndicSyllabicCategory::Bindu,
        0x0C03 => IndicSyllabicCategory::Visarga,
        0x0C05..=0x0C14 | 0x0C60 | 0x0C61 => IndicSyllabicCategory::VowelIndependent,
        0x0C15..=0x0C39 | 0x0C58..=0x0C5A => IndicSyllabicCategory::Consonant,
        0x0C3C => IndicSyllabicCategory::Nukta,
        0x0C3D => IndicSyllabicCategory::Avagraha,
        0x0C3E..=0x0C4C | 0x0C55 | 0x0C56 | 0x0C62 | 0x0C63 => {
            IndicSyllabicCategory::VowelDependent
        }
        0x0C4D => IndicSyllabicCategory::Virama,
        0x0C66..=0x0C6F => IndicSyllabicCategory::Number,
        _ => IndicSyllabicCategory::Other,
    }
}

const fn telugu_positional(cp: u32) -> IndicPositionalCategory {
    match cp {
        // Telugu matras largely sit above or below; most "pre-base" I/II visually
        // attach via the base itself, so we mark none as Left.
        0x0C3E | 0x0C3F | 0x0C40 | 0x0C46 | 0x0C47 | 0x0C4A | 0x0C4B | 0x0C55 => {
            IndicPositionalCategory::Top
        }
        0x0C41 | 0x0C42 | 0x0C43 | 0x0C44 | 0x0C56 | 0x0C62 | 0x0C63 => {
            IndicPositionalCategory::Bottom
        }
        0x0C48 | 0x0C4C => IndicPositionalCategory::TopAndBottom,
        0x0C4D => IndicPositionalCategory::Top,
        _ => IndicPositionalCategory::NotApplicable,
    }
}

// -----------------------------------------------------------------
// Kannada (U+0C80..U+0CFF)
// -----------------------------------------------------------------

const fn kannada_syllabic(cp: u32) -> IndicSyllabicCategory {
    match cp {
        0x0C80 => IndicSyllabicCategory::ConsonantPlaceholder,
        0x0C81 | 0x0C82 => IndicSyllabicCategory::Bindu,
        0x0C83 => IndicSyllabicCategory::Visarga,
        0x0C85..=0x0C94 | 0x0CE0 | 0x0CE1 => IndicSyllabicCategory::VowelIndependent,
        0x0C95..=0x0CB9 | 0x0CDE | 0x0CF1 | 0x0CF2 => IndicSyllabicCategory::Consonant,
        0x0CBC => IndicSyllabicCategory::Nukta,
        0x0CBD => IndicSyllabicCategory::Avagraha,
        0x0CBE..=0x0CCC | 0x0CD5 | 0x0CD6 | 0x0CE2 | 0x0CE3 => {
            IndicSyllabicCategory::VowelDependent
        }
        0x0CCD => IndicSyllabicCategory::Virama,
        0x0CE6..=0x0CEF => IndicSyllabicCategory::Number,
        _ => IndicSyllabicCategory::Other,
    }
}

const fn kannada_positional(cp: u32) -> IndicPositionalCategory {
    match cp {
        0x0CBF | 0x0CC6 => IndicPositionalCategory::Top,
        0x0CBE | 0x0CC1..=0x0CC4 => IndicPositionalCategory::Right,
        0x0CC0 | 0x0CC7 | 0x0CC8 | 0x0CCA | 0x0CCB => IndicPositionalCategory::TopAndRight,
        0x0CBC | 0x0CCC | 0x0CCD | 0x0CD5 | 0x0CD6 | 0x0CE2 | 0x0CE3 => {
            IndicPositionalCategory::Bottom
        }
        _ => IndicPositionalCategory::NotApplicable,
    }
}

// -----------------------------------------------------------------
// Malayalam (U+0D00..U+0D7F)
// -----------------------------------------------------------------

const fn malayalam_syllabic(cp: u32) -> IndicSyllabicCategory {
    match cp {
        0x0D00..=0x0D02 => IndicSyllabicCategory::Bindu,
        0x0D03 => IndicSyllabicCategory::Visarga,
        0x0D04..=0x0D14 | 0x0D60 | 0x0D61 => IndicSyllabicCategory::VowelIndependent,
        // Malayalam has an explicit encoded reph at U+0D4E ("Malayalam letter
        // dot reph"). HarfBuzz treats it as a consonant; we bundle it into the
        // Consonant range alongside the script's base consonants and U+0D54..0D56
        // chillus so the syllable segmenter handles it as a base-eligible glyph.
        // Reph reorder proper (LogRepha mode) is a follow-up issue.
        0x0D15..=0x0D3A | 0x0D4E | 0x0D54..=0x0D56 => IndicSyllabicCategory::Consonant,
        0x0D3B | 0x0D3C => IndicSyllabicCategory::Nukta,
        0x0D3D => IndicSyllabicCategory::Avagraha,
        0x0D3E..=0x0D4C | 0x0D57 | 0x0D62 | 0x0D63 => IndicSyllabicCategory::VowelDependent,
        0x0D4D => IndicSyllabicCategory::Virama,
        0x0D66..=0x0D6F => IndicSyllabicCategory::Number,
        _ => IndicSyllabicCategory::Other,
    }
}

const fn malayalam_positional(cp: u32) -> IndicPositionalCategory {
    match cp {
        0x0D3F | 0x0D46 | 0x0D47 | 0x0D48 => IndicPositionalCategory::Left,
        0x0D3E | 0x0D40 | 0x0D4A | 0x0D4B | 0x0D4C | 0x0D57 => IndicPositionalCategory::Right,
        0x0D41 | 0x0D42 | 0x0D43 | 0x0D44 | 0x0D4D | 0x0D62 | 0x0D63 => {
            IndicPositionalCategory::Bottom
        }
        _ => IndicPositionalCategory::NotApplicable,
    }
}

// -----------------------------------------------------------------
// Sinhala (U+0D80..U+0DFF)
// -----------------------------------------------------------------

const fn sinhala_syllabic(cp: u32) -> IndicSyllabicCategory {
    match cp {
        0x0D81..=0x0D82 => IndicSyllabicCategory::Bindu,
        0x0D83 => IndicSyllabicCategory::Visarga,
        0x0D85..=0x0D96 => IndicSyllabicCategory::VowelIndependent,
        0x0D9A..=0x0DC6 => IndicSyllabicCategory::Consonant,
        0x0DCA => IndicSyllabicCategory::Virama,
        0x0DCF..=0x0DDF | 0x0DF2 | 0x0DF3 => IndicSyllabicCategory::VowelDependent,
        0x0DE6..=0x0DEF => IndicSyllabicCategory::Number,
        _ => IndicSyllabicCategory::Other,
    }
}

const fn sinhala_positional(cp: u32) -> IndicPositionalCategory {
    match cp {
        0x0DD9 | 0x0DDB => IndicPositionalCategory::Left,
        // Two-part matras in Sinhala.
        0x0DDA | 0x0DDC..=0x0DDE => IndicPositionalCategory::LeftAndRight,
        0x0DCF..=0x0DD1 | 0x0DDF | 0x0DF2 | 0x0DF3 => IndicPositionalCategory::Right,
        0x0DCA | 0x0DD2..=0x0DD4 | 0x0DD6 | 0x0DD8 => IndicPositionalCategory::Top,
        _ => IndicPositionalCategory::NotApplicable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devanagari_consonants_classify_as_consonant() {
        // क U+0915 .. ह U+0939 are consonants.
        assert_eq!(
            syllabic_category('\u{0915}'),
            IndicSyllabicCategory::Consonant
        );
        assert_eq!(
            syllabic_category('\u{0924}'),
            IndicSyllabicCategory::Consonant
        ); // त
        assert_eq!(
            syllabic_category('\u{0939}'),
            IndicSyllabicCategory::Consonant
        );
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
        // Thai: no table yet.
        assert_eq!(syllabic_category('\u{0E01}'), IndicSyllabicCategory::Other);
    }

    #[test]
    fn zwj_and_zwnj_have_dedicated_categories() {
        assert_eq!(
            syllabic_category('\u{200C}'),
            IndicSyllabicCategory::NonJoiner
        );
        assert_eq!(syllabic_category('\u{200D}'), IndicSyllabicCategory::Joiner);
    }

    #[test]
    fn dotted_circle_is_a_consonant_placeholder() {
        assert_eq!(
            syllabic_category('\u{25CC}'),
            IndicSyllabicCategory::ConsonantPlaceholder
        );
    }

    #[test]
    fn bengali_ra_classifies_as_consonant() {
        // U+09B0 BENGALI LETTER RA.
        assert_eq!(
            syllabic_category('\u{09B0}'),
            IndicSyllabicCategory::Consonant
        );
        // U+09CD BENGALI SIGN VIRAMA.
        assert_eq!(syllabic_category('\u{09CD}'), IndicSyllabicCategory::Virama);
    }

    #[test]
    fn bengali_pre_base_matra_is_left() {
        // U+09BF BENGALI VOWEL SIGN I.
        assert_eq!(
            positional_category('\u{09BF}'),
            IndicPositionalCategory::Left
        );
    }

    #[test]
    fn gurmukhi_ra_classifies_as_consonant() {
        // U+0A30 GURMUKHI LETTER RA.
        assert_eq!(
            syllabic_category('\u{0A30}'),
            IndicSyllabicCategory::Consonant
        );
        assert_eq!(syllabic_category('\u{0A4D}'), IndicSyllabicCategory::Virama);
    }

    #[test]
    fn gujarati_consonant_and_virama() {
        assert_eq!(
            syllabic_category('\u{0AAA}'),
            IndicSyllabicCategory::Consonant
        );
        assert_eq!(syllabic_category('\u{0ACD}'), IndicSyllabicCategory::Virama);
        assert_eq!(
            positional_category('\u{0ABF}'),
            IndicPositionalCategory::Left
        );
    }

    #[test]
    fn oriya_consonant_and_virama() {
        assert_eq!(
            syllabic_category('\u{0B15}'),
            IndicSyllabicCategory::Consonant
        );
        assert_eq!(syllabic_category('\u{0B4D}'), IndicSyllabicCategory::Virama);
    }

    #[test]
    fn tamil_consonant_and_virama() {
        assert_eq!(
            syllabic_category('\u{0B95}'),
            IndicSyllabicCategory::Consonant
        );
        assert_eq!(syllabic_category('\u{0BCD}'), IndicSyllabicCategory::Virama);
    }

    #[test]
    fn telugu_consonant_and_virama() {
        assert_eq!(
            syllabic_category('\u{0C15}'),
            IndicSyllabicCategory::Consonant
        );
        assert_eq!(syllabic_category('\u{0C4D}'), IndicSyllabicCategory::Virama);
    }

    #[test]
    fn kannada_consonant_and_virama() {
        assert_eq!(
            syllabic_category('\u{0C95}'),
            IndicSyllabicCategory::Consonant
        );
        assert_eq!(syllabic_category('\u{0CCD}'), IndicSyllabicCategory::Virama);
    }

    #[test]
    fn malayalam_consonant_and_virama() {
        assert_eq!(
            syllabic_category('\u{0D15}'),
            IndicSyllabicCategory::Consonant
        );
        assert_eq!(syllabic_category('\u{0D4D}'), IndicSyllabicCategory::Virama);
    }

    #[test]
    fn sinhala_consonant_and_virama() {
        assert_eq!(
            syllabic_category('\u{0D9A}'),
            IndicSyllabicCategory::Consonant
        );
        assert_eq!(syllabic_category('\u{0DCA}'), IndicSyllabicCategory::Virama);
    }
}
