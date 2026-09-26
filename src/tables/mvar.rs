//! `MVAR`: Metrics Variations.
//!
//! Carries deltas for *font-wide* instance metrics (typo
//! ascender / descender, x-height, sub/super script offsets,
//! strikeout, underline, gasp ranges, ...) so a renderer can
//! interpolate them away from the font's default instance.
//!
//! Unlike HVAR (per-glyph advance widths) MVAR is consulted once
//! per layout: every record names a 4-byte metric tag and a
//! `(outer, inner)` index into the shared
//! [`ItemVariationStore`](super::variation_store::ItemVariationStore).
//! sigilbuzz exposes the parsed records and a `metric_delta`
//! helper; consumers wire individual metrics to their own
//! design-unit fields (the shape pipeline doesn't read these).
//!
//! # Layout
//!
//! ```text
//!   u16       majorVersion = 1
//!   u16       minorVersion = 0
//!   u16       reserved
//!   u16       valueRecordSize    must be >= 8
//!   u16       valueRecordCount
//!   Offset16  itemVariationStoreOffset
//!   ValueRecord valueRecords[valueRecordCount]
//! ```
//!
//! Each `ValueRecord` is at least:
//!
//! ```text
//!   Tag  valueTag                    metric identifier (e.g. b"hasc")
//!   u16  deltaSetOuterIndex
//!   u16  deltaSetInnerIndex
//! ```
//!
//! `valueRecordSize` is variable so future spec revisions can pad
//! records without breaking older parsers; sigilbuzz reads exactly
//! the eight bytes it cares about and skips the rest.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;
use crate::tables::variation_store::ItemVariationStore;

/// A parsed `MVAR` table.
#[derive(Debug, Clone)]
pub struct Mvar<'a> {
    data: &'a [u8],
    store: Option<ItemVariationStore<'a>>,
    records_off: usize,
    record_size: u16,
    record_count: u16,
}

