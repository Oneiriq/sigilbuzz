//! GPOS lookup type 3: Cursive Attachment.
//!
//! Connects each glyph's *exit* point to the next glyph's *entry*
//! point, so a joined script (Arabic Nastaliq, N'Ko, some Indic
//! conjunct forms) flows as one continuous stroke even when the
//! glyphs sit at different heights. The feature that drives this at
//! shaping time is `curs`.
//!
//! # Subtable layout
//!
//! ```text
//!   u16      posFormat = 1
//!   Offset16 coverageOffset
//!   u16      entryExitCount
//!   EntryExitRecord records[entryExitCount]:
//!     Offset16 entryAnchorOffset   (relative to the subtable; 0 = none)
//!     Offset16 exitAnchorOffset    (relative to the subtable; 0 = none)
//! ```
//!
//! The records run parallel to the coverage indices. Anchors are the
//! shared [`Anchor`] type, so format 3 device offsets resolve through
//! [`Anchor::resolve`] against this subtable's bytes.

use crate::error::{Error, Result};
use crate::tables::gpos::anchor::Anchor;
use crate::tables::layout::Coverage;
use crate::tables::parse::Reader;

/// A parsed Cursive Attachment subtable (format 1).
#[derive(Debug, Clone, Copy)]
pub struct CursivePos<'a> {
    data: &'a [u8],
    coverage: Coverage<'a>,
    records_off: usize,
    record_count: u16,
}

