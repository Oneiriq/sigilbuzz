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
    /// Khmer. Universal Shaping Engine (USE) applies.
    Khmer,
    /// Myanmar (Burmese, Shan, Mon). Universal Shaping Engine (USE)
    /// applies. Covers main block U+1000..U+109F plus Myanmar
    /// Extended-A U+AA60..U+AA7F and Extended-B U+A9E0..U+A9FF.
    Myanmar,
    /// Thai. Routed through the USE pipeline — no coeng-style
    /// subscripts but the same mark reorder + feature-chain shape.
    /// Covers U+0E00..U+0E7F.
    Thai,
    /// Lao. Structurally near-identical to Thai; routed through USE.
    /// Covers U+0E80..U+0EFF.
    Lao,
    /// Hangul. Modern precomposed syllables (U+AC00..U+D7A3) reach
    /// the shaper through cmap and the default pipeline; Jamo-
    /// decomposed text (U+1100..U+11FF, U+A960..U+A97F, U+D7B0..U+D7FF)
    /// routes through USE so `ljmo` / `vjmo` / `tjmo` see the L / V / T
    /// jamo in logical order.
    Hangul,
    /// Tibetan (U+0F00..U+0FFF). Stacked above/below-base subjoined
    /// consonants — runs through the feature-loop-only Tibetan shaper
    /// in [`crate::ot::tibetan`].
    Tibetan,
    /// Mongolian (U+1800..U+18AF). Cursive-joining like Arabic, with
    /// Free Variation Selectors (U+180B..U+180D, U+180F) overriding
    /// the joining-form choice. Runs through the Mongolian shaper in
    /// [`crate::ot::mongolian`].
    Mongolian,
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

    /// Returns `true` if the script routes through the Universal
    /// Shaping Engine pipeline. Khmer, Myanmar, Thai, Lao, and the
    /// Jamo subset of Hangul all share the USE state machine — each
    /// with its own category table and feature list, but the same
    /// segment / reorder / basic+topographical dispatch.
    #[must_use]
    pub const fn is_use(self) -> bool {
        matches!(
            self,
            Script::Khmer | Script::Myanmar | Script::Thai | Script::Lao | Script::Hangul
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
        // Thai
        0x0E00..=0x0E7F => Script::Thai,
        // Lao
        0x0E80..=0x0EFF => Script::Lao,
        // Myanmar (main + Extended-A + Extended-B)
        0x1000..=0x109F | 0xAA60..=0xAA7F | 0xA9E0..=0xA9FF => Script::Myanmar,
        // Hangul Jamo + Jamo Extended-A + Jamo Extended-B +
        // precomposed Hangul Syllables + Hangul Compatibility Jamo.
        // The USE routing in shape.rs only triggers for the Jamo
        // ranges; precomposed syllables flow through the default
        // pipeline — matches HarfBuzz / rustybuzz.
        0x1100..=0x11FF | 0xA960..=0xA97F | 0xAC00..=0xD7A3 | 0xD7B0..=0xD7FF | 0x3130..=0x318F => {
            Script::Hangul
        }
        // Tibetan — base block. Stacked subjoined consonants live
        // in U+0F90..U+0FBC; the whole block routes through the
        // Tibetan feature-loop shaper.
        0x0F00..=0x0FFF => Script::Tibetan,
        // Khmer + Khmer Symbols
        0x1780..=0x17FF | 0x19E0..=0x19FF => Script::Khmer,
        // Mongolian — main block. Mongolian Supplement (U+11660..)
        // is intentionally out of scope for the bootstrap classifier
        // since cargo's `char` is `u32` but the binding is `const fn`
        // and the supplement lives outside the Basic Multilingual
        // Plane; modern Noto Sans Mongolian's covered glyphs sit in
        // the main block, which is what 0.7.0's parity corpus tests.
        0x1800..=0x18AF => Script::Mongolian,
        // CJK unified ideographs + extensions A/B + Hiragana + Katakana
        0x3040..=0x309F | 0x30A0..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF => Script::Han,
        _ => Script::Other,
    }
}