impl<'a> Mvar<'a> {
    /// Parses an `MVAR` table.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported MVAR major version",
            });
        }
        let _reserved = r.read_u16()?;
        let record_size = r.read_u16()?;
        let record_count = r.read_u16()?;
        let store_off = r.read_u16()? as usize;

        // valueRecordSize must accommodate at least the 8 bytes
        // sigilbuzz reads (tag + outer + inner). A smaller value
        // would be a malformed font.
        if record_count > 0 && (record_size as usize) < 8 {
            return Err(Error::Malformed {
                offset: 6,
                context: "MVAR valueRecordSize < 8",
            });
        }

        let records_off = r.position();
        // Bounds-check the records array up front so the iterator
        // never has to.
        let need = (record_count as usize)
            .checked_mul(record_size as usize)
            .ok_or(Error::Malformed {
                offset: records_off,
                context: "MVAR records overflow",
            })?;
        if data.len() < records_off + need {
            return Err(Error::Truncated {
                offset: records_off,
                context: "MVAR records truncated",
            });
        }

        // Per spec: itemVariationStoreOffset == 0 means the table
        // carries records but no variation store, so every record
        // resolves to a delta of 0. Real fonts always set this when
        // record_count > 0, but a defensive parser handles the
        // optional case rather than rejecting.
        let store = if store_off == 0 {
            None
        } else {
            Some(ItemVariationStore::parse(data.get(store_off..).ok_or(
                Error::Malformed {
                    offset: store_off,
                    context: "MVAR itemVariationStore offset past end",
                },
            )?)?)
        };

        Ok(Self {
            data,
            store,
            records_off,
            record_size,
            record_count,
        })
    }

    /// Number of value records carried by this table.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.record_count
    }

    /// Whether the table carries no records.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.record_count == 0
    }

    /// Borrowed reference to the underlying [`ItemVariationStore`],
    /// or `None` when the table carried no store offset (i.e. when
    /// every record evaluates to zero).
    #[must_use]
    pub fn variation_store(&self) -> Option<&ItemVariationStore<'a>> {
        self.store.as_ref()
    }

    /// Iterates over `(tag, (outer, inner))` for every record. The
    /// iterator is cheap: it slices into the original blob and
    /// decodes one record at a time.
    pub fn entries(&self) -> impl Iterator<Item = ([u8; 4], (u16, u16))> + '_ {
        (0..self.record_count).filter_map(move |i| self.record_at(i))
    }

    fn record_at(&self, index: u16) -> Option<([u8; 4], (u16, u16))> {
        if index >= self.record_count {
            return None;
        }
        let off = self.records_off + index as usize * self.record_size as usize;
        let bytes = self.data.get(off..off + 8)?;
        let tag = [bytes[0], bytes[1], bytes[2], bytes[3]];
        let outer = u16::from_be_bytes([bytes[4], bytes[5]]);
        let inner = u16::from_be_bytes([bytes[6], bytes[7]]);
        Some((tag, (outer, inner)))
    }

    /// Looks up the `(outer, inner)` pair for a metric tag using a
    /// linear scan. Records appear in tag-sorted order in valid
    /// fonts; sigilbuzz doesn't rely on that. The tables are tiny
    /// (a few dozen entries at most).
    fn lookup(&self, tag: [u8; 4]) -> Option<(u16, u16)> {
        for i in 0..self.record_count {
            let (rec_tag, idx) = self.record_at(i)?;
            if rec_tag == tag {
                return Some(idx);
            }
        }
        None
    }

    /// Returns the design-unit delta for the named metric at the
    /// given normalized axis coords, or `None` when the table
    /// doesn't carry a record for `tag`.
    ///
    /// The variation store this MVAR points at is consulted via
    /// [`Self::variation_store`]. Callers needn't pass it
    /// explicitly. (Keeping the parameter list to a single
    /// `(tag, coords)` pair matches HVAR's surface and avoids
    /// mismatching stores.)
    #[must_use]
    pub fn metric_delta(&self, tag: [u8; 4], coords: &[f32]) -> Option<f32> {
        let (outer, inner) = self.lookup(tag)?;
        let store = self.store.as_ref()?;
        Some(store.delta(outer, inner, coords))
    }
}

