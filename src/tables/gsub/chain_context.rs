//! GSUB lookup type 6: Chained Context Substitution.
//!
//! Formats 1 (glyph-based) and 2 (class-based) are parsed via the
//! shared `tables::layout::context` helpers; see
//! [`ChainContextAny`] for the format-dispatching entry point every
//! downstream caller should use. The [`ChainContext`] struct in this
//! file implements format 3 only.
//!
//! Chained context is the mechanism behind `calt`, `clig`, and the
//! positional-form GSUB features (`init`, `medi`, `fina`, `isol`)
//! that Arabic, Indic, and cursive-Latin fonts lean on heavily. A
//! single chained-context rule says: "when this input sequence
//! appears, preceded by these glyphs and followed by these others,
//! run these nested lookups at these positions."
//!
//! # Format 3 layout
//!
//! ```text
//!   u16      substFormat = 3
//!   u16      backtrackGlyphCount
//!   Offset16 backtrackCoverageOffsets[backtrackGlyphCount]
//!   u16      inputGlyphCount
//!   Offset16 inputCoverageOffsets[inputGlyphCount]
//!   u16      lookaheadGlyphCount
//!   Offset16 lookaheadCoverageOffsets[lookaheadGlyphCount]
//!   u16      substCount
//!   SubstLookupRecord records[substCount]:
//!     u16 sequenceIndex     (position within the input where the
//!                            nested lookup fires, 0-indexed)
//!     u16 lookupListIndex
//! ```
//!
//! Backtrack coverages are listed in *reverse* match order per the
//! spec: `backtrackCoverageOffsets[0]` is the glyph immediately
//! before the input, `[1]` is the one before that, and so on.
//! sigilbuzz preserves that ordering internally and reverses on
//! iteration so callers do not have to think about it.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::layout::skip_iter::{
    match_backtrack, match_input, match_lookahead, InputMatch, MatchContext, MatchGlyph,
};
use crate::tables::layout::Coverage;
use crate::tables::parse::Reader;

/// A parsed chained-context-substitution subtable (format 3).
#[derive(Debug, Clone)]
pub struct ChainContext<'a> {
    /// Coverage tables the *backtrack* context must match. Index 0
    /// is the glyph immediately before the input.
    backtrack: Vec<Coverage<'a>>,
    /// Coverage tables the *input* must match in order.
    input: Vec<Coverage<'a>>,
    /// Coverage tables the *lookahead* must match in order. Index
    /// 0 is the glyph immediately after the input.
    lookahead: Vec<Coverage<'a>>,
    /// Nested lookups to invoke when the context matches.
    substitutions: Vec<SubstLookupRecord>,
}

/// A single `(sequenceIndex, lookupListIndex)` record. `sequenceIndex`
/// is 0-based into the matched *input* run; it tells the caller
/// which glyph the nested lookup should fire at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubstLookupRecord {
    /// Position within the input window where the nested lookup
    /// applies.
    pub sequence_index: u16,
    /// Index into the enclosing GSUB `LookupList`.
    pub lookup_list_index: u16,
}

impl<'a> ChainContext<'a> {
    /// Parses a format-3 chained-context subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 3 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported chainContextSubst format (only 3 is implemented)",
            });
        }

        let backtrack = parse_coverage_array(data, &mut r)?;
        // An empty input is accepted: the spec does not forbid it. The
        // rule then matches nothing, as in HarfBuzz, which reads its
        // first input coverage from a null offset.
        let input = parse_coverage_array(data, &mut r)?;
        let lookahead = parse_coverage_array(data, &mut r)?;

        let subst_count = r.read_u16()?;
        let mut substitutions = Vec::with_capacity(usize::from(subst_count).min(r.remaining() / 4));
        for _ in 0..subst_count {
            let sequence_index = r.read_u16()?;
            let lookup_list_index = r.read_u16()?;
            substitutions.push(SubstLookupRecord {
                sequence_index,
                lookup_list_index,
            });
        }

        Ok(Self {
            backtrack,
            input,
            lookahead,
            substitutions,
        })
    }

    /// Context window required for a match: the number of
    /// backtrack + input + lookahead glyphs.
    #[must_use]
    pub fn context_len(&self) -> (usize, usize, usize) {
        (self.backtrack.len(), self.input.len(), self.lookahead.len())
    }

    /// Coverage of the first input glyph, exposed for the run-level
    /// "would_apply" precheck. `None` only for the rare empty-input
    /// chain rule, which matches nothing.
    #[must_use]
    pub fn input_first_coverage(&self) -> Option<&Coverage<'a>> {
        self.input.first()
    }

    /// The nested-lookup records that fire on a successful match.
    #[must_use]
    pub fn substitutions(&self) -> &[SubstLookupRecord] {
        &self.substitutions
    }

    /// Matches the rule around input position `i`: input first (the
    /// cheap test that fails at most cursors), then lookahead, then
    /// backtrack, each with HarfBuzz's skipping iterator (see
    /// [`crate::tables::layout::skip_iter`]). `glyphs[i]` must be in
    /// the first input coverage. `None` for an empty input sequence,
    /// which matches nothing (HarfBuzz reads its first coverage from a
    /// null offset).
    #[must_use]
    pub fn matches(
        &self,
        glyphs: &[MatchGlyph],
        i: usize,
        cx: &MatchContext<'_>,
    ) -> Option<InputMatch> {
        let (first, rest) = self.input.split_first()?;
        if !first.contains(glyphs.get(i)?.id) {
            return None;
        }
        let m = match_input(glyphs, i, rest.len(), cx, |k, g| rest[k].contains(g))?;
        let (ahead, back) = (&self.lookahead, &self.backtrack);
        let context = match_lookahead(glyphs, m.end, ahead.len(), cx, |k, g| ahead[k].contains(g))
            && match_backtrack(glyphs, i, back.len(), cx, |k, g| back[k].contains(g));
        context.then_some(m)
    }
}

