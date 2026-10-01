//! Matchers for the glyph-based and class-based formats, contextual
//! and chained. Each tries its rule set's rules in order and returns
//! the first that matches: the matched input and the rule's nested
//! lookups. Matching follows HarfBuzz's skipping iterator (see
//! [`crate::tables::layout::skip_iter`]): input glyphs with the
//! context's input walk, backtrack and lookahead with its context
//! walk.
//!
//! Every rule tried reports the glyphs HarfBuzz marks unsafe to break
//! or concatenate (`context_apply_lookup` and
//! `chain_context_apply_lookup` in `hb-ot-layout-gsubgpos.hh`) to an
//! [`UnsafeRanges`]. A rule set of more than four rules goes through
//! HarfBuzz's fast path (see [`super::fast_path`]).

use super::fast_path::{match_rule_set, Cursor, Rule, RuleHead, RuleValues};
use super::{
    ChainClassRule2, ChainContext1, ChainContext2, ChainRule1, ClassRule2, Context1, Context2,
    Rule1, SequenceLookupRecord,
};
use crate::tables::layout::skip_iter::{
    match_backtrack_in, match_input_in, match_lookahead_in, InputMatch, MatchContext, MatchGlyph,
    MatchSeq, UnsafeRanges,
};

/// HarfBuzz's `context_apply_lookup` without the nested lookups: the
/// `tail` input glyphs after `seq[i]`, tested by `input`. A match
/// marks the input unsafe to break. A failure marks where the walk
/// gave up unsafe to concatenate.
pub(crate) fn context_rule<S: MatchSeq + ?Sized>(
    seq: &S,
    i: usize,
    cx: &MatchContext<'_>,
    sink: &mut impl UnsafeRanges,
    tail: usize,
    input: impl FnMut(usize, u16) -> bool,
) -> Option<InputMatch> {
    match match_input_in(seq, i, tail, cx, input) {
        Ok(m) => {
            sink.unsafe_to_break(i, m.end, false);
            Some(m)
        }
        Err(end) => {
            sink.unsafe_to_concat(i, end.unwrap_or(0), false);
            None
        }
    }
}

/// The three glyph tests of one chained rule: input tail, lookahead,
/// and backtrack, each a length and a matcher.
pub(crate) struct ChainTests<I, L, B> {
    /// Input glyphs after the first, and their test.
    pub(crate) input: (usize, I),
    /// Lookahead glyphs and their test.
    pub(crate) lookahead: (usize, L),
    /// Backtrack glyphs (nearest first) and their test.
    pub(crate) backtrack: (usize, B),
}

/// HarfBuzz's `chain_context_apply_lookup` without the nested
/// lookups: input, then lookahead, then backtrack around `seq[i]`. A
/// match marks backtrack through lookahead unsafe to break. A failure
/// marks what was examined unsafe to concatenate.
pub(crate) fn chain_rule<S, I, L, B>(
    seq: &S,
    i: usize,
    cx: &MatchContext<'_>,
    sink: &mut impl UnsafeRanges,
    tests: ChainTests<I, L, B>,
) -> Option<InputMatch>
where
    S: MatchSeq + ?Sized,
    I: FnMut(usize, u16) -> bool,
    L: FnMut(usize, u16) -> bool,
    B: FnMut(usize, u16) -> bool,
{
    let ChainTests {
        input,
        lookahead,
        backtrack,
    } = tests;
    let Ok(m) = match_input_in(seq, i, input.0, cx, input.1) else {
        // `end_index` is still the cursor: nothing to mark.
        sink.unsafe_to_concat(i, i, false);
        return None;
    };
    let end = match match_lookahead_in(seq, i, m.end, lookahead.0, cx, lookahead.1) {
        Ok(end) => end,
        Err(unsafe_to) => {
            sink.unsafe_to_concat(i, unsafe_to, false);
            return None;
        }
    };
    match match_backtrack_in(seq, i, backtrack.0, cx, backtrack.1) {
        Ok(start) => {
            sink.unsafe_to_break(start, end, true);
            Some(m)
        }
        Err(unsafe_from) => {
            sink.unsafe_to_concat(unsafe_from, end, true);
            None
        }
    }
}

