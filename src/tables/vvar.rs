//! `VVAR`: Vertical Metrics Variations.
//!
//! HVAR's vertical sibling. Carries per-glyph advance-height and
//! top-side-bearing deltas for a variable font that supports
//! vertical layout. Without VVAR a variable font with `vmtx` keeps
//! the same advances at every axis instance, which works for
//! horizontal-only fonts but loses correctness for CJK / vertical
//! Latin runs whose vertical metrics need to track weight or
//! width changes.
//!
//! # Layout
//!
//! ```text
//!   u16       majorVersion = 1
//!   u16       minorVersion = 0
//!   Offset32  itemVariationStoreOffset
//!   Offset32  advanceHeightMappingOffset      (may be 0, use gid)
//!   Offset32  tsbMappingOffset                (optional)
//!   Offset32  bsbMappingOffset                (optional, unused here)
//!   Offset32  vorgMappingOffset               (optional)
//! ```
//!
//! Each mapping offset, when non-zero, points at a
//! `DeltaSetIndexMap` (glyph id -> `(outer, inner)`). The decoder
//! is shared with [`super::hvar`].

use crate::error::{Error, Result};
use crate::tables::parse::Reader;
use crate::tables::variation_store::ItemVariationStore;

/// A parsed `VVAR` table.
#[derive(Debug, Clone)]
pub struct Vvar<'a> {
    data: &'a [u8],
    store: ItemVariationStore<'a>,
    advance_map_off: u32,
    tsb_map_off: u32,
    vorg_map_off: u32,
}

impl<'a> Vvar<'a> {
    /// Parses a `VVAR` table.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported VVAR major version",
            });
        }
        let store_off = r.read_u32()? as usize;
        let advance_map_off = r.read_u32()?;
        let tsb_map_off = r.read_u32()?;
        let _bsb_off = r.read_u32()?;
        let vorg_map_off = r.read_u32()?;

        let store = ItemVariationStore::parse(data.get(store_off..).ok_or(Error::Malformed {
            offset: store_off,
            context: "VVAR itemVariationStore offset past end",
        })?)?;

        Ok(Self {
            data,
            store,
            advance_map_off,
            tsb_map_off,
            vorg_map_off,
        })
    }

    /// Returns the advance-height delta for `glyph_id` at the
    /// given normalized coords. Mirrors HVAR's surface; the caller
    /// rounds and adds to the base `vmtx` advance. `0.0` for a
    /// glyph with no entry.
    #[must_use]
    pub fn advance_height_delta(&self, glyph_id: u16, coords: &[f32]) -> f32 {
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

    /// Returns the top-side-bearing delta for `glyph_id`, or
    /// `None` when the table doesn't carry a tsb mapping. (Most
    /// VVAR tables carry only the advance-height map; tsb deltas
    /// are needed only when vertical bearings vary independently.)
    #[must_use]
    pub fn top_side_bearing_delta(&self, glyph_id: u16, coords: &[f32]) -> Option<f32> {
        if self.tsb_map_off == 0 {
            return None;
        }
        let (outer, inner) = read_index_map(self.data, self.tsb_map_off as usize, glyph_id)?;
        Some(self.store.delta(outer, inner, coords))
    }

    /// Returns the vertical-origin delta for `glyph_id` at the given
    /// normalized coords: how far the glyph's `VORG` origin moves. As
    /// in HarfBuzz's `get_vorg_delta_unscaled`, a table without a
    /// vertical origin mapping has no deltas; this returns `None`
    /// then, and for a glyph the mapping has no entry for.
    ///
    /// The caller adds it to the `VORG` value and rounds the sum.
    #[must_use]
    pub fn vorg_delta(&self, glyph_id: u16, coords: &[f32]) -> Option<f32> {
        if self.vorg_map_off == 0 {
            return None;
        }
        let (outer, inner) = read_index_map(self.data, self.vorg_map_off as usize, glyph_id)?;
        Some(self.store.delta(outer, inner, coords))
    }
}