/// Standard four-byte metric tags carried by `MVAR`. The full list
/// is enumerated in the OpenType spec; sigilbuzz exposes the ones
/// most consumers reach for so they don't have to type the byte
/// literals.
pub mod tag {
    /// `hasc`: OS/2 typoAscender.
    pub const HORIZ_ASCENDER: [u8; 4] = *b"hasc";
    /// `hdsc`: OS/2 typoDescender.
    pub const HORIZ_DESCENDER: [u8; 4] = *b"hdsc";
    /// `hlgp`: OS/2 typoLineGap.
    pub const HORIZ_LINE_GAP: [u8; 4] = *b"hlgp";
    /// `hcla`: OS/2 winAscent.
    pub const HORIZ_CLIPPING_ASCENT: [u8; 4] = *b"hcla";
    /// `hcld`: OS/2 winDescent.
    pub const HORIZ_CLIPPING_DESCENT: [u8; 4] = *b"hcld";
    /// `vasc`: vhea ascent.
    pub const VERT_ASCENDER: [u8; 4] = *b"vasc";
    /// `vdsc`: vhea descent.
    pub const VERT_DESCENDER: [u8; 4] = *b"vdsc";
    /// `vlgp`: vhea line gap.
    pub const VERT_LINE_GAP: [u8; 4] = *b"vlgp";
    /// `xhgt`: OS/2 sxHeight.
    pub const X_HEIGHT: [u8; 4] = *b"xhgt";
    /// `cpht`: OS/2 sCapHeight.
    pub const CAP_HEIGHT: [u8; 4] = *b"cpht";
    /// `sbxs`: OS/2 ySubscriptXSize.
    pub const SUBSCRIPT_X_SIZE: [u8; 4] = *b"sbxs";
    /// `sbys`: OS/2 ySubscriptYSize.
    pub const SUBSCRIPT_Y_SIZE: [u8; 4] = *b"sbys";
    /// `sbxo`: OS/2 ySubscriptXOffset.
    pub const SUBSCRIPT_X_OFFSET: [u8; 4] = *b"sbxo";
    /// `sbyo`: OS/2 ySubscriptYOffset.
    pub const SUBSCRIPT_Y_OFFSET: [u8; 4] = *b"sbyo";
    /// `spxs`: OS/2 ySuperscriptXSize.
    pub const SUPERSCRIPT_X_SIZE: [u8; 4] = *b"spxs";
    /// `spys`: OS/2 ySuperscriptYSize.
    pub const SUPERSCRIPT_Y_SIZE: [u8; 4] = *b"spys";
    /// `spxo`: OS/2 ySuperscriptXOffset.
    pub const SUPERSCRIPT_X_OFFSET: [u8; 4] = *b"spxo";
    /// `spyo`: OS/2 ySuperscriptYOffset.
    pub const SUPERSCRIPT_Y_OFFSET: [u8; 4] = *b"spyo";
    /// `strs`: OS/2 yStrikeoutSize.
    pub const STRIKEOUT_SIZE: [u8; 4] = *b"strs";
    /// `stro`: OS/2 yStrikeoutPosition.
    pub const STRIKEOUT_OFFSET: [u8; 4] = *b"stro";
    /// `unds`: post underlineThickness.
    pub const UNDERLINE_SIZE: [u8; 4] = *b"unds";
    /// `undo`: post underlinePosition.
    pub const UNDERLINE_OFFSET: [u8; 4] = *b"undo";
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
        let raw = (v * 16384.0).round() as i16;
        out.extend_from_slice(&raw.to_be_bytes());
    }

    /// A minimal one-axis, one-region, one-subtable, one-item store.
    /// Mirrors the helper used in hvar.rs tests.
    fn build_ivs_one_item(delta: i16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        let region_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // subtable count
        let subtable_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());

        let region_start = out.len() as u32;
        out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_start.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
        out.extend_from_slice(&1u16.to_be_bytes()); // regionCount
        write_f2dot14(&mut out, 0.0);
        write_f2dot14(&mut out, 1.0);
        write_f2dot14(&mut out, 1.0);

        let sub_start = out.len() as u32;
        out[subtable_slot..subtable_slot + 4].copy_from_slice(&sub_start.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // itemCount
        out.extend_from_slice(&1u16.to_be_bytes()); // wordDeltaCount
        out.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
        out.extend_from_slice(&0u16.to_be_bytes()); // region index 0
        out.extend_from_slice(&delta.to_be_bytes()); // single delta

        out
    }

    /// Build an MVAR table with `records` and an embedded IVS that
    /// produces `delta` at coord 1.0. All records share the same
    /// (outer=0, inner=0) item by default; overrides go through the
    /// record list directly.
    fn build_mvar(records: &[([u8; 4], u16, u16)], delta: i16) -> Vec<u8> {
        let mut out = Vec::new();
        // Header: 12 bytes total (4 ver/reserved + 2 size + 2 count
        // + 2 + 2 store off, actually 6+2+2+2 = 12).
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
        out.extend_from_slice(&0u16.to_be_bytes()); // reserved
        out.extend_from_slice(&8u16.to_be_bytes()); // valueRecordSize = 8
        out.extend_from_slice(&(records.len() as u16).to_be_bytes());
        let store_off_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // store offset placeholder

        for (tag, outer, inner) in records {
            out.extend_from_slice(tag);
            out.extend_from_slice(&outer.to_be_bytes());
            out.extend_from_slice(&inner.to_be_bytes());
        }

        let store_off = out.len() as u16;
        out[store_off_slot..store_off_slot + 2].copy_from_slice(&store_off.to_be_bytes());
        out.extend_from_slice(&build_ivs_one_item(delta));

        out
    }

    #[test]
    fn parse_returns_record_count() {
        let data = build_mvar(&[(*b"hasc", 0, 0), (*b"hdsc", 0, 0)], 0);
        let mvar = Mvar::parse(&data).unwrap();
        assert_eq!(mvar.len(), 2);
        assert!(!mvar.is_empty());
    }

    #[test]
    fn entries_iterates_in_record_order() {
        let data = build_mvar(&[(*b"hasc", 0, 0), (*b"xhgt", 0, 0)], 0);
        let mvar = Mvar::parse(&data).unwrap();
        let collected: Vec<_> = mvar.entries().collect();
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].0, *b"hasc");
        assert_eq!(collected[1].0, *b"xhgt");
    }

    #[test]
    fn metric_delta_resolves_known_tag() {
        let data = build_mvar(&[(*b"hasc", 0, 0)], 75);
        let mvar = Mvar::parse(&data).unwrap();
        let d = mvar.metric_delta(*b"hasc", &[1.0]).unwrap();
        assert!((d - 75.0).abs() < 1e-3);
        // At coord 0.5 the IVS region (0...1...1) gives scalar 0.5.
        let d = mvar.metric_delta(*b"hasc", &[0.5]).unwrap();
        assert!((d - 37.5).abs() < 1e-3);
    }

    #[test]
    fn metric_delta_unknown_tag_returns_none() {
        let data = build_mvar(&[(*b"hasc", 0, 0)], 50);
        let mvar = Mvar::parse(&data).unwrap();
        assert!(mvar.metric_delta(*b"xxxx", &[1.0]).is_none());
    }

    #[test]
    fn rejects_unsupported_major_version() {
        let mut data = build_mvar(&[(*b"hasc", 0, 0)], 0);
        data[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert!(matches!(Mvar::parse(&data), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_record_size_below_minimum() {
        let mut data = build_mvar(&[(*b"hasc", 0, 0)], 0);
        // valueRecordSize lives at offset 6.
        data[6..8].copy_from_slice(&4u16.to_be_bytes());
        assert!(matches!(Mvar::parse(&data), Err(Error::Malformed { .. })));
    }

    /// MVAR's `valueRecordSize` is allowed to exceed 8 bytes when a
    /// future spec revision pads the record. The decoder must skip
    /// the trailing slack rather than treating it as the next
    /// record.
    #[test]
    fn record_size_larger_than_eight_skips_padding() {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&12u16.to_be_bytes()); // record size = 12 (8 + 4 padding)
        out.extend_from_slice(&2u16.to_be_bytes()); // 2 records
        let store_off_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());

        // Two records, each padded to 12 bytes.
        out.extend_from_slice(b"hasc");
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&[0xAB, 0xCD, 0xEF, 0x42]); // padding
        out.extend_from_slice(b"xhgt");
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&[0; 4]);

        let store_off = out.len() as u16;
        out[store_off_slot..store_off_slot + 2].copy_from_slice(&store_off.to_be_bytes());
        out.extend_from_slice(&build_ivs_one_item(99));

        let mvar = Mvar::parse(&out).unwrap();
        let entries: Vec<_> = mvar.entries().collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, *b"hasc");
        assert_eq!(entries[1].0, *b"xhgt");
        let d = mvar.metric_delta(*b"xhgt", &[1.0]).unwrap();
        assert!((d - 99.0).abs() < 1e-3);
    }

    /// A zero `itemVariationStoreOffset` is the spec-defined "no
    /// store" case. Records still iterate but every delta is zero.
    /// This is rare in the wild but valid.
    #[test]
    fn zero_store_offset_yields_no_store() {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&8u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // store offset = 0
        out.extend_from_slice(b"hasc");
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());

        let mvar = Mvar::parse(&out).unwrap();
        assert!(mvar.variation_store().is_none());
        assert!(mvar.metric_delta(*b"hasc", &[1.0]).is_none());
    }
}
