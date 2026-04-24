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
        // PostBase: 17B6 sign aa, 17C4/17C5 signs oo/au
        // AboveBase: 17B7..17BA (i/ii/y/yy) and 17BE..17C0 (oe/ya/ie)
        // BelowBase: 17BB..17BD (u/uu/ua)
        // PreBase:   17C1..17C3 (e/ai/am-prefix)
        0x17B6 | 0x17C4 | 0x17C5 => UsePosition::PostBase,
        0x17B7..=0x17BA | 0x17BE..=0x17C0 => UsePosition::AboveBase,
        0x17BB..=0x17BD => UsePosition::BelowBase,
        0x17C1..=0x17C3 => UsePosition::PreBase,

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

        // --- Bases ------------------------------------------------
        // Khmer consonants (U+1780..U+17A4, incl. deprecated inherent-
        // vowel consonants 17A3 / 17A4 which rustybuzz still treats
        // as bases) plus U+17DC avakrahasanya.
        0x1780..=0x17A4 | 0x17DC => UseCategory::B,
        // Independent vowels U+17A5..U+17B3.
        0x17A5..=0x17B3 => UseCategory::IV,
        // --- Generic bases ----------------------------------------
        // Dotted circle, Khmer inherent-vowel signs (17B4/17B5), the
        // riel currency (17DB), and the Khmer Symbols block.
        0x25CC | 0x17B4..=0x17B5 | 0x17DB | 0x19E0..=0x19FF => UseCategory::GB,
        // --- Dependent vowel signs, grouped by visual position. ---
        //   VPst (post-base):  17B6, 17C4, 17C5
        //   VAbv (above-base): 17B7..17BA and 17BE..17C0
        //   VBlw (below-base): 17BB..17BD
        //   VPre (pre-base):   17C1..17C3
        0x17B6 | 0x17C4 | 0x17C5 => UseCategory::VPst,
        0x17B7..=0x17BA | 0x17BE..=0x17C0 => UseCategory::VAbv,
        0x17BB..=0x17BD => UseCategory::VBlw,
        0x17C1..=0x17C3 => UseCategory::VPre,
        // --- Final marks ------------------------------------------
        // Nikahit, reahmuk, yuukaleapintu — syllable-final.
        0x17C6..=0x17C8 => UseCategory::FM,
        // --- Modifying marks --------------------------------------
        // Register shifters (muusikatoan, triisap), bantoc, robat,
        // toandakhiat, kakabat, ahsda, samyok sannya, viriam,
        // bathamasat, lek too, atthacan.
        0x17C9..=0x17D1 | 0x17D3 | 0x17D7 | 0x17DD => UseCategory::M,
        // --- Coeng — the Khmer virama / subscript-joiner. ---------
        0x17D2 => UseCategory::H,
        // Khmer punctuation (17D4..17D6 khan/bariyoosan/camnuc and
        // 17D8..17DA beyyal/phnaek/koomuut) falls through the
        // wildcard below to UseCategory::O — a match arm here would
        // be identical to the fallback, so we keep the dispatch
        // lean.
        // --- Numbers ----------------------------------------------
        // Khmer digits + Khmer numerals for divination.
        0x17E0..=0x17E9 | 0x17F0..=0x17F9 => UseCategory::N,

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
