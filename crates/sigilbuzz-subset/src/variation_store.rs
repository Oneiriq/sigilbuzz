//! Item-Variation-Store rewriter shared by the HVAR subsetter (and
//! eventually VVAR / MVAR, which this crate does not subset yet).
//!
//! HVAR maps each glyph to an `(outer, inner)` pair via a
//! `DeltaSetIndexMap`; the pair indexes into an `ItemVariationStore`
//! whose `ItemVariationData` subtables carry one delta row per
//! item, plus the per-region scalars that combine into the final
//! advance delta. Subsetting walks every kept glyph, collects the
//! `(outer, inner)` pairs it references, and emits a fresh store
//! that holds only those rows (deduped across the source) plus
//! the regions those rows reference.
//!
//! The rewriter is generic over the rows: callers (HVAR today,
//! VVAR / MVAR later) pull the rows they need with [`pull_row`],
//! dedupe them, and hand them to [`rebuild_store`] in the order they
//! should be emitted. The output is a single `ItemVariationData`
//! subtable in outer-index 0; per-glyph mappings are then short
//! `(0, new_inner)` pairs the caller can pack into the densest
//! `DeltaSetIndexMap` the inner range allows.

use alloc::vec::Vec;

use sigilbuzz::tables::parse::Reader;

use crate::SubsetError;

/// One row pulled out of the source store: the deltas for every
/// region the row references, in the source's region-index order.
/// We carry the full source region indexes so the rewriter can
/// remap them to the deduped output region list.
#[derive(Debug, Clone)]
pub(crate) struct PulledRow {
    /// Source region indexes (in subtable-region order).
    pub region_indexes: Vec<u16>,
    /// Source deltas (one per region index, same order).
    pub deltas: Vec<i32>,
}

/// Result of rebuilding an `ItemVariationStore` from a kept-row set.
pub(crate) struct RebuiltStore {
    /// Serialized `ItemVariationStore` bytes. Caller embeds these
    /// at some offset inside the new HVAR.
    pub bytes: Vec<u8>,
    /// Number of items in the rebuilt store (== kept-row count).
    /// Only the unit tests read it back.
    #[cfg(test)]
    pub item_count: u32,
}

/// Pulls the row at `(outer, inner)` out of a source HVAR's store.
/// Returns `None` when the indexes do not resolve to a valid row;
/// the HVAR rewriter treats that as "no advance delta for this gid"
/// and falls back to mapping the gid at `(0, 0)` of the output.
///
/// The pull is implemented against raw bytes (not the parser in
/// `sigilbuzz::tables::variation_store`) because the parser only
/// surfaces evaluated deltas, and we need the *unscaled* source
/// integers to rebuild a byte-exact copy.
pub(crate) fn pull_row(
    store_bytes: &[u8],
    outer: u16,
    inner: u16,
) -> Result<Option<PulledRow>, SubsetError> {
    let mut r = Reader::new(store_bytes);
    let format = r.read_u16().map_err(|_| truncated("ivs format"))?;
    if format != 1 {
        return Err(SubsetError::Unsupported(
            "ItemVariationStore format != 1 in HVAR",
        ));
    }
    let _region_list_off = r.read_u32().map_err(|_| truncated("ivs region off"))?;
    let subtable_count = r.read_u16().map_err(|_| truncated("ivs subtable count"))?;
    if outer >= subtable_count {
        return Ok(None);
    }
    // Skip ahead to the subtable offset for `outer`.
    r.skip(outer as usize * 4)
        .map_err(|_| truncated("ivs subtable skip"))?;
    let subtable_off = r.read_u32().map_err(|_| truncated("ivs subtable off"))? as usize;

    let row = read_subtable_row(store_bytes, subtable_off, inner)?;
    Ok(row)
}

/// Reads region (start, peak, end) triples from a region list, given
/// the absolute offset of the region list inside the store bytes.
pub(crate) fn read_regions(store_bytes: &[u8]) -> Result<(u16, Vec<RegionTriple>), SubsetError> {
    let mut r = Reader::new(store_bytes);
    let format = r.read_u16().map_err(|_| truncated("ivs format"))?;
    if format != 1 {
        return Err(SubsetError::Unsupported(
            "ItemVariationStore format != 1 in HVAR",
        ));
    }
    let region_list_off = r.read_u32().map_err(|_| truncated("ivs region off"))? as usize;

    let mut rr =
        Reader::at(store_bytes, region_list_off).map_err(|_| truncated("ivs region list"))?;
    let axis_count = rr.read_u16().map_err(|_| truncated("region axis count"))?;
    let region_count = rr.read_u16().map_err(|_| truncated("region count"))?;
    let mut out = Vec::with_capacity(region_count as usize);
    for _ in 0..region_count {
        let mut axes = Vec::with_capacity(axis_count as usize);
        for _ in 0..axis_count {
            let start = rr.read_f2dot14().map_err(|_| truncated("region start"))?;
            let peak = rr.read_f2dot14().map_err(|_| truncated("region peak"))?;
            let end = rr.read_f2dot14().map_err(|_| truncated("region end"))?;
            axes.push((start, peak, end));
        }
        out.push(RegionTriple { axes });
    }
    Ok((axis_count, out))
}

