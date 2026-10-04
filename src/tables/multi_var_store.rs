//! `MultiItemVariationStore`: VARC's multi-tuple variation store.
//!
//! Sibling primitive to [`crate::tables::ItemVariationStore`]. Where the
//! IVS used by HVAR / VVAR / MVAR / GDEF stores per-region triples
//! `(start, peak, end)` for **every** axis the font defines, the
//! `MultiItemVariationStore` (introduced for VARC in OpenType 1.10 /
//! 2024) stores **sparse** regions that name only the axes they
//! constrain. Every other axis is implicitly the "no contribution"
//! triple `(0, 0, 0)`, which evaluates to a scalar of `1.0`.
//!
//! This makes VARC's tuple representation roughly the same size as a
//! gvar tuple variation, and lets a single store carry deltas that
//! address very different axis subsets without paying for the dense
//! triple per region.
//!
//! # Layout
//!
//! ```text
//!   u16       format = 1
//!   Offset32  variationRegionListOffset    -> SparseVariationRegionList
//!   u16       itemVariationDataCount
//!   Offset32  itemVariationDataOffsets[itemVariationDataCount]
//! ```
//!
//! `SparseVariationRegionList`:
//!
//! ```text
//!   u16       regionCount
//!   Offset32  variationRegionOffsets[regionCount]
//! ```
//!
//! Each `variationRegionOffsets[i]` points at a `SparseVariationRegion`:
//!
//! ```text
//!   u16       regionAxisCount
//!   record    regionAxes[regionAxisCount]:
//!     u16     axisIndex
//!     F2DOT14 startCoord
//!     F2DOT14 peakCoord
//!     F2DOT14 endCoord
//! ```
//!
//! `MultiItemVariationData` (delta sets):
//!
//! ```text
//!   u8        format = 1
//!   u16       regionIndexCount
//!   u16       regionIndexes[regionIndexCount]
//!   CFF2Index<TupleValues>  deltaSets
//! ```
//!
//! Each delta set is a `TupleValues` stream holding one tuple per region
//! of its subtable, region after region. The consumer knows the tuple
//! length (how many transform fields or axis values vary);
//! [`MultiVarStore::resolve_deltas`] sums the tuples, each scaled by
//! its region's scalar at the given coords, the way HarfBuzz's
//! `MultiItemVariationStore::get_delta` does.

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// Reads a CFF2-style INDEX (u32 count + u8 offSize + offsets + data)
/// starting at `r`'s current position. Returns one byte slice per
/// entry, advancing `r` past the data block.
///
/// This is the four-byte-count variant used by CFF2 and VARC; CFF1's
/// `read_index` is u16-count and lives in [`crate::tables::cff`].
pub(crate) fn read_cff2_index<'a>(r: &mut Reader<'a>) -> Result<Vec<&'a [u8]>> {
    let count = r.read_u32()? as usize;
    if count == 0 {
        return Ok(Vec::new());
    }
    let off_size = r.read_u8()? as usize;
    if !(1..=4).contains(&off_size) {
        return Err(Error::Malformed {
            offset: r.position(),
            context: "CFF2 INDEX offSize out of range",
        });
    }
    // The offset array holds `count + 1` entries. Make sure the data
    // holds all of them before reserving memory sized by `count`.
    let offsets_len = count
        .checked_add(1)
        .and_then(|n| n.checked_mul(off_size))
        .ok_or(Error::Malformed {
            offset: r.position(),
            context: "CFF2 INDEX offset array overflow",
        })?;
    r.peek_bytes(offsets_len)?;
    let mut offsets = Vec::with_capacity(count + 1);
    for _ in 0..=count {
        let bytes = r.read_bytes(off_size)?;
        let mut v = 0u32;
        for &b in bytes {
            v = (v << 8) | u32::from(b);
        }
        offsets.push(v as usize);
    }
    // CFF2 offsets are 1-based and relative to the start of the data
    // block (which begins immediately after the offset table).
    let data_start = r.position();
    let total = *offsets.last().unwrap_or(&1);
    if total == 0 {
        return Err(Error::Malformed {
            offset: data_start,
            context: "CFF2 INDEX total length zero",
        });
    }
    // Bounds check. After it, `data_start + total - 1` cannot overflow.
    r.peek_bytes(total - 1)?;
    let data_end = data_start + (total - 1);
    let mut out = Vec::with_capacity(count);
    for (&a, &b) in offsets.iter().zip(offsets.iter().skip(1)) {
        if a == 0 || b < a {
            return Err(Error::Malformed {
                offset: r.position(),
                context: "CFF2 INDEX offsets non-monotone",
            });
        }
        if b > total {
            return Err(Error::Malformed {
                offset: data_start.saturating_add(b - 1),
                context: "CFF2 INDEX entry past end",
            });
        }
        let start = data_start + a - 1;
        let end = data_start + b - 1;
        // Reach back into the underlying slice via a temp reader.
        let mut tmp = *r;
        tmp.seek(start)?;
        let slice = tmp.peek_bytes(end - start)?;
        out.push(slice);
    }
    r.seek(data_end)?;
    Ok(out)
}

