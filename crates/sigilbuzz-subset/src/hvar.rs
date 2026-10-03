//! `HVAR` and `VVAR` subsetting.
//!
//! HVAR carries per-glyph advance-width deltas as
//! `(outer, inner) -> ItemVariationStore` lookups. The outer index
//! selects an `ItemVariationData` subtable; the inner index picks a
//! delta row within it. The mapping from gid to `(outer, inner)` is
//! either implicit (`outer = 0`, `inner = gid`) or explicit via a
//! `DeltaSetIndexMap`. VVAR is the vertical sibling, with the same
//! store and up to four maps: advance height, top and bottom side
//! bearings, and the vertical origin that `VORG` values vary by.
//!
//! Subsetting walks every kept gid through every map the subset
//! carries, pulls the row it references out of the source store, and
//! rebuilds:
//!
//! - a fresh `ItemVariationStore` containing only the referenced
//!   rows (deduped across the source and across the maps, so rows
//!   shared between glyphs stay shared in the output),
//! - a fresh `DeltaSetIndexMap` per map, mapping
//!   `new_gid -> (0, new_inner)` in `format 0` (compact `u16` map
//!   count) when the glyph count fits, else `format 1`.
//!
//! The output store always uses outer index 0. We never bother with
//! multiple subtables. The OpenType spec permits multiple outer
//! groupings to enable better delta packing per group, but for the
//! sizes a font subsetter produces the savings are negligible
//! against the rest of the table and the single-outer layout keeps
//! the rewriter trivial. Glyphs with no source row map to
//! `(0, 0)` of the output, where row 0 is a synthesized all-zero
//! row, equivalent to "no variation for this gid".
//!
//! That one subtable holds at most 65,535 rows, its `itemCount` being
//! a `u16`. The maps of a VVAR can pull more distinct rows than there
//! are glyphs, so a crafted source can need more. Such an HVAR fails
//! the subset, and such a VVAR is left out with a warning, rather than
//! wrap the count and point glyphs at the wrong rows.
//!
//! HVAR keeps only its advance map, as it always has. VVAR keeps every
//! map the source carries: the instancer folds the top side bearing
//! deltas into `vmtx` and the vertical origin deltas into `VORG`.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use crate::variation_store::{
    pull_row, read_regions, rebuild_store, rebuilt_store_len, PulledRow, MAX_ROWS,
};
use crate::warnings::Warnings;
use crate::{GlyphId, SubsetError};

/// Byte offset of `itemVariationStoreOffset` in HVAR and VVAR.
pub(crate) const STORE_SLOT: usize = 4;

/// Byte offset of the advance map's Offset32 in HVAR and VVAR.
const ADVANCE_SLOT: usize = 8;

/// Byte offset of `vorgMappingOffset` in VVAR.
pub(crate) const VVAR_VORG_SLOT: usize = 20;

/// The header of one metrics-variations table, and the messages its
/// rewrite reports.
struct MetricsVarLayout {
    /// Header length: the version, the store Offset32 at byte 4, then
    /// one Offset32 per `DeltaSetIndexMap` from byte 8.
    header_len: usize,
    /// The map slots the subset carries. The advance map at byte 8
    /// comes first; a zero there means the implicit gid mapping, so
    /// it is always written. Any other slot is written only when the
    /// source has that map.
    slots: &'static [usize],
    header_truncated: &'static str,
    bad_major: &'static str,
    store_past_end: &'static str,
    rows_overlap: &'static str,
    too_large: &'static str,
    too_many_rows: &'static str,
}

/// HVAR: version, store, then the advance, LSB and RSB maps. Only the
/// advance map is carried.
const HVAR: MetricsVarLayout = MetricsVarLayout {
    header_len: 20,
    slots: &[ADVANCE_SLOT],
    header_truncated: "HVAR header truncated",
    bad_major: "HVAR major != 1",
    store_past_end: "HVAR store offset past end",
    rows_overlap: "HVAR store rows overlap past the store size",
    too_large: "HVAR rebuilt store too large",
    too_many_rows: "HVAR rebuilt store has too many rows",
};

