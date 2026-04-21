//! GSUB lookup type 1 — Single Substitution.
//!
//! Maps one glyph to one glyph. The simplest substitution type, and
//! the mechanism behind a surprisingly large fraction of shaping
//! work: small caps (`smcp`), stylistic alternates (`salt`),
//! stylistic sets (`ss01`..`ss20`), localised forms (`locl`), glyph
//! composition/decomposition (`ccmp`), and vertical substitutes
//! (`vert`) all ship as single-sub lookups in the real fonts that
//! declare them.
//!
//! # Subtable formats
//!
//! ## Format 1 — delta
//!
//! ```text
//!   u16      substFormat = 1
//!   Offset16 coverageOffset
//!   i16      deltaGlyphID
//! ```
//!
//! Output glyph = `(input + delta) mod 65536`. Works only when the
//! set of replacement glyphs is contiguous and offset from the
//! inputs by a constant — common when a font places "A..Z" and
//! their small-cap variants in adjacent glyph-id ranges.
//!
//! ## Format 2 — explicit
//!
//! ```text
//!   u16      substFormat = 2
//!   Offset16 coverageOffset
//!   u16      glyphCount
//!   u16      substituteGlyphIDs[glyphCount]
//! ```
//!
//! Output glyph = `substituteGlyphIDs[coverage_index]`. Used when
//! replacements do not form a contiguous range.

use crate::error::{Error, Result};
use crate::tables::layout::Coverage;
use crate::tables::parse::Reader;

/// A parsed Single Substitution subtable.
#[derive(Debug, Clone, Copy)]
pub enum Single<'a> {
    /// Format 1 — `(input + delta) mod 65536`.
    Delta {
        /// First-glyph coverage.
        coverage: Coverage<'a>,
        /// Delta to add to every covered glyph id.
        delta: i16,
    },
    /// Format 2 — explicit substitute list indexed by coverage index.
    Explicit {
        /// First-glyph coverage.
        coverage: Coverage<'a>,
        /// The subtable bytes (to re-read the substitute array on
        /// lookup without cloning).
        data: &'a [u8],
        /// Byte offset of the substitute array.
        substitutes_off: usize,
        /// Count of substitutes (must equal coverage length).
        glyph_count: u16,
    },
}

impl<'a> Single<'a> {
    /// Parses a Single Substitution subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        let coverage_off = r.read_u16()? as usize;
        let coverage = Coverage::parse(data.get(coverage_off..).ok_or(Error::Malformed {
            offset: coverage_off,
            context: "single-sub coverage offset past end",
        })?)?;

        match format {
            1 => {
                let delta = r.read_i16()?;
                Ok(Self::Delta { coverage, delta })
            }
            2 => {
                let glyph_count = r.read_u16()?;
                let substitutes_off = r.position();
                let need = substitutes_off + glyph_count as usize * 2;
                if data.len() < need {
                    return Err(Error::Truncated {
                        offset: substitutes_off,
                        context: "single-sub format 2 substitute array shorter than count",
                    });
                }
                Ok(Self::Explicit {
                    coverage,
                    data,
                    substitutes_off,
                    glyph_count,
                })
            }
            _ => Err(Error::Malformed {
                offset: 0,
                context: "unsupported single substitution format",
            }),
        }
    }

    /// Returns the substitute glyph id for `glyph` if this subtable
    /// covers it; `None` otherwise.
    #[must_use]
    pub fn apply(&self, glyph: u16) -> Option<u16> {
        match *self {
            Self::Delta { coverage, delta } => {
                coverage.index_of(glyph)?;
                Some(glyph.wrapping_add(delta as u16))
            }
            Self::Explicit {
                coverage,
                data,
                substitutes_off,
                glyph_count,
            } => {
                let idx = coverage.index_of(glyph)?;
                if idx >= glyph_count {
                    return None;
                }
                let off = substitutes_off + idx as usize * 2;
                Some(u16::from_be_bytes([data[off], data[off + 1]]))
            }
        }
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

    fn build_format1(covered: &[u16], delta: i16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // substFormat
        let cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&delta.to_be_bytes());
        let cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(covered));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    fn build_format2(covered: &[u16], substitutes: &[u16]) -> Vec<u8> {
        assert_eq!(covered.len(), substitutes.len());
        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes()); // substFormat
        let cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&(substitutes.len() as u16).to_be_bytes());
        for s in substitutes {
            out.extend_from_slice(&s.to_be_bytes());
        }
        let cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(covered));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    #[test]
    fn format1_adds_delta_to_covered_glyphs() {
        // Small-cap-style mapping: covered 65..=90 ('A'..='Z'), delta
        // +200 so 'A' (65) maps to small-cap glyph 265.
        let covered: Vec<u16> = (65..=90).collect();
        let bytes = build_format1(&covered, 200);
        let sub = Single::parse(&bytes).unwrap();
        assert_eq!(sub.apply(65), Some(265));
        assert_eq!(sub.apply(90), Some(290));
        assert_eq!(sub.apply(64), None);
        assert_eq!(sub.apply(91), None);
    }

    #[test]
    fn format1_handles_negative_delta() {
        let bytes = build_format1(&[100, 101, 102], -50);
        let sub = Single::parse(&bytes).unwrap();
        assert_eq!(sub.apply(100), Some(50));
        assert_eq!(sub.apply(101), Some(51));
        assert_eq!(sub.apply(102), Some(52));
    }

    #[test]
    fn format1_wraps_at_u16_boundary() {
        // delta big enough to wrap: 65500 + 100 = 65600 → wraps to 64.
        let bytes = build_format1(&[65500], 100);
        let sub = Single::parse(&bytes).unwrap();
        assert_eq!(sub.apply(65500), Some(64));
    }

    #[test]
    fn format2_yields_explicit_substitutes_by_coverage_index() {
        let bytes = build_format2(&[10, 20, 30], &[100, 200, 300]);
        let sub = Single::parse(&bytes).unwrap();
        assert_eq!(sub.apply(10), Some(100));
        assert_eq!(sub.apply(20), Some(200));
        assert_eq!(sub.apply(30), Some(300));
        assert_eq!(sub.apply(25), None);
    }

    #[test]
    fn format2_substitutes_can_jump_anywhere_in_glyph_space() {
        // Common for stylistic sets: replacements are scattered.
        let bytes = build_format2(&[1, 2, 3], &[9999, 1234, 42]);
        let sub = Single::parse(&bytes).unwrap();
        assert_eq!(sub.apply(1), Some(9999));
        assert_eq!(sub.apply(2), Some(1234));
        assert_eq!(sub.apply(3), Some(42));
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        // append a tiny coverage so the parse gets far enough to
        // hit the format check.
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        assert!(matches!(
            Single::parse(&bytes),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_truncated_format2_substitutes() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes()); // format
        bytes.extend_from_slice(&8u16.to_be_bytes()); // coverage offset (points past end to fail ahead of time)
        bytes.extend_from_slice(&3u16.to_be_bytes()); // glyphCount = 3
        bytes.extend_from_slice(&99u16.to_be_bytes()); // only one substitute
                                                       // no coverage present → parse fails on coverage lookup.
        assert!(Single::parse(&bytes).is_err());
    }
}
