//! `VARC` (Variable Composite Glyphs) subsetting.
//!
//! This module owns the VARC subset path: closure expansion (walking
//! every kept VARC-covered gid for its referenced component gids) and
//! the table re-emit (Coverage / VarCompositeGlyph rewrite plus a
//! pruned MultiVarStore).
//!
//! # Closure walk
//!
//! For every VARC-covered gid in the kept set, walk the component
//! records and pull each referenced gid into the kept set. A worklist
//! visits each newly kept gid once, so pulled-in gids that are
//! themselves VARC-covered cascade, and component cycles terminate.
//!
//! # Component record layout (recap)
//!
//! Each VarComposite record is a flag-driven variable-length blob:
//!
//! ```text
//!   uint32var flags
//!   u16 | u24 gid              // u24 when VC_GID_IS_24BIT
//!   uint32var conditionIndex   // when VC_HAVE_CONDITION
//!   uint32var axisIndicesIndex // when VC_HAVE_AXES
//!   TupleValues axisValues     // when VC_HAVE_AXES (one run-control + payload)
//!   uint32var axisVarIndex     // when VC_AXIS_VALUES_HAVE_VARIATION
//!   uint32var transformVarIdx  // when VC_TRANSFORM_HAS_VARIATION
//!   i16 fields                 // one per present transform field
//! ```

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::tables::Varc;
use sigilbuzz::Face;

use crate::util::{WorkBudget, WORK_LIMIT};
use crate::{GlyphId, SubsetError};

// Variable-component flag bits (mirrors the parser's set, kept private
// to this module so the subset path doesn't depend on parser internals).
// Only the flags that drive byte-layout decisions in the record walker
// are named here; bits like RESET_UNSPECIFIED_AXES affect *evaluation*
// but not byte length, so the subset path doesn't need to track them.
const VC_HAVE_AXES: u32 = 1 << 1;
const VC_AXIS_VALUES_HAVE_VARIATION: u32 = 1 << 2;
const VC_TRANSFORM_HAS_VARIATION: u32 = 1 << 3;
const VC_HAVE_TRANSLATE_X: u32 = 1 << 4;
const VC_HAVE_TRANSLATE_Y: u32 = 1 << 5;
const VC_HAVE_ROTATION: u32 = 1 << 6;
const VC_HAVE_CONDITION: u32 = 1 << 7;
const VC_HAVE_SCALE_X: u32 = 1 << 8;
const VC_HAVE_SCALE_Y: u32 = 1 << 9;
const VC_HAVE_TCENTER_X: u32 = 1 << 10;
const VC_HAVE_TCENTER_Y: u32 = 1 << 11;
const VC_GID_IS_24BIT: u32 = 1 << 12;
const VC_HAVE_SKEW_X: u32 = 1 << 13;
const VC_HAVE_SKEW_Y: u32 = 1 << 14;

/// A valid Coverage lists each glyph at most once, so it never has more
/// than this many entries. [`CoverageIter`] stops there.
const MAX_COVERAGE_ENTRIES: usize = 1 << 16;

/// Expand `keep` to include every gid referenced (transitively) by a
/// VARC-covered gid already in the set. References to other
/// VARC-covered gids cascade.
///
/// Tolerates malformed records silently. A single bad component record
/// should not stop the closure walk. Stops early once `budget` is
/// spent.
pub(crate) fn varc_closure_bitset(face: &Face<'_>, keep: &mut [bool], budget: &WorkBudget) {
    let Ok(Some(varc)) = face.varc() else {
        return;
    };
    let Ok(varc_bytes) = face.table_bytes(tag::VARC) else {
        return;
    };
    let Ok(parsed) = ParsedVarc::parse(varc_bytes) else {
        return;
    };

    // Coverage index for every gid in the font, first match wins like a
    // linear scan of the table. Built once so each lookup is O(1).
    let mut index_of: Vec<Option<usize>> = alloc::vec![None; keep.len()];
    let mut entries = 0usize;
    for (gid, idx) in parsed.coverage_iter() {
        entries += 1;
        if let Some(slot @ None) = index_of.get_mut(gid as usize) {
            *slot = Some(idx);
        }
    }
    if !budget.spend(entries + keep.len()) {
        return;
    }

    let mut stack: Vec<GlyphId> = keep
        .iter()
        .enumerate()
        .filter_map(|(i, &k)| if k { Some(i as GlyphId) } else { None })
        .collect();
    while let Some(g) = stack.pop() {
        if !varc.covers(g) {
            continue;
        }
        let Some(idx) = index_of.get(g as usize).copied().flatten() else {
            continue;
        };
        let Some(record) = parsed.glyph_record(idx) else {
            continue;
        };
        let children = walk_component_gids(record);
        if !budget.spend(1 + children.len()) {
            return;
        }
        for child in children {
            if let Some(slot @ false) = keep.get_mut(child as usize) {
                *slot = true;
                stack.push(child);
            }
        }
    }
}

/// Subsets a VARC table.
///
/// Drops coverage entries whose gid is not in `kept_gids`, renumbers
/// surviving entries per `new_gid_for`, and rewrites every component
/// record so its referenced gid maps to the new namespace. Returns the
/// new VARC bytes, or `Ok(None)` when no covered gid survives (in which
/// case the caller should omit the table from the output entirely).
///
/// MultiVarStore pruning: walks the surviving glyph records to collect
/// every `MultiVarIdx` they reference, builds a remap that drops every
/// unreferenced delta-set entry (and collapses subtables that become
/// empty), re-emits the MVS, and rewrites the surviving records'
/// `MultiVarIdx` slots through the remap. ConditionList and
/// AxisIndicesList are preserved verbatim.
///
/// A malformed coverage can list a glyph twice or send two glyphs to
/// the same record. Only the first such entry survives, so the output
/// never holds more records than the source.
pub(crate) fn subset_varc(
    src_varc: &Varc<'_>,
    src_bytes: &[u8],
    kept_gids: &[GlyphId],
    new_gid_for: &dyn Fn(GlyphId) -> Option<GlyphId>,
) -> Result<Option<Vec<u8>>, SubsetError> {
    let _ = src_varc; // signature compatibility: we re-parse the raw bytes
    let parsed = ParsedVarc::parse(src_bytes)
        .map_err(|_| SubsetError::Unsupported("VARC malformed during subset"))?;

    // Determine which coverage entries survive. Walk in source coverage
    // order so we can pull the right glyph record per entry.
    let kept_set: BTreeSet<GlyphId> = kept_gids.iter().copied().collect();
    let mut seen_gids: BTreeSet<GlyphId> = BTreeSet::new();
    let mut seen_records: BTreeSet<usize> = BTreeSet::new();
    let mut surviving: Vec<(GlyphId, usize)> = Vec::new();
    for (gid, idx) in parsed.coverage_iter() {
        if kept_set.contains(&gid) && seen_gids.insert(gid) && seen_records.insert(idx) {
            surviving.push((gid, idx));
        }
    }

    if surviving.is_empty() {
        // No kept gid is VARC-covered: drop the whole table.
        return Ok(None);
    }

    // Renumber and sort by new gid. Coverage format 1 requires sorted
    // glyphArray; the corresponding glyphRecords INDEX walks in the
    // same order.
    let mut renumbered: Vec<(GlyphId, usize)> = Vec::with_capacity(surviving.len());
    for (old_gid, src_idx) in &surviving {
        let new_gid = new_gid_for(*old_gid).ok_or(SubsetError::Unsupported(
            "VARC kept gid lacks a new-gid mapping",
        ))?;
        renumbered.push((new_gid, *src_idx));
    }
    renumbered.sort_by_key(|(new_gid, _)| *new_gid);

    // Closure-collect every MultiVarIdx the surviving records reference.
    // This drives the MVS pruning remap.
    let mut referenced: BTreeSet<(u16, u16)> = BTreeSet::new();
    for (_, src_idx) in &renumbered {
        let raw = parsed
            .glyph_record(*src_idx)
            .ok_or(SubsetError::Unsupported("VARC glyph record index OOB"))?;
        for pair in walk_component_var_idxs(raw) {
            referenced.insert(pair);
        }
    }

    // Build the MVS remap + the new MVS bytes. When the source has no
    // MVS the remap is a no-op (no var-idx references should exist
    // either; if they do, `rewrite_component_record` will surface the
    // mismatch).
    let (mvs_remap, new_var_store) = if let Some(mvs_bytes) = parsed.var_store_bytes() {
        prune_multi_var_store(mvs_bytes, &referenced)?
    } else {
        (MvsRemap::new(), None)
    };

    // Rewrite every surviving glyph record with both gid renumbering
    // and MVS index remapping in one pass.
    let mut new_records: Vec<Vec<u8>> = Vec::with_capacity(renumbered.len());
    for (_, src_idx) in &renumbered {
        let raw = parsed
            .glyph_record(*src_idx)
            .ok_or(SubsetError::Unsupported("VARC glyph record index OOB"))?;
        let remap_lookup = |outer: u16, inner: u16| -> Option<(u16, u16)> {
            mvs_remap.get(&(outer, inner)).copied()
        };
        let new_record = rewrite_component_record(raw, &new_gid_for, &remap_lookup)?;
        new_records.push(new_record);
    }

    // Coverage format 1 with the new sorted gid list.
    let new_coverage = build_coverage_format1(renumbered.iter().map(|(g, _)| *g));

    // glyphRecords CFF2 INDEX over the rewritten records.
    let new_glyph_records = try_build_cff2_index(&new_records)
        .ok_or(SubsetError::Unsupported("VARC glyph records exceed 4 GiB"))?;

    // Pass-throughs (MVS now handled separately by `new_var_store`).
    let condition_list_bytes = parsed.condition_list_bytes();
    let axis_indices_bytes = parsed.axis_indices_bytes();

    // Reassemble. Header is 24 bytes (u16 major, u16 minor, then five
    // Offset32 slots). Subsequent blocks are placed in source order with
    // 4-byte alignment between blocks.
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor

    let cov_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    let vs_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    let cl_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    let ail_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    let gr_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());

    // Blocks in source order: coverage, varStore (rewritten with only
    // the kept entries), conditionList, axisIndicesList. Each one is
    // followed by 4-byte alignment padding.
    let blocks = [
        (cov_off_slot, Some(new_coverage.as_slice())),
        (vs_off_slot, new_var_store.as_deref()),
        (cl_off_slot, condition_list_bytes),
        (ail_off_slot, axis_indices_bytes),
    ];
    for (slot, block) in blocks {
        let Some(block) = block else {
            continue;
        };
        patch_offset32(&mut out, slot)?;
        out.extend_from_slice(block);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    // glyphRecords
    patch_offset32(&mut out, gr_off_slot)?;
    out.extend_from_slice(&new_glyph_records);

    Ok(Some(out))
}