impl Context1<'_> {
    /// Tries every rule in the rule set of `glyphs[i]`.
    #[must_use]
    pub fn matches(
        &self,
        glyphs: &[MatchGlyph],
        i: usize,
        cx: &MatchContext<'_>,
    ) -> Option<(InputMatch, &[SequenceLookupRecord])> {
        self.matches_in(glyphs, i, cx, &mut ())
    }

    /// [`Self::matches`] over any [`MatchSeq`], reporting unsafe
    /// ranges to `sink`.
    pub(crate) fn matches_in<S: MatchSeq + ?Sized>(
        &self,
        seq: &S,
        i: usize,
        cx: &MatchContext<'_>,
        sink: &mut impl UnsafeRanges,
    ) -> Option<(InputMatch, &[SequenceLookupRecord])> {
        let first = seq.glyph(i)?.id;
        let set = self.rule_set(self.coverage.index_of(first)?)?;
        let cur = Cursor { seq, at: i, cx };
        let values = RuleValues {
            input: |g| g,
            lookahead: |g| g,
            digest: false,
        };
        match_rule_set(&cur, sink, &set.rules, &values, |rule, sink| {
            let tail = &rule.input_tail;
            let m = context_rule(seq, i, cx, sink, tail.len(), |k, g| g == tail[k])?;
            Some((m, rule.lookups.as_slice()))
        })
    }
}

impl Context2<'_> {
    /// Tries every class rule in the class set of `glyphs[i]`'s class.
    /// The coverage gates the first glyph: a glyph outside it starts
    /// no rule, whatever its class.
    #[must_use]
    pub fn matches(
        &self,
        glyphs: &[MatchGlyph],
        i: usize,
        cx: &MatchContext<'_>,
    ) -> Option<(InputMatch, &[SequenceLookupRecord])> {
        self.matches_in(glyphs, i, cx, &mut ())
    }

    /// [`Self::matches`] over any [`MatchSeq`], reporting unsafe
    /// ranges to `sink`.
    pub(crate) fn matches_in<S: MatchSeq + ?Sized>(
        &self,
        seq: &S,
        i: usize,
        cx: &MatchContext<'_>,
        sink: &mut impl UnsafeRanges,
    ) -> Option<(InputMatch, &[SequenceLookupRecord])> {
        let first = seq.glyph(i)?.id;
        self.coverage.index_of(first)?;
        let set = self.class_set(self.class_def.class_of(first))?;
        let cur = Cursor { seq, at: i, cx };
        let values = RuleValues {
            input: |g| self.class_def.class_of(g),
            lookahead: |g| self.class_def.class_of(g),
            digest: cx.rule_set_digests(),
        };
        match_rule_set(&cur, sink, &set.rules, &values, |rule, sink| {
            let tail = &rule.input_classes_tail;
            let m = context_rule(seq, i, cx, sink, tail.len(), |k, g| {
                self.class_def.class_of(g) == tail[k]
            })?;
            Some((m, rule.lookups.as_slice()))
        })
    }
}