/// Returns `true` if the codepoint is a Hangul Jamo (Leading / Vowel /
/// Trailing / Extended-A / Extended-B) — the subset of Hangul that USE
/// reorders via the `ljmo`/`vjmo`/`tjmo` features. Precomposed syllables
/// (U+AC00..U+D7A3) and Compatibility Jamo (U+3130..U+318F) stay on the
/// default path.
#[must_use]
pub const fn is_hangul_jamo(ch: char) -> bool {
    let cp = ch as u32;
    matches!(cp, 0x1100..=0x11FF | 0xA960..=0xA97F | 0xD7B0..=0xD7FF)
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
        // Armenian — not in the bootstrap table.
        assert_eq!(script_of('\u{0531}'), Script::Other);
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

    #[test]
    fn classifies_myanmar() {
        // ကာ — U+1000 (consonant ka) + U+102C (sign aa).
        assert_eq!(script_of('\u{1000}'), Script::Myanmar);
        assert_eq!(script_of('\u{102C}'), Script::Myanmar);
        // Myanmar Extended-A (e.g. Shan sign maun).
        assert_eq!(script_of('\u{AA60}'), Script::Myanmar);
        // Myanmar Extended-B.
        assert_eq!(script_of('\u{A9E0}'), Script::Myanmar);
    }

    #[test]
    fn classifies_thai() {
        // ก U+0E01 (consonant ko kai), ั U+0E31 (mai han-akat).
        assert_eq!(script_of('\u{0E01}'), Script::Thai);
        assert_eq!(script_of('\u{0E31}'), Script::Thai);
        // Block end — Thai digits.
        assert_eq!(script_of('\u{0E50}'), Script::Thai);
    }

    #[test]
    fn classifies_lao() {
        // ກ U+0E81 (consonant ko), ັ U+0EB1 (mai kan).
        assert_eq!(script_of('\u{0E81}'), Script::Lao);
        assert_eq!(script_of('\u{0EB1}'), Script::Lao);
    }

    #[test]
    fn classifies_hangul() {
        // Jamo: leading ᄀ (U+1100), vowel ᅡ (U+1161), trailing ᆨ
        // (U+11A8). Precomposed 가 (U+AC00) also in the Hangul
        // bucket — `is_hangul_jamo` separates the USE-routed subset.
        assert_eq!(script_of('\u{1100}'), Script::Hangul);
        assert_eq!(script_of('\u{1161}'), Script::Hangul);
        assert_eq!(script_of('\u{11A8}'), Script::Hangul);
        assert_eq!(script_of('\u{AC00}'), Script::Hangul);
        assert_eq!(script_of('\u{A960}'), Script::Hangul);
        assert_eq!(script_of('\u{D7B0}'), Script::Hangul);
    }

    #[test]
    fn hangul_jamo_predicate_only_matches_jamo_blocks() {
        assert!(is_hangul_jamo('\u{1100}'));
        assert!(is_hangul_jamo('\u{11A8}'));
        assert!(is_hangul_jamo('\u{A960}'));
        assert!(is_hangul_jamo('\u{D7B0}'));
        // Precomposed syllables are NOT jamo.
        assert!(!is_hangul_jamo('\u{AC00}'));
        // Compatibility jamo are NOT the USE-routed block.
        assert!(!is_hangul_jamo('\u{3131}'));
    }

    #[test]
    fn is_use_covers_all_use_scripts() {
        assert!(Script::Khmer.is_use());
        assert!(Script::Myanmar.is_use());
        assert!(Script::Thai.is_use());
        assert!(Script::Lao.is_use());
        assert!(Script::Hangul.is_use());
        assert!(!Script::Latin.is_use());
        assert!(!Script::Devanagari.is_use());
    }
}