/// Points the Offset32 at `slot` to the current end of `out`.
fn patch_offset32(out: &mut [u8], slot: usize) -> Result<(), SubsetError> {
    let pos = u32::try_from(out.len())
        .map_err(|_| SubsetError::Unsupported("VARC output exceeds 4 GiB"))?;
    out.get_mut(slot..slot + 4)
        .ok_or(SubsetError::Unsupported("VARC offset slot out of range"))?
        .copy_from_slice(&pos.to_be_bytes());
    Ok(())
}

/// Builds a CFF2 INDEX over the given entries, picking the smallest
/// off_size that fits. Determinism: identical inputs always produce
/// identical output bytes.
#[cfg(test)]
fn build_cff2_index(entries: &[Vec<u8>]) -> Vec<u8> {
    try_build_cff2_index(entries).unwrap_or_default()
}

/// Builds a CFF2 INDEX over the given entries, picking the smallest
/// off_size that fits. Returns `None` when the entries do not fit the
/// INDEX's 32-bit counts and offsets.
fn try_build_cff2_index(entries: &[Vec<u8>]) -> Option<Vec<u8>> {
    let count = u32::try_from(entries.len()).ok()?;
    let mut out = Vec::new();
    out.extend_from_slice(&count.to_be_bytes());
    if entries.is_empty() {
        return Some(out);
    }
    let total: usize = entries.iter().map(Vec::len).sum();
    let max_off = u32::try_from(total).ok()?.checked_add(1)?;
    let off_size: u8 = if max_off <= 0xFF {
        1
    } else if max_off <= 0xFFFF {
        2
    } else if max_off <= 0x00FF_FFFF {
        3
    } else {
        4
    };
    out.push(off_size);
    // Offsets are at most `max_off`, which fits `off_size` bytes.
    let write_off = |out: &mut Vec<u8>, v: u32| {
        let bytes = v.to_be_bytes();
        out.extend_from_slice(&bytes[4 - usize::from(off_size)..]);
    };
    let mut cursor: u32 = 1;
    write_off(&mut out, cursor);
    for e in entries {
        // Each step stays at or below `max_off`.
        cursor += e.len() as u32;
        write_off(&mut out, cursor);
    }
    for e in entries {
        out.extend_from_slice(e);
    }
    Some(out)
}

/// Internal lightweight parse of a VARC table: enumerates coverage
/// entries, glyph records, and exposes the byte ranges of the
/// pass-through blocks (MultiVarStore / ConditionList / AxisIndicesList).
struct ParsedVarc<'a> {
    coverage_bytes: &'a [u8],
    var_store: Option<&'a [u8]>,
    condition_list: Option<&'a [u8]>,
    axis_indices_list: Option<&'a [u8]>,
    glyph_records: Vec<&'a [u8]>,
}

impl<'a> ParsedVarc<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, &'static str> {
        if data.len() < 24 {
            return Err("VARC header truncated");
        }
        let major = u16::from_be_bytes([data[0], data[1]]);
        if major != 1 {
            return Err("VARC unsupported major version");
        }
        let coverage_off = read_u32(data, 4)? as usize;
        let var_store_off = read_u32(data, 8)? as usize;
        let condition_list_off = read_u32(data, 12)? as usize;
        let axis_indices_off = read_u32(data, 16)? as usize;
        let glyph_records_off = read_u32(data, 20)? as usize;

        let coverage_bytes = data.get(coverage_off..).ok_or("VARC coverage off OOB")?;

        // Each block extends to the start of the next block, in source
        // file order. Build a sorted list of non-zero offsets and map
        // each block to (start, end) using its successor.
        let mut markers: Vec<usize> = [
            coverage_off,
            var_store_off,
            condition_list_off,
            axis_indices_off,
            glyph_records_off,
        ]
        .iter()
        .copied()
        .filter(|o| *o != 0)
        .collect();
        markers.push(data.len());
        markers.sort_unstable();
        markers.dedup();

        let block_end = |start: usize| -> usize { next_marker(&markers, start, data.len()) };

        let var_store = if var_store_off == 0 {
            None
        } else {
            Some(
                data.get(var_store_off..block_end(var_store_off))
                    .ok_or("VARC varStore OOB")?,
            )
        };
        let condition_list = if condition_list_off == 0 {
            None
        } else {
            Some(
                data.get(condition_list_off..block_end(condition_list_off))
                    .ok_or("VARC conditionList OOB")?,
            )
        };
        let axis_indices_list = if axis_indices_off == 0 {
            None
        } else {
            Some(
                data.get(axis_indices_off..block_end(axis_indices_off))
                    .ok_or("VARC axisIndices OOB")?,
            )
        };

        let glyph_records = if glyph_records_off == 0 {
            Vec::new()
        } else {
            let block = data
                .get(glyph_records_off..)
                .ok_or("VARC glyphRecords OOB")?;
            parse_cff2_index(block)?
        };

        Ok(Self {
            coverage_bytes,
            var_store,
            condition_list,
            axis_indices_list,
            glyph_records,
        })
    }

    fn var_store_bytes(&self) -> Option<&'a [u8]> {
        self.var_store
    }
    fn condition_list_bytes(&self) -> Option<&'a [u8]> {
        self.condition_list
    }
    fn axis_indices_bytes(&self) -> Option<&'a [u8]> {
        self.axis_indices_list
    }

    fn glyph_record(&self, idx: usize) -> Option<&'a [u8]> {
        self.glyph_records.get(idx).copied()
    }

    /// Parses the coverage table to find the index of `gid`.
    #[cfg(test)]
    fn coverage_index_of(&self, gid: GlyphId) -> Option<usize> {
        coverage_index_of(self.coverage_bytes, gid)
    }

    /// Iterator over `(gid, record_index)` pairs in coverage order.
    /// The record index is the position used to look up the glyph
    /// record inside `glyph_records`.
    fn coverage_iter(&self) -> impl Iterator<Item = (GlyphId, usize)> + '_ {
        CoverageIter::new(self.coverage_bytes)
    }
}

/// Returns the smallest entry of the sorted `markers` that is greater
/// than `start`, or `fallback` when there is none.
fn next_marker(markers: &[usize], start: usize, fallback: usize) -> usize {
    let i = markers.partition_point(|&m| m <= start);
    markers.get(i).copied().unwrap_or(fallback)
}

/// Iterator over coverage entries yielding `(gid, record_index)` pairs.
/// Used by [`subset_varc`] to walk the source coverage in order while
/// filtering on the kept-gid set.
///
/// Stops after [`MAX_COVERAGE_ENTRIES`] entries: a valid coverage never
/// has more, and overlapping ranges in a malformed one could otherwise
/// yield billions of entries.
struct CoverageIter<'a> {
    bytes: &'a [u8],
    format: u16,
    count: usize,
    cursor: usize,
    /// For format 2: which range we're inside.
    range_idx: usize,
    /// For format 2: current glyph inside the range (offset from start).
    /// Wider than a gid so a full `0..=0xFFFF` range can step past its
    /// end.
    range_offset: u32,
    /// Entries yielded so far.
    yielded: usize,
}

