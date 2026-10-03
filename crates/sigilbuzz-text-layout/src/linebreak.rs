//! UAX #14 line break iterator.
//!
//! [`LineBreakIter`] applies the rules of UAX #14 revision 55
//! (Unicode 17.0.0) in order, LB1 through LB31, as a forward state
//! machine. The state carries what the rules look back at: the
//! previous unit after LB9 (a base character with its combining marks),
//! the unit before it, the last unit that is not a space (the `SP*`
//! rules), the regional indicator parity, and the numeric context of
//! LB25. The few rules that look ahead (LB15b, LB15c, LB19a, LB25, and
//! LB28a) read at most two units past the current character, so the
//! iterator runs in linear time.
//!
//! The rule summaries in the comments write the spec's no-break sign as
//! `x`.
//!
//! [`WordBreak`] tailors the rules the way CSS Text 3 `word-break`
//! does.

use core::str::CharIndices;

use crate::class::{
    lookup, LineBreakClass, EAST_ASIAN, FINAL_QUOTE, INITIAL_QUOTE, LETTER_UNIT, SA_MARK,
    UNASSIGNED_PICTOGRAPHIC,
};

/// Whether a position may host a line break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakOpportunity {
    /// Mandatory break: after `CR`, `LF`, `NL`, `BK`, and at the end of
    /// the text. The wrapper *must* break here.
    Mandatory,
    /// Allowed break. The wrapper *may* break here if the next chunk
    /// would not fit.
    Allowed,
    /// Prohibited break: included for completeness so callers can
    /// distinguish "we considered this position and rejected it" from
    /// "we never reached this position".
    Prohibited,
}

/// How line breaking treats letters, following the CSS Text 3
/// `word-break` property.
///
/// ```
/// use sigilbuzz_text_layout::{line_break_opportunities_with, WordBreak};
///
/// let breaks = |word_break| -> Vec<usize> {
///     line_break_opportunities_with("\u{D55C}\u{AD6D}\u{C5B4} \u{ACF5}\u{BD80}", word_break)
///         .map(|(offset, _)| offset)
///         .collect()
/// };
/// // Normal breaks between Hangul syllables.
/// assert_eq!(breaks(WordBreak::Normal), [3, 6, 10, 13, 16]);
/// // KeepAll breaks only at the space (and at the end).
/// assert_eq!(breaks(WordBreak::KeepAll), [10, 16]);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WordBreak {
    /// The default rules of UAX #14 (CSS `word-break: normal`).
    /// Ideographs and Hangul syllables break between characters, and
    /// words in alphabetic scripts break at spaces and punctuation.
    #[default]
    Normal,
    /// CSS `word-break: keep-all`. No implicit break between two
    /// typographic letter units: letters and numbers (General_Category
    /// L* and N*) and characters of class NU, AL, AI, or ID. Korean
    /// then breaks between words (at spaces) instead of between
    /// syllables. Everything else, including punctuation and spaces,
    /// breaks as under [`WordBreak::Normal`].
    KeepAll,
    /// CSS `word-break: break-all`. Letters and digits of class AL,
    /// HL, NU, AI, and SA are treated as ideographs (ID), so words in
    /// any script may break between characters. Punctuation still
    /// follows the default rules.
    BreakAll,
}

/// Iterator over [`BreakOpportunity`] decisions in a `&str`.
///
/// Each item is a `(byte_offset, opportunity)` pair. The byte offset is
/// the position of the break: a wrapper that splits `text[..offset]`
/// and `text[offset..]` produces the two sides of the break.
///
/// The iterator emits `Allowed` and `Mandatory` opportunities, in
/// ascending order of offset, and ends with `(text.len(), Mandatory)`
/// for non-empty text (LB3). It does *not* emit `Prohibited` rows
/// because they are uninteresting for almost all callers. `Prohibited`
/// is exported on the enum to give callers a vocabulary for their own
/// decisions.
#[derive(Debug, Clone)]
pub struct LineBreakIter<'a> {
    text: &'a str,
    chars: CharIndices<'a>,
    word_break: WordBreak,
    /// The previous unit, after LB9 and LB10. `None` at the start.
    prev: Option<Unit>,
    /// The last unit that is not SP: `prev` itself, or the unit before
    /// the run of spaces that ends at `prev`.
    last_non_space: Option<Unit>,
    /// The character before the current position is a ZWJ (LB8a).
    after_zwj: bool,
    /// `prev` ends a run of an odd number of regional indicators
    /// (LB30a).
    odd_regional_indicators: bool,
    /// Where `prev` stands in a number (LB25).
    number: Number,
    /// The end-of-text break has been emitted.
    finished: bool,
}