/// Takes `n` records from the parse budget, or fails when the store
/// would expand into more records than it has bytes.
///
/// Records that do not overlap take at least one byte each, so a
/// well-formed store always fits. A store whose offsets overlap, so
/// the same bytes parse as many large records, runs out instead of
/// allocating without bound.
fn spend(budget: &mut usize, n: usize, offset: usize) -> Result<()> {
    *budget = budget.checked_sub(n).ok_or(Error::Malformed {
        offset,
        context: "MultiItemVariationStore records overlap",
    })?;
    Ok(())
}

/// One sparse variation region: a list of `(axisIndex, start, peak,
/// end)` triples. Axes not named by the region contribute a scalar of
/// `1.0` (i.e. they are not used by this region).
#[derive(Debug, Clone)]
pub struct SparseRegion {
    /// One entry per axis the region constrains.
    pub axes: Vec<SparseAxisCoord>,
}

/// One axis triple inside a [`SparseRegion`]. `axis_index` references
/// the font's `fvar` axis order.
#[derive(Debug, Clone, Copy)]
pub struct SparseAxisCoord {
    /// Index into the font's `fvar` axis list.
    pub axis_index: u16,
    /// Start of the triangular falloff.
    pub start: f32,
    /// Peak (scalar = 1.0 here).
    pub peak: f32,
    /// End of the triangular falloff.
    pub end: f32,
}

/// One `MultiItemVariationData` subtable header. The actual
/// `deltaSets` are exposed as raw `TupleValues` byte slices via
/// [`MultiVarStore::delta_set_bytes`].
#[derive(Debug, Clone)]
struct MultiItemVarData<'a> {
    region_indexes: Vec<u16>,
    delta_sets: Vec<&'a [u8]>,
}

/// A parsed `MultiItemVariationStore`.
///
/// Offsets that repeat are parsed once: `region_slots[i]` names the
/// entry of `regions` that region index `i` resolves to, and
/// `subtable_slots` does the same for `subtables`.
#[derive(Debug, Clone)]
pub struct MultiVarStore<'a> {
    regions: Vec<SparseRegion>,
    region_slots: Vec<usize>,
    subtables: Vec<MultiItemVarData<'a>>,
    subtable_slots: Vec<usize>,
}

