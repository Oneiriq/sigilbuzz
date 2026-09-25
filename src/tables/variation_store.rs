//! `ItemVariationStore`: the shared primitive behind HVAR, VVAR,
//! MVAR, and feature-variations in GSUB/GPOS.
//!
//! Variable fonts express per-item deltas (advance widths, kerning
//! deltas, anchor positions, ...) as sums of region-weighted
//! contributions. Each *region* is a product of per-axis triangular
//! functions parameterized by `(start, peak, end)` in normalized
//! axis space. Each *item* carries a row of deltas, one per region
//! it participates in, plus an index table that maps region slot to
//! global region.
//!
//! # Layout
//!
//! ```text
//!   u16       format = 1
//!   Offset32  variationRegionListOffset
//!   u16       itemVariationDataCount
//!   Offset32  itemVariationDataOffsets[itemVariationDataCount]
//! ```
//!
//! `VariationRegionList` (at `variationRegionListOffset`):
//!
//! ```text
//!   u16     axisCount
//!   u16     regionCount
//!   Region  regions[regionCount]:
//!     F2DOT14 startCoord
//!     F2DOT14 peakCoord
//!     F2DOT14 endCoord
//!   (axisCount such triples per region)
//! ```
//!
//! `ItemVariationData` (at each `itemVariationDataOffset`):
//!
//! ```text
//!   u16       itemCount
//!   u16       wordDeltaCount       low 15 bits = # of word deltas
//!                                  bit 15 set means LONG_WORDS (i32/i16 pairs)
//!   u16       regionIndexCount
//!   u16       regionIndexes[regionIndexCount]
//!   DeltaSet  deltaSets[itemCount]
//! ```
//!
//! Each `DeltaSet` carries `regionIndexCount` deltas. The first
//! `wordDeltaCount & 0x7FFF` are stored as `i16` (or `i32` when
//! LONG_WORDS); the rest are `i8` (or `i16` when LONG_WORDS).
//!
//! # Evaluation
//!
//! For a given item `(outer_index, inner_index)` and normalized
//! coord vector, the delta is:
//!
//! ```text
//!   sum over regions referenced by the item:
//!     scalar(region, coords) * delta
//! ```
//!
//! where `scalar` is the product across axes of the 1D triangular
//! function: 1 inside `peak`, tapering linearly to 0 at `start`
//! and `end`. A zero-width region (start == peak == end == 0)
//! contributes a scalar of 1.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// A parsed `ItemVariationStore`.
#[derive(Debug, Clone)]
pub struct ItemVariationStore<'a> {
    data: &'a [u8],
    region_list_off: usize,
    axis_count: u16,
    region_count: u16,
    subtable_offsets: Vec<u32>,
}

