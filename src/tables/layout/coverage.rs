//! OpenType Coverage tables.
//!
//! A Coverage table enumerates the glyphs a particular lookup cares
//! about and assigns each of them a stable "coverage index". Every
//! GSUB and GPOS lookup starts by asking Coverage two questions in
//! sequence:
//!
//! 1. Is this glyph covered?
//! 2. If yes, at what index — so the lookup can pick the right entry
//!    out of a parallel array of substitutions or adjustments.
//!
//! Both questions resolve via a single [`Coverage::index_of`] call:
//! `Some(i)` answers both simultaneously, `None` means "skip this
//! glyph, it is not in this lookup's domain."
//!
//! # Formats
//!
//! ## Format 1 — `GlyphArray`
//!
//! ```text
//!   u16 coverageFormat   = 1
//!   u16 glyphCount
//!   u16 glyphArray[glyphCount]   (sorted ascending)
//! ```
//!
//! Coverage index of a hit is its position in `glyphArray`. Binary
//! search finds it in `O(log n)`.
//!
//! ## Format 2 — `RangeRecords`
//!
//! ```text
//!   u16 coverageFormat   = 2
//!   u16 rangeCount
//!   RangeRecord records[rangeCount]:
//!     u16 startGlyphID
//!     u16 endGlyphID
//!     u16 startCoverageIndex
//! ```
//!
//! Ranges are sorted and non-overlapping. Coverage index of a hit
//! inside a range is `startCoverageIndex + (glyphID - startGlyphID)`.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// A parsed Coverage table.
///
/// Borrows the underlying bytes so lookups are allocation-free.
#[derive(Debug, Clone, Copy)]
pub struct Coverage<'a> {
    data: &'a [u8],
    format: Format,
    count: u16,
    body_off: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Format1,
    Format2,
}

