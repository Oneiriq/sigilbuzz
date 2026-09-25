//! `SVG `: OpenType SVG table.
//!
//! A pre-COLRv1 color-glyph format: maps glyph ids to inline SVG XML
//! documents. Mostly seen in older Twitter / Mozilla emoji fonts and a
//! handful of designer color fonts; COLRv1 has largely displaced it,
//! but plenty of fonts in the wild still ship it (often alongside
//! COLR/CPAL as a fallback for SVG-aware renderers).
//!
//! sigilbuzz follows the same expose-bytes-not-pixels policy used by
//! `CBDT` / `sbix`: we surface the raw SVG payload (gzip-compressed or
//! plain) and a flag describing whether the bytes start with the gzip
//! magic. Decompression and SVG XML parsing are the consumer's job.
//! We do not pull `flate2`, `xml-rs`, `usvg`, or any other
//! heavyweight dep for this.
//!
//! # Format
//!
//! ```text
//!   SVG header
//!     0  u16     version             = 0
//!     2  Offset32 svgDocumentList    relative to start of SVG table
//!     6  u32     reserved            = 0 (ignored)
//!
//!   SVG Document List Index (at svgDocumentList)
//!     0  u16     numEntries
//!     2  SVGDocumentRecord[numEntries]
//!
//!   SVGDocumentRecord (12 bytes each)
//!     0  u16     startGlyphID        first gid in the inclusive range
//!     2  u16     endGlyphID          last gid in the inclusive range
//!     4  Offset32 svgDocOffset       relative to SVGDocumentList start
//!     8  u32     svgDocLength        bytes
//! ```
//!
//! Document payload is one of:
//! - Plain UTF-8 / ASCII SVG XML (often starts `<?xml` or `<svg`).
//! - gzip-compressed SVG: detect via the two-byte magic `1f 8b` at the
//!   payload's start.
//!
//! Records may overlap; the spec lets a single document cover a range
//! of glyph ids. sigilbuzz returns the first record whose range
//! contains the requested gid; ties go to the lowest index, matching
//! the OpenType recommendation that "documents earlier in the array
//! are preferred."

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// Two-byte gzip magic. RFC 1952 §2.3.1: every gzip stream begins with
/// `0x1f 0x8b`.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// One SVG document carved out of the `SVG ` table.
///
/// Borrows directly into the font blob (`data` is a slice, not a
/// copy), so a lookup costs a single bounds check.
#[derive(Debug, Clone, Copy)]
pub struct SvgDocument<'a> {
    /// First glyph id this document covers (inclusive).
    pub start_gid: u16,
    /// Last glyph id this document covers (inclusive).
    pub end_gid: u16,
    /// Raw payload bytes. Plain SVG XML or a gzip stream: the
    /// `gzipped` field tells which.
    pub data: &'a [u8],
    /// `true` when `data` begins with the gzip magic `1f 8b`. The
    /// consumer is responsible for decompression; sigilbuzz does not
    /// pull a gzip dep.
    pub gzipped: bool,
}

/// Parsed `SVG ` table.
#[derive(Debug, Clone, Copy)]
pub struct Svg<'a> {
    /// Whole-table slice. Document offsets in the index are relative
    /// to the document-list start, which itself sits at `list_off`
    /// bytes into this slice.
    data: &'a [u8],
    /// Absolute offset (within `data`) of the SVG Document List Index.
    list_off: usize,
    /// Number of entries in the document-list index.
    num_entries: u16,
    /// Slice covering exactly the `numEntries * 12` records, sliced
    /// for cheap indexing during lookup.
    records: &'a [u8],
}

/// Bytes per `SVGDocumentRecord` in the index.
const RECORD_LEN: usize = 12;