/// VVAR: version, store, then the advance height, TSB, BSB and vertical
/// origin maps. Every map the source has is carried.
const VVAR: MetricsVarLayout = MetricsVarLayout {
    header_len: 24,
    slots: &[ADVANCE_SLOT, 12, 16, VVAR_VORG_SLOT],
    header_truncated: "VVAR header truncated",
    bad_major: "VVAR major != 1",
    store_past_end: "VVAR store offset past end",
    rows_overlap: "VVAR store rows overlap past the store size",
    too_large: "VVAR rebuilt store too large",
    too_many_rows: "VVAR rebuilt store has too many rows",
};

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
    subset_metrics_var(&HVAR, hvar_bytes, kept)
        .map(Some)
        .map_err(|bad| SubsetError::Unsupported(bad.context))
}

/// Subsets VVAR for `kept` (kept gids in new-gid order). Returns `None`
/// when the source font has no VVAR. A VVAR that cannot be rebuilt is
/// left out of the subset, like the other vertical tables, and
/// reported in `warnings`: the subset's vertical metrics then stop
/// varying, but the run goes on.
pub(crate) fn subset_vvar(
    face: &Face<'_>,
    kept: &[GlyphId],
    warnings: &Warnings,
) -> Option<Vec<u8>> {
    let vvar_bytes = match face.table_bytes(tag::VVAR) {
        Ok(b) => b,
        Err(sigilbuzz::Error::MissingTable { .. }) => return None,
        Err(e) => {
            warnings.parse_error(tag::VVAR, 0, &e, "the whole table");
            return None;
        }
    };
    match subset_metrics_var(&VVAR, vvar_bytes, kept) {
        Ok(bytes) => Some(bytes),
        Err(bad) => {
            warnings.push(tag::VVAR, bad.offset, bad.context, "the whole table");
            None
        }
    }
}

/// Why a metrics-variations table could not be rebuilt, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Unreadable {
    /// Byte offset from the start of the table.
    offset: usize,
    context: &'static str,
}

/// Turns a store-walk error into an [`Unreadable`] at `offset`, the
/// start of the store.
fn store_error(offset: usize) -> impl Fn(SubsetError) -> Unreadable {
    move |e| Unreadable {
        offset,
        context: match e {
            SubsetError::Unsupported(context) => context,
            _ => "ItemVariationStore unreadable",
        },
    }
}

