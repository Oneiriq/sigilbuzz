//! `HVAR`: Horizontal Metrics Variations.
//!
//! Carries per-glyph advance-width deltas for a variable font.
//! The shaper needs this because `hmtx` only holds the advances
//! at the font's default instance; at any other axis coordinate
//! the real advance is `hmtx.advance(gid) + HVAR.advance_delta(gid, coords)`.
//!
//! # Layout
//!
//! ```text
//!   u16       majorVersion = 1
//!   u16       minorVersion = 0
//!   Offset32  itemVariationStoreOffset
//!   Offset32  advanceWidthMappingOffset    (may be 0: use gid directly)
//!   Offset32  lsbMappingOffset             (optional, unused here)
//!   Offset32  rsbMappingOffset             (optional, unused here)
//! ```
//!
//! When `advanceWidthMappingOffset` is non-zero it points at a
//! `DeltaSetIndexMap`, a glyph-id -> `(outer, inner)` table. When
//! zero, the glyph id *is* the inner index with outer = 0. The
//! mapping table uses `entryFormat` to pack `(outer, inner)` into
//! a variable number of bytes per entry.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;
use crate::tables::variation_store::ItemVariationStore;

/// A parsed `HVAR` table.
#[derive(Debug, Clone)]
pub struct Hvar<'a> {
    data: &'a [u8],
    store: ItemVariationStore<'a>,
    advance_map_off: u32,
}

impl<'a> Hvar<'a> {
    /// Parses an `HVAR` table.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported HVAR major version",
            });
        }
        let store_off = r.read_u32()? as usize;
        let advance_map_off = r.read_u32()?;
        let _lsb_off = r.read_u32()?;
        let _rsb_off = r.read_u32()?;

        let store = ItemVariationStore::parse(data.get(store_off..).ok_or(Error::Malformed {
            offset: store_off,
            context: "HVAR itemVariationStore offset past end",
        })?)?;

        Ok(Self {
            data,
            store,
            advance_map_off,
        })
    }

    /// Returns the advance-width delta for `glyph_id` at the
    /// given normalized coordinates. The caller adds this (as a
    /// design-unit integer) to the base `hmtx` advance. Returns
    /// `0.0` when the glyph has no entry, equivalent to "no
    /// variation applies".
    #[must_use]
    pub fn advance_delta(&self, glyph_id: u16, coords: &[f32]) -> f32 {
        let (outer, inner) = if self.advance_map_off == 0 {
            (0, glyph_id)
        } else {
            match read_index_map(self.data, self.advance_map_off as usize, glyph_id) {
                Some(v) => v,
                None => return 0.0,
            }
        };
        self.store.delta(outer, inner, coords)
    }
}

