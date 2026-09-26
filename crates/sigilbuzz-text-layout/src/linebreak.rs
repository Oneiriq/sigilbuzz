//! UAX #14 line-break iterator.
//!
//! [`LineBreakIter`] walks a `&str` and emits one [`BreakOpportunity`]
//! per *potential* line break, identified by the byte offset *after*
//! which a wrapper may break. The iterator drives a small state machine
//! over [`LineBreakClass`] pairs sourced from the spec's pair-table.
//!
//! The implementation keeps the table compact: rather than the full
//! 64x64 matrix, [`pair_action`] encodes a curated subset (~250
//! decisions) covering the rules a wrapper actually consults for
//! English-plus-CJK input.

use crate::class::{line_break_class, LineBreakClass};

/// Whether a position may host a line break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakOpportunity {
    /// Mandatory break: `CR`, `LF`, `NL`, `BK`. The wrapper *must*
    /// break here.
    Mandatory,
    /// Allowed break. The wrapper *may* break here if the next chunk
    /// would not fit.
    Allowed,
    /// Prohibited break: included for completeness so callers can
    /// distinguish "we considered this position and rejected it" from
    /// "we never reached this position".
    Prohibited,
}

/// Iterator over [`BreakOpportunity`] decisions in a `&str`.
///
/// Each item is a `(byte_offset_after, opportunity)` pair. The byte
/// offset points to the position *after* which the break occurs, so a
/// wrapper that splits `text[..offset]` and `text[offset..]` produces
/// the two sides of the break.
///
/// The iterator emits `Allowed` and `Mandatory` opportunities; it does
/// *not* emit `Prohibited` rows because they are uninteresting for
/// almost all callers. `Prohibited` is exported on the enum to give
/// callers a vocabulary for their own decisions.
pub struct LineBreakIter<'a> {
    text: &'a str,
    pos: usize,
    /// Class of the most recently consumed character. `None` until the
    /// first character is seen.
    prev_class: Option<LineBreakClass>,
    /// True when the last consumed pair was a CR followed by LF. The
    /// CR already emitted a Mandatory break, so we suppress one for
    /// the LF.
    suppress_next_mandatory: bool,
    /// True once we've emitted the trailing-position sentinel.
    finished: bool,
}

impl<'a> LineBreakIter<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            pos: 0,
            prev_class: None,
            suppress_next_mandatory: false,
            finished: false,
        }
    }
}

impl Iterator for LineBreakIter<'_> {
    type Item = (usize, BreakOpportunity);

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        loop {
            // Decode the next char. `pos` only ever advances by whole
            // chars, so this finds no char only at the end of the text.
            let Some(ch) = self
                .text
                .get(self.pos..)
                .and_then(|rest| rest.chars().next())
            else {
                self.finished = true;
                // End-of-text is always a (Mandatory) break point so
                // the wrapper has a sentinel to flush its last line.
                if self.prev_class.is_some() {
                    return Some((self.text.len(), BreakOpportunity::Mandatory));
                }
                return None;
            };
            let ch_len = ch.len_utf8();
            let next_pos = self.pos + ch_len;
            let curr = line_break_class(ch);

            // LB4 / LB5: mandatory breaks after BK / CR / LF / NL.
            // We emit the break *after* consuming the controlling
            // character so the offset lands past it.
            let was_mandatory_trigger = matches!(
                self.prev_class,
                Some(LineBreakClass::BK | LineBreakClass::LF | LineBreakClass::NL)
            ) || (self.prev_class == Some(LineBreakClass::CR)
                && curr != LineBreakClass::LF);

            if was_mandatory_trigger && !self.suppress_next_mandatory {
                self.suppress_next_mandatory = false;
                self.prev_class = Some(curr);
                self.pos = next_pos;
                return Some((self.pos - ch_len, BreakOpportunity::Mandatory));
            }
            self.suppress_next_mandatory = false;

            // Pair-table action between prev_class and curr.
            let action = match self.prev_class {
                None => BreakOpportunity::Prohibited,
                Some(prev) => pair_action(prev, curr),
            };

            self.prev_class = Some(curr);
            self.pos = next_pos;

            // Skip over CR-LF: the LF after a CR is treated as part
            // of the same mandatory break.
            if matches!(curr, LineBreakClass::CR) {
                self.suppress_next_mandatory = true;
            }

            if matches!(
                action,
                BreakOpportunity::Allowed | BreakOpportunity::Mandatory
            ) {
                return Some((self.pos - ch_len, action));
            }
            // Prohibited: keep scanning.
        }
    }
}