impl<'a> MultiVarStore<'a> {
    /// Parses a `MultiItemVariationStore` starting at byte 0 of `data`.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported MultiItemVariationStore format",
            });
        }
        let region_list_off = r.read_u32()? as usize;
        let subtable_count = r.read_u16()? as usize;
        let mut subtable_offsets = Vec::with_capacity(subtable_count);
        for _ in 0..subtable_count {
            subtable_offsets.push(r.read_u32()? as usize);
        }

        // Every parsed axis record, region index, and delta set takes
        // one unit of this budget. See `spend`.
        let mut budget = data.len();

        // SparseVariationRegionList.
        let mut rr = Reader::at(data, region_list_off)?;
        let region_count = rr.read_u16()? as usize;
        let mut region_offsets = Vec::with_capacity(region_count);
        for _ in 0..region_count {
            region_offsets.push(rr.read_u32()? as usize);
        }
        let mut regions = Vec::new();
        let mut region_slots = Vec::with_capacity(region_count);
        let mut seen: BTreeMap<usize, usize> = BTreeMap::new();
        for off in region_offsets {
            if let Some(&slot) = seen.get(&off) {
                region_slots.push(slot);
                continue;
            }
            // Region offsets are relative to the region list start.
            let abs = region_list_off.checked_add(off).ok_or(Error::Malformed {
                offset: region_list_off,
                context: "MultiItemVariationStore region offset overflow",
            })?;
            let mut sr = Reader::at(data, abs)?;
            let axis_count = sr.read_u16()? as usize;
            spend(&mut budget, axis_count, abs)?;
            let mut axes = Vec::with_capacity(axis_count);
            for _ in 0..axis_count {
                let axis_index = sr.read_u16()?;
                let start = sr.read_f2dot14()?;
                let peak = sr.read_f2dot14()?;
                let end = sr.read_f2dot14()?;
                axes.push(SparseAxisCoord {
                    axis_index,
                    start,
                    peak,
                    end,
                });
            }
            let slot = regions.len();
            regions.push(SparseRegion { axes });
            seen.insert(off, slot);
            region_slots.push(slot);
        }

        // MultiItemVariationData subtables.
        let mut subtables = Vec::new();
        let mut subtable_slots = Vec::with_capacity(subtable_count);
        let mut seen: BTreeMap<usize, usize> = BTreeMap::new();
        for off in subtable_offsets {
            if let Some(&slot) = seen.get(&off) {
                subtable_slots.push(slot);
                continue;
            }
            let mut sr = Reader::at(data, off)?;
            let format = sr.read_u8()?;
            if format != 1 {
                return Err(Error::Malformed {
                    offset: off,
                    context: "unsupported MultiItemVariationData format",
                });
            }
            let region_index_count = sr.read_u16()? as usize;
            spend(&mut budget, region_index_count, off)?;
            let mut region_indexes = Vec::with_capacity(region_index_count);
            for _ in 0..region_index_count {
                region_indexes.push(sr.read_u16()?);
            }
            let delta_sets = read_cff2_index(&mut sr)?;
            spend(&mut budget, delta_sets.len(), off)?;
            let slot = subtables.len();
            subtables.push(MultiItemVarData {
                region_indexes,
                delta_sets,
            });
            seen.insert(off, slot);
            subtable_slots.push(slot);
        }

        Ok(Self {
            regions,
            region_slots,
            subtables,
            subtable_slots,
        })
    }

    /// Number of variation regions in the region list.
    #[must_use]
    pub fn region_count(&self) -> u16 {
        self.region_slots.len() as u16
    }

    /// Number of `MultiItemVariationData` subtables (outer index max).
    #[must_use]
    pub fn subtable_count(&self) -> u16 {
        self.subtable_slots.len() as u16
    }

    /// Returns the sparse region at `region_index`, or `None` if the
    /// index is out of range.
    #[must_use]
    pub fn region(&self, region_index: u16) -> Option<&SparseRegion> {
        let slot = *self.region_slots.get(region_index as usize)?;
        self.regions.get(slot)
    }

    fn subtable(&self, outer: u16) -> Option<&MultiItemVarData<'a>> {
        let slot = *self.subtable_slots.get(outer as usize)?;
        self.subtables.get(slot)
    }

    /// Per-region scalar at `region_index` for the given normalized
    /// axis coords. Axes not named by the region contribute `1.0`
    /// (i.e. the region does not use them).
    #[must_use]
    pub fn region_scalar(&self, region_index: u16, coords: &[f32]) -> f32 {
        match self.region(region_index) {
            Some(region) => sparse_region_scalar(region, coords),
            None => 0.0,
        }
    }

    /// Scalars for each entry of `region_indexes`, in order. Each
    /// distinct region is evaluated once, so a subtable that names one
    /// large region many times does not repeat the work.
    fn scalars_for(&self, region_indexes: &[u16], coords: &[f32]) -> Vec<f32> {
        let mut cache: BTreeMap<usize, f32> = BTreeMap::new();
        region_indexes
            .iter()
            .map(|&ri| {
                let Some(&slot) = self.region_slots.get(ri as usize) else {
                    return 0.0;
                };
                let Some(region) = self.regions.get(slot) else {
                    return 0.0;
                };
                *cache
                    .entry(slot)
                    .or_insert_with(|| sparse_region_scalar(region, coords))
            })
            .collect()
    }

    /// Per-region scalars for all regions referenced by subtable `outer`,
    /// in subtable-region-index order. Returns `None` when `outer` is
    /// out of range.
    #[must_use]
    pub fn region_scalars(&self, outer: u16, coords: &[f32]) -> Option<Vec<f32>> {
        let sub = self.subtable(outer)?;
        Some(self.scalars_for(&sub.region_indexes, coords))
    }

    /// Number of regions referenced by subtable `outer`.
    #[must_use]
    pub fn variation_region_count(&self, outer: u16) -> Option<u16> {
        Some(self.subtable(outer)?.region_indexes.len() as u16)
    }

    /// Region indexes referenced by subtable `outer`.
    #[must_use]
    pub fn region_indexes(&self, outer: u16) -> Option<&[u16]> {
        Some(self.subtable(outer)?.region_indexes.as_slice())
    }

    /// Raw `TupleValues` byte slice for delta set `(outer, inner)`.
    #[must_use]
    pub fn delta_set_bytes(&self, outer: u16, inner: u32) -> Option<&'a [u8]> {
        let sub = self.subtable(outer)?;
        sub.delta_sets.get(inner as usize).copied()
    }

    /// Resolves the delta set `(outer, inner)` against `coords`,
    /// returning `value_count` summed deltas, one per output value.
    ///
    /// The delta set is a `TupleValues` stream holding one tuple of
    /// `value_count` deltas per region of the subtable, region after
    /// region (region 0's tuple, then region 1's, ...), as the spec,
    /// fontTools and HarfBuzz lay it out. Values past the last region's
    /// tuple are ignored, as in HarfBuzz. A stream that ends before it
    /// fills every tuple adds the values it holds, as HarfBuzz's
    /// `MultiItemVariationStore::get_delta` does: a region's tuple stops
    /// where the stream does, and a run whose values do not fit the
    /// bytes left ends that region's tuple after its control byte, so
    /// the next region's tuple starts at the byte after it.
    ///
    /// Returns `None` when `outer` or `inner` is out of range, or when
    /// the delta set is too short to hold `value_count` values even for
    /// one region (a control byte codes at most 64), so a huge
    /// `value_count` cannot size a huge buffer.
    #[must_use]
    pub fn resolve_deltas(
        &self,
        outer: u16,
        inner: u32,
        value_count: usize,
        coords: &[f32],
    ) -> Option<Vec<f32>> {
        let sub = self.subtable(outer)?;
        let raw = sub.delta_sets.get(inner as usize)?;
        if value_count > raw.len().saturating_mul(64) {
            return None;
        }
        let mut out = vec![0.0_f32; value_count];
        self.add_deltas(outer, inner, coords, &mut out);
        Some(out)
    }

    /// Adds the deltas of delta set `(outer, inner)` at `coords` to
    /// `out`, region by region, the way HarfBuzz's
    /// `MultiItemVariationStore::get_delta` adds them to the values it
    /// varies: each region's scalar times its tuple, added in place.
    /// Regions whose scalar is zero are skipped. Does nothing when the
    /// indices are out of range.
    ///
    /// A set that ends early adds what it holds, read as HarfBuzz's
    /// `TupleValues::fetcher_t` reads it: a region's tuple stops where
    /// the stream does, and a run whose values do not fit the bytes left
    /// ends that region's tuple (or the skip past regions whose scalar
    /// is zero) after its control byte, so the next region's tuple
    /// starts at the byte after that control byte.
    pub(crate) fn add_deltas(&self, outer: u16, inner: u32, coords: &[f32], out: &mut [f32]) {
        let Some(slot) = self.subtable_slot(outer) else {
            return;
        };
        let scalars = self.slot_scalars(slot, coords);
        self.add_slot_deltas(slot, inner, &scalars, out);
    }

    /// The parsed subtable `outer` resolves to. Outers whose offsets
    /// alias share a slot.
    pub(crate) fn subtable_slot(&self, outer: u16) -> Option<usize> {
        self.subtable_slots.get(usize::from(outer)).copied()
    }

    /// Region indexes of the subtable in `slot`: the length of its
    /// scalar list, and how many tuples each of its delta sets holds.
    pub(crate) fn slot_region_count(&self, slot: usize) -> usize {
        self.subtables
            .get(slot)
            .map_or(0, |s| s.region_indexes.len())
    }

    /// The scalar of each region index of the subtable in `slot` at
    /// `coords`, for [`Self::add_slot_deltas`]. Costs one step per
    /// region index plus one evaluation per distinct region.
    pub(crate) fn slot_scalars(&self, slot: usize, coords: &[f32]) -> Vec<f32> {
        self.subtables
            .get(slot)
            .map_or_else(Vec::new, |s| self.scalars_for(&s.region_indexes, coords))
    }

    /// [`Self::add_deltas`] for the subtable in `slot`, with its region
    /// scalars already worked out by [`Self::slot_scalars`]. Walks at
    /// most `scalars.len() * out.len()` values.
    pub(crate) fn add_slot_deltas(
        &self,
        slot: usize,
        inner: u32,
        scalars: &[f32],
        out: &mut [f32],
    ) {
        let Some(sub) = self.subtables.get(slot) else {
            return;
        };
        let Some(&raw) = sub.delta_sets.get(inner as usize) else {
            return;
        };
        let mut values = TupleFetcher::new(raw);
        let mut skip = 0usize;
        for &scalar in scalars {
            if scalar == 0.0 {
                skip = skip.saturating_add(out.len());
                continue;
            }
            values.skip(core::mem::take(&mut skip));
            values.add_to(out, scalar);
        }
    }
}

