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
//! a fixed point: pulled-in gids may themselves be VARC-covered, and
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

mod component;
mod coverage;
mod mvs;

use component::{rewrite_component_record, walk_component_gids, walk_component_var_idxs};
use coverage::{build_coverage_format1, coverage_index_of, CoverageIter};
use mvs::{prune_multi_var_store, MvsRemap};

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
/// Tolerates malformed records silently. A single bad component record
/// should not stop the closure walk.
///
/// `BTreeSet` flavor: used by callers that already model the kept set
/// as a sorted set; the closure driver uses [`varc_closure_bitset`] for
/// the existing `Vec<bool>` representation.
#[allow(dead_code)] // public API surface. The in-tree driver uses the bitset variant
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

/// `Vec<bool>` flavor of the closure walk. Wires into the existing
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
/// AxisIndicesList are still preserved verbatim. Pruning those is a
/// future follow-up.
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
    let mut surviving: Vec<(GlyphId, usize)> = Vec::new();
    for (gid, idx) in parsed.coverage_iter() {
        if kept_set.contains(&gid) {
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

/// Reads u32 BE at `off`, bounds-checked.
fn read_u32(data: &[u8], off: usize) -> Result<u32, &'static str> {
    let bytes = data.get(off..off + 4).ok_or("u32 OOB")?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
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

#[cfg(test)]
mod tests;