/// A base character as the rules see it: its resolved class and flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Base {
    class: LineBreakClass,
    flags: u8,
    /// U+25CC DOTTED CIRCLE, which LB28a groups with the aksaras.
    dotted_circle: bool,
}

impl Base {
    fn has(self, flag: u8) -> bool {
        self.flags & flag != 0
    }

    fn east_asian(self) -> bool {
        self.has(EAST_ASIAN)
    }

    /// AK, AS, or U+25CC (the `(AK | [U+25CC] | AS)` of LB28a).
    fn aksara(self) -> bool {
        matches!(self.class, LineBreakClass::AK | LineBreakClass::AS) || self.dotted_circle
    }
}

/// A unit after LB9: a base character (its combining marks add
/// nothing), and the unit before it.
#[derive(Debug, Clone, Copy)]
struct Unit {
    base: Base,
    before: Option<Base>,
}

/// The LB25 context of the previous unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Number {
    /// Not in a number.
    None,
    /// `NU (SY | IS)*`.
    Digits,
    /// `NU (SY | IS)* (CL | CP)`.
    Closed,
}

/// Resolves the class of `c` for the rules: LB1, then the
/// [`WordBreak::BreakAll`] tailoring.
fn resolve(c: char, word_break: WordBreak) -> Base {
    use LineBreakClass as L;
    let (class, flags) = lookup(c);
    let class = match class {
        L::AL | L::HL | L::NU | L::AI if word_break == WordBreak::BreakAll => L::ID,
        L::SA if flags & SA_MARK != 0 => L::CM,
        L::SA if word_break == WordBreak::BreakAll => L::ID,
        L::AI | L::SG | L::XX | L::SA => L::AL,
        L::CJ => L::NS,
        other => other,
    };
    Base {
        class,
        flags,
        dotted_circle: c == '\u{25CC}',
    }
}

/// A combining mark or ZWJ, which LB9 attaches to the base before it.
fn is_combining(class: LineBreakClass) -> bool {
    matches!(class, LineBreakClass::CM | LineBreakClass::ZWJ)
}

impl<'a> LineBreakIter<'a> {
    fn new(text: &'a str, word_break: WordBreak) -> Self {
        Self {
            text,
            chars: text.char_indices(),
            word_break,
            prev: None,
            last_non_space: None,
            after_zwj: false,
            odd_regional_indicators: false,
            number: Number::None,
            finished: false,
        }
    }

    /// The first unit that starts at or after byte `from`, skipping the
    /// combining marks that LB9 attaches to the character before
    /// `from`, and the byte offset after its base character. `None` at
    /// the end of the text.
    fn base_after(&self, from: usize) -> Option<(Base, usize)> {
        let rest = self.text.get(from..)?;
        rest.char_indices().find_map(|(i, c)| {
            let base = resolve(c, self.word_break);
            (!is_combining(base.class)).then_some((base, from + i + c.len_utf8()))
        })
    }

    /// The class of the unit after the one that starts with the
    /// character ending at byte `end`.
    fn class_after(&self, end: usize) -> Option<LineBreakClass> {
        self.base_after(end).map(|(base, _)| base.class)
    }

    /// LB10: a combining mark or ZWJ that LB9 did not attach is treated
    /// as U+0041 LATIN CAPITAL LETTER A.
    fn standalone(&self, base: Base) -> Base {
        if is_combining(base.class) {
            resolve('A', self.word_break)
        } else {
            base
        }
    }

    /// Decides the break before `c`, the character at `pos`, then moves
    /// past it.
    fn step(&mut self, pos: usize, c: char) -> BreakOpportunity {
        let base = resolve(c, self.word_break);
        let decision = match self.prev {
            // LB2: never break at the start of the text.
            None => {
                self.push(self.standalone(base));
                BreakOpportunity::Prohibited
            }
            // LB9: a combining mark or ZWJ joins the unit before it.
            // LB4 through LB8a never break there either: the unit
            // before is not a hard break, a space, or ZW.
            Some(prev) if is_combining(base.class) && can_carry_marks(prev.base.class) => {
                BreakOpportunity::Prohibited
            }
            Some(prev) => {
                let cur = self.standalone(base);
                let decision = match self.mandatory_rules(prev, cur) {
                    Some(decision) => decision,
                    None => self.pair_rules(prev, cur, pos + c.len_utf8()),
                };
                let decision = self.tailor(prev.base, cur, decision);
                self.push(cur);
                decision
            }
        };
        self.after_zwj = base.class == LineBreakClass::ZWJ;
        decision
    }