/// Reads a `TupleValues` stream lazily, as HarfBuzz's
/// `TupleValues::fetcher_t` does: reads past the end add nothing, and a
/// run whose values do not fit the remaining bytes stops the read that
/// reached it after its control byte, so the next read takes the byte
/// after that control byte as a control byte.
struct TupleFetcher<'a> {
    data: &'a [u8],
    /// Values left in the current run.
    run: usize,
    /// Bytes per value in the current run: 0 for a zero run.
    width: usize,
}

impl<'a> TupleFetcher<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            run: 0,
            width: 0,
        }
    }

    /// Starts the next run if the current one is used up. False when
    /// the stream has ended, or when the next run does not fit the bytes
    /// after its control byte, which is then used up.
    fn ensure_run(&mut self) -> bool {
        if self.run > 0 {
            return true;
        }
        let Some((&control, rest)) = self.data.split_first() else {
            return false;
        };
        self.data = rest;
        let run = usize::from(control & 0x3F) + 1;
        let width = match control & 0xC0 {
            0x80 => 0,
            0x00 => 1,
            0x40 => 2,
            _ => 4,
        };
        if rest.len() < run * width {
            return false;
        }
        self.run = run;
        self.width = width;
        true
    }

    /// The value at the head of the current run. The caller checked
    /// [`Self::ensure_run`].
    fn take(&mut self) -> i32 {
        self.run -= 1;
        let (value, rest) = match self.width {
            0 => (0, self.data),
            1 => (i32::from(self.data[0] as i8), &self.data[1..]),
            2 => (
                i32::from(i16::from_be_bytes([self.data[0], self.data[1]])),
                &self.data[2..],
            ),
            _ => (
                i32::from_be_bytes([self.data[0], self.data[1], self.data[2], self.data[3]]),
                &self.data[4..],
            ),
        };
        self.data = rest;
        value
    }

    fn skip(&mut self, mut n: usize) {
        while n > 0 && self.ensure_run() {
            let k = n.min(self.run);
            self.run -= k;
            self.data = &self.data[k * self.width..];
            n -= k;
        }
    }

    fn add_to(&mut self, out: &mut [f32], scale: f32) {
        for slot in out {
            if !self.ensure_run() {
                return;
            }
            *slot += self.take() as f32 * scale;
        }
    }
}