impl<'a> CursivePos<'a> {
    /// Parses a Cursive Attachment subtable.
    ///
    /// ```
    /// use sigilbuzz::tables::gpos::cursive::CursivePos;
    ///
    /// // One covered glyph (gid 7) with an entry anchor at (0, 0)
    /// // and no exit anchor.
    /// let bytes = [
    ///     0x00, 0x01, // posFormat
    ///     0x00, 0x0A, // coverageOffset
    ///     0x00, 0x01, // entryExitCount
    ///     0x00, 0x10, 0x00, 0x00, // entry at 16, no exit
    ///     0x00, 0x01, 0x00, 0x01, 0x00, 0x07, // Coverage fmt 1: [7]
    ///     0x00, 0x01, 0x00, 0x00, 0x00, 0x00, // AnchorFormat1 (0, 0)
    /// ];
    /// let cp = CursivePos::parse(&bytes).unwrap();
    /// assert!(cp.entry(7).is_some());
    /// assert!(cp.exit(7).is_none());
    /// assert!(cp.entry(8).is_none());
    /// ```
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported cursivePos format",
            });
        }
        let cov_off = r.read_u16()? as usize;
        let record_count = r.read_u16()?;
        let records_off = r.position();
        let need = records_off + record_count as usize * 4;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: records_off,
                context: "cursivePos entryExitRecords shorter than entryExitCount",
            });
        }
        let coverage = Coverage::parse(data.get(cov_off..).ok_or(Error::Malformed {
            offset: cov_off,
            context: "cursivePos coverage offset past end",
        })?)?;
        Ok(Self {
            data,
            coverage,
            records_off,
            record_count,
        })
    }

    /// The raw subtable bytes. Anchors returned by [`Self::entry`] and
    /// [`Self::exit`] resolve their device offsets against this slice.
    #[must_use]
    pub const fn data(&self) -> &'a [u8] {
        self.data
    }

    /// True when `glyph_id` is in the subtable's coverage, whether or
    /// not it carries an entry or exit anchor.
    #[must_use]
    pub fn covers(&self, glyph_id: u16) -> bool {
        self.coverage.contains(glyph_id)
    }

    /// Entry anchor of `glyph_id`: where the previous glyph's exit
    /// point connects. `None` when the glyph is not covered, its
    /// entry offset is null, or the anchor is malformed.
    #[must_use]
    pub fn entry(&self, glyph_id: u16) -> Option<Anchor> {
        self.anchor(glyph_id, 0)
    }

    /// Exit anchor of `glyph_id`: where the next glyph's entry point
    /// connects. `None` under the same conditions as [`Self::entry`].
    #[must_use]
    pub fn exit(&self, glyph_id: u16) -> Option<Anchor> {
        self.anchor(glyph_id, 2)
    }

    fn anchor(&self, glyph_id: u16, field: usize) -> Option<Anchor> {
        let idx = self.coverage.index_of(glyph_id)?;
        if idx >= self.record_count {
            return None;
        }
        let at = self.records_off + idx as usize * 4 + field;
        let off = u16::from_be_bytes([*self.data.get(at)?, *self.data.get(at + 1)?]) as usize;
        if off == 0 {
            return None;
        }
        Anchor::parse_at(self.data, off).ok()
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

    fn anchor_fmt1(x: i16, y: i16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&x.to_be_bytes());
        out.extend_from_slice(&y.to_be_bytes());
        out
    }

    /// An optional `(x, y)` anchor point; `None` is a null offset.
    type Point = Option<(i16, i16)>;

    /// Builds a CursivePos subtable. `records[i]` pairs the entry and
    /// exit anchors of the i-th covered glyph.
    fn build(covered: &[u16], records: &[(Point, Point)]) -> Vec<u8> {
        let header_len = 6 + records.len() * 4;
        let coverage = build_coverage_format1(covered);
        let mut anchors: Vec<u8> = Vec::new();
        let mut offsets: Vec<(u16, u16)> = Vec::new();
        let anchors_base = header_len + coverage.len();
        let place = |a: Option<(i16, i16)>, anchors: &mut Vec<u8>| -> u16 {
            match a {
                None => 0,
                Some((x, y)) => {
                    let off = (anchors_base + anchors.len()) as u16;
                    anchors.extend_from_slice(&anchor_fmt1(x, y));
                    off
                }
            }
        };
        for (entry, exit) in records {
            let e = place(*entry, &mut anchors);
            let x = place(*exit, &mut anchors);
            offsets.push((e, x));
        }
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(header_len as u16).to_be_bytes());
        out.extend_from_slice(&(records.len() as u16).to_be_bytes());
        for (e, x) in offsets {
            out.extend_from_slice(&e.to_be_bytes());
            out.extend_from_slice(&x.to_be_bytes());
        }
        out.extend_from_slice(&coverage);
        out.extend_from_slice(&anchors);
        out
    }

    #[test]
    fn entry_and_exit_follow_coverage_index() {
        let bytes = build(
            &[10, 20],
            &[(Some((0, 100)), Some((500, 120))), (Some((5, 90)), None)],
        );
        let cp = CursivePos::parse(&bytes).unwrap();
        let e10 = cp.entry(10).unwrap();
        let x10 = cp.exit(10).unwrap();
        assert_eq!((e10.x, e10.y), (0, 100));
        assert_eq!((x10.x, x10.y), (500, 120));
        let e20 = cp.entry(20).unwrap();
        assert_eq!((e20.x, e20.y), (5, 90));
        assert!(cp.exit(20).is_none(), "null exit offset means no anchor");
        assert!(cp.covers(20));
    }

    #[test]
    fn uncovered_glyph_has_no_anchors() {
        let bytes = build(&[10], &[(Some((1, 2)), Some((3, 4)))]);
        let cp = CursivePos::parse(&bytes).unwrap();
        assert!(cp.entry(11).is_none());
        assert!(cp.exit(11).is_none());
        assert!(!cp.covers(11));
    }

    #[test]
    fn anchors_remember_their_subtable_position() {
        let bytes = build(&[10], &[(Some((1, 2)), Some((3, 4)))]);
        let cp = CursivePos::parse(&bytes).unwrap();
        let entry = cp.entry(10).unwrap();
        let exit = cp.exit(10).unwrap();
        assert!(entry.table_offset > 0);
        assert_eq!(exit.table_offset, entry.table_offset + 6);
        assert_eq!(cp.data().len(), bytes.len());
    }

    #[test]
    fn coverage_index_past_record_count_is_ignored() {
        // Two covered glyphs but only one record: the second glyph's
        // coverage index points past the record array.
        let mut bytes = build(&[10], &[(Some((1, 2)), None)]);
        // Rewrite the coverage in place to list [10, 11]: append a new
        // coverage table and repoint coverageOffset at it.
        let cov_off = bytes.len() as u16;
        bytes.extend_from_slice(&build_coverage_format1(&[10, 11]));
        bytes[2..4].copy_from_slice(&cov_off.to_be_bytes());
        let cp = CursivePos::parse(&bytes).unwrap();
        assert!(cp.entry(10).is_some());
        assert!(cp.entry(11).is_none());
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = build(&[10], &[(None, None)]);
        bytes[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert!(matches!(
            CursivePos::parse(&bytes),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_truncated_records() {
        let bytes = build(&[10, 20], &[(None, None), (None, None)]);
        // Header claims two records; cut the second one off.
        assert!(matches!(
            CursivePos::parse(&bytes[..8]),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn rejects_coverage_offset_past_end() {
        let mut bytes = build(&[10], &[(None, None)]);
        bytes[2..4].copy_from_slice(&0x0400u16.to_be_bytes());
        assert!(CursivePos::parse(&bytes).is_err());
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(CursivePos::parse(&[0x00, 0x01, 0x00]).is_err());
    }

    #[test]
    fn malformed_anchor_reads_as_absent() {
        let mut bytes = build(&[10], &[(Some((1, 2)), None)]);
        // Corrupt the anchor format word (it is the last 6 bytes).
        let n = bytes.len();
        bytes[n - 6..n - 4].copy_from_slice(&9u16.to_be_bytes());
        let cp = CursivePos::parse(&bytes).unwrap();
        assert!(cp.entry(10).is_none());
    }
}
