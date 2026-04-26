//! `HVAR` subsetting.
//!
//! HVAR carries per-glyph advance-width deltas as
//! `(outer, inner) → ItemVariationStore` lookups. The outer index
//! selects an `ItemVariationData` subtable; the inner index picks a
//! delta row within it. The mapping from gid to `(outer, inner)` is
//! either implicit (`outer = 0`, `inner = gid`) or explicit via a
//! `DeltaSetIndexMap`.
//!
//! Subsetting walks every kept gid, pulls the row it references out
//! of the source store, and rebuilds:
//!
//! - a fresh `ItemVariationStore` containing only the referenced
//!   rows (deduped across the source so rows shared between glyphs
//!   stay shared in the output),
//! - a fresh `DeltaSetIndexMap` mapping `new_gid → (0, new_inner)`
//!   in `format 0` (compact `u16` map count) when the inner range
//!   fits, else `format 1` for big subsets.
//!
//! The output store always uses outer index 0 — we never bother with
//! multiple subtables. The OpenType spec permits multiple outer
//! groupings to enable better delta packing per group, but for the
//! sizes a font subsetter produces the savings are negligible
//! against the rest of the table and the single-outer layout keeps
//! the rewriter trivial. Glyphs with no source row map to
//! `(0, 0)` of the output, where row 0 is a synthesized all-zero
//! row — equivalent to "no advance variation for this gid".

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use crate::variation_store::{pull_row, read_regions, rebuild_store, PulledRow};
use crate::{GlyphId, SubsetError};

/// Subsets HVAR for `kept` (kept gids in new-gid order). Returns
/// `None` when the source font has no HVAR (caller emits no HVAR
/// either).
pub(crate) fn subset_hvar(
    face: &Face<'_>,
    kept: &[GlyphId],
) -> Result<Option<Vec<u8>>, SubsetError> {
    let hvar_bytes = match face.table_bytes(tag::HVAR) {
        Ok(b) => b,
        Err(sigilbuzz::Error::MissingTable { .. }) => return Ok(None),
        Err(e) => return Err(SubsetError::from(e)),
    };
    let parsed = parse_hvar_header(hvar_bytes)?;
    let store_bytes = hvar_bytes
        .get(parsed.store_off..)
        .ok_or(SubsetError::Unsupported("HVAR store offset past end"))?;

    // Step 1 — pull each kept gid's source row. Glyphs missing from
    // the source map (or out of range) yield None and are routed
    // through the synthesized zero-row in the output.
    let mut pulled_rows: Vec<PulledRow> = Vec::with_capacity(kept.len() + 1);
    // Reserve slot 0 for the synthesized all-zero row.
    pulled_rows.push(PulledRow {
        region_indexes: Vec::new(),
        deltas: Vec::new(),
    });
    // Map `new_gid → output_inner_index`. We assign inner indexes in
    // first-appearance (i.e. kept order) so the layout is
    // deterministic; identical source rows are deduped onto the same
    // inner slot to keep the table small.
    let mut new_inner_per_gid: Vec<u16> = alloc::vec![0u16; kept.len()];

    for (new_gid, &old_gid) in kept.iter().enumerate() {
        let (outer, inner) = if parsed.advance_map_off == 0 {
            (0u16, old_gid)
        } else {
            match read_index_map(hvar_bytes, parsed.advance_map_off as usize, old_gid) {
                Some(p) => p,
                None => {
                    // No mapping → falls back to the synthesized
                    // zero row at output inner 0.
                    new_inner_per_gid[new_gid] = 0;
                    continue;
                }
            }
        };
        let row_opt = pull_row(store_bytes, outer, inner)?;
        match row_opt {
            None => {
                new_inner_per_gid[new_gid] = 0;
            }
            Some(row) => {
                // Dedupe against earlier rows. A row that equals a
                // previously-pulled row (same regions, same deltas)
                // collapses onto that row's slot. The first slot is
                // the synthesized all-zero row, which absorbs source
                // rows that are themselves zero.
                let dedup_idx = pulled_rows.iter().position(|r| rows_equal(r, &row));
                let slot = match dedup_idx {
                    Some(i) => i as u16,
                    None => {
                        let idx = pulled_rows.len() as u16;
                        pulled_rows.push(row);
                        idx
                    }
                };
                new_inner_per_gid[new_gid] = slot;
            }
        }
    }

    // Step 2 — rebuild the ItemVariationStore from the pulled rows.
    let (axis_count, regions) = read_regions(store_bytes)?;
    let rebuilt = rebuild_store(&pulled_rows, axis_count, &regions);

    // Step 3 — build the new DeltaSetIndexMap mapping
    // new_gid → (0, new_inner). Pick the densest format the inner
    // range allows.
    let max_inner = new_inner_per_gid.iter().copied().max().unwrap_or(0);
    let map_bytes = build_index_map(&new_inner_per_gid, max_inner);

    // Step 4 — assemble the HVAR header.
    let mut out: Vec<u8> = Vec::with_capacity(20 + map_bytes.len() + rebuilt.bytes.len());
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    let store_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // itemVariationStoreOffset
    let map_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // advanceWidthMappingOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // lsbMappingOffset = 0
    out.extend_from_slice(&0u32.to_be_bytes()); // rsbMappingOffset = 0

    // Store comes first (right after the 20-byte header) so its
    // offset is known before we write the map.
    let store_off = out.len() as u32;
    out[store_off_slot..store_off_slot + 4].copy_from_slice(&store_off.to_be_bytes());
    out.extend_from_slice(&rebuilt.bytes);

    let map_off = out.len() as u32;
    out[map_off_slot..map_off_slot + 4].copy_from_slice(&map_off.to_be_bytes());
    out.extend_from_slice(&map_bytes);

    Ok(Some(out))
}

