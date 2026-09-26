//! Unicode NFC-style canonical pair composition.
//!
//! `HarfBuzz` implicitly runs NFC on its input buffer before cmap
//! lookup so that `e + U+0301` renders the same as `é`. sigilbuzz
//! matches that behavior when the caller opts in via
//! [`crate::Buffer::set_normalize_nfc`]. This module supplies the
//! composition half of NFC, which is what actually changes the
//! output: once you have a starter codepoint followed by
//! combining marks in canonical order, composition folds the
//! sequence into its precomposed form.
//!
//! The module **does not** implement canonical decomposition or
//! canonical combining-class reordering. Inputs that are already
//! either fully NFD (no precomposed forms) or fully NFC
//! (precomposed up front) round-trip correctly; pathological
//! inputs (precomposed glyph followed by yet another mark in the
//! wrong order) fall through unchanged.
//!
//! # Coverage
//!
//! The composition table covers the ranges sigilbuzz has been
//! able to curate by hand from the Unicode Character Database:
//!
//! - Latin-1 Supplement (U+00C0..U+00FF): complete
//! - Latin Extended-A (U+0100..U+017F): complete
//! - Common Greek monotonic precomposed forms
//! - Common Cyrillic precomposed forms
//!
//! Hangul syllable composition is handled algorithmically, so the
//! full 11,172-codepoint block works without any static data.
//!
//! Uncovered codepoints pass through unchanged. The shaper still
//! gets the raw sequence, and fonts that anchor combining marks
//! via GPOS will render fine. Only precomposed-oriented fonts
//! (Open Sans is the canonical example) lose fidelity here.
//!
//! # Algorithm
//!
//! Greedy left-to-right pair composition: keep a running "last
//! emitted" codepoint, and for every input codepoint try
//! `compose_pair(last, next)`. On success, replace last with the
//! composed codepoint and keep iterating. The new composite may
//! itself compose with the following mark (e.g. Vietnamese
//! stacked accents). On failure, commit last and move on.

use alloc::string::String;
use alloc::vec::Vec;

/// Tries to compose two codepoints into a single precomposed
/// codepoint. Returns `None` when there is no composition.
#[must_use]
pub fn compose_pair(a: char, b: char) -> Option<char> {
    if let Some(c) = compose_hangul(a, b) {
        return Some(c);
    }
    let key = (a as u32, b as u32);
    COMPOSITIONS
        .binary_search_by_key(&key, |e| (e.0, e.1))
        .ok()
        .and_then(|idx| char::from_u32(COMPOSITIONS[idx].2))
}