/// A format-dispatching wrapper over every chained-context
/// subtable. The shape driver calls [`ChainContextAny::parse`] and
/// then matches on the variant. Format 1 and 2 reuse the shared
/// layout-level parsers.
#[derive(Debug, Clone)]
pub enum ChainContextAny<'a> {
    /// Format 1: glyph-based.
    Format1(crate::tables::layout::ChainContext1<'a>),
    /// Format 2: class-based.
    Format2(crate::tables::layout::ChainContext2<'a>),
    /// Format 3: coverage-based (the original sigilbuzz
    /// implementation, kept for BC).
    Format3(ChainContext<'a>),
}

impl<'a> ChainContextAny<'a> {
    /// Parses a chained-context subtable of any format.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        match format {
            1 => Ok(Self::Format1(crate::tables::layout::ChainContext1::parse(
                data,
            )?)),
            2 => Ok(Self::Format2(crate::tables::layout::ChainContext2::parse(
                data,
            )?)),
            3 => Ok(Self::Format3(ChainContext::parse(data)?)),
            _ => Err(Error::Malformed {
                offset: 0,
                context: "unsupported chainContextSubst format",
            }),
        }
    }
}

fn parse_coverage_array<'a>(data: &'a [u8], r: &mut Reader<'_>) -> Result<Vec<Coverage<'a>>> {
    let count = r.read_u16()? as usize;
    let mut out = Vec::with_capacity(count.min(r.remaining() / 2));
    for _ in 0..count {
        let off = r.read_u16()? as usize;
        let bytes = data.get(off..).ok_or(Error::Malformed {
            offset: off,
            context: "chainContextSubst coverage offset past end",
        })?;
        out.push(Coverage::parse(bytes)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plain matching of `ctx` at `ids[i]`.
    fn matches(ctx: &ChainContext<'_>, ids: &[u16], i: usize) -> bool {
        let glyphs: Vec<MatchGlyph> = ids.iter().map(|&id| MatchGlyph::new(id)).collect();
        ctx.matches(&glyphs, i, &MatchContext::plain()).is_some()
    }

    fn build_coverage_format1(glyphs: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
        for g in glyphs {
            out.extend_from_slice(&g.to_be_bytes());
        }
        out
    }

    /// Builds a format-3 subtable with explicit coverage lists for
    /// each window. `subst_records` is a slice of `(seq_idx, lookup_idx)`.
    fn build_format3(
        backtrack: &[&[u16]],
        input: &[&[u16]],
        lookahead: &[&[u16]],
        subst_records: &[(u16, u16)],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&3u16.to_be_bytes()); // format

        // Backtrack.
        out.extend_from_slice(&(backtrack.len() as u16).to_be_bytes());
        let bt_slots_start = out.len();
        for _ in 0..backtrack.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        // Input.
        out.extend_from_slice(&(input.len() as u16).to_be_bytes());
        let in_slots_start = out.len();
        for _ in 0..input.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        // Lookahead.
        out.extend_from_slice(&(lookahead.len() as u16).to_be_bytes());
        let la_slots_start = out.len();
        for _ in 0..lookahead.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        // Subst records.
        out.extend_from_slice(&(subst_records.len() as u16).to_be_bytes());
        for (seq, lk) in subst_records {
            out.extend_from_slice(&seq.to_be_bytes());
            out.extend_from_slice(&lk.to_be_bytes());
        }

        // Append each coverage, patching its slot.
        let mut patch = |start: usize, covs: &[&[u16]]| {
            for (i, gs) in covs.iter().enumerate() {
                let cov_start = out.len();
                out.extend_from_slice(&build_coverage_format1(gs));
                let slot = start + i * 2;
                out[slot..slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
            }
        };
        patch(bt_slots_start, backtrack);
        patch(in_slots_start, input);
        patch(la_slots_start, lookahead);
        out
    }

    #[test]
    fn parses_and_reports_context_window() {
        let bytes = build_format3(
            &[&[1], &[2]], // 2 backtrack coverages
            &[&[3], &[4]], // 2 input coverages
            &[&[5]],       // 1 lookahead coverage
            &[(0, 7), (1, 8)],
        );
        let ctx = ChainContext::parse(&bytes).unwrap();
        assert_eq!(ctx.context_len(), (2, 2, 1));
        assert_eq!(
            ctx.substitutions(),
            &[
                SubstLookupRecord {
                    sequence_index: 0,
                    lookup_list_index: 7
                },
                SubstLookupRecord {
                    sequence_index: 1,
                    lookup_list_index: 8
                },
            ]
        );
    }

    #[test]
    fn matches_when_full_context_lines_up() {
        // Backtrack[0] = glyph right before input must be in {10}.
        // Backtrack[1] = two before input must be in {20}.
        // Input[0..=1] must be in ({30}, {40}).
        // Lookahead[0] must be in {50}.
        let bytes = build_format3(&[&[10], &[20]], &[&[30], &[40]], &[&[50]], &[(0, 99)]);
        let ctx = ChainContext::parse(&bytes).unwrap();

        // Glyphs: [20, 10, 30, 40, 50]. Input starts at index 2.
        assert!(matches(&ctx, &[20, 10, 30, 40, 50], 2));
    }

    #[test]
    fn rejects_backtrack_mismatch() {
        let bytes = build_format3(&[&[10]], &[&[30]], &[], &[(0, 1)]);
        let ctx = ChainContext::parse(&bytes).unwrap();
        // glyph before input is 11, not 10.
        assert!(!matches(&ctx, &[11, 30], 1));
    }

    #[test]
    fn rejects_input_mismatch() {
        let bytes = build_format3(&[], &[&[30], &[40]], &[], &[(0, 1)]);
        let ctx = ChainContext::parse(&bytes).unwrap();
        assert!(!matches(&ctx, &[30, 99], 0));
    }

    #[test]
    fn rejects_lookahead_mismatch() {
        let bytes = build_format3(&[], &[&[30]], &[&[50]], &[(0, 1)]);
        let ctx = ChainContext::parse(&bytes).unwrap();
        assert!(!matches(&ctx, &[30, 99], 0));
    }

    #[test]
    fn fails_when_backtrack_runs_off_start_of_run() {
        let bytes = build_format3(&[&[10]], &[&[30]], &[], &[(0, 1)]);
        let ctx = ChainContext::parse(&bytes).unwrap();
        // Input at position 0 with one-glyph backtrack requirement
        // cannot match: there is nothing behind position 0.
        assert!(!matches(&ctx, &[30, 40], 0));
    }

    #[test]
    fn fails_when_lookahead_runs_off_end_of_run() {
        let bytes = build_format3(&[], &[&[30]], &[&[50]], &[(0, 1)]);
        let ctx = ChainContext::parse(&bytes).unwrap();
        assert!(!matches(&ctx, &[30], 0));
    }

    #[test]
    fn empty_context_matches_anywhere() {
        // No backtrack, no lookahead, single-glyph input.
        let bytes = build_format3(&[], &[&[30]], &[], &[(0, 5)]);
        let ctx = ChainContext::parse(&bytes).unwrap();
        assert!(matches(&ctx, &[30], 0));
        assert!(matches(&ctx, &[99, 30, 88], 1));
        assert!(!matches(&ctx, &[99, 30], 0));
    }

    #[test]
    fn empty_input_with_backtrack_handles_cursor_past_the_run() {
        // An empty-input rule has no input check, so a backtrack walk
        // could start from a cursor past the run and index out of
        // bounds. The rule matches nothing, wherever the cursor is.
        let bytes = build_format3(&[&[7]], &[], &[], &[]);
        let ctx = ChainContext::parse(&bytes).unwrap();
        assert!(!matches(&ctx, &[7], 1));
        assert!(!matches(&ctx, &[7], 5));
        assert!(!matches(&ctx, &[8], 5));
        assert!(!matches(&ctx, &[], usize::MAX));
        // With an input coverage, a cursor past the run is no match.
        let bytes = build_format3(&[&[7]], &[&[7]], &[], &[]);
        let ctx = ChainContext::parse(&bytes).unwrap();
        assert!(matches(&ctx, &[7, 7], 1));
        assert!(!matches(&ctx, &[7], 5));
        assert!(!matches(&ctx, &[7], usize::MAX));
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        assert!(ChainContext::parse(&bytes).is_err());
    }
}
