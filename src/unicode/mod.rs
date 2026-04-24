//! Unicode property data that the shaper needs.
//!
//! Scripts (Latin, Arabic, Hangul, ...) and general categories drive
//! which feature list the shaper applies and how cluster boundaries
//! are decided. Bootstrap impl is intentionally minimal; a full table
//! of UCD-derived data lands as the shaper's needs grow.

#![allow(missing_docs)]

pub mod bidi;
pub mod indic_category;
pub mod joining;
pub mod normalize;

/// Coarse script classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    /// Basic Latin + supplements. Covers ASCII and Western European.
    Latin,
    /// CJK unified ideographs and kana.
    Han,
    /// Arabic family — Arabic, Persian, Urdu presentations.
    Arabic,
    /// Hebrew.
    Hebrew,
    /// Cyrillic.
    Cyrillic,
    /// Greek.
    Greek,
    /// Devanagari. Indic reordering shaper applies.
    Devanagari,
    /// Bengali. Indic reordering shaper applies.
    Bengali,
    /// Gurmukhi. Indic reordering shaper applies.
    Gurmukhi,
    /// Gujarati. Indic reordering shaper applies.
    Gujarati,
    /// Oriya. Indic reordering shaper applies.
    Oriya,
    /// Tamil. Indic reordering shaper applies.
    Tamil,
    /// Telugu. Indic reordering shaper applies.
    Telugu,
    /// Kannada. Indic reordering shaper applies.
    Kannada,
    /// Malayalam. Indic reordering shaper applies.
    Malayalam,
    /// Sinhala. Indic reordering shaper applies.
    Sinhala,
    /// Anything else — returned when sigilbuzz has no specialised
    /// table for the codepoint's script.
    Other,
}

impl Script {
    /// Returns `true` if the script is one of the Indic family scripts
    /// that run through the Indic reordering shaper.
    #[must_use]
    pub const fn is_indic(self) -> bool {
        matches!(
            self,
            Script::Devanagari
                | Script::Bengali
                | Script::Gurmukhi
                | Script::Gujarati
                | Script::Oriya
                | Script::Tamil
                | Script::Telugu
                | Script::Kannada
                | Script::Malayalam
                | Script::Sinhala
        )
    }
}

