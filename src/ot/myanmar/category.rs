//! The character categories and positions the Myanmar syllable
//! scanner reads, for the Myanmar blocks (U+1000..U+109F, Extended-A
//! U+AA60..U+AA7F, Extended-B U+A9E0..U+A9FF), the joiners, the
//! variation selectors, and the dotted circle. Every other character
//! is [`Category::O`].

/// The role a character plays in a Myanmar syllable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Category {
    /// Consonant: anchors a syllable.
    B,
    /// Independent vowel.
    IV,
    /// Digit.
    N,
    /// Generic base: punctuation, symbols, and the dotted circle.
    GB,
    /// Virama or asat.
    H,
    /// Pre-base vowel sign.
    VPre,
    /// Above-base vowel sign.
    VAbv,
    /// Below-base vowel sign.
    VBlw,
    /// Post-base vowel sign.
    VPst,
    /// Tone mark and other modifiers.
    M,
    /// Syllable-final mark (anusvara, dot below, visarga).
    FM,
    /// Medial consonant.
    CM,
    /// Variation selector.
    VS,
    /// Zero-width non-joiner.
    Zwnj,
    /// Zero-width joiner.
    Zwj,
    /// Anything else.
    O,
}

/// The category of `ch`.
pub(crate) const fn category(ch: char) -> Category {
    match ch as u32 {
        0x200C => Category::Zwnj,
        0x200D => Category::Zwj,
        0xFE00..=0xFE0F | 0xE0100..=0xE01EF => Category::VS,
        0x25CC => Category::GB,
        0x1000..=0x1020
        | 0x103F
        | 0x1050..=0x1055
        | 0x105A..=0x105D
        | 0x1061
        | 0x1065..=0x1066
        | 0x106E..=0x1070
        | 0x1075..=0x1081
        | 0x108E => Category::B,
        0x1021..=0x102A => Category::IV,
        0x102B | 0x102C => Category::VPst,
        0x102D | 0x102E => Category::VAbv,
        0x102F | 0x1030 => Category::VBlw,
        0x1031 => Category::VPre,
        0x1032..=0x1035 => Category::VAbv,
        0x1036..=0x1038 => Category::FM,
        0x1039..=0x103A => Category::H,
        0x103B..=0x103E => Category::CM,
        0x1040..=0x1049 | 0x1090..=0x1099 => Category::N,
        0x104A..=0x104F => Category::GB,
        0x1056..=0x1057 => Category::VPst,
        0x1058..=0x1059 => Category::VBlw,
        0x105E..=0x1060 => Category::CM,
        0x1062..=0x1064 | 0x1067..=0x1068 | 0x1083..=0x1084 => Category::VPst,
        0x1071..=0x1074 | 0x1085..=0x1086 | 0x108D => Category::VAbv,
        0x1069..=0x106D | 0x1087..=0x108C | 0x108F | 0x109A..=0x109B => Category::M,
        0x1082 => Category::CM,
        0x109C => Category::VPst,
        0x109D => Category::VAbv,
        0x109E..=0x109F => Category::GB,
        0xAA60..=0xAA6F | 0xAA71..=0xAA76 | 0xAA7A | 0xAA7E..=0xAA7F => Category::B,
        0xAA70 => Category::CM,
        0xAA77..=0xAA79 => Category::GB,
        0xAA7B | 0xAA7D => Category::M,
        0xAA7C => Category::VAbv,
        0xA9E0..=0xA9E4 | 0xA9E7..=0xA9EF | 0xA9FA..=0xA9FE => Category::B,
        0xA9E5 => Category::VAbv,
        0xA9E6 => Category::CM,
        0xA9F0..=0xA9F9 => Category::N,
        _ => Category::O,
    }
}

/// True for the characters that render before the base: the vowel
/// sign e (U+1031) and the medial ra (U+103C).
pub(crate) const fn is_pre_base(ch: char) -> bool {
    matches!(ch as u32, 0x1031 | 0x103C)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn myanmar_letters_and_signs() {
        assert_eq!(category('\u{1000}'), Category::B); // ka
        assert_eq!(category('\u{103F}'), Category::B); // great sa
        assert_eq!(category('\u{1021}'), Category::IV); // a
        assert_eq!(category('\u{102C}'), Category::VPst); // aa
        assert_eq!(category('\u{102D}'), Category::VAbv); // i
        assert_eq!(category('\u{102F}'), Category::VBlw); // u
        assert_eq!(category('\u{1031}'), Category::VPre); // e
        assert_eq!(category('\u{1039}'), Category::H); // virama
        assert_eq!(category('\u{103A}'), Category::H); // asat
        assert_eq!(category('\u{103C}'), Category::CM); // medial ra
        assert_eq!(category('\u{1036}'), Category::FM); // anusvara
        assert_eq!(category('\u{1040}'), Category::N); // digit zero
        assert_eq!(category('\u{AA60}'), Category::B);
        assert_eq!(category('\u{A9E5}'), Category::VAbv);
        assert!(is_pre_base('\u{1031}') && is_pre_base('\u{103C}'));
        assert!(!is_pre_base('\u{102C}'));
    }

    #[test]
    fn joiners_selectors_and_others() {
        assert_eq!(category('\u{200C}'), Category::Zwnj);
        assert_eq!(category('\u{200D}'), Category::Zwj);
        assert_eq!(category('\u{FE00}'), Category::VS);
        assert_eq!(category('\u{25CC}'), Category::GB);
        assert_eq!(category('A'), Category::O);
        assert_eq!(category('\u{1780}'), Category::O);
    }
}
