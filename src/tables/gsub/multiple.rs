//! GSUB lookup type 2 — Multiple Substitution.
//!
//! Replaces one input glyph with a sequence of output glyphs. The
//! inverse of ligature substitution. Most commonly used for
//! decomposition features (e.g. `ccmp` splitting a single compound
//! glyph into base + mark components), and by certain scripts that
//! decompose during shaping.
//!
//! # Layout
//!
//! ```text
//!   u16      substFormat = 1
//!   Offset16 coverageOffset
//!   u16      sequenceCount
//!   Offset16 sequenceOffsets[sequenceCount]
//! ```
//!
//! Each Sequence (at `sequenceOffset`):
//!
//! ```text
//!   u16 glyphCount
//!   u16 substituteGlyphIDs[glyphCount]
//! ```

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::layout::Coverage;
use crate::tables::parse::Reader;

/// A parsed Multiple Substitution subtable.
#[derive(Debug, Clone, Copy)]
pub struct Multiple<'a> {
    data: &'a [u8],
    coverage: Coverage<'a>,
    sequence_offsets_off: usize,
    sequence_count: u16,
}

impl<'a> Multiple<'a> {
    /// Parses a Multiple Substitution subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported multiple substitution format",
            });
        }
        let coverage_off = r.read_u16()? as usize;
        let sequence_count = r.read_u16()?;
        let sequence_offsets_off = r.position();
        let need = sequence_offsets_off + sequence_count as usize * 2;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: sequence_offsets_off,
                context: "multiple substitution sequence offsets shorter than count",
            });
        }

        let coverage = Coverage::parse(data.get(coverage_off..).ok_or(Error::Malformed {
            offset: coverage_off,
            context: "multiple substitution coverage offset past end",
        })?)?;

        Ok(Self {
            data,
            coverage,
            sequence_offsets_off,
            sequence_count,
        })
    }

    /// Returns the substitute glyph sequence for `glyph`, or `None`
    /// when this subtable does not cover it.
    #[must_use]
    pub fn apply(&self, glyph: u16) -> Option<Vec<u16>> {
        let idx = self.coverage.index_of(glyph)?;
        if idx >= self.sequence_count {
            return None;
        }
        let off_slot = self.sequence_offsets_off + idx as usize * 2;
        let seq_off = u16::from_be_bytes([self.data[off_slot], self.data[off_slot + 1]]) as usize;
        let seq_bytes = self.data.get(seq_off..)?;

        let mut r = Reader::new(seq_bytes);
        let glyph_count = r.read_u16().ok()?;
        if seq_bytes.len() < 2 + glyph_count as usize * 2 {
            return None;
        }
        let mut out = Vec::with_capacity(glyph_count as usize);
        for _ in 0..glyph_count {
            out.push(r.read_u16().ok()?);
        }
        Some(out)
    }
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

    fn build_subtable(sets: &[(u16, Vec<u16>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        let cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&(sets.len() as u16).to_be_bytes());
        let seq_slots_start = out.len();
        for _ in 0..sets.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        for (i, (_glyph, seq)) in sets.iter().enumerate() {
            let seq_start = out.len();
            out.extend_from_slice(&(seq.len() as u16).to_be_bytes());
            for g in seq {
                out.extend_from_slice(&g.to_be_bytes());
            }
            let slot = seq_slots_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&(seq_start as u16).to_be_bytes());
        }
        let cov_start = out.len();
        let covered: Vec<u16> = sets.iter().map(|(g, _)| *g).collect();
        out.extend_from_slice(&build_coverage_format1(&covered));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    #[test]
    fn single_glyph_decomposes_into_sequence() {
        // Glyph 100 decomposes to [40, 50, 60].
        let bytes = build_subtable(&[(100, alloc::vec![40, 50, 60])]);
        let m = Multiple::parse(&bytes).unwrap();
        assert_eq!(m.apply(100), Some(alloc::vec![40, 50, 60]));
    }

    #[test]
    fn uncovered_glyph_returns_none() {
        let bytes = build_subtable(&[(100, alloc::vec![40, 50])]);
        let m = Multiple::parse(&bytes).unwrap();
        assert!(m.apply(99).is_none());
    }

    #[test]
    fn multiple_covered_glyphs_each_decompose() {
        let bytes = build_subtable(&[
            (10, alloc::vec![1, 2]),
            (20, alloc::vec![3, 4, 5]),
            (30, alloc::vec![6]),
        ]);
        let m = Multiple::parse(&bytes).unwrap();
        assert_eq!(m.apply(10), Some(alloc::vec![1, 2]));
        assert_eq!(m.apply(20), Some(alloc::vec![3, 4, 5]));
        assert_eq!(m.apply(30), Some(alloc::vec![6]));
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        assert!(Multiple::parse(&bytes).is_err());
    }
}