    /// Records `base` as the new previous unit.
    fn push(&mut self, base: Base) {
        use LineBreakClass as L;
        let prev = self.prev.map(|unit| unit.base);
        let unit = Unit { base, before: prev };
        let prev_class = prev.map(|base| base.class);
        self.odd_regional_indicators =
            base.class == L::RI && !(prev_class == Some(L::RI) && self.odd_regional_indicators);
        self.number = match base.class {
            L::NU => Number::Digits,
            L::SY | L::IS if self.number == Number::Digits => Number::Digits,
            L::CL | L::CP if self.number == Number::Digits => Number::Closed,
            _ => Number::None,
        };
        if base.class != L::SP {
            self.last_non_space = Some(unit);
        }
        self.prev = Some(unit);
    }

    /// LB4 through LB8a, the rules that come before LB9. `None` when
    /// none of them applies.
    fn mandatory_rules(&self, prev: Unit, base: Base) -> Option<BreakOpportunity> {
        use BreakOpportunity::{Allowed, Mandatory, Prohibited};
        use LineBreakClass as L;
        let decision = match (prev.base.class, base.class) {
            // LB4, LB5: break after hard line breaks, but not inside CR LF.
            (L::CR, L::LF) => Prohibited,
            (L::BK | L::CR | L::LF | L::NL, _) => Mandatory,
            // LB6: do not break before hard line breaks.
            (_, L::BK | L::CR | L::LF | L::NL) => Prohibited,
            // LB7: do not break before spaces or zero width space.
            (_, L::SP | L::ZW) => Prohibited,
            // LB8: break after a zero width space, even after spaces.
            _ if self.last_class() == Some(L::ZW) => Allowed,
            // LB8a: do not break after a zero width joiner.
            _ if self.after_zwj => Prohibited,
            _ => return None,
        };
        Some(decision)
    }

    fn last_class(&self) -> Option<LineBreakClass> {
        self.last_non_space.map(|unit| unit.base.class)
    }

