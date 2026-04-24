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
pub mod use_category;

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
    /// Khmer. Universal Shaping Engine (USE) applies.
    Khmer,
    /// Anything else — returned when sigilbuzz has no specialised
    /// table for the codepoint's script.
    Other,
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
        // Khmer + Khmer Symbols
        0x1780..=0x17FF | 0x19E0..=0x19FF => Script::Khmer,
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
    fn unknown_scripts_fall_through_to_other() {
        // Thai — not in the bootstrap table.
        assert_eq!(script_of('ก'), Script::Other);
    }

    #[test]
    fn classifies_khmer() {
        // ក U+1780 (consonant ka), ៊ U+17CA (register shifter),
        // ៛ U+17DB (currency riel), and a Khmer Symbols sign
        // U+19E0 sit in the Khmer bucket.
        assert_eq!(script_of('\u{1780}'), Script::Khmer);
        assert_eq!(script_of('\u{17CA}'), Script::Khmer);
        assert_eq!(script_of('\u{17DB}'), Script::Khmer);
        assert_eq!(script_of('\u{19E0}'), Script::Khmer);
    }
}
