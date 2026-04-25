//! Per-codepoint Bidi_Class table (UAX #9 §3.1).
//!
//! Sparse, curated coverage targeting the scripts and category families
//! sigilbuzz consumers actually shape:
//!
//! - **L** — Latin / Greek / Cyrillic / Armenian / CJK / Hangul /
//!   Devanagari and the rest of the Brahmic family.
//! - **R** — Hebrew letters + presentation forms, Thaana, N'Ko.
//! - **AL** — Arabic letters + presentation forms, Syriac.
//! - **EN** — ASCII digits.
//! - **AN** — Arabic-Indic digits + Extended Arabic-Indic digits.
//! - **ES** — `+`, `-`, `−` (minus).
//! - **ET** — `#`, `$`, `%`, `&`, `*`, `°`, `+/-` etc. plus per-mille,
//!   currency, and the degree sign — "European Terminator".
//! - **CS** — `,`, `.`, `:`, `/`, `\u{00A0}` (NBSP), Arabic comma /
//!   semicolon — "Common Number Separator".
//! - **NSM** — Combining diacritics, Hebrew points, Arabic harakat.
//! - **BN** — `BOUNDARY_NEUTRAL`: format controls + ZWNBSP + soft hyphen.
//! - **B** — Paragraph separators: LF, CR, U+0085, U+2029.
//! - **S** — Segment separators: TAB, U+001F.
//! - **WS** — Whitespace: U+0020, U+00A0 (no — that's CS), U+200x set,
//!   U+205F, U+3000.
//! - **ON** — Anything not classified above that the algorithm should
//!   treat as a neutral (most punctuation and symbols).
//! - **LRE / RLE / LRO / RLO / PDF** — Explicit-embedding and override
//!   format characters (U+202A..U+202E).
//! - **LRI / RLI / FSI / PDI** — Explicit-isolate format characters
//!   (U+2066..U+2069).
//!
//! Anything not matched falls through to **ON** — the safe neutral
//! default. Real-world consumers hit this table on Latin / Hebrew /
//! Arabic mixed runs; the long tail (e.g. Inscriptional Pahlavi,
//! Mandaic) remains classifiable via a future expansion without
//! changing the dispatch path.

use crate::buffer::Direction;

/// UAX #9 Bidi_Class — full set of categories the algorithm
/// distinguishes. The variants exactly match the names used in the
/// UAX #9 rule pseudocode (`L`, `R`, `AL`, `EN`, …) so cross-checking
/// against the spec stays mechanical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum BidiClass {
    // Strong types.
    L,
    R,
    Al,
    // Weak types.
    En,
    Es,
    Et,
    An,
    Cs,
    Nsm,
    Bn,
    // Neutral types.
    B,
    S,
    Ws,
    On,
    // Explicit formatting.
    Lre,
    Lro,
    Rle,
    Rlo,
    Pdf,
    Lri,
    Rli,
    Fsi,
    Pdi,
}

impl BidiClass {
    /// True for the three strong directional categories (L / R / AL).
    #[must_use]
    pub const fn is_strong(self) -> bool {
        matches!(self, Self::L | Self::R | Self::Al)
    }

    /// True for the categories that participate as RTL strong types
    /// in the implicit-level pass (R, AL).
    #[must_use]
    pub const fn is_rtl_strong(self) -> bool {
        matches!(self, Self::R | Self::Al)
    }

    /// True for the explicit-formatting characters consumed by X1-X8.
    #[must_use]
    pub const fn is_explicit(self) -> bool {
        matches!(
            self,
            Self::Lre
                | Self::Lro
                | Self::Rle
                | Self::Rlo
                | Self::Pdf
                | Self::Lri
                | Self::Rli
                | Self::Fsi
                | Self::Pdi
        )
    }

    /// True for the isolate-initiator subset (LRI / RLI / FSI).
    #[must_use]
    pub const fn is_isolate_initiator(self) -> bool {
        matches!(self, Self::Lri | Self::Rli | Self::Fsi)
    }

    /// True for codepoints that L1 resets back to the paragraph level.
    #[must_use]
    pub const fn is_l1_reset(self) -> bool {
        matches!(
            self,
            Self::B | Self::S | Self::Ws | Self::Fsi | Self::Lri | Self::Rli | Self::Pdi
        )
    }
}

/// Maps a paragraph-direction tag to its strong-class equivalent for
/// the higher-level API.
#[must_use]
pub const fn strong_for_direction(dir: Direction) -> BidiClass {
    match dir {
        Direction::Rtl => BidiClass::R,
        _ => BidiClass::L,
    }
}