impl<'a> CoverageIter<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        let (format, count) = match (
            crate::layout::read_u16(bytes, 0),
            crate::layout::read_u16(bytes, 2),
        ) {
            (Some(format), Some(count)) => (format, usize::from(count)),
            _ => (0, 0),
        };
        Self {
            bytes,
            format,
            count,
            cursor: 0,
            range_idx: 0,
            range_offset: 0,
            yielded: 0,
        }
    }

    fn next_entry(&mut self) -> Option<(GlyphId, usize)> {
        match self.format {
            1 => {
                if self.cursor >= self.count {
                    return None;
                }
                let g = crate::layout::read_u16(self.bytes, 4 + self.cursor * 2)?;
                let idx = self.cursor;
                self.cursor += 1;
                Some((g, idx))
            }
            2 => {
                while self.range_idx < self.count {
                    let rec = self
                        .bytes
                        .get(4 + self.range_idx * 6..)?
                        .first_chunk::<6>()?;
                    let start = u16::from_be_bytes([rec[0], rec[1]]);
                    let end = u16::from_be_bytes([rec[2], rec[3]]);
                    let start_cov = u16::from_be_bytes([rec[4], rec[5]]);
                    let span = u32::from(end.saturating_sub(start));
                    if self.range_offset > span {
                        self.range_idx += 1;
                        self.range_offset = 0;
                        continue;
                    }
                    // start + range_offset <= end, so this fits a gid.
                    let g = u16::try_from(u32::from(start) + self.range_offset).ok()?;
                    let idx = usize::from(start_cov) + self.range_offset as usize;
                    self.range_offset += 1;
                    return Some((g, idx));
                }
                None
            }
            _ => None,
        }
    }
}

impl Iterator for CoverageIter<'_> {
    type Item = (GlyphId, usize);

    fn next(&mut self) -> Option<Self::Item> {
        if self.yielded >= MAX_COVERAGE_ENTRIES {
            return None;
        }
        let entry = self.next_entry()?;
        self.yielded += 1;
        Some(entry)
    }
}

/// Builds a Coverage format-1 table from a sorted ascending iterator of
/// gids. Used by [`subset_varc`] to emit the rewritten coverage. The
/// caller passes at most one entry per gid, so the count fits in 16
/// bits.
fn build_coverage_format1(gids: impl IntoIterator<Item = GlyphId>) -> Vec<u8> {
    let gids: Vec<GlyphId> = gids.into_iter().collect();
    let mut out = Vec::with_capacity(4 + gids.len() * 2);
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    let count = gids.len() as u16;
    out.extend_from_slice(&count.to_be_bytes());
    for g in gids {
        out.extend_from_slice(&g.to_be_bytes());
    }
    out
}

/// Reads u32 BE at `off`, bounds-checked.
fn read_u32(data: &[u8], off: usize) -> Result<u32, &'static str> {
    let bytes = data
        .get(off..)
        .and_then(<[u8]>::first_chunk::<4>)
        .ok_or("u32 OOB")?;
    Ok(u32::from_be_bytes(*bytes))
}

/// Walks a coverage table for the index of `gid`. Mirrors the parser's
/// search; returns `usize` so callers can index into the glyph-records
/// vec directly.
#[cfg(test)]
fn coverage_index_of(bytes: &[u8], gid: GlyphId) -> Option<usize> {
    CoverageIter::new(bytes).find_map(|(g, idx)| (g == gid).then_some(idx))
}

/// Parses a CFF2 INDEX (u32 count + u8 offSize + offsets + data),
/// returning one byte slice per entry. Mirrors the layout the parser
/// uses for `glyphRecords`.
fn parse_cff2_index(block: &[u8]) -> Result<Vec<&[u8]>, &'static str> {
    let count = read_u32(block, 0).map_err(|_| "CFF2 INDEX truncated header")? as usize;
    if count == 0 {
        return Ok(Vec::new());
    }
    let off_size = usize::from(*block.get(4).ok_or("CFF2 INDEX truncated offSize")?);
    if !(1..=4).contains(&off_size) {
        return Err("CFF2 INDEX offSize out of range");
    }
    let offsets_start = 5;
    let offsets_bytes = count
        .checked_add(1)
        .and_then(|n| n.checked_mul(off_size))
        .ok_or("CFF2 INDEX offsets truncated")?;
    let data_start = offsets_start + offsets_bytes;
    let offset_table = block
        .get(offsets_start..data_start)
        .ok_or("CFF2 INDEX offsets truncated")?;
    // The offset table fits in `block`, which bounds this allocation.
    let offsets: Vec<usize> = offset_table
        .chunks_exact(off_size)
        .map(|c| c.iter().fold(0usize, |v, &b| (v << 8) | usize::from(b)))
        .collect();
    let Some(&total) = offsets.last() else {
        return Err("CFF2 INDEX offsets truncated");
    };
    if total == 0 {
        return Err("CFF2 INDEX total length zero");
    }
    let data_len = total - 1;
    if block.len() < data_start + data_len {
        return Err("CFF2 INDEX data truncated");
    }
    let mut out: Vec<&[u8]> = Vec::with_capacity(count);
    for w in offsets.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a == 0 || b < a {
            return Err("CFF2 INDEX offsets non-monotone");
        }
        let entry = block
            .get(data_start + a - 1..data_start + b - 1)
            .ok_or("CFF2 INDEX entry past end")?;
        out.push(entry);
    }
    Ok(out)
}

/// Prunes a `MultiItemVariationStore` to keep only the delta-set
/// entries listed in `referenced` (a set of source `(outer, inner)`
/// pairs). Returns:
///
/// - `BTreeMap<(old_outer, old_inner), (new_outer, new_inner)>`: the
///   index remap callers apply to surviving component records'
///   `MultiVarIdx` slots. Subtables that lose every entry are dropped
///   and the outer-index space collapses; `inner` indices are dense
///   per kept subtable.
/// - `Option<Vec<u8>>`: the rewritten MVS bytes, or `None` when the
///   pruned store has no surviving subtables (caller emits no
///   varStore offset).
///
/// The region list is also pruned: after subtable pruning, every
/// surviving subtable's `region_indexes` is walked to collect the set
/// of regions any kept tuple still references; unreferenced regions
/// are dropped from the region list and surviving subtables' region
/// indexes are renumbered through the remap.
///
/// Tolerates a source `referenced` set that names entries the source
/// MVS doesn't actually have. Those are silently skipped, but they
/// won't appear in the remap either, so the caller's
/// [`rewrite_component_record`] will surface the orphan via the
/// `Unsupported` path.
///
/// Subtables and regions may share bytes in the source, and each kept
/// copy is written out separately. The copies are charged to a work
/// budget, and a store that would grow past it is rejected.
type MvsRemap = BTreeMap<(u16, u16), (u16, u16)>;

fn prune_multi_var_store(
    src: &[u8],
    referenced: &BTreeSet<(u16, u16)>,
) -> Result<(MvsRemap, Option<Vec<u8>>), SubsetError> {
    const TOO_LARGE: SubsetError =
        SubsetError::Unsupported("VARC MVS too large to prune; subtables share data");
    let budget = WorkBudget::new(WORK_LIMIT);
    let parsed = ParsedMvs::parse(src)
        .map_err(|_| SubsetError::Unsupported("VARC MVS malformed during prune"))?;

    // Group `referenced` by old outer index so we can decide which
    // subtables survive (those with at least one referenced inner).
    let mut by_outer: BTreeMap<u16, BTreeSet<u16>> = BTreeMap::new();
    for &(outer, inner) in referenced {
        by_outer.entry(outer).or_default().insert(inner);
    }

    // Walk source subtables in order; for each, build a list of kept
    // inner indices and assign a new outer index iff the kept list is
    // non-empty.
    let mut new_subtables: Vec<RewrittenMvsSubtable> = Vec::new();
    let mut remap: BTreeMap<(u16, u16), (u16, u16)> = BTreeMap::new();
    for (old_outer, sub) in parsed.subtables() {
        let Some(kept_inners) = by_outer.get(&old_outer) else {
            continue;
        };
        // Keep only inners that exist in the source (defensive: an
        // out-of-range source ref means the source font is malformed).
        // `kept_inners` is a BTreeSet, so the pairs come out sorted by
        // inner index.
        let kept_pairs: Vec<(u16, &[u8])> = kept_inners
            .iter()
            .filter_map(|&inner| Some((inner, *sub.delta_sets.get(inner as usize)?)))
            .collect();
        if kept_pairs.is_empty() {
            continue;
        }
        let copied: usize = kept_pairs.iter().map(|(_, b)| b.len()).sum();
        if !budget.spend(sub.region_indexes.len() + copied) {
            return Err(TOO_LARGE);
        }

        // At most one subtable per source outer index, so this fits.
        let new_outer = new_subtables.len() as u16;
        let mut new_delta_sets: Vec<Vec<u8>> = Vec::with_capacity(kept_pairs.len());
        for (new_inner_idx, (old_inner, bytes)) in kept_pairs.iter().enumerate() {
            // At most one entry per source inner index, so this fits.
            let new_inner = new_inner_idx as u16;
            remap.insert((old_outer, *old_inner), (new_outer, new_inner));
            new_delta_sets.push((*bytes).to_vec());
        }
        new_subtables.push(RewrittenMvsSubtable {
            region_indexes: sub.region_indexes.clone(),
            delta_sets: new_delta_sets,
        });
    }

    if new_subtables.is_empty() {
        return Ok((remap, None));
    }

    // ---- Region-list prune ------------------------------------------
    //
    // After subtable pruning, walk every surviving subtable's
    // `region_indexes` to collect the set of regions any tuple still
    // references. Regions outside this set are unreachable and dropped
    // from the region list; surviving subtables' region indexes are
    // renumbered through the remap.
    //
    // Defensive: a malformed source MVS where a subtable's region
    // index points past the source region list would normally be
    // surfaced by the parser, but we re-check here and skip such
    // entries during region collection. Skipping is safer than failing
    // because the resulting subtable simply carries no contribution from
    // that region, mirroring the parser's tolerant behavior.
    let referenced_regions = collect_referenced_regions(&new_subtables);
    let src_regions = parse_region_list(&parsed.region_list_bytes)
        .map_err(|_| SubsetError::Unsupported("VARC MVS region list malformed during prune"))?;

    // Region remap: old region index -> new region index. Built by
    // walking referenced regions in ascending order so the new region
    // list preserves source order. That keeps output bytes stable for
    // round-trip determinism.
    let mut region_remap: BTreeMap<u16, u16> = BTreeMap::new();
    let mut kept_region_payloads: Vec<&[u8]> = Vec::new();
    for &old_ri in &referenced_regions {
        let Some(payload) = src_regions.get(old_ri as usize).copied() else {
            // Source's tuple referenced a region that doesn't exist:
            // skip it. The subtable's region_indexes will be filtered
            // below and the region effectively contributes zero, which
            // matches the parser's behavior for an OOB region.
            continue;
        };
        if !budget.spend(payload.len()) {
            return Err(TOO_LARGE);
        }
        // At most one entry per source region index, so this fits.
        let new_ri = kept_region_payloads.len() as u16;
        region_remap.insert(old_ri, new_ri);
        kept_region_payloads.push(payload);
    }

    // Renumber each surviving subtable's region_indexes through the
    // remap. Drop indexes that lacked a kept region. A subtable left
    // with zero region indexes has only orphaned regions, so it is
    // dropped to keep the output structurally valid.
    let mut pruned_subtables: Vec<RewrittenMvsSubtable> = Vec::with_capacity(new_subtables.len());
    let mut outer_remap_collapse: BTreeMap<u16, u16> = BTreeMap::new();
    for (old_outer, sub) in new_subtables.into_iter().enumerate() {
        let new_region_indexes: Vec<u16> = sub
            .region_indexes
            .iter()
            .filter_map(|ri| region_remap.get(ri).copied())
            .collect();
        if new_region_indexes.is_empty() {
            continue;
        }
        let new_outer = pruned_subtables.len() as u16;
        if (old_outer as u16) != new_outer {
            outer_remap_collapse.insert(old_outer as u16, new_outer);
        }
        pruned_subtables.push(RewrittenMvsSubtable {
            region_indexes: new_region_indexes,
            delta_sets: sub.delta_sets,
        });
    }

    // If a subtable was dropped during the region collapse, fold the
    // outer-index shift into the existing `(outer, inner)` remap so the
    // record-rewrite path sees the final outer indices.
    if !outer_remap_collapse.is_empty() {
        for (_, (no, _)) in remap.iter_mut() {
            if let Some(&final_no) = outer_remap_collapse.get(no) {
                *no = final_no;
            }
        }
    }

    if pruned_subtables.is_empty() {
        // Every subtable's regions were orphaned. The MVS becomes
        // effectively region-less and contributes nothing. Drop it
        // entirely so the caller emits no varStore offset.
        return Ok((BTreeMap::new(), None));
    }

    let new_region_list_bytes = build_region_list_bytes(&kept_region_payloads);
    let new_bytes = emit_multi_var_store(&new_region_list_bytes, &pruned_subtables)
        .ok_or(SubsetError::Unsupported("VARC MVS exceeds 4 GiB"))?;
    Ok((remap, Some(new_bytes)))
}

