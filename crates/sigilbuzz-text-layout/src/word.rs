//! UAX #29 word boundaries.
//!
//! [`WordBreakIter`] applies the word boundary rules of UAX #29
//! revision 47 (Unicode 17.0.0), WB1 through WB999, over the
//! `Word_Break` property from the generated `WORD_BREAK` table. Like
//! the line break iterator it is a forward state machine: the state
//! holds the previous two units after WB4 (a character with the
//! Extend, Format, and ZWJ characters that follow it), the raw previous
//! character for WB3 through WB3d, and the regional indicator parity.
//! The rules that look ahead (WB6, WB7b, and WB12) read one unit past
//! the current character, so the iterator runs in linear time.

use core::str::CharIndices;

use crate::word_break_table::{EXTENDED_PICTOGRAPHIC, WORD_BREAK};

/// The `Word_Break` property of a character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WordClass {
    Other,
    Cr,
    Lf,
    Newline,
    Extend,
    Zwj,
    RegionalIndicator,
    Format,
    Katakana,
    HebrewLetter,
    ALetter,
    SingleQuote,
    DoubleQuote,
    MidNumLet,
    MidLetter,
    MidNum,
    Numeric,
    ExtendNumLet,
    WSegSpace,
}

fn word_class(c: char) -> WordClass {
    let cp = c as u32;
    let i = WORD_BREAK.partition_point(|&(_, last, _)| last < cp);
    match WORD_BREAK.get(i) {
        Some(&(first, _, class)) if first <= cp => class,
        _ => WordClass::Other,
    }
}

fn is_extended_pictographic(c: char) -> bool {
    let cp = c as u32;
    let i = EXTENDED_PICTOGRAPHIC.partition_point(|&(_, last)| last < cp);
    EXTENDED_PICTOGRAPHIC
        .get(i)
        .is_some_and(|&(first, _)| first <= cp)
}

impl WordClass {
    /// `AHLetter`: ALetter or Hebrew_Letter.
    fn letter(self) -> bool {
        matches!(self, Self::ALetter | Self::HebrewLetter)
    }

    /// `MidNumLetQ`: MidNumLet or Single_Quote.
    fn mid_num_let(self) -> bool {
        matches!(self, Self::MidNumLet | Self::SingleQuote)
    }

    /// The characters WB4 attaches to the character before them.
    fn ignored(self) -> bool {
        matches!(self, Self::Extend | Self::Format | Self::Zwj)
    }

    fn newline(self) -> bool {
        matches!(self, Self::Newline | Self::Cr | Self::Lf)
    }
}

/// Returns an iterator over the UAX #29 word boundaries of `text`.
///
/// The iterator yields the byte offset of every boundary after the
/// start of the text, ending with `text.len()` for non-empty text, so
/// the slices between successive offsets (starting from 0) are the
/// segments: words, runs of spaces, and single punctuation marks.
/// Korean words (eojeol), Latin words with digits, apostrophes, and
/// periods ("can't", "e.g", "3.14"), and emoji sequences each stay in
/// one segment.
///
/// ```
/// use sigilbuzz_text_layout::word_breaks;
///
/// // The UAX #29 example: "I live in Chicago." in Korean.
/// let text = "\u{B098}\u{B294} Chicago\u{C5D0} \u{C0B0}\u{B2E4}.";
/// let mut start = 0;
/// let segments: Vec<&str> = word_breaks(text)
///     .map(|end| {
///         let segment = &text[start..end];
///         start = end;
///         segment
///     })
///     .collect();
/// assert_eq!(
///     segments,
///     ["\u{B098}\u{B294}", " ", "Chicago\u{C5D0}", " ", "\u{C0B0}\u{B2E4}", "."]
/// );
/// ```
#[must_use]
pub fn word_breaks(text: &str) -> WordBreakIter<'_> {
    WordBreakIter {
        text,
        chars: text.char_indices(),
        prev_char: None,
        prev: None,
        before: None,
        odd_regional_indicators: false,
        finished: false,
    }
}

/// Iterator over the UAX #29 word boundaries of a `&str`, returned by
/// [`word_breaks`].
///
/// Each item is the byte offset of a boundary. The start of the text
/// is not reported, and the end is, for non-empty text.
#[derive(Debug, Clone)]
pub struct WordBreakIter<'a> {
    text: &'a str,
    chars: CharIndices<'a>,
    /// The class of the character before the current position, for
    /// WB3 through WB3d. `None` at the start.
    prev_char: Option<WordClass>,
    /// The previous unit after WB4.
    prev: Option<WordClass>,
    /// The unit before `prev`.
    before: Option<WordClass>,
    /// `prev` ends a run of an odd number of regional indicators.
    odd_regional_indicators: bool,
    /// The end-of-text boundary has been emitted.
    finished: bool,
}

