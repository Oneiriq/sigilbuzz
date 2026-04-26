//! `VARC` (Variable Composite Glyphs) subsetting.
//!
//! This module owns the VARC subset path: closure expansion (walking
//! every kept VARC-covered gid for its referenced component gids) and
//! the eventual table re-emit (Coverage / VarCompositeGlyph rewrite +
//! MultiVarStore pass-through). The first commit only ships the
//! closure walker; the table-rewrite emit lands alongside the driver
//! wire-up in a follow-up commit.
//!
//! # Closure phase
//!
//! For every VARC-covered gid in the kept set, walk the component
//! records and pull each referenced gid into the kept set. Iterate to
//! a fixed point — pulled-in gids may themselves be VARC-covered, and
//! so on. The walk caps recursion at 64 levels (the same hard cap the
//! parser uses against malicious cycles).
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

/// Maximum nesting depth for the closure walk. Matches the parser's
/// `MAX_VARC_DEPTH` cap on `Face::glyph_outline_at_coords`.
const MAX_VARC_DEPTH: usize = 64;

/// Expand `kept` to include every gid referenced (transitively) by a
/// VARC-covered gid already in the set. Iterates to a fixed point so
/// references to other VARC-covered gids cascade.
///
/// Tolerates malformed records silently — a single bad component record
/// should not stop the closure walk.
///
/// `BTreeSet` flavour: used by callers that already model the kept set
/// as a sorted set; the closure driver uses [`varc_closure_bitset`] for
/// the existing `Vec<bool>` representation.
#[allow(dead_code)] // public API surface — the in-tree driver uses the bitset variant
pub(crate) fn varc_closure(face: &Face<'_>, kept: &mut BTreeSet<GlyphId>) {
    let Ok(Some(varc)) = face.varc() else {
        return;
    };
    let Ok(varc_bytes) = face.table_bytes(tag::VARC) else {
        return;
    };
    let Ok(parsed) = ParsedVarc::parse(varc_bytes) else {
        return;
    };

    for _ in 0..MAX_VARC_DEPTH {
        let before = kept.len();
        let snapshot: Vec<GlyphId> = kept.iter().copied().collect();
        for g in snapshot {
            if !varc.covers(g) {
                continue;
            }
            let Some(idx) = parsed.coverage_index_of(g) else {
                continue;
            };
            let Some(record) = parsed.glyph_record(idx) else {
                continue;
            };
            for child in walk_component_gids(record) {
                kept.insert(child);
            }
        }
        if kept.len() == before {
            break;
        }
    }
}

