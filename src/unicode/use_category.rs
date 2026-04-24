//! Universal Shaping Engine (USE) per-codepoint categorisation.
//!
//! The USE is Microsoft's generalised complex-script shaper. It covers
//! Khmer, Myanmar, Tai Tham, Buginese, Cham, New Tai Lue and several
//! others that do not fit the Arabic / Indic2 moulds. Each codepoint
//! it sees is classified along two axes:
//!
//! - [`UseCategory`] — the role the codepoint plays inside a syllable
//!   (base, halant, vowel, final mark, ...). Drives the syllable state
//!   machine and the feature masking.
//! - [`UsePosition`] — where a mark visually sits relative to its base
//!   (pre-base, above-base, below-base, post-base). Drives the reorder
//!   pass and picks the correct positional feature bucket (`abvf`,
//!   `blwf`, `pstf`, `pref`).
//!
//! sigilbuzz 0.2.0 ships Khmer coverage (U+1780..U+17FF and the Khmer
//! Symbols block U+19E0..U+19FF). Myanmar / Thai / Lao / Old Hangul /
//! Tai Tham get added incrementally against the same tables — follow-up
//! issues track the work. The tables live here rather than inside a
//! script-specific module because the same state machine consumes them
//! for every USE script.
//!
//! # Sources
//!
//! The classification mirrors the columns in the MS-published
//! `IndicSyllabicCategory.txt` / `IndicPositionalCategory.txt` files
//! combined with the USE-specific overrides listed in the MS USE docs.
//! The Khmer slice was cross-checked against `rustybuzz`'s
//! `ot_shaper_use_table.rs` (reference implementation) so the state
//! machine consumes identical categories for every Khmer codepoint.

/// USE per-codepoint category. The mnemonic names mirror the labels
/// from the MS USE documentation so OpenType spec readers can map
/// straight across.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum UseCategory {
    /// Base consonant / independent letter — anchors a syllable.
    B,
    /// Independent vowel.
    IV,
    /// Number — digits and number signs.
    N,
    /// Generic base — default for letters that do not participate in
    /// the syllable cluster (punctuation, signs that stand alone).
    GB,
    /// Repha — reordering ra-equivalent; Khmer does not use this but
    /// the category is here for future Myanmar / Devanagari-adjacent
    /// coverage.
    R,
    /// Symbol.
    S,
    /// Halant / virama — deletes the inherent vowel of the preceding
    /// base and glues it to the following consonant as a subscript.
    /// Khmer uses U+17D2 COENG for this role.
    H,
    /// Pre-base vowel sign — renders before the base visually.
    VPre,
    /// Above-base vowel sign.
    VAbv,
    /// Below-base vowel sign.
    VBlw,
    /// Post-base vowel sign — renders after the base visually.
    VPst,
    /// Modifying mark — tone marks, registers, robat, bindu-likes.
    M,
    /// Final mark — syllable-final modifiers (visarga, anusvara).
    FM,
    /// Consonant modifier / medial — e.g. Khmer coeng-forming
    /// relationship is expressed via H above; this covers scripts
    /// with explicit medial letters (Myanmar medial ya/ra/wa/ha).
    CM,
    /// Variation selector.
    VS,
    /// Zero-width non-joiner.
    ZWNJ,
    /// Zero-width joiner.
    ZWJ,
    /// Whitespace / cluster boundary.
    WS,
    /// Anything else — treated as a cluster-break / pass-through.
    O,
}

/// Where a mark sits relative to its base. Matches the IPC partition
/// used by the Indic shaper but with the USE-specific pre/below/post
/// split laid out explicitly so the reorder pass can branch cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum UsePosition {
    /// No positional role — the default for bases, whitespace, marks
    /// that attach at the overall glyph box.
    NotApplicable,
    /// Before the base visually — pre-base matras.
    PreBase,
    /// Above the base — above-base vowel signs and tone marks.
    AboveBase,
    /// Below the base — below-base vowel signs and subscript marks.
    BelowBase,
    /// After the base visually — post-base matras and final marks.
    PostBase,
}

