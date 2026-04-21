//! GSUB lookup type 6 — Chained Context Substitution, format 3.
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
    /// coverages all line up. Does not modify anything — callers
    /// run the nested lookups when this returns true.
    ///
    /// `glyphs[i..]` is the forward view; the backtrack walks
    /// `glyphs[..i]` from the right.
    #[must_use]
    pub fn matches(&self, glyphs: &[u16], i: usize) -> bool {
        // Backtrack — iterate in spec order (first record = closest
        // preceding glyph).
        for (offset, cov) in self.backtrack.iter().enumerate() {
            let Some(pos) = i.checked_sub(offset + 1) else {
                return false;
            };
            if !cov.contains(glyphs[pos]) {
                return false;
            }
        }
        // Input.
        if i + self.input.len() > glyphs.len() {
            return false;
        }
        for (j, cov) in self.input.iter().enumerate() {
            if !cov.contains(glyphs[i + j]) {
                return false;
            }
        }
        // Lookahead.
        let after = i + self.input.len();
        if after + self.lookahead.len() > glyphs.len() {
            return false;
        }
        for (j, cov) in self.lookahead.iter().enumerate() {
            if !cov.contains(glyphs[after + j]) {
                return false;
            }
        }
        true
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
