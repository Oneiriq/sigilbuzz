//! GSUB lookup type 8: Reverse Chained Contextual Single Substitution.
//!
//! The "reverse chained" lookup is a one-glyph substitution (like
//! type 1) that runs *backwards* over the run and that matches a
//! chained context around every candidate. It is the mechanism
//! behind Nastaliq-style Arabic fonts and a handful of complex
//! Indic shapers: rules that depend on what comes *after* the
//! input need to fire right-to-left so the decision for the last
//! glyph is not contaminated by earlier substitutions.
//!
//! # Subtable layout (format 1, the only defined format)
//!
//! ```text
//!   u16      substFormat = 1
//!   Offset16 coverageOffset           (input glyph coverage)
//!   u16      backtrackGlyphCount
//!   Offset16 backtrackCoverageOffsets[backtrackGlyphCount]
//!   u16      lookaheadGlyphCount
//!   Offset16 lookaheadCoverageOffsets[lookaheadGlyphCount]
//!   u16      glyphCount                (== coverage.len())
//!   u16      substituteGlyphIDs[glyphCount]
//! ```
//!
//! Backtrack and lookahead coverages use the same ordering rules as
//! chained context: backtrack is reverse-order (closest first),
//! lookahead is forward-order.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::layout::Coverage;
use crate::tables::parse::Reader;

/// A parsed reverse chained single-substitution subtable.
#[derive(Debug, Clone)]
pub struct ReverseChain<'a> {
    coverage: Coverage<'a>,
    backtrack: Vec<Coverage<'a>>,
    lookahead: Vec<Coverage<'a>>,
    substitutes: Vec<u16>,
}

impl<'a> ReverseChain<'a> {
    /// Parses a reverse-chained-single subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported reverse-chained-sub format",
            });
        }
        let coverage_off = r.read_u16()? as usize;
        let backtrack = parse_coverage_array(data, &mut r)?;
        let lookahead = parse_coverage_array(data, &mut r)?;
        let glyph_count = r.read_u16()? as usize;
        let mut substitutes = Vec::with_capacity(glyph_count);
        for _ in 0..glyph_count {
            substitutes.push(r.read_u16()?);
        }

        let coverage = Coverage::parse(data.get(coverage_off..).ok_or(Error::Malformed {
            offset: coverage_off,
            context: "reverse-chained-sub coverage offset past end",
        })?)?;

        Ok(Self {
            coverage,
            backtrack,
            lookahead,
            substitutes,
        })
    }

    /// Window widths (backtrack, lookahead). Input width is always 1.
    #[must_use]
    pub fn context_len(&self) -> (usize, usize) {
        (self.backtrack.len(), self.lookahead.len())
    }

    /// Substitute for `glyph_id` when the surrounding context matches,
    /// or `None`. `glyphs[..i]` is the prefix, `glyphs[i+1..]` is the
    /// suffix.
    #[must_use]
    pub fn apply(&self, glyphs: &[u16], i: usize) -> Option<u16> {
        let gid = *glyphs.get(i)?;
        let cov_i = self.coverage.index_of(gid)? as usize;
        // Backtrack walks `glyphs[..i]` right-to-left.
        for (offset, cov) in self.backtrack.iter().enumerate() {
            let pos = i.checked_sub(offset + 1)?;
            if !cov.contains(glyphs[pos]) {
                return None;
            }
        }
        // Lookahead walks `glyphs[i+1..]` left-to-right.
        let after = i + 1;
        if after + self.lookahead.len() > glyphs.len() {
            return None;
        }
        for (j, cov) in self.lookahead.iter().enumerate() {
            if !cov.contains(glyphs[after + j]) {
                return None;
            }
        }
        self.substitutes.get(cov_i).copied()
    }
}

