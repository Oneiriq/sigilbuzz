//! `HVAR` subsetting.
//!
//! HVAR carries per-glyph advance-width deltas as
//! `(outer, inner) -> ItemVariationStore` lookups. The outer index
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
//! - a fresh `DeltaSetIndexMap` mapping `new_gid -> (0, new_inner)`
//!   in `format 0` (compact `u16` map count) when the inner range
//!   fits, else `format 1` for big subsets.
//!
//! The output store always uses outer index 0. We never bother with
//! multiple subtables. The OpenType spec permits multiple outer
//! groupings to enable better delta packing per group, but for the
//! sizes a font subsetter produces the savings are negligible
//! against the rest of the table and the single-outer layout keeps
//! the rewriter trivial. Glyphs with no source row map to
//! `(0, 0)` of the output, where row 0 is a synthesized all-zero
//! row, equivalent to "no advance variation for this gid".

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use crate::variation_store::{pull_row, read_regions, rebuild_store, rebuilt_store_len, PulledRow};
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

    // Step 1: pull each kept gid's source row. Glyphs missing from
    // the source map (or out of range) yield None and are routed
    // through the synthesized zero-row in the output.
    let mut pulled_rows: Vec<PulledRow> = Vec::with_capacity(kept.len() + 1);
    // Reserve slot 0 for the synthesized all-zero row.
    pulled_rows.push(PulledRow {
        region_indexes: Vec::new(),
        deltas: Vec::new(),
    });
    // Output slot of every distinct row content seen so far. Identical
    // source rows are deduped onto the same inner slot to keep the
    // table small. The first slot is the synthesized all-zero row,
    // which absorbs source rows that are themselves empty.
    let mut slot_by_row: BTreeMap<(Vec<u16>, Vec<i32>), u16> = BTreeMap::new();
    slot_by_row.insert((Vec::new(), Vec::new()), 0);
    // Output slot already resolved for a source `(outer, inner)`
    // pair. Many glyphs can share a pair, and pulling the same row
    // again would repeat the same work.
    let mut slot_by_pair: BTreeMap<(u16, u16), u16> = BTreeMap::new();
    // Source row entries the pulls may still read. Distinct pairs in
    // a well-formed store read disjoint rows, so their total stays
    // near the store size. Subtable offsets that alias one large
    // subtable would otherwise let a small table cost quadratic time.
    let mut pull_budget = store_bytes.len().saturating_mul(4).saturating_add(1 << 16);
    // Map `new_gid -> output_inner_index`. We assign inner indexes in
    // first-appearance (i.e. kept order) so the layout is
    // deterministic.
    let mut new_inner_per_gid: Vec<u16> = Vec::with_capacity(kept.len());

    for &old_gid in kept {
        let (outer, inner) = if parsed.advance_map_off == 0 {
            (0u16, old_gid)
        } else {
            match read_index_map(hvar_bytes, parsed.advance_map_off as usize, old_gid) {
                Some(p) => p,
                None => {
                    // No mapping -> falls back to the synthesized
                    // zero row at output inner 0.
                    new_inner_per_gid.push(0);
                    continue;
                }
            }
        };
        if let Some(&slot) = slot_by_pair.get(&(outer, inner)) {
            new_inner_per_gid.push(slot);
            continue;
        }
        let slot = match pull_row(store_bytes, outer, inner)? {
            None => 0,
            Some(row) => {
                pull_budget = pull_budget
                    .checked_sub(row.region_indexes.len() + row.deltas.len())
                    .ok_or(SubsetError::Unsupported(
                        "HVAR store rows overlap past the store size",
                    ))?;
                let key = (row.region_indexes, row.deltas);
                match slot_by_row.get(&key) {
                    Some(&slot) => slot,
                    None => {
                        let idx = pulled_rows.len() as u16;
                        pulled_rows.push(PulledRow {
                            region_indexes: key.0.clone(),
                            deltas: key.1.clone(),
                        });
                        slot_by_row.insert(key, idx);
                        idx
                    }
                }
            }
        };
        slot_by_pair.insert((outer, inner), slot);
        new_inner_per_gid.push(slot);
    }

    // Step 2: rebuild the ItemVariationStore from the pulled rows.
    // Every output row is padded to the union of all rows' regions,
    // so rows drawn from many small subtables can multiply the size.
    // Refuse outputs far beyond anything the source could justify.
    let (axis_count, regions) = read_regions(store_bytes)?;
    let max_store_len = hvar_bytes.len().saturating_mul(256).max(1 << 24);
    if rebuilt_store_len(&pulled_rows, axis_count) > max_store_len {
        return Err(SubsetError::Unsupported("HVAR rebuilt store too large"));
    }
    let rebuilt = rebuild_store(&pulled_rows, axis_count, &regions);

    // Step 3: build the new DeltaSetIndexMap mapping
    // new_gid -> (0, new_inner). Pick the densest format the inner
    // range allows.
    let max_inner = new_inner_per_gid.iter().copied().max().unwrap_or(0);
    let map_bytes = build_index_map(&new_inner_per_gid, max_inner);

    // Step 4: assemble the HVAR header.
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
    let map = data.get(start..)?;
    // The map header is at least 4 bytes (format 0).
    if map.len() < 4 {
        return None;
    }
    let (&[format, entry_format], rest) = map.split_first_chunk::<2>()?;
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
    let idx = if u32::from(gid) < map_count {
        u32::from(gid)
    } else {
        map_count.saturating_sub(1)
    } as usize;
    let entry = idx
        .checked_mul(entry_bytes)
        .and_then(|entry_off| entries.get(entry_off..))
        .and_then(|rest| rest.get(..entry_bytes))?;
    let raw = entry.iter().fold(0u32, |raw, &b| (raw << 8) | u32::from(b));
    let inner = (raw & inner_mask) as u16;
    let outer = (raw >> inner_bits) as u16;
    Some((outer, inner))
}