/// Builds an MVS region-list block from kept region payload slices.
/// Each `payload` is the raw bytes of one region (`u16 axisCount` +
/// axis triples), as returned by [`parse_region_list`]. Output layout:
///
/// ```text
///   u16       regionCount
///   Offset32  variationRegionOffsets[regionCount] (relative to block start)
///   <region payloads, concatenated in input order>
/// ```
///
/// Callers pass at most one payload per source region index, so the
/// count fits in 16 bits, and the prune budget keeps the block far
/// below 4 GiB.
fn build_region_list_bytes(payloads: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    let count = payloads.len() as u16;
    out.extend_from_slice(&count.to_be_bytes());
    let off_table_start = out.len();
    out.resize(off_table_start + payloads.len() * 4, 0);
    for (i, p) in payloads.iter().enumerate() {
        let start = out.len() as u32;
        let slot = off_table_start + i * 4;
        if let Some(dst) = out.get_mut(slot..slot + 4) {
            dst.copy_from_slice(&start.to_be_bytes());
        }
        out.extend_from_slice(p);
    }
    out
}

/// One subtable in the rewritten MVS. After the region-list pruning
/// pass, `region_indexes` are renumbered into the new region-index
/// space; the verbatim copy left here by the subtable prune is
/// rewritten before [`emit_multi_var_store`] is called.
struct RewrittenMvsSubtable {
    region_indexes: Vec<u16>,
    delta_sets: Vec<Vec<u8>>,
}

/// Walks every surviving subtable's `region_indexes` and returns the
/// set of regions any tuple still references. Drives the region list
/// prune: anything not in this set is unreachable after the MVS
/// subtable prune and can be dropped.
fn collect_referenced_regions(subtables: &[RewrittenMvsSubtable]) -> BTreeSet<u16> {
    let mut out: BTreeSet<u16> = BTreeSet::new();
    for sub in subtables {
        for &ri in &sub.region_indexes {
            out.insert(ri);
        }
    }
    out
}

/// Parses an MVS region-list block into one byte slice per region. The
/// returned slices cover each region's body (`u16 axisCount` + axis
/// triples). They're spliced verbatim into the new region list, so the
/// pruner doesn't need to decode F2DOT14 coords.
///
/// The block layout (mirrors `src/tables/multi_var_store.rs`):
///
/// ```text
///   u16       regionCount
///   Offset32  variationRegionOffsets[regionCount] (relative to block start)
///   <region payloads>
/// ```
fn parse_region_list(bytes: &[u8]) -> Result<Vec<&[u8]>, &'static str> {
    let count =
        usize::from(crate::layout::read_u16(bytes, 0).ok_or("MVS region list header truncated")?);
    let off_table_end = 2 + count * 4;
    let offset_table = bytes
        .get(2..off_table_end)
        .ok_or("MVS region list offsets truncated")?;
    // Read region offsets (relative to region-list start). Their
    // ascending order plus the block end give us each region's byte
    // span.
    let mut offsets: Vec<usize> = Vec::with_capacity(count);
    for c in offset_table.chunks_exact(4) {
        let v = u32::from_be_bytes([c[0], c[1], c[2], c[3]]) as usize;
        if v < off_table_end || v > bytes.len() {
            return Err("MVS region offset OOB");
        }
        offsets.push(v);
    }
    // Each region body extends to the next-greater offset, or to end.
    let mut sorted_bounds: Vec<usize> = offsets.clone();
    sorted_bounds.push(bytes.len());
    sorted_bounds.sort_unstable();
    sorted_bounds.dedup();
    let mut regions: Vec<&[u8]> = Vec::with_capacity(count);
    for &start in &offsets {
        let end = next_marker(&sorted_bounds, start, bytes.len());
        let region = bytes.get(start..end).ok_or("MVS region body OOB")?;
        // Sanity: at least the axisCount u16 must fit.
        let axis_count = usize::from(
            crate::layout::read_u16(region, 0).ok_or("MVS region axisCount truncated")?,
        );
        // Trim any trailing padding the source may have between
        // regions: emit only the region's structural bytes so the
        // rewriter produces a tightly-packed region list.
        let body = region
            .get(..2 + axis_count * 8)
            .ok_or("MVS region axes truncated")?;
        regions.push(body);
    }
    Ok(regions)
}

/// Lightweight parse of the MVS used by the pruner. Mirrors the layout
/// in `src/tables/multi_var_store.rs`. We keep references to the
/// region-list bytes so the rewriter can splice them back in verbatim.
struct ParsedMvs<'a> {
    /// Bytes of the region list block: header (regionCount + offset
    /// table) + every region payload, concatenated as in the source.
    /// The pruner re-emits these as-is.
    region_list_bytes: Vec<u8>,
    /// For each source outer index, the position of its parsed body in
    /// `bodies`. Subtables that share an offset share one body.
    subtable_body: Vec<usize>,
    bodies: Vec<ParsedMvsSubtable<'a>>,
}

struct ParsedMvsSubtable<'a> {
    region_indexes: Vec<u16>,
    delta_sets: Vec<&'a [u8]>,
}