/// Returns the script bucket for a character.
///
/// This is a deliberately sparse classifier — only the codepoints
/// sigilbuzz knows how to shape differently are listed. The
/// fallthrough is [`Script::Other`], which the shaper treats with the
/// generic path.
#[must_use]
pub const fn script_of(ch: char) -> Script {
    let cp = ch as u32;
    match cp {
        // Basic Latin + Latin-1 Supplement + Latin Extended-A/B
        0x0000..=0x024F => Script::Latin,
        // Greek + Coptic + Greek Extended
        0x0370..=0x03FF | 0x1F00..=0x1FFF => Script::Greek,
        // Cyrillic + supplements
        0x0400..=0x052F => Script::Cyrillic,
        // Hebrew — main block plus the Hebrew presentation forms
        // (U+FB1D..U+FB4F). Alphabetic Presentation Forms splits
        // between Hebrew (U+FB1D..U+FB4F) and Armenian/Latin (below
        // U+FB1D), so classify the Hebrew sub-block explicitly.
        0x0590..=0x05FF | 0xFB1D..=0xFB4F => Script::Hebrew,
        // Arabic + supplements. Note the Arabic Presentation Forms-A
        // block (U+FB50..U+FDFF) starts immediately after the Hebrew
        // presentation forms above, so no overlap.
        0x0600..=0x06FF | 0x0750..=0x077F | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => Script::Arabic,
        // Devanagari
        0x0900..=0x097F => Script::Devanagari,
        // Bengali
        0x0980..=0x09FF => Script::Bengali,
        // Gurmukhi
        0x0A00..=0x0A7F => Script::Gurmukhi,
        // Gujarati
        0x0A80..=0x0AFF => Script::Gujarati,
        // Oriya (Odia)
        0x0B00..=0x0B7F => Script::Oriya,
        // Tamil
        0x0B80..=0x0BFF => Script::Tamil,
        // Telugu
        0x0C00..=0x0C7F => Script::Telugu,
        // Kannada
        0x0C80..=0x0CFF => Script::Kannada,
        // Malayalam
        0x0D00..=0x0D7F => Script::Malayalam,
        // Sinhala
        0x0D80..=0x0DFF => Script::Sinhala,
        // CJK unified ideographs + extensions A/B + Hiragana + Katakana
        0x3040..=0x309F | 0x30A0..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF => Script::Han,
        _ => Script::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_latin_ascii() {
        assert_eq!(script_of('A'), Script::Latin);
        assert_eq!(script_of('z'), Script::Latin);
        assert_eq!(script_of('0'), Script::Latin);
    }

    #[test]
    fn classifies_cjk_ideographs() {
        assert_eq!(script_of('字'), Script::Han);
        assert_eq!(script_of('あ'), Script::Han); // Hiragana
        assert_eq!(script_of('カ'), Script::Han); // Katakana
    }

    #[test]
    fn classifies_arabic() {
        assert_eq!(script_of('ا'), Script::Arabic);
        assert_eq!(script_of('ل'), Script::Arabic);
    }

    #[test]
    fn classifies_hebrew() {
        // Main block: alef, lamed, final-mem, sheva, cantillation
        // etnahta.
        assert_eq!(script_of('\u{05D0}'), Script::Hebrew);
        assert_eq!(script_of('\u{05DC}'), Script::Hebrew);
        assert_eq!(script_of('\u{05DD}'), Script::Hebrew);
        assert_eq!(script_of('\u{05B0}'), Script::Hebrew);
        assert_eq!(script_of('\u{0591}'), Script::Hebrew);
        // Presentation forms: alef with patah, shin with dot,
        // lam+alef equivalent position.
        assert_eq!(script_of('\u{FB2E}'), Script::Hebrew);
        assert_eq!(script_of('\u{FB2A}'), Script::Hebrew);
        // Boundary: U+FB50 is Arabic presentation forms, not Hebrew.
        assert_eq!(script_of('\u{FB50}'), Script::Arabic);
    }

    #[test]
    fn classifies_greek_and_cyrillic() {
        assert_eq!(script_of('Δ'), Script::Greek);
        assert_eq!(script_of('Д'), Script::Cyrillic);
    }

    #[test]
    fn classifies_devanagari() {
        // क (U+0915) and vowel sign I (U+093F).
        assert_eq!(script_of('\u{0915}'), Script::Devanagari);
        assert_eq!(script_of('\u{093F}'), Script::Devanagari);
    }

    #[test]
    fn classifies_indic_family() {
        assert_eq!(script_of('\u{09B0}'), Script::Bengali); // Bengali RA
        assert_eq!(script_of('\u{0A30}'), Script::Gurmukhi); // Gurmukhi RA
        assert_eq!(script_of('\u{0AB0}'), Script::Gujarati); // Gujarati RA
        assert_eq!(script_of('\u{0B30}'), Script::Oriya); // Oriya RA
        assert_eq!(script_of('\u{0BB0}'), Script::Tamil); // Tamil RA
        assert_eq!(script_of('\u{0C30}'), Script::Telugu); // Telugu RA
        assert_eq!(script_of('\u{0CB0}'), Script::Kannada); // Kannada RA
        assert_eq!(script_of('\u{0D30}'), Script::Malayalam); // Malayalam RA
        assert_eq!(script_of('\u{0DB1}'), Script::Sinhala); // Sinhala NA
    }

    #[test]
    fn is_indic_covers_full_family() {
        assert!(Script::Devanagari.is_indic());
        assert!(Script::Bengali.is_indic());
        assert!(Script::Gurmukhi.is_indic());
        assert!(Script::Gujarati.is_indic());
        assert!(Script::Oriya.is_indic());
        assert!(Script::Tamil.is_indic());
        assert!(Script::Telugu.is_indic());
        assert!(Script::Kannada.is_indic());
        assert!(Script::Malayalam.is_indic());
        assert!(Script::Sinhala.is_indic());
        assert!(!Script::Latin.is_indic());
        assert!(!Script::Arabic.is_indic());
        assert!(!Script::Other.is_indic());
    }

    #[test]
    fn unknown_scripts_fall_through_to_other() {
        // Thai — not in the bootstrap table.
        assert_eq!(script_of('ก'), Script::Other);
    }
}