/// Scalar of one sparse region at `coords`: the product of its
/// per-axis falloffs, stopping early at zero.
fn sparse_region_scalar(region: &SparseRegion, coords: &[f32]) -> f32 {
    let mut scalar = 1.0_f32;
    for axis in &region.axes {
        let coord = *coords.get(axis.axis_index as usize).unwrap_or(&0.0);
        scalar *= axis_scalar(axis.start, axis.peak, axis.end, coord);
        if scalar == 0.0 {
            return 0.0;
        }
    }
    scalar
}

/// Triangular region falloff for one axis. Returns `1.0` at `peak`,
/// tapering linearly to `0.0` at `start` and `end`, as HarfBuzz's
/// `VarRegionAxis::evaluate` does. An axis whose peak is zero does not
/// constrain the region, and so does an invalid triple: one out of
/// order, or one that crosses zero.
fn axis_scalar(start: f32, peak: f32, end: f32, coord: f32) -> f32 {
    if peak == 0.0 || coord == peak {
        return 1.0;
    }
    if coord == 0.0 {
        return 0.0;
    }
    if start > peak || peak > end || (start < 0.0 && end > 0.0) {
        return 1.0;
    }
    if coord <= start || end <= coord {
        return 0.0;
    }
    if coord < peak {
        (coord - start) / (peak - start)
    } else {
        (end - coord) / (end - peak)
    }
}

// ----------------------------------------------------------------------
// TupleValues decoder: VARC's packed delta encoding.
// ----------------------------------------------------------------------
//
// Each delta-set entry is a stream of control-byte runs. The encoding
// is a backward-compatible extension of the gvar packed-delta scheme:
//
//   control byte:
//     bit 7 (0x80):  DELTAS_ARE_ZERO  (run of zeros, no payload)
//     bit 6 (0x40):  DELTAS_ARE_WORDS (i16 deltas, else i8)
//     bits 0..5:     runLength - 1    (1..64 deltas in this run)
//
//   When DELTAS_ARE_ZERO and DELTAS_ARE_WORDS are both set, the run
//   carries i32 deltas instead. (boring-expansion-spec extension.)

