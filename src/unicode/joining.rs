//! Arabic joining-type classification.
//!
//! Every codepoint that participates in Arabic cursive joining carries
//! a *joining type* from `ArabicShaping.txt` (a companion UCD file to
//! `UnicodeData.txt`). The type drives the [`crate::ot::arabic`]
//! state machine that assigns `init` / `medi` / `fina` / `isol`
//! feature tags to each position in an Arabic run.
//!
//! # The six joining types
//!
//! - **U**: Non-joining. Does not connect to its neighbors on
//!   either side (hamza U+0621, brackets, most punctuation).
//! - **R**: Right-joining. Connects to the *previous* (right-side in
//!   logical order for RTL) letter only (alef U+0627, dal U+062F,
//!   reh U+0631, zain U+0632, waw U+0648, alef maksura variants).
//! - **D**: Dual-joining. Connects on both sides (beh U+0628, teh
//!   U+062A, seen U+0633, the bulk of Arabic letters).
//! - **C**: Join-causing. Forces joining behavior through itself
//!   without having a visual form that changes (tatweel U+0640,
//!   ZWJ U+200D).
//! - **T**: Transparent. Skipped by the joining state machine but
//!   kept in the glyph run (combining marks, harakat, Arabic digits'
//!   diacritical additions).
//! - **L**: Left-joining. Exists in the spec for completeness; no
//!   codepoint currently assigned. Included so future tables can grow.
//!
//! # Coverage
//!
//! Curated from `ArabicShaping.txt` (Unicode 15.1, 2023-09-11):
//!
//! - `U+0600..U+06FF` (Arabic): every assigned letter + mark
//! - `U+0750..U+077F` (Arabic Supplement): all letters
//! - `U+0870..U+089F` (Arabic Extended-B, partial)
//! - `U+08A0..U+08FF` (Arabic Extended-A): letters and marks
//! - `U+1800..U+18AA` (Mongolian): letters + Free Variation Selectors
//! - `U+200C..U+200D`: ZWNJ (non-joiner) / ZWJ (join-causing)
//!
//! Codepoints outside the Arabic family return [`JoiningType::U`]
//! (non-joining), which is the safe default: it does not change the
//! shape of any neighboring Arabic letter. The state machine treats
//! a non-Arabic letter exactly like a run boundary.
//!
//! # Table style
//!
//! The table is a hand-transcribed sorted array of
//! `(start, end, type)` tuples, searched with a single binary pass.
//! This matches the style of [`crate::unicode::normalize`]: no
//! generated code, small, easy to audit against the spec by eye.

/// The six Arabic joining types from `ArabicShaping.txt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoiningType {
    /// Non-joining (`U`). Does not connect on either side.
    U,
    /// Right-joining (`R`). Connects to the preceding letter only.
    R,
    /// Dual-joining (`D`). Connects on both sides.
    D,
    /// Join-causing (`C`). Propagates joining without visual change.
    C,
    /// Transparent (`T`). Skipped by the joining state machine.
    T,
    /// Left-joining (`L`). No assigned codepoints in Unicode 15.1 but
    /// reserved by the spec; included so the enum is exhaustive.
    L,
}

/// Returns the joining type for `ch`. Non-Arabic codepoints map to
/// [`JoiningType::U`], which the state machine treats as a run
/// boundary. The neighboring Arabic letter therefore gets its
/// isolated or final form, matching the OpenType spec.
#[must_use]
pub fn joining_type(ch: char) -> JoiningType {
    let cp = ch as u32;
    // Fast reject for the dominant case: everything outside the
    // Arabic family (plus the ZWJ/ZWNJ pair) is non-joining.
    if !is_arabic_range(cp) {
        return JoiningType::U;
    }
    // Binary search over the (start, end, type) array.
    let mut lo = 0usize;
    let mut hi = JOINING_TABLE.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let (start, end, jt) = JOINING_TABLE[mid];
        if cp < start {
            hi = mid;
        } else if cp > end {
            lo = mid + 1;
        } else {
            return jt;
        }
    }
    JoiningType::U
}

