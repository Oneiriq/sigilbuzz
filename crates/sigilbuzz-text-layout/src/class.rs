//! UAX #14 line break classes and the code point classifier.
//!
//! The classes come from the generated `LINE_BREAK` table, which holds
//! the `Line_Break` property of Unicode 17.0.0 together with the flag
//! bits below.

use crate::line_break_table::LINE_BREAK;

/// The `Line_Break` property of a character (UAX #14, Unicode 17.0.0).
///
/// Variants mirror the spec's two-letter abbreviations (`BK`, `CR`,
/// `LF`, ...) so the rules read like the spec. [`line_break_class`]
/// returns the property as the Unicode Character Database assigns it,
/// before the algorithm resolves AI, CJ, SA, SG, and XX (rule LB1).
///
/// New Unicode versions can add classes, so the enum is
/// `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
// The public variant names are the spec's class codes, so they stay upper case.
#[allow(clippy::upper_case_acronyms)]
pub enum LineBreakClass {
    /// Mandatory break, such as U+2028 LINE SEPARATOR (LB4).
    BK,
    /// Carriage return (LB5).
    CR,
    /// Line feed (LB5).
    LF,
    /// Next line, U+0085 (LB5).
    NL,
    /// Space, U+0020 (LB7, LB18).
    SP,
    /// Zero width space, U+200B (LB7, LB8).
    ZW,
    /// Word joiner: never break around it (LB11).
    WJ,
    /// Non-breaking glue, such as U+00A0 NO-BREAK SPACE (LB12).
    GL,
    /// Combining mark (LB9, LB10).
    CM,
    /// Zero width joiner, U+200D (LB8a).
    ZWJ,
    /// Contingent break opportunity, such as U+FFFC (LB20).
    CB,
    /// Close punctuation (LB13, LB16).
    CL,
    /// Closing parenthesis (LB13, LB30).
    CP,
    /// Open punctuation (LB14, LB30).
    OP,
    /// Quotation mark (LB15a, LB15b, LB19).
    QU,
    /// Exclamation or interrogation (LB13).
    EX,
    /// Infix numeric separator, such as `.` and `,` (LB15c, LB15d).
    IS,
    /// Symbol allowing a break after, such as `/` (LB13).
    SY,
    /// Nonstarter, such as small kana and U+3005 (LB21).
    NS,
    /// Break after, such as tab and most dashes (LB21).
    BA,
    /// Break before (LB21).
    BB,
    /// Break opportunity before and after, U+2014 EM DASH (LB17).
    B2,
    /// Hyphen-minus (LB20a, LB21).
    HY,
    /// Unambiguous hyphen, such as U+2010 HYPHEN (LB20a, LB21).
    HH,
    /// Inseparable characters, such as the ellipsis (LB22).
    IN,
    /// Numeric (LB23, LB25).
    NU,
    /// Prefix numeric, such as `$` (LB24, LB25).
    PR,
    /// Postfix numeric, such as `%` (LB24, LB25).
    PO,
    /// Alphabetic and ordinary symbols (LB28).
    AL,
    /// Hebrew letter (LB21a, LB28).
    HL,
    /// Ideographic, such as CJK ideographs and kana (LB31).
    ID,
    /// Emoji base (LB30b).
    EB,
    /// Emoji modifier (LB30b).
    EM,
    /// Regional indicator (LB30a).
    RI,
    /// Hangul LV syllable (LB26).
    H2,
    /// Hangul LVT syllable (LB26).
    H3,
    /// Hangul leading jamo (LB26).
    JL,
    /// Hangul vowel jamo (LB26).
    JV,
    /// Hangul trailing jamo (LB26).
    JT,
    /// Aksara of a Brahmic script (LB28a).
    AK,
    /// Aksara prebase of a Brahmic script (LB28a).
    AP,
    /// Aksara start of a Brahmic script (LB28a).
    AS,
    /// Virama final of a Brahmic script (LB28a).
    VF,
    /// Virama of a Brahmic script (LB28a).
    VI,
    /// Ambiguous: resolved to AL (LB1).
    AI,
    /// Complex context dependent, Southeast Asian: resolved to CM or AL
    /// (LB1).
    SA,
    /// Surrogate: resolved to AL (LB1).
    SG,
    /// Conditional Japanese starter, small kana: resolved to NS (LB1).
    CJ,
    /// Unknown, including most unassigned code points: resolved to AL
    /// (LB1).
    XX,
}