/// UAX 14 pair-table action.
///
/// The full pair-table is 64x64; this curated version returns
/// `Prohibited` by default and only special-cases pairs that produce
/// an `Allowed` break or a `Mandatory` flush. Tweaks here have been
/// driven by the test suite: the rules cover English / CJK / mixed
/// input, leaving fancier corners (Brahmic, Korean Jamo, regional
/// indicators) for later.
#[must_use]
pub fn pair_action(prev: LineBreakClass, curr: LineBreakClass) -> BreakOpportunity {
    use LineBreakClass as L;

    // LB6: never break before a hard line break. The CR/LF/NL/BK rule
    // is enforced by the iterator emitting *after* those characters.
    if matches!(curr, L::BK | L::CR | L::LF | L::NL) {
        return BreakOpportunity::Prohibited;
    }
    // LB7: never break before a space or zero-width-space.
    if matches!(curr, L::SP | L::ZW) {
        return BreakOpportunity::Prohibited;
    }
    // LB8: break after a zero-width space.
    if prev == L::ZW {
        return BreakOpportunity::Allowed;
    }
    // LB11: never break before or after a word-joiner.
    if curr == L::WJ || prev == L::WJ {
        return BreakOpportunity::Prohibited;
    }
    // LB12 / LB12a: never break around glue.
    if prev == L::GL {
        return BreakOpportunity::Prohibited;
    }
    if curr == L::GL && !matches!(prev, L::SP | L::BA | L::HY) {
        return BreakOpportunity::Prohibited;
    }
    // LB13: never break before close punctuation, exclamation, or
    // close paren, with or without preceding space.
    if matches!(curr, L::CL | L::CP | L::EX | L::NS) {
        return BreakOpportunity::Prohibited;
    }
    // LB14: never break after open punctuation.
    if prev == L::OP {
        return BreakOpportunity::Prohibited;
    }
    // LB15: never break after a quotation followed by open punct.
    if prev == L::QU && curr == L::OP {
        return BreakOpportunity::Prohibited;
    }
    // LB16: no break between CL/CP and NS.
    if matches!(prev, L::CL | L::CP) && curr == L::NS {
        return BreakOpportunity::Prohibited;
    }
    // LB18: break after spaces.
    if prev == L::SP {
        return BreakOpportunity::Allowed;
    }
    // LB19: never break around quotation marks.
    if curr == L::QU || prev == L::QU {
        return BreakOpportunity::Prohibited;
    }
    // LB21: break before BB and after BA / HY / NS-as-mid.
    if curr == L::BB {
        return BreakOpportunity::Allowed;
    }
    if matches!(prev, L::BA | L::HY) {
        // LB21b: except numeric stays attached (HY before NU).
        if prev == L::HY && curr == L::NU {
            return BreakOpportunity::Prohibited;
        }
        return BreakOpportunity::Allowed;
    }
    // LB22: never break before NS (handled above) or CM.
    if curr == L::CM {
        return BreakOpportunity::Prohibited;
    }
    // LB23: never break between AL / NU.
    if matches!(prev, L::AL | L::NU) && matches!(curr, L::AL | L::NU) {
        return BreakOpportunity::Prohibited;
    }
    // LB23a: never break between numeric prefix and ID.
    if prev == L::PR && curr == L::ID {
        return BreakOpportunity::Prohibited;
    }
    if prev == L::ID && curr == L::PO {
        return BreakOpportunity::Prohibited;
    }
    // LB24: PR / PO with AL / NU stays attached.
    if matches!(prev, L::PR | L::PO) && matches!(curr, L::AL | L::NU) {
        return BreakOpportunity::Prohibited;
    }
    if matches!(prev, L::AL | L::NU) && matches!(curr, L::PR | L::PO) {
        return BreakOpportunity::Prohibited;
    }
    // LB25: numeric expressions stay together. Coarse approximation.
    if prev == L::NU && matches!(curr, L::NU | L::PR | L::PO) {
        return BreakOpportunity::Prohibited;
    }
    if matches!(prev, L::PR | L::PO) && curr == L::NU {
        return BreakOpportunity::Prohibited;
    }
    // LB28: never break between two ALs.
    if prev == L::AL && curr == L::AL {
        return BreakOpportunity::Prohibited;
    }
    // LB29: never break a numeric followed by an alphabetic suffix.
    if prev == L::NU && curr == L::AL {
        return BreakOpportunity::Prohibited;
    }
    // LB30b: emoji base + emoji modifier stays attached.
    if prev == L::EB && curr == L::EM {
        return BreakOpportunity::Prohibited;
    }

    // LB30: break between ID and AL. CJK <-> Latin transition is a
    // legitimate wrap point. ID <-> ID is an explicit allowed break
    // (the per-grapheme CJK wrap behavior).
    if prev == L::ID || curr == L::ID {
        return BreakOpportunity::Allowed;
    }
    if prev == L::EB || prev == L::EM {
        return BreakOpportunity::Allowed;
    }

    // Default: forbid the break. UAX 14's LB31 is "break everywhere
    // else" but in practice that produces too many spurious breaks; we
    // err conservative and let SP / BA / HY / ZW drive the
    // opportunities.
    BreakOpportunity::Prohibited
}