impl<'a> ParsedMvs<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, &'static str> {
        let (Some(format), Ok(region_list_off), Some(subtable_count)) = (
            crate::layout::read_u16(data, 0),
            read_u32(data, 2),
            crate::layout::read_u16(data, 6),
        ) else {
            return Err("MVS header truncated");
        };
        if format != 1 {
            return Err("MVS unsupported format");
        }
        let region_list_off = region_list_off as usize;
        let subtable_count = usize::from(subtable_count);
        let offset_table = data
            .get(8..8 + subtable_count * 4)
            .ok_or("MVS subtable offset OOB")?;
        let subtable_offsets: Vec<usize> = offset_table
            .chunks_exact(4)
            .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]) as usize)
            .collect();

        // Every block ends at the next-greater block start, or at the
        // end of the data.
        let mut markers: Vec<usize> = subtable_offsets.clone();
        markers.push(region_list_off);
        markers.sort_unstable();
        markers.dedup();

        // Region list: bytes from `region_list_off` to the start of
        // the next block. The region list contains its own offset
        // array; for the pruner we don't need to decode regions, just
        // capture the byte range.
        let region_list_end = next_marker(&markers, region_list_off, data.len());
        let region_list_bytes = data
            .get(region_list_off..region_list_end)
            .ok_or("MVS region list OOB")?
            .to_vec();

        // Subtables. Blocks between distinct markers never overlap, so
        // parsing each distinct offset once keeps the work linear in
        // the data size.
        let mut body_at: BTreeMap<usize, usize> = BTreeMap::new();
        let mut bodies: Vec<ParsedMvsSubtable<'a>> = Vec::new();
        let mut subtable_body: Vec<usize> = Vec::with_capacity(subtable_count);
        for &off in &subtable_offsets {
            if let Some(&body) = body_at.get(&off) {
                subtable_body.push(body);
                continue;
            }
            let sub_end = next_marker(&markers, off, data.len());
            let parsed = Self::parse_subtable(data, off, sub_end)?;
            body_at.insert(off, bodies.len());
            subtable_body.push(bodies.len());
            bodies.push(parsed);
        }

        Ok(Self {
            region_list_bytes,
            subtable_body,
            bodies,
        })
    }

    /// Parses the subtable stored in `data[off..sub_end]`.
    fn parse_subtable(
        data: &'a [u8],
        off: usize,
        sub_end: usize,
    ) -> Result<ParsedMvsSubtable<'a>, &'static str> {
        let block = data.get(off..sub_end).ok_or("MVS subtable OOB")?;
        let (Some(&subtable_format), Some(region_index_count)) =
            (block.first(), crate::layout::read_u16(block, 1))
        else {
            return Err("MVS subtable header truncated");
        };
        if subtable_format != 1 {
            return Err("MVS unsupported subtable format");
        }
        let need = 3 + usize::from(region_index_count) * 2;
        let region_indexes: Vec<u16> = block
            .get(3..need)
            .ok_or("MVS subtable region indexes OOB")?
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        // Borrow the delta INDEX from `data` so the returned slices
        // carry lifetime `'a`.
        let idx_block: &'a [u8] = data.get(off + need..sub_end).ok_or("MVS delta index OOB")?;
        let delta_sets: Vec<&'a [u8]> = if idx_block.len() < 4 {
            Vec::new()
        } else {
            parse_cff2_index(idx_block).map_err(|_| "MVS delta CFF2 INDEX malformed")?
        };
        Ok(ParsedMvsSubtable {
            region_indexes,
            delta_sets,
        })
    }

    /// Iterates `(outer_index, subtable)` in source order.
    fn subtables(&self) -> impl Iterator<Item = (u16, &ParsedMvsSubtable<'a>)> + '_ {
        self.subtable_body
            .iter()
            .enumerate()
            .filter_map(|(outer, &body)| Some((u16::try_from(outer).ok()?, self.bodies.get(body)?)))
    }
}

/// Re-emits the MVS bytes from rewritten subtables. Region list is
/// spliced in verbatim from the source. Subtable offsets are computed
/// fresh; each subtable carries its CFF2 INDEX of delta-set bytes.
/// Returns `None` when the store does not fit its 32-bit offsets.
fn emit_multi_var_store(
    region_list_bytes: &[u8],
    subtables: &[RewrittenMvsSubtable],
) -> Option<Vec<u8>> {
    // Header layout:
    //   u16  format = 1
    //   u32  regionListOffset
    //   u16  subtableCount
    //   u32  subtableOffsets[subtableCount]
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    let region_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    let subtable_count = u16::try_from(subtables.len()).ok()?;
    out.extend_from_slice(&subtable_count.to_be_bytes());
    let sub_off_slots_start = out.len();
    out.resize(sub_off_slots_start + subtables.len() * 4, 0);

    // Region list directly follows the header, 4-byte aligned (the
    // header already ends on a 4-byte boundary because subtable
    // offsets are u32).
    let region_off = u32::try_from(out.len()).ok()?;
    out.get_mut(region_off_slot..region_off_slot + 4)?
        .copy_from_slice(&region_off.to_be_bytes());
    out.extend_from_slice(region_list_bytes);
    while out.len() % 4 != 0 {
        out.push(0);
    }

    // Subtables, each preceded by 4-byte alignment.
    for (i, sub) in subtables.iter().enumerate() {
        let sub_off = u32::try_from(out.len()).ok()?;
        let slot = sub_off_slots_start + i * 4;
        out.get_mut(slot..slot + 4)?
            .copy_from_slice(&sub_off.to_be_bytes());
        out.push(1); // format
        let ric = u16::try_from(sub.region_indexes.len()).ok()?;
        out.extend_from_slice(&ric.to_be_bytes());
        for ri in &sub.region_indexes {
            out.extend_from_slice(&ri.to_be_bytes());
        }
        out.extend_from_slice(&try_build_cff2_index(&sub.delta_sets)?);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    Some(out)
}

/// Walks a single VarComposite glyph record and yields the gid of every
/// component. Tolerates malformed records by stopping mid-walk. The
/// caller treats that as "no further components in this record."
fn walk_component_gids(record: &[u8]) -> Vec<GlyphId> {
    let mut out: Vec<GlyphId> = Vec::new();
    let mut cursor = 0usize;
    while cursor < record.len() {
        match parse_one_component(record, cursor) {
            Some((info, next)) => {
                out.push(info.gid);
                if next <= cursor {
                    // Defensive: a buggy walk that doesn't advance would
                    // loop forever on malformed input.
                    break;
                }
                cursor = next;
            }
            None => break,
        }
    }
    out
}

/// Walks a single VarComposite glyph record and yields every
/// MultiVarIdx referenced by its component records: both transform
/// deltas (`VC_TRANSFORM_HAS_VARIATION`) and axis-coord deltas
/// (`VC_AXIS_VALUES_HAVE_VARIATION`). Each value is split into its
/// outer/inner halves (high 16 bits -> outer, low 16 -> inner).
///
/// Tolerates malformed records by stopping mid-walk, mirroring
/// [`walk_component_gids`].
fn walk_component_var_idxs(record: &[u8]) -> Vec<(u16, u16)> {
    let mut out: Vec<(u16, u16)> = Vec::new();
    let mut cursor = 0usize;
    while cursor < record.len() {
        match parse_one_component(record, cursor) {
            Some((info, next)) => {
                if let Some((_, _, v)) = info.axis_var_idx {
                    out.push(split_var_idx(v));
                }
                if let Some((_, _, v)) = info.transform_var_idx {
                    out.push(split_var_idx(v));
                }
                if next <= cursor {
                    break;
                }
                cursor = next;
            }
            None => break,
        }
    }
    out
}

/// Splits a MultiVarIdx into its `(outer, inner)` halves.
fn split_var_idx(v: u32) -> (u16, u16) {
    ((v >> 16) as u16, (v & 0xFFFF) as u16)
}

/// Per-component metadata extracted by `parse_one_component`.
struct ComponentInfo {
    /// Source-file gid this component points at.
    gid: GlyphId,
    /// Byte range inside the record where the gid lives. The rewrite
    /// path uses these bounds to splice in the new gid.
    gid_range: (usize, usize),
    /// True when the gid was encoded as 24 bits (VC_GID_IS_24BIT). The
    /// rewrite path keeps this width even if the new gid would fit in
    /// 16 bits, so the gid field never changes size.
    gid_is_24bit: bool,
    /// Byte range + old value of the axis-values MultiVarIdx, when
    /// `VC_AXIS_VALUES_HAVE_VARIATION` is set. The MVS pruning path
    /// rewrites the bytes in this range with the remapped index.
    axis_var_idx: Option<(usize, usize, u32)>,
    /// Byte range + old value of the transform MultiVarIdx, when
    /// `VC_TRANSFORM_HAS_VARIATION` is set.
    transform_var_idx: Option<(usize, usize, u32)>,
}

/// Parses one component record at `start`, returning the gid info and
/// the cursor position immediately past the record.
fn parse_one_component(record: &[u8], start: usize) -> Option<(ComponentInfo, usize)> {
    let mut cur = start;
    let (flags, after_flags) = read_uint32var(record, cur)?;
    cur = after_flags;

    let gid_start = cur;
    let (gid, gid_is_24bit, after_gid) = if flags & VC_GID_IS_24BIT != 0 {
        let b = record.get(cur..)?.first_chunk::<3>()?;
        let g = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        // sigilbuzz uses u16 gids throughout; a u24 source gid > 0xFFFF
        // would silently truncate to its low 16 bits and lie about the
        // reference graph (#196). Treat it as a malformed component and
        // bail. The walker's caller treats `None` as "no further
        // components in this record" and skips it tolerantly.
        let gid = u16::try_from(g).ok()?;
        (gid, true, cur + 3)
    } else {
        let g = crate::layout::read_u16(record, cur)?;
        (g, false, cur + 2)
    };
    let gid_end = after_gid;
    cur = after_gid;

    if flags & VC_HAVE_CONDITION != 0 {
        let (_cond, n) = read_uint32var(record, cur)?;
        cur = n;
    }

    if flags & VC_HAVE_AXES != 0 {
        let (_axis_indices_index, n) = read_uint32var(record, cur)?;
        cur = n;
        // axisValues is a TupleValues stream. We consume one run-control
        // + payload. VARC's writers emit a single run per axisValues
        // covering every axis the component touches.
        let n = consume_tuple_values_one_run(record, cur)?;
        cur = n;
    }

    let mut axis_var_idx: Option<(usize, usize, u32)> = None;
    if flags & VC_AXIS_VALUES_HAVE_VARIATION != 0 {
        let var_idx_start = cur;
        let (var_idx, n) = read_uint32var(record, cur)?;
        axis_var_idx = Some((var_idx_start, n, var_idx));
        cur = n;
    }

    let mut transform_var_idx: Option<(usize, usize, u32)> = None;
    if flags & VC_TRANSFORM_HAS_VARIATION != 0 {
        let var_idx_start = cur;
        let (var_idx, n) = read_uint32var(record, cur)?;
        transform_var_idx = Some((var_idx_start, n, var_idx));
        cur = n;
    }

    // i16 transform fields, in spec order. Each present flag adds 2 bytes.
    let field_count = [
        VC_HAVE_TRANSLATE_X,
        VC_HAVE_TRANSLATE_Y,
        VC_HAVE_ROTATION,
        VC_HAVE_SCALE_X,
        VC_HAVE_SCALE_Y,
        VC_HAVE_SKEW_X,
        VC_HAVE_SKEW_Y,
        VC_HAVE_TCENTER_X,
        VC_HAVE_TCENTER_Y,
    ]
    .iter()
    .filter(|&&bit| flags & bit != 0)
    .count();
    let bytes_needed = field_count * 2;
    if cur + bytes_needed > record.len() {
        return None;
    }
    cur += bytes_needed;

    Some((
        ComponentInfo {
            gid,
            gid_range: (gid_start, gid_end),
            gid_is_24bit,
            axis_var_idx,
            transform_var_idx,
        },
        cur,
    ))
}

/// Reads exactly one TupleValues run-control plus its payload, returning
/// the byte position immediately after.
fn consume_tuple_values_one_run(data: &[u8], start: usize) -> Option<usize> {
    let Some(&ctrl) = data.get(start) else {
        return Some(start);
    };
    let cur = start + 1;
    let run_len = (ctrl & 0x3F) as usize + 1;
    let zeros = ctrl & 0x80 != 0;
    let words = ctrl & 0x40 != 0;
    let slot_size = match (zeros, words) {
        (true, false) => 0,
        (true, true) => 4,
        (false, true) => 2,
        (false, false) => 1,
    };
    let need = run_len * slot_size;
    if cur + need > data.len() {
        return None;
    }
    Some(cur + need)
}

/// Reads a uint32var starting at `off` in `data`. Returns the value plus
/// the byte position immediately after.
fn read_uint32var(data: &[u8], off: usize) -> Option<(u32, usize)> {
    let rest = data.get(off..)?;
    let b0 = *rest.first()?;
    match b0 {
        0x00..=0x7F => Some((u32::from(b0), off + 1)),
        0x80..=0xBF => {
            let b = rest.first_chunk::<2>()?;
            Some((((u32::from(b0) - 0x80) << 8) | u32::from(b[1]), off + 2))
        }
        0xC0..=0xDF => {
            let b = rest.first_chunk::<3>()?;
            Some((
                ((u32::from(b0) - 0xC0) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]),
                off + 3,
            ))
        }
        0xE0..=0xEF => {
            let b = rest.first_chunk::<4>()?;
            Some((
                ((u32::from(b0) - 0xE0) << 24)
                    | (u32::from(b[1]) << 16)
                    | (u32::from(b[2]) << 8)
                    | u32::from(b[3]),
                off + 4,
            ))
        }
        0xF0..=0xFF => {
            let b = rest.first_chunk::<5>()?;
            Some((u32::from_be_bytes([b[1], b[2], b[3], b[4]]), off + 5))
        }
    }
}

