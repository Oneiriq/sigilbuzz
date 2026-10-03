//! OpenType Anchor table: `(x, y)` attachment point.
//!
//! Anchors are the glue for four GPOS lookup types: cursive
//! attachment (type 3), mark-to-base (type 4), mark-to-ligature
//! (type 5), and mark-to-mark (type 6). Each lookup holds a
//! MarkArray and a BaseArray (or the ligature/mark/entry-exit
//! equivalents); every element is an Anchor giving the `(x, y)`
//! point the glyph should snap to.
//!
//! # Formats
//!
//! ```text
//!   format 1:  u16 format=1, i16 x, i16 y
//!   format 2:  format 1 + u16 anchorPoint              (hinting, ignored)
//!   format 3:  format 1 + Offset16 xDeviceOffset, Offset16 yDeviceOffset
//! ```
//!
//! The format 2 contour point only matters to a hinting rasterizer
//! at a specific ppem, so it is read and discarded.
//!
//! The format 3 device offsets are relative to the start of the
//! Anchor table itself (not the enclosing subtable). Each one points
//! at either a `Device` table (per-ppem hinting deltas) or a
//! `VariationIndex` into GDEF's `ItemVariationStore`. sigilbuzz keeps
//! both offsets plus the anchor's own position inside the slice it
//! was parsed from, so [`Anchor::resolve`] can follow them later
//! against the font's variation coordinates. `VariationIndex` deltas
//! move the anchor with the design axes; plain `Device` tables
//! contribute nothing, because the shaper works in design units and
//! has no ppem (the same rule `ValueRecord` device slots follow).

use crate::error::{Error, Result};
use crate::tables::gpos::value_record::resolve_variation_delta;
use crate::tables::parse::Reader;
use crate::tables::variation_store::ItemVariationStore;

/// A parsed anchor: a design-unit `(x, y)` attachment point, plus the
/// optional device / variation slots of an AnchorFormat3 table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Anchor {
    /// X coordinate in font design units.
    pub x: i16,
    /// Y coordinate in font design units.
    pub y: i16,
    /// AnchorFormat3 `xDeviceOffset`: offset from the start of this
    /// Anchor table to a `Device` or `VariationIndex` table for `x`.
    /// Zero when absent, and always zero for formats 1 and 2.
    pub x_device_off: u16,
    /// AnchorFormat3 `yDeviceOffset`, same rules as `x_device_off`.
    pub y_device_off: u16,
    /// Byte position of this Anchor table inside the slice it was
    /// parsed from: the `offset` handed to [`Anchor::parse_at`], or
    /// zero for [`Anchor::parse`]. The device offsets are relative
    /// to this position.
    pub table_offset: usize,
}