fn parse_coverage_array<'a>(data: &'a [u8], r: &mut Reader<'_>) -> Result<Vec<Coverage<'a>>> {
    let count = r.read_u16()? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let off = r.read_u16()? as usize;
        let bytes = data.get(off..).ok_or(Error::Malformed {
            offset: off,
            context: "reverse-chained-sub coverage offset past end",
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

    /// Builds a format-1 reverse-chain subtable. `subs[i]` is the
    /// output for the i-th coverage entry.
    fn build_subtable(
        input: &[u16],
        backtrack: &[&[u16]],
        lookahead: &[&[u16]],
        subs: &[u16],
    ) -> Vec<u8> {
        assert_eq!(input.len(), subs.len());
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        out.extend_from_slice(&0u16.to_be_bytes()); // coverage slot

        let cov_slot = 2;

        // Backtrack array.
        out.extend_from_slice(&(backtrack.len() as u16).to_be_bytes());
        let bt_slots_start = out.len();
        for _ in 0..backtrack.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        // Lookahead array.
        out.extend_from_slice(&(lookahead.len() as u16).to_be_bytes());
        let la_slots_start = out.len();
        for _ in 0..lookahead.len() {
            out.extend_from_slice(&[0u8; 2]);
        }

        // substituteGlyphIDs.
        out.extend_from_slice(&(subs.len() as u16).to_be_bytes());
        for s in subs {
            out.extend_from_slice(&s.to_be_bytes());
        }

        // Append coverage and patch.
        let cov_off = out.len();
        out.extend_from_slice(&build_coverage_format1(input));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());

        // Patch backtrack + lookahead.
        let patch = |out: &mut Vec<u8>, start: usize, covs: &[&[u16]]| {
            for (i, gs) in covs.iter().enumerate() {
                let off = out.len();
                out.extend_from_slice(&build_coverage_format1(gs));
                let slot = start + i * 2;
                out[slot..slot + 2].copy_from_slice(&(off as u16).to_be_bytes());
            }
        };
        patch(&mut out, bt_slots_start, backtrack);
        patch(&mut out, la_slots_start, lookahead);
        out
    }

    #[test]
    fn substitutes_when_context_matches() {
        // Input {10} -> substitute 100. Backtrack: [5]. Lookahead: [30].
        let bytes = build_subtable(&[10], &[&[5]], &[&[30]], &[100]);
        let rc = ReverseChain::parse(&bytes).unwrap();
        assert_eq!(rc.context_len(), (1, 1));
        assert_eq!(rc.apply(&[5, 10, 30], 1), Some(100));
    }

    #[test]
    fn rejects_mismatched_backtrack() {
        let bytes = build_subtable(&[10], &[&[5]], &[], &[100]);
        let rc = ReverseChain::parse(&bytes).unwrap();
        assert_eq!(rc.apply(&[99, 10], 1), None);
    }

    #[test]
    fn rejects_mismatched_lookahead() {
        let bytes = build_subtable(&[10], &[], &[&[30]], &[100]);
        let rc = ReverseChain::parse(&bytes).unwrap();
        assert_eq!(rc.apply(&[10, 99], 0), None);
    }

    #[test]
    fn uncovered_input_returns_none() {
        let bytes = build_subtable(&[10], &[], &[], &[100]);
        let rc = ReverseChain::parse(&bytes).unwrap();
        assert_eq!(rc.apply(&[11], 0), None);
    }

    #[test]
    fn returns_per_coverage_index_substitute() {
        // Coverage {10, 11, 12} -> substitutes {100, 101, 102}.
        let bytes = build_subtable(&[10, 11, 12], &[], &[], &[100, 101, 102]);
        let rc = ReverseChain::parse(&bytes).unwrap();
        assert_eq!(rc.apply(&[10], 0), Some(100));
        assert_eq!(rc.apply(&[11], 0), Some(101));
        assert_eq!(rc.apply(&[12], 0), Some(102));
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        assert!(ReverseChain::parse(&bytes).is_err());
    }
}