impl<'a> Svg<'a> {
    /// Parses an `SVG ` table.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.read_u16()?;
        if version != 0 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported SVG table version",
            });
        }
        let list_off = r.read_u32()? as usize;
        // u32 reserved; spec says ignore.
        let _reserved = r.read_u32()?;

        if list_off == 0 || list_off > data.len() {
            return Err(Error::Malformed {
                offset: 2,
                context: "SVG documentListOffset out of range",
            });
        }

        // Document List Index starts with u16 numEntries.
        let mut lr = Reader::at(data, list_off)?;
        let num_entries = lr.read_u16()?;

        let arr_start = lr.position();
        let arr_bytes = (num_entries as usize)
            .checked_mul(RECORD_LEN)
            .ok_or(Error::Malformed {
                offset: arr_start,
                context: "SVG document records overflow",
            })?;
        let arr_end = arr_start.checked_add(arr_bytes).ok_or(Error::Malformed {
            offset: arr_start,
            context: "SVG document records overflow",
        })?;
        if arr_end > data.len() {
            return Err(Error::Truncated {
                offset: arr_end,
                context: "SVG document records",
            });
        }

        Ok(Self {
            data,
            list_off,
            num_entries,
            records: &data[arr_start..arr_end],
        })
    }

    /// Number of `SVGDocumentRecord` entries.
    #[must_use]
    pub fn num_entries(&self) -> u16 {
        self.num_entries
    }

    /// Returns the document for `gid`, or `None` when no record's
    /// range covers it.
    ///
    /// Records may overlap; the first record whose `[start_gid,
    /// end_gid]` covers `gid` wins, matching the OpenType
    /// "earlier-is-preferred" rule.
    #[must_use]
    pub fn document_for(&self, gid: u16) -> Option<SvgDocument<'a>> {
        for i in 0..self.num_entries as usize {
            // Records are bound-checked at parse time, so each 12-byte
            // window is in-range.
            let off = i * RECORD_LEN;
            let rec = &self.records[off..off + RECORD_LEN];
            let start_gid = u16::from_be_bytes([rec[0], rec[1]]);
            let end_gid = u16::from_be_bytes([rec[2], rec[3]]);
            if gid < start_gid || gid > end_gid {
                continue;
            }
            let doc_off = u32::from_be_bytes([rec[4], rec[5], rec[6], rec[7]]) as usize;
            let doc_len = u32::from_be_bytes([rec[8], rec[9], rec[10], rec[11]]) as usize;
            let abs_start = self.list_off.checked_add(doc_off)?;
            let abs_end = abs_start.checked_add(doc_len)?;
            if abs_end > self.data.len() {
                // Malformed record: skip rather than panic; a later
                // record might still be well-formed for this gid.
                continue;
            }
            let payload = &self.data[abs_start..abs_end];
            let gzipped = payload.len() >= 2 && payload[0..2] == GZIP_MAGIC;
            return Some(SvgDocument {
                start_gid,
                end_gid,
                data: payload,
                gzipped,
            });
        }
        None
    }

    /// Iterates every record in directory order, yielding the parsed
    /// `SvgDocument`. Records that fail bounds checks are skipped so
    /// one malformed entry doesn't blind callers to its siblings.
    pub fn documents(&self) -> impl Iterator<Item = SvgDocument<'a>> + '_ {
        (0..self.num_entries as usize).filter_map(move |i| {
            let off = i * RECORD_LEN;
            let rec = self.records.get(off..off + RECORD_LEN)?;
            let start_gid = u16::from_be_bytes([rec[0], rec[1]]);
            let end_gid = u16::from_be_bytes([rec[2], rec[3]]);
            let doc_off = u32::from_be_bytes([rec[4], rec[5], rec[6], rec[7]]) as usize;
            let doc_len = u32::from_be_bytes([rec[8], rec[9], rec[10], rec[11]]) as usize;
            let abs_start = self.list_off.checked_add(doc_off)?;
            let abs_end = abs_start.checked_add(doc_len)?;
            if abs_end > self.data.len() {
                return None;
            }
            let payload = &self.data[abs_start..abs_end];
            let gzipped = payload.len() >= 2 && payload[0..2] == GZIP_MAGIC;
            Some(SvgDocument {
                start_gid,
                end_gid,
                data: payload,
                gzipped,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Builds a minimal `SVG ` table containing the supplied
    /// `(start_gid, end_gid, payload)` records, in order.
    fn build(records: &[(u16, u16, &[u8])]) -> Vec<u8> {
        // Header: 10 bytes. Document list immediately after.
        let mut out = Vec::new();
        out.extend_from_slice(&0u16.to_be_bytes()); // version
        out.extend_from_slice(&10u32.to_be_bytes()); // documentListOffset
        out.extend_from_slice(&0u32.to_be_bytes()); // reserved

        // Document List Index at offset 10.
        let list_start = out.len();
        assert_eq!(list_start, 10);
        out.extend_from_slice(&(records.len() as u16).to_be_bytes());

        // Reserve record array.
        let records_pos = out.len();
        out.resize(records_pos + records.len() * RECORD_LEN, 0);

        // Append payloads, recording offsets relative to list_start.
        let mut entries = Vec::with_capacity(records.len());
        for (start_gid, end_gid, payload) in records {
            let doc_off = (out.len() - list_start) as u32;
            let doc_len = payload.len() as u32;
            out.extend_from_slice(payload);
            entries.push((*start_gid, *end_gid, doc_off, doc_len));
        }

        // Patch records.
        for (i, (s, e, off, len)) in entries.iter().enumerate() {
            let dst = records_pos + i * RECORD_LEN;
            out[dst..dst + 2].copy_from_slice(&s.to_be_bytes());
            out[dst + 2..dst + 4].copy_from_slice(&e.to_be_bytes());
            out[dst + 4..dst + 8].copy_from_slice(&off.to_be_bytes());
            out[dst + 8..dst + 12].copy_from_slice(&len.to_be_bytes());
        }

        out
    }

    const TINY_SVG: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><circle r=\"5\"/></svg>";

    #[test]
    fn parses_header_and_record_count() {
        let blob = build(&[(1, 1, TINY_SVG)]);
        let svg = Svg::parse(&blob).unwrap();
        assert_eq!(svg.num_entries(), 1);
    }

    #[test]
    fn document_for_returns_payload_in_range() {
        let blob = build(&[(1, 1, TINY_SVG)]);
        let svg = Svg::parse(&blob).unwrap();
        let doc = svg.document_for(1).expect("gid 1 in range");
        assert_eq!(doc.start_gid, 1);
        assert_eq!(doc.end_gid, 1);
        assert!(!doc.gzipped);
        assert_eq!(doc.data, TINY_SVG);
    }

    #[test]
    fn document_for_returns_none_outside_range() {
        let blob = build(&[(2, 4, TINY_SVG)]);
        let svg = Svg::parse(&blob).unwrap();
        assert!(svg.document_for(0).is_none());
        assert!(svg.document_for(1).is_none());
        assert!(svg.document_for(2).is_some());
        assert!(svg.document_for(3).is_some());
        assert!(svg.document_for(4).is_some());
        assert!(svg.document_for(5).is_none());
    }

    #[test]
    fn detects_gzip_magic() {
        // Gzip stream: `1f 8b` + arbitrary tail.
        let gz: &[u8] = &[0x1f, 0x8b, 0x08, 0x00, 0xde, 0xad, 0xbe, 0xef];
        let blob = build(&[(7, 7, gz)]);
        let svg = Svg::parse(&blob).unwrap();
        let doc = svg.document_for(7).unwrap();
        assert!(doc.gzipped);
        assert_eq!(doc.data, gz);
    }

    #[test]
    fn does_not_flag_short_payload_as_gzip() {
        // 1 byte can't carry the 2-byte magic.
        let blob = build(&[(0, 0, &[0x1f])]);
        let svg = Svg::parse(&blob).unwrap();
        let doc = svg.document_for(0).unwrap();
        assert!(!doc.gzipped);
    }

    #[test]
    fn first_matching_record_wins_on_overlap() {
        // Two records covering gid 5; first one has different bytes.
        let first: &[u8] = b"<svg id=\"first\"/>";
        let second: &[u8] = b"<svg id=\"second\"/>";
        let blob = build(&[(0, 9, first), (5, 5, second)]);
        let svg = Svg::parse(&blob).unwrap();
        let doc = svg.document_for(5).unwrap();
        assert_eq!(doc.data, first);
    }

    #[test]
    fn multiple_records_resolve_to_correct_payload() {
        let a: &[u8] = b"<svg id=\"a\"/>";
        let b: &[u8] = b"<svg id=\"b\"/>";
        let c: &[u8] = b"<svg id=\"c\"/>";
        let blob = build(&[(1, 1, a), (2, 2, b), (3, 5, c)]);
        let svg = Svg::parse(&blob).unwrap();
        assert_eq!(svg.document_for(1).unwrap().data, a);
        assert_eq!(svg.document_for(2).unwrap().data, b);
        assert_eq!(svg.document_for(3).unwrap().data, c);
        assert_eq!(svg.document_for(4).unwrap().data, c);
        assert_eq!(svg.document_for(5).unwrap().data, c);
    }

    #[test]
    fn documents_iterator_yields_each_record() {
        let a: &[u8] = b"<svg id=\"a\"/>";
        let b: &[u8] = b"<svg id=\"b\"/>";
        let blob = build(&[(0, 0, a), (1, 3, b)]);
        let svg = Svg::parse(&blob).unwrap();
        let docs: Vec<_> = svg.documents().collect();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].start_gid, 0);
        assert_eq!(docs[0].end_gid, 0);
        assert_eq!(docs[0].data, a);
        assert_eq!(docs[1].start_gid, 1);
        assert_eq!(docs[1].end_gid, 3);
        assert_eq!(docs[1].data, b);
    }

    #[test]
    fn empty_document_list_parses() {
        let blob = build(&[]);
        let svg = Svg::parse(&blob).unwrap();
        assert_eq!(svg.num_entries(), 0);
        assert!(svg.document_for(0).is_none());
    }

    #[test]
    fn rejects_bad_version() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&7u16.to_be_bytes()); // bad version
        blob.extend_from_slice(&10u32.to_be_bytes());
        blob.extend_from_slice(&0u32.to_be_bytes());
        let err = Svg::parse(&blob).unwrap_err();
        assert!(matches!(err, Error::Malformed { .. }));
    }

    #[test]
    fn rejects_zero_document_list_offset() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&0u16.to_be_bytes());
        blob.extend_from_slice(&0u32.to_be_bytes()); // zero list offset
        blob.extend_from_slice(&0u32.to_be_bytes());
        let err = Svg::parse(&blob).unwrap_err();
        assert!(matches!(err, Error::Malformed { .. }));
    }

    #[test]
    fn rejects_truncated_records() {
        // Header claims numEntries=2 but only 1 record fits.
        let mut blob = Vec::new();
        blob.extend_from_slice(&0u16.to_be_bytes());
        blob.extend_from_slice(&10u32.to_be_bytes());
        blob.extend_from_slice(&0u32.to_be_bytes());
        blob.extend_from_slice(&2u16.to_be_bytes()); // numEntries
        blob.extend_from_slice(&[0u8; RECORD_LEN]); // only one record's worth
        let err = Svg::parse(&blob).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn malformed_record_offset_is_skipped() {
        // Hand-build a table with one well-formed and one malformed
        // record; the malformed one points past end-of-table.
        let mut blob = Vec::new();
        blob.extend_from_slice(&0u16.to_be_bytes()); // version
        blob.extend_from_slice(&10u32.to_be_bytes()); // listOff
        blob.extend_from_slice(&0u32.to_be_bytes()); // reserved

        // Document list at offset 10.
        blob.extend_from_slice(&2u16.to_be_bytes()); // numEntries
        let records_pos = blob.len();
        blob.extend_from_slice(&[0u8; RECORD_LEN * 2]);
        let payload_off = (blob.len() - 10) as u32;
        let payload: &[u8] = b"<svg/>";
        blob.extend_from_slice(payload);

        // Record 0: gid 1, well-formed.
        let r0 = records_pos;
        blob[r0..r0 + 2].copy_from_slice(&1u16.to_be_bytes());
        blob[r0 + 2..r0 + 4].copy_from_slice(&1u16.to_be_bytes());
        blob[r0 + 4..r0 + 8].copy_from_slice(&payload_off.to_be_bytes());
        blob[r0 + 8..r0 + 12].copy_from_slice(&(payload.len() as u32).to_be_bytes());

        // Record 1: gid 2, points past end.
        let r1 = records_pos + RECORD_LEN;
        blob[r1..r1 + 2].copy_from_slice(&2u16.to_be_bytes());
        blob[r1 + 2..r1 + 4].copy_from_slice(&2u16.to_be_bytes());
        blob[r1 + 4..r1 + 8].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        blob[r1 + 8..r1 + 12].copy_from_slice(&1u32.to_be_bytes());

        let svg = Svg::parse(&blob).unwrap();
        assert_eq!(svg.document_for(1).unwrap().data, payload);
        // Malformed record for gid 2 returns None instead of erroring.
        assert!(svg.document_for(2).is_none());
    }
}
