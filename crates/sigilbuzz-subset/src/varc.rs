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
use sigilbuzz::Face;

use crate::GlyphId;

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

/// Internal lightweight parse of a VARC table — just enough to enumerate
/// coverage entries and glyph records. The full subset emit (commit
/// follow-up) layers Coverage rewrite + record-renumber on top of this.
struct ParsedVarc<'a> {
    coverage_bytes: &'a [u8],
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
        let glyph_records_off = read_u32(data, 20)? as usize;

        let coverage_bytes = data.get(coverage_off..).ok_or("VARC coverage off OOB")?;

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
            glyph_records,
        })
    }

    fn glyph_record(&self, idx: usize) -> Option<&'a [u8]> {
        self.glyph_records.get(idx).copied()
    }

    /// Parses the coverage table to find the index of `gid`.
    fn coverage_index_of(&self, gid: GlyphId) -> Option<usize> {
        coverage_index_of(self.coverage_bytes, gid)
    }
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
    /// Byte range inside the record where the gid lives — used by the
    /// rewrite path (commit follow-up) to splice in the new gid.
    #[allow(dead_code)]
    gid_range: (usize, usize),
    /// True when the gid was encoded as 24 bits (VC_GID_IS_24BIT).
    #[allow(dead_code)]
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
            let v = u32::from_be_bytes([
                data[off + 1],
                data[off + 2],
                data[off + 3],
                data[off + 4],
            ]);
            Some((v, off + 5))
        }
    }
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
    fn walk_component_gids_handles_24bit_gid() {
        // VC_GID_IS_24BIT (1<<12 = 0x1000) requires a 2-byte uint32var:
        // 0x80|0x10 = 0x90, 0x00.
        let mut record = Vec::new();
        record.push(0x90);
        record.push(0x00);
        record.extend_from_slice(&[0x00, 0x12, 0x34]); // 24-bit gid 0x1234
        assert_eq!(walk_component_gids(&record), vec![0x1234]);
    }
}