impl WordBreakIter<'_> {
    /// The class of the first character at or after byte `from` that
    /// WB4 does not ignore. `None` at the end of the text.
    fn class_after(&self, from: usize) -> Option<WordClass> {
        self.text
            .get(from..)?
            .chars()
            .map(word_class)
            .find(|class| !class.ignored())
    }

    /// Decides whether there is a boundary before `c`, the character at
    /// `pos`, then moves past it.
    fn step(&mut self, pos: usize, c: char) -> bool {
        let cur = word_class(c);
        let prev_char = self.prev_char.replace(cur);
        let Some(prev_char) = prev_char else {
            // WB1: the start of the text is not reported.
            self.push(cur);
            return false;
        };
        // WB3: do not break within CR LF.
        if prev_char == WordClass::Cr && cur == WordClass::Lf {
            self.push(cur);
            return false;
        }
        // WB3a, WB3b: otherwise break before and after newlines.
        if prev_char.newline() || cur.newline() {
            self.push(cur);
            return true;
        }
        // WB3c: do not break within emoji ZWJ sequences.
        // WB3d: keep horizontal whitespace together.
        let joined = (prev_char == WordClass::Zwj && is_extended_pictographic(c))
            || (prev_char == WordClass::WSegSpace && cur == WordClass::WSegSpace);
        // WB4: Extend, Format, and ZWJ join the character before them.
        // After a newline WB3a has already broken, so they only reach
        // here after another character.
        if cur.ignored() {
            return false;
        }
        let boundary = !joined && self.boundary(cur, pos + c.len_utf8());
        self.push(cur);
        boundary
    }

    /// WB5 through WB999 between the previous unit and `cur`. `end` is
    /// the byte offset after `cur`, where the look-ahead starts.
    fn boundary(&self, cur: WordClass, end: usize) -> bool {
        use WordClass as W;
        let Some(prev) = self.prev else {
            return true;
        };
        let before = self.before;
        let joined =
            // WB5: do not break between letters.
            (prev.letter() && cur.letter())
            // WB6, WB7: do not break letters across certain punctuation.
            || (prev.letter()
                && (cur == W::MidLetter || cur.mid_num_let())
                && self.class_after(end).is_some_and(WordClass::letter))
            || ((prev == W::MidLetter || prev.mid_num_let())
                && before.is_some_and(WordClass::letter)
                && cur.letter())
            // WB7a, WB7b, WB7c: Hebrew letters and quotation marks.
            || (prev == W::HebrewLetter && cur == W::SingleQuote)
            || (prev == W::HebrewLetter
                && cur == W::DoubleQuote
                && self.class_after(end) == Some(W::HebrewLetter))
            || (before == Some(W::HebrewLetter) && prev == W::DoubleQuote && cur == W::HebrewLetter)
            // WB8, WB9, WB10: do not break within digits, or digits
            // next to letters.
            || ((prev == W::Numeric || prev.letter()) && cur == W::Numeric)
            || (prev == W::Numeric && cur.letter())
            // WB11, WB12: do not break within numbers such as 3.2.
            || (before == Some(W::Numeric)
                && (prev == W::MidNum || prev.mid_num_let())
                && cur == W::Numeric)
            || (prev == W::Numeric
                && (cur == W::MidNum || cur.mid_num_let())
                && self.class_after(end) == Some(W::Numeric))
            // WB13: do not break between Katakana.
            || (prev == W::Katakana && cur == W::Katakana)
            // WB13a, WB13b: do not break from extenders.
            || ((prev.letter() || matches!(prev, W::Numeric | W::Katakana | W::ExtendNumLet))
                && cur == W::ExtendNumLet)
            || (prev == W::ExtendNumLet
                && (cur.letter() || matches!(cur, W::Numeric | W::Katakana)))
            // WB15, WB16: do not break within emoji flag sequences.
            || (prev == W::RegionalIndicator
                && cur == W::RegionalIndicator
                && self.odd_regional_indicators);
        // WB999: otherwise break everywhere.
        !joined
    }

    /// Records `cur` as the new previous unit.
    fn push(&mut self, cur: WordClass) {
        self.odd_regional_indicators = cur == WordClass::RegionalIndicator
            && !(self.prev == Some(WordClass::RegionalIndicator) && self.odd_regional_indicators);
        self.before = self.prev;
        self.prev = Some(cur);
    }
}

