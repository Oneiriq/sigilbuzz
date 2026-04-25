//! `MultiItemVariationStore` — VARC's multi-tuple variation store.
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
//! sigilbuzz's reader returns the raw bytes of one delta-set entry on
//! demand; full TupleValues decoding is the consumer's job (it depends
//! on whether the tuple represents a transform field, an axis-coord
//! delta, or an arbitrary scalar). The store handles region selection
//! and per-region scalar evaluation; consumers fold scalars back into
//! their own tuple decoding.

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
    let data_end = data_start + (total - 1);
    let _ = r.peek_bytes(total - 1)?; // bounds-check
    let mut out = Vec::with_capacity(count);
    for w in offsets.windows(2) {
        let a = w[0];
        let b = w[1];
        if a == 0 || b < a {
            return Err(Error::Malformed {
                offset: r.position(),
                context: "CFF2 INDEX offsets non-monotone",
            });
        }
        let start = data_start + a - 1;
        let end = data_start + b - 1;
        if end > data_end {
            return Err(Error::Malformed {
                offset: end,
                context: "CFF2 INDEX entry past end",
            });
        }
        // Reach back into the underlying slice via a temp reader.
        let mut tmp = *r;
        tmp.seek(start)?;
        let slice = tmp.peek_bytes(end - start)?;
        out.push(slice);
    }
    r.seek(data_end)?;
    Ok(out)
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
#[derive(Debug, Clone)]
pub struct MultiVarStore<'a> {
    regions: Vec<SparseRegion>,
    subtables: Vec<MultiItemVarData<'a>>,
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

        // SparseVariationRegionList.
        let mut rr = Reader::at(data, region_list_off)?;
        let region_count = rr.read_u16()? as usize;
        let mut region_offsets = Vec::with_capacity(region_count);
        for _ in 0..region_count {
            region_offsets.push(rr.read_u32()? as usize);
        }
        let mut regions = Vec::with_capacity(region_count);
        for off in region_offsets {
            // Region offsets are relative to the region list start.
            let abs = region_list_off + off;
            let mut sr = Reader::at(data, abs)?;
            let axis_count = sr.read_u16()? as usize;
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
            regions.push(SparseRegion { axes });
        }

        // MultiItemVariationData subtables.
        let mut subtables = Vec::with_capacity(subtable_count);
        for off in subtable_offsets {
            let mut sr = Reader::at(data, off)?;
            let format = sr.read_u8()?;
            if format != 1 {
                return Err(Error::Malformed {
                    offset: off,
                    context: "unsupported MultiItemVariationData format",
                });
            }
            let region_index_count = sr.read_u16()? as usize;
            let mut region_indexes = Vec::with_capacity(region_index_count);
            for _ in 0..region_index_count {
                region_indexes.push(sr.read_u16()?);
            }
            let delta_sets = read_cff2_index(&mut sr)?;
            subtables.push(MultiItemVarData {
                region_indexes,
                delta_sets,
            });
        }

        Ok(Self { regions, subtables })
    }

    /// Number of variation regions in the region list.
    #[must_use]
    pub fn region_count(&self) -> u16 {
        self.regions.len() as u16
    }

    /// Number of `MultiItemVariationData` subtables (outer index max).
    #[must_use]
    pub fn subtable_count(&self) -> u16 {
        self.subtables.len() as u16
    }

    /// Returns the sparse region at `region_index`, or `None` if the
    /// index is out of range.
    #[must_use]
    pub fn region(&self, region_index: u16) -> Option<&SparseRegion> {
        self.regions.get(region_index as usize)
    }

    /// Per-region scalar at `region_index` for the given normalized
    /// axis coords. Axes not named by the region contribute `1.0`
    /// (i.e. the region does not use them).
    #[must_use]
    pub fn region_scalar(&self, region_index: u16, coords: &[f32]) -> f32 {
        let Some(region) = self.region(region_index) else {
            return 0.0;
        };
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

    /// Per-region scalars for all regions referenced by subtable `outer`,
    /// in subtable-region-index order. Returns `None` when `outer` is
    /// out of range.
    #[must_use]
    pub fn region_scalars(&self, outer: u16, coords: &[f32]) -> Option<Vec<f32>> {
        let sub = self.subtables.get(outer as usize)?;
        let mut out = Vec::with_capacity(sub.region_indexes.len());
        for &ri in &sub.region_indexes {
            out.push(self.region_scalar(ri, coords));
        }
        Some(out)
    }

    /// Number of regions referenced by subtable `outer`.
    #[must_use]
    pub fn variation_region_count(&self, outer: u16) -> Option<u16> {
        Some(self.subtables.get(outer as usize)?.region_indexes.len() as u16)
    }

    /// Region indexes referenced by subtable `outer`.
    #[must_use]
    pub fn region_indexes(&self, outer: u16) -> Option<&[u16]> {
        Some(self.subtables.get(outer as usize)?.region_indexes.as_slice())
    }

    /// Raw `TupleValues` byte slice for delta set `(outer, inner)`.
    /// VARC's consumers (axis-value deltas, transform-field deltas)
    /// each call `decode_tuple_values` over this with the expected
    /// tuple length and fold per-region scalars back in themselves.
    #[must_use]
    pub fn delta_set_bytes(&self, outer: u16, inner: u32) -> Option<&'a [u8]> {
        let sub = self.subtables.get(outer as usize)?;
        sub.delta_sets.get(inner as usize).copied()
    }

    /// Decodes the delta-set payload for `(outer, inner)` as a flat
    /// `TupleValues` stream of `value_count × region_count` deltas
    /// (region-major: tuple-0-region-0, tuple-0-region-1, …,
    /// tuple-1-region-0, …) and resolves them against `coords`,
    /// returning `value_count` summed deltas — one per output value.
    ///
    /// Returns `None` when indices are out of range or the payload
    /// cannot decode the requested length.
    #[must_use]
    pub fn resolve_deltas(
        &self,
        outer: u16,
        inner: u32,
        value_count: usize,
        coords: &[f32],
    ) -> Option<Vec<f32>> {
        let sub = self.subtables.get(outer as usize)?;
        let raw = sub.delta_sets.get(inner as usize).copied()?;
        let region_count = sub.region_indexes.len();
        let total = value_count.checked_mul(region_count)?;
        let deltas = decode_tuple_values(raw, total)?;
        let scalars: Vec<f32> = sub
            .region_indexes
            .iter()
            .map(|&ri| self.region_scalar(ri, coords))
            .collect();
        let mut out = vec![0.0_f32; value_count];
        for v in 0..value_count {
            let row = v * region_count;
            for r in 0..region_count {
                #[allow(clippy::cast_precision_loss)]
                let d = deltas[row + r] as f32;
                out[v] += d * scalars[r];
            }
        }
        Some(out)
    }
}