/// True when `cp` sits in any block that can contribute a joining
/// type. Fast-path filter so Latin / CJK text skips the binary search
/// entirely.
const fn is_arabic_range(cp: u32) -> bool {
    matches!(
        cp,
        // Arabic, Arabic Supplement, Arabic Extended-B / A,
        // Arabic Presentation Forms (handled separately below).
        0x0600..=0x06FF
        | 0x0750..=0x077F
        // N'Ko: same Arabic-style joining state machine.
        | 0x07C0..=0x07FF
        | 0x0870..=0x089F
        | 0x08A0..=0x08FF
        | 0xFB50..=0xFDFF
        | 0xFE70..=0xFEFF
        // Mongolian: letters + Free Variation Selectors. Mongolian
        // shapes through the same joining-state machine as Arabic
        // (same `init`/`medi`/`fina`/`isol` GSUB feature tags), so
        // the joining table is the natural place for it.
        | 0x1800..=0x18AF
        // ZWNJ / ZWJ: bidi format characters that participate in
        // joining even though they are not Arabic letters.
        | 0x200C..=0x200D
    )
}

/// `(start, end_inclusive, type)` runs from `ArabicShaping.txt`,
/// sorted by `start`. The handful of assignments that are neither
/// T nor D live as single-codepoint ranges; the Transparent blocks
/// (harakat, honorifics) collapse into runs of contiguous marks.
///
/// Entries derived from Unicode 15.1 `ArabicShaping.txt`. Any code
/// point not covered here defaults to `U` via the lookup
/// fall-through, matching the spec's "No_Joining_Group / U" default
/// for unlisted characters.
#[rustfmt::skip]
const JOINING_TABLE: &[(u32, u32, JoiningType)] = &[
    // --- U+0600..U+063F block ---
    // 0x0600..0x0605: Arabic number signs, all Transparent prefix marks.
    (0x0600, 0x0605, JoiningType::U),
    // Arabic combining marks / vowel signs (Transparent).
    (0x0610, 0x061A, JoiningType::T),
    // 0x061C Arabic letter mark: Transparent.
    (0x061C, 0x061C, JoiningType::T),
    // Hamza, isolated letters (Non-joining).
    (0x0621, 0x0621, JoiningType::U),
    // Alef family (Right-joining).
    (0x0622, 0x0625, JoiningType::R),
    // Beh (Dual).
    (0x0626, 0x0626, JoiningType::D),
    // Alef (Right).
    (0x0627, 0x0627, JoiningType::R),
    // Beh, Teh Marbuta, Teh, Theh: D, R, D, D.
    (0x0628, 0x0628, JoiningType::D),
    (0x0629, 0x0629, JoiningType::R),
    (0x062A, 0x062B, JoiningType::D),
    (0x062C, 0x062E, JoiningType::D),
    // Dal, Thal (Right).
    (0x062F, 0x0630, JoiningType::R),
    // Reh, Zain (Right).
    (0x0631, 0x0632, JoiningType::R),
    // Seen family (Dual).
    (0x0633, 0x0639, JoiningType::D),
    (0x063A, 0x063A, JoiningType::D),
    (0x063B, 0x063F, JoiningType::D),
    // Tatweel (Join-causing).
    (0x0640, 0x0640, JoiningType::C),
    // Feh, Qaf, Kaf, Lam, Meem, Noon (Dual).
    (0x0641, 0x0647, JoiningType::D),
    // Waw (Right).
    (0x0648, 0x0648, JoiningType::R),
    // Alef Maksura (Right) + Yeh (Dual).
    (0x0649, 0x0649, JoiningType::R),
    (0x064A, 0x064A, JoiningType::D),
    // Fathatan..shadda..sukun (Transparent vowel marks).
    (0x064B, 0x065F, JoiningType::T),
    // 0x0660..0x0669: Arabic-Indic digits (Non-joining).
    (0x0660, 0x0669, JoiningType::U),
    // 0x066A..0x066D: percent / decimal separator / five-pointed star (U).
    (0x066A, 0x066D, JoiningType::U),
    // Dotless beh / qaf / feh (Right / Dual / Dual).
    (0x066E, 0x066E, JoiningType::D),
    (0x066F, 0x066F, JoiningType::D),
    // Superscript alef (Transparent).
    (0x0670, 0x0670, JoiningType::T),
    // Wavy hamza above / below (Right) + peh / theh variants (Dual).
    (0x0671, 0x0673, JoiningType::R),
    // High hamza (Non-joining).
    (0x0674, 0x0674, JoiningType::U),
    // High hamza alef / waw / yeh (Right).
    (0x0675, 0x0677, JoiningType::R),
    // Tteheh, Tteh, Beeh, Beheh, ...: the bulk Dual-joining block.
    (0x0678, 0x0687, JoiningType::D),
    // Ddal, Dahal family (Right).
    (0x0688, 0x0699, JoiningType::R),
    // Dual-joining: Ghain variants, Tcheh family, Keheh, Kaf variants.
    (0x069A, 0x06A9, JoiningType::D),
    // Kaf variants continue (Dual).
    (0x06AA, 0x06BF, JoiningType::D),
    // Waw with hamza above (Right).
    (0x06C0, 0x06C0, JoiningType::R),
    // Heh goal (Dual).
    (0x06C1, 0x06C2, JoiningType::D),
    // Teh marbuta goal, Kirghiz oe, Kirghiz yu, Oe: Right.
    (0x06C3, 0x06CB, JoiningType::R),
    // Farsi yeh (Dual).
    (0x06CC, 0x06CC, JoiningType::D),
    // Waw with ring, Yeh with tail: Right.
    (0x06CD, 0x06CD, JoiningType::R),
    // E, Yu, Yeh barree variants: Dual / Right mix.
    (0x06CE, 0x06CE, JoiningType::D),
    (0x06CF, 0x06CF, JoiningType::R),
    (0x06D0, 0x06D1, JoiningType::D),
    (0x06D2, 0x06D3, JoiningType::R),
    // Dot above, small medial yeh: Non-joining / Transparent.
    (0x06D4, 0x06D4, JoiningType::U),
    (0x06D5, 0x06D5, JoiningType::R),
    (0x06D6, 0x06DC, JoiningType::T),
    (0x06DD, 0x06DD, JoiningType::U),
    (0x06DE, 0x06DE, JoiningType::T),
    (0x06DF, 0x06E4, JoiningType::T),
    // Small waw / yeh (Non-joining).
    (0x06E5, 0x06E6, JoiningType::D),
    (0x06E7, 0x06E8, JoiningType::T),
    // Place of sajdah (Non-joining).
    (0x06E9, 0x06E9, JoiningType::U),
    // Empty centre low / high stops (Transparent).
    (0x06EA, 0x06ED, JoiningType::T),
    // Dal / reh with small v: Right.
    (0x06EE, 0x06EF, JoiningType::R),
    // Extended Arabic-Indic digits (Non-joining).
    (0x06F0, 0x06F9, JoiningType::U),
    // Dyeh, ae, e variants (Dual / Right).
    (0x06FA, 0x06FC, JoiningType::D),
    // Sindhi signs (Non-joining).
    (0x06FD, 0x06FE, JoiningType::U),
    // Heh with inverted v (Dual).
    (0x06FF, 0x06FF, JoiningType::D),

    // --- U+0750..U+077F Arabic Supplement ---
    // Beh / peh / tteh family variants for African languages: all Dual
    // in the supplement, with a single Right-joining reh entry.
    (0x0750, 0x0755, JoiningType::D),
    (0x0756, 0x0756, JoiningType::D),
    (0x0757, 0x0758, JoiningType::D),
    (0x0759, 0x075B, JoiningType::R),
    (0x075C, 0x076A, JoiningType::D),
    (0x076B, 0x076C, JoiningType::R),
    (0x076D, 0x0770, JoiningType::D),
    (0x0771, 0x0771, JoiningType::R),
    (0x0772, 0x0772, JoiningType::D),
    (0x0773, 0x0774, JoiningType::R),
    (0x0775, 0x0777, JoiningType::D),
    (0x0778, 0x0779, JoiningType::R),
    (0x077A, 0x077F, JoiningType::D),

    // --- U+07C0..U+07FF N'Ko ---
    // N'Ko is RTL alphabetic with cursive joining of the same shape
    // as Arabic: every letter has an init/medi/fina/isol form
    // selected by the same state machine. The categorization mirrors
    // rustybuzz's `gen-arabic-table.py` output: digits + tone marks
    // are X (fallback to U / T by general-category: non-spacing
    // marks become T, everything else U), letters 07CA..07EA are
    // Dual, and 07FA Lajanyalan (low-tone mark stretcher) is Dual.
    // 07C0..07C9: N'Ko digits (Non-joining).
    (0x07C0, 0x07C9, JoiningType::U),
    // 07CA..07EA: N'Ko letters (Dual-joining).
    (0x07CA, 0x07EA, JoiningType::D),
    // 07EB..07F3: N'Ko combining tone marks (Transparent: the
    // joining state machine threads them through without breaking
    // the cursive chain).
    (0x07EB, 0x07F3, JoiningType::T),
    // 07F4..07F5: N'Ko high/low tone apostrophes (Non-joining).
    (0x07F4, 0x07F5, JoiningType::U),
    // 07F6..07F9: N'Ko symbols + punctuation (Non-joining).
    (0x07F6, 0x07F9, JoiningType::U),
    // 07FA: N'Ko Lajanyalan (Dual-joining).
    (0x07FA, 0x07FA, JoiningType::D),
    // 07FD: N'Ko Dantayalan (Transparent, combining low-tone mark).
    (0x07FD, 0x07FD, JoiningType::T),

    // --- U+0870..U+088E Arabic Extended-B (Quranic) ---
    // Largely Non-joining letters + one Right-joining alef variant.
    (0x0870, 0x0882, JoiningType::U),
    (0x0883, 0x0885, JoiningType::U),
    (0x0886, 0x0886, JoiningType::D),
    (0x0887, 0x0887, JoiningType::U),
    (0x0888, 0x0888, JoiningType::U),
    (0x0889, 0x088E, JoiningType::D),

    // --- U+0890..U+0891 Arabic Extended-B signs ---
    (0x0890, 0x0891, JoiningType::U),

    // --- U+0898..U+089F combining marks (Transparent) ---
    (0x0898, 0x089F, JoiningType::T),

    // --- U+08A0..U+08FF Arabic Extended-A ---
    // Extended letters for Central Asian / African orthographies.
    (0x08A0, 0x08A9, JoiningType::D),
    (0x08AA, 0x08AC, JoiningType::R),
    (0x08AD, 0x08AD, JoiningType::U),
    (0x08AE, 0x08AE, JoiningType::R),
    (0x08AF, 0x08B0, JoiningType::D),
    (0x08B1, 0x08B2, JoiningType::R),
    (0x08B3, 0x08B4, JoiningType::D),
    (0x08B5, 0x08B5, JoiningType::D),
    (0x08B6, 0x08B8, JoiningType::D),
    (0x08B9, 0x08B9, JoiningType::R),
    (0x08BA, 0x08C7, JoiningType::D),
    (0x08C8, 0x08C8, JoiningType::D),
    (0x08C9, 0x08C9, JoiningType::D),
    (0x08CA, 0x08E1, JoiningType::T),
    (0x08E2, 0x08E2, JoiningType::U),
    (0x08E3, 0x08FF, JoiningType::T),

    // --- U+1800..U+18AA Mongolian ---
    // The Mongolian script ships with letters whose default joining
    // type matches the values rustybuzz's generated table assigns
    // (`gen-arabic-table.py` from the HarfBuzz tree). Mongolian's
    // joining flow is the same as Arabic: dual-joining letters take
    // `init`/`medi`/`fina` based on neighbors, and Free Variation
    // Selectors are transparent so the post-FVS form selection in
    // [`crate::ot::mongolian`] can promote the chosen variant onto
    // the previous letter without disturbing the chain.
    //
    // 0x1806 MONGOLIAN TODO SOFT HYPHEN (Todo is the script name, not a
    // work note): Non-joining (used as line-break hint).
    (0x1806, 0x1806, JoiningType::U),
    // 0x1807 SIBE SYLLABLE BOUNDARY MARKER: Dual.
    (0x1807, 0x1807, JoiningType::D),
    // 0x180A NIRUGU: Dual (a connecting baseline).
    (0x180A, 0x180A, JoiningType::D),
    // 0x180B..0x180D Free Variation Selectors 1/2/3: Transparent.
    // FVS4 (U+180F, Unicode 14.0) joins them. The Mongolian shaper
    // promotes the FVS-selected variant onto the preceding letter
    // after joining-form assignment; the FVS itself does not join.
    (0x180B, 0x180D, JoiningType::T),
    // 0x180E MONGOLIAN VOWEL SEPARATOR: Non-joining. Breaks the
    // cursive chain so the preceding letter takes its final form.
    (0x180E, 0x180E, JoiningType::U),
    // 0x180F FVS4: Transparent (Unicode 14.0).
    (0x180F, 0x180F, JoiningType::T),
    // 0x1820..0x1877 Mongolian letters: bulk Dual-joining block.
    // Covers Mongolian, Galik, Manchu, and Sibe letters; every
    // assigned letter in this range is dual-joining.
    (0x1820, 0x1877, JoiningType::D),
    // 0x1880..0x1884 Mongolian letters: ali gali anusvara, visarga,
    // damaru, ubadama, three baluda. Non-joining symbols.
    (0x1880, 0x1884, JoiningType::U),
    // 0x1885..0x1886 Mongolian letter ali gali baluda + ali gali three
    // baluda: Transparent combining marks.
    (0x1885, 0x1886, JoiningType::T),
    // 0x1887..0x18A8 Mongolian Galik / Manchu / Sibe letters: Dual.
    (0x1887, 0x18A8, JoiningType::D),
    // 0x18A9 MONGOLIAN LETTER ALI GALI DAGALGA: Non-joining; rustybuzz
    // marks this position as `X` (no joining data) which we treat as U.
    (0x18A9, 0x18A9, JoiningType::U),
    // 0x18AA MONGOLIAN LETTER MANCHU ALI GALI LHA: Dual.
    (0x18AA, 0x18AA, JoiningType::D),

    // --- Format characters that participate in joining ---
    (0x200C, 0x200C, JoiningType::U), // ZWNJ: breaks joining
    (0x200D, 0x200D, JoiningType::C), // ZWJ: forces joining
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_non_joining() {
        assert_eq!(joining_type('A'), JoiningType::U);
        assert_eq!(joining_type(' '), JoiningType::U);
        assert_eq!(joining_type('0'), JoiningType::U);
    }

    #[test]
    fn alef_is_right_joining() {
        // U+0627 ARABIC LETTER ALEF: connects only on the right.
        assert_eq!(joining_type('\u{0627}'), JoiningType::R);
        // U+0622..0625: alef variants.
        assert_eq!(joining_type('\u{0622}'), JoiningType::R);
        assert_eq!(joining_type('\u{0625}'), JoiningType::R);
    }

    #[test]
    fn beh_is_dual_joining() {
        // U+0628 ARABIC LETTER BEH: connects both sides.
        assert_eq!(joining_type('\u{0628}'), JoiningType::D);
        // U+062A TEH, U+062B THEH: also D.
        assert_eq!(joining_type('\u{062A}'), JoiningType::D);
        assert_eq!(joining_type('\u{062B}'), JoiningType::D);
    }

    #[test]
    fn reh_and_waw_are_right_joining() {
        assert_eq!(joining_type('\u{0631}'), JoiningType::R); // reh
        assert_eq!(joining_type('\u{0632}'), JoiningType::R); // zain
        assert_eq!(joining_type('\u{0648}'), JoiningType::R); // waw
    }

    #[test]
    fn tatweel_is_join_causing() {
        // U+0640 ARABIC TATWEEL: the stretching baseline glyph.
        assert_eq!(joining_type('\u{0640}'), JoiningType::C);
    }

    #[test]
    fn harakat_are_transparent() {
        // U+064E FATHA, U+064F DAMMA, U+0650 KASRA: vowel marks
        // skipped by the joining state machine.
        assert_eq!(joining_type('\u{064E}'), JoiningType::T);
        assert_eq!(joining_type('\u{064F}'), JoiningType::T);
        assert_eq!(joining_type('\u{0650}'), JoiningType::T);
        // U+0651 SHADDA, U+0652 SUKUN.
        assert_eq!(joining_type('\u{0651}'), JoiningType::T);
        assert_eq!(joining_type('\u{0652}'), JoiningType::T);
    }

    #[test]
    fn zwj_is_join_causing_and_zwnj_is_non_joining() {
        assert_eq!(joining_type('\u{200D}'), JoiningType::C); // ZWJ
        assert_eq!(joining_type('\u{200C}'), JoiningType::U); // ZWNJ
    }

    #[test]
    fn hamza_is_non_joining() {
        // U+0621 ARABIC LETTER HAMZA: visually isolated.
        assert_eq!(joining_type('\u{0621}'), JoiningType::U);
    }

    #[test]
    fn supplement_letters_classify() {
        // U+0750 ARABIC LETTER BEH WITH THREE DOTS HORIZONTALLY
        // BELOW: dual-joining.
        assert_eq!(joining_type('\u{0750}'), JoiningType::D);
    }

    #[test]
    fn extended_a_letters_classify() {
        // U+08A0 ARABIC LETTER BEH WITH SMALL V BELOW: Dual.
        assert_eq!(joining_type('\u{08A0}'), JoiningType::D);
    }

    #[test]
    fn cjk_and_greek_fall_through_to_non_joining() {
        assert_eq!(joining_type('字'), JoiningType::U);
        assert_eq!(joining_type('Δ'), JoiningType::U);
        // Above BMP: astral code point.
        assert_eq!(joining_type('\u{1F600}'), JoiningType::U);
    }

    #[test]
    fn table_entries_sorted_and_non_overlapping() {
        // Invariant check: binary search depends on monotonic starts.
        for pair in JOINING_TABLE.windows(2) {
            let (_, end_prev, _) = pair[0];
            let (start_next, _, _) = pair[1];
            assert!(
                end_prev < start_next,
                "overlap or out-of-order entry: {end_prev:#x} then {start_next:#x}"
            );
        }
    }
}