/// Returns the UAX #9 Bidi_Class for `ch`. Sparse / curated — see the
/// module docs for the coverage envelope. Codepoints outside the
/// curated set fall through to [`BidiClass::On`] (Other Neutral),
/// which is the safe default for the algorithm.
#[must_use]
#[allow(clippy::match_same_arms, clippy::too_many_lines)]
pub const fn bidi_class(ch: char) -> BidiClass {
    let cp = ch as u32;
    match cp {
        // --- Explicit format characters (handled before W/N rules) --
        0x202A => BidiClass::Lre,
        0x202B => BidiClass::Rle,
        0x202C => BidiClass::Pdf,
        0x202D => BidiClass::Lro,
        0x202E => BidiClass::Rlo,
        0x2066 => BidiClass::Lri,
        0x2067 => BidiClass::Rli,
        0x2068 => BidiClass::Fsi,
        0x2069 => BidiClass::Pdi,

        // --- Paragraph separators (B) ----------------------------
        0x000A | 0x000D | 0x001C..=0x001E | 0x0085 | 0x2029 => BidiClass::B,

        // --- Segment separators (S) -----------------------------
        0x0009 | 0x000B | 0x001F => BidiClass::S,

        // --- Whitespace (WS) ------------------------------------
        0x000C | 0x0020 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x205F | 0x3000 => BidiClass::Ws,

        // --- Boundary Neutrals (BN) ------------------------------
        // C0 controls (excluding tab/lf/etc above), zero-width
        // joiner / non-joiner, LRM / RLM / ALM, soft hyphen,
        // ZWNBSP, BOM, format characters in the Cf category that
        // the algorithm ignores, etc.
        0x0000..=0x0008
        | 0x000E..=0x001B
        | 0x007F..=0x0084
        | 0x0086..=0x009F
        | 0x00AD
        | 0x061C
        | 0x200B..=0x200D
        | 0x200E
        | 0x200F
        | 0x2060..=0x2064
        | 0xFEFF => match cp {
            0x200E => BidiClass::L,          // LRM is strong-L
            0x200F | 0x061C => BidiClass::R, // RLM and ALM are strong-R
            _ => BidiClass::Bn,
        },

        // --- ASCII digits → EN ----------------------------------
        0x0030..=0x0039 => BidiClass::En,
        // Superscript / subscript digits → EN.
        0x00B2 | 0x00B3 | 0x00B9 | 0x2070 | 0x2074..=0x2079 | 0x2080..=0x2089 => BidiClass::En,

        // --- Arabic-Indic digits → AN ---------------------------
        0x0660..=0x0669 | 0x066B | 0x066C => BidiClass::An,
        // Extended Arabic-Indic digits.
        0x06F0..=0x06F9 => BidiClass::An,

        // --- ES (European Separator) ----------------------------
        // PLUS, MINUS, ASCII SOLIDUS variants.
        0x002B | 0x002D | 0x207A | 0x207B | 0x208A | 0x208B | 0x2212 => BidiClass::Es,

        // --- CS (Common Separator) ------------------------------
        // Comma, full stop, colon, slash, NBSP, Arabic comma /
        // semicolon / decimal-separator, etc.
        0x002C | 0x002E | 0x003A | 0x002F | 0x00A0 | 0x060C | 0x202F | 0xFE50 | 0xFE52 | 0xFE55
        | 0xFF0C | 0xFF0E | 0xFF1A => BidiClass::Cs,

        // --- ET (European Terminator) ---------------------------
        // `#`, `$`, `%`, `*`, `°`, `‰`, `‱`, currency symbols,
        // Latin-1 currency / per-mille, percent / per-mille, plus
        // common sub-/super-script signs.
        0x0023
        | 0x0024
        | 0x0025
        | 0x002A
        | 0x00A2..=0x00A5
        | 0x00B0
        | 0x00B1
        | 0x066A
        | 0x09F2
        | 0x09F3
        | 0x0AF1
        | 0x0BF9
        | 0x0E3F
        | 0x17DB
        | 0x2030..=0x2034
        | 0x20A0..=0x20CF => BidiClass::Et,

        // --- ASCII letters (L) ----------------------------------
        0x0041..=0x005A | 0x0061..=0x007A => BidiClass::L,

        // --- Latin-1 / Latin Extended (L) -----------------------
        0x00AA
        | 0x00B5
        | 0x00BA
        | 0x00C0..=0x00D6
        | 0x00D8..=0x00F6
        | 0x00F8..=0x02AF
        | 0x0370..=0x0373
        | 0x0376..=0x0377
        | 0x037A..=0x037D
        | 0x037F
        | 0x0384..=0x038A
        | 0x038C
        | 0x038E..=0x03A1
        | 0x03A3..=0x03FF
        | 0x0400..=0x0482
        | 0x048A..=0x052F
        | 0x0531..=0x0556
        | 0x0559..=0x058A => BidiClass::L,

        // --- Combining diacritical marks (NSM) ------------------
        0x0300..=0x036F
        | 0x0483..=0x0489
        | 0x0591..=0x05BD
        | 0x05BF
        | 0x05C1..=0x05C2
        | 0x05C4..=0x05C5
        | 0x05C7
        | 0x0610..=0x061A
        | 0x064B..=0x065F
        | 0x0670
        | 0x06D6..=0x06DC
        | 0x06DF..=0x06E4
        | 0x06E7..=0x06E8
        | 0x06EA..=0x06ED
        | 0x0711
        | 0x0730..=0x074A
        | 0x07A6..=0x07B0
        | 0x07EB..=0x07F3
        | 0x1AB0..=0x1AFF
        | 0x1DC0..=0x1DFF
        | 0x20D0..=0x20FF
        | 0xFE20..=0xFE2F => BidiClass::Nsm,

        // --- Hebrew letters / punctuation (R) -------------------
        0x05BE | 0x05C0 | 0x05C3 | 0x05C6 | 0x05D0..=0x05EA | 0x05EF..=0x05F4 => BidiClass::R,
        // Hebrew presentation forms (R).
        0xFB1D..=0xFB4F => BidiClass::R,

        // --- Arabic letters (AL) --------------------------------
        // Note: harakat / fatha / kasra / etc handled by NSM range
        // above at U+064B..U+065F. The remaining Arabic letters,
        // tatweel, and punctuation are AL.
        0x0608
        | 0x060B
        | 0x060D
        | 0x061B..=0x064A
        | 0x066D..=0x066F
        | 0x0671..=0x06D5
        | 0x06E5..=0x06E6
        | 0x06EE..=0x06EF
        | 0x06FA..=0x06FF
        | 0x0750..=0x077F
        | 0x08A0..=0x08FF => BidiClass::Al,

        // --- Syriac (AL) ----------------------------------------
        0x0700..=0x070D | 0x070F | 0x0710 | 0x0712..=0x072F | 0x074D..=0x074F => BidiClass::Al,

        // --- Thaana (R) -----------------------------------------
        0x0780..=0x07A5 | 0x07B1 => BidiClass::R,

        // --- N'Ko (R) -------------------------------------------
        0x07C0..=0x07EA | 0x07F4..=0x07FF => BidiClass::R,

        // --- Arabic Presentation Forms-A and -B (AL) ------------
        0xFB50..=0xFDFF | 0xFE70..=0xFEFF => BidiClass::Al,

        // --- Devanagari, Bengali, Gurmukhi, Gujarati, Oriya,
        // Tamil, Telugu, Kannada, Malayalam, Sinhala, Thai, Lao,
        // Tibetan, Myanmar, CJK, Hangul, Yi etc. (L) -----------
        0x0900..=0x0DFF
        | 0x0E00..=0x0EFF
        | 0x0F00..=0x0FFF
        | 0x1000..=0x109F
        | 0x10A0..=0x10FF
        | 0x1100..=0x11FF
        | 0x1200..=0x16FF
        | 0x1700..=0x171F
        | 0x1720..=0x173F
        | 0x1740..=0x175F
        | 0x1760..=0x177F
        | 0x1780..=0x17FF
        | 0x1800..=0x18AF
        | 0x18B0..=0x18FF
        | 0x1900..=0x194F
        | 0x1950..=0x197F
        | 0x1980..=0x19DF
        | 0x19E0..=0x19FF
        | 0x1A00..=0x1A1F
        | 0x1A20..=0x1AAF
        | 0x1B00..=0x1B7F
        | 0x1B80..=0x1BBF
        | 0x1C00..=0x1C7F
        | 0x1CC0..=0x1CCF
        | 0x1CD3
        | 0x1CE1
        | 0x1CE9..=0x1CEC
        | 0x1CEE..=0x1CF1
        | 0x1CF5..=0x1CF6
        | 0x2C00..=0x2DFF
        | 0x2E80..=0x2FFF
        | 0x3005..=0x3007
        | 0x3021..=0x3029
        | 0x3031..=0x3035
        | 0x3038..=0x303C
        | 0x3041..=0x3096
        | 0x309D..=0x309F
        | 0x30A1..=0x30FA
        | 0x30FC..=0x30FF
        | 0x3105..=0x312F
        | 0x3131..=0x318E
        | 0x3190..=0x31BF
        | 0x31F0..=0x321C
        | 0x322A..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xA4D0..=0xA8FF
        | 0xA900..=0xAA5F
        | 0xAA60..=0xAA7F
        | 0xAA80..=0xAADF
        | 0xAAE0..=0xABFF
        | 0xAC00..=0xD7FF => BidiClass::L,

        // --- Brackets and other punctuation → ON ----------------
        // The algorithm's neutral handling will resolve these.
        _ => BidiClass::On,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_letter_is_l() {
        assert_eq!(bidi_class('A'), BidiClass::L);
        assert_eq!(bidi_class('z'), BidiClass::L);
    }

    #[test]
    fn hebrew_letter_is_r() {
        assert_eq!(bidi_class('\u{05D0}'), BidiClass::R); // alef
        assert_eq!(bidi_class('\u{05EA}'), BidiClass::R); // tav
    }

    #[test]
    fn arabic_letter_is_al() {
        assert_eq!(bidi_class('\u{0627}'), BidiClass::Al); // alef
        assert_eq!(bidi_class('\u{0645}'), BidiClass::Al); // mim
    }

    #[test]
    fn ascii_digit_is_en() {
        assert_eq!(bidi_class('0'), BidiClass::En);
        assert_eq!(bidi_class('9'), BidiClass::En);
    }

    #[test]
    fn arabic_digit_is_an() {
        assert_eq!(bidi_class('\u{0660}'), BidiClass::An);
        assert_eq!(bidi_class('\u{0669}'), BidiClass::An);
    }

    #[test]
    fn separators_classified() {
        assert_eq!(bidi_class('+'), BidiClass::Es);
        assert_eq!(bidi_class('-'), BidiClass::Es);
        assert_eq!(bidi_class(','), BidiClass::Cs);
        assert_eq!(bidi_class('.'), BidiClass::Cs);
        assert_eq!(bidi_class(':'), BidiClass::Cs);
        assert_eq!(bidi_class('/'), BidiClass::Cs);
    }

    #[test]
    fn terminators_classified() {
        assert_eq!(bidi_class('$'), BidiClass::Et);
        assert_eq!(bidi_class('%'), BidiClass::Et);
        assert_eq!(bidi_class('#'), BidiClass::Et);
    }

    #[test]
    fn whitespace_and_paragraph() {
        assert_eq!(bidi_class(' '), BidiClass::Ws);
        assert_eq!(bidi_class('\t'), BidiClass::S);
        assert_eq!(bidi_class('\n'), BidiClass::B);
    }

    #[test]
    fn explicit_format_characters() {
        assert_eq!(bidi_class('\u{202A}'), BidiClass::Lre);
        assert_eq!(bidi_class('\u{202B}'), BidiClass::Rle);
        assert_eq!(bidi_class('\u{202C}'), BidiClass::Pdf);
        assert_eq!(bidi_class('\u{202D}'), BidiClass::Lro);
        assert_eq!(bidi_class('\u{202E}'), BidiClass::Rlo);
        assert_eq!(bidi_class('\u{2066}'), BidiClass::Lri);
        assert_eq!(bidi_class('\u{2067}'), BidiClass::Rli);
        assert_eq!(bidi_class('\u{2068}'), BidiClass::Fsi);
        assert_eq!(bidi_class('\u{2069}'), BidiClass::Pdi);
    }

    #[test]
    fn lrm_rlm_alm_strong() {
        assert_eq!(bidi_class('\u{200E}'), BidiClass::L); // LRM
        assert_eq!(bidi_class('\u{200F}'), BidiClass::R); // RLM
        assert_eq!(bidi_class('\u{061C}'), BidiClass::R); // ALM
    }

    #[test]
    fn combining_marks_are_nsm() {
        assert_eq!(bidi_class('\u{0301}'), BidiClass::Nsm); // acute
        assert_eq!(bidi_class('\u{064E}'), BidiClass::Nsm); // fatha
    }

    #[test]
    fn unknown_falls_through_to_on() {
        // U+2603 SNOWMAN — not in any classified range.
        assert_eq!(bidi_class('\u{2603}'), BidiClass::On);
    }

    #[test]
    fn brackets_are_on() {
        assert_eq!(bidi_class('('), BidiClass::On);
        assert_eq!(bidi_class(')'), BidiClass::On);
        assert_eq!(bidi_class('['), BidiClass::On);
        assert_eq!(bidi_class(']'), BidiClass::On);
    }

    #[test]
    fn strong_predicate_matches_strong_classes() {
        assert!(BidiClass::L.is_strong());
        assert!(BidiClass::R.is_strong());
        assert!(BidiClass::Al.is_strong());
        assert!(!BidiClass::En.is_strong());
        assert!(!BidiClass::On.is_strong());
    }
}