// The flag bits of the `LINE_BREAK` table. `tests/table_gen.rs` uses
// the same values.

/// East_Asian_Width is F, W, or H (`$EastAsian` in UAX #14).
pub(crate) const EAST_ASIAN: u8 = 1;
/// A QU character with General_Category Pi.
pub(crate) const INITIAL_QUOTE: u8 = 2;
/// A QU character with General_Category Pf.
pub(crate) const FINAL_QUOTE: u8 = 4;
/// An SA character with General_Category Mn or Mc, which LB1 resolves
/// to CM.
pub(crate) const SA_MARK: u8 = 8;
/// A letter unit for CSS `word-break: keep-all`, as Blink's
/// `ShouldKeepAfterKeepAll` finds them: a letter or number
/// (General_Category L* or N*) that is not of class SA.
pub(crate) const LETTER_UNIT: u8 = 16;
/// An unassigned (General_Category Cn) Extended_Pictographic code
/// point (LB30b).
pub(crate) const UNASSIGNED_PICTOGRAPHIC: u8 = 32;

/// Returns the line break class and the flag bits of `c`.
pub(crate) fn lookup(c: char) -> (LineBreakClass, u8) {
    let cp = c as u32;
    let i = LINE_BREAK.partition_point(|&(_, last, _, _)| last < cp);
    match LINE_BREAK.get(i) {
        Some(&(first, _, class, flags)) if first <= cp => (class, flags),
        _ => (LineBreakClass::XX, 0),
    }
}