    /// LB11 through LB31, between the units `prev` and `cur`. `end` is
    /// the byte offset after the base character of `cur`, where the
    /// look-ahead starts.
    fn pair_rules(&self, prev: Unit, cur: Base, end: usize) -> BreakOpportunity {
        use BreakOpportunity::{Allowed, Prohibited};
        use LineBreakClass as L;
        let p = prev.base.class;
        let c = cur.class;
        let before = prev.before.map(|base| base.class);
        let last = self.last_non_space;
        let last_class = last.map(|unit| unit.base.class);

        // LB11: do not break before or after a word joiner.
        if c == L::WJ || p == L::WJ {
            return Prohibited;
        }
        // LB12: do not break after NBSP and related characters.
        if p == L::GL {
            return Prohibited;
        }
        // LB12a: do not break before them, except after spaces and
        // hyphens.
        if c == L::GL && !matches!(p, L::SP | L::BA | L::HY | L::HH) {
            return Prohibited;
        }
        // LB13: do not break before ']', '!', or '/', even after spaces.
        if matches!(c, L::CL | L::CP | L::EX | L::SY) {
            return Prohibited;
        }
        // LB14: do not break after '[', even after spaces.
        if last_class == Some(L::OP) {
            return Prohibited;
        }
        // LB15a: do not break after an initial quotation mark at the
        // start of a line, after a space, or after opening punctuation,
        // even after spaces.
        if let Some(unit) = last {
            let opening = matches!(
                unit.before.map(|base| base.class),
                None | Some(L::BK | L::CR | L::LF | L::NL | L::OP | L::QU | L::GL | L::SP | L::ZW)
            );
            if unit.base.class == L::QU && unit.base.has(INITIAL_QUOTE) && opening {
                return Prohibited;
            }
        }
        // LB15b: do not break before a final quotation mark that ends a
        // line, or comes before a space, a prohibited break, or another
        // quotation mark.
        if c == L::QU && cur.has(FINAL_QUOTE) {
            let closing = matches!(
                self.class_after(end),
                None | Some(
                    L::SP
                        | L::GL
                        | L::WJ
                        | L::CL
                        | L::QU
                        | L::CP
                        | L::EX
                        | L::IS
                        | L::SY
                        | L::BK
                        | L::CR
                        | L::LF
                        | L::NL
                        | L::ZW
                )
            );
            if closing {
                return Prohibited;
            }
        }
        if c == L::IS {
            // LB15c: break before a decimal mark that follows a space.
            if p == L::SP && self.class_after(end) == Some(L::NU) {
                return Allowed;
            }
            // LB15d: otherwise do not break before ';', ',', or '.'.
            return Prohibited;
        }
        // LB16: do not break between closing punctuation and a
        // nonstarter, even after spaces.
        if matches!(last_class, Some(L::CL | L::CP)) && c == L::NS {
            return Prohibited;
        }
        // LB17: do not break within em dashes, even after spaces.
        if last_class == Some(L::B2) && c == L::B2 {
            return Prohibited;
        }
        // LB18: break after spaces.
        if p == L::SP {
            return Allowed;
        }
        // LB19: do not break before a quotation mark that is not
        // initial, or after one that is not final.
        if (c == L::QU && !cur.has(INITIAL_QUOTE)) || (p == L::QU && !prev.base.has(FINAL_QUOTE)) {
            return Prohibited;
        }
        // LB19a: unless surrounded by East Asian characters, do not
        // break either side of a quotation mark.
        if c == L::QU {
            if !prev.base.east_asian() {
                return Prohibited;
            }
            let next_east_asian = self
                .base_after(end)
                .is_some_and(|(base, _)| base.east_asian());
            if !next_east_asian {
                return Prohibited;
            }
        }
        if p == L::QU {
            if !cur.east_asian() {
                return Prohibited;
            }
            if !prev.before.is_some_and(Base::east_asian) {
                return Prohibited;
            }
        }
        // LB20: break before and after contingent break opportunities.
        if c == L::CB || p == L::CB {
            return Allowed;
        }
        // LB20a: do not break after a word-initial hyphen.
        if matches!(p, L::HY | L::HH)
            && matches!(c, L::AL | L::HL)
            && matches!(
                before,
                None | Some(L::BK | L::CR | L::LF | L::NL | L::SP | L::ZW | L::CB | L::GL)
            )
        {
            return Prohibited;
        }
        // LB21: do not break before hyphens and nonstarters, or after
        // acute accents.
        if matches!(c, L::BA | L::HH | L::HY | L::NS) || p == L::BB {
            return Prohibited;
        }
        // LB21a: do not break after the hyphen in Hebrew, hyphen,
        // non-Hebrew.
        if matches!(p, L::HY | L::HH) && before == Some(L::HL) && c != L::HL {
            return Prohibited;
        }
        // LB21b: do not break between a solidus and a Hebrew letter.
        if p == L::SY && c == L::HL {
            return Prohibited;
        }
        // LB22: do not break before ellipses.
        if c == L::IN {
            return Prohibited;
        }
        // LB23: do not break between digits and letters.
        if (matches!(p, L::AL | L::HL) && c == L::NU) || (p == L::NU && matches!(c, L::AL | L::HL))
        {
            return Prohibited;
        }
        // LB23a: do not break between numeric prefixes and ideographs,
        // or between ideographs and numeric postfixes.
        if (p == L::PR && matches!(c, L::ID | L::EB | L::EM))
            || (matches!(p, L::ID | L::EB | L::EM) && c == L::PO)
        {
            return Prohibited;
        }
        // LB24: do not break between numeric prefixes or postfixes and
        // letters.
        if (matches!(p, L::PR | L::PO) && matches!(c, L::AL | L::HL))
            || (matches!(p, L::AL | L::HL) && matches!(c, L::PR | L::PO))
        {
            return Prohibited;
        }
        // LB25: do not break numbers.
        if self.number_rules(p, c, end) {
            return Prohibited;
        }
        // LB26: do not break a Korean syllable.
        if (p == L::JL && matches!(c, L::JL | L::JV | L::H2 | L::H3))
            || (matches!(p, L::JV | L::H2) && matches!(c, L::JV | L::JT))
            || (matches!(p, L::JT | L::H3) && c == L::JT)
        {
            return Prohibited;
        }
        // LB27: treat a Korean syllable block like an ideograph.
        if (matches!(p, L::JL | L::JV | L::JT | L::H2 | L::H3) && c == L::PO)
            || (p == L::PR && matches!(c, L::JL | L::JV | L::JT | L::H2 | L::H3))
        {
            return Prohibited;
        }
        // LB28: do not break between alphabetics.
        if matches!(p, L::AL | L::HL) && matches!(c, L::AL | L::HL) {
            return Prohibited;
        }
        // LB28a: do not break inside the orthographic syllables of
        // Brahmic scripts.
        if self.brahmic_rules(prev, cur, end) {
            return Prohibited;
        }
        // LB29: do not break between numeric punctuation and
        // alphabetics.
        if p == L::IS && matches!(c, L::AL | L::HL) {
            return Prohibited;
        }
        // LB30: do not break between letters, numbers, or ordinary
        // symbols and non-East-Asian parentheses.
        if (matches!(p, L::AL | L::HL | L::NU) && c == L::OP && !cur.east_asian())
            || (p == L::CP && !prev.base.east_asian() && matches!(c, L::AL | L::HL | L::NU))
        {
            return Prohibited;
        }
        // LB30a: break between regional indicators only after an even
        // number of them.
        if p == L::RI && c == L::RI && self.odd_regional_indicators {
            return Prohibited;
        }
        // LB30b: do not break between an emoji base (or a potential
        // one) and an emoji modifier.
        if c == L::EM && (p == L::EB || prev.base.has(UNASSIGNED_PICTOGRAPHIC)) {
            return Prohibited;
        }
        // LB31: break everywhere else.
        Allowed
    }

