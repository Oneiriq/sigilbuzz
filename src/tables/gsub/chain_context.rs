//! GSUB lookup type 6 — Chained Context Substitution.
//!
//! Formats 1 (glyph-based) and 2 (class-based) are parsed via the
//! shared `tables::layout::context` helpers; see
//! [`ChainContextAny`] for the format-dispatching entry point every
//! downstream caller should use. The original [`ChainContext`] struct
//! implements format 3 only and stays public for backwards
//! compatibility with the M2 shape driver.
//!
//! Chained context is the mechanism behind `calt`, `clig`, and the
//! positional-form GSUB features (`init`, `medi`, `fina`, `isol`)
//! that Arabic, Indic, and cursive-Latin fonts lean on heavily. A
//! single chained-context rule says: "when this input sequence
//! appears, preceded by these glyphs and followed by these others,
//! run these nested lookups at these positions."
//!
//! sigilbuzz implements format 3, the explicit-coverage form. It is
//! the format every modern font uses for `calt`; formats 1 (class
//! set) and 2 (pure coverage) land when a real font needs them.
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
//! spec — `backtrackCoverageOffsets[0]` is the glyph immediately
//! before the input, `[1]` is the one before that, and so on.
//! sigilbuzz preserves that ordering internally and reverses on
//! iteration so callers do not have to think about it.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::layout::skip_iter::MatchFilter;
use crate::tables::layout::Coverage;
use crate::tables::parse::Reader;

/// A parsed chained-context-substitution subtable (format 3).
#[derive(Debug, Clone)]
pub struct ChainContext<'a> {
    /// Coverage tables the *backtrack* context must match — index 0
    /// is the glyph immediately before the input.
    backtrack: Vec<Coverage<'a>>,
    /// Coverage tables the *input* must match in order.
    input: Vec<Coverage<'a>>,
    /// Coverage tables the *lookahead* must match in order — index
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
        let input = parse_coverage_array(data, &mut r)?;
        if input.is_empty() {
            // An empty input makes the lookup fire everywhere — the
            // spec does not forbid it, but the common case is one
            // or more input coverages. We accept it to match real
            // fonts, which sometimes use a single backtrack/lookahead
            // with an empty input as a zero-width assertion.
        }
        let lookahead = parse_coverage_array(data, &mut r)?;

        let subst_count = r.read_u16()?;
        let mut substitutions = Vec::with_capacity(subst_count as usize);
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

    /// The nested-lookup records that fire on a successful match.
    #[must_use]
    pub fn substitutions(&self) -> &[SubstLookupRecord] {
        &self.substitutions
    }

    /// Tests whether the run matches starting at input position
    /// `i`. Returns `true` when backtrack, input, and lookahead
    /// coverages all line up. Pass-through shorthand for
    /// [`ChainContext::matches_filtered`] with
    /// [`MatchFilter::none`].
    #[must_use]
    pub fn matches(&self, glyphs: &[u16], i: usize) -> bool {
        self.matches_filtered(glyphs, i, &MatchFilter::none())
            .is_some()
    }

    /// Filter-aware match. Returns the raw span of the input window
    /// (first to last matched glyph, inclusive) or `None` when any
    /// of the three coverage arrays fails to align. The backtrack
    /// and lookahead steps honour the skip-iterator semantics of
    /// the active `LookupFlag` — skipped glyphs (typically marks)
    /// between two matched backtrack positions do not block the
    /// match.
    #[must_use]
    pub fn matches_filtered(
        &self,
        glyphs: &[u16],
        i: usize,
        filter: &MatchFilter<'_>,
    ) -> Option<usize> {
        // Backtrack.
        let mut bt_cursor = i;
        for cov in &self.backtrack {
            let pos = filter.prev_unskipped(glyphs, bt_cursor)?;
            if !cov.contains(glyphs[pos]) {
                return None;
            }
            bt_cursor = pos;
        }
        // Input. Empty input is a zero-width assertion — fall
        // through to lookahead using the caller's anchor position.
        let (last, after) = if self.input.is_empty() {
            (i, i)
        } else {
            if !self.input[0].contains(*glyphs.get(i)?) {
                return None;
            }
            let mut last = i;
            let mut cursor = i + 1;
            for cov in &self.input[1..] {
                let pos = filter.next_unskipped(glyphs, cursor)?;
                if !cov.contains(glyphs[pos]) {
                    return None;
                }
                last = pos;
                cursor = pos + 1;
            }
            (last, last + 1)
        };
        // Lookahead.
        let mut la_cursor = after;
        for cov in &self.lookahead {
            let pos = filter.next_unskipped(glyphs, la_cursor)?;
            if !cov.contains(glyphs[pos]) {
                return None;
            }
            la_cursor = pos + 1;
        }
        if self.input.is_empty() {
            Some(0)
        } else {
            Some(last - i + 1)
        }
    }
}