impl<'a> ItemVariationStore<'a> {
    /// Parses an `ItemVariationStore` starting at byte 0 of `data`.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported ItemVariationStore format",
            });
        }
        let region_list_off = r.read_u32()? as usize;
        let subtable_count = r.read_u16()? as usize;
        let mut subtable_offsets = Vec::with_capacity(subtable_count);
        for _ in 0..subtable_count {
            subtable_offsets.push(r.read_u32()?);
        }

        let mut rr = Reader::at(data, region_list_off)?;
        let axis_count = rr.read_u16()?;
        let region_count = rr.read_u16()?;

        // Validate that region list fits. Each region is
        // axis_count * 6 bytes (3 * F2DOT14 per axis). Once this holds,
        // offset arithmetic inside the region list cannot overflow.
        let regions_start = rr.position();
        let regions_end = (region_count as usize)
            .checked_mul(axis_count as usize)
            .and_then(|n| n.checked_mul(6))
            .and_then(|n| n.checked_add(regions_start));
        if regions_end.map_or(true, |end| data.len() < end) {
            return Err(Error::Truncated {
                offset: regions_start,
                context: "ItemVariationStore region list truncated",
            });
        }

        Ok(Self {
            data,
            region_list_off,
            axis_count,
            region_count,
            subtable_offsets,
        })
    }

    /// Number of axes this store addresses.
    #[must_use]
    pub const fn axis_count(&self) -> u16 {
        self.axis_count
    }

    /// Number of regions in the region list.
    #[must_use]
    pub const fn region_count(&self) -> u16 {
        self.region_count
    }

    /// Number of ItemVariationData subtables (outer index max).
    #[must_use]
    pub fn subtable_count(&self) -> u16 {
        self.subtable_offsets.len() as u16
    }

    /// Computes the per-region scalar for `region_index` given the
    /// normalized axis coords. Returns `None` when the region
    /// index is out of range or the data is truncated; callers
    /// treat that as a contribution of zero.
    #[must_use]
    pub fn region_scalar(&self, region_index: u16, coords: &[f32]) -> Option<f32> {
        if region_index >= self.region_count {
            return None;
        }
        let axis_count = self.axis_count as usize;
        let base = self.region_list_off + 4 + region_index as usize * axis_count * 6;
        let mut scalar: f32 = 1.0;
        for axis_i in 0..axis_count {
            let off = base + axis_i * 6;
            if self.data.len() < off + 6 {
                return None;
            }
            let start = f2dot14(self.data, off);
            let peak = f2dot14(self.data, off + 2);
            let end = f2dot14(self.data, off + 4);
            let coord = *coords.get(axis_i).unwrap_or(&0.0);
            scalar *= axis_scalar(start, peak, end, coord);
            if scalar == 0.0 {
                return Some(0.0);
            }
        }
        Some(scalar)
    }

    /// Number of regions referenced by subtable `outer`. CFF2 blend
    /// uses this as `n_regions` for each delta row. Returns `None`
    /// when the subtable index is out of range or the subtable
    /// header is truncated.
    #[must_use]
    pub fn variation_region_count(&self, outer: u16) -> Option<u16> {
        let off = *self.subtable_offsets.get(outer as usize)?;
        let sub = ItemVariationData::parse(self.data, off as usize).ok()?;
        Some(sub.region_indexes.len() as u16)
    }

    /// Per-region scalars for subtable `outer`, in subtable-region
    /// order. CFF2 blend multiplies each column of deltas by the
    /// corresponding entry.
    #[must_use]
    pub fn region_scalars(&self, outer: u16, coords: &[f32]) -> Option<Vec<f32>> {
        let off = *self.subtable_offsets.get(outer as usize)?;
        let sub = ItemVariationData::parse(self.data, off as usize).ok()?;
        let scalars = self.scalars_for(&sub.region_indexes, coords);
        Some(scalars.into_iter().map(|s| s.unwrap_or(0.0)).collect())
    }

    /// [`Self::region_scalar`] for each entry of `region_indexes`, in
    /// order. Each distinct region is evaluated once, so a subtable
    /// that names one many-axis region many times does not repeat the
    /// work.
    fn scalars_for(&self, region_indexes: &[u16], coords: &[f32]) -> Vec<Option<f32>> {
        let mut cache: BTreeMap<u16, Option<f32>> = BTreeMap::new();
        region_indexes
            .iter()
            .map(|&ri| {
                *cache
                    .entry(ri)
                    .or_insert_with(|| self.region_scalar(ri, coords))
            })
            .collect()
    }

    /// Evaluates the delta for item `(outer, inner)` at the given
    /// normalized coords. Returns `0.0` for out-of-range indices.
    #[must_use]
    pub fn delta(&self, outer: u16, inner: u16, coords: &[f32]) -> f32 {
        let Some(subtable_off) = self.subtable_offsets.get(outer as usize).copied() else {
            return 0.0;
        };
        let subtable_off = subtable_off as usize;
        let Some(subtable) = ItemVariationData::parse(self.data, subtable_off).ok() else {
            return 0.0;
        };
        if inner >= subtable.item_count {
            return 0.0;
        }
        let Some(deltas) = subtable.deltas_for(inner) else {
            return 0.0;
        };
        let scalars = self.scalars_for(&subtable.region_indexes, coords);
        let mut out: f32 = 0.0;
        for (slot, d) in deltas.iter().enumerate() {
            let Some(&scalar) = scalars.get(slot) else {
                break;
            };
            let Some(scalar) = scalar else {
                continue;
            };
            let delta_f = *d as f32;
            out += scalar * delta_f;
        }
        out
    }
}

/// Decodes an F2DOT14 at absolute offset `off` without advancing a
/// reader.
fn f2dot14(data: &[u8], off: usize) -> f32 {
    let raw = i16::from_be_bytes([data[off], data[off + 1]]);
    f32::from(raw) / 16384.0
}

/// Triangular region function for one axis. Returns `1.0` at
/// `peak`, tapering linearly to `0.0` at `start` and `end`, and
/// clamped to zero outside that range. Matches the OpenType spec's
/// `supportScalar` function.
fn axis_scalar(start: f32, peak: f32, end: f32, coord: f32) -> f32 {
    // Per spec: a region with peak == 0 on any axis evaluates to 1
    // on that axis. The axis is "not used by this region".
    if peak == 0.0 && start <= 0.0 && end >= 0.0 {
        return 1.0;
    }
    if coord == peak {
        return 1.0;
    }
    if coord < start || coord > end {
        return 0.0;
    }
    if coord < peak {
        if peak == start {
            return 0.0;
        }
        return (coord - start) / (peak - start);
    }
    // coord > peak
    if peak == end {
        return 0.0;
    }
    (end - coord) / (end - peak)
}

