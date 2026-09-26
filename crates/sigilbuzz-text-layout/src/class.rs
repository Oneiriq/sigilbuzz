//! UAX #14 line-break classes and a curated codepoint classifier.
//!
//! The classifier covers the codepoints needed to wrap English /
//! European text (Latin / Greek / Cyrillic, ASCII punctuation, common
//! quotation forms, hyphens) and CJK ideographs at every grapheme
//! boundary. Anything we have not classified falls back to
//! [`LineBreakClass::AL`], which is the UAX 14 default class for
//! "alphabetic", a safe choice that participates in normal
//! pair-table behavior without inventing breaks.

/// UAX #14 line-break class, restricted to the subset we implement.
///
/// Variants mirror the spec's two-letter abbreviations (`BK`, `CR`,
/// `LF`, ...) so the pair-table reads like the spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::upper_case_acronyms)]
pub enum LineBreakClass {
    /// Mandatory break (LB4).
    BK,
    /// Carriage return (LB5).
    CR,
    /// Line feed (LB5).
    LF,
    /// Next line (LB5).
    NL,
    /// Word joiner: never break around (LB11).
    WJ,
    /// Close punctuation (LB13).
    CL,
    /// Closing parenthesis (LB13/LB16).
    CP,
    /// Open punctuation (LB14).
    OP,
    /// Quotation (LB15/LB19).
    QU,
    /// Non-breaking glue (LB12).
    GL,
    /// Non-starter (LB16).
    NS,
    /// Combining mark (LB9 / LB10).
    CM,
    /// Space (LB7 / LB18).
    SP,
    /// Break-after: hyphen-minus, en-dash, etc. (LB21).
    BA,
    /// Break-before (LB21).
    BB,
    /// Hyphen: break-after but with NU interaction (LB21).
    HY,
    /// Alphabetic: Latin / Greek / Cyrillic letters (LB28).
    AL,
    /// Numeric (LB23 / LB25).
    NU,
    /// Prefix numeric (LB25).
    PR,
    /// Postfix numeric (LB25).
    PO,
    /// Ideographic: CJK, Yi, etc. (LB23a / LB29).
    ID,
    /// Exclamation / question marks (LB13).
    EX,
    /// Zero-width space: break after (LB8).
    ZW,
    /// Emoji base (LB30b).
    EB,
    /// Emoji modifier (LB30b).
    EM,
}