/// A format-dispatching wrapper over every chained-context
/// subtable. The shape driver calls [`ChainContextAny::parse`] and
/// then matches on the variant — format 1 and 2 reuse the shared
/// layout-level parsers.
#[derive(Debug, Clone)]
pub enum ChainContextAny<'a> {
    /// Format 1 — glyph-based.
    Format1(crate::tables::layout::ChainContext1<'a>),
    /// Format 2 — class-based.
    Format2(crate::tables::layout::ChainContext2<'a>),
    /// Format 3 — coverage-based (the original sigilbuzz
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
    let mut out = Vec::with_capacity(count);
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
        assert!(ctx.matches(&[20, 10, 30, 40, 50], 2));
    }

    #[test]
    fn rejects_backtrack_mismatch() {
        let bytes = build_format3(&[&[10]], &[&[30]], &[], &[(0, 1)]);
        let ctx = ChainContext::parse(&bytes).unwrap();
        // glyph before input is 11, not 10.
        assert!(!ctx.matches(&[11, 30], 1));
    }

    #[test]
    fn rejects_input_mismatch() {
        let bytes = build_format3(&[], &[&[30], &[40]], &[], &[(0, 1)]);
        let ctx = ChainContext::parse(&bytes).unwrap();
        assert!(!ctx.matches(&[30, 99], 0));
    }

    #[test]
    fn rejects_lookahead_mismatch() {
        let bytes = build_format3(&[], &[&[30]], &[&[50]], &[(0, 1)]);
        let ctx = ChainContext::parse(&bytes).unwrap();
        assert!(!ctx.matches(&[30, 99], 0));
    }

    #[test]
    fn fails_when_backtrack_runs_off_start_of_run() {
        let bytes = build_format3(&[&[10]], &[&[30]], &[], &[(0, 1)]);
        let ctx = ChainContext::parse(&bytes).unwrap();
        // Input at position 0 with one-glyph backtrack requirement
        // cannot match — there is nothing behind position 0.
        assert!(!ctx.matches(&[30, 40], 0));
    }

    #[test]
    fn fails_when_lookahead_runs_off_end_of_run() {
        let bytes = build_format3(&[], &[&[30]], &[&[50]], &[(0, 1)]);
        let ctx = ChainContext::parse(&bytes).unwrap();
        assert!(!ctx.matches(&[30], 0));
    }

    #[test]
    fn empty_context_matches_anywhere() {
        // No backtrack, no lookahead, single-glyph input.
        let bytes = build_format3(&[], &[&[30]], &[], &[(0, 5)]);
        let ctx = ChainContext::parse(&bytes).unwrap();
        assert!(ctx.matches(&[30], 0));
        assert!(ctx.matches(&[99, 30, 88], 1));
        assert!(!ctx.matches(&[99, 30], 0));
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        assert!(ChainContext::parse(&bytes).is_err());
    }
}