impl Anchor {
    /// Parses an anchor from `data`. Accepts all three spec formats;
    /// the format 2 contour point is read and discarded, the format 3
    /// device offsets are kept for [`Anchor::resolve`].
    ///
    /// ```
    /// use sigilbuzz::tables::Anchor;
    ///
    /// // AnchorFormat1 at (120, -40).
    /// let bytes = [0x00, 0x01, 0x00, 0x78, 0xFF, 0xD8];
    /// let anchor = Anchor::parse(&bytes).unwrap();
    /// assert_eq!((anchor.x, anchor.y), (120, -40));
    /// assert_eq!(anchor.x_device_off, 0);
    /// ```
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        let x = r.read_i16()?;
        let y = r.read_i16()?;
        let mut anchor = Self {
            x,
            y,
            ..Self::default()
        };
        match format {
            1 => Ok(anchor),
            2 => {
                // u16 anchorPoint: contour index for hinting.
                let _ = r.read_u16()?;
                Ok(anchor)
            }
            3 => {
                anchor.x_device_off = r.read_u16()?;
                anchor.y_device_off = r.read_u16()?;
                Ok(anchor)
            }
            _ => Err(Error::Malformed {
                offset: 0,
                context: "unsupported anchor format",
            }),
        }
    }

    /// Parses the anchor at `offset` inside `data`. Convenience for
    /// callers that hold offsets relative to some enclosing table.
    /// The returned anchor remembers `offset` in
    /// [`Anchor::table_offset`] so [`Anchor::resolve`] can later
    /// follow its device offsets through the same `data` slice.
    pub fn parse_at(data: &[u8], offset: usize) -> Result<Self> {
        let slice = data.get(offset..).ok_or(Error::Malformed {
            offset,
            context: "anchor offset past end",
        })?;
        let mut anchor = Self::parse(slice).map_err(|e| match e {
            Error::Malformed { offset: o, context } => Error::Malformed {
                offset: offset.saturating_add(o),
                context,
            },
            Error::Truncated { offset: o, context } => Error::Truncated {
                offset: offset.saturating_add(o),
                context,
            },
            other => other,
        })?;
        anchor.table_offset = offset;
        Ok(anchor)
    }

    /// True when the anchor carries at least one device or variation
    /// slot (AnchorFormat3 with a non-null offset).
    #[must_use]
    pub const fn has_device(&self) -> bool {
        self.x_device_off != 0 || self.y_device_off != 0
    }

    /// Resolves the anchor to design-unit `(x, y)` coordinates at the
    /// given normalized variation `coords`.
    ///
    /// `data` must be the same slice the anchor was parsed from with
    /// [`Anchor::parse_at`] (for the GPOS attachment subtables that is
    /// the whole subtable), because the device offsets are relative to
    /// [`Anchor::table_offset`] inside it. `store` is GDEF's
    /// `ItemVariationStore`.
    ///
    /// `VariationIndex` deltas are evaluated at `coords` and rounded to
    /// the nearest integer exactly like `ValueRecord` device slots,
    /// halves up as in HarfBuzz.
    /// Plain `Device` (hinting) tables, a missing store, empty
    /// `coords`, and malformed or out-of-range device tables all
    /// contribute zero. The additions saturate instead of wrapping.
    ///
    /// ```
    /// use sigilbuzz::tables::Anchor;
    ///
    /// // AnchorFormat1: nothing to resolve, the static point comes back.
    /// let bytes = [0x00, 0x01, 0x00, 0x0A, 0x00, 0x14];
    /// let anchor = Anchor::parse(&bytes).unwrap();
    /// assert_eq!(anchor.resolve(&bytes, None, &[]), (10, 20));
    /// ```
    #[must_use]
    pub fn resolve(
        &self,
        data: &[u8],
        store: Option<&ItemVariationStore<'_>>,
        coords: &[f32],
    ) -> (i32, i32) {
        let x = i32::from(self.x);
        let y = i32::from(self.y);
        if !self.has_device() || store.is_none() || coords.is_empty() {
            return (x, y);
        }
        let Some(table) = data.get(self.table_offset..) else {
            return (x, y);
        };
        let dx = resolve_variation_delta(table, self.x_device_off, store, coords);
        let dy = resolve_variation_delta(table, self.y_device_off, store, coords);
        (x.saturating_add(dx), y.saturating_add(dy))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn build_format(format: u16, x: i16, y: i16, trailer: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&format.to_be_bytes());
        out.extend_from_slice(&x.to_be_bytes());
        out.extend_from_slice(&y.to_be_bytes());
        for t in trailer {
            out.extend_from_slice(&t.to_be_bytes());
        }
        out
    }

    fn xy(x: i16, y: i16) -> Anchor {
        Anchor {
            x,
            y,
            ..Anchor::default()
        }
    }

    /// Minimal ItemVariationStore: one region on axis 0 peaking at
    /// +1.0, one ItemVariationData with `rows` single-region i16
    /// deltas. Mirrors the layout `variation_store.rs` parses.
    fn build_ivs(rows: &[i16]) -> Vec<u8> {
        let mut out = Vec::new();
        // IVS header: format, regionListOffset (u32), dataCount,
        // itemVariationDataOffsets[1] (u32).
        let header_len = 2 + 4 + 2 + 4;
        let region_list_off = header_len as u32;
        // RegionList: axisCount=1, regionCount=1, one region
        // (start 0, peak 1.0, end 1.0) in F2Dot14.
        let mut region_list = Vec::new();
        region_list.extend_from_slice(&1u16.to_be_bytes());
        region_list.extend_from_slice(&1u16.to_be_bytes());
        region_list.extend_from_slice(&0i16.to_be_bytes());
        region_list.extend_from_slice(&0x4000i16.to_be_bytes());
        region_list.extend_from_slice(&0x4000i16.to_be_bytes());
        let data_off = region_list_off + region_list.len() as u32;
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&region_list_off.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&data_off.to_be_bytes());
        out.extend_from_slice(&region_list);
        // ItemVariationData: itemCount, wordDeltaCount=1 (one i16
        // column), regionIndexCount=1, regionIndexes[1]=0, rows.
        out.extend_from_slice(&(rows.len() as u16).to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        for r in rows {
            out.extend_from_slice(&r.to_be_bytes());
        }
        out
    }

    /// AnchorFormat3 followed by two VariationIndex tables. `x_row`
    /// and `y_row` select the IVS rows; `None` leaves that slot null.
    fn build_format3_varidx(x: i16, y: i16, x_row: Option<u16>, y_row: Option<u16>) -> Vec<u8> {
        // Anchor is 10 bytes; VariationIndex tables follow at 10 / 16.
        let x_off: u16 = if x_row.is_some() { 10 } else { 0 };
        let y_off: u16 = if y_row.is_some() { 16 } else { 0 };
        let mut out = build_format(3, x, y, &[x_off, y_off]);
        for row in [x_row, y_row] {
            // VariationIndex: outer, inner, deltaFormat = 0x8000.
            out.extend_from_slice(&0u16.to_be_bytes());
            out.extend_from_slice(&row.unwrap_or(0).to_be_bytes());
            out.extend_from_slice(&0x8000u16.to_be_bytes());
        }
        out
    }

    #[test]
    fn format1_reads_xy() {
        let bytes = build_format(1, 200, -50, &[]);
        let a = Anchor::parse(&bytes).unwrap();
        assert_eq!(a.x, 200);
        assert_eq!(a.y, -50);
        assert!(!a.has_device());
    }

    #[test]
    fn format2_ignores_contour_point() {
        // Contour point 42 must not change the anchor coordinates.
        let bytes = build_format(2, 10, 20, &[42]);
        let a = Anchor::parse(&bytes).unwrap();
        assert_eq!(a, xy(10, 20));
    }

    #[test]
    fn format3_captures_device_offsets() {
        let bytes = build_format(3, -7, 8, &[0xDEAD, 0xBEEF]);
        let a = Anchor::parse(&bytes).unwrap();
        assert_eq!((a.x, a.y), (-7, 8));
        assert_eq!(a.x_device_off, 0xDEAD);
        assert_eq!(a.y_device_off, 0xBEEF);
        assert_eq!(a.table_offset, 0);
        assert!(a.has_device());
    }

    #[test]
    fn format3_with_null_offsets_is_plain_point() {
        let bytes = build_format(3, 1, 2, &[0, 0]);
        let a = Anchor::parse(&bytes).unwrap();
        assert_eq!(a, xy(1, 2));
        assert!(!a.has_device());
    }

    #[test]
    fn format3_truncated_device_offsets_are_rejected() {
        // Format 3 needs 10 bytes; 8 leaves the y slot missing.
        let bytes = build_format(3, 1, 2, &[0x0010]);
        assert!(Anchor::parse(&bytes).is_err());
    }

    #[test]
    fn parse_at_slices_to_offset() {
        let mut bytes = alloc::vec![0u8; 8];
        bytes.extend_from_slice(&build_format(1, 100, 200, &[]));
        let a = Anchor::parse_at(&bytes, 8).unwrap();
        assert_eq!((a.x, a.y), (100, 200));
        assert_eq!(a.table_offset, 8);
    }

    #[test]
    fn parse_at_rejects_offset_past_end() {
        let bytes = build_format(1, 1, 2, &[]);
        assert!(Anchor::parse_at(&bytes, 99).is_err());
    }

    #[test]
    fn parse_at_reports_absolute_offset_on_truncation() {
        let mut bytes = alloc::vec![0u8; 4];
        bytes.extend_from_slice(&[0x00, 0x01, 0x00]);
        match Anchor::parse_at(&bytes, 4) {
            Err(Error::Truncated { offset, .. }) => assert!(offset >= 4),
            other => panic!("expected truncation error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_format() {
        let bytes = build_format(9, 0, 0, &[]);
        assert!(matches!(
            Anchor::parse(&bytes),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(Anchor::parse(&[0u8; 3]).is_err());
    }

    #[test]
    fn resolve_applies_variation_index_deltas() {
        let ivs_bytes = build_ivs(&[30, -12]);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let bytes = build_format3_varidx(100, 200, Some(0), Some(1));
        let a = Anchor::parse(&bytes).unwrap();
        // Full axis: full delta.
        assert_eq!(a.resolve(&bytes, Some(&store), &[1.0]), (130, 188));
        // Half way: 15 and -6.
        assert_eq!(a.resolve(&bytes, Some(&store), &[0.5]), (115, 194));
        // Default instance: no delta.
        assert_eq!(a.resolve(&bytes, Some(&store), &[0.0]), (100, 200));
    }

    #[test]
    fn resolve_rounds_halves_up_like_harfbuzz() {
        let ivs_bytes = build_ivs(&[5, -5]);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let bytes = build_format3_varidx(0, 0, Some(0), Some(1));
        let a = Anchor::parse(&bytes).unwrap();
        // 5 * 0.5 = 2.5 -> 3; -2.5 -> -2, as HarfBuzz's roundf
        // (floor(x + 0.5)) rounds it.
        assert_eq!(a.resolve(&bytes, Some(&store), &[0.5]), (3, -2));
    }

    #[test]
    fn resolve_follows_offsets_relative_to_the_anchor() {
        // Put 6 bytes of padding in front so the anchor sits at 6.
        // The device offsets inside it (10 / 16) must be read from the
        // anchor start, not from the start of `data`.
        let ivs_bytes = build_ivs(&[40, 0]);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let mut data = alloc::vec![0xFFu8; 6];
        data.extend_from_slice(&build_format3_varidx(10, 20, Some(0), None));
        let a = Anchor::parse_at(&data, 6).unwrap();
        assert_eq!(a.table_offset, 6);
        assert_eq!(a.resolve(&data, Some(&store), &[1.0]), (50, 20));
    }

    #[test]
    fn resolve_without_store_or_coords_is_static() {
        let ivs_bytes = build_ivs(&[30, 30]);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let bytes = build_format3_varidx(1, 2, Some(0), Some(1));
        let a = Anchor::parse(&bytes).unwrap();
        assert_eq!(a.resolve(&bytes, None, &[1.0]), (1, 2));
        assert_eq!(a.resolve(&bytes, Some(&store), &[]), (1, 2));
    }

    #[test]
    fn resolve_ignores_hinting_device_tables() {
        // Device table (deltaFormat 1) instead of a VariationIndex.
        let ivs_bytes = build_ivs(&[30]);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let mut bytes = build_format(3, 7, 9, &[10, 0]);
        bytes.extend_from_slice(&12u16.to_be_bytes()); // startSize
        bytes.extend_from_slice(&12u16.to_be_bytes()); // endSize
        bytes.extend_from_slice(&1u16.to_be_bytes()); // deltaFormat 1
        bytes.extend_from_slice(&0x1000u16.to_be_bytes()); // one packed delta
        let a = Anchor::parse(&bytes).unwrap();
        assert_eq!(a.resolve(&bytes, Some(&store), &[1.0]), (7, 9));
    }

    #[test]
    fn resolve_tolerates_truncated_device_table() {
        // xDeviceOffset points past the end: contributes zero, no panic.
        let ivs_bytes = build_ivs(&[30]);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let bytes = build_format(3, 4, 5, &[200, 8]);
        let a = Anchor::parse(&bytes).unwrap();
        assert_eq!(a.resolve(&bytes, Some(&store), &[1.0]), (4, 5));
        // A table_offset past the end of `data` also degrades to static.
        let far = Anchor {
            table_offset: 1000,
            ..a
        };
        assert_eq!(far.resolve(&bytes, Some(&store), &[1.0]), (4, 5));
    }

    #[test]
    fn resolve_widens_past_the_i16_range() {
        // The static point is i16, but the resolved coordinate is i32
        // so a large delta on an extreme anchor does not wrap.
        let ivs_bytes = build_ivs(&[i16::MAX]);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let bytes = build_format3_varidx(i16::MAX, 0, Some(0), None);
        let a = Anchor::parse(&bytes).unwrap();
        let (x, _) = a.resolve(&bytes, Some(&store), &[1.0]);
        assert_eq!(x, i32::from(i16::MAX) * 2);
    }
}
