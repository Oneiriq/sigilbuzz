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

use alloc::collections::BTreeSet;
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
/// The MultiVarStore, ConditionList, and AxisIndicesList are preserved
/// verbatim — pruning their entries when component references drop is a
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

    // Rewrite every surviving glyph record.
    let mut new_records: Vec<Vec<u8>> = Vec::with_capacity(renumbered.len());
    for (_, src_idx) in &renumbered {
        let raw = parsed
            .glyph_record(*src_idx)
            .ok_or(SubsetError::Unsupported("VARC glyph record index OOB"))?;
        let new_record = rewrite_component_gids(raw, &new_gid_for)?;
        new_records.push(new_record);
    }

    // Coverage format 1 with the new sorted gid list.
    let new_coverage = build_coverage_format1(renumbered.iter().map(|(g, _)| *g));

    // glyphRecords CFF2 INDEX over the rewritten records.
    let new_glyph_records = build_cff2_index(&new_records);

    // Pass-throughs.
    let var_store_bytes = parsed.var_store_bytes();
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

    // varStore
    if let Some(vs) = var_store_bytes {
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

    if flags & VC_AXIS_VALUES_HAVE_VARIATION != 0 {
        let (_var_idx, n) = read_uint32var(record, cur)?;
        cur = n;
    }

    if flags & VC_TRANSFORM_HAS_VARIATION != 0 {
        let (_var_idx, n) = read_uint32var(record, cur)?;
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
fn rewrite_component_gids(
    record: &[u8],
    new_gid_for: &dyn Fn(GlyphId) -> Option<GlyphId>,
) -> Result<Vec<u8>, SubsetError> {
    let mut splices: Vec<(usize, usize, GlyphId, bool)> = Vec::new();
    let mut cursor = 0usize;
    while cursor < record.len() {
        match parse_one_component(record, cursor) {
            Some((info, next)) => {
                let new_gid = new_gid_for(info.gid).ok_or(SubsetError::Unsupported(
                    "VARC component gid not in kept set",
                ))?;
                splices.push((
                    info.gid_range.0,
                    info.gid_range.1,
                    new_gid,
                    info.gid_is_24bit,
                ));
                if next <= cursor {
                    break;
                }
                cursor = next;
            }
            None => break,
        }
    }

    let mut out: Vec<u8> = Vec::with_capacity(record.len());
    let mut copy_from = 0usize;
    for (s, e, new_gid, is_24bit) in &splices {
        out.extend_from_slice(&record[copy_from..*s]);
        if *is_24bit {
            out.push(0); // top byte of u24 — sigilbuzz only uses 16-bit ids
            out.extend_from_slice(&new_gid.to_be_bytes());
        } else {
            out.extend_from_slice(&new_gid.to_be_bytes());
        }
        copy_from = *e;
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
}