    /// The LB25 rules. True when they prohibit the break between `p`
    /// and `c`.
    fn number_rules(&self, p: LineBreakClass, c: LineBreakClass, end: usize) -> bool {
        use LineBreakClass as L;
        match (p, c) {
            // NU (SY | IS)* (CL | CP)? x (PO | PR)
            (_, L::PO | L::PR) if self.number != Number::None => true,
            // (PR | PO) x NU, HY x NU, IS x NU
            (L::PR | L::PO | L::HY | L::IS, L::NU) => true,
            // NU (SY | IS)* x NU
            (_, L::NU) => self.number == Number::Digits,
            // (PR | PO) x OP IS? NU
            (L::PR | L::PO, L::OP) => match self.base_after(end) {
                Some((next, _)) if next.class == L::NU => true,
                Some((next, after)) if next.class == L::IS => {
                    self.class_after(after) == Some(L::NU)
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// The LB28a rules. True when they prohibit the break between
    /// `prev` and `cur`.
    fn brahmic_rules(&self, prev: Unit, cur: Base, end: usize) -> bool {
        use LineBreakClass as L;
        let p = prev.base;
        // AP x (AK | [U+25CC] | AS)
        (p.class == L::AP && cur.aksara())
            // (AK | [U+25CC] | AS) x (VF | VI)
            || (p.aksara() && matches!(cur.class, L::VF | L::VI))
            // (AK | [U+25CC] | AS) VI x (AK | [U+25CC])
            || (p.class == L::VI
                && prev.before.is_some_and(Base::aksara)
                && (cur.class == L::AK || cur.dotted_circle))
            // (AK | [U+25CC] | AS) x (AK | [U+25CC] | AS) VF
            || (p.aksara() && cur.aksara() && self.class_after(end) == Some(L::VF))
    }

    /// Applies [`WordBreak::KeepAll`] to a decision of the rules.
    fn tailor(&self, prev: Base, cur: Base, decision: BreakOpportunity) -> BreakOpportunity {
        let keep = self.word_break == WordBreak::KeepAll
            && decision == BreakOpportunity::Allowed
            && prev.has(LETTER_UNIT)
            && cur.has(LETTER_UNIT);
        if keep {
            BreakOpportunity::Prohibited
        } else {
            decision
        }
    }
}

/// LB9 attaches combining marks to any base except these.
fn can_carry_marks(class: LineBreakClass) -> bool {
    use LineBreakClass as L;
    !matches!(class, L::BK | L::CR | L::LF | L::NL | L::SP | L::ZW)
}

impl Iterator for LineBreakIter<'_> {
    type Item = (usize, BreakOpportunity);

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((pos, c)) = self.chars.next() {
            match self.step(pos, c) {
                BreakOpportunity::Prohibited => {}
                decision => return Some((pos, decision)),
            }
        }
        if self.finished || self.text.is_empty() {
            return None;
        }
        // LB3: always break at the end of the text.
        self.finished = true;
        Some((self.text.len(), BreakOpportunity::Mandatory))
    }
}

impl core::iter::FusedIterator for LineBreakIter<'_> {}

/// Returns an iterator over UAX #14 line break opportunities in `text`,
/// with the default rules ([`WordBreak::Normal`]).
///
/// Each item is `(byte_offset, opportunity)`: the offset at which the
/// wrapper may break, and whether the break is mandatory. The iterator
/// always ends with a `(text.len(), Mandatory)` sentinel for non-empty
/// text so callers have a flush point.
///
/// ```
/// use sigilbuzz_text_layout::{line_break_opportunities, BreakOpportunity};
///
/// let breaks: Vec<_> = line_break_opportunities("Hello, world!\nBye").collect();
/// assert_eq!(
///     breaks,
///     [
///         (7, BreakOpportunity::Allowed),
///         (14, BreakOpportunity::Mandatory),
///         (17, BreakOpportunity::Mandatory),
///     ]
/// );
/// ```
#[must_use]
pub fn line_break_opportunities(text: &str) -> LineBreakIter<'_> {
    LineBreakIter::new(text, WordBreak::Normal)
}

/// Returns an iterator over UAX #14 line break opportunities in `text`,
/// tailored by `word_break` the way CSS `word-break` tailors line
/// breaking.
///
/// [`WordBreak::Normal`] gives the same breaks as
/// [`line_break_opportunities`]. [`WordBreak::KeepAll`] keeps Korean
/// words (and CJK runs) whole and breaks them at spaces and
/// punctuation. [`WordBreak::BreakAll`] lets any word break between
/// letters.
///
/// ```
/// use sigilbuzz_text_layout::{line_break_opportunities_with, WordBreak};
///
/// let offsets = |text, word_break| -> Vec<usize> {
///     line_break_opportunities_with(text, word_break)
///         .map(|(offset, _)| offset)
///         .collect()
/// };
/// // "Korean is fun." in Korean, with a space between the two words.
/// let text = "\u{D55C}\u{AD6D}\u{C5B4}\u{B294} \u{C7AC}\u{BBF8}\u{C788}\u{C5B4}\u{C694}.";
/// assert_eq!(offsets(text, WordBreak::KeepAll), [13, 29]);
/// // English words break only at the space, unless break-all is on.
/// assert_eq!(offsets("big word", WordBreak::Normal), [4, 8]);
/// assert_eq!(offsets("big word", WordBreak::BreakAll), [1, 2, 4, 5, 6, 7, 8]);
/// ```
#[must_use]
pub fn line_break_opportunities_with(text: &str, word_break: WordBreak) -> LineBreakIter<'_> {
    LineBreakIter::new(text, word_break)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn opportunities(text: &str) -> Vec<(usize, BreakOpportunity)> {
        line_break_opportunities(text).collect()
    }