impl Iterator for WordBreakIter<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((pos, c)) = self.chars.next() {
            if self.step(pos, c) {
                return Some(pos);
            }
        }
        if self.finished || self.text.is_empty() {
            return None;
        }
        // WB2: break at the end of the text.
        self.finished = true;
        Some(self.text.len())
    }
}

impl core::iter::FusedIterator for WordBreakIter<'_> {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn boundaries(text: &str) -> Vec<usize> {
        word_breaks(text).collect()
    }

    fn segments(text: &str) -> Vec<&str> {
        let mut start = 0;
        word_breaks(text)
            .map(|end| {
                let segment = &text[start..end];
                start = end;
                segment
            })
            .collect()
    }

    #[test]
    fn empty_string_no_boundaries() {
        assert!(boundaries("").is_empty());
    }

    #[test]
    fn space_separates_two_words() {
        assert_eq!(boundaries("hi there"), vec![2, 3, 8]);
    }

    #[test]
    fn cjk_each_char_is_own_word() {
        assert_eq!(boundaries("\u{4E16}\u{754C}"), vec![3, 6]);
    }

    #[test]
    fn mixed_latin_cjk_breaks_at_transition() {
        assert_eq!(boundaries("Hi\u{4E16}\u{754C}"), vec![2, 5, 8]);
    }

    #[test]
    fn letters_and_digits_form_one_word() {
        // WB9 and WB10.
        assert_eq!(segments("abc123 4x4"), ["abc123", " ", "4x4"]);
    }

    #[test]
    fn punctuation_inside_words_and_numbers() {
        assert_eq!(
            segments("can't e.g. 3,456.78"),
            ["can't", " ", "e.g", ".", " ", "3,456.78"]
        );
    }

    #[test]
    fn spaces_stay_together() {
        assert_eq!(segments("a   b"), ["a", "   ", "b"]);
    }

    #[test]
    fn newlines_are_segments_of_their_own() {
        assert_eq!(segments("a\r\n\nb"), ["a", "\r\n", "\n", "b"]);
    }

    #[test]
    fn korean_words_are_units() {
        // "한국어를 공부해요." ("I study Korean.")
        let text = "\u{D55C}\u{AD6D}\u{C5B4}\u{B97C} \u{ACF5}\u{BD80}\u{D574}\u{C694}.";
        assert_eq!(
            segments(text),
            [
                "\u{D55C}\u{AD6D}\u{C5B4}\u{B97C}",
                " ",
                "\u{ACF5}\u{BD80}\u{D574}\u{C694}",
                "."
            ]
        );
    }

    #[test]
    fn decomposed_korean_is_one_word() {
        // U+1112 U+1161 U+11AB is "한" spelled with conjoining jamo.
        let text = "\u{1112}\u{1161}\u{11AB}\u{AD6D} x";
        assert_eq!(
            segments(text),
            ["\u{1112}\u{1161}\u{11AB}\u{AD6D}", " ", "x"]
        );
    }

    #[test]
    fn katakana_runs_and_extenders() {
        // "カタカナ_word" stays one word (WB13, WB13a, WB13b).
        let text = "\u{30AB}\u{30BF}\u{30AB}\u{30CA}_word";
        assert_eq!(segments(text), [text]);
    }

    #[test]
    fn marks_and_emoji_sequences_stay_attached() {
        // e + COMBINING ACUTE ACCENT (WB4), a ZWJ emoji sequence (WB3c),
        // and two flags (WB15, WB16).
        assert_eq!(segments("ne\u{0301}e"), ["ne\u{0301}e"]);
        let family = "\u{1F468}\u{200D}\u{1F469}";
        assert_eq!(segments(family), [family]);
        let flags = "\u{1F1F0}\u{1F1F7}\u{1F1FA}\u{1F1F8}";
        assert_eq!(
            segments(flags),
            ["\u{1F1F0}\u{1F1F7}", "\u{1F1FA}\u{1F1F8}"]
        );
    }

    #[test]
    fn iterator_is_fused() {
        let mut iter = word_breaks("a");
        assert_eq!(iter.next(), Some(1));
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);
    }
}
