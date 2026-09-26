//! Matchers for the glyph-based and class-based formats, contextual
//! and chained. Each tries its rule set's rules in order and returns
//! the first that matches: the matched input and the rule's nested
//! lookups. Matching follows HarfBuzz's skipping iterator (see
//! [`crate::tables::layout::skip_iter`]): input glyphs with the
//! context's input walk, backtrack and lookahead with its context
//! walk.

use super::{ChainContext1, ChainContext2, Context1, Context2, SequenceLookupRecord};
use crate::tables::layout::skip_iter::{
    match_backtrack, match_input, match_lookahead, InputMatch, MatchContext, MatchGlyph,
};

impl Context1<'_> {
    /// Tries every rule in the rule set of `glyphs[i]`.
    #[must_use]
    pub fn matches(
        &self,
        glyphs: &[MatchGlyph],
        i: usize,
        cx: &MatchContext<'_>,
    ) -> Option<(InputMatch, &[SequenceLookupRecord])> {
        let first = glyphs.get(i)?.id;
        let set = self.rule_set(self.coverage.index_of(first)?)?;
        set.rules.iter().find_map(|rule| {
            let tail = &rule.input_tail;
            let m = match_input(glyphs, i, tail.len(), cx, |k, g| g == tail[k])?;
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
        let first = glyphs.get(i)?.id;
        self.coverage.index_of(first)?;
        let set = self.class_set(self.class_def.class_of(first))?;
        set.rules.iter().find_map(|rule| {
            let tail = &rule.input_classes_tail;
            let m = match_input(glyphs, i, tail.len(), cx, |k, g| {
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
        let first = glyphs.get(i)?.id;
        let set = self.rule_set(self.coverage.index_of(first)?)?;
        set.rules.iter().find_map(|rule| {
            let tail = &rule.input_tail;
            let m = match_input(glyphs, i, tail.len(), cx, |k, g| g == tail[k])?;
            let (ahead, back) = (&rule.lookahead, &rule.backtrack);
            let context = match_lookahead(glyphs, m.end, ahead.len(), cx, |k, g| g == ahead[k])
                && match_backtrack(glyphs, i, back.len(), cx, |k, g| g == back[k]);
            context.then_some((m, rule.lookups.as_slice()))
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
        let first = glyphs.get(i)?.id;
        self.coverage.index_of(first)?;
        let set = self.class_set(self.input_class.class_of(first))?;
        set.rules.iter().find_map(|rule| {
            let tail = &rule.input_classes_tail;
            let m = match_input(glyphs, i, tail.len(), cx, |k, g| {
                self.input_class.class_of(g) == tail[k]
            })?;
            let (ahead, back) = (&rule.lookahead, &rule.backtrack);
            let context = match_lookahead(glyphs, m.end, ahead.len(), cx, |k, g| {
                self.lookahead_class.class_of(g) == ahead[k]
            }) && match_backtrack(glyphs, i, back.len(), cx, |k, g| {
                self.backtrack_class.class_of(g) == back[k]
            });
            context.then_some((m, rule.lookups.as_slice()))
        })
    }
}