/// `Vec<bool>` flavour of the closure walk — wires into the existing
/// closure driver in [`crate::closure`] which uses a bitset keyed by
/// gid. Same fixed-point iteration as [`varc_closure`].
pub(crate) fn varc_closure_bitset(face: &Face<'_>, keep: &mut [bool]) {
    let Ok(Some(varc)) = face.varc() else {
        return;
    };
    let Ok(varc_bytes) = face.table_bytes(tag::VARC) else {
        return;
    };
    let Ok(parsed) = ParsedVarc::parse(varc_bytes) else {
        return;
    };

    for _ in 0..MAX_VARC_DEPTH {
        let before = keep.iter().filter(|k| **k).count();
        let snapshot: Vec<GlyphId> = (0..keep.len())
            .filter(|i| keep[*i])
            .map(|i| i as GlyphId)
            .collect();
        for g in snapshot {
            if !varc.covers(g) {
                continue;
            }
            let Some(idx) = parsed.coverage_index_of(g) else {
                continue;
            };
            let Some(record) = parsed.glyph_record(idx) else {
                continue;
            };
            for child in walk_component_gids(record) {
                if (child as usize) < keep.len() {
                    keep[child as usize] = true;
                }
            }
        }
        let after = keep.iter().filter(|k| **k).count();
        if after == before {
            break;
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
/// AxisIndicesList are still preserved verbatim — pruning those is a
/// future follow-up.
pub(crate) fn subset_varc(
    src_varc: &Varc<'_>,
    src_bytes: &[u8],
    kept_gids: &[GlyphId],
    new_gid_for: &dyn Fn(GlyphId) -> Option<GlyphId>,
) -> Result<Option<Vec<u8>>, SubsetError> {
    let _ = src_varc; // signature compatibility — we re-parse the raw bytes
    let parsed = ParsedVarc::parse(src_bytes)
        .map_err(|_| SubsetError::Unsupported("VARC malformed during subset"))?;

    // Determine which coverage entries survive. Walk in source coverage
    // order so we can pull the right glyph record per entry.
    let kept_set: BTreeSet<GlyphId> = kept_gids.iter().copied().collect();
    let mut surviving: Vec<(GlyphId, usize)> = Vec::new();
    for (gid, idx) in parsed.coverage_iter() {
        if kept_set.contains(&gid) {
            surviving.push((gid, idx));
        }
    }

    if surviving.is_empty() {
        // No kept gid is VARC-covered → drop the whole table.
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
    let new_glyph_records = build_cff2_index(&new_records);

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

    // coverage
    let cov_start = out.len() as u32;
    out[cov_off_slot..cov_off_slot + 4].copy_from_slice(&cov_start.to_be_bytes());
    out.extend_from_slice(&new_coverage);
    while out.len() % 4 != 0 {
        out.push(0);
    }

    // varStore (rewritten with only the kept entries)
    if let Some(vs) = &new_var_store {
        let vs_start = out.len() as u32;
        out[vs_off_slot..vs_off_slot + 4].copy_from_slice(&vs_start.to_be_bytes());
        out.extend_from_slice(vs);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    // conditionList
    if let Some(cl) = condition_list_bytes {
        let cl_start = out.len() as u32;
        out[cl_off_slot..cl_off_slot + 4].copy_from_slice(&cl_start.to_be_bytes());
        out.extend_from_slice(cl);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    // axisIndicesList
    if let Some(ail) = axis_indices_bytes {
        let ail_start = out.len() as u32;
        out[ail_off_slot..ail_off_slot + 4].copy_from_slice(&ail_start.to_be_bytes());
        out.extend_from_slice(ail);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    // glyphRecords
    let gr_start = out.len() as u32;
    out[gr_off_slot..gr_off_slot + 4].copy_from_slice(&gr_start.to_be_bytes());
    out.extend_from_slice(&new_glyph_records);

    Ok(Some(out))
}

/// Builds a CFF2 INDEX over the given entries, picking the smallest
/// off_size that fits. Determinism: identical inputs always produce
/// identical output bytes.
fn build_cff2_index(entries: &[Vec<u8>]) -> Vec<u8> {
    let count = entries.len() as u32;
    let mut out = Vec::new();
    out.extend_from_slice(&count.to_be_bytes());
    if entries.is_empty() {
        return out;
    }
    let total: u32 = entries.iter().map(|e| e.len() as u32).sum();
    let max_off = total + 1;
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
    let write_off = |out: &mut Vec<u8>, v: u32| match off_size {
        1 => {
            #[allow(clippy::cast_possible_truncation)]
            out.push(v as u8);
        }
        2 => {
            #[allow(clippy::cast_possible_truncation)]
            out.extend_from_slice(&(v as u16).to_be_bytes());
        }
        3 => {
            out.push(((v >> 16) & 0xFF) as u8);
            out.push(((v >> 8) & 0xFF) as u8);
            out.push((v & 0xFF) as u8);
        }
        _ => out.extend_from_slice(&v.to_be_bytes()),
    };
    let mut cursor: u32 = 1;
    write_off(&mut out, cursor);
    for e in entries {
        cursor += e.len() as u32;
        write_off(&mut out, cursor);
    }
    for e in entries {
        out.extend_from_slice(e);
    }
    out
}

/// Internal lightweight parse of a VARC table — enumerates coverage
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

        let block_end = |start: usize| -> usize {
            let next = markers.iter().copied().find(|m| *m > start);
            next.unwrap_or(data.len())
        };

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

/// Iterator over coverage entries yielding `(gid, record_index)` pairs.
/// Used by [`subset_varc`] to walk the source coverage in order while
/// filtering on the kept-gid set.
struct CoverageIter<'a> {
    bytes: &'a [u8],
    format: u16,
    count: usize,
    cursor: usize,
    /// For format 2: which range we're inside.
    range_idx: usize,
    /// For format 2: current glyph inside the range (offset from start).
    range_offset: u16,
}

impl<'a> CoverageIter<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        if bytes.len() < 4 {
            return Self {
                bytes,
                format: 0,
                count: 0,
                cursor: 0,
                range_idx: 0,
                range_offset: 0,
            };
        }
        let format = u16::from_be_bytes([bytes[0], bytes[1]]);
        let count = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
        Self {
            bytes,
            format,
            count,
            cursor: 0,
            range_idx: 0,
            range_offset: 0,
        }
    }
}

impl<'a> Iterator for CoverageIter<'a> {
    type Item = (GlyphId, usize);

    fn next(&mut self) -> Option<Self::Item> {
        match self.format {
            1 => {
                if self.cursor >= self.count {
                    return None;
                }
                let off = 4 + self.cursor * 2;
                if off + 2 > self.bytes.len() {
                    return None;
                }
                let g = u16::from_be_bytes([self.bytes[off], self.bytes[off + 1]]);
                let idx = self.cursor;
                self.cursor += 1;
                Some((g, idx))
            }
            2 => {
                while self.range_idx < self.count {
                    let off = 4 + self.range_idx * 6;
                    if off + 6 > self.bytes.len() {
                        return None;
                    }
                    let start = u16::from_be_bytes([self.bytes[off], self.bytes[off + 1]]);
                    let end = u16::from_be_bytes([self.bytes[off + 2], self.bytes[off + 3]]);
                    let start_cov = u16::from_be_bytes([self.bytes[off + 4], self.bytes[off + 5]]);
                    let span = end.saturating_sub(start);
                    if self.range_offset > span {
                        self.range_idx += 1;
                        self.range_offset = 0;
                        continue;
                    }
                    let g = start.checked_add(self.range_offset)?;
                    let idx = (start_cov as usize) + (self.range_offset as usize);
                    self.range_offset += 1;
                    return Some((g, idx));
                }
                None
            }
            _ => None,
        }
    }
}

/// Builds a Coverage format-1 table from a sorted ascending iterator of
/// gids. Used by [`subset_varc`] to emit the rewritten coverage.
fn build_coverage_format1(gids: impl IntoIterator<Item = GlyphId>) -> Vec<u8> {
    let gids: Vec<GlyphId> = gids.into_iter().collect();
    let mut out = Vec::with_capacity(4 + gids.len() * 2);
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    #[allow(clippy::cast_possible_truncation)]
    let count = gids.len() as u16;
    out.extend_from_slice(&count.to_be_bytes());
    for g in gids {
        out.extend_from_slice(&g.to_be_bytes());
    }
    out
}

/// Reads u32 BE at `off`, bounds-checked.
fn read_u32(data: &[u8], off: usize) -> Result<u32, &'static str> {
    let bytes = data.get(off..off + 4).ok_or("u32 OOB")?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Walks a coverage table for the index of `gid`. Mirrors the parser's
/// search; returns `usize` so callers can index into the glyph-records
/// vec directly.
fn coverage_index_of(bytes: &[u8], gid: GlyphId) -> Option<usize> {
    if bytes.len() < 4 {
        return None;
    }
    let format = u16::from_be_bytes([bytes[0], bytes[1]]);
    let count = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
    match format {
        1 => {
            let need = 4 + count * 2;
            if bytes.len() < need {
                return None;
            }
            for i in 0..count {
                let off = 4 + i * 2;
                let g = u16::from_be_bytes([bytes[off], bytes[off + 1]]);
                if g == gid {
                    return Some(i);
                }
            }
            None
        }
        2 => {
            let need = 4 + count * 6;
            if bytes.len() < need {
                return None;
            }
            for i in 0..count {
                let off = 4 + i * 6;
                let start = u16::from_be_bytes([bytes[off], bytes[off + 1]]);
                let end = u16::from_be_bytes([bytes[off + 2], bytes[off + 3]]);
                let start_cov = u16::from_be_bytes([bytes[off + 4], bytes[off + 5]]);
                if gid >= start && gid <= end {
                    let cov = (start_cov as usize) + (gid - start) as usize;
                    return Some(cov);
                }
            }
            None
        }
        _ => None,
    }
}

/// Parses a CFF2 INDEX (u32 count + u8 offSize + offsets + data),
/// returning one byte slice per entry. Mirrors the layout the parser
/// uses for `glyphRecords`.
fn parse_cff2_index(block: &[u8]) -> Result<Vec<&[u8]>, &'static str> {
    if block.len() < 4 {
        return Err("CFF2 INDEX truncated header");
    }
    let count = u32::from_be_bytes([block[0], block[1], block[2], block[3]]) as usize;
    if count == 0 {
        return Ok(Vec::new());
    }
    if block.len() < 5 {
        return Err("CFF2 INDEX truncated offSize");
    }
    let off_size = block[4] as usize;
    if !(1..=4).contains(&off_size) {
        return Err("CFF2 INDEX offSize out of range");
    }
    let offsets_start = 5;
    let offsets_bytes = (count + 1) * off_size;
    if block.len() < offsets_start + offsets_bytes {
        return Err("CFF2 INDEX offsets truncated");
    }
    let mut offsets: Vec<usize> = Vec::with_capacity(count + 1);
    for i in 0..=count {
        let off = offsets_start + i * off_size;
        let mut v = 0u32;
        for k in 0..off_size {
            v = (v << 8) | u32::from(block[off + k]);
        }
        offsets.push(v as usize);
    }
    let data_start = offsets_start + offsets_bytes;
    let total = *offsets.last().unwrap();
    if total == 0 {
        return Err("CFF2 INDEX total length zero");
    }
    let data_len = total - 1;
    if block.len() < data_start + data_len {
        return Err("CFF2 INDEX data truncated");
    }
    let mut out: Vec<&[u8]> = Vec::with_capacity(count);
    for w in offsets.windows(2) {
        let a = w[0];
        let b = w[1];
        if a == 0 || b < a {
            return Err("CFF2 INDEX offsets non-monotone");
        }
        let s = data_start + a - 1;
        let e = data_start + b - 1;
        if e > block.len() {
            return Err("CFF2 INDEX entry past end");
        }
        out.push(&block[s..e]);
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
/// MVS doesn't actually have — those are silently skipped, but they
/// won't appear in the remap either, so the caller's
/// [`rewrite_component_record`] will surface the orphan via the
/// `Unsupported` path.
type MvsRemap = BTreeMap<(u16, u16), (u16, u16)>;

fn prune_multi_var_store(
    src: &[u8],
    referenced: &BTreeSet<(u16, u16)>,
) -> Result<(MvsRemap, Option<Vec<u8>>), SubsetError> {
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
    for (old_outer, sub) in parsed.subtables.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let old_outer_u16 = old_outer as u16;
        let Some(kept_inners) = by_outer.get(&old_outer_u16) else {
            continue;
        };
        // Keep only inners that exist in the source (defensive: an
        // out-of-range source ref means the source font is malformed).
        let mut kept_pairs: Vec<(u16, &[u8])> = Vec::new();
        for &inner in kept_inners {
            let Some(bytes) = sub.delta_sets.get(inner as usize).copied() else {
                continue;
            };
            kept_pairs.push((inner, bytes));
        }
        if kept_pairs.is_empty() {
            continue;
        }
        // Sorted by inner index — kept_inners is a BTreeSet so already
        // ascending; re-sort defensively in case future paths feed
        // unsorted refs in.
        kept_pairs.sort_by_key(|(i, _)| *i);

        #[allow(clippy::cast_possible_truncation)]
        let new_outer = new_subtables.len() as u16;
        let mut new_delta_sets: Vec<Vec<u8>> = Vec::with_capacity(kept_pairs.len());
        for (new_inner_idx, (old_inner, bytes)) in kept_pairs.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let new_inner = new_inner_idx as u16;
            remap.insert((old_outer_u16, *old_inner), (new_outer, new_inner));
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
    // — the resulting subtable simply carries no contribution from
    // that region, mirroring the parser's tolerant behaviour.
    let referenced_regions = collect_referenced_regions(&new_subtables);
    let src_regions = parse_region_list(&parsed.region_list_bytes)
        .map_err(|_| SubsetError::Unsupported("VARC MVS region list malformed during prune"))?;

    // Region remap: old region index → new region index. Built by
    // walking referenced regions in ascending order so the new region
    // list preserves source order — that keeps output bytes stable for
    // round-trip determinism.
    let mut region_remap: BTreeMap<u16, u16> = BTreeMap::new();
    let mut kept_region_payloads: Vec<&[u8]> = Vec::new();
    for &old_ri in &referenced_regions {
        let Some(payload) = src_regions.get(old_ri as usize).copied() else {
            // Source's tuple referenced a region that doesn't exist —
            // skip it. The subtable's region_indexes will be filtered
            // below and the region effectively contributes zero, which
            // matches the parser's behaviour for an OOB region.
            continue;
        };
        #[allow(clippy::cast_possible_truncation)]
        let new_ri = kept_region_payloads.len() as u16;
        region_remap.insert(old_ri, new_ri);
        kept_region_payloads.push(payload);
    }

    // Renumber each surviving subtable's region_indexes through the
    // remap. Drop indexes that lacked a kept region (defensive — if a
    // subtable ends up with zero region indexes after this filter,
    // every region it referenced was orphaned, which shouldn't happen
    // when the subtable prune is correct; we drop the subtable in that
    // case to keep the output structurally valid).
    let mut pruned_subtables: Vec<RewrittenMvsSubtable> = Vec::with_capacity(new_subtables.len());
    let mut outer_remap_collapse: BTreeMap<u16, u16> = BTreeMap::new();
    for (old_outer, sub) in new_subtables.into_iter().enumerate() {
        let mut new_region_indexes: Vec<u16> = Vec::with_capacity(sub.region_indexes.len());
        for ri in &sub.region_indexes {
            if let Some(&new_ri) = region_remap.get(ri) {
                new_region_indexes.push(new_ri);
            }
        }
        if new_region_indexes.is_empty() {
            // Defensive collapse — see comment above.
            continue;
        }
        #[allow(clippy::cast_possible_truncation)]
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
    // record-rewrite path sees the final outer indices. In practice
    // this branch is dead — the subtable prune above already drops
    // empty subtables — but guards against future edits where a
    // subtable could survive subtable pruning yet collapse here.
    if !outer_remap_collapse.is_empty() {
        for (_, (no, _)) in remap.iter_mut() {
            if let Some(&final_no) = outer_remap_collapse.get(no) {
                *no = final_no;
            }
        }
    }

    if pruned_subtables.is_empty() {
        // Every subtable's regions were orphaned — the MVS becomes
        // effectively region-less and contributes nothing. Drop it
        // entirely so the caller emits no varStore offset.
        return Ok((BTreeMap::new(), None));
    }

    let new_region_list_bytes = build_region_list_bytes(&kept_region_payloads);
    let new_bytes = emit_multi_var_store(&new_region_list_bytes, &pruned_subtables);
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
fn build_region_list_bytes(payloads: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    #[allow(clippy::cast_possible_truncation)]
    let count = payloads.len() as u16;
    out.extend_from_slice(&count.to_be_bytes());
    let off_table_start = out.len();
    for _ in payloads {
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    let mut starts: Vec<u32> = Vec::with_capacity(payloads.len());
    for p in payloads {
        #[allow(clippy::cast_possible_truncation)]
        let start = out.len() as u32;
        starts.push(start);
        out.extend_from_slice(p);
    }
    for (i, s) in starts.iter().enumerate() {
        let slot = off_table_start + i * 4;
        out[slot..slot + 4].copy_from_slice(&s.to_be_bytes());
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
/// prune — anything not in this set is unreachable after the MVS
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
/// triples) — they're spliced verbatim into the new region list, so the
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
    if bytes.len() < 2 {
        return Err("MVS region list header truncated");
    }
    let count = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
    let off_table_end = 2 + count * 4;
    if bytes.len() < off_table_end {
        return Err("MVS region list offsets truncated");
    }
    // Read region offsets (relative to region-list start). Their
    // ascending order plus the block end give us each region's byte
    // span.
    let mut offsets: Vec<usize> = Vec::with_capacity(count);
    for i in 0..count {
        let p = 2 + i * 4;
        let v = u32::from_be_bytes([bytes[p], bytes[p + 1], bytes[p + 2], bytes[p + 3]]) as usize;
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
        let end = sorted_bounds
            .iter()
            .copied()
            .find(|m| *m > start)
            .unwrap_or(bytes.len());
        let region = bytes.get(start..end).ok_or("MVS region body OOB")?;
        // Sanity: at least the axisCount u16 must fit.
        if region.len() < 2 {
            return Err("MVS region axisCount truncated");
        }
        let axis_count = u16::from_be_bytes([region[0], region[1]]) as usize;
        let need = 2 + axis_count * 8;
        if region.len() < need {
            return Err("MVS region axes truncated");
        }
        // Trim any trailing padding the source may have between
        // regions — emit only the region's structural bytes so the
        // rewriter produces a tightly-packed region list.
        regions.push(&region[..need]);
    }
    Ok(regions)
}

/// Lightweight parse of the MVS used by the pruner. Mirrors the layout
/// in `src/tables/multi_var_store.rs`. We keep references to the
/// region-list bytes so the rewriter can splice them back in verbatim.
struct ParsedMvs<'a> {
    /// Bytes of the region list block — header (regionCount + offset
    /// table) + every region payload, concatenated as in the source.
    /// The pruner re-emits these as-is.
    region_list_bytes: Vec<u8>,
    subtables: Vec<ParsedMvsSubtable<'a>>,
}

struct ParsedMvsSubtable<'a> {
    region_indexes: Vec<u16>,
    delta_sets: Vec<&'a [u8]>,
}

impl<'a> ParsedMvs<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, &'static str> {
        if data.len() < 8 {
            return Err("MVS header truncated");
        }
        let format = u16::from_be_bytes([data[0], data[1]]);
        if format != 1 {
            return Err("MVS unsupported format");
        }
        let region_list_off = u32::from_be_bytes([data[2], data[3], data[4], data[5]]) as usize;
        let subtable_count = u16::from_be_bytes([data[6], data[7]]) as usize;
        let mut subtable_offsets: Vec<usize> = Vec::with_capacity(subtable_count);
        for i in 0..subtable_count {
            let off = 8 + i * 4;
            if off + 4 > data.len() {
                return Err("MVS subtable offset OOB");
            }
            let v = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
                as usize;
            subtable_offsets.push(v);
        }

        // Region list — bytes from `region_list_off` to the start of
        // the next block. The region list contains its own offset
        // array; for the pruner we don't need to decode regions, just
        // capture the byte range.
        let region_list_end = compute_block_end(
            data.len(),
            region_list_off,
            &[region_list_off]
                .iter()
                .chain(subtable_offsets.iter())
                .copied()
                .collect::<Vec<_>>(),
        );
        let region_list_bytes = data
            .get(region_list_off..region_list_end)
            .ok_or("MVS region list OOB")?
            .to_vec();

        // Subtables.
        let mut markers: Vec<usize> = subtable_offsets.to_vec();
        markers.push(region_list_off);
        markers.push(data.len());
        let mut subtables: Vec<ParsedMvsSubtable<'a>> = Vec::with_capacity(subtable_count);
        for &off in &subtable_offsets {
            let sub_end = compute_block_end(data.len(), off, &markers);
            let block = data.get(off..sub_end).ok_or("MVS subtable OOB")?;
            if block.len() < 3 {
                return Err("MVS subtable header truncated");
            }
            if block[0] != 1 {
                return Err("MVS unsupported subtable format");
            }
            let region_index_count = u16::from_be_bytes([block[1], block[2]]) as usize;
            let need = 3 + region_index_count * 2;
            if block.len() < need {
                return Err("MVS subtable region indexes OOB");
            }
            let mut region_indexes: Vec<u16> = Vec::with_capacity(region_index_count);
            for i in 0..region_index_count {
                let p = 3 + i * 2;
                region_indexes.push(u16::from_be_bytes([block[p], block[p + 1]]));
            }
            let idx_start = need;
            // Use the absolute offset within `data` so the returned
            // slices outlive `block` (they borrow from `data`, lifetime
            // `'a`). `parse_cff2_index` takes a single slice and
            // returns sub-slices of it; we feed it the tail of `data`
            // starting at this subtable's CFF2 INDEX block.
            let abs_off = off + idx_start;
            let idx_block: &'a [u8] = data.get(abs_off..sub_end).ok_or("MVS delta index OOB")?;
            let delta_sets: Vec<&'a [u8]> = if idx_block.len() < 4 {
                Vec::new()
            } else {
                parse_cff2_index(idx_block).map_err(|_| "MVS delta CFF2 INDEX malformed")?
            };
            subtables.push(ParsedMvsSubtable {
                region_indexes,
                delta_sets,
            });
        }

        Ok(Self {
            region_list_bytes,
            subtables,
        })
    }
}

/// Returns the end offset of a block that starts at `start`, given a
/// list of all block start offsets in the table. The block ends at the
/// next-greater offset, or at `data_len` if none follow.
fn compute_block_end(data_len: usize, start: usize, all_offsets: &[usize]) -> usize {
    let mut next = data_len;
    for &o in all_offsets {
        if o > start && o < next {
            next = o;
        }
    }
    next
}

/// Re-emits the MVS bytes from rewritten subtables. Region list is
/// spliced in verbatim from the source. Subtable offsets are computed
/// fresh; each subtable carries its CFF2 INDEX of delta-set bytes.
fn emit_multi_var_store(region_list_bytes: &[u8], subtables: &[RewrittenMvsSubtable]) -> Vec<u8> {
    // Header layout:
    //   u16  format = 1
    //   u32  regionListOffset
    //   u16  subtableCount
    //   u32  subtableOffsets[subtableCount]
    let header_len = 2 + 4 + 2 + subtables.len() * 4;

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    let region_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    #[allow(clippy::cast_possible_truncation)]
    let subtable_count = subtables.len() as u16;
    out.extend_from_slice(&subtable_count.to_be_bytes());
    let sub_off_slots_start = out.len();
    for _ in subtables {
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    debug_assert_eq!(out.len(), header_len);

    // Region list directly follows the header, 4-byte aligned (the
    // header already ends on a 4-byte boundary because subtable
    // offsets are u32).
    #[allow(clippy::cast_possible_truncation)]
    let region_off = out.len() as u32;
    out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_off.to_be_bytes());
    out.extend_from_slice(region_list_bytes);
    while out.len() % 4 != 0 {
        out.push(0);
    }

    // Subtables, each preceded by 4-byte alignment.
    for (i, sub) in subtables.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let sub_off = out.len() as u32;
        let slot = sub_off_slots_start + i * 4;
        out[slot..slot + 4].copy_from_slice(&sub_off.to_be_bytes());
        out.push(1); // format
        #[allow(clippy::cast_possible_truncation)]
        let ric = sub.region_indexes.len() as u16;
        out.extend_from_slice(&ric.to_be_bytes());
        for ri in &sub.region_indexes {
            out.extend_from_slice(&ri.to_be_bytes());
        }
        out.extend_from_slice(&build_cff2_index(&sub.delta_sets));
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    out
}

/// Walks a single VarComposite glyph record and yields the gid of every
/// component. Tolerates malformed records by stopping mid-walk — the
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
/// MultiVarIdx referenced by its component records — both transform
/// deltas (`VC_TRANSFORM_HAS_VARIATION`) and axis-coord deltas
/// (`VC_AXIS_VALUES_HAVE_VARIATION`). Each value is split into its
/// outer/inner halves (high 16 bits → outer, low 16 → inner).
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
                    out.push(((v >> 16) as u16, (v & 0xFFFF) as u16));
                }
                if let Some((_, _, v)) = info.transform_var_idx {
                    out.push(((v >> 16) as u16, (v & 0xFFFF) as u16));
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

/// Per-component metadata extracted by `parse_one_component`.
struct ComponentInfo {
    /// Source-file gid this component points at.
    gid: GlyphId,
    /// Byte range inside the record where the gid lives — the rewrite
    /// path uses these bounds to splice in the new gid.
    gid_range: (usize, usize),
    /// True when the gid was encoded as 24 bits (VC_GID_IS_24BIT). The
    /// rewrite path keeps this width even if the new gid would fit in
    /// 16 bits — that's a future compaction follow-up and would
    /// otherwise risk shifting subsequent component records.
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
        if cur + 3 > record.len() {
            return None;
        }
        let g = (u32::from(record[cur]) << 16)
            | (u32::from(record[cur + 1]) << 8)
            | u32::from(record[cur + 2]);
        // sigilbuzz uses u16 gids throughout; a u24 source gid > 0xFFFF
        // would silently truncate to its low 16 bits and lie about the
        // reference graph (#196). Treat it as a malformed component and
        // bail — the walker's caller treats `None` as "no further
        // components in this record" and skips it tolerantly.
        if g > u32::from(u16::MAX) {
            return None;
        }
        #[allow(clippy::cast_possible_truncation)]
        let gid = g as u16;
        (gid, true, cur + 3)
    } else {
        if cur + 2 > record.len() {
            return None;
        }
        let g = u16::from_be_bytes([record[cur], record[cur + 1]]);
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
        // + payload — VARC's writers emit a single run per axisValues
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
    let mut field_count = 0usize;
    if flags & VC_HAVE_TRANSLATE_X != 0 {
        field_count += 1;
    }
    if flags & VC_HAVE_TRANSLATE_Y != 0 {
        field_count += 1;
    }
    if flags & VC_HAVE_ROTATION != 0 {
        field_count += 1;
    }
    if flags & VC_HAVE_SCALE_X != 0 {
        field_count += 1;
    }
    if flags & VC_HAVE_SCALE_Y != 0 {
        field_count += 1;
    }
    if flags & VC_HAVE_SKEW_X != 0 {
        field_count += 1;
    }
    if flags & VC_HAVE_SKEW_Y != 0 {
        field_count += 1;
    }
    if flags & VC_HAVE_TCENTER_X != 0 {
        field_count += 1;
    }
    if flags & VC_HAVE_TCENTER_Y != 0 {
        field_count += 1;
    }
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
    let mut cur = start;
    if cur >= data.len() {
        return Some(cur);
    }
    let ctrl = data[cur];
    cur += 1;
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
    cur += need;
    Some(cur)
}

/// Reads a uint32var starting at `off` in `data`. Returns the value plus
/// the byte position immediately after.
fn read_uint32var(data: &[u8], off: usize) -> Option<(u32, usize)> {
    if off >= data.len() {
        return None;
    }
    let b0 = data[off];
    match b0 {
        0x00..=0x7F => Some((u32::from(b0), off + 1)),
        0x80..=0xBF => {
            if off + 2 > data.len() {
                return None;
            }
            let b1 = data[off + 1];
            Some((((u32::from(b0) - 0x80) << 8) | u32::from(b1), off + 2))
        }
        0xC0..=0xDF => {
            if off + 3 > data.len() {
                return None;
            }
            let b1 = data[off + 1];
            let b2 = data[off + 2];
            Some((
                ((u32::from(b0) - 0xC0) << 16) | (u32::from(b1) << 8) | u32::from(b2),
                off + 3,
            ))
        }
        0xE0..=0xEF => {
            if off + 4 > data.len() {
                return None;
            }
            let b1 = data[off + 1];
            let b2 = data[off + 2];
            let b3 = data[off + 3];
            Some((
                ((u32::from(b0) - 0xE0) << 24)
                    | (u32::from(b1) << 16)
                    | (u32::from(b2) << 8)
                    | u32::from(b3),
                off + 4,
            ))
        }
        0xF0..=0xFF => {
            if off + 5 > data.len() {
                return None;
            }
            let v =
                u32::from_be_bytes([data[off + 1], data[off + 2], data[off + 3], data[off + 4]]);
            Some((v, off + 5))
        }
    }
}

/// Rewrites every component gid in a single VARC glyph record to the
/// new-gid namespace. Walks the record with [`parse_one_component`] to
/// find each component's gid byte range, then splices the new gid in
/// place. The rewritten record has the same length unless the gid
/// encoding width changes — today we keep the width identical (24-bit
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
/// Returns the encoded bytes (1–5 bytes long).
fn encode_uint32var(v: u32) -> Vec<u8> {
    if v <= 0x7F {
        #[allow(clippy::cast_possible_truncation)]
        let b = v as u8;
        alloc::vec![b]
    } else if v <= 0x3FFF {
        // Two-byte form: top bits (0x80..=0xBF) carry the high 6 bits.
        let hi = ((v >> 8) & 0x3F) as u8 | 0x80;
        let lo = (v & 0xFF) as u8;
        alloc::vec![hi, lo]
    } else if v <= 0x001F_FFFF {
        // Three-byte form: top bits (0xC0..=0xDF).
        let hi = ((v >> 16) & 0x1F) as u8 | 0xC0;
        let m = ((v >> 8) & 0xFF) as u8;
        let lo = (v & 0xFF) as u8;
        alloc::vec![hi, m, lo]
    } else if v <= 0x0FFF_FFFF {
        // Four-byte form: top bits (0xE0..=0xEF).
        let hi = ((v >> 24) & 0x0F) as u8 | 0xE0;
        let b1 = ((v >> 16) & 0xFF) as u8;
        let b2 = ((v >> 8) & 0xFF) as u8;
        let lo = (v & 0xFF) as u8;
        alloc::vec![hi, b1, b2, lo]
    } else {
        // Five-byte form: 0xF0 marker + u32 BE.
        let mut out = alloc::vec![0xF0_u8];
        out.extend_from_slice(&v.to_be_bytes());
        out
    }
}

/// Rewrites a single VARC glyph record:
///
/// - Every component gid is renumbered through `new_gid_for`.
/// - Every `MultiVarIdx` (transform deltas + axis-values deltas) is
///   remapped through `var_idx_remap` — the closure takes
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
    let mut out: Vec<u8> = Vec::with_capacity(record.len());
    let mut copy_from = 0usize;
    let mut cursor = 0usize;
    while cursor < record.len() {
        match parse_one_component(record, cursor) {
            Some((info, next)) => {
                let new_gid = new_gid_for(info.gid).ok_or(SubsetError::Unsupported(
                    "VARC component gid not in kept set",
                ))?;

                // Splice points inside this component, in source byte
                // order. (gid first, then axis_var_idx, then
                // transform_var_idx.)
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
                    let outer = (v >> 16) as u16;
                    let inner = (v & 0xFFFF) as u16;
                    let (no, ni) = var_idx_remap(outer, inner).ok_or(SubsetError::Unsupported(
                        "VARC axis-values MultiVarIdx not in kept MVS set",
                    ))?;
                    let new_v = (u32::from(no) << 16) | u32::from(ni);
                    splices.push((s, e, encode_uint32var(new_v)));
                }
                if let Some((s, e, v)) = info.transform_var_idx {
                    let outer = (v >> 16) as u16;
                    let inner = (v & 0xFFFF) as u16;
                    let (no, ni) = var_idx_remap(outer, inner).ok_or(SubsetError::Unsupported(
                        "VARC transform MultiVarIdx not in kept MVS set",
                    ))?;
                    let new_v = (u32::from(no) << 16) | u32::from(ni);
                    splices.push((s, e, encode_uint32var(new_v)));
                }

                // Splices are already in component-byte-order (gid
                // before any var_idx), but be defensive so future
                // reorders don't silently corrupt records.
                splices.sort_by_key(|(s, _, _)| *s);

                for (s, e, bytes) in splices {
                    out.extend_from_slice(&record[copy_from..s]);
                    out.extend_from_slice(&bytes);
                    copy_from = e;
                }

                if next <= cursor {
                    break;
                }
                cursor = next;
            }
            None => break,
        }
    }
    out.extend_from_slice(&record[copy_from..]);
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
        #[allow(clippy::cast_possible_truncation)]
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
        // would fit in 16 bits — keeps record byte length stable.
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
        // Map returns None for the source gid → rewriter must error.
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
        let rec_a = build_translate_record(2, 0, 0); // gid 1 → gid 2
        let rec_b = build_translate_record(1, 0, 0); // gid 2 → gid 1
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
    /// (>0xFFFF) used to truncate silently to its low 16 bits — the
    /// closure walker would then claim a wrong glyph was referenced.
    /// The walker must skip such records cleanly instead of fabricating
    /// a fake gid in the kept set.
    #[test]
    fn walk_skips_24bit_gid_overflowing_u16() {
        // VC_GID_IS_24BIT (1<<12 = 0x1000) → uint32var encoding is the
        // two-byte form (0x80..=0xBF first byte): 0x90, 0x00.
        // 24-bit gid 0x010005 — high byte non-zero, doesn't fit u16.
        let mut record = Vec::new();
        record.push(0x90);
        record.push(0x00);
        record.extend_from_slice(&[0x01, 0x00, 0x05]);
        // Walker must NOT yield 0x0005 (the truncated low bits) — that
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
        // Coverage covers gid 5 only; kept set has only gid 2 → drop.
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
        // Map old gid 5 → new gid 1, old gid 7 (component) → new gid 2.
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
}
