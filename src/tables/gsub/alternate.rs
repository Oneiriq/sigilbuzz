//! GSUB lookup type 3 — Alternate Substitution.
//!
//! Replaces one input glyph with one output chosen from an
//! `AlternateSet`. The choice is driven by the feature's *value*:
//! a user enabling `salt` or `swsh` with `value: N` picks the
//! `N`-th alternate (1-indexed per the spec). `value: 0` disables
//! the feature entirely, same as every other sigilbuzz feature.
//!
//! # Layout
//!
//! ```text
//!   u16      substFormat = 1
//!   Offset16 coverageOffset
//!   u16      alternateSetCount
//!   Offset16 alternateSetOffsets[alternateSetCount]
//! ```
//!
//! Each AlternateSet (at `alternateSetOffset`):
//!
//! ```text
//!   u16 glyphCount
//!   u16 alternateGlyphIDs[glyphCount]
//! ```

use crate::error::{Error, Result};
use crate::tables::layout::Coverage;
use crate::tables::parse::Reader;

/// A parsed Alternate Substitution subtable.
#[derive(Debug, Clone, Copy)]
pub struct Alternate<'a> {
    data: &'a [u8],
    coverage: Coverage<'a>,
    alt_set_offsets_off: usize,
    alt_set_count: u16,
}

impl<'a> Alternate<'a> {
    /// Parses an Alternate Substitution subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported alternate substitution format",
            });
        }
        let coverage_off = r.read_u16()? as usize;
        let alt_set_count = r.read_u16()?;
        let alt_set_offsets_off = r.position();
        let need = alt_set_offsets_off + alt_set_count as usize * 2;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: alt_set_offsets_off,
                context: "alternate substitution set offsets shorter than count",
            });
        }
        let coverage = Coverage::parse(data.get(coverage_off..).ok_or(Error::Malformed {
            offset: coverage_off,
            context: "alternate substitution coverage offset past end",
        })?)?;
        Ok(Self {
            data,
            coverage,
            alt_set_offsets_off,
            alt_set_count,
        })
    }

    /// Coverage table for the run-level "would_apply" precheck.
    #[must_use]
    pub const fn coverage(&self) -> &Coverage<'a> {
        &self.coverage
    }

    /// Returns the chosen alternate for `glyph`. `alternate_index`
    /// is 0-based; per the spec's 1-based value encoding, callers
    /// pass `feature_value - 1` and clamp to the available range.
    /// Returns `None` when the glyph is not covered or the index
    /// overruns the alternate set.
    #[must_use]
    pub fn apply(&self, glyph: u16, alternate_index: u16) -> Option<u16> {
        let set_idx = self.coverage.index_of(glyph)?;
        if set_idx >= self.alt_set_count {
            return None;
        }
        let off_slot = self.alt_set_offsets_off + set_idx as usize * 2;
        let set_off = u16::from_be_bytes([self.data[off_slot], self.data[off_slot + 1]]) as usize;
        let set_bytes = self.data.get(set_off..)?;

        let mut r = Reader::new(set_bytes);
        let glyph_count = r.read_u16().ok()?;
        if alternate_index >= glyph_count {
            return None;
        }
        let at = 2 + alternate_index as usize * 2;
        if set_bytes.len() < at + 2 {
            return None;
        }
        Some(u16::from_be_bytes([set_bytes[at], set_bytes[at + 1]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

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
        let slots_start = out.len();
        for _ in 0..sets.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        for (i, (_glyph, alts)) in sets.iter().enumerate() {
            let set_start = out.len();
            out.extend_from_slice(&(alts.len() as u16).to_be_bytes());
            for g in alts {
                out.extend_from_slice(&g.to_be_bytes());
            }
            let slot = slots_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&(set_start as u16).to_be_bytes());
        }
        let cov_start = out.len();
        let covered: Vec<u16> = sets.iter().map(|(g, _)| *g).collect();
        out.extend_from_slice(&build_coverage_format1(&covered));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    #[test]
    fn first_alternate_is_index_zero() {
        let bytes = build_subtable(&[(10, alloc::vec![100, 200, 300])]);
        let alt = Alternate::parse(&bytes).unwrap();
        assert_eq!(alt.apply(10, 0), Some(100));
        assert_eq!(alt.apply(10, 1), Some(200));
        assert_eq!(alt.apply(10, 2), Some(300));
    }

    #[test]
    fn index_past_set_returns_none() {
        let bytes = build_subtable(&[(10, alloc::vec![100, 200])]);
        let alt = Alternate::parse(&bytes).unwrap();
        assert!(alt.apply(10, 2).is_none());
        assert!(alt.apply(10, 99).is_none());
    }

    #[test]
    fn uncovered_glyph_returns_none() {
        let bytes = build_subtable(&[(10, alloc::vec![100])]);
        let alt = Alternate::parse(&bytes).unwrap();
        assert!(alt.apply(99, 0).is_none());
    }

    #[test]
    fn multiple_alternate_sets_are_independent() {
        let bytes = build_subtable(&[
            (10, alloc::vec![100, 101]),
            (20, alloc::vec![200, 201, 202]),
        ]);
        let alt = Alternate::parse(&bytes).unwrap();
        assert_eq!(alt.apply(10, 1), Some(101));
        assert_eq!(alt.apply(20, 2), Some(202));
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        assert!(Alternate::parse(&bytes).is_err());
    }
}
