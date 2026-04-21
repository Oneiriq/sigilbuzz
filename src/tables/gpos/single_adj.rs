//! GPOS lookup type 1 — Single Adjustment.
//!
//! Applies a [`ValueRecord`] to every covered glyph. Two formats
//! exist: format 1 uses the same record for every glyph (used when
//! a feature wants to shift a whole class of glyphs by a constant
//! — e.g. lining vs. old-style figures that need a vertical tweak),
//! format 2 has a parallel array of records, one per covered glyph
//! (used for per-glyph fine positioning).
//!
//! # Layout
//!
//! ```text
//!   format 1:
//!     u16         posFormat     = 1
//!     Offset16    coverageOffset
//!     u16         valueFormat
//!     ValueRecord valueRecord
//!
//!   format 2:
//!     u16         posFormat     = 2
//!     Offset16    coverageOffset
//!     u16         valueFormat
//!     u16         valueCount    (must equal coverage length)
//!     ValueRecord valueRecords[valueCount]
//! ```

use crate::error::{Error, Result};
use crate::tables::gpos::value_record::ValueRecord;
use crate::tables::layout::Coverage;
use crate::tables::parse::Reader;

/// A parsed Single Adjustment subtable.
#[derive(Debug, Clone, Copy)]
pub enum SinglePos<'a> {
    /// Format 1 — one `ValueRecord` for every covered glyph.
    Uniform {
        /// Covered glyph set.
        coverage: Coverage<'a>,
        /// The shared adjustment.
        value: ValueRecord,
    },
    /// Format 2 — one `ValueRecord` per coverage index.
    PerGlyph {
        /// Covered glyph set.
        coverage: Coverage<'a>,
        /// Subtable bytes (to re-read value records on demand).
        data: &'a [u8],
        /// Byte offset of the value-record array.
        values_off: usize,
        /// Count of records (must equal coverage length).
        value_count: u16,
        /// `ValueRecord` format word for each record.
        value_format: u16,
    },
}

impl<'a> SinglePos<'a> {
    /// Parses a Single Adjustment subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        let coverage_off = r.read_u16()? as usize;
        let value_format = r.read_u16()?;
        let coverage = Coverage::parse(data.get(coverage_off..).ok_or(Error::Malformed {
            offset: coverage_off,
            context: "singlePos coverage offset past end",
        })?)?;

        match format {
            1 => {
                let value = ValueRecord::parse(&mut r, value_format)?;
                Ok(Self::Uniform { coverage, value })
            }
            2 => {
                let value_count = r.read_u16()?;
                let values_off = r.position();
                let stride = ValueRecord::size(value_format);
                let need = values_off + value_count as usize * stride;
                if data.len() < need {
                    return Err(Error::Truncated {
                        offset: values_off,
                        context: "singlePos format 2 value records shorter than count",
                    });
                }
                Ok(Self::PerGlyph {
                    coverage,
                    data,
                    values_off,
                    value_count,
                    value_format,
                })
            }
            _ => Err(Error::Malformed {
                offset: 0,
                context: "unsupported singlePos format",
            }),
        }
    }

    /// Returns the adjustment for `glyph_id`, or `None` when it is
    /// not covered by this subtable.
    #[must_use]
    pub fn adjustment(&self, glyph_id: u16) -> Option<ValueRecord> {
        match *self {
            Self::Uniform { coverage, value } => {
                coverage.index_of(glyph_id)?;
                Some(value)
            }
            Self::PerGlyph {
                coverage,
                data,
                values_off,
                value_count,
                value_format,
            } => {
                let idx = coverage.index_of(glyph_id)?;
                if idx >= value_count {
                    return None;
                }
                let stride = ValueRecord::size(value_format);
                let at = values_off + idx as usize * stride;
                let mut r = Reader::at(data, at).ok()?;
                ValueRecord::parse(&mut r, value_format).ok()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::gpos::value_record::{X_ADVANCE, X_PLACEMENT};
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

    fn build_format1(covered: &[u16], value_format: u16, fields: &[i16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
        let cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&value_format.to_be_bytes());
        for f in fields {
            out.extend_from_slice(&f.to_be_bytes());
        }
        let cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(covered));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    fn build_format2(covered: &[u16], value_format: u16, records: &[&[i16]]) -> Vec<u8> {
        assert_eq!(covered.len(), records.len());
        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes()); // posFormat
        let cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&value_format.to_be_bytes());
        out.extend_from_slice(&(records.len() as u16).to_be_bytes()); // valueCount
        for rec in records {
            for f in *rec {
                out.extend_from_slice(&f.to_be_bytes());
            }
        }
        let cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(covered));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    #[test]
    fn format1_applies_same_adjustment_to_every_covered_glyph() {
        // Covered 10, 20, 30; shared x_advance delta = -25.
        let bytes = build_format1(&[10, 20, 30], X_ADVANCE, &[-25]);
        let sp = SinglePos::parse(&bytes).unwrap();
        assert_eq!(sp.adjustment(10).unwrap().x_advance, -25);
        assert_eq!(sp.adjustment(20).unwrap().x_advance, -25);
        assert_eq!(sp.adjustment(30).unwrap().x_advance, -25);
        assert!(sp.adjustment(40).is_none());
    }

    #[test]
    fn format1_can_mix_placement_and_advance_fields() {
        // value_format = X_PLACEMENT | X_ADVANCE → two i16 fields.
        let bytes = build_format1(&[5], X_PLACEMENT | X_ADVANCE, &[4, -10]);
        let sp = SinglePos::parse(&bytes).unwrap();
        let v = sp.adjustment(5).unwrap();
        assert_eq!(v.x_placement, 4);
        assert_eq!(v.x_advance, -10);
    }

    #[test]
    fn format2_uses_per_glyph_records() {
        let bytes = build_format2(&[10, 20, 30], X_ADVANCE, &[&[-5], &[-10], &[-15]]);
        let sp = SinglePos::parse(&bytes).unwrap();
        assert_eq!(sp.adjustment(10).unwrap().x_advance, -5);
        assert_eq!(sp.adjustment(20).unwrap().x_advance, -10);
        assert_eq!(sp.adjustment(30).unwrap().x_advance, -15);
        assert!(sp.adjustment(25).is_none());
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 6]); // minimal bytes to let parser reach format check
        assert!(SinglePos::parse(&bytes).is_err());
    }

    #[test]
    fn rejects_truncated_format2_body() {
        // posFormat=2, coverageOffset pointing to later slot,
        // valueFormat=X_ADVANCE (2 bytes/record), valueCount=3 but
        // only one record bytes present.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&100u16.to_be_bytes()); // far coverage — may or may not parse
        bytes.extend_from_slice(&X_ADVANCE.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes()); // valueCount
        bytes.extend_from_slice(&0u16.to_be_bytes()); // one record instead of three
        assert!(SinglePos::parse(&bytes).is_err());
    }
}