/// Returns an NFC-composed copy of `input`. When no compositions
/// apply, the output is byte-for-byte identical to the input.
#[must_use]
pub fn compose_str(input: &str) -> String {
    let mut out: Vec<char> = Vec::with_capacity(input.len());
    for ch in input.chars() {
        if let Some(last) = out.last_mut() {
            if let Some(composed) = compose_pair(*last, ch) {
                *last = composed;
                continue;
            }
        }
        out.push(ch);
    }
    out.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Hangul algorithmic composition. Spec reference:
// https://www.unicode.org/versions/Unicode15.0.0/ch03.pdf §3.12.
// ---------------------------------------------------------------------------

const HANGUL_SBASE: u32 = 0xAC00;
const HANGUL_LBASE: u32 = 0x1100;
const HANGUL_VBASE: u32 = 0x1161;
const HANGUL_TBASE: u32 = 0x11A7;
const HANGUL_LCOUNT: u32 = 19;
const HANGUL_VCOUNT: u32 = 21;
const HANGUL_TCOUNT: u32 = 28;
const HANGUL_NCOUNT: u32 = HANGUL_VCOUNT * HANGUL_TCOUNT;
const HANGUL_SCOUNT: u32 = HANGUL_LCOUNT * HANGUL_NCOUNT;

fn compose_hangul(a: char, b: char) -> Option<char> {
    let ac = a as u32;
    let bc = b as u32;

    let l_range = HANGUL_LBASE..HANGUL_LBASE + HANGUL_LCOUNT;
    let v_range = HANGUL_VBASE..HANGUL_VBASE + HANGUL_VCOUNT;
    let s_range = HANGUL_SBASE..HANGUL_SBASE + HANGUL_SCOUNT;
    let t_range = HANGUL_TBASE + 1..HANGUL_TBASE + HANGUL_TCOUNT;

    // Leading jamo + vowel jamo -> LV syllable.
    if l_range.contains(&ac) && v_range.contains(&bc) {
        let l_i = ac - HANGUL_LBASE;
        let v_i = bc - HANGUL_VBASE;
        let lv = HANGUL_SBASE + (l_i * HANGUL_VCOUNT + v_i) * HANGUL_TCOUNT;
        return char::from_u32(lv);
    }

    // LV syllable + trailing jamo -> LVT syllable. The existing
    // syllable must have T=0 (i.e. be aligned on a `TCOUNT`
    // boundary) for composition to apply; otherwise a trailing
    // jamo cannot meaningfully attach.
    if s_range.contains(&ac) {
        let s_i = ac - HANGUL_SBASE;
        if s_i % HANGUL_TCOUNT == 0 && t_range.contains(&bc) {
            let t_i = bc - HANGUL_TBASE;
            return char::from_u32(ac + t_i);
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Curated canonical-composition table.
//
// Each entry is (base, mark, composed). Sorted primarily by base
// then by mark so binary search over `(u32, u32)` keys works.
// Entries correspond to `UnicodeData.txt` canonical-decomposition
// pairs, minus the `DerivedComposition_Exclusions.txt` list (no
// entries in the covered ranges are on the exclusion list).
// ---------------------------------------------------------------------------

const COMPOSITIONS: &[(u32, u32, u32)] = &[
    // Latin-1 Supplement, uppercase.
    (0x0041, 0x0300, 0x00C0), // À
    (0x0041, 0x0301, 0x00C1), // Á
    (0x0041, 0x0302, 0x00C2), // Â
    (0x0041, 0x0303, 0x00C3), // Ã
    (0x0041, 0x0304, 0x0100), // Ā  (Latin Extended-A)
    (0x0041, 0x0306, 0x0102), // Ă
    (0x0041, 0x0308, 0x00C4), // Ä
    (0x0041, 0x030A, 0x00C5), // Å
    (0x0041, 0x0328, 0x0104), // Ą
    (0x0043, 0x0301, 0x0106), // Ć
    (0x0043, 0x0302, 0x0108), // Ĉ
    (0x0043, 0x0307, 0x010A), // Ċ
    (0x0043, 0x030C, 0x010C), // Č
    (0x0043, 0x0327, 0x00C7), // Ç
    (0x0044, 0x030C, 0x010E), // Ď
    (0x0045, 0x0300, 0x00C8), // È
    (0x0045, 0x0301, 0x00C9), // É
    (0x0045, 0x0302, 0x00CA), // Ê
    (0x0045, 0x0304, 0x0112), // Ē
    (0x0045, 0x0306, 0x0114), // Ĕ
    (0x0045, 0x0307, 0x0116), // Ė
    (0x0045, 0x0308, 0x00CB), // Ë
    (0x0045, 0x030C, 0x011A), // Ě
    (0x0045, 0x0328, 0x0118), // Ę
    (0x0047, 0x0302, 0x011C), // Ĝ
    (0x0047, 0x0306, 0x011E), // Ğ
    (0x0047, 0x0307, 0x0120), // Ġ
    (0x0047, 0x0327, 0x0122), // Ģ
    (0x0048, 0x0302, 0x0124), // Ĥ
    (0x0049, 0x0300, 0x00CC), // Ì
    (0x0049, 0x0301, 0x00CD), // Í
    (0x0049, 0x0302, 0x00CE), // Î
    (0x0049, 0x0303, 0x0128), // Ĩ
    (0x0049, 0x0304, 0x012A), // Ī
    (0x0049, 0x0306, 0x012C), // Ĭ
    (0x0049, 0x0307, 0x0130), // İ
    (0x0049, 0x0308, 0x00CF), // Ï
    (0x0049, 0x0328, 0x012E), // Į
    (0x004A, 0x0302, 0x0134), // Ĵ
    (0x004B, 0x0327, 0x0136), // Ķ
    (0x004C, 0x0301, 0x0139), // Ĺ
    (0x004C, 0x030C, 0x013D), // Ľ
    (0x004C, 0x0327, 0x013B), // Ļ
    (0x004E, 0x0301, 0x0143), // Ń
    (0x004E, 0x0303, 0x00D1), // Ñ
    (0x004E, 0x030C, 0x0147), // Ň
    (0x004E, 0x0327, 0x0145), // Ņ
    (0x004F, 0x0300, 0x00D2), // Ò
    (0x004F, 0x0301, 0x00D3), // Ó
    (0x004F, 0x0302, 0x00D4), // Ô
    (0x004F, 0x0303, 0x00D5), // Õ
    (0x004F, 0x0304, 0x014C), // Ō
    (0x004F, 0x0306, 0x014E), // Ŏ
    (0x004F, 0x0308, 0x00D6), // Ö
    (0x004F, 0x030B, 0x0150), // Ő
    (0x0052, 0x0301, 0x0154), // Ŕ
    (0x0052, 0x030C, 0x0158), // Ř
    (0x0052, 0x0327, 0x0156), // Ŗ
    (0x0053, 0x0301, 0x015A), // Ś
    (0x0053, 0x0302, 0x015C), // Ŝ
    (0x0053, 0x030C, 0x0160), // Š
    (0x0053, 0x0327, 0x015E), // Ş
    (0x0054, 0x030C, 0x0164), // Ť
    (0x0054, 0x0327, 0x0162), // Ţ
    (0x0055, 0x0300, 0x00D9), // Ù
    (0x0055, 0x0301, 0x00DA), // Ú
    (0x0055, 0x0302, 0x00DB), // Û
    (0x0055, 0x0303, 0x0168), // Ũ
    (0x0055, 0x0304, 0x016A), // Ū
    (0x0055, 0x0306, 0x016C), // Ŭ
    (0x0055, 0x0308, 0x00DC), // Ü
    (0x0055, 0x030A, 0x016E), // Ů
    (0x0055, 0x030B, 0x0170), // Ű
    (0x0055, 0x0328, 0x0172), // Ų
    (0x0057, 0x0302, 0x0174), // Ŵ
    (0x0059, 0x0301, 0x00DD), // Ý
    (0x0059, 0x0302, 0x0176), // Ŷ
    (0x0059, 0x0308, 0x0178), // Ÿ
    (0x005A, 0x0301, 0x0179), // Ź
    (0x005A, 0x0307, 0x017B), // Ż
    (0x005A, 0x030C, 0x017D), // Ž
    // Latin-1 Supplement, lowercase.
    (0x0061, 0x0300, 0x00E0), // à
    (0x0061, 0x0301, 0x00E1), // á
    (0x0061, 0x0302, 0x00E2), // â
    (0x0061, 0x0303, 0x00E3), // ã
    (0x0061, 0x0304, 0x0101), // ā
    (0x0061, 0x0306, 0x0103), // ă
    (0x0061, 0x0308, 0x00E4), // ä
    (0x0061, 0x030A, 0x00E5), // å
    (0x0061, 0x0328, 0x0105), // ą
    (0x0063, 0x0301, 0x0107), // ć
    (0x0063, 0x0302, 0x0109), // ĉ
    (0x0063, 0x0307, 0x010B), // ċ
    (0x0063, 0x030C, 0x010D), // č
    (0x0063, 0x0327, 0x00E7), // ç
    (0x0064, 0x030C, 0x010F), // ď
    (0x0065, 0x0300, 0x00E8), // è
    (0x0065, 0x0301, 0x00E9), // é
    (0x0065, 0x0302, 0x00EA), // ê
    (0x0065, 0x0304, 0x0113), // ē
    (0x0065, 0x0306, 0x0115), // ĕ
    (0x0065, 0x0307, 0x0117), // ė
    (0x0065, 0x0308, 0x00EB), // ë
    (0x0065, 0x030C, 0x011B), // ě
    (0x0065, 0x0328, 0x0119), // ę
    (0x0067, 0x0302, 0x011D), // ĝ
    (0x0067, 0x0306, 0x011F), // ğ
    (0x0067, 0x0307, 0x0121), // ġ
    (0x0067, 0x0327, 0x0123), // ģ
    (0x0068, 0x0302, 0x0125), // ĥ
    (0x0069, 0x0300, 0x00EC), // ì
    (0x0069, 0x0301, 0x00ED), // í
    (0x0069, 0x0302, 0x00EE), // î
    (0x0069, 0x0303, 0x0129), // ĩ
    (0x0069, 0x0304, 0x012B), // ī
    (0x0069, 0x0306, 0x012D), // ĭ
    (0x0069, 0x0308, 0x00EF), // ï
    (0x0069, 0x0328, 0x012F), // į
    (0x006A, 0x0302, 0x0135), // ĵ
    (0x006B, 0x0327, 0x0137), // ķ
    (0x006C, 0x0301, 0x013A), // ĺ
    (0x006C, 0x030C, 0x013E), // ľ
    (0x006C, 0x0327, 0x013C), // ļ
    (0x006E, 0x0301, 0x0144), // ń
    (0x006E, 0x0303, 0x00F1), // ñ
    (0x006E, 0x030C, 0x0148), // ň
    (0x006E, 0x0327, 0x0146), // ņ
    (0x006F, 0x0300, 0x00F2), // ò
    (0x006F, 0x0301, 0x00F3), // ó
    (0x006F, 0x0302, 0x00F4), // ô
    (0x006F, 0x0303, 0x00F5), // õ
    (0x006F, 0x0304, 0x014D), // ō
    (0x006F, 0x0306, 0x014F), // ŏ
    (0x006F, 0x0308, 0x00F6), // ö
    (0x006F, 0x030B, 0x0151), // ő
    (0x0072, 0x0301, 0x0155), // ŕ
    (0x0072, 0x030C, 0x0159), // ř
    (0x0072, 0x0327, 0x0157), // ŗ
    (0x0073, 0x0301, 0x015B), // ś
    (0x0073, 0x0302, 0x015D), // ŝ
    (0x0073, 0x030C, 0x0161), // š
    (0x0073, 0x0327, 0x015F), // ş
    (0x0074, 0x030C, 0x0165), // ť
    (0x0074, 0x0327, 0x0163), // ţ
    (0x0075, 0x0300, 0x00F9), // ù
    (0x0075, 0x0301, 0x00FA), // ú
    (0x0075, 0x0302, 0x00FB), // û
    (0x0075, 0x0303, 0x0169), // ũ
    (0x0075, 0x0304, 0x016B), // ū
    (0x0075, 0x0306, 0x016D), // ŭ
    (0x0075, 0x0308, 0x00FC), // ü
    (0x0075, 0x030A, 0x016F), // ů
    (0x0075, 0x030B, 0x0171), // ű
    (0x0075, 0x0328, 0x0173), // ų
    (0x0077, 0x0302, 0x0175), // ŵ
    (0x0079, 0x0301, 0x00FD), // ý
    (0x0079, 0x0302, 0x0177), // ŷ
    (0x0079, 0x0308, 0x00FF), // ÿ
    (0x007A, 0x0301, 0x017A), // ź
    (0x007A, 0x0307, 0x017C), // ż
    (0x007A, 0x030C, 0x017E), // ž
    // Greek monotonic precomposed accents.
    (0x0391, 0x0301, 0x0386), // Ά
    (0x0395, 0x0301, 0x0388), // Έ
    (0x0397, 0x0301, 0x0389), // Ή
    (0x0399, 0x0301, 0x038A), // Ί
    (0x0399, 0x0308, 0x03AA), // Ϊ
    (0x039F, 0x0301, 0x038C), // Ό
    (0x03A5, 0x0301, 0x038E), // Ύ
    (0x03A5, 0x0308, 0x03AB), // Ϋ
    (0x03A9, 0x0301, 0x038F), // Ώ
    (0x03B1, 0x0301, 0x03AC), // ά
    (0x03B5, 0x0301, 0x03AD), // έ
    (0x03B7, 0x0301, 0x03AE), // ή
    (0x03B9, 0x0301, 0x03AF), // ί
    (0x03B9, 0x0308, 0x03CA), // ϊ
    (0x03BF, 0x0301, 0x03CC), // ό
    (0x03C5, 0x0301, 0x03CD), // ύ
    (0x03C5, 0x0308, 0x03CB), // ϋ
    (0x03C9, 0x0301, 0x03CE), // ώ
    (0x03CA, 0x0301, 0x0390), // ΐ  = iota + dialytika + tonos
    (0x03CB, 0x0301, 0x03B0), // ΰ  = upsilon + dialytika + tonos
    // Cyrillic precomposed, common letters.
    (0x0406, 0x0308, 0x0407), // Ї
    (0x0415, 0x0308, 0x0401), // Ё
    (0x0418, 0x0300, 0x040D), // Ѝ
    (0x0418, 0x0306, 0x0419), // Й
    (0x0423, 0x0306, 0x040E), // Ў
    (0x0435, 0x0308, 0x0451), // ё
    (0x0438, 0x0300, 0x045D), // ѝ
    (0x0438, 0x0306, 0x0439), // й
    (0x0443, 0x0306, 0x045E), // ў
    (0x0456, 0x0308, 0x0457), // ї
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compositions_are_sorted_by_base_then_mark() {
        // Binary search depends on this invariant.
        for pair in COMPOSITIONS.windows(2) {
            let a = (pair[0].0, pair[0].1);
            let b = (pair[1].0, pair[1].1);
            assert!(a < b, "table not sorted: {a:?} >= {b:?}");
        }
    }

    #[test]
    fn latin_e_plus_acute_composes() {
        assert_eq!(compose_pair('e', '\u{0301}'), Some('é'));
    }

    #[test]
    fn latin_uppercase_composes() {
        assert_eq!(compose_pair('A', '\u{0308}'), Some('Ä'));
        assert_eq!(compose_pair('O', '\u{0303}'), Some('Õ'));
    }

    #[test]
    fn greek_and_cyrillic_compose() {
        assert_eq!(compose_pair('α', '\u{0301}'), Some('ά'));
        assert_eq!(compose_pair('е', '\u{0308}'), Some('ё'));
    }

    #[test]
    fn unknown_pairs_return_none() {
        // Not in the table and not Hangul.
        assert_eq!(compose_pair('x', '\u{0301}'), None);
        // Any mark + any mark never composes in this table.
        assert_eq!(compose_pair('\u{0301}', '\u{0308}'), None);
    }

    #[test]
    fn hangul_l_v_composes_to_syllable() {
        // 'ㄱ' (U+1100) + 'ㅏ' (U+1161) -> '가' (U+AC00).
        assert_eq!(compose_pair('\u{1100}', '\u{1161}'), Some('\u{AC00}'));
    }

    #[test]
    fn hangul_lv_plus_t_composes_to_lvt() {
        // '가' (U+AC00) + 'ㄱ' (U+11A8) -> '각' (U+AC01).
        assert_eq!(compose_pair('\u{AC00}', '\u{11A8}'), Some('\u{AC01}'));
    }

    #[test]
    fn compose_str_folds_decomposed_run() {
        let input = "cafe\u{0301}";
        assert_eq!(compose_str(input), "café");
    }

    #[test]
    fn compose_str_is_identity_when_nothing_composes() {
        assert_eq!(compose_str("plain ascii"), "plain ascii");
        // Already-composed runs stay the same too.
        assert_eq!(compose_str("café"), "café");
    }

    #[test]
    fn compose_str_handles_multiple_compositions_in_a_row() {
        // "naïve" with both diacritics in decomposed form.
        let input = "nai\u{0308}ve\u{0301}";
        assert_eq!(compose_str(input), "naïvé");
    }

    #[test]
    fn double_accent_folds_through_extended_a() {
        // 'A' + macron (0304) composes to Ā (U+0100).
        assert_eq!(compose_pair('A', '\u{0304}'), Some('\u{0100}'));
    }
}