impl ChainContext1<'_> {
    /// Tries every rule in the rule set of `glyphs[i]`: input first,
    /// then lookahead, then backtrack, as HarfBuzz does.
    #[must_use]
    pub fn matches(
        &self,
        glyphs: &[MatchGlyph],
        i: usize,
        cx: &MatchContext<'_>,
    ) -> Option<(InputMatch, &[SequenceLookupRecord])> {
        self.matches_in(glyphs, i, cx, &mut ())
    }

    /// [`Self::matches`] over any [`MatchSeq`], reporting unsafe
    /// ranges to `sink`.
    pub(crate) fn matches_in<S: MatchSeq + ?Sized>(
        &self,
        seq: &S,
        i: usize,
        cx: &MatchContext<'_>,
        sink: &mut impl UnsafeRanges,
    ) -> Option<(InputMatch, &[SequenceLookupRecord])> {
        let first = seq.glyph(i)?.id;
        let set = self.rule_set(self.coverage.index_of(first)?)?;
        let cur = Cursor { seq, at: i, cx };
        let values = RuleValues {
            input: |g| g,
            lookahead: |g| g,
            digest: false,
        };
        match_rule_set(&cur, sink, &set.rules, &values, |rule, sink| {
            let (tail, ahead, back) = (&rule.input_tail, &rule.lookahead, &rule.backtrack);
            let tests = ChainTests {
                input: (tail.len(), |k: usize, g: u16| g == tail[k]),
                lookahead: (ahead.len(), |k: usize, g: u16| g == ahead[k]),
                backtrack: (back.len(), |k: usize, g: u16| g == back[k]),
            };
            let m = chain_rule(seq, i, cx, sink, tests)?;
            Some((m, rule.lookups.as_slice()))
        })
    }
}

impl ChainContext2<'_> {
    /// Tries every class rule in the class set of `glyphs[i]`'s input
    /// class.
    #[must_use]
    pub fn matches(
        &self,
        glyphs: &[MatchGlyph],
        i: usize,
        cx: &MatchContext<'_>,
    ) -> Option<(InputMatch, &[SequenceLookupRecord])> {
        self.matches_in(glyphs, i, cx, &mut ())
    }

    /// [`Self::matches`] over any [`MatchSeq`], reporting unsafe
    /// ranges to `sink`.
    pub(crate) fn matches_in<S: MatchSeq + ?Sized>(
        &self,
        seq: &S,
        i: usize,
        cx: &MatchContext<'_>,
        sink: &mut impl UnsafeRanges,
    ) -> Option<(InputMatch, &[SequenceLookupRecord])> {
        let first = seq.glyph(i)?.id;
        self.coverage.index_of(first)?;
        let set = self.class_set(self.input_class.class_of(first))?;
        let cur = Cursor { seq, at: i, cx };
        let values = RuleValues {
            input: |g| self.input_class.class_of(g),
            lookahead: |g| self.lookahead_class.class_of(g),
            digest: cx.rule_set_digests(),
        };
        match_rule_set(&cur, sink, &set.rules, &values, |rule, sink| {
            let (tail, ahead, back) = (&rule.input_classes_tail, &rule.lookahead, &rule.backtrack);
            let tests = ChainTests {
                input: (tail.len(), |k: usize, g: u16| {
                    self.input_class.class_of(g) == tail[k]
                }),
                lookahead: (ahead.len(), |k: usize, g: u16| {
                    self.lookahead_class.class_of(g) == ahead[k]
                }),
                backtrack: (back.len(), |k: usize, g: u16| {
                    self.backtrack_class.class_of(g) == back[k]
                }),
            };
            let m = chain_rule(seq, i, cx, sink, tests)?;
            Some((m, rule.lookups.as_slice()))
        })
    }
}

impl Rule for Rule1 {
    fn head(&self) -> RuleHead<'_> {
        RuleHead {
            input: &self.input_tail,
            lookahead: &[],
        }
    }
}

impl Rule for ClassRule2 {
    fn head(&self) -> RuleHead<'_> {
        RuleHead {
            input: &self.input_classes_tail,
            lookahead: &[],
        }
    }
}

impl Rule for ChainRule1 {
    fn head(&self) -> RuleHead<'_> {
        RuleHead {
            input: &self.input_tail,
            lookahead: &self.lookahead,
        }
    }
}

impl Rule for ChainClassRule2 {
    fn head(&self) -> RuleHead<'_> {
        RuleHead {
            input: &self.input_classes_tail,
            lookahead: &self.lookahead,
        }
    }
}