/// Reads a `(outer, inner)` index pair from a `DeltaSetIndexMap`.
/// Identical to HVAR's helper, kept private to each module so the
/// two parsers stay independent if the mapping format ever forks
/// for one of them.
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

    if map_count == 0 {
        return None;
    }

    let entry_bytes = ((entry_format >> 4) & 0x03) as usize + 1;
    let inner_bits = (entry_format & 0x0F) as u32 + 1;
    let inner_mask: u32 = (1u32 << inner_bits) - 1;

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
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        let region_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        let subtable_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());

        let region_start = out.len() as u32;
        out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_start.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        write_f2dot14(&mut out, 0.0);
        write_f2dot14(&mut out, 1.0);
        write_f2dot14(&mut out, 1.0);

        let sub_start = out.len() as u32;
        out[subtable_slot..subtable_slot + 4].copy_from_slice(&sub_start.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&delta.to_be_bytes());

        out
    }

    fn build_vvar_without_maps(ivs: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        // 4 (ver) + 4*5 (offsets) = 24 bytes of header.
        out.extend_from_slice(&24u32.to_be_bytes()); // ivs offset
        out.extend_from_slice(&0u32.to_be_bytes()); // no advance map
        out.extend_from_slice(&0u32.to_be_bytes()); // no tsb map
        out.extend_from_slice(&0u32.to_be_bytes()); // no bsb map
        out.extend_from_slice(&0u32.to_be_bytes()); // no vorg map
        out.extend_from_slice(ivs);
        out
    }

    /// Without an advance-height mapping the glyph id is the inner
    /// index directly, and at coord 1.0 the delta is the
    /// stored value.
    #[test]
    fn advance_height_delta_uses_gid_as_inner_without_map() {
        let ivs = build_ivs_one_axis_one_region_one_item(80);
        let bytes = build_vvar_without_maps(&ivs);
        let vvar = Vvar::parse(&bytes).unwrap();
        assert!((vvar.advance_height_delta(0, &[1.0]) - 80.0).abs() < 1e-3);
        // GID 1 has no item: yields zero, not panic.
        assert!(vvar.advance_height_delta(1, &[1.0]).abs() < 1e-6);
    }

    #[test]
    fn advance_height_delta_scales_with_coord() {
        let ivs = build_ivs_one_axis_one_region_one_item(120);
        let bytes = build_vvar_without_maps(&ivs);
        let vvar = Vvar::parse(&bytes).unwrap();
        assert!((vvar.advance_height_delta(0, &[0.5]) - 60.0).abs() < 1e-3);
        assert!(vvar.advance_height_delta(0, &[0.0]).abs() < 1e-6);
    }

    #[test]
    fn rejects_unsupported_major_version() {
        let ivs = build_ivs_one_axis_one_region_one_item(0);
        let mut bytes = build_vvar_without_maps(&ivs);
        bytes[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert!(matches!(Vvar::parse(&bytes), Err(Error::Malformed { .. })));
    }

    /// When the table doesn't ship a tsb mapping the helper
    /// returns `None`, distinct from "mapping present but glyph
    /// has no entry". Lets consumers tell the two cases apart.
    #[test]
    fn missing_tsb_map_returns_none() {
        let ivs = build_ivs_one_axis_one_region_one_item(50);
        let bytes = build_vvar_without_maps(&ivs);
        let vvar = Vvar::parse(&bytes).unwrap();
        assert!(vvar.top_side_bearing_delta(0, &[1.0]).is_none());
    }

    /// Build a VVAR that *does* carry a tsb mapping and check the
    /// look-up resolves through it.
    #[test]
    fn tsb_delta_resolves_through_index_map() {
        let ivs = build_ivs_one_axis_one_region_one_item(40);
        // Header is 24 bytes; we'll place: header, then ivs, then
        // the index map. tsb mapping offset points at the index
        // map.
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        // ivs at 24
        out.extend_from_slice(&24u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // no advance map
        let tsb_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes()); // tsb offset placeholder
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&ivs);

        // DeltaSetIndexMap: format 0 (u16 mapCount).
        // entryFormat: 0x00 -> 1 byte per entry, 1 inner bit.
        let map_off = out.len() as u32;
        out[tsb_off_slot..tsb_off_slot + 4].copy_from_slice(&map_off.to_be_bytes());
        out.push(0); // format
        out.push(0); // entryFormat: 1 byte entries, inner bits = 1
        out.extend_from_slice(&1u16.to_be_bytes()); // mapCount
        out.push(0); // single entry: outer=0, inner=0

        let vvar = Vvar::parse(&out).unwrap();
        let d = vvar.top_side_bearing_delta(0, &[1.0]).unwrap();
        assert!((d - 40.0).abs() < 1e-3);
    }

    /// The vertical origin delta reads the last mapping offset, and a
    /// table without that mapping has none, as in HarfBuzz.
    #[test]
    fn vorg_delta_resolves_through_its_own_index_map() {
        let ivs = build_ivs_one_axis_one_region_one_item(-30);
        assert!(Vvar::parse(&build_vvar_without_maps(&ivs))
            .unwrap()
            .vorg_delta(0, &[1.0])
            .is_none());
        let mut out = build_vvar_without_maps(&ivs);
        let map_off = out.len() as u32;
        out[20..24].copy_from_slice(&map_off.to_be_bytes());
        // Format 0, 1-byte entries with 1 inner bit, one entry: (0, 0).
        out.extend_from_slice(&[0, 0, 0, 1, 0]);
        let vvar = Vvar::parse(&out).unwrap();
        assert_eq!(vvar.vorg_delta(0, &[1.0]), Some(-30.0));
        assert_eq!(vvar.vorg_delta(0, &[0.5]), Some(-15.0));
        // The other mappings are still absent.
        assert!(vvar.top_side_bearing_delta(0, &[1.0]).is_none());
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