    /// Every break offset, Allowed and Mandatory, under `word_break`.
    fn offsets(text: &str, word_break: WordBreak) -> Vec<usize> {
        line_break_opportunities_with(text, word_break)
            .map(|(offset, _)| offset)
            .collect()
    }

    /// The text of each segment between breaks.
    fn segments(text: &str, word_break: WordBreak) -> Vec<&str> {
        let mut start = 0;
        offsets(text, word_break)
            .into_iter()
            .map(|end| {
                let segment = &text[start..end];
                start = end;
                segment
            })
            .collect()
    }

    fn allowed(text: &str) -> Vec<usize> {
        opportunities(text)
            .into_iter()
            .filter(|(_, o)| matches!(o, BreakOpportunity::Allowed))
            .map(|(p, _)| p)
            .collect()
    }

    fn mandatory_offsets(text: &str) -> Vec<usize> {
        opportunities(text)
            .into_iter()
            .filter(|(_, o)| matches!(o, BreakOpportunity::Mandatory))
            .map(|(p, _)| p)
            .collect()
    }

    #[test]
    fn empty_text_yields_no_breaks() {
        assert!(opportunities("").is_empty());
    }

    #[test]
    fn single_word_yields_only_end_sentinel() {
        assert_eq!(opportunities("hello"), [(5, BreakOpportunity::Mandatory)]);
    }

    #[test]
    fn space_separated_words_break_after_each_space() {
        assert_eq!(allowed("the quick brown"), vec![4, 10]);
    }

    #[test]
    fn lf_emits_mandatory_break() {
        assert_eq!(mandatory_offsets("a\nb"), vec![2, 3]);
    }

    #[test]
    fn crlf_breaks_once_after_the_lf() {
        assert_eq!(mandatory_offsets("a\r\nb"), vec![3, 4]);
    }

    #[test]
    fn lone_cr_emits_mandatory_break() {
        // UAX #14 LB5: CR ! when the CR is not followed by LF.
        assert_eq!(mandatory_offsets("a\rb"), vec![2, 3]);
    }

    #[test]
    fn consecutive_crs_break_after_each() {
        assert_eq!(mandatory_offsets("a\r\rb"), vec![2, 3, 4]);
    }

    #[test]
    fn cr_before_crlf_breaks_twice() {
        assert_eq!(mandatory_offsets("a\r\r\nb"), vec![2, 4, 5]);
    }

    #[test]
    fn trailing_cr_folds_into_end_sentinel() {
        assert_eq!(mandatory_offsets("a\r"), vec![2]);
    }

    #[test]
    fn nel_and_line_separator_emit_mandatory_breaks() {
        // U+0085 NEL (class NL) and U+2028 LINE SEPARATOR (class BK).
        assert_eq!(mandatory_offsets("a\u{0085}b"), vec![3, 4]);
        assert_eq!(mandatory_offsets("a\u{2028}b"), vec![4, 5]);
    }