// --------------------------------------------------------------------------
// ItemVariationData subtable.
// --------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ItemVariationData<'a> {
    data: &'a [u8],
    item_count: u16,
    word_delta_count: u16,
    long_words: bool,
    region_indexes: Vec<u16>,
    delta_sets_off: usize,
    delta_set_size: usize,
}

impl<'a> ItemVariationData<'a> {
    fn parse(full: &'a [u8], start: usize) -> Result<Self> {
        let mut r = Reader::at(full, start)?;
        let item_count = r.read_u16()?;
        let word_delta_count_raw = r.read_u16()?;
        let long_words = word_delta_count_raw & 0x8000 != 0;
        let word_delta_count = word_delta_count_raw & 0x7FFF;
        let region_index_count = r.read_u16()?;

        let mut region_indexes = Vec::with_capacity(region_index_count as usize);
        for _ in 0..region_index_count {
            region_indexes.push(r.read_u16()?);
        }
        let delta_sets_off = r.position();
        // Size of one delta set: wordDeltaCount "wide" entries
        // (i16 or i32) + (regionIndexCount - wordDeltaCount)
        // "narrow" entries (i8 or i16).
        let (wide, narrow) = if long_words { (4, 2) } else { (2, 1) };
        if word_delta_count > region_index_count {
            return Err(Error::Malformed {
                offset: start,
                context: "ItemVariationData wordDeltaCount > regionIndexCount",
            });
        }
        let delta_set_size = word_delta_count as usize * wide
            + (region_index_count - word_delta_count) as usize * narrow;
        // Once this holds, row offsets in `deltas_for` cannot overflow.
        let need = (item_count as usize)
            .checked_mul(delta_set_size)
            .and_then(|n| n.checked_add(delta_sets_off));
        if need.map_or(true, |need| full.len() < need) {
            return Err(Error::Truncated {
                offset: delta_sets_off,
                context: "ItemVariationData delta sets truncated",
            });
        }

        Ok(Self {
            data: full,
            item_count,
            word_delta_count,
            long_words,
            region_indexes,
            delta_sets_off,
            delta_set_size,
        })
    }

    fn deltas_for(&self, inner: u16) -> Option<Vec<i32>> {
        if inner >= self.item_count {
            return None;
        }
        let row = self.delta_sets_off + inner as usize * self.delta_set_size;
        let mut out = Vec::with_capacity(self.region_indexes.len());
        let mut cursor = row;
        for slot in 0..self.region_indexes.len() {
            let is_wide = (slot as u16) < self.word_delta_count;
            let (bytes, advance) = match (is_wide, self.long_words) {
                (true, true) => (4, 4), // i32
                // i16: either wide/short-word or narrow/long-word.
                (true, false) | (false, true) => (2, 2),
                (false, false) => (1, 1), // i8
            };
            if self.data.len() < cursor + bytes {
                return None;
            }
            let value: i32 = match (is_wide, self.long_words) {
                (true, true) => i32::from_be_bytes([
                    self.data[cursor],
                    self.data[cursor + 1],
                    self.data[cursor + 2],
                    self.data[cursor + 3],
                ]),
                (true, false) | (false, true) => i32::from(i16::from_be_bytes([
                    self.data[cursor],
                    self.data[cursor + 1],
                ])),
                (false, false) => i32::from(self.data[cursor] as i8),
            };
            out.push(value);
            cursor += advance;
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
        #[allow(clippy::cast_possible_truncation)]
        let raw = (v * 16384.0).round() as i16;
        out.extend_from_slice(&raw.to_be_bytes());
    }

    /// (regionIndexes, deltaSets, longWords) per subtable.
    type TestSubtable = (Vec<u16>, Vec<Vec<i32>>, bool);

    fn build_store(
        axis_count: u16,
        regions: &[&[(f32, f32, f32)]],
        subtables: &[TestSubtable],
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

        // Region list.
        let region_list_start = out.len() as u32;
        out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_list_start.to_be_bytes());
        out.extend_from_slice(&axis_count.to_be_bytes());
        out.extend_from_slice(&(regions.len() as u16).to_be_bytes());
        for region in regions {
            assert_eq!(region.len(), axis_count as usize);
            for (s, p, e) in *region {
                write_f2dot14(&mut out, *s);
                write_f2dot14(&mut out, *p);
                write_f2dot14(&mut out, *e);
            }
        }

        // Subtables.
        for (i, (region_indexes, delta_sets, long_words)) in subtables.iter().enumerate() {
            let start = out.len() as u32;
            let slot = subtable_slot_start + i * 4;
            out[slot..slot + 4].copy_from_slice(&start.to_be_bytes());

            out.extend_from_slice(&(delta_sets.len() as u16).to_be_bytes());
            // Store all deltas as wide; wordDeltaCount = regionIndexCount.
            let word_delta_count = region_indexes.len() as u16;
            let mut wdc_word = word_delta_count;
            if *long_words {
                wdc_word |= 0x8000;
            }
            out.extend_from_slice(&wdc_word.to_be_bytes());
            out.extend_from_slice(&(region_indexes.len() as u16).to_be_bytes());
            for ri in region_indexes {
                out.extend_from_slice(&ri.to_be_bytes());
            }
            for set in delta_sets {
                assert_eq!(set.len(), region_indexes.len());
                for d in set {
                    if *long_words {
                        out.extend_from_slice(&d.to_be_bytes());
                    } else {
                        #[allow(clippy::cast_possible_truncation)]
                        let v = *d as i16;
                        out.extend_from_slice(&v.to_be_bytes());
                    }
                }
            }
        }

        out
    }