/// Builds a fresh `DeltaSetIndexMap`. Outer is always 0, so each
/// entry's outer-bit slice is zero. We encode the inner index
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
        // honor it. .notdef is typically invariant (delta 0).
        let got = new_hvar.advance_delta(0, &coords);
        let want = face.hvar().unwrap().unwrap().advance_delta(0, &coords);
        assert!((got - want).abs() <= 1.0);
    }

    /// One `ItemVariationData` subtable: region indexes plus i8 rows.
    struct TestSubtable {
        region_indexes: Vec<u16>,
        rows: Vec<Vec<i8>>,
    }

    /// Serializes an IVS whose region list has `axis_count` axes and
    /// `region_count` regions (all zero), plus `subtables`. Each entry
    /// of `subtable_refs` is the index of the subtable its offset
    /// points at, so offsets may alias.
    fn test_store(
        axis_count: u16,
        region_count: u16,
        subtables: &[TestSubtable],
        subtable_refs: &[usize],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        let region_list_off = 8 + 4 * subtable_refs.len() as u32;
        out.extend_from_slice(&region_list_off.to_be_bytes());
        out.extend_from_slice(&(subtable_refs.len() as u16).to_be_bytes());
        let offsets_at = out.len();
        out.resize(out.len() + 4 * subtable_refs.len(), 0);
        out.extend_from_slice(&axis_count.to_be_bytes());
        out.extend_from_slice(&region_count.to_be_bytes());
        out.resize(
            out.len() + usize::from(axis_count) * usize::from(region_count) * 6,
            0,
        );
        let mut subtable_offs = Vec::new();
        for sub in subtables {
            subtable_offs.push(out.len() as u32);
            out.extend_from_slice(&(sub.rows.len() as u16).to_be_bytes());
            out.extend_from_slice(&0u16.to_be_bytes()); // all narrow
            out.extend_from_slice(&(sub.region_indexes.len() as u16).to_be_bytes());
            for ri in &sub.region_indexes {
                out.extend_from_slice(&ri.to_be_bytes());
            }
            for row in &sub.rows {
                out.extend(row.iter().map(|&d| d as u8));
            }
        }
        for (i, &sub) in subtable_refs.iter().enumerate() {
            let at = offsets_at + 4 * i;
            out[at..at + 4].copy_from_slice(&subtable_offs[sub].to_be_bytes());
        }
        out
    }

    /// Wraps `store` in an HVAR with an optional DeltaSetIndexMap, then
    /// in an SFNT whose only table is that HVAR.
    fn font_with_hvar(store: &[u8], advance_map: Option<&[u8]>) -> Vec<u8> {
        let mut hvar = Vec::new();
        hvar.extend_from_slice(&1u16.to_be_bytes()); // major
        hvar.extend_from_slice(&0u16.to_be_bytes()); // minor
        hvar.extend_from_slice(&20u32.to_be_bytes()); // store
        let map_off = advance_map.map_or(0, |_| 20 + store.len() as u32);
        hvar.extend_from_slice(&map_off.to_be_bytes());
        hvar.extend_from_slice(&0u32.to_be_bytes()); // lsb map
        hvar.extend_from_slice(&0u32.to_be_bytes()); // rsb map
        hvar.extend_from_slice(store);
        if let Some(map) = advance_map {
            hvar.extend_from_slice(map);
        }
        crate::sfnt::build(0x0001_0000, &[(tag::HVAR, hvar)])
    }

    #[test]
    fn padding_rows_to_out_of_range_regions_is_bounded() {
        // A 60000-axis region list with no regions, and one row that
        // references 2000 region indexes. Padding each referenced
        // region to 60000 axes used to build a 720 MB store.
        let store = test_store(
            60_000,
            0,
            &[TestSubtable {
                region_indexes: (0..2000).collect(),
                rows: alloc::vec![alloc::vec![1; 2000]],
            }],
            &[0],
        );
        let font = font_with_hvar(&store, None);
        let face = Face::parse_bytes(&font, 0).unwrap();
        let r = subset_hvar(&face, &[0]);
        assert!(matches!(r, Err(SubsetError::Unsupported(_))), "{r:?}");
    }

    #[test]
    fn many_wide_rows_rebuild_in_linear_time() {
        // 200 distinct rows over the same 5000 regions. Region dedup
        // and row emission used per-region linear scans, which made
        // this rebuild take billions of steps.
        const REGIONS: u16 = 5000;
        let rows: Vec<Vec<i8>> = (0..200)
            .map(|i: i32| {
                (0..i32::from(REGIONS))
                    .map(|j| ((i * 7 + j) % 251) as i8)
                    .collect()
            })
            .collect();
        let store = test_store(
            1,
            REGIONS,
            &[TestSubtable {
                region_indexes: (0..REGIONS).collect(),
                rows,
            }],
            &[0],
        );
        let font = font_with_hvar(&store, None);
        let face = Face::parse_bytes(&font, 0).unwrap();
        let kept: Vec<u16> = (0..200).collect();
        let out = subset_hvar(&face, &kept).expect("subset").expect("HVAR");
        // Header (20) + store header (12) + region list (4 + 6 per
        // region) + subtable header (6) + 2 bytes per region index +
        // 201 rows (the zero row plus 200 distinct ones) of 2 bytes per
        // region, then a 1-byte-entry index map for 200 glyphs.
        let store_len = 12 + 4 + 6 * 5000 + 6 + 2 * 5000 + 201 * 2 * 5000;
        assert_eq!(out.len(), 20 + store_len + 4 + 200);
    }

    #[test]
    fn aliased_subtables_do_not_multiply_row_pulls() {
        // 20000 subtable offsets all point at one subtable whose single
        // row references 20000 regions, and every glyph maps to a
        // different offset. Each pull re-read the whole row.
        const N: u16 = 20_000;
        let store = test_store(
            1,
            0,
            &[TestSubtable {
                region_indexes: (0..N).collect(),
                rows: alloc::vec![alloc::vec![1; usize::from(N)]],
            }],
            &alloc::vec![0; usize::from(N)],
        );
        // DeltaSetIndexMap format 0, 2-byte entries, 1 inner bit:
        // entry for gid i is (outer i, inner 0).
        let mut map = Vec::new();
        map.push(0); // format
        map.push(0x10); // entryFormat: 2 bytes, 1 inner bit
        map.extend_from_slice(&N.to_be_bytes());
        for i in 0..N {
            map.extend_from_slice(&(i << 1).to_be_bytes());
        }
        let font = font_with_hvar(&store, Some(&map));
        let face = Face::parse_bytes(&font, 0).unwrap();
        let kept: Vec<u16> = (0..N).collect();
        let r = subset_hvar(&face, &kept);
        assert!(matches!(r, Err(SubsetError::Unsupported(_))), "{r:?}");
    }
}