/// One region's per-axis (start, peak, end) triples.
#[derive(Debug, Clone)]
pub(crate) struct RegionTriple {
    pub axes: Vec<(f32, f32, f32)>,
}

/// Number of distinct `u16` region indexes.
const REGION_INDEX_SPACE: usize = 1 << 16;

/// Collects the union of regions referenced by `rows` in
/// first-appearance order. Also returns, per source region index, its
/// position in that list. Region indexes are `u16`, so the position
/// table covers every index and each lookup is constant time.
fn kept_regions(rows: &[PulledRow]) -> (Vec<u16>, Vec<Option<u32>>) {
    let mut kept: Vec<u16> = Vec::new();
    let mut position: Vec<Option<u32>> = alloc::vec![None; REGION_INDEX_SPACE];
    for row in rows {
        for &ri in &row.region_indexes {
            if let Some(slot @ None) = position.get_mut(usize::from(ri)) {
                *slot = Some(kept.len() as u32);
                kept.push(ri);
            }
        }
    }
    (kept, position)
}

/// Upper bound on the byte length [`rebuild_store`] produces for the
/// same inputs, computed without building it. Callers compare it
/// against a budget before rebuilding, because every row is padded to
/// the union of all rows' regions.
pub(crate) fn rebuilt_store_len(rows: &[PulledRow], source_axis_count: u16) -> usize {
    let (kept, _) = kept_regions(rows);
    let regions = kept.len();
    let region_list = regions
        .saturating_mul(usize::from(source_axis_count))
        .saturating_mul(6);
    let subtable_rows = rows.len().saturating_mul(regions).saturating_mul(2);
    // Store header (12) + region list header (4) + subtable header
    // (6) + region index list + rows.
    22usize
        .saturating_add(region_list)
        .saturating_add(regions.saturating_mul(2))
        .saturating_add(subtable_rows)
}

/// Rebuilds an `ItemVariationStore` from a kept-row set. The output
/// has exactly one `ItemVariationData` subtable (outer index 0)
/// containing every kept row in the order supplied. Region indexes
/// are remapped through the deduped output region list. All deltas
/// are emitted in the wide form (i16) for simplicity. Every source
/// HVAR we have measured already uses this form for the bulk of its
/// rows, and the byte-budget difference for short rows is small
/// against the table's overall size.
pub(crate) fn rebuild_store(
    rows: &[PulledRow],
    source_axis_count: u16,
    source_regions: &[RegionTriple],
) -> RebuiltStore {
    // Collect the union of regions referenced by the kept rows in
    // first-appearance order, which keeps the output deterministic.
    let (kept_regions, position) = kept_regions(rows);

    // The new subtable references *every* kept region in its
    // per-row delta arrays. Source rows that referenced only a
    // subset of regions are padded with zero deltas in the slots
    // they didn't reference. This lets the output use a single
    // shared region-index list for every row, which keeps the
    // emitter trivial and the layout deterministic.
    let region_index_count = kept_regions.len() as u16;

    // ----- Serialize -----
    let mut out: Vec<u8> = Vec::new();

    // Header.
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    let region_list_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // patched below
    out.extend_from_slice(&1u16.to_be_bytes()); // subtableCount = 1
    let subtable_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // patched below

    // Region list.
    let region_list_off = out.len() as u32;
    patch_u32(&mut out, region_list_off_slot, region_list_off);
    out.extend_from_slice(&source_axis_count.to_be_bytes());
    out.extend_from_slice(&region_index_count.to_be_bytes());
    for &old_i in &kept_regions {
        let triples = source_regions
            .get(old_i as usize)
            .map(|r| r.axes.as_slice())
            .unwrap_or(&[]);
        // If the source had fewer axes than the header advertises,
        // pad with zero axes (keeps the byte layout valid).
        for axis_i in 0..source_axis_count as usize {
            let (start, peak, end) = triples.get(axis_i).copied().unwrap_or((0.0, 0.0, 0.0));
            write_f2dot14(&mut out, start);
            write_f2dot14(&mut out, peak);
            write_f2dot14(&mut out, end);
        }
    }

    // Subtable.
    let subtable_off = out.len() as u32;
    patch_u32(&mut out, subtable_off_slot, subtable_off);

    let item_count = rows.len() as u16;
    out.extend_from_slice(&item_count.to_be_bytes());
    // wordDeltaCount == regionIndexCount (every column is wide i16),
    // long_words = false. wordDeltaCount mask is the low 15 bits.
    let wdc_word: u16 = region_index_count;
    out.extend_from_slice(&wdc_word.to_be_bytes());
    out.extend_from_slice(&region_index_count.to_be_bytes());
    for new_i in 0..region_index_count {
        out.extend_from_slice(&new_i.to_be_bytes());
    }
    // One row per item: emit deltas in new-region order. For each
    // new region index, take the row's first slot that references
    // it. If none does, write zero.
    let mut values: Vec<Option<i16>> = alloc::vec![None; kept_regions.len()];
    for row in rows {
        values.fill(None);
        for (slot, &row_old_region) in row.region_indexes.iter().enumerate() {
            let new_slot = position
                .get(usize::from(row_old_region))
                .copied()
                .flatten()
                .and_then(|k| values.get_mut(k as usize));
            if let Some(value @ None) = new_slot {
                let raw = row.deltas.get(slot).copied().unwrap_or(0);
                *value = Some(raw.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16);
            }
        }
        for value in &values {
            out.extend_from_slice(&value.unwrap_or(0).to_be_bytes());
        }
    }

    RebuiltStore {
        bytes: out,
        #[cfg(test)]
        item_count: u32::from(item_count),
    }
}