/// Rewrites every component gid in a single VARC glyph record to the
/// new-gid namespace. Walks the record with [`parse_one_component`] to
/// find each component's gid byte range, then splices the new gid in
/// place. The rewritten record has the same length unless the gid
/// encoding width changes. Today we keep the width identical (24-bit
/// stays 24-bit) for byte-stable output.
///
/// Test-only thin wrapper around [`rewrite_component_record`] with an
/// identity var-idx remap; the production path always goes through the
/// full record-rewrite to also apply the MVS prune remap.
#[cfg(test)]
fn rewrite_component_gids(
    record: &[u8],
    new_gid_for: &dyn Fn(GlyphId) -> Option<GlyphId>,
) -> Result<Vec<u8>, SubsetError> {
    rewrite_component_record(record, new_gid_for, &|outer, inner| Some((outer, inner)))
}

/// Encodes a `u32` using VARC's variable-length integer encoding,
/// picking the smallest form that fits. Mirrors `read_uint32var` in
/// `src/tables/varc.rs`.
///
/// Returns the encoded bytes (1-5 bytes long).
fn encode_uint32var(v: u32) -> Vec<u8> {
    let b = v.to_be_bytes();
    if v <= 0x7F {
        alloc::vec![b[3]]
    } else if v <= 0x3FFF {
        // Two-byte form: top bits (0x80..=0xBF) carry the high 6 bits.
        alloc::vec![b[2] | 0x80, b[3]]
    } else if v <= 0x001F_FFFF {
        // Three-byte form: top bits (0xC0..=0xDF).
        alloc::vec![b[1] | 0xC0, b[2], b[3]]
    } else if v <= 0x0FFF_FFFF {
        // Four-byte form: top bits (0xE0..=0xEF).
        alloc::vec![b[0] | 0xE0, b[1], b[2], b[3]]
    } else {
        // Five-byte form: 0xF0 marker + u32 BE.
        alloc::vec![0xF0, b[0], b[1], b[2], b[3]]
    }
}