/// Returns the UAX 14 line-break class for a codepoint.
///
/// Codepoints we have not curated fall back to [`LineBreakClass::AL`].
#[must_use]
pub fn line_break_class(c: char) -> LineBreakClass {
    let cp = c as u32;

    // Mandatory break / line-feed family: the LB4-LB6 controls.
    match cp {
        0x000B | 0x000C | 0x0085 | 0x2028 | 0x2029 => return LineBreakClass::BK,
        0x000D => return LineBreakClass::CR,
        0x000A => return LineBreakClass::LF,
        _ => {}
    }

    // Zero-width space and word-joiner.
    if cp == 0x200B {
        return LineBreakClass::ZW;
    }
    if cp == 0x2060 || cp == 0xFEFF {
        return LineBreakClass::WJ;
    }

    // Spaces.
    if cp == 0x0020
        || cp == 0x1680
        || (0x2000..=0x200A).contains(&cp)
        || cp == 0x205F
        || cp == 0x3000
    {
        return LineBreakClass::SP;
    }
    if cp == 0x00A0 || cp == 0x202F {
        // Non-breaking space: GL in UAX 14.
        return LineBreakClass::GL;
    }
    // Tab counts as BA in our subset (break-after) so wrapping treats it
    // like a soft break point.
    if cp == 0x0009 {
        return LineBreakClass::BA;
    }

    // ASCII punctuation.
    match cp {
        // Open punctuation.
        0x0028 | 0x005B | 0x007B => return LineBreakClass::OP,
        // Close punctuation.
        0x005D | 0x007D => return LineBreakClass::CL,
        // Closing parenthesis.
        0x0029 => return LineBreakClass::CP,
        // Hyphen-minus is HY in UAX 14.
        0x002D => return LineBreakClass::HY,
        // Exclamation / question / colon / semicolon / ASCII fullwidth.
        0x0021 | 0x003F => return LineBreakClass::EX,
        // Comma, period, colon, semicolon: non-starters in UAX 14.
        0x002C | 0x002E | 0x003A | 0x003B => return LineBreakClass::NS,
        // Slash and other break-after punctuation.
        0x002F => return LineBreakClass::BA,
        // Quotation forms.
        0x0022 | 0x0027 => return LineBreakClass::QU,
        // Numeric digits.
        0x0030..=0x0039 => return LineBreakClass::NU,
        // Currency / prefix-numeric (PR).
        0x0024 | 0x00A3 | 0x00A5 | 0x20AC | 0x00A2 => return LineBreakClass::PR,
        // Postfix-numeric (PO): percent / per-mille / degree.
        0x0025 | 0x00B0 | 0x2030 | 0x2031 => return LineBreakClass::PO,
        _ => {}
    }

    // Curly / smart quotation marks.
    if matches!(
        cp,
        0x2018 | 0x2019 | 0x201A | 0x201B | 0x201C | 0x201D | 0x201E | 0x201F | 0x00AB | 0x00BB
    ) {
        return LineBreakClass::QU;
    }

    // Dashes: en/em/figure/horizontal-bar are BA. Soft-hyphen -> BA.
    if matches!(cp, 0x2010 | 0x2012 | 0x2013 | 0x2014 | 0x2015 | 0x00AD) {
        return LineBreakClass::BA;
    }

    // Mid-line dot leaders / horizontal ellipsis are non-starters.
    if matches!(cp, 0x2026 | 0x2025 | 0x22EF) {
        return LineBreakClass::NS;
    }

    // CJK ideographic ranges. We treat each as its own break point
    // (LB29/LB30), which matches the UAX 14 spec and gives Chinese / Japanese
    // / Korean text the per-grapheme wrap behavior the user expects.
    if (0x3040..=0x309F).contains(&cp)        // Hiragana
        || (0x30A0..=0x30FF).contains(&cp)    // Katakana
        || (0x3400..=0x4DBF).contains(&cp)    // CJK Ext A
        || (0x4E00..=0x9FFF).contains(&cp)    // CJK Unified
        || (0xF900..=0xFAFF).contains(&cp)    // CJK Compat Ideographs
        || (0xAC00..=0xD7AF).contains(&cp)    // Hangul Syllables
        || (0x20000..=0x2FFFF).contains(&cp)
    // CJK Ext B-F
    {
        return LineBreakClass::ID;
    }

    // Halfwidth / fullwidth CJK punctuation that *must not* start a
    // line: non-starters in UAX 14.
    if matches!(
        cp,
        0x3001 | 0x3002 | 0xFF01 | 0xFF0C | 0xFF0E | 0xFF1A | 0xFF1B | 0xFF1F
    ) {
        return LineBreakClass::NS;
    }
    // Fullwidth open / close brackets.
    if matches!(
        cp,
        0x3008 | 0x300A | 0x300C | 0x300E | 0x3010 | 0xFF08 | 0xFF3B | 0xFF5B
    ) {
        return LineBreakClass::OP;
    }
    if matches!(
        cp,
        0x3009 | 0x300B | 0x300D | 0x300F | 0x3011 | 0xFF09 | 0xFF3D | 0xFF5D
    ) {
        return LineBreakClass::CL;
    }

    // Combining marks (general category Mn / Mc) for the Latin /
    // Greek / Cyrillic ranges we cover. Any combining mark that lacks
    // a specific class falls into CM.
    if (0x0300..=0x036F).contains(&cp)
        || (0x1AB0..=0x1AFF).contains(&cp)
        || (0x1DC0..=0x1DFF).contains(&cp)
        || (0x20D0..=0x20FF).contains(&cp)
        || (0xFE20..=0xFE2F).contains(&cp)
    {
        return LineBreakClass::CM;
    }

    // Emoji modifiers: Fitzpatrick skin tones (LB30b).
    if (0x1F3FB..=0x1F3FF).contains(&cp) {
        return LineBreakClass::EM;
    }

    // Common emoji bases. We err on the side of covering pictographic
    // ranges; over-classifying as EB is benign because EB only matters
    // in the EB x EM rule.
    if (0x1F300..=0x1F5FF).contains(&cp)
        || (0x1F600..=0x1F64F).contains(&cp)
        || (0x1F900..=0x1F9FF).contains(&cp)
        || (0x1FA70..=0x1FAFF).contains(&cp)
    {
        return LineBreakClass::EB;
    }

    // Latin / Greek / Cyrillic / general alphabetic. The UAX 14
    // default for any letter we have not specially classified.
    LineBreakClass::AL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_letters_are_alphabetic() {
        assert_eq!(line_break_class('A'), LineBreakClass::AL);
        assert_eq!(line_break_class('z'), LineBreakClass::AL);
    }

    #[test]
    fn space_is_sp() {
        assert_eq!(line_break_class(' '), LineBreakClass::SP);
    }

    #[test]
    fn hyphen_minus_is_hy() {
        assert_eq!(line_break_class('-'), LineBreakClass::HY);
    }

    #[test]
    fn newline_classes() {
        assert_eq!(line_break_class('\n'), LineBreakClass::LF);
        assert_eq!(line_break_class('\r'), LineBreakClass::CR);
    }

    #[test]
    fn cjk_is_ideographic() {
        assert_eq!(line_break_class('世'), LineBreakClass::ID);
        assert_eq!(line_break_class('界'), LineBreakClass::ID);
        assert_eq!(line_break_class('あ'), LineBreakClass::ID);
    }

    #[test]
    fn digits_are_numeric() {
        assert_eq!(line_break_class('0'), LineBreakClass::NU);
        assert_eq!(line_break_class('9'), LineBreakClass::NU);
    }

    #[test]
    fn smart_quotes_are_qu() {
        assert_eq!(line_break_class('\u{201C}'), LineBreakClass::QU);
        assert_eq!(line_break_class('\u{201D}'), LineBreakClass::QU);
    }

    #[test]
    fn nbsp_is_glue() {
        assert_eq!(line_break_class('\u{00A0}'), LineBreakClass::GL);
    }

    #[test]
    fn zwsp_is_zw() {
        assert_eq!(line_break_class('\u{200B}'), LineBreakClass::ZW);
    }
}