/// Rebuilds the table `layout` describes, `bytes`, for `kept`.
fn subset_metrics_var(
    layout: &MetricsVarLayout,
    bytes: &[u8],
    kept: &[GlyphId],
) -> Result<Vec<u8>, Unreadable> {
    let header = bytes.get(..layout.header_len).ok_or(Unreadable {
        offset: 0,
        context: layout.header_truncated,
    })?;
    if u16::from_be_bytes([header[0], header[1]]) != 1 {
        return Err(Unreadable {
            offset: 0,
            context: layout.bad_major,
        });
    }
    let read_slot = |slot: usize| -> usize {
        header
            .get(slot..)
            .and_then(<[u8]>::first_chunk::<4>)
            .map_or(0, |b| u32::from_be_bytes(*b) as usize)
    };
    let store_off = read_slot(STORE_SLOT);
    let store_bytes = bytes.get(store_off..).ok_or(Unreadable {
        offset: STORE_SLOT,
        context: layout.store_past_end,
    })?;
    let store_err = store_error(store_off);

    // Every pulled row lands in `pulled_rows`; slot 0 is the
    // synthesized all-zero row.
    let mut pulled_rows: Vec<PulledRow> = Vec::with_capacity(kept.len() + 1);
    pulled_rows.push(PulledRow {
        region_indexes: Vec::new(),
        deltas: Vec::new(),
    });
    // Output slot of every distinct row content seen so far. Identical
    // source rows are deduped onto the same inner slot to keep the
    // table small, across every map. The first slot is the
    // synthesized all-zero row, which absorbs source rows that are
    // themselves empty.
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

    // Per carried map: its header slot and `new_gid -> output inner`.
    // Inner indexes are assigned in first-appearance order (maps in
    // header order, glyphs in kept order) so the layout is
    // deterministic.
    let mut maps: Vec<(usize, Vec<u16>)> = Vec::with_capacity(layout.slots.len());
    for &slot in layout.slots {
        let map_off = read_slot(slot);
        if map_off == 0 && slot != ADVANCE_SLOT {
            continue;
        }
        let mut new_inner_per_gid: Vec<u16> = Vec::with_capacity(kept.len());
        for &old_gid in kept {
            let (outer, inner) = if map_off == 0 {
                (0u16, old_gid)
            } else {
                match read_index_map(bytes, map_off, old_gid) {
                    Some(p) => p,
                    None => {
                        // No mapping -> falls back to the synthesized
                        // zero row at output inner 0.
                        new_inner_per_gid.push(0);
                        continue;
                    }
                }
            };
            if let Some(&out_slot) = slot_by_pair.get(&(outer, inner)) {
                new_inner_per_gid.push(out_slot);
                continue;
            }
            let out_slot = match pull_row(store_bytes, outer, inner).map_err(&store_err)? {
                None => 0,
                Some(row) => {
                    pull_budget = pull_budget
                        .checked_sub(row.region_indexes.len() + row.deltas.len())
                        .ok_or(Unreadable {
                            offset: store_off,
                            context: layout.rows_overlap,
                        })?;
                    let key = (row.region_indexes, row.deltas);
                    match slot_by_row.get(&key) {
                        Some(&s) => s,
                        None => {
                            // The rebuilt subtable counts its rows in a
                            // u16. Up to four maps pull rows into it, so
                            // distinct rows can outnumber the glyphs.
                            let idx = u16::try_from(pulled_rows.len())
                                .ok()
                                .filter(|&idx| usize::from(idx) < MAX_ROWS)
                                .ok_or(Unreadable {
                                    offset: store_off,
                                    context: layout.too_many_rows,
                                })?;
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
            slot_by_pair.insert((outer, inner), out_slot);
            new_inner_per_gid.push(out_slot);
        }
        maps.push((slot, new_inner_per_gid));
    }

    // Rebuild the ItemVariationStore from the pulled rows. Every
    // output row is padded to the union of all rows' regions, so rows
    // drawn from many small subtables can multiply the size. Refuse
    // outputs far beyond anything the source could justify.
    let (axis_count, regions) = read_regions(store_bytes).map_err(&store_err)?;
    let max_store_len = bytes.len().saturating_mul(256).max(1 << 24);
    if rebuilt_store_len(&pulled_rows, axis_count) > max_store_len {
        return Err(Unreadable {
            offset: store_off,
            context: layout.too_large,
        });
    }
    let rebuilt = rebuild_store(&pulled_rows, axis_count, &regions).map_err(&store_err)?;

    // Assemble: the header, then the store right after it (so its
    // offset is known before the maps are written), then each map.
    let mut out: Vec<u8> = Vec::with_capacity(layout.header_len + rebuilt.bytes.len());
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.resize(layout.header_len, 0);
    write_offset(&mut out, STORE_SLOT, layout.header_len);
    out.extend_from_slice(&rebuilt.bytes);
    for (slot, new_inner_per_gid) in &maps {
        let max_inner = new_inner_per_gid.iter().copied().max().unwrap_or(0);
        let map_at = out.len();
        write_offset(&mut out, *slot, map_at);
        out.extend_from_slice(&build_index_map(new_inner_per_gid, max_inner));
    }
    Ok(out)
}

/// Writes `value` as the Offset32 at `slot`. A rebuilt table stays far
/// below 4 GiB: the store is capped above and each map holds at most
/// four bytes per glyph.
fn write_offset(out: &mut [u8], slot: usize, value: usize) {
    if let Some(field) = out.get_mut(slot..).and_then(<[u8]>::first_chunk_mut::<4>) {
        *field = (value as u32).to_be_bytes();
    }
}

/// Reads a `(outer, inner)` from a `DeltaSetIndexMap` at absolute
/// offset `start`. Mirrors the parser in `sigilbuzz::tables::hvar`
/// but lives here so the subsetter does not depend on the parser's
/// return type. A gid past the map's count takes the last entry;
/// `None` when the map is empty or unreadable.
pub(crate) fn read_index_map(data: &[u8], start: usize, gid: u16) -> Option<(u16, u16)> {
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

    /// One subtable over regions 0 and 1 (or 1 and 0 when `swapped`)
    /// with `count` distinct narrow rows: row i holds the low and high
    /// bytes of i.
    fn distinct_rows(count: u32, swapped: bool) -> TestSubtable {
        TestSubtable {
            region_indexes: if swapped {
                alloc::vec![1, 0]
            } else {
                alloc::vec![0, 1]
            },
            rows: (0..count)
                .map(|i| alloc::vec![(i & 0xFF) as u8 as i8, (i >> 8) as u8 as i8])
                .collect(),
        }
    }

    #[test]
    fn hvar_rows_past_one_subtable_are_an_error() {
        use sigilbuzz::tables::hvar::Hvar as ParsedHvar;
        // Glyph i maps to row i, and every row is distinct, so n glyphs
        // pull n rows next to the synthesized zero row.
        let store = test_store(1, 2, &[distinct_rows(65_535, false)], &[0]);
        let font = font_with_hvar(&store, None);
        let face = Face::parse_bytes(&font, 0).unwrap();

        // 65,535 rows in all: the most one subtable holds.
        let kept: Vec<u16> = (0..65_534).collect();
        let out = subset_hvar(&face, &kept).unwrap().expect("HVAR");
        let hvar = ParsedHvar::parse(&out).unwrap();
        // Row 65,533 holds 0xFD and 0xFF, -3 and -1.
        assert_eq!(hvar.advance_delta(65_533, &[0.5]), -4.0);

        // One more glyph used to wrap itemCount to 0.
        let kept: Vec<u16> = (0..65_535).collect();
        assert_eq!(
            subset_hvar(&face, &kept),
            Err(SubsetError::Unsupported(
                "HVAR rebuilt store has too many rows"
            ))
        );
    }

    #[test]
    fn hvar_rows_over_too_many_regions_are_an_error() {
        // One row over 32,768 regions: wordDeltaCount cannot say that
        // every column is wide.
        let store = test_store(
            1,
            0,
            &[TestSubtable {
                region_indexes: (0..0x8000).collect(),
                rows: alloc::vec![alloc::vec![1; 0x8000]],
            }],
            &[0],
        );
        let font = font_with_hvar(&store, None);
        let face = Face::parse_bytes(&font, 0).unwrap();
        assert_eq!(
            subset_hvar(&face, &[0]),
            Err(SubsetError::Unsupported(
                "ItemVariationStore rows reference more than 32,767 regions"
            ))
        );
    }

    /// A format 0 DeltaSetIndexMap that maps glyph i to `(outer, i)`,
    /// for `count` glyphs, in two-byte entries with 15 inner bits.
    fn outer_index_map(outer: u16, count: u16) -> Vec<u8> {
        let mut map = alloc::vec![0, 0x1E];
        map.extend_from_slice(&count.to_be_bytes());
        for inner in 0..count {
            map.extend_from_slice(&((outer << 15) | inner).to_be_bytes());
        }
        map
    }

    #[test]
    fn vvar_maps_pulling_too_many_rows_leave_it_out_with_a_warning() {
        // The advance map reads one subtable and the TSB map the other,
        // 32,768 distinct rows each: 65,537 rows with the zero row, for
        // 32,768 glyphs. The rows' slots used to wrap past 65,535.
        const N: u16 = 0x8000;
        let store = test_store(
            1,
            2,
            &[
                distinct_rows(u32::from(N), false),
                distinct_rows(u32::from(N), true),
            ],
            &[0, 1],
        );
        let advance = outer_index_map(0, N);
        let tsb = outer_index_map(1, N);
        let font = font_with_vvar(
            &store,
            [Some(advance.as_slice()), Some(tsb.as_slice()), None, None],
        );
        let face = Face::parse_bytes(&font, 0).unwrap();
        let kept: Vec<u16> = (0..N).collect();
        let sink = Warnings::default();
        assert!(subset_vvar(&face, &kept, &sink).is_none());
        let got: Vec<_> = sink
            .into_sorted()
            .iter()
            .map(|w| (w.table, w.offset, w.context, w.dropped))
            .collect();
        assert_eq!(
            got,
            [(
                tag::VVAR,
                24,
                "VVAR rebuilt store has too many rows",
                "the whole table"
            )]
        );

        // Half the glyphs pull 32,769 rows, which fit.
        let sink = Warnings::default();
        assert!(subset_vvar(&face, &kept[..usize::from(N / 2)], &sink).is_some());
        assert!(sink.into_sorted().is_empty());
    }

    /// A format 0 DeltaSetIndexMap with one-byte entries that hold the
    /// inner index alone (outer 0).
    fn byte_index_map(inners: &[u8]) -> Vec<u8> {
        let mut map = alloc::vec![0, 0x07]; // format 0, 1 byte, 8 inner bits
        map.extend_from_slice(&(inners.len() as u16).to_be_bytes());
        map.extend_from_slice(inners);
        map
    }

    /// A VVAR around `store` with the given maps (advance, TSB, BSB,
    /// vertical origin; `None` leaves the offset zero), in an SFNT
    /// whose only table is that VVAR.
    fn font_with_vvar(store: &[u8], maps: [Option<&[u8]>; 4]) -> Vec<u8> {
        let mut vvar = Vec::new();
        vvar.extend_from_slice(&1u16.to_be_bytes()); // major
        vvar.extend_from_slice(&0u16.to_be_bytes()); // minor
        vvar.extend_from_slice(&24u32.to_be_bytes()); // store
        vvar.resize(24, 0);
        vvar.extend_from_slice(store);
        for (i, map) in maps.iter().enumerate() {
            if let Some(map) = map {
                let at = vvar.len() as u32;
                vvar[8 + i * 4..12 + i * 4].copy_from_slice(&at.to_be_bytes());
                vvar.extend_from_slice(map);
            }
        }
        crate::sfnt::build(0x4F54_544F, &[(tag::VVAR, vvar)])
    }

    /// The Offset32 at `slot` of `table`.
    fn offset_at(table: &[u8], slot: usize) -> usize {
        u32::from_be_bytes(table[slot..slot + 4].try_into().unwrap()) as usize
    }

    #[test]
    fn subset_vvar_keeps_every_map_the_source_has() {
        use sigilbuzz::tables::variation_store::ItemVariationStore;
        use sigilbuzz::tables::Vvar;

        // One subtable over one all-zero region (scalar 1 everywhere),
        // so each row's delta is its stored value: row i is 10 * i.
        let store = test_store(
            1,
            1,
            &[TestSubtable {
                region_indexes: alloc::vec![0],
                rows: (0..5i8).map(|i| alloc::vec![10 * i]).collect(),
            }],
            &[0],
        );
        // Six glyphs. Advance: gid % 5. TSB: 4 - gid % 5. No BSB map.
        // Vertical origin: a three-entry map, so gids past it take the
        // last entry.
        let advance = byte_index_map(&[0, 1, 2, 3, 4, 0]);
        let tsb = byte_index_map(&[4, 3, 2, 1, 0, 4]);
        let vorg = byte_index_map(&[1, 2, 3]);
        let maps = [
            Some(advance.as_slice()),
            Some(&tsb[..]),
            None,
            Some(&vorg[..]),
        ];
        let font = font_with_vvar(&store, maps);
        let face = Face::parse_bytes(&font, 0).unwrap();
        let sink = Warnings::default();
        let out = subset_vvar(&face, &[0, 2, 5], &sink).expect("VVAR kept");
        assert!(sink.into_sorted().is_empty());

        assert_eq!(offset_at(&out, 16), 0, "no BSB map in, none out");
        let coords = [0.5];
        let vvar = Vvar::parse(&out).expect("subset VVAR parses");
        let advances: Vec<f32> = (0..3)
            .map(|g| vvar.advance_height_delta(g, &coords))
            .collect();
        assert_eq!(advances, [0.0, 20.0, 0.0]);
        let tsbs: Vec<Option<f32>> = (0..3)
            .map(|g| vvar.top_side_bearing_delta(g, &coords))
            .collect();
        assert_eq!(tsbs, [Some(40.0), Some(20.0), Some(40.0)]);
        // The core parser skips the vertical origin map; read it here.
        let store = ItemVariationStore::parse(&out[offset_at(&out, STORE_SLOT)..]).unwrap();
        let vorg_off = offset_at(&out, VVAR_VORG_SLOT);
        let origins: Vec<f32> = (0..3)
            .map(|g| {
                let (outer, inner) = read_index_map(&out, vorg_off, g).unwrap();
                store.delta(outer, inner, &coords)
            })
            .collect();
        assert_eq!(origins, [10.0, 30.0, 30.0]);
    }

    #[test]
    fn subset_vvar_without_an_advance_map_maps_glyph_ids_directly() {
        use sigilbuzz::tables::Vvar;
        let store = test_store(
            1,
            1,
            &[TestSubtable {
                region_indexes: alloc::vec![0],
                rows: (0..4i8).map(|i| alloc::vec![i + 1]).collect(),
            }],
            &[0],
        );
        let font = font_with_vvar(&store, [None; 4]);
        let face = Face::parse_bytes(&font, 0).unwrap();
        let out = subset_vvar(&face, &[0, 3], &Warnings::default()).unwrap();
        // The subset writes an explicit advance map and nothing else.
        assert_ne!(offset_at(&out, 8), 0);
        for slot in [12, 16, 20] {
            assert_eq!(offset_at(&out, slot), 0);
        }
        let vvar = Vvar::parse(&out).unwrap();
        assert_eq!(vvar.advance_height_delta(0, &[1.0]), 1.0);
        assert_eq!(vvar.advance_height_delta(1, &[1.0]), 4.0);
    }

    #[test]
    fn a_malformed_vvar_is_left_out_with_a_warning() {
        let store = test_store(1, 0, &[], &[]);
        let font = font_with_vvar(&store, [None; 4]);
        let warned = |vvar: Vec<u8>| {
            let font = crate::sfnt::build(0x4F54_544F, &[(tag::VVAR, vvar)]);
            let face = Face::parse_bytes(&font, 0).unwrap();
            let sink = Warnings::default();
            assert!(subset_vvar(&face, &[0], &sink).is_none());
            sink.into_sorted()
                .iter()
                .map(|w| (w.table, w.offset, w.context))
                .collect::<Vec<_>>()
        };
        let face = Face::parse_bytes(&font, 0).unwrap();
        let vvar = face.table_bytes(tag::VVAR).unwrap().to_vec();
        assert_eq!(
            warned(vvar[..20].to_vec()),
            [(tag::VVAR, 0, "VVAR header truncated")]
        );
        let mut far = vvar.clone();
        far[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            warned(far),
            [(tag::VVAR, STORE_SLOT, "VVAR store offset past end")]
        );
        let mut cut = vvar.clone();
        cut.truncate(26);
        assert_eq!(
            warned(cut).first().map(|w| (w.0, w.1)),
            Some((tag::VVAR, 24))
        );
        let mut major = vvar;
        major[1] = 2;
        assert_eq!(warned(major), [(tag::VVAR, 0, "VVAR major != 1")]);
    }
}