/// Returns the UAX #14 `Line_Break` property of `c`, as the Unicode
/// Character Database (Unicode 17.0.0) assigns it.
///
/// Unassigned code points take the defaults of the Unicode Character
/// Database: ID in the CJK ideograph blocks, in Planes 2 and 3, and in
/// the emoji ranges U+1F000..U+1FAFF and U+1FC00..U+1FFFD, PR in the
/// Currency Symbols block (U+20A0..U+20CF), and
/// [`LineBreakClass::XX`] everywhere else.
///
/// The line break iterators resolve AI, SG, and XX to AL, SA to CM or
/// AL, and CJ to NS before they apply the rules (LB1).
/// [`WordBreak::BreakAll`](crate::WordBreak::BreakAll) resolves AI, CJ,
/// SG, XX, and the SA letters to ID instead.
///
/// ```
/// use sigilbuzz_text_layout::{line_break_class, LineBreakClass};
///
/// assert_eq!(line_break_class('a'), LineBreakClass::AL);
/// assert_eq!(line_break_class(' '), LineBreakClass::SP);
/// // A Hangul LV syllable and the jamo it decomposes into.
/// assert_eq!(line_break_class('\u{AC00}'), LineBreakClass::H2);
/// assert_eq!(line_break_class('\u{1100}'), LineBreakClass::JL);
/// assert_eq!(line_break_class('\u{1161}'), LineBreakClass::JV);
/// // Unassigned code points in a CJK block, in Currency Symbols, and
/// // in Greek.
/// assert_eq!(line_break_class('\u{2A6E0}'), LineBreakClass::ID);
/// assert_eq!(line_break_class('\u{20CF}'), LineBreakClass::PR);
/// assert_eq!(line_break_class('\u{0378}'), LineBreakClass::XX);
/// ```
#[must_use]
pub fn line_break_class(c: char) -> LineBreakClass {
    lookup(c).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_classes() {
        assert_eq!(line_break_class('A'), LineBreakClass::AL);
        assert_eq!(line_break_class('z'), LineBreakClass::AL);
        assert_eq!(line_break_class('0'), LineBreakClass::NU);
        assert_eq!(line_break_class(' '), LineBreakClass::SP);
        assert_eq!(line_break_class('-'), LineBreakClass::HY);
        assert_eq!(line_break_class('\t'), LineBreakClass::BA);
        assert_eq!(line_break_class('\n'), LineBreakClass::LF);
        assert_eq!(line_break_class('\r'), LineBreakClass::CR);
        assert_eq!(line_break_class('.'), LineBreakClass::IS);
        assert_eq!(line_break_class(','), LineBreakClass::IS);
        assert_eq!(line_break_class('/'), LineBreakClass::SY);
        assert_eq!(line_break_class('"'), LineBreakClass::QU);
        assert_eq!(line_break_class('('), LineBreakClass::OP);
        assert_eq!(line_break_class(')'), LineBreakClass::CP);
    }

    #[test]
    fn korean_classes() {
        assert_eq!(line_break_class('\u{D55C}'), LineBreakClass::H3);
        assert_eq!(line_break_class('\u{AC00}'), LineBreakClass::H2);
        assert_eq!(line_break_class('\u{1112}'), LineBreakClass::JL);
        assert_eq!(line_break_class('\u{1161}'), LineBreakClass::JV);
        assert_eq!(line_break_class('\u{11AB}'), LineBreakClass::JT);
    }

    #[test]
    fn cjk_classes() {
        assert_eq!(line_break_class('\u{4E16}'), LineBreakClass::ID);
        assert_eq!(line_break_class('\u{3042}'), LineBreakClass::ID);
        assert_eq!(line_break_class('\u{3041}'), LineBreakClass::CJ);
        assert_eq!(line_break_class('\u{3005}'), LineBreakClass::NS);
        assert_eq!(line_break_class('\u{300C}'), LineBreakClass::OP);
        assert_eq!(line_break_class('\u{300D}'), LineBreakClass::CL);
        assert_eq!(line_break_class('\u{3002}'), LineBreakClass::CL);
    }

    #[test]
    fn other_classes() {
        assert_eq!(line_break_class('\u{00A0}'), LineBreakClass::GL);
        assert_eq!(line_break_class('\u{200B}'), LineBreakClass::ZW);
        assert_eq!(line_break_class('\u{200D}'), LineBreakClass::ZWJ);
        assert_eq!(line_break_class('\u{2060}'), LineBreakClass::WJ);
        assert_eq!(line_break_class('\u{2014}'), LineBreakClass::B2);
        assert_eq!(line_break_class('\u{0E01}'), LineBreakClass::SA);
        assert_eq!(line_break_class('\u{05D0}'), LineBreakClass::HL);
        assert_eq!(line_break_class('\u{1F1E6}'), LineBreakClass::RI);
        assert_eq!(line_break_class('\u{1F466}'), LineBreakClass::EB);
        assert_eq!(line_break_class('\u{1F3FB}'), LineBreakClass::EM);
        assert_eq!(line_break_class('\u{1B05}'), LineBreakClass::AK);
        assert_eq!(line_break_class('\u{10FFFF}'), LineBreakClass::XX);
    }

    #[test]
    fn unassigned_code_points_take_the_ucd_defaults() {
        // ID in the CJK blocks, Planes 2 and 3, and the emoji ranges.
        for c in [
            '\u{FA6E}',
            '\u{2A6E0}',
            '\u{3FFFD}',
            '\u{1F02C}',
            '\u{1FFFD}',
        ] {
            assert_eq!(line_break_class(c), LineBreakClass::ID, "{c:?}");
        }
        // PR in Currency Symbols.
        assert_eq!(line_break_class('\u{20C2}'), LineBreakClass::PR);
        assert_eq!(line_break_class('\u{20CF}'), LineBreakClass::PR);
        // XX elsewhere. Private use code points are XX too.
        for c in ['\u{0378}', '\u{E0080}', '\u{E000}', '\u{10FFFF}'] {
            assert_eq!(line_break_class(c), LineBreakClass::XX, "{c:?}");
        }
    }

    #[test]
    fn flags_match_the_generator() {
        let flags = |c| lookup(c).1;
        assert_eq!(flags('\u{AC00}'), EAST_ASIAN | LETTER_UNIT);
        assert_eq!(flags('\u{201C}'), INITIAL_QUOTE);
        assert_eq!(flags('\u{201D}'), FINAL_QUOTE);
        assert_eq!(flags('\u{0E31}') & SA_MARK, SA_MARK);
        assert_eq!(flags('\u{0E01}') & SA_MARK, 0);
        assert_eq!(flags('\u{300C}'), EAST_ASIAN);
        assert_eq!(flags('('), 0);
        assert_eq!(flags('\u{1F02C}'), UNASSIGNED_PICTOGRAPHIC);
        assert_eq!(flags('\u{0E01}') & LETTER_UNIT, 0);
        assert_eq!(flags('\u{1F600}') & LETTER_UNIT, 0);
    }
}