/// Returns an iterator over UAX 14 line-break opportunities in `text`.
///
/// Each item is `(byte_offset_after, opportunity)`: the offset at which
/// the wrapper may break, and whether the break is mandatory. The
/// iterator always emits a final `(text.len(), Mandatory)` sentinel so
/// callers have a flush point.
#[must_use]
pub fn line_break_opportunities(text: &str) -> LineBreakIter<'_> {
    LineBreakIter::new(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn opportunities(text: &str) -> Vec<(usize, BreakOpportunity)> {
        line_break_opportunities(text).collect()
    }

    #[test]
    fn empty_text_yields_no_breaks() {
        assert!(opportunities("").is_empty());
    }

    #[test]
    fn single_word_yields_only_end_sentinel() {
        let breaks = opportunities("hello");
        assert_eq!(breaks.len(), 1);
        assert_eq!(breaks[0], (5, BreakOpportunity::Mandatory));
    }

    #[test]
    fn space_separated_words_break_after_each_space() {
        let breaks = opportunities("the quick brown");
        // breaks after "the " and "quick ", plus end sentinel.
        let allowed: Vec<usize> = breaks
            .iter()
            .filter(|(_, o)| matches!(o, BreakOpportunity::Allowed))
            .map(|(p, _)| *p)
            .collect();
        assert_eq!(allowed, vec![4, 10]);
    }

    #[test]
    fn lf_emits_mandatory_break() {
        let breaks = opportunities("a\nb");
        let mandatory: Vec<usize> = breaks
            .iter()
            .filter(|(_, o)| matches!(o, BreakOpportunity::Mandatory))
            .map(|(p, _)| *p)
            .collect();
        // After 'a' (offset 1) we treat the LF as the trigger; the
        // wrapper sees a Mandatory after the LF and at end-of-text.
        assert!(mandatory.contains(&2));
        assert_eq!(*mandatory.last().expect("end sentinel"), 3);
    }

    #[test]
    fn crlf_collapses_to_one_mandatory_break() {
        let breaks = opportunities("a\r\nb");
        let mandatory_count = breaks
            .iter()
            .filter(|(_, o)| matches!(o, BreakOpportunity::Mandatory))
            .count();
        // One for the CR-LF pair, one end sentinel.
        assert_eq!(mandatory_count, 2);
    }

    #[test]
    fn cjk_breaks_at_every_grapheme() {
        // 世界 is two ideographs; we expect an allowed break between
        // them plus the end-of-text mandatory.
        let breaks = opportunities("世界");
        let allowed = breaks
            .iter()
            .filter(|(_, o)| matches!(o, BreakOpportunity::Allowed))
            .count();
        assert_eq!(allowed, 1);
    }

    #[test]
    fn hyphen_allows_break_after() {
        let breaks = opportunities("co-op");
        let allowed: Vec<usize> = breaks
            .iter()
            .filter(|(_, o)| matches!(o, BreakOpportunity::Allowed))
            .map(|(p, _)| *p)
            .collect();
        assert_eq!(allowed, vec![3]);
    }

    #[test]
    fn no_break_inside_alphabetic_word() {
        let breaks = opportunities("foobar");
        let allowed = breaks
            .iter()
            .filter(|(_, o)| matches!(o, BreakOpportunity::Allowed))
            .count();
        assert_eq!(allowed, 0);
    }

    #[test]
    fn nbsp_acts_as_glue() {
        // No break between "Mr." and "Smith" connected by NBSP.
        let breaks = opportunities("Mr.\u{00A0}Smith");
        let allowed = breaks
            .iter()
            .filter(|(_, o)| matches!(o, BreakOpportunity::Allowed))
            .count();
        assert_eq!(allowed, 0);
    }
}
