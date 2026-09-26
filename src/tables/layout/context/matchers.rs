//! Matchers for the glyph-based and class-based formats, contextual
//! and chained. Each returns the matched input span and the rule's
//! nested lookups.

use super::{ChainContext1, ChainContext2, Context1, Context2, SequenceLookupRecord};
use crate::tables::layout::skip_iter::MatchFilter;

// ---------------------------------------------------------------------
// Matchers: return (input_len, &lookups) on a successful match.
// ---------------------------------------------------------------------

impl Context1<'_> {
    /// Tries every rule in the ruleset for `glyphs[i]`. Pass-through
    /// filter shorthand, equivalent to [`Context1::matches_filtered`]
    /// with `MatchFilter::none()`.
    #[must_use]
    pub fn matches(&self, glyphs: &[u16], i: usize) -> Option<(usize, &[SequenceLookupRecord])> {
        self.matches_filtered(glyphs, i, &MatchFilter::none())
    }

    /// Filter-aware match: walks the input stream via the
    /// skip-iterator semantics baked into `filter`. Returns the span
    /// (from the first input glyph to the last, inclusive) in raw
    /// glyph positions. The dispatcher uses this to advance past
    /// the whole match region, skipped glyphs included.
    #[must_use]
    pub fn matches_filtered(
        &self,
        glyphs: &[u16],
        i: usize,
        filter: &MatchFilter<'_>,
    ) -> Option<(usize, &[SequenceLookupRecord])> {
        let first = *glyphs.get(i)?;
        let cov_i = self.coverage.index_of(first)?;
        let set = self.rule_set(cov_i)?;
        'rules: for rule in &set.rules {
            let mut last = i;
            let mut cursor = i + 1;
            for &g in &rule.input_tail {
                let Some(pos) = filter.next_unskipped(glyphs, cursor) else {
                    continue 'rules;
                };
                if glyphs[pos] != g {
                    continue 'rules;
                }
                last = pos;
                cursor = pos + 1;
            }
            return Some((last - i + 1, &rule.lookups));
        }
        None
    }
}

impl Context2<'_> {
    /// Tries every class rule in the classset for `glyphs[i]`'s class.
    /// Pass-through filter shorthand.
    #[must_use]
    pub fn matches(&self, glyphs: &[u16], i: usize) -> Option<(usize, &[SequenceLookupRecord])> {
        self.matches_filtered(glyphs, i, &MatchFilter::none())
    }

    /// Filter-aware match. See [`Context1::matches_filtered`] for
    /// the input-span convention.
    #[must_use]
    pub fn matches_filtered(
        &self,
        glyphs: &[u16],
        i: usize,
        filter: &MatchFilter<'_>,
    ) -> Option<(usize, &[SequenceLookupRecord])> {
        let first = *glyphs.get(i)?;
        // Coverage gates matching. Format 2 stores the coverage for
        // every first glyph that appears in *any* class rule; a glyph
        // outside coverage cannot start a rule regardless of its class.
        self.coverage.index_of(first)?;
        let cls = self.class_def.class_of(first);
        let set = self.class_set(cls)?;
        'rules: for rule in &set.rules {
            let mut last = i;
            let mut cursor = i + 1;
            for &c in &rule.input_classes_tail {
                let Some(pos) = filter.next_unskipped(glyphs, cursor) else {
                    continue 'rules;
                };
                if self.class_def.class_of(glyphs[pos]) != c {
                    continue 'rules;
                }
                last = pos;
                cursor = pos + 1;
            }
            return Some((last - i + 1, &rule.lookups));
        }
        None
    }
}

impl ChainContext1<'_> {
    /// Tries every rule in the ruleset for `glyphs[i]`. Pass-through
    /// filter shorthand.
    #[must_use]
    pub fn matches(&self, glyphs: &[u16], i: usize) -> Option<(usize, &[SequenceLookupRecord])> {
        self.matches_filtered(glyphs, i, &MatchFilter::none())
    }

    /// Filter-aware match.
    #[must_use]
    pub fn matches_filtered(
        &self,
        glyphs: &[u16],
        i: usize,
        filter: &MatchFilter<'_>,
    ) -> Option<(usize, &[SequenceLookupRecord])> {
        let first = *glyphs.get(i)?;
        let cov_i = self.coverage.index_of(first)?;
        let set = self.rule_set(cov_i)?;
        'rules: for rule in &set.rules {
            // Backtrack: walk left from `i` via prev_unskipped, one
            // entry per backtrack step.
            let mut bt_cursor = i;
            for &g in &rule.backtrack {
                let Some(pos) = filter.prev_unskipped(glyphs, bt_cursor) else {
                    continue 'rules;
                };
                if glyphs[pos] != g {
                    continue 'rules;
                }
                bt_cursor = pos;
            }
            // Input tail.
            let mut last = i;
            let mut cursor = i + 1;
            for &g in &rule.input_tail {
                let Some(pos) = filter.next_unskipped(glyphs, cursor) else {
                    continue 'rules;
                };
                if glyphs[pos] != g {
                    continue 'rules;
                }
                last = pos;
                cursor = pos + 1;
            }
            // Lookahead: walk right from `last+1`.
            let mut la_cursor = last + 1;
            for &g in &rule.lookahead {
                let Some(pos) = filter.next_unskipped(glyphs, la_cursor) else {
                    continue 'rules;
                };
                if glyphs[pos] != g {
                    continue 'rules;
                }
                la_cursor = pos + 1;
            }
            return Some((last - i + 1, &rule.lookups));
        }
        None
    }
}

impl ChainContext2<'_> {
    /// Tries every class rule in the classset for `glyphs[i]`'s input class.
    /// Pass-through filter shorthand.
    #[must_use]
    pub fn matches(&self, glyphs: &[u16], i: usize) -> Option<(usize, &[SequenceLookupRecord])> {
        self.matches_filtered(glyphs, i, &MatchFilter::none())
    }

    /// Filter-aware match.
    #[must_use]
    pub fn matches_filtered(
        &self,
        glyphs: &[u16],
        i: usize,
        filter: &MatchFilter<'_>,
    ) -> Option<(usize, &[SequenceLookupRecord])> {
        let first = *glyphs.get(i)?;
        self.coverage.index_of(first)?;
        let cls = self.input_class.class_of(first);
        let set = self.class_set(cls)?;
        'rules: for rule in &set.rules {
            // Backtrack classes.
            let mut bt_cursor = i;
            for &c in &rule.backtrack {
                let Some(pos) = filter.prev_unskipped(glyphs, bt_cursor) else {
                    continue 'rules;
                };
                if self.backtrack_class.class_of(glyphs[pos]) != c {
                    continue 'rules;
                }
                bt_cursor = pos;
            }
            let mut last = i;
            let mut cursor = i + 1;
            for &c in &rule.input_classes_tail {
                let Some(pos) = filter.next_unskipped(glyphs, cursor) else {
                    continue 'rules;
                };
                if self.input_class.class_of(glyphs[pos]) != c {
                    continue 'rules;
                }
                last = pos;
                cursor = pos + 1;
            }
            let mut la_cursor = last + 1;
            for &c in &rule.lookahead {
                let Some(pos) = filter.next_unskipped(glyphs, la_cursor) else {
                    continue 'rules;
                };
                if self.lookahead_class.class_of(glyphs[pos]) != c {
                    continue 'rules;
                }
                la_cursor = pos + 1;
            }
            return Some((last - i + 1, &rule.lookups));
        }
        None
    }
}