/// Triangular region falloff for one axis. Returns `1.0` at `peak`,
/// tapering linearly to `0.0` at `start` and `end`. Mirrors the
/// classic `supportScalar` from the OpenType spec.
fn axis_scalar(start: f32, peak: f32, end: f32, coord: f32) -> f32 {
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
    if peak == end {
        return 0.0;
    }
    (end - coord) / (end - peak)
}

// ----------------------------------------------------------------------
// TupleValues decoder — VARC's packed delta encoding.
// ----------------------------------------------------------------------
//
// Each delta-set entry is a stream of control-byte runs. The encoding
// is a backward-compatible extension of the gvar packed-delta scheme:
//
//   control byte:
//     bit 7 (0x80) — DELTAS_ARE_ZERO  (run of zeros, no payload)
//     bit 6 (0x40) — DELTAS_ARE_WORDS (i16 deltas, else i8)
//     bits 0..5    — runLength - 1    (1..64 deltas in this run)
//
//   When DELTAS_ARE_ZERO and DELTAS_ARE_WORDS are both set, the run
//   carries i32 deltas instead. (boring-expansion-spec extension.)

/// Decodes a `TupleValues` byte stream into exactly `count` deltas.
/// Returns `None` on truncation or if the stream encodes more deltas
/// than `count`.
pub(crate) fn decode_tuple_values(data: &[u8], count: usize) -> Option<Vec<i32>> {
    let mut out: Vec<i32> = Vec::with_capacity(count);
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
                return None; // run overruns target — malformed
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
                    #[allow(clippy::cast_possible_wrap)]
                    let v = data[i] as i8;
                    i += 1;
                    i32::from(v)
                }
            };
            out.push(delta);
        }
    }
    if out.len() == count {
        Some(out)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
        #[allow(clippy::cast_possible_truncation)]
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
        let bytes = build_store(
            &[vec![(0, 0.0, 1.0, 1.0)]],
            &[0],
            &[&[0x00, 0x64]],
        );
        let s = MultiVarStore::parse(&bytes).unwrap();
        let v = s.region_scalar(0, &[0.5]);
        assert!((v - 0.5).abs() < 1e-3);
    }

    #[test]
    fn resolve_deltas_sums_region_contributions() {
        // Two regions: axis 0 peak +1, axis 0 peak -1.
        // One delta-set with two values: [r0:100, r1:50, r0:-10, r1:5]
        //   value 0 -> 100*scalar(r0) + 50*scalar(r1)
        //   value 1 -> -10*scalar(r0) + 5*scalar(r1)
        // Build TupleValues: 4 i8 deltas, control = 0x03 (no zero, no
        // words, run_len=4).
        let payload = vec![0x03_u8, 100, 50, (-10_i8) as u8, 5];
        let bytes = build_store(
            &[
                vec![(0, 0.0, 1.0, 1.0)],
                vec![(0, -1.0, -1.0, 0.0)],
            ],
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
    fn tuple_values_decodes_zero_run() {
        // 0x80 | 0x03 = run of 4 zeros, no payload.
        let out = decode_tuple_values(&[0x83], 4).unwrap();
        assert_eq!(out, vec![0, 0, 0, 0]);
    }

    #[test]
    fn tuple_values_decodes_word_run() {
        // 0x40 | 0x01 = run of 2 i16s. Payload = 0x0064, 0xFFFF (-1).
        let out =
            decode_tuple_values(&[0x41, 0x00, 0x64, 0xFF, 0xFF], 2).unwrap();
        assert_eq!(out, vec![100, -1]);
    }

    #[test]
    fn tuple_values_zero_words_combo_decodes_i32() {
        // 0xC0 | 0x00 = run of 1 i32. Payload = 0x00010000 = 65536.
        let out =
            decode_tuple_values(&[0xC0, 0x00, 0x01, 0x00, 0x00], 1).unwrap();
        assert_eq!(out, vec![65536]);
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
}