/// Returns the USE positional category for a character.
///
/// Unknown codepoints return [`UsePosition::NotApplicable`].
#[must_use]
pub const fn use_position(ch: char) -> UsePosition {
    let cp = ch as u32;
    match cp {
        // --- Khmer dependent vowel signs --------------------------
        0x17B6 => UsePosition::PostBase, // sign aa
        0x17B7 | 0x17B8 | 0x17B9 | 0x17BA => UsePosition::AboveBase, // i/ii/y/yy
        0x17BB | 0x17BC | 0x17BD => UsePosition::BelowBase, // u/uu/ua
        0x17BE | 0x17BF | 0x17C0 => UsePosition::AboveBase, // oe/ya/ie (above glyph anchor)
        0x17C1 | 0x17C2 | 0x17C3 => UsePosition::PreBase,   // e/ai/am-prefix
        0x17C4 | 0x17C5 => UsePosition::PostBase,           // oo/au

        // Currency + signs — no positional role (the currency is a
        // base glyph itself).
        _ => UsePosition::NotApplicable,
    }
}

/// Returns the USE category for a character.
///
/// The table is structured as a series of range matches so Myanmar /
/// Tai Tham / Buginese / Thai / Lao can be added as additional match
/// arms without touching the state machine.
///
/// Unknown codepoints return [`UseCategory::O`]. sigilbuzz only has
/// data for scripts it can actively shape; everything else flows
/// through the generic GSUB/GPOS pipeline.
///
/// # Coverage
///
/// Khmer (U+1780..U+17FF) and Khmer Symbols (U+19E0..U+19FF) are
/// complete. Format characters and variation selectors carry their
/// shared USE categories.
#[must_use]
pub const fn use_category(ch: char) -> UseCategory {
    let cp = ch as u32;
    match cp {
        // --- Shared format characters -----------------------------
        0x200C => UseCategory::ZWNJ,
        0x200D => UseCategory::ZWJ,
        0xFE00..=0xFE0F | 0xE0100..=0xE01EF => UseCategory::VS,
        0x25CC => UseCategory::GB,

        // --- Khmer block (U+1780..U+17FF) -------------------------
        //
        // Consonants: U+1780..U+17A2 ka..ha. All are USE bases.
        0x1780..=0x17A2 => UseCategory::B,
        // U+17A3 / U+17A4 — deprecated inherent-vowel consonants.
        // Treated as bases for shaping (rustybuzz parity).
        0x17A3 | 0x17A4 => UseCategory::B,
        // Independent vowels U+17A5..U+17B3.
        0x17A5..=0x17B3 => UseCategory::IV,
        // U+17B4, U+17B5 inherent vowel signs — generic base.
        0x17B4 | 0x17B5 => UseCategory::GB,
        // Dependent vowel signs.
        //
        // Categorisation by position:
        //   VPst (post-base):  U+17B6, U+17C4, U+17C5
        //   VAbv (above-base): U+17B7, U+17B8, U+17B9, U+17BA,
        //                       U+17BE, U+17BF, U+17C0
        //   VBlw (below-base): U+17BB, U+17BC, U+17BD
        //   VPre (pre-base):   U+17C1, U+17C2, U+17C3
        0x17B6 | 0x17C4 | 0x17C5 => UseCategory::VPst,
        0x17B7..=0x17BA | 0x17BE..=0x17C0 => UseCategory::VAbv,
        0x17BB..=0x17BD => UseCategory::VBlw,
        0x17C1..=0x17C3 => UseCategory::VPre,
        // Khmer sign nikahit / reahmuk / yuukaleapintu / muusikatoan
        // / triisap / bantoc / robat — modifier marks and registers.
        0x17C6 => UseCategory::FM, // sign nikahit (above)
        0x17C7 | 0x17C8 => UseCategory::FM, // sign reahmuk, sign yuukaleapintu
        0x17C9 | 0x17CA => UseCategory::M,  // muusikatoan, triisap (register shifters)
        0x17CB => UseCategory::M,           // bantoc
        0x17CC => UseCategory::M,           // robat (above)
        0x17CD => UseCategory::M,           // toandakhiat
        0x17CE => UseCategory::M,           // kakabat
        0x17CF => UseCategory::M,           // ahsda
        0x17D0 => UseCategory::M,           // samyok sannya
        0x17D1 => UseCategory::M,           // viriam (kills inherent vowel)
        // Coeng — the Khmer virama / subscript-joiner.
        0x17D2 => UseCategory::H,
        0x17D3 => UseCategory::M, // bathamasat
        // U+17D4..U+17D6 are punctuation (khan, bariyoosan, camnuc).
        0x17D4..=0x17D6 => UseCategory::O,
        0x17D7 => UseCategory::M,  // lek too — iteration mark
        0x17D8..=0x17DA => UseCategory::O, // punctuation
        0x17DB => UseCategory::GB, // currency riel
        0x17DC => UseCategory::B,  // avakrahasanya — base-like
        0x17DD => UseCategory::M,  // atthacan — above
        0x17E0..=0x17E9 => UseCategory::N, // Khmer digits
        0x17F0..=0x17F9 => UseCategory::N, // Khmer numerals for divination

        // --- Khmer Symbols block (U+19E0..U+19FF) -----------------
        //
        // Lunar-date symbols — all generic bases (they render as
        // standalone ideograph-like glyphs).
        0x19E0..=0x19FF => UseCategory::GB,

        _ => UseCategory::O,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn khmer_consonant_ka_is_base() {
        assert_eq!(use_category('\u{1780}'), UseCategory::B);
        assert_eq!(use_category('\u{17A2}'), UseCategory::B); // ha
    }

    #[test]
    fn khmer_independent_vowels_are_iv() {
        assert_eq!(use_category('\u{17A5}'), UseCategory::IV);
        assert_eq!(use_category('\u{17B3}'), UseCategory::IV);
    }

    #[test]
    fn khmer_coeng_is_halant() {
        assert_eq!(use_category('\u{17D2}'), UseCategory::H);
    }

    #[test]
    fn khmer_pre_base_vowels() {
        // U+17C1 sign e, U+17C2 sign ai, U+17C3 sign am-prefix.
        assert_eq!(use_category('\u{17C1}'), UseCategory::VPre);
        assert_eq!(use_category('\u{17C2}'), UseCategory::VPre);
        assert_eq!(use_category('\u{17C3}'), UseCategory::VPre);
        assert_eq!(use_position('\u{17C1}'), UsePosition::PreBase);
    }

    #[test]
    fn khmer_above_base_vowels() {
        assert_eq!(use_category('\u{17B7}'), UseCategory::VAbv); // i
        assert_eq!(use_category('\u{17B8}'), UseCategory::VAbv); // ii
        assert_eq!(use_position('\u{17B7}'), UsePosition::AboveBase);
    }

    #[test]
    fn khmer_below_base_vowels() {
        assert_eq!(use_category('\u{17BB}'), UseCategory::VBlw); // u
        assert_eq!(use_category('\u{17BC}'), UseCategory::VBlw); // uu
        assert_eq!(use_position('\u{17BB}'), UsePosition::BelowBase);
    }

    #[test]
    fn khmer_post_base_aa_and_au() {
        assert_eq!(use_category('\u{17B6}'), UseCategory::VPst);
        assert_eq!(use_category('\u{17C4}'), UseCategory::VPst);
        assert_eq!(use_position('\u{17B6}'), UsePosition::PostBase);
    }

    #[test]
    fn khmer_nikahit_and_reahmuk_are_final_marks() {
        assert_eq!(use_category('\u{17C6}'), UseCategory::FM);
        assert_eq!(use_category('\u{17C7}'), UseCategory::FM);
    }

    #[test]
    fn khmer_register_shifters_are_modifiers() {
        // U+17C9 muusikatoan, U+17CA triisap — register shifters.
        assert_eq!(use_category('\u{17C9}'), UseCategory::M);
        assert_eq!(use_category('\u{17CA}'), UseCategory::M);
    }

    #[test]
    fn khmer_digits_are_numbers() {
        assert_eq!(use_category('\u{17E0}'), UseCategory::N);
        assert_eq!(use_category('\u{17E9}'), UseCategory::N);
    }

    #[test]
    fn khmer_symbols_block_is_generic_base() {
        assert_eq!(use_category('\u{19E0}'), UseCategory::GB);
        assert_eq!(use_category('\u{19FF}'), UseCategory::GB);
    }

    #[test]
    fn format_characters_classify_correctly() {
        assert_eq!(use_category('\u{200C}'), UseCategory::ZWNJ);
        assert_eq!(use_category('\u{200D}'), UseCategory::ZWJ);
        assert_eq!(use_category('\u{FE00}'), UseCategory::VS);
    }

    #[test]
    fn dotted_circle_is_generic_base() {
        assert_eq!(use_category('\u{25CC}'), UseCategory::GB);
    }

    #[test]
    fn latin_and_other_scripts_fall_through_to_other() {
        assert_eq!(use_category('A'), UseCategory::O);
        assert_eq!(use_category('\u{0915}'), UseCategory::O); // Devanagari
    }

    #[test]
    fn positional_defaults_to_not_applicable_for_bases() {
        assert_eq!(use_position('\u{1780}'), UsePosition::NotApplicable);
        assert_eq!(use_position('A'), UsePosition::NotApplicable);
    }
}
