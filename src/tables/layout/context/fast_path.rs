//! HarfBuzz's fast path through a rule set of more than four rules
//! (`RuleSet::apply` and `ChainRuleSet::apply` in
//! `hb-ot-layout-gsubgpos.hh`), for the glyph-based and class-based
//! formats.
//!
//! HarfBuzz reads the one or two glyphs after the cursor with the
//! context walk, compares them with the start of each rule, and runs
//! the full match only for the rules that can still match. The rule
//! that matches is the one the plain walk over the rules finds, but
//! the glyphs HarfBuzz marks unsafe to concatenate differ: a rule it
//! passes over marks the cursor through the glyph that ruled it out,
//! where the full match of a chained rule marks nothing when its input
//! fails, and once a rule matches the mark starts where the match
//! ends.
//!
//! A class-based rule set in one of the first eight subtables of a
//! lookup also keeps a digest of the classes its rules accept after
//! the cursor (`hb_ot_layout_ruleset_digest_t`, built by
//! `collect_first_input_classes`). HarfBuzz checks it as soon as it
//! has the glyph after the cursor, and when the glyph's class is not
//! in it marks the cursor through that glyph and gives up. Passing
//! over every rule marks the same glyphs, except when the glyph after
//! that one is a default ignorable: without the digest, HarfBuzz then
//! takes the plain walk over the rules, which marks nothing.

use crate::tables::layout::skip_iter::{InputMatch, MatchContext, MatchSeq, MaySkip, UnsafeRanges};

/// The fewest rules a rule set needs for HarfBuzz's fast path.
const FAST_PATH_MIN_RULES: usize = 5;

/// The start of one rule as the fast path reads it: the input values
/// after the first glyph, and the lookahead values (empty for a rule
/// that is not chained).
pub(crate) struct RuleHead<'r> {
    pub(crate) input: &'r [u16],
    pub(crate) lookahead: &'r [u16],
}

/// A rule of a glyph-based or class-based rule set.
pub(crate) trait Rule {
    /// The values the fast path compares before matching the rule.
    fn head(&self) -> RuleHead<'_>;
}

/// How the fast path turns a glyph id into the value a rule compares:
/// the glyph id itself in the glyph-based formats, its input or
/// lookahead class in the class-based ones. `digest` is true when the
/// rule set checks its digest of first input values (the class-based
/// formats, see [`MatchContext::rule_set_digests`]).
pub(crate) struct RuleValues<I, L> {
    pub(crate) input: I,
    pub(crate) lookahead: L,
    pub(crate) digest: bool,
}

/// `hb_ot_layout_ruleset_digest_t::may_have` for the input value
/// `value` of the glyph after the cursor: a rule with no input after
/// the cursor fills the digest, the others add their first input
/// value, and values are compared modulo 64.
fn digest_may_have<R: Rule>(rules: &[R], value: u16) -> bool {
    rules.iter().any(|r| {
        r.head()
            .input
            .first()
            .map_or(true, |&v| v % 64 == value % 64)
    })
}

/// Where a rule set is matched: the run, the cursor, and the lookup's
/// matching context.
pub(crate) struct Cursor<'c, 'a, S: ?Sized> {
    pub(crate) seq: &'c S,
    pub(crate) at: usize,
    pub(crate) cx: &'c MatchContext<'a>,
}

/// Tries the rules of one rule set at the cursor in order and returns
/// the first match `apply` reports, with HarfBuzz's unsafe ranges.
/// `apply` runs the full match of one rule and reports its own ranges.
pub(crate) fn match_rule_set<'r, S, R, T, K, I, L>(
    cur: &Cursor<'_, '_, S>,
    sink: &mut K,
    rules: &'r [R],
    values: &RuleValues<I, L>,
    mut apply: impl FnMut(&'r R, &mut K) -> Option<(InputMatch, T)>,
) -> Option<(InputMatch, T)>
where
    S: MatchSeq + ?Sized,
    R: Rule,
    K: UnsafeRanges,
    I: Fn(u16) -> u16,
    L: Fn(u16) -> u16,
{
    if rules.len() < FAST_PATH_MIN_RULES {
        return rules.iter().find_map(|r| apply(r, sink));
    }
    let (seq, at) = (cur.seq, cur.at);
    let walk = cur.cx.context_at(seq, at);
    let usable = |j: usize| {
        seq.glyph(j)
            .filter(|&g| walk.may_skip(g) == MaySkip::No)
            .map(|g| g.id)
    };
    let first_at = match walk.next_in(seq, at + 1, |_| Some(true)) {
        Ok(j) => j,
        Err(unsafe_to1) => {
            // Nothing after the cursor: only rules with no input or
            // lookahead after it can match. A rule before the match
            // that needs more marks the end of the run.
            let mut needs_more = false;
            let found = rules.iter().find_map(|r| {
                let h = r.head();
                if h.input.is_empty() && h.lookahead.is_empty() {
                    apply(r, sink)
                } else {
                    needs_more = true;
                    None
                }
            });
            if needs_more {
                // As below, the mark starts where a matched rule left
                // the cursor.
                let from = found.as_ref().map_or(at, |f| f.0.end);
                sink.unsafe_to_concat(from, unsafe_to1, false);
            }
            return found;
        }
    };
    // A glyph the walk might skip (a default ignorable) takes the
    // plain walk over the rules.
    let Some(first) = usable(first_at) else {
        return rules.iter().find_map(|r| apply(r, sink));
    };
    let unsafe_to1 = first_at + 1;
    let (second, unsafe_to2) = match walk.next_in(seq, unsafe_to1, |_| Some(true)) {
        Ok(j) => match usable(j) {
            Some(g) => (Some(g), j + 1),
            None => {
                // HarfBuzz checks the digest before it reads this
                // glyph. Anywhere else the digest marks what the rules
                // below would.
                if values.digest && !digest_may_have(rules, (values.input)(first)) {
                    sink.unsafe_to_concat(at, unsafe_to1, false);
                    return None;
                }
                return rules.iter().find_map(|r| apply(r, sink));
            }
        },
        Err(_) => (None, 0),
    };

    let mut unsafe_to: Option<usize> = None;
    let mut k = 0;
    while let Some(r) = rules.get(k) {
        let h = r.head();
        let len_p1 = h.input.len() + 1;
        let first_ok = match h.input.first() {
            Some(&v) => (values.input)(first) == v,
            None => h
                .lookahead
                .first()
                .map_or(true, |&v| (values.lookahead)(first) == v),
        };
        if first_ok {
            let second_ok = second.map_or(true, |g| match h.input.get(1) {
                Some(&v) => (values.input)(g) == v,
                None => h
                    .lookahead
                    .get(2 - len_p1)
                    .map_or(true, |&v| (values.lookahead)(g) == v),
            });
            if second_ok {
                if let Some(found) = apply(r, sink) {
                    // HarfBuzz marks from where the rule left the
                    // cursor: the end of its match.
                    if let Some(end) = unsafe_to {
                        sink.unsafe_to_concat(found.0.end, end, false);
                    }
                    return Some(found);
                }
            } else {
                unsafe_to = Some(unsafe_to2);
            }
        } else {
            unsafe_to.get_or_insert(unsafe_to1);
            // Rules that start with the same value fail the same way.
            if let Some(&v) = h.input.first() {
                while rules
                    .get(k + 1)
                    .is_some_and(|r2| r2.head().input.first() == Some(&v))
                {
                    k += 1;
                }
            }
        }
        k += 1;
    }
    if let Some(end) = unsafe_to {
        sink.unsafe_to_concat(at, end, false);
    }
    None
}