impl<'a> Coverage<'a> {
    /// Parses a Coverage table from its raw bytes.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = match r.read_u16()? {
            1 => Format::Format1,
            2 => Format::Format2,
            other => {
                return Err(Error::Malformed {
                    offset: 0,
                    context: match other {
                        0 => "coverage format 0 not defined",
                        _ => "unsupported coverage format",
                    },
                });
            }
        };
        let count = r.read_u16()?;
        let body_off = r.position();
        let body_bytes = count as usize
            * match format {
                Format::Format1 => 2,
                Format::Format2 => 6,
            };
        if data.len() < body_off + body_bytes {
            return Err(Error::Truncated {
                offset: body_off,
                context: "coverage body shorter than count implies",
            });
        }
        Ok(Self {
            data,
            format,
            count,
            body_off,
        })
    }

    /// Number of covered glyph slots. For format 2 this is the
    /// number of ranges, not the number of covered glyphs.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.count
    }

    /// True when the table covers zero glyphs.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Coverage index of `glyph_id`, or `None` if the glyph is not
    /// covered by this table.
    #[must_use]
    pub fn index_of(&self, glyph_id: u16) -> Option<u16> {
        match self.format {
            Format::Format1 => self.index_format1(glyph_id),
            Format::Format2 => self.index_format2(glyph_id),
        }
    }

    /// True if the table covers `glyph_id`.
    #[must_use]
    pub fn contains(&self, glyph_id: u16) -> bool {
        self.index_of(glyph_id).is_some()
    }

    fn glyph_at(&self, i: u16) -> u16 {
        let off = self.body_off + i as usize * 2;
        u16::from_be_bytes([self.data[off], self.data[off + 1]])
    }

    fn range_at(&self, i: u16) -> (u16, u16, u16) {
        let off = self.body_off + i as usize * 6;
        let start = u16::from_be_bytes([self.data[off], self.data[off + 1]]);
        let end = u16::from_be_bytes([self.data[off + 2], self.data[off + 3]]);
        let start_cov = u16::from_be_bytes([self.data[off + 4], self.data[off + 5]]);
        (start, end, start_cov)
    }

    fn index_format1(&self, glyph_id: u16) -> Option<u16> {
        if self.count == 0 {
            return None;
        }
        let mut lo: u16 = 0;
        let mut hi: u16 = self.count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let g = self.glyph_at(mid);
            match g.cmp(&glyph_id) {
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
                core::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    fn index_format2(&self, glyph_id: u16) -> Option<u16> {
        if self.count == 0 {
            return None;
        }
        let mut lo: u16 = 0;
        let mut hi: u16 = self.count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (start, end, start_cov) = self.range_at(mid);
            if glyph_id < start {
                hi = mid;
            } else if glyph_id > end {
                lo = mid + 1;
            } else {
                return Some(start_cov + (glyph_id - start));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn build_format1(glyphs: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
        for g in glyphs {
            out.extend_from_slice(&g.to_be_bytes());
        }
        out
    }

    fn build_format2(ranges: &[(u16, u16, u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes());
        out.extend_from_slice(&(ranges.len() as u16).to_be_bytes());
        for (start, end, cov) in ranges {
            out.extend_from_slice(&start.to_be_bytes());
            out.extend_from_slice(&end.to_be_bytes());
            out.extend_from_slice(&cov.to_be_bytes());
        }
        out
    }

    #[test]
    fn format1_returns_position_on_hit() {
        let bytes = build_format1(&[5, 10, 15, 20]);
        let cov = Coverage::parse(&bytes).unwrap();
        assert_eq!(cov.index_of(5), Some(0));
        assert_eq!(cov.index_of(10), Some(1));
        assert_eq!(cov.index_of(15), Some(2));
        assert_eq!(cov.index_of(20), Some(3));
    }

    #[test]
    fn format1_returns_none_for_miss_and_boundary() {
        let bytes = build_format1(&[5, 10, 15, 20]);
        let cov = Coverage::parse(&bytes).unwrap();
        assert_eq!(cov.index_of(0), None);
        assert_eq!(cov.index_of(7), None);
        assert_eq!(cov.index_of(21), None);
    }

    #[test]
    fn format1_handles_single_element_table() {
        let bytes = build_format1(&[42]);
        let cov = Coverage::parse(&bytes).unwrap();
        assert_eq!(cov.index_of(41), None);
        assert_eq!(cov.index_of(42), Some(0));
        assert_eq!(cov.index_of(43), None);
    }

    #[test]
    fn format2_returns_offset_within_range() {
        // Range 100..=102 maps to coverage indices 0..=2.
        // Range 200..=204 maps to coverage indices 10..=14.
        let bytes = build_format2(&[(100, 102, 0), (200, 204, 10)]);
        let cov = Coverage::parse(&bytes).unwrap();
        assert_eq!(cov.index_of(100), Some(0));
        assert_eq!(cov.index_of(101), Some(1));
        assert_eq!(cov.index_of(102), Some(2));
        assert_eq!(cov.index_of(200), Some(10));
        assert_eq!(cov.index_of(204), Some(14));
    }

    #[test]
    fn format2_returns_none_in_gaps_and_outside_table() {
        let bytes = build_format2(&[(100, 102, 0), (200, 204, 10)]);
        let cov = Coverage::parse(&bytes).unwrap();
        assert_eq!(cov.index_of(0), None);
        assert_eq!(cov.index_of(99), None);
        assert_eq!(cov.index_of(103), None);
        assert_eq!(cov.index_of(199), None);
        assert_eq!(cov.index_of(205), None);
    }

    #[test]
    fn empty_table_reports_zero_length_and_no_matches() {
        let bytes = build_format1(&[]);
        let cov = Coverage::parse(&bytes).unwrap();
        assert_eq!(cov.len(), 0);
        assert!(cov.is_empty());
        assert_eq!(cov.index_of(1), None);
    }

    #[test]
    fn contains_is_a_boolean_view_of_index_of() {
        let bytes = build_format1(&[1, 2, 3]);
        let cov = Coverage::parse(&bytes).unwrap();
        assert!(cov.contains(2));
        assert!(!cov.contains(4));
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = alloc::vec![0u8, 9]; // format = 9
        bytes.extend_from_slice(&0u16.to_be_bytes());
        assert!(matches!(
            Coverage::parse(&bytes),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_truncated_body() {
        // Format 1 declares three glyphs but only provides two.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&2u16.to_be_bytes());
        assert!(matches!(
            Coverage::parse(&bytes),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(Coverage::parse(&[0]).is_err());
    }
}