    #[test]
    fn cjk_breaks_between_ideographs() {
        assert_eq!(allowed("\u{4E16}\u{754C}"), vec![3]);
    }

    #[test]
    fn hyphen_allows_break_after() {
        assert_eq!(allowed("co-op"), vec![3]);
    }

    #[test]
    fn no_break_inside_alphabetic_word() {
        assert!(allowed("foobar").is_empty());
    }

    #[test]
    fn nbsp_acts_as_glue() {
        // No break between "Mr." and "Smith" joined by NBSP.
        assert!(allowed("Mr.\u{00A0}Smith").is_empty());
    }

    #[test]
    fn punctuation_stays_with_the_word_before() {
        // LB13 and LB15d: no break before ',', '.', '!', or ')'.
        assert_eq!(
            segments("Hi, (you). Go!", WordBreak::Normal),
            ["Hi, ", "(you). ", "Go!"]
        );
    }

    #[test]
    fn numbers_stay_whole() {
        // LB25: prefixes, separators, and postfixes stay with digits.
        assert_eq!(
            segments("$12,345.67 (50%) -3", WordBreak::Normal),
            ["$12,345.67 ", "(50%) ", "-3"]
        );
    }

    #[test]
    fn space_before_decimal_mark_allows_a_break() {
        // LB15c: "subtract .5".
        assert_eq!(segments("a .5", WordBreak::Normal), ["a ", ".5"]);
    }

    #[test]
    fn quotation_marks_stay_with_their_words() {
        assert_eq!(
            segments("He said \u{201C}hi\u{201D} twice", WordBreak::Normal),
            ["He ", "said ", "\u{201C}hi\u{201D} ", "twice"]
        );
        assert_eq!(
            segments("\"a b\" c", WordBreak::Normal),
            ["\"a ", "b\" ", "c"]
        );
    }

    #[test]
    fn regional_indicators_pair_up() {
        // Three flags' worth of regional indicators break after each
        // pair (LB30a).
        let flags = "\u{1F1F0}\u{1F1F7}\u{1F1FA}\u{1F1F8}\u{1F1EF}\u{1F1F5}";
        assert_eq!(offsets(flags, WordBreak::Normal), [8, 16, 24]);
    }

    #[test]
    fn emoji_modifier_stays_with_its_base() {
        // U+1F466 BOY and a skin tone modifier, twice.
        let text = "\u{1F466}\u{1F3FB}\u{1F466}\u{1F3FB}";
        assert_eq!(offsets(text, WordBreak::Normal), [8, 16]);
    }

    #[test]
    fn combining_marks_follow_their_base() {
        // LB9: e + COMBINING ACUTE ACCENT between ideographs.
        let text = "\u{4E16}e\u{0301}\u{754C}";
        assert_eq!(offsets(text, WordBreak::Normal), [3, 6, 9]);
    }

    // --- Korean ------------------------------------------------------

    /// "한국어를 공부해요." ("I study Korean.")
    const STUDY: &str = "\u{D55C}\u{AD6D}\u{C5B4}\u{B97C} \u{ACF5}\u{BD80}\u{D574}\u{C694}.";

    #[test]
    fn korean_normal_breaks_between_syllables() {
        assert_eq!(
            segments(STUDY, WordBreak::Normal),
            [
                "\u{D55C}",
                "\u{AD6D}",
                "\u{C5B4}",
                "\u{B97C} ",
                "\u{ACF5}",
                "\u{BD80}",
                "\u{D574}",
                "\u{C694}."
            ]
        );
    }

    #[test]
    fn korean_keep_all_breaks_only_at_the_space() {
        assert_eq!(
            segments(STUDY, WordBreak::KeepAll),
            [
                "\u{D55C}\u{AD6D}\u{C5B4}\u{B97C} ",
                "\u{ACF5}\u{BD80}\u{D574}\u{C694}."
            ]
        );
    }

    #[test]
    fn korean_keep_all_with_corner_brackets() {
        // "「세로쓰기」도 됩니다" ("Vertical writing works too"). The
        // brackets keep their default behavior: no break after 「 or
        // before 」, a break after 」.
        let text =
            "\u{300C}\u{C138}\u{B85C}\u{C4F0}\u{AE30}\u{300D}\u{B3C4} \u{B429}\u{B2C8}\u{B2E4}";
        assert_eq!(
            segments(text, WordBreak::KeepAll),
            [
                "\u{300C}\u{C138}\u{B85C}\u{C4F0}\u{AE30}\u{300D}",
                "\u{B3C4} ",
                "\u{B429}\u{B2C8}\u{B2E4}"
            ]
        );
        assert_eq!(
            segments(text, WordBreak::Normal),
            [
                "\u{300C}\u{C138}",
                "\u{B85C}",
                "\u{C4F0}",
                "\u{AE30}\u{300D}",
                "\u{B3C4} ",
                "\u{B429}",
                "\u{B2C8}",
                "\u{B2E4}"
            ]
        );
    }