    #[test]
    fn region_scalar_peaks_at_one() {
        // One axis, one region (-1 ... 1 ... 1). At coord = 1.0 scalar = 1.
        let bytes = build_store(
            1,
            &[&[(-1.0, 1.0, 1.0)]],
            &[(alloc::vec![0], alloc::vec![alloc::vec![100]], false)],
        );
        let s = ItemVariationStore::parse(&bytes).unwrap();
        let v = s.region_scalar(0, &[1.0]).unwrap();
        assert!((v - 1.0).abs() < 1e-3);
    }

    #[test]
    fn region_scalar_tapers_linearly() {
        let bytes = build_store(
            1,
            &[&[(0.0, 1.0, 1.0)]],
            &[(alloc::vec![0], alloc::vec![alloc::vec![100]], false)],
        );
        let s = ItemVariationStore::parse(&bytes).unwrap();
        // coord = 0.5 is halfway between start (0) and peak (1) -> 0.5.
        let v = s.region_scalar(0, &[0.5]).unwrap();
        assert!((v - 0.5).abs() < 1e-3);
    }

    #[test]
    fn delta_sums_contributions_across_regions() {
        // Two regions, one item with delta 100 in region 0 and 50 in
        // region 1.
        let bytes = build_store(
            1,
            &[&[(0.0, 1.0, 1.0)], &[(-1.0, -1.0, 0.0)]],
            &[(alloc::vec![0, 1], alloc::vec![alloc::vec![100, 50]], false)],
        );
        let s = ItemVariationStore::parse(&bytes).unwrap();

        // At coord = 1.0: region 0 scalar = 1, region 1 scalar = 0
        // -> delta = 100.
        let d = s.delta(0, 0, &[1.0]);
        assert!((d - 100.0).abs() < 1e-3);

        // At coord = -1.0: region 0 scalar = 0, region 1 scalar = 1
        // -> delta = 50.
        let d = s.delta(0, 0, &[-1.0]);
        assert!((d - 50.0).abs() < 1e-3);
    }

    #[test]
    fn out_of_range_indices_yield_zero_delta() {
        let bytes = build_store(
            1,
            &[&[(0.0, 1.0, 1.0)]],
            &[(alloc::vec![0], alloc::vec![alloc::vec![100]], false)],
        );
        let s = ItemVariationStore::parse(&bytes).unwrap();
        assert!(s.delta(9, 0, &[1.0]).abs() < 1e-6);
        assert!(s.delta(0, 9, &[1.0]).abs() < 1e-6);
    }

    #[test]
    fn long_words_flag_reads_i32_deltas() {
        let bytes = build_store(
            1,
            &[&[(0.0, 1.0, 1.0)]],
            &[(alloc::vec![0], alloc::vec![alloc::vec![100_000]], true)],
        );
        let s = ItemVariationStore::parse(&bytes).unwrap();
        let d = s.delta(0, 0, &[1.0]);
        assert!((d - 100_000.0).abs() < 1.0);
    }

    #[test]
    fn repeated_region_index_is_evaluated_once() {
        // One region over 65535 axes, named 32767 times by one item
        // (the most the test builder can encode). Every axis has peak
        // 0, so no axis cuts the walk short. Evaluating the region once
        // per mention took about two billion axis steps per delta.
        let axes: u16 = u16::MAX;
        let mentions: u16 = 0x7FFF;
        let region = alloc::vec![(0.0_f32, 0.0_f32, 0.0_f32); usize::from(axes)];
        let indexes = alloc::vec![0_u16; usize::from(mentions)];
        let deltas = alloc::vec![alloc::vec![1_i32; usize::from(mentions)]];
        let bytes = build_store(axes, &[&region], &[(indexes, deltas, false)]);
        let s = ItemVariationStore::parse(&bytes).unwrap();
        assert_eq!(s.delta(0, 0, &[]), f32::from(mentions));
        let scalars = s.region_scalars(0, &[]).unwrap();
        assert_eq!(scalars.len(), usize::from(mentions));
    }
}