/// Rewrites a single VARC glyph record:
///
/// - Every component gid is renumbered through `new_gid_for`.
/// - Every `MultiVarIdx` (transform deltas + axis-values deltas) is
///   remapped through `var_idx_remap`. The closure takes
///   `(outer, inner)` halves of the source `MultiVarIdx` and returns
///   the new halves, or `None` when the entry was unreferenced and is
///   being dropped (a hard error in this path: every var-idx the walker
///   sees in a surviving record must be in the kept set, otherwise the
///   pruning closure missed it).
///
/// Layout-wise the record is rebuilt by walking each component, copying
/// the gap from the previous cursor, splicing the new gid in place,
/// then splicing each (possibly width-changed) `MultiVarIdx`. Output
/// length differs from input length when var-idx encoding widths shift.
fn rewrite_component_record(
    record: &[u8],
    new_gid_for: &dyn Fn(GlyphId) -> Option<GlyphId>,
    var_idx_remap: &dyn Fn(u16, u16) -> Option<(u16, u16)>,
) -> Result<Vec<u8>, SubsetError> {
    const BAD_SPLICE: SubsetError = SubsetError::Unsupported("VARC component splice out of order");
    let mut out: Vec<u8> = Vec::with_capacity(record.len());
    let mut copy_from = 0usize;
    let mut cursor = 0usize;
    while cursor < record.len() {
        let Some((info, next)) = parse_one_component(record, cursor) else {
            break;
        };
        let new_gid = new_gid_for(info.gid).ok_or(SubsetError::Unsupported(
            "VARC component gid not in kept set",
        ))?;

        // Splice points inside this component, in source byte order:
        // gid first, then axis_var_idx, then transform_var_idx.
        let mut splices: Vec<(usize, usize, Vec<u8>)> = Vec::new();
        splices.push((
            info.gid_range.0,
            info.gid_range.1,
            if info.gid_is_24bit {
                let mut v = alloc::vec![0u8];
                v.extend_from_slice(&new_gid.to_be_bytes());
                v
            } else {
                new_gid.to_be_bytes().to_vec()
            },
        ));
        if let Some((s, e, v)) = info.axis_var_idx {
            let (outer, inner) = split_var_idx(v);
            let (no, ni) = var_idx_remap(outer, inner).ok_or(SubsetError::Unsupported(
                "VARC axis-values MultiVarIdx not in kept MVS set",
            ))?;
            let new_v = (u32::from(no) << 16) | u32::from(ni);
            splices.push((s, e, encode_uint32var(new_v)));
        }
        if let Some((s, e, v)) = info.transform_var_idx {
            let (outer, inner) = split_var_idx(v);
            let (no, ni) = var_idx_remap(outer, inner).ok_or(SubsetError::Unsupported(
                "VARC transform MultiVarIdx not in kept MVS set",
            ))?;
            let new_v = (u32::from(no) << 16) | u32::from(ni);
            splices.push((s, e, encode_uint32var(new_v)));
        }

        for (s, e, bytes) in splices {
            out.extend_from_slice(record.get(copy_from..s).ok_or(BAD_SPLICE)?);
            out.extend_from_slice(&bytes);
            copy_from = e;
        }

        if next <= cursor {
            break;
        }
        cursor = next;
    }
    out.extend_from_slice(record.get(copy_from..).ok_or(BAD_SPLICE)?);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Builds a coverage format-1 table.
    fn build_coverage(gids: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(gids.len() as u16).to_be_bytes());
        for g in gids {
            out.extend_from_slice(&g.to_be_bytes());
        }
        out
    }

    /// Builds a CFF2 INDEX with 1-byte offsets.
    fn build_cff2_index_test(entries: &[&[u8]]) -> Vec<u8> {
        let count = entries.len() as u32;
        let mut out = Vec::new();
        out.extend_from_slice(&count.to_be_bytes());
        if entries.is_empty() {
            return out;
        }
        out.push(1);
        let mut cursor: u32 = 1;
        out.push(cursor as u8);
        for e in entries {
            cursor += e.len() as u32;
            out.push(cursor as u8);
        }
        for e in entries {
            out.extend_from_slice(e);
        }
        out
    }

    /// Builds a synthetic VARC with the listed coverage gids and raw
    /// glyph records. Returns the assembled bytes.
    fn build_varc(coverage_gids: &[u16], glyph_records: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        let cov_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // varStore
        out.extend_from_slice(&0u32.to_be_bytes()); // conditionList
        out.extend_from_slice(&0u32.to_be_bytes()); // axisIndicesList
        let gr_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());

        let cov_off = out.len() as u32;
        out[cov_slot..cov_slot + 4].copy_from_slice(&cov_off.to_be_bytes());
        out.extend_from_slice(&build_coverage(coverage_gids));

        let gr_off = out.len() as u32;
        out[gr_slot..gr_slot + 4].copy_from_slice(&gr_off.to_be_bytes());
        out.extend_from_slice(&build_cff2_index_test(glyph_records));
        out
    }

    /// Builds a single-component record carrying just translate_x/y.
    fn build_translate_record(gid: u16, tx: i16, ty: i16) -> Vec<u8> {
        let flags = VC_HAVE_TRANSLATE_X | VC_HAVE_TRANSLATE_Y;
        let mut record = Vec::new();
        record.push(flags as u8);
        record.extend_from_slice(&gid.to_be_bytes());
        record.extend_from_slice(&tx.to_be_bytes());
        record.extend_from_slice(&ty.to_be_bytes());
        record
    }

    #[test]
    fn parse_finds_glyph_records_and_coverage() {
        let rec = build_translate_record(7, 10, 20);
        let bytes = build_varc(&[1], &[&rec]);
        let parsed = ParsedVarc::parse(&bytes).unwrap();
        assert_eq!(parsed.glyph_records.len(), 1);
        assert_eq!(parsed.coverage_index_of(1), Some(0));
        assert_eq!(parsed.coverage_index_of(99), None);
    }

    #[test]
    fn walk_component_gids_returns_referenced_gid() {
        let rec = build_translate_record(7, 10, 20);
        assert_eq!(walk_component_gids(&rec), vec![7u16]);
    }

    #[test]
    fn walk_component_gids_returns_all_components() {
        let mut rec = build_translate_record(5, 10, 20);
        rec.extend(build_translate_record(9, 30, 40));
        assert_eq!(walk_component_gids(&rec), vec![5, 9]);
    }

    #[test]
    fn coverage_iter_format1_walks_in_source_order() {
        let cov = build_coverage(&[1, 5, 10]);
        let pairs: Vec<_> = CoverageIter::new(&cov).collect();
        assert_eq!(pairs, vec![(1u16, 0usize), (5, 1), (10, 2)]);
    }

    #[test]
    fn coverage_iter_format2_yields_one_pair_per_gid_in_range() {
        // Format 2 with one range 100..=102 starting at coverage idx 5.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes()); // format
        bytes.extend_from_slice(&1u16.to_be_bytes()); // rangeCount
        bytes.extend_from_slice(&100u16.to_be_bytes()); // start
        bytes.extend_from_slice(&102u16.to_be_bytes()); // end
        bytes.extend_from_slice(&5u16.to_be_bytes()); // startCov
        let pairs: Vec<_> = CoverageIter::new(&bytes).collect();
        assert_eq!(pairs, vec![(100u16, 5usize), (101, 6), (102, 7)]);
    }

    #[test]
    fn build_coverage_emits_format_1() {
        let cov = build_coverage_format1([1u16, 5, 10]);
        assert_eq!(&cov[0..2], &1u16.to_be_bytes()); // format
        assert_eq!(&cov[2..4], &3u16.to_be_bytes()); // count
        assert_eq!(&cov[4..6], &1u16.to_be_bytes());
        assert_eq!(&cov[6..8], &5u16.to_be_bytes());
        assert_eq!(&cov[8..10], &10u16.to_be_bytes());
    }

    #[test]
    fn build_coverage_handles_empty_input() {
        let cov = build_coverage_format1(core::iter::empty());
        assert_eq!(&cov[0..2], &1u16.to_be_bytes());
        assert_eq!(&cov[2..4], &0u16.to_be_bytes());
        assert_eq!(cov.len(), 4);
    }

    #[test]
    fn walk_component_gids_handles_24bit_gid() {
        // VC_GID_IS_24BIT (1<<12 = 0x1000) requires a 2-byte uint32var:
        // 0x80|0x10 = 0x90, 0x00.
        let mut record = Vec::new();
        record.push(0x90);
        record.push(0x00);
        record.extend_from_slice(&[0x00, 0x12, 0x34]); // 24-bit gid 0x1234
        assert_eq!(walk_component_gids(&record), vec![0x1234]);
    }

    #[test]
    fn rewrite_component_gids_renumbers_basic_record() {
        let rec = build_translate_record(7, 10, 20);
        let map = |g: u16| if g == 7 { Some(42) } else { None };
        let new_rec = rewrite_component_gids(&rec, &map).unwrap();
        // Same length, but the embedded gid is now 42.
        assert_eq!(new_rec.len(), rec.len());
        assert_eq!(walk_component_gids(&new_rec), vec![42u16]);
    }

    #[test]
    fn rewrite_component_gids_renumbers_multi_component_record() {
        let mut rec = build_translate_record(5, 10, 20);
        rec.extend(build_translate_record(9, 30, 40));
        let map = |g: u16| match g {
            5 => Some(1),
            9 => Some(2),
            _ => None,
        };
        let new_rec = rewrite_component_gids(&rec, &map).unwrap();
        assert_eq!(walk_component_gids(&new_rec), vec![1u16, 2]);
        assert_eq!(new_rec.len(), rec.len());
    }

    #[test]
    fn rewrite_preserves_24bit_width() {
        // 24-bit gid must stay 24-bit on output even when the new gid
        // would fit in 16 bits. Keeps record byte length stable.
        let mut record = Vec::new();
        record.push(0x90);
        record.push(0x00);
        record.extend_from_slice(&[0x00, 0x12, 0x34]); // gid 0x1234
        let map = |g: u16| if g == 0x1234 { Some(7) } else { None };
        let new_record = rewrite_component_gids(&record, &map).unwrap();
        assert_eq!(new_record.len(), record.len());
        assert_eq!(walk_component_gids(&new_record), vec![7u16]);
    }

    #[test]
    fn rewrite_errors_when_kept_gid_lacks_mapping() {
        let rec = build_translate_record(7, 10, 20);
        // Map returns None for the source gid, so rewriter must error.
        let map = |_: u16| None;
        let err = rewrite_component_gids(&rec, &map).unwrap_err();
        assert!(matches!(err, SubsetError::Unsupported(_)));
    }

    /// VARC closure walker must terminate on a cyclic component graph
    /// (gid A references gid B, gid B references gid A). The fixed-
    /// point loop should converge after one iteration once both gids
    /// are in the kept set, regardless of the cycle.
    #[test]
    fn closure_terminates_on_circular_components() {
        // Two records: idx 0 covering gid 1 references gid 2; idx 1
        // covering gid 2 references gid 1.
        let cov = build_coverage(&[1, 2]);
        let rec_a = build_translate_record(2, 0, 0); // gid 1 -> gid 2
        let rec_b = build_translate_record(1, 0, 0); // gid 2 -> gid 1
        let bytes = build_varc(&[1, 2], &[&rec_a, &rec_b]);
        let _ = cov; // silence unused (build_varc constructs its own)
        let parsed = ParsedVarc::parse(&bytes).expect("parses");

        // Manually drive the cycle: start with gid 1, then iterate.
        let mut keep: alloc::collections::BTreeSet<GlyphId> = alloc::collections::BTreeSet::new();
        keep.insert(1);

        let mut iterations = 0;
        loop {
            let before = keep.len();
            let snapshot: alloc::vec::Vec<GlyphId> = keep.iter().copied().collect();
            for g in snapshot {
                let Some(idx) = parsed.coverage_index_of(g) else {
                    continue;
                };
                let Some(record) = parsed.glyph_record(idx) else {
                    continue;
                };
                for child in walk_component_gids(record) {
                    keep.insert(child);
                }
            }
            iterations += 1;
            if keep.len() == before {
                break;
            }
            assert!(
                iterations < 10,
                "VARC cycle walker must terminate quickly; iter={iterations}",
            );
        }
        // Both gids end up in the kept set, no infinite loop.
        assert!(keep.contains(&1));
        assert!(keep.contains(&2));
    }

    /// Regression for #196: a 24-bit gid with a non-zero high byte
    /// (>0xFFFF) used to truncate silently to its low 16 bits. The
    /// closure walker would then claim a wrong glyph was referenced.
    /// The walker must skip such records cleanly instead of fabricating
    /// a fake gid in the kept set.
    #[test]
    fn walk_skips_24bit_gid_overflowing_u16() {
        // VC_GID_IS_24BIT (1<<12 = 0x1000) -> uint32var encoding is the
        // two-byte form (0x80..=0xBF first byte): 0x90, 0x00.
        // 24-bit gid 0x010005: high byte non-zero, doesn't fit u16.
        let mut record = Vec::new();
        record.push(0x90);
        record.push(0x00);
        record.extend_from_slice(&[0x01, 0x00, 0x05]);
        // Walker must NOT yield 0x0005 (the truncated low bits). That
        // would lie about the source's reference graph.
        let gids = walk_component_gids(&record);
        assert!(
            gids.is_empty(),
            "u24 gid > 0xFFFF must be rejected, not silently truncated; got {gids:?}",
        );
    }

    #[test]
    fn cff2_index_round_trips_through_parser() {
        let entries: Vec<Vec<u8>> = vec![vec![0xAAu8, 0xBB], vec![0xCC, 0xDD, 0xEE]];
        let block = build_cff2_index(&entries);
        let parsed = parse_cff2_index(&block).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], &[0xAA, 0xBB][..]);
        assert_eq!(parsed[1], &[0xCC, 0xDD, 0xEE][..]);
    }

    #[test]
    fn subset_drops_table_when_no_covered_gid_kept() {
        // Coverage covers gid 5 only; kept set has only gid 2 -> drop.
        let rec = build_translate_record(7, 0, 0);
        let bytes = build_varc(&[5], &[&rec]);
        let varc = sigilbuzz::tables::Varc::parse(&bytes).unwrap();
        let map = |g: u16| Some(g);
        let kept = vec![2u16];
        let out = subset_varc(&varc, &bytes, &kept, &map).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn subset_keeps_table_with_renumbered_coverage() {
        let rec = build_translate_record(7, 10, 20);
        let bytes = build_varc(&[5], &[&rec]);
        let varc = sigilbuzz::tables::Varc::parse(&bytes).unwrap();
        // Map old gid 5 -> new gid 1, old gid 7 (component) -> new gid 2.
        let map = |g: u16| match g {
            5 => Some(1),
            7 => Some(2),
            _ => None,
        };
        let kept = vec![5u16, 7];
        let out = subset_varc(&varc, &bytes, &kept, &map).unwrap().unwrap();
        // Re-parse the output and verify it still passes the parser.
        let new_varc = sigilbuzz::tables::Varc::parse(&out).unwrap();
        assert!(new_varc.covers(1));
        assert!(!new_varc.covers(5));
        assert_eq!(new_varc.glyph_record_count(), 1);
        // Component gid in the new record is 2.
        let comp = new_varc.composite(1, &[]).unwrap();
        assert_eq!(comp.components.len(), 1);
        assert_eq!(comp.components[0].gid, 2);
        // Translation preserved verbatim.
        assert!((comp.components[0].transform[4] - 10.0).abs() < 1e-3);
        assert!((comp.components[0].transform[5] - 20.0).abs() < 1e-3);
    }

    /// Builds a region-list block with `regions.len()` entries. Each
    /// region is `&[(axis_index, start, peak, end)]`. Returns the raw
    /// bytes that would sit at the source MVS's `regionListOffset`.
    fn build_region_list(regions: &[&[(u16, f32, f32, f32)]]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(regions.len() as u16).to_be_bytes());
        let off_table_start = out.len();
        for _ in regions {
            out.extend_from_slice(&0u32.to_be_bytes());
        }
        let mut starts: Vec<u32> = Vec::with_capacity(regions.len());
        for region in regions {
            starts.push(out.len() as u32);
            out.extend_from_slice(&(region.len() as u16).to_be_bytes());
            for (ai, s, p, e) in *region {
                out.extend_from_slice(&ai.to_be_bytes());
                let raw_s = (s * 16384.0).round() as i16;
                let raw_p = (p * 16384.0).round() as i16;
                let raw_e = (e * 16384.0).round() as i16;
                out.extend_from_slice(&raw_s.to_be_bytes());
                out.extend_from_slice(&raw_p.to_be_bytes());
                out.extend_from_slice(&raw_e.to_be_bytes());
            }
        }
        for (i, s) in starts.iter().enumerate() {
            let slot = off_table_start + i * 4;
            out[slot..slot + 4].copy_from_slice(&s.to_be_bytes());
        }
        out
    }

    #[test]
    fn parse_region_list_decodes_each_region_payload() {
        let bytes = build_region_list(&[
            &[(0u16, 0.0, 1.0, 1.0)],
            &[(0u16, -1.0, -1.0, 0.0), (1u16, 0.0, 1.0, 1.0)],
        ]);
        let regions = parse_region_list(&bytes).unwrap();
        assert_eq!(regions.len(), 2);
        // First region: 1 axis -> 2 (axisCount) + 8 (one axis triple) = 10 bytes.
        assert_eq!(regions[0].len(), 10);
        // Second region: 2 axes -> 2 + 16 = 18 bytes.
        assert_eq!(regions[1].len(), 18);
    }

    #[test]
    fn parse_region_list_handles_zero_regions() {
        let bytes = build_region_list(&[]);
        let regions = parse_region_list(&bytes).unwrap();
        assert!(regions.is_empty());
    }

    #[test]
    fn collect_referenced_regions_unions_all_subtable_indexes() {
        let s0 = RewrittenMvsSubtable {
            region_indexes: vec![0, 2],
            delta_sets: Vec::new(),
        };
        let s1 = RewrittenMvsSubtable {
            region_indexes: vec![2, 3],
            delta_sets: Vec::new(),
        };
        let refs = collect_referenced_regions(&[s0, s1]);
        let v: Vec<u16> = refs.into_iter().collect();
        assert_eq!(v, vec![0, 2, 3]);
    }

    #[test]
    fn build_region_list_bytes_produces_parseable_output() {
        // Synthesize 3 regions, splice through parse_region_list,
        // re-emit via build_region_list_bytes, then re-parse.
        let src = build_region_list(&[
            &[(0u16, 0.0, 1.0, 1.0)],
            &[(1u16, -1.0, -1.0, 0.0)],
            &[(0u16, 0.0, 1.0, 1.0), (1u16, 0.0, 1.0, 1.0)],
        ]);
        let regions = parse_region_list(&src).unwrap();
        let rebuilt = build_region_list_bytes(&regions);
        let reparsed = parse_region_list(&rebuilt).unwrap();
        assert_eq!(reparsed.len(), 3);
        assert_eq!(reparsed[0], regions[0]);
        assert_eq!(reparsed[1], regions[1]);
        assert_eq!(reparsed[2], regions[2]);
    }

    #[test]
    fn build_region_list_bytes_handles_zero_regions() {
        let bytes = build_region_list_bytes(&[]);
        // Just a u16 region count of 0; no offset table, no payloads.
        assert_eq!(bytes.len(), 2);
        assert_eq!(&bytes[..2], &0u16.to_be_bytes());
    }

    /// Builds a coverage format-2 table from `(start, end, start_cov)`
    /// range records.
    fn build_coverage_format2(ranges: &[(u16, u16, u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes());
        out.extend_from_slice(&(ranges.len() as u16).to_be_bytes());
        for (start, end, cov) in ranges {
            out.extend_from_slice(&start.to_be_bytes());
            out.extend_from_slice(&end.to_be_bytes());
            out.extend_from_slice(&cov.to_be_bytes());
        }
        out
    }

    #[test]
    fn coverage_iter_stops_after_a_full_glyph_range() {
        // A range covering every glyph used to overflow the u16 range
        // offset after gid 0xFFFF: a panic in debug builds and an
        // endless iterator in release builds.
        let cov = build_coverage_format2(&[(0, 0xFFFF, 0)]);
        let mut count = 0usize;
        let mut last = None;
        for entry in CoverageIter::new(&cov) {
            count += 1;
            last = Some(entry);
        }
        assert_eq!(count, 1 << 16);
        assert_eq!(last, Some((0xFFFF, 0xFFFF)));
    }

    #[test]
    fn coverage_iter_caps_overlapping_ranges() {
        // Repeated full ranges would otherwise yield 65536 entries each.
        let cov = build_coverage_format2(&[(0, 0xFFFF, 0); 64]);
        assert_eq!(CoverageIter::new(&cov).count(), MAX_COVERAGE_ENTRIES);
    }

    #[test]
    fn subset_keeps_one_record_per_gid_and_record_index() {
        // Coverage lists gid 5 twice (both at record 0) and sends gid 6
        // to record 0 as well. Only the first entry survives, so the output
        // carries one record instead of copying record 0 three times.
        let rec = build_translate_record(7, 1, 2);
        let mut bytes = build_varc(&[5], &[&rec]);
        let cov = build_coverage_format2(&[(5, 5, 0), (5, 5, 0), (6, 6, 0)]);
        let cov_off = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&cov_off.to_be_bytes());
        bytes.extend_from_slice(&cov);
        let varc = sigilbuzz::tables::Varc::parse(&bytes).unwrap();
        let map = |g: u16| Some(g);
        let out = subset_varc(&varc, &bytes, &[5, 6, 7], &map)
            .unwrap()
            .unwrap();
        let new_varc = sigilbuzz::tables::Varc::parse(&out).unwrap();
        assert_eq!(new_varc.glyph_record_count(), 1);
        assert!(new_varc.covers(5));
    }

    #[test]
    fn parse_cff2_index_rejects_counts_past_the_block() {
        // A count near u32::MAX must be rejected by the length check,
        // not by an allocation or an overflowing size computation.
        let mut block = Vec::new();
        block.extend_from_slice(&u32::MAX.to_be_bytes());
        block.push(4);
        block.extend_from_slice(&[0u8; 16]);
        assert!(parse_cff2_index(&block).is_err());
    }
}