/// Decodes `count` values from the head of a `TupleValues` byte stream,
/// returning them with the number of bytes they took. Returns `None`
/// on truncation or when a run goes past `count`, as HarfBuzz's
/// `TupleValues::decompile` fails.
pub(crate) fn decode_tuple_values(data: &[u8], count: usize) -> Option<(Vec<i32>, usize)> {
    // One control byte yields at most 64 deltas, so `data` cannot
    // encode more than `64 * data.len()` of them. Reserve no more.
    let mut out: Vec<i32> = Vec::with_capacity(count.min(data.len().saturating_mul(64)));
    let mut i = 0usize;
    while out.len() < count {
        if i >= data.len() {
            return None;
        }
        let ctrl = data[i];
        i += 1;
        let run_len = (ctrl & 0x3F) as usize + 1;
        let zeros = ctrl & 0x80 != 0;
        let words = ctrl & 0x40 != 0;
        for _ in 0..run_len {
            if out.len() >= count {
                return None; // run overruns target: malformed
            }
            let delta: i32 = match (zeros, words) {
                (true, false) => 0,
                (true, true) => {
                    if i + 4 > data.len() {
                        return None;
                    }
                    let v = i32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
                    i += 4;
                    v
                }
                (false, true) => {
                    if i + 2 > data.len() {
                        return None;
                    }
                    let v = i32::from(i16::from_be_bytes([data[i], data[i + 1]]));
                    i += 2;
                    v
                }
                (false, false) => {
                    if i >= data.len() {
                        return None;
                    }
                    let v = data[i] as i8;
                    i += 1;
                    i32::from(v)
                }
            };
            out.push(delta);
        }
    }
    if out.len() == count {
        Some((out, i))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
        let raw = (v * 16384.0).round() as i16;
        out.extend_from_slice(&raw.to_be_bytes());
    }

    /// Builds a CFF2-style INDEX with 1-byte offsets.
    fn build_cff2_index(entries: &[&[u8]]) -> Vec<u8> {
        let count = entries.len() as u32;
        let mut out = Vec::new();
        out.extend_from_slice(&count.to_be_bytes());
        if entries.is_empty() {
            return out;
        }
        // Total payload + 1 must fit the chosen offSize. Pick the
        // smallest size that works (1 unless we exceed 254 bytes).
        let total: usize = entries.iter().map(|e| e.len()).sum();
        let off_size: u8 = if total + 1 < 256 { 1 } else { 4 };
        out.push(off_size);
        let mut cursor: u32 = 1;
        let push_off = |out: &mut Vec<u8>, off: u32, size: u8| match size {
            1 => out.push(off as u8),
            4 => out.extend_from_slice(&off.to_be_bytes()),
            _ => unreachable!(),
        };
        push_off(&mut out, cursor, off_size);
        for e in entries {
            cursor += e.len() as u32;
            push_off(&mut out, cursor, off_size);
        }
        for e in entries {
            out.extend_from_slice(e);
        }
        out
    }

    /// Builds a synthetic MultiItemVariationStore with one subtable.
    /// Each region is `(axis_index, start, peak, end)*`. Each delta-set
    /// entry is a raw TupleValues byte stream.
    fn build_store(
        regions: &[Vec<(u16, f32, f32, f32)>],
        subtable_region_indexes: &[u16],
        delta_sets: &[&[u8]],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        // Header.
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        let region_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes()); // regionListOff
        out.extend_from_slice(&1u16.to_be_bytes()); // subtable count
        let sub_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes()); // subtable[0] offset

        // Region list.
        let region_list_start = out.len() as u32;
        out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_list_start.to_be_bytes());
        out.extend_from_slice(&(regions.len() as u16).to_be_bytes());
        let region_off_array_start = out.len();
        for _ in regions {
            out.extend_from_slice(&0u32.to_be_bytes());
        }
        let mut region_offsets_relative = Vec::with_capacity(regions.len());
        for region in regions {
            let rel = (out.len() as u32) - region_list_start;
            region_offsets_relative.push(rel);
            out.extend_from_slice(&(region.len() as u16).to_be_bytes());
            for (axis_index, start, peak, end) in region {
                out.extend_from_slice(&axis_index.to_be_bytes());
                write_f2dot14(&mut out, *start);
                write_f2dot14(&mut out, *peak);
                write_f2dot14(&mut out, *end);
            }
        }
        for (i, rel) in region_offsets_relative.iter().enumerate() {
            let slot = region_off_array_start + i * 4;
            out[slot..slot + 4].copy_from_slice(&rel.to_be_bytes());
        }

        // Subtable.
        let sub_start = out.len() as u32;
        out[sub_off_slot..sub_off_slot + 4].copy_from_slice(&sub_start.to_be_bytes());
        out.push(1); // format
        out.extend_from_slice(&(subtable_region_indexes.len() as u16).to_be_bytes());
        for ri in subtable_region_indexes {
            out.extend_from_slice(&ri.to_be_bytes());
        }
        let idx = build_cff2_index(delta_sets);
        out.extend_from_slice(&idx);
        out
    }

    #[test]
    fn region_scalar_axes_outside_region_default_to_one() {
        // Region constrains only axis 1 (peak 1.0); axis 0 should be
        // ignored.
        let bytes = build_store(
            &[vec![(1, 0.0, 1.0, 1.0)]],
            &[0],
            &[&[0x00, 0x64]], // single i8 = 100
        );
        let s = MultiVarStore::parse(&bytes).unwrap();
        // axis 0 wandering does not zero the scalar.
        let v = s.region_scalar(0, &[0.5, 1.0]);
        assert!((v - 1.0).abs() < 1e-3);
    }

    #[test]
    fn region_scalar_tapers_on_named_axis() {
        let bytes = build_store(&[vec![(0, 0.0, 1.0, 1.0)]], &[0], &[&[0x00, 0x64]]);
        let s = MultiVarStore::parse(&bytes).unwrap();
        let v = s.region_scalar(0, &[0.5]);
        assert!((v - 0.5).abs() < 1e-3);
    }

    #[test]
    fn resolve_deltas_sums_region_contributions() {
        // Two regions: axis 0 peak +1, axis 0 peak -1.
        // One delta-set with two values per region, region after
        // region: [r0:100, r0:-10, r1:50, r1:5]
        //   value 0 -> 100*scalar(r0) + 50*scalar(r1)
        //   value 1 -> -10*scalar(r0) + 5*scalar(r1)
        // Build TupleValues: 4 i8 deltas, control = 0x03 (no zero, no
        // words, run_len=4).
        let payload = vec![0x03_u8, 100, (-10_i8) as u8, 50, 5];
        let bytes = build_store(
            &[vec![(0, 0.0, 1.0, 1.0)], vec![(0, -1.0, -1.0, 0.0)]],
            &[0, 1],
            &[&payload],
        );
        let s = MultiVarStore::parse(&bytes).unwrap();
        // At coord = 1.0 only region 0 contributes, scalar = 1.
        let resolved = s.resolve_deltas(0, 0, 2, &[1.0]).unwrap();
        assert!((resolved[0] - 100.0).abs() < 1e-3);
        assert!((resolved[1] - -10.0).abs() < 1e-3);
        // At coord = -1.0 only region 1 contributes.
        let resolved = s.resolve_deltas(0, 0, 2, &[-1.0]).unwrap();
        assert!((resolved[0] - 50.0).abs() < 1e-3);
        assert!((resolved[1] - 5.0).abs() < 1e-3);
    }

    #[test]
    fn axis_falloff_matches_harfbuzz_for_odd_triples() {
        // A zero peak, an out-of-order triple, and one that crosses
        // zero all leave the region unconstrained on that axis.
        assert_eq!(axis_scalar(-0.5, 0.0, 0.5, 0.3), 1.0);
        assert_eq!(axis_scalar(0.5, 0.25, 1.0, 0.3), 1.0);
        assert_eq!(axis_scalar(-0.5, 0.5, 1.0, 0.3), 1.0);
        // At the default an axis with a nonzero peak contributes zero.
        assert_eq!(axis_scalar(0.0, 0.5, 1.0, 0.0), 0.0);
        // The ends themselves are zero; inside it tapers linearly.
        assert_eq!(axis_scalar(0.25, 0.5, 1.0, 0.25), 0.0);
        assert_eq!(axis_scalar(0.25, 0.5, 1.0, 1.0), 0.0);
        assert_eq!(axis_scalar(0.0, 0.5, 1.0, 0.75), 0.5);
        assert_eq!(axis_scalar(-1.0, -1.0, 0.0, -0.25), 0.25);
    }

    #[test]
    fn tuple_values_decodes_zero_run() {
        // 0x80 | 0x03 = run of 4 zeros, no payload.
        let out = decode_tuple_values(&[0x83, 0x55], 4).unwrap();
        assert_eq!(out, (vec![0, 0, 0, 0], 1));
    }

    #[test]
    fn tuple_values_decodes_word_run() {
        // 0x40 | 0x01 = run of 2 i16s. Payload = 0x0064, 0xFFFF (-1).
        let out = decode_tuple_values(&[0x41, 0x00, 0x64, 0xFF, 0xFF], 2).unwrap();
        assert_eq!(out, (vec![100, -1], 5));
    }

    #[test]
    fn tuple_values_zero_words_combo_decodes_i32() {
        // 0xC0 | 0x00 = run of 1 i32. Payload = 0x00010000 = 65536.
        let out = decode_tuple_values(&[0xC0, 0x00, 0x01, 0x00, 0x00], 1).unwrap();
        assert_eq!(out, (vec![65536], 5));
    }

    #[test]
    fn tuple_values_truncated_returns_none() {
        // run of 4 i16s but only one i16 of payload.
        assert!(decode_tuple_values(&[0x43, 0x00, 0x01], 4).is_none());
    }

    #[test]
    fn cff2_index_round_trips_entries() {
        let bytes = build_cff2_index(&[b"abc", b"de", b"f"]);
        let mut r = Reader::new(&bytes);
        let entries = read_cff2_index(&mut r).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], b"abc");
        assert_eq!(entries[1], b"de");
        assert_eq!(entries[2], b"f");
    }

    #[test]
    fn cff2_index_empty_returns_empty_vec() {
        let bytes = 0u32.to_be_bytes().to_vec();
        let mut r = Reader::new(&bytes);
        let entries = read_cff2_index(&mut r).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn cff2_index_huge_count_fails_before_reserving_memory() {
        // count = u32::MAX with no offset array. The reader used to
        // reserve room for four billion offsets first.
        let bytes = [0xFF, 0xFF, 0xFF, 0xFF, 0x01];
        let mut r = Reader::new(&bytes);
        assert!(read_cff2_index(&mut r).is_err());
    }

    /// Store header plus a region list of `count` offsets, where
    /// `offsets(i)` gives offset `i` (relative to the region list).
    /// `tail` follows the offset array.
    fn store_with_region_offsets(count: u16, offsets: impl Fn(u16) -> u32, tail: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        out.extend_from_slice(&8u32.to_be_bytes()); // region list at 8
        out.extend_from_slice(&0u16.to_be_bytes()); // no subtables
        out.extend_from_slice(&count.to_be_bytes());
        for i in 0..count {
            out.extend_from_slice(&offsets(i).to_be_bytes());
        }
        out.extend_from_slice(tail);
        out
    }

    #[test]
    fn aliased_region_offsets_parse_once() {
        // 65535 region offsets name one region with 65535 axes. Each
        // copy used to be parsed separately, about 68 GB in total.
        let count = u16::MAX;
        let region_rel = 2 + 4 * u32::from(count);
        let mut region = Vec::new();
        region.extend_from_slice(&count.to_be_bytes());
        region.resize(2 + 8 * usize::from(count), 0);
        let bytes = store_with_region_offsets(count, |_| region_rel, &region);
        let s = MultiVarStore::parse(&bytes).unwrap();
        assert_eq!(s.region_count(), count);
        assert_eq!(s.region(count - 1).unwrap().axes.len(), usize::from(count));
    }

    #[test]
    fn overlapping_region_offsets_are_rejected() {
        // Region offsets two bytes apart inside a block of 0xFF bytes.
        // Every one of them reads as a region with 65535 axes, so the
        // store used to expand into about 68 GB of axis records.
        let count = u16::MAX;
        let region_rel = 2 + 4 * u32::from(count);
        let block = vec![0xFF_u8; 2 + 8 * 65535 + 2 * usize::from(count)];
        let bytes = store_with_region_offsets(count, |i| region_rel + 2 * u32::from(i), &block);
        assert!(matches!(
            MultiVarStore::parse(&bytes),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn aliased_subtable_offsets_parse_once() {
        // 65535 subtable offsets name one subtable whose delta-set
        // INDEX has 100 000 empty entries. Each copy used to be parsed
        // separately, about 100 GB of slices.
        let count = u16::MAX;
        let entries: u32 = 100_000;
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        let region_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&count.to_be_bytes());
        let sub_off = (out.len() + 4 * usize::from(count)) as u32;
        for _ in 0..count {
            out.extend_from_slice(&sub_off.to_be_bytes());
        }
        out.push(1); // MultiItemVariationData format
        out.extend_from_slice(&0u16.to_be_bytes()); // no region indexes
        out.extend_from_slice(&entries.to_be_bytes());
        out.push(1); // offSize
        out.resize(out.len() + entries as usize + 1, 1); // all offsets = 1
        let region_list = out.len() as u32;
        out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_list.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // no regions
        let s = MultiVarStore::parse(&out).unwrap();
        assert_eq!(s.subtable_count(), count);
        assert_eq!(s.delta_set_bytes(count - 1, entries - 1), Some(&[][..]));
    }

    #[test]
    fn resolve_deltas_huge_value_count_does_not_reserve_memory() {
        // The decoder used to reserve `value_count * region_count`
        // deltas before looking at the payload.
        let bytes = build_store(&[vec![(0, 0.0, 1.0, 1.0)]], &[0], &[&[0x00, 0x05]]);
        let s = MultiVarStore::parse(&bytes).unwrap();
        assert!(s.resolve_deltas(0, 0, 1 << 40, &[0.0]).is_none());
    }

    #[test]
    fn repeated_region_index_is_evaluated_once() {
        // One region with 65535 axes, named 65535 times by the
        // subtable. Evaluating it once per mention took about four
        // billion axis steps.
        let region = vec![(0_u16, 0.0_f32, 0.0_f32, 0.0_f32); 65535];
        let indexes = vec![0_u16; 65535];
        // 65535 zero deltas: 1023 runs of 64 plus one run of 63.
        let mut payload = vec![0xBF_u8; 1023];
        payload.push(0xBE);
        let bytes = build_store(&[region], &indexes, &[&payload]);
        let s = MultiVarStore::parse(&bytes).unwrap();
        let deltas = s.resolve_deltas(0, 0, 1, &[]).unwrap();
        assert_eq!(deltas, vec![0.0]);
        let scalars = s.region_scalars(0, &[]).unwrap();
        assert_eq!(scalars.len(), 65535);
        assert!(scalars.iter().all(|&v| v == 1.0));
    }
}