/// Overwrites the big-endian `u32` slot at `off` in `out`.
fn patch_u32(out: &mut [u8], off: usize, value: u32) {
    if let Some(slot) = out.get_mut(off..).and_then(<[u8]>::first_chunk_mut::<4>) {
        *slot = value.to_be_bytes();
    }
}

fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
    let raw = (v * 16384.0).round() as i16;
    out.extend_from_slice(&raw.to_be_bytes());
}

/// Returns a `SubsetError::Unsupported` with a short context.
fn truncated(ctx: &'static str) -> SubsetError {
    SubsetError::Unsupported(ctx)
}

/// Reads one row from an `ItemVariationData` subtable, given the
/// absolute offset of the subtable inside the store bytes.
fn read_subtable_row(
    store_bytes: &[u8],
    subtable_off: usize,
    inner: u16,
) -> Result<Option<PulledRow>, SubsetError> {
    let mut r =
        Reader::at(store_bytes, subtable_off).map_err(|_| truncated("ivs subtable seek"))?;
    let item_count = r.read_u16().map_err(|_| truncated("ivs item count"))?;
    if inner >= item_count {
        return Ok(None);
    }
    let word_delta_count_raw = r
        .read_u16()
        .map_err(|_| truncated("ivs word delta count"))?;
    let long_words = word_delta_count_raw & 0x8000 != 0;
    let word_delta_count = word_delta_count_raw & 0x7FFF;
    let region_index_count = r
        .read_u16()
        .map_err(|_| truncated("ivs region index count"))?;
    if word_delta_count > region_index_count {
        return Err(SubsetError::Unsupported(
            "ItemVariationData wordDeltaCount > regionIndexCount",
        ));
    }
    let mut region_indexes = Vec::with_capacity(region_index_count as usize);
    for _ in 0..region_index_count {
        region_indexes.push(r.read_u16().map_err(|_| truncated("ivs region index"))?);
    }
    let delta_sets_off = r.position();

    let (wide, narrow) = if long_words { (4, 2) } else { (2, 1) };
    let word_delta_count = usize::from(word_delta_count);
    let delta_set_size =
        word_delta_count * wide + (usize::from(region_index_count) - word_delta_count) * narrow;
    let row = usize::from(inner)
        .checked_mul(delta_set_size)
        .and_then(|row_off| delta_sets_off.checked_add(row_off))
        .and_then(|row_off| store_bytes.get(row_off..))
        .and_then(|rest| rest.get(..delta_set_size))
        .ok_or(truncated("ivs delta set row"))?;

    let (wide_bytes, narrow_bytes) = row
        .split_at_checked(word_delta_count * wide)
        .ok_or(truncated("ivs delta set row"))?;
    let wide_values = wide_bytes.chunks_exact(wide).map(|b| match *b {
        [b0, b1, b2, b3] => i32::from_be_bytes([b0, b1, b2, b3]),
        [b0, b1] => i32::from(i16::from_be_bytes([b0, b1])),
        _ => 0,
    });
    let narrow_values = narrow_bytes.chunks_exact(narrow).map(|b| match *b {
        [b0, b1] => i32::from(i16::from_be_bytes([b0, b1])),
        [b0] => i32::from(b0 as i8),
        _ => 0,
    });
    let deltas: Vec<i32> = wide_values.chain(narrow_values).collect();

    Ok(Some(PulledRow {
        region_indexes,
        deltas,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_f2dot14_t(out: &mut Vec<u8>, v: f32) {
        let raw = (v * 16384.0).round() as i16;
        out.extend_from_slice(&raw.to_be_bytes());
    }

    fn build_test_store(
        axis_count: u16,
        regions: &[Vec<(f32, f32, f32)>],
        subtables: &[(Vec<u16>, Vec<Vec<i16>>)],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        let region_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(subtables.len() as u16).to_be_bytes());
        let subtable_slot_start = out.len();
        for _ in 0..subtables.len() {
            out.extend_from_slice(&0u32.to_be_bytes());
        }
        let region_start = out.len() as u32;
        out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_start.to_be_bytes());
        out.extend_from_slice(&axis_count.to_be_bytes());
        out.extend_from_slice(&(regions.len() as u16).to_be_bytes());
        for region in regions {
            for (s, p, e) in region {
                write_f2dot14_t(&mut out, *s);
                write_f2dot14_t(&mut out, *p);
                write_f2dot14_t(&mut out, *e);
            }
        }
        for (i, (region_indexes, rows)) in subtables.iter().enumerate() {
            let s = out.len() as u32;
            let slot = subtable_slot_start + i * 4;
            out[slot..slot + 4].copy_from_slice(&s.to_be_bytes());
            out.extend_from_slice(&(rows.len() as u16).to_be_bytes());
            out.extend_from_slice(&(region_indexes.len() as u16).to_be_bytes());
            out.extend_from_slice(&(region_indexes.len() as u16).to_be_bytes());
            for ri in region_indexes {
                out.extend_from_slice(&ri.to_be_bytes());
            }
            for row in rows {
                for d in row {
                    out.extend_from_slice(&d.to_be_bytes());
                }
            }
        }
        out
    }

    #[test]
    fn pull_row_returns_source_deltas() {
        let store = build_test_store(
            1,
            &alloc::vec![alloc::vec![(0.0, 1.0, 1.0)]],
            &[(
                alloc::vec![0],
                alloc::vec![alloc::vec![100], alloc::vec![200]],
            )],
        );
        let row = pull_row(&store, 0, 1).unwrap().expect("row exists");
        assert_eq!(row.region_indexes, alloc::vec![0]);
        assert_eq!(row.deltas, alloc::vec![200]);
    }

    #[test]
    fn pull_row_returns_none_for_oob() {
        let store = build_test_store(
            1,
            &alloc::vec![alloc::vec![(0.0, 1.0, 1.0)]],
            &[(alloc::vec![0], alloc::vec![alloc::vec![100]])],
        );
        assert!(pull_row(&store, 9, 0).unwrap().is_none());
        assert!(pull_row(&store, 0, 9).unwrap().is_none());
    }

    #[test]
    fn rebuild_dedupes_regions_in_first_appearance_order() {
        // Two source regions; row A references region 1, row B
        // references region 0. Output region order must be [1, 0]
        // because that's the order rows are pulled.
        let regions = alloc::vec![
            RegionTriple {
                axes: alloc::vec![(0.0, 1.0, 1.0)],
            },
            RegionTriple {
                axes: alloc::vec![(-1.0, -1.0, 0.0)],
            },
        ];
        let rows = alloc::vec![
            PulledRow {
                region_indexes: alloc::vec![1],
                deltas: alloc::vec![10],
            },
            PulledRow {
                region_indexes: alloc::vec![0],
                deltas: alloc::vec![20],
            },
        ];
        let rebuilt = rebuild_store(&rows, 1, &regions);
        // Item count is 2.
        assert_eq!(rebuilt.item_count, 2);
        // Round-trip the output through pull_row to verify the
        // dedup actually placed each row's delta in the right slot.
        let pulled0 = pull_row(&rebuilt.bytes, 0, 0).unwrap().unwrap();
        let pulled1 = pull_row(&rebuilt.bytes, 0, 1).unwrap().unwrap();
        // Output region order is [old_region 1, old_region 0]; the
        // first row referenced old_region 1 and so its delta lives
        // in new-region slot 0.
        assert_eq!(pulled0.deltas, alloc::vec![10, 0]);
        // Second row referenced old_region 0 -> slot 1.
        assert_eq!(pulled1.deltas, alloc::vec![0, 20]);
    }
}