    #[test]
    fn korean_keep_all_with_quotes_and_periods() {
        // "그는 “안녕.”이라고 했다." ("He said 'hello.'")
        let text = "\u{ADF8}\u{B294} \u{201C}\u{C548}\u{B155}.\u{201D}\u{C774}\u{B77C}\u{ACE0} \
                    \u{D588}\u{B2E4}.";
        assert_eq!(
            segments(text, WordBreak::KeepAll),
            [
                "\u{ADF8}\u{B294} ",
                "\u{201C}\u{C548}\u{B155}.\u{201D}\u{C774}\u{B77C}\u{ACE0} ",
                "\u{D588}\u{B2E4}."
            ]
        );
    }

    #[test]
    fn korean_mixed_with_latin() {
        // "Rust로 작성된 (예시) 코드" ("example code written in Rust").
        let text = "Rust\u{B85C} \u{C791}\u{C131}\u{B41C} (\u{C608}\u{C2DC}) \u{CF54}\u{B4DC}";
        assert_eq!(
            segments(text, WordBreak::KeepAll),
            [
                "Rust\u{B85C} ",
                "\u{C791}\u{C131}\u{B41C} ",
                "(\u{C608}\u{C2DC}) ",
                "\u{CF54}\u{B4DC}"
            ]
        );
        // Normal breaks between "Rust" and the particle, and between
        // syllables, but never inside the parentheses' edges.
        assert_eq!(
            segments(text, WordBreak::Normal),
            [
                "Rust",
                "\u{B85C} ",
                "\u{C791}",
                "\u{C131}",
                "\u{B41C} ",
                "(\u{C608}",
                "\u{C2DC}) ",
                "\u{CF54}",
                "\u{B4DC}"
            ]
        );
    }

    #[test]
    fn decomposed_jamo_never_split() {
        // U+1112 U+1161 U+11AB is "한" spelled with conjoining jamo
        // (LB26). Two such syllables break only between them.
        let han = "\u{1112}\u{1161}\u{11AB}";
        assert_eq!(offsets(han, WordBreak::Normal), [9]);
        let twice = "\u{1112}\u{1161}\u{11AB}\u{1112}\u{1161}\u{11AB}";
        assert_eq!(offsets(twice, WordBreak::Normal), [9, 18]);
        assert_eq!(offsets(twice, WordBreak::KeepAll), [18]);
        assert_eq!(offsets(twice, WordBreak::BreakAll), [9, 18]);
        // A precomposed LV syllable takes a trailing jamo too.
        assert_eq!(offsets("\u{D558}\u{11AB}", WordBreak::Normal), [6]);
    }

    #[test]
    fn keep_all_leaves_other_scripts_alone() {
        assert_eq!(
            offsets("the quick brown", WordBreak::KeepAll),
            offsets("the quick brown", WordBreak::Normal)
        );
        // Ideographs keep together too.
        assert_eq!(offsets("\u{4E16}\u{754C}", WordBreak::KeepAll), [6]);
    }

    #[test]
    fn break_all_breaks_inside_words() {
        assert_eq!(segments("abc", WordBreak::BreakAll), ["a", "b", "c"]);
        assert_eq!(
            segments("ab cd.", WordBreak::BreakAll),
            ["a", "b ", "c", "d."]
        );
        // Digits break too, punctuation still does not start a line.
        assert_eq!(segments("12,3", WordBreak::BreakAll), ["1", "2,", "3"]);
        // Korean is already per-syllable.
        assert_eq!(
            offsets(STUDY, WordBreak::BreakAll),
            offsets(STUDY, WordBreak::Normal)
        );
    }

    #[test]
    fn word_break_default_is_normal() {
        assert_eq!(WordBreak::default(), WordBreak::Normal);
        assert_eq!(
            offsets(STUDY, WordBreak::default()),
            offsets(STUDY, WordBreak::Normal)
        );
    }

    #[test]
    fn iterator_is_fused() {
        let mut iter = line_break_opportunities("a");
        assert_eq!(iter.next(), Some((1, BreakOpportunity::Mandatory)));
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);
    }
}
