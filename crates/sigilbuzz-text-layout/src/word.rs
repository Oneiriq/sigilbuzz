//! Simplified UAX #29 word-segmentation iterator.
//!
//! This is *not* a full implementation of UAX 29. It produces word
//! boundaries good enough to drive cursor-by-word movement and
//! double-click selection in the common Latin / CJK cases:
//!
//! - The boundary between alphabetic and non-alphabetic codepoints is
//!   always a word break.
//! - Each ideographic codepoint is its own word.
//! - Whitespace runs collapse into a single boundary.
//!
//! Brahmic syllable clustering, Korean Jamo handling, and the WB6 /
//! WB7 numeric-with-mid-letter rules are deferred.

use crate::class::{line_break_class, LineBreakClass};

/// Returns an iterator over byte offsets at which word boundaries
/// occur in `text`. The iterator emits the offset *after* each
/// boundary character; consumers can treat the slices between
/// successive offsets as words.
#[must_use]
pub fn word_breaks(text: &str) -> WordBreakIter<'_> {
    WordBreakIter {
        text,
        pos: 0,
        prev_kind: None,
        finished: false,
    }
}

/// Iterator returned by [`word_breaks`].
pub struct WordBreakIter<'a> {
    text: &'a str,
    pos: usize,
    prev_kind: Option<WordKind>,
    finished: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordKind {
    Alpha,
    Numeric,
    Ideograph,
    Whitespace,
    Other,
}

fn classify(c: char) -> WordKind {
    match line_break_class(c) {
        LineBreakClass::AL | LineBreakClass::CM => WordKind::Alpha,
        LineBreakClass::NU => WordKind::Numeric,
        LineBreakClass::ID | LineBreakClass::EB | LineBreakClass::EM => WordKind::Ideograph,
        LineBreakClass::SP
        | LineBreakClass::BA
        | LineBreakClass::ZW
        | LineBreakClass::BK
        | LineBreakClass::CR
        | LineBreakClass::LF
        | LineBreakClass::NL => WordKind::Whitespace,
        _ => WordKind::Other,
    }
}

impl<'a> Iterator for WordBreakIter<'a> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let bytes = self.text.as_bytes();
        loop {
            if self.pos >= bytes.len() {
                self.finished = true;
                if self.prev_kind.is_some() {
                    return Some(self.text.len());
                }
                return None;
            }
            let rest = &self.text[self.pos..];
            let ch = rest.chars().next().expect("non-empty rest");
            let len = ch.len_utf8();
            let kind = classify(ch);
            let next_pos = self.pos + len;

            let boundary = match self.prev_kind {
                None => false,
                Some(prev) => prev != kind || kind == WordKind::Ideograph,
            };

            self.prev_kind = Some(kind);
            self.pos = next_pos;
            if boundary {
                return Some(self.pos - len);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn boundaries(text: &str) -> Vec<usize> {
        word_breaks(text).collect()
    }

    #[test]
    fn empty_string_no_boundaries() {
        assert!(boundaries("").is_empty());
    }

    #[test]
    fn space_separates_two_words() {
        let bounds = boundaries("hi there");
        // boundary at "hi"|" " (offset 2), " "|"there" (offset 3),
        // and end-of-text.
        assert_eq!(bounds, vec![2, 3, 8]);
    }

    #[test]
    fn cjk_each_char_is_own_word() {
        let bounds = boundaries("世界");
        // every ideograph forces a boundary; both transitions plus
        // end-of-text.
        assert_eq!(bounds.len(), 2);
    }

    #[test]
    fn mixed_latin_cjk_breaks_at_transition() {
        let bounds = boundaries("Hi世界");
        assert!(!bounds.is_empty());
        // First boundary lands at byte offset 2 ("Hi" → "世").
        assert_eq!(bounds[0], 2);
    }

    #[test]
    fn digits_are_their_own_word() {
        let bounds = boundaries("abc123");
        assert_eq!(bounds[0], 3);
    }
}