/// Reads a `(outer, inner)` index pair from a `DeltaSetIndexMap`.
///
/// Both the `format 0` (u16 mapCount) and `format 1` (u32
/// mapCount) layouts are supported. `entryFormat` encodes the
/// bytes per entry and the split between outer and inner bits.
fn read_index_map(data: &[u8], start: usize, glyph_id: u16) -> Option<(u16, u16)> {
    // Slice instead of adding to `start`, so a huge offset cannot
    // overflow on 32-bit targets.
    let (&format, rest) = data.get(start..)?.split_first()?;
    let (&entry_format, rest) = rest.split_first()?;

    let (map_count, entries) = match format {
        0 => {
            let (count, entries) = rest.split_first_chunk::<2>()?;
            (u32::from(u16::from_be_bytes(*count)), entries)
        }
        1 => {
            let (count, entries) = rest.split_first_chunk::<4>()?;
            (u32::from_be_bytes(*count), entries)
        }
        _ => return None,
    };

    // A DeltaSetIndexMap with no entries cannot produce a valid
    // (outer, inner) pair; treat it as "glyph has no variation
    // mapping" so the caller emits a zero delta. Without this
    // guard, `map_count.saturating_sub(1)` quietly collapses to 0
    // and the decoder reads the first entry_bytes after the header
    // as if they were a real entry, which they are not.
    if map_count == 0 {
        return None;
    }

    // entryFormat bits 4-5: one less than the number of bytes per
    // mapping entry (1..=4). Bits 0-3: one less than the number
    // of inner-index bits.
    let entry_bytes = ((entry_format >> 4) & 0x03) as usize + 1;
    let inner_bits = (entry_format & 0x0F) as u32 + 1;
    let inner_mask: u32 = (1u32 << inner_bits) - 1;

    // Clamp glyph id: spec says overruns map to the last entry.
    let idx = if (glyph_id as u32) < map_count {
        glyph_id as u32
    } else {
        map_count.saturating_sub(1)
    } as usize;
    let entry = entries
        .get(idx.checked_mul(entry_bytes)?..)?
        .get(..entry_bytes)?;

    let raw = entry.iter().fold(0u32, |raw, &b| (raw << 8) | u32::from(b));
    let inner = (raw & inner_mask) as u16;
    let outer = (raw >> inner_bits) as u16;
    Some((outer, inner))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
        let raw = (v * 16384.0).round() as i16;
        out.extend_from_slice(&raw.to_be_bytes());
    }

    fn build_ivs_one_axis_one_region_one_item(delta: i16) -> Vec<u8> {
        // ItemVariationStore: format 1, one region, one subtable,
        // one item with one delta.
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        let region_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // subtable count
        let subtable_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());

        // Region list.
        let region_start = out.len() as u32;
        out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_start.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
        out.extend_from_slice(&1u16.to_be_bytes()); // regionCount
        write_f2dot14(&mut out, 0.0);
        write_f2dot14(&mut out, 1.0);
        write_f2dot14(&mut out, 1.0);

        // Subtable.
        let sub_start = out.len() as u32;
        out[subtable_slot..subtable_slot + 4].copy_from_slice(&sub_start.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // itemCount
        out.extend_from_slice(&1u16.to_be_bytes()); // wordDeltaCount (all word-sized, short)
        out.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
        out.extend_from_slice(&0u16.to_be_bytes()); // region index 0
        out.extend_from_slice(&delta.to_be_bytes()); // single delta

        out
    }

    fn build_hvar_without_map(ivs: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&20u32.to_be_bytes()); // ivs offset = header length
        out.extend_from_slice(&0u32.to_be_bytes()); // no advance map
        out.extend_from_slice(&0u32.to_be_bytes()); // no lsb map
        out.extend_from_slice(&0u32.to_be_bytes()); // no rsb map
        out.extend_from_slice(ivs);
        out
    }

    #[test]
    fn advance_delta_uses_gid_as_inner_without_map() {
        let ivs = build_ivs_one_axis_one_region_one_item(50);
        let bytes = build_hvar_without_map(&ivs);
        let hvar = Hvar::parse(&bytes).unwrap();
        // Only one item exists, at inner=0. GID 0 -> delta 50 at
        // coord 1.0; GID 1 out of range -> 0.
        assert!((hvar.advance_delta(0, &[1.0]) - 50.0).abs() < 1e-3);
        assert!(hvar.advance_delta(1, &[1.0]).abs() < 1e-6);
    }

    #[test]
    fn advance_delta_scales_with_coord() {
        let ivs = build_ivs_one_axis_one_region_one_item(100);
        let bytes = build_hvar_without_map(&ivs);
        let hvar = Hvar::parse(&bytes).unwrap();
        assert!((hvar.advance_delta(0, &[0.5]) - 50.0).abs() < 1e-3);
        assert!(hvar.advance_delta(0, &[0.0]).abs() < 1e-6);
    }

    #[test]
    fn rejects_unsupported_major_version() {
        let ivs = build_ivs_one_axis_one_region_one_item(0);
        let mut bytes = build_hvar_without_map(&ivs);
        bytes[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert!(matches!(Hvar::parse(&bytes), Err(Error::Malformed { .. })));
    }

    /// A DeltaSetIndexMap with `mapCount = 0` carries no entries, so
    /// every glyph must yield "no mapping" (the caller then emits a
    /// zero delta). Before the guard landed, `map_count.saturating_sub(1)`
    /// collapsed to 0 and the decoder read the first `entry_bytes`
    /// after the header as if they were a real entry, producing a
    /// bogus `(outer, inner)` pair that the ItemVariationStore would
    /// happily treat as a real variation index.
    #[test]
    fn index_map_with_zero_map_count_yields_no_mapping() {
        // format 0 (u16 mapCount); entryFormat with 1-byte entries,
        // 1 inner bit; mapCount = 0; then a slack byte the decoder
        // would otherwise interpret as the first entry's payload.
        let mut data = Vec::new();
        data.push(0u8);
        data.push(0u8);
        data.extend_from_slice(&0u16.to_be_bytes());
        data.push(0xAB);
        assert!(read_index_map(&data, 0, 0).is_none());
        assert!(read_index_map(&data, 0, 999).is_none());
    }

    #[test]
    fn index_map_offset_near_usize_max_yields_no_mapping() {
        // `start + 2` used to overflow. On 32-bit targets a u32 map
        // offset from the font can reach this range.
        let data = [0u8; 8];
        assert!(read_index_map(&data, usize::MAX - 1, 0).is_none());
        assert!(read_index_map(&data, usize::MAX, 0).is_none());
    }
}