#[derive(Debug, Clone, Copy)]
struct HvarHeader {
    store_off: usize,
    advance_map_off: u32,
}

fn parse_hvar_header(bytes: &[u8]) -> Result<HvarHeader, SubsetError> {
    if bytes.len() < 20 {
        return Err(SubsetError::Unsupported("HVAR header truncated"));
    }
    let major = u16::from_be_bytes([bytes[0], bytes[1]]);
    if major != 1 {
        return Err(SubsetError::Unsupported("HVAR major != 1"));
    }
    let store_off = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    let advance_map_off = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    Ok(HvarHeader {
        store_off,
        advance_map_off,
    })
}

/// Reads a `(outer, inner)` from a `DeltaSetIndexMap` at absolute
/// offset `start`. Mirrors the parser in `sigilbuzz::tables::hvar`
/// but lives here so the subsetter does not depend on the parser's
/// return type.
fn read_index_map(data: &[u8], start: usize, gid: u16) -> Option<(u16, u16)> {
    if data.len() < start + 4 {
        return None;
    }
    let format = data[start];
    let entry_format = data[start + 1];
    let mut cursor = start + 2;
    let map_count = match format {
        0 => {
            let v = u16::from_be_bytes([data[cursor], data[cursor + 1]]) as u32;
            cursor += 2;
            v
        }
        1 => {
            if data.len() < cursor + 4 {
                return None;
            }
            let v = u32::from_be_bytes([
                data[cursor],
                data[cursor + 1],
                data[cursor + 2],
                data[cursor + 3],
            ]);
            cursor += 4;
            v
        }
        _ => return None,
    };
    if map_count == 0 {
        return None;
    }
    let entry_bytes = ((entry_format >> 4) & 0x03) as usize + 1;
    let inner_bits = (entry_format & 0x0F) as u32 + 1;
    let inner_mask: u32 = (1u32 << inner_bits) - 1;
    let idx = if (gid as u32) < map_count {
        gid as u32
    } else {
        map_count.saturating_sub(1)
    } as usize;
    let entry_off = cursor + idx * entry_bytes;
    if data.len() < entry_off + entry_bytes {
        return None;
    }
    let mut raw: u32 = 0;
    for i in 0..entry_bytes {
        raw = (raw << 8) | u32::from(data[entry_off + i]);
    }
    let inner = (raw & inner_mask) as u16;
    let outer = (raw >> inner_bits) as u16;
    Some((outer, inner))
}

fn rows_equal(a: &PulledRow, b: &PulledRow) -> bool {
    if a.region_indexes != b.region_indexes {
        return false;
    }
    a.deltas == b.deltas
}

/// Builds a fresh `DeltaSetIndexMap`. Outer is always 0, so each
/// entry's outer-bit slice is zero — we encode the inner index
/// alone in the densest fitting form.
///
/// The choice between `format 0` (u16 mapCount) and `format 1`
/// (u32 mapCount) is made by the kept-gid count: the OpenType spec
/// caps format 0 at 65535 entries.
fn build_index_map(new_inner_per_gid: &[u16], max_inner: u16) -> Vec<u8> {
    let map_count = new_inner_per_gid.len() as u32;
    // We need enough inner bits to encode max_inner; outer is always
    // 0 so `outer_bits = 0` and the entire entry is inner. The spec
    // requires at least 1 inner bit; clamp accordingly.
    let inner_bits = inner_bit_count(max_inner);
    // The total entry width (outer_bits + inner_bits) determines
    // bytes-per-entry: one of {1, 2, 3, 4}. With outer always 0 the
    // entry width equals inner_bits.
    let entry_bytes: usize = inner_bits.div_ceil(8).max(1) as usize;
    let entry_format: u8 =
        ((((entry_bytes - 1) as u8) & 0x03) << 4) | ((inner_bits - 1) as u8 & 0x0F);

    let format: u8 = if u16::try_from(map_count).is_ok() {
        0
    } else {
        1
    };

    let mut out: Vec<u8> = Vec::new();
    out.push(format);
    out.push(entry_format);
    if format == 0 {
        out.extend_from_slice(&(map_count as u16).to_be_bytes());
    } else {
        out.extend_from_slice(&map_count.to_be_bytes());
    }
    for &inner in new_inner_per_gid {
        let raw = u32::from(inner);
        let bytes = raw.to_be_bytes();
        // Write the low `entry_bytes` bytes of `raw`, big-endian.
        out.extend_from_slice(&bytes[4 - entry_bytes..]);
    }
    out
}

