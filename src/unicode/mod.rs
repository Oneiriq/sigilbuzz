//! Unicode property data that the shaper needs.
//!
//! Scripts (Latin, Arabic, Hangul, ...) and general categories drive
//! which feature list the shaper applies and how cluster boundaries
//! are decided. Bootstrap impl is intentionally minimal; a full table
//! of UCD-derived data lands as the shaper's needs grow.

#![allow(missing_docs)]

pub mod bidi;
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
        // Hebrew
        0x0590..=0x05FF => Script::Hebrew,
        // Arabic + supplements
        0x0600..=0x06FF | 0x0750..=0x077F | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => Script::Arabic,
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
    fn classifies_greek_and_cyrillic() {
        assert_eq!(script_of('Δ'), Script::Greek);
        assert_eq!(script_of('Д'), Script::Cyrillic);
    }

    #[test]
    fn unknown_scripts_fall_through_to_other() {
        // Thai — not in the bootstrap table.
        assert_eq!(script_of('ก'), Script::Other);
    }
}