fn inner_bit_count(max_inner: u16) -> u32 {
    let mut bits = 1u32;
    while (1u32 << bits) <= u32::from(max_inner) {
        bits += 1;
    }
    bits.min(16)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUBIK: &[u8] = include_bytes!("../../../tests/fixtures/rubik_vf.ttf");

    fn rubik_face() -> Face<'static> {
        Face::parse_bytes(RUBIK, 0).unwrap()
    }

    #[test]
    fn inner_bit_count_grows_with_max() {
        assert_eq!(inner_bit_count(0), 1);
        assert_eq!(inner_bit_count(1), 1);
        assert_eq!(inner_bit_count(2), 2);
        assert_eq!(inner_bit_count(3), 2);
        assert_eq!(inner_bit_count(255), 8);
        assert_eq!(inner_bit_count(256), 9);
    }

    #[test]
    fn missing_hvar_returns_none() {
        const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
        let face = Face::parse_bytes(OPEN_SANS, 0).unwrap();
        let kept: Vec<u16> = (0..face.maxp().unwrap().num_glyphs).collect();
        assert!(subset_hvar(&face, &kept).unwrap().is_none());
    }

    #[test]
    fn subset_hvar_round_trips_advance_deltas() {
        // For three kept gids, the subset HVAR's advance delta at
        // wght=900 must equal the source HVAR's advance delta for
        // the same gids. We exercise this through the parser in
        // `sigilbuzz::tables::hvar`.
        use sigilbuzz::tables::hvar::Hvar as ParsedHvar;

        let face = rubik_face();
        let cmap = face.cmap().unwrap();
        let gid_a = cmap.glyph_id('A').unwrap();
        let gid_b = cmap.glyph_id('B').unwrap();
        let gid_c = cmap.glyph_id('C').unwrap();
        let kept: Vec<u16> = alloc::vec![0, gid_a, gid_b, gid_c];
        let kept_sorted = {
            let mut k = kept.clone();
            k.sort_unstable();
            k
        };

        let src_hvar = face.hvar().unwrap().expect("rubik has HVAR");
        let coords = face.fvar().unwrap().unwrap().normalize_coords(&[900.0]);

        let new_bytes = subset_hvar(&face, &kept_sorted).unwrap().expect("HVAR");
        let new_hvar = ParsedHvar::parse(&new_bytes).expect("parse subset HVAR");

        for (new_gid, &old_gid) in kept_sorted.iter().enumerate() {
            let want = src_hvar.advance_delta(old_gid, &coords);
            let got = new_hvar.advance_delta(new_gid as u16, &coords);
            // Allow 1-unit drift from i16 quantization.
            assert!(
                (want - got).abs() <= 1.0,
                "delta mismatch at new_gid={new_gid} (old_gid={old_gid}): want {want} got {got}"
            );
        }
    }

    #[test]
    fn subset_hvar_dedupes_zero_rows_to_slot_zero() {
        // Glyphs whose source advance is invariant should all collapse
        // onto the synthesized zero row. We don't verify the exact
        // inner indexes in the output (those are private), but a
        // delta of 0.0 is the round-trip invariant.
        use sigilbuzz::tables::hvar::Hvar as ParsedHvar;
        let face = rubik_face();
        let kept: Vec<u16> = alloc::vec![0]; // .notdef only
        let new_bytes = subset_hvar(&face, &kept).unwrap().expect("HVAR");
        let new_hvar = ParsedHvar::parse(&new_bytes).expect("parse subset HVAR");
        let coords = face.fvar().unwrap().unwrap().normalize_coords(&[900.0]);
        // Whatever the source had for .notdef, the output must
        // honour it. .notdef is typically invariant (delta 0).
        let got = new_hvar.advance_delta(0, &coords);
        let want = face.hvar().unwrap().unwrap().advance_delta(0, &coords);
        assert!((got - want).abs() <= 1.0);
    }
}
