//! GPOS byte-level rewriter.
//!
//! Walks every lookup in the source `GPOS` table and produces a new
//! `GPOS` whose Coverage / ClassDef / glyph references resolve against
//! the new gid namespace defined by the caller's [`GidMap`]. GPOS
//! lookups don't introduce new gids — they only reposition existing
//! ones — so the rewriter is purely a filter + remap pass over the
//! per-type byte layout.
//!
//! # Per-lookup-type coverage
//!
//! As of this commit the rewriter ships byte-level support for:
//!
//! - **Type 1 (single-adj)** — formats 1 (uniform ValueRecord) and 2
//!   (per-glyph ValueRecord array). Filters Coverage; for fmt 2 drops
//!   the corresponding ValueRecord slots in lockstep. The ValueRecord
//!   bytes themselves travel verbatim — they contain no gid references.
//! - **Type 2 (pair-adj) format 1** — explicit pair entries. Filters
//!   Coverage of the first glyph; for each surviving PairSet, walks
//!   PairValueRecords and drops pairs whose `secondGlyph` was dropped.
//!   Empty PairSets collapse to a Coverage drop.
//! - **Type 2 (pair-adj) format 2** — class-based matrix. The class
//!   structure is gid-independent at the table level, but a kept-gid
//!   subset can drop entire classes or merge them. The rewriter passes
//!   through when both ClassDefs reduce cleanly without re-numbering
//!   their classes; when class structure changes the subtable drops
//!   (a follow-up issue tracks fmt-1 fallback synthesis). Coverage is
//!   filtered to surviving first glyphs.
//! - **Type 3 (cursive)** — Coverage + EntryExitRecord array. Filters
//!   Coverage; drops corresponding entry/exit slots. Anchors travel
//!   verbatim (coordinates only, no gid references).
//! - **Type 4 / 5 / 6 (mark attachment)** — Mark+Base / Mark+Liga /
//!   Mark1+Mark2 Coverages with parallel MarkArray and BaseArray /
//!   LigatureArray / Mark2Array entries. Filters both Coverages, drops
//!   array entries in lockstep. Anchors and class IDs travel verbatim.
//! - **Type 9 (extension)** — pass-through after rewriting the inner
//!   subtable. Falls back to a lookup drop when the inner type has no
//!   rewriter.
//!
//! Type 7 (context positioning) and type 8 (chained context) drop their
//! lookups; the drop cascade then removes empty subtables, lookups,
//! features, and scripts as usual. Sibling issue #126 tracks landing
//! the context rewriters (and the PairPos fmt-2 fmt-1 fallback).

use alloc::vec::Vec;

use sigilbuzz::tables::gpos::lookup_type as gpos_type;

use crate::coverage::emit_coverage_from_pairs;
use crate::layout::{
    parse_classdef_pairs_from_bytes, parse_coverage_glyphs, RewriterCtx, RewrittenLookup,
    RewrittenSubtable,
};

/// Rewrites a single GPOS lookup. Returns `None` if the lookup has no
/// surviving subtables after rewriting (drop cascade will remove the
/// lookup).
pub(crate) fn rewrite_lookup(
    ctx: &RewriterCtx,
    lookup_type: u16,
    lookup_flag: u16,
    mark_filtering_set: Option<u16>,
    subtable_bodies: &[&[u8]],
) -> Option<RewrittenLookup> {
    let mut rewritten_subs: Vec<RewrittenSubtable> = Vec::new();

    for &sub_bytes in subtable_bodies {
        if let Some(rs) = rewrite_subtable(ctx, lookup_type, sub_bytes) {
            rewritten_subs.push(rs);
        }
    }

    if rewritten_subs.is_empty() {
        return None;
    }

    Some(RewrittenLookup {
        lookup_type,
        lookup_flag,
        mark_filtering_set,
        subtables: rewritten_subs,
    })
}

fn rewrite_subtable(ctx: &RewriterCtx, lookup_type: u16, sub: &[u8]) -> Option<RewrittenSubtable> {
    match lookup_type {
        gpos_type::SINGLE_ADJUSTMENT => rewrite_single_adj(ctx, sub),
        gpos_type::PAIR_ADJUSTMENT => rewrite_pair_pos(ctx, sub),
        gpos_type::CURSIVE_ATTACHMENT => rewrite_cursive(ctx, sub),
        gpos_type::MARK_TO_BASE | gpos_type::MARK_TO_MARK => {
            rewrite_mark_attach(ctx, sub, MarkAttachKind::FixedClassRow)
        }
        gpos_type::MARK_TO_LIGATURE => {
            rewrite_mark_attach(ctx, sub, MarkAttachKind::LigatureAttach)
        }
        gpos_type::EXTENSION => rewrite_extension(ctx, sub),
        // Context (7) and chained context (8) drop until their
        // byte-level rewriters ship — see follow-up issue.
        _ => None,
    }
}

/// Distinguishes the "second array" layout shared by mark-attachment
/// lookups. Types 4 and 6 use a fixed `markClassCount` row of anchor
/// offsets per coverage entry; type 5 uses one Offset16 per coverage
/// entry pointing at a variable-length `LigatureAttach`.
#[derive(Copy, Clone)]
enum MarkAttachKind {
    /// Type 4 / 6 — base / mark2 array.
    FixedClassRow,
    /// Type 5 — ligature array.
    LigatureAttach,
}

/// One surviving entry in the second array of a mark-attach lookup.
/// For type 4 / 6 (`MarkAttachKind::FixedClassRow`) the second slot is
/// empty and the third holds `markClassCount` anchor-byte vectors
/// (an empty vector means a null offset). For type 5
/// (`MarkAttachKind::LigatureAttach`) the second slot holds the
/// already-rewritten LigatureAttach body and the third is empty.
type SurvivingBase = (u16, Vec<u8>, Vec<Vec<u8>>);

// ---------------------------------------------------------------------------
// Type 1 — Single Adjustment
// ---------------------------------------------------------------------------

/// Rewrites a Single Adjustment subtable.
///
/// Format 1 layout:
///
/// ```text
///   u16         posFormat = 1
///   Offset16    coverageOffset
///   u16         valueFormat
///   ValueRecord valueRecord
/// ```
///
/// Format 2 layout:
///
/// ```text
///   u16         posFormat = 2
///   Offset16    coverageOffset
///   u16         valueFormat
///   u16         valueCount       (== Coverage entry count)
///   ValueRecord valueRecords[valueCount]
/// ```
///
/// The ValueRecord body is gid-independent: every field is either a
/// signed coordinate delta or a Device/VariationIndex offset. Filtering
/// is purely about which Coverage entries survive; the bytes for the
/// surviving ValueRecord(s) travel verbatim.
fn rewrite_single_adj(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 6 {
        return None;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let value_format = u16::from_be_bytes([sub[4], sub[5]]);
    let cov_bytes = sub.get(cov_off..)?;
    let covered = parse_coverage_glyphs(cov_bytes);
    let stride = value_record_size(value_format);
    let map = ctx.gid_map;

    match format {
        1 => {
            // One shared ValueRecord. The stride bytes after the
            // 6-byte header carry it.
            let value_end = 6 + stride;
            if sub.len() < value_end {
                return None;
            }
            let value_bytes = &sub[6..value_end];

            let mut surviving_gids: Vec<u16> = Vec::new();
            for &g in &covered {
                if let Some(new) = map.map(g) {
                    surviving_gids.push(new);
                }
            }
            if surviving_gids.is_empty() {
                return None;
            }
            Some(emit_single_adj_format1(
                value_format,
                value_bytes,
                &surviving_gids,
            ))
        }
        2 => {
            // Per-glyph ValueRecord array right after the header.
            let value_count = u16::from_be_bytes([sub[6], sub[7]]) as usize;
            let values_off = 8usize;
            let need = values_off + value_count * stride;
            if sub.len() < need {
                return None;
            }
            // Spec requires Coverage entry count == valueCount; if the
            // source is malformed we cap.
            let pair_count = covered.len().min(value_count);

            let mut surviving: Vec<(u16, Vec<u8>)> = Vec::new();
            for (i, &g_old) in covered.iter().enumerate().take(pair_count) {
                let Some(g_new) = map.map(g_old) else {
                    continue;
                };
                let off = values_off + i * stride;
                let body = sub[off..off + stride].to_vec();
                surviving.push((g_new, body));
            }
            if surviving.is_empty() {
                return None;
            }
            Some(emit_single_adj_format2(value_format, &surviving))
        }
        _ => None,
    }
}

fn emit_single_adj_format1(
    value_format: u16,
    value_bytes: &[u8],
    surviving_gids: &[u16],
) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
    out.extend_from_slice(&value_format.to_be_bytes());
    out.extend_from_slice(value_bytes);
    let cov_off = out.len() as u16;
    let cov_bytes = crate::coverage::emit_coverage_from_glyphs(surviving_gids);
    out.extend_from_slice(&cov_bytes);
    out[cov_slot..cov_slot + 2].copy_from_slice(&cov_off.to_be_bytes());
    RewrittenSubtable { bytes: out }
}

fn emit_single_adj_format2(value_format: u16, surviving: &[(u16, Vec<u8>)]) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes()); // posFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
    out.extend_from_slice(&value_format.to_be_bytes());
    out.extend_from_slice(&(surviving.len() as u16).to_be_bytes()); // valueCount
    for (_, body) in surviving {
        out.extend_from_slice(body);
    }
    let cov_off = out.len() as u16;
    let pairs: Vec<(u16, u16)> = surviving
        .iter()
        .enumerate()
        .map(|(i, (g, _))| (*g, i as u16))
        .collect();
    let cov_bytes = emit_coverage_from_pairs(&pairs);
    out.extend_from_slice(&cov_bytes);
    out[cov_slot..cov_slot + 2].copy_from_slice(&cov_off.to_be_bytes());
    RewrittenSubtable { bytes: out }
}

/// Number of bytes a `ValueRecord` with the given format word occupies.
/// Mirrors `sigilbuzz::tables::gpos::value_record::ValueRecord::size`
/// without taking a runtime dependency on the parser.
fn value_record_size(format: u16) -> usize {
    // Each set defined bit is one i16 (or Offset16, same size).
    // Defined bits are 0x0001..=0x0080.
    const DEFINED: u16 = 0x00FF;
    (format & DEFINED).count_ones() as usize * 2
}

// ---------------------------------------------------------------------------
// Type 2 — Pair Adjustment
// ---------------------------------------------------------------------------

/// Rewrites a PairPos subtable. Dispatches on format.
fn rewrite_pair_pos(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 2 {
        return None;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    match format {
        1 => rewrite_pair_pos_format1(ctx, sub),
        2 => rewrite_pair_pos_format2(ctx, sub),
        _ => None,
    }
}

/// Rewrites PairPos format 1.
///
/// Layout:
///
/// ```text
///   u16      posFormat = 1
///   Offset16 coverageOffset
///   u16      valueFormat1
///   u16      valueFormat2
///   u16      pairSetCount         (== Coverage entry count)
///   Offset16 pairSetOffsets[pairSetCount]
///
///   PairSet:
///     u16             pairValueCount
///     PairValueRecord pairValueRecords[pairValueCount]
///
///   PairValueRecord:
///     u16         secondGlyph
///     ValueRecord valueRecord1   (sized by valueFormat1)
///     ValueRecord valueRecord2   (sized by valueFormat2)
/// ```
fn rewrite_pair_pos_format1(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 10 {
        return None;
    }
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let value_format1 = u16::from_be_bytes([sub[4], sub[5]]);
    let value_format2 = u16::from_be_bytes([sub[6], sub[7]]);
    let pair_set_count = u16::from_be_bytes([sub[8], sub[9]]) as usize;
    let set_offsets_off = 10usize;
    if sub.len() < set_offsets_off + pair_set_count * 2 {
        return None;
    }
    let cov_bytes = sub.get(cov_off..)?;
    let covered = parse_coverage_glyphs(cov_bytes);
    let pair_count = covered.len().min(pair_set_count);

    let v1_size = value_record_size(value_format1);
    let v2_size = value_record_size(value_format2);
    let pvr_size = 2 + v1_size + v2_size;

    let map = ctx.gid_map;
    // (new_first_gid, encoded_pair_set_bytes) for surviving entries.
    let mut surviving_sets: Vec<(u16, Vec<u8>)> = Vec::new();

    for (i, &first_old) in covered.iter().enumerate().take(pair_count) {
        let Some(first_new) = map.map(first_old) else {
            continue;
        };
        let off_off = set_offsets_off + i * 2;
        let set_off = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        let Some(set_bytes) = sub.get(set_off..) else {
            continue;
        };
        if set_bytes.len() < 2 {
            continue;
        }
        let pair_value_count = u16::from_be_bytes([set_bytes[0], set_bytes[1]]) as usize;
        let need = 2 + pair_value_count * pvr_size;
        if set_bytes.len() < need {
            continue;
        }
        // Filter PairValueRecords by surviving secondGlyph.
        let mut survivors: Vec<(u16, Vec<u8>)> = Vec::new();
        for j in 0..pair_value_count {
            let off = 2 + j * pvr_size;
            let second_old = u16::from_be_bytes([set_bytes[off], set_bytes[off + 1]]);
            let Some(second_new) = map.map(second_old) else {
                continue;
            };
            // Bytes following the secondGlyph are the two ValueRecords;
            // both are gid-independent so they travel verbatim.
            let body_off = off + 2;
            let body_end = body_off + v1_size + v2_size;
            let body = set_bytes[body_off..body_end].to_vec();
            survivors.push((second_new, body));
        }
        if survivors.is_empty() {
            continue;
        }
        // Spec requires PairValueRecords sorted by secondGlyph; sort
        // and dedupe.
        survivors.sort_by_key(|(g, _)| *g);
        survivors.dedup_by_key(|(g, _)| *g);
        let new_set = encode_pair_set(&survivors, v1_size, v2_size);
        surviving_sets.push((first_new, new_set));
    }

    if surviving_sets.is_empty() {
        return None;
    }

    Some(emit_pair_pos_format1(
        value_format1,
        value_format2,
        &surviving_sets,
    ))
}

fn encode_pair_set(survivors: &[(u16, Vec<u8>)], _v1: usize, _v2: usize) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(survivors.len() as u16).to_be_bytes());
    for (second, body) in survivors {
        out.extend_from_slice(&second.to_be_bytes());
        out.extend_from_slice(body);
    }
    out
}

fn emit_pair_pos_format1(
    value_format1: u16,
    value_format2: u16,
    surviving: &[(u16, Vec<u8>)],
) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
    out.extend_from_slice(&value_format1.to_be_bytes());
    out.extend_from_slice(&value_format2.to_be_bytes());
    out.extend_from_slice(&(surviving.len() as u16).to_be_bytes()); // pairSetCount
    let set_offsets_start = out.len();
    for _ in 0..surviving.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    for (i, (_first, set_body)) in surviving.iter().enumerate() {
        let body_start = out.len();
        out.extend_from_slice(set_body);
        let slot = set_offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
    }
    let cov_off = out.len() as u16;
    let pairs: Vec<(u16, u16)> = surviving
        .iter()
        .enumerate()
        .map(|(i, (g, _))| (*g, i as u16))
        .collect();
    let cov_bytes = emit_coverage_from_pairs(&pairs);
    out.extend_from_slice(&cov_bytes);
    out[cov_slot..cov_slot + 2].copy_from_slice(&cov_off.to_be_bytes());
    RewrittenSubtable { bytes: out }
}

/// Rewrites PairPos format 2 (class-based matrix).
///
/// Layout:
///
/// ```text
///   u16      posFormat = 2
///   Offset16 coverageOffset
///   u16      valueFormat1
///   u16      valueFormat2
///   Offset16 classDef1Offset
///   Offset16 classDef2Offset
///   u16      class1Count
///   u16      class2Count
///   Class1Record class1Records[class1Count]:
///     Class2Record class2Records[class2Count]:
///       ValueRecord valueRecord1   (sized by valueFormat1)
///       ValueRecord valueRecord2   (sized by valueFormat2)
/// ```
///
/// # Drop policy
///
/// The class matrix is gid-independent, but a kept-gid subset can
/// drop entire classes (every class member dropped) or split the
/// "implicit class 0" gids across the kept/dropped boundary. The
/// safest behaviour without re-numbering classes is:
///
/// - Walk both ClassDefs and rewrite them through the GidMap. If a
///   surviving gid's class number remains valid (within
///   `class1Count` / `class2Count`), preserve the matrix verbatim.
/// - If any class has no surviving gid, the matrix row/column is
///   wasted but still safe — keep it.
/// - Coverage is filtered to surviving first glyphs.
///
/// Re-numbering classes (compacting class IDs) would require rewriting
/// the matrix to re-order rows and columns. Today that's out of scope:
/// we keep the matrix and ClassDef shapes intact and rely on Coverage
/// pruning + the drop cascade to handle the rest. Follow-up issue
/// tracks fmt-1 fallback synthesis when class structure collapses.
fn rewrite_pair_pos_format2(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 16 {
        return None;
    }
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let value_format1 = u16::from_be_bytes([sub[4], sub[5]]);
    let value_format2 = u16::from_be_bytes([sub[6], sub[7]]);
    let cd1_off = u16::from_be_bytes([sub[8], sub[9]]) as usize;
    let cd2_off = u16::from_be_bytes([sub[10], sub[11]]) as usize;
    let class1_count = u16::from_be_bytes([sub[12], sub[13]]);
    let class2_count = u16::from_be_bytes([sub[14], sub[15]]);
    let records_off = 16usize;

    let v_pair = value_record_size(value_format1) + value_record_size(value_format2);
    let class2_stride = v_pair;
    let class1_stride = class2_count as usize * class2_stride;
    let need = records_off + class1_count as usize * class1_stride;
    if sub.len() < need {
        return None;
    }

    let cov_bytes = sub.get(cov_off..)?;
    let covered = parse_coverage_glyphs(cov_bytes);
    let cd1_bytes = sub.get(cd1_off..)?;
    let cd2_bytes = sub.get(cd2_off..)?;
    let cd1_pairs = parse_classdef_pairs_from_bytes(cd1_bytes);
    let cd2_pairs = parse_classdef_pairs_from_bytes(cd2_bytes);

    let map = ctx.gid_map;

    // Filter Coverage to surviving first glyphs.
    let mut surviving_cov: Vec<u16> = Vec::new();
    for &g in &covered {
        if let Some(new) = map.map(g) {
            surviving_cov.push(new);
        }
    }
    if surviving_cov.is_empty() {
        return None;
    }

    // Filter ClassDefs: keep (new_gid, class) pairs whose class is
    // still valid in the matrix (< classNCount). We do not renumber
    // classes — the matrix bytes are preserved verbatim.
    let surviving_cd1: Vec<(u16, u16)> = cd1_pairs
        .iter()
        .filter_map(|&(g, c)| {
            let new = map.map(g)?;
            if c < class1_count {
                Some((new, c))
            } else {
                None
            }
        })
        .collect();
    let surviving_cd2: Vec<(u16, u16)> = cd2_pairs
        .iter()
        .filter_map(|&(g, c)| {
            let new = map.map(g)?;
            if c < class2_count {
                Some((new, c))
            } else {
                None
            }
        })
        .collect();

    let cd1_bytes_new = crate::classdef::emit_classdef(&surviving_cd1);
    let cd2_bytes_new = crate::classdef::emit_classdef(&surviving_cd2);

    // Matrix bytes travel verbatim — neither ValueRecord field nor
    // class indices changed.
    let matrix_bytes =
        sub[records_off..records_off + class1_count as usize * class1_stride].to_vec();

    Some(emit_pair_pos_format2(
        value_format1,
        value_format2,
        class1_count,
        class2_count,
        &surviving_cov,
        &cd1_bytes_new,
        &cd2_bytes_new,
        &matrix_bytes,
    ))
}

#[allow(clippy::too_many_arguments)]
fn emit_pair_pos_format2(
    value_format1: u16,
    value_format2: u16,
    class1_count: u16,
    class2_count: u16,
    surviving_cov: &[u16],
    cd1_bytes: &[u8],
    cd2_bytes: &[u8],
    matrix_bytes: &[u8],
) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes()); // posFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
    out.extend_from_slice(&value_format1.to_be_bytes());
    out.extend_from_slice(&value_format2.to_be_bytes());
    let cd1_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // classDef1Offset placeholder
    let cd2_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // classDef2Offset placeholder
    out.extend_from_slice(&class1_count.to_be_bytes());
    out.extend_from_slice(&class2_count.to_be_bytes());
    out.extend_from_slice(matrix_bytes);

    let cov_start = out.len() as u16;
    let cov_emitted = crate::coverage::emit_coverage_from_glyphs(surviving_cov);
    out.extend_from_slice(&cov_emitted);
    out[cov_slot..cov_slot + 2].copy_from_slice(&cov_start.to_be_bytes());

    let cd1_start = out.len() as u16;
    out.extend_from_slice(cd1_bytes);
    out[cd1_slot..cd1_slot + 2].copy_from_slice(&cd1_start.to_be_bytes());

    let cd2_start = out.len() as u16;
    out.extend_from_slice(cd2_bytes);
    out[cd2_slot..cd2_slot + 2].copy_from_slice(&cd2_start.to_be_bytes());

    RewrittenSubtable { bytes: out }
}

// ---------------------------------------------------------------------------
// Type 3 — Cursive Attachment
// ---------------------------------------------------------------------------

/// Rewrites a Cursive Attachment subtable.
///
/// Layout:
///
/// ```text
///   u16      posFormat = 1
///   Offset16 coverageOffset
///   u16      entryExitCount       (== Coverage entry count)
///   EntryExitRecord records[entryExitCount]:
///     Offset16 entryAnchorOffset  (relative to subtable; 0 = none)
///     Offset16 exitAnchorOffset   (relative to subtable; 0 = none)
/// ```
///
/// Anchors are pure `(x, y)` coordinate pairs (with optional hinting
/// trailers); they don't reference gids. We re-emit each surviving
/// anchor's body verbatim and re-thread the offsets.
fn rewrite_cursive(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 6 {
        return None;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    if format != 1 {
        return None;
    }
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let count = u16::from_be_bytes([sub[4], sub[5]]) as usize;
    let records_off = 6usize;
    if sub.len() < records_off + count * 4 {
        return None;
    }
    let cov_bytes = sub.get(cov_off..)?;
    let covered = parse_coverage_glyphs(cov_bytes);
    let pair_count = covered.len().min(count);
    let map = ctx.gid_map;

    // Survivor: (new_gid, entry_anchor_bytes, exit_anchor_bytes). An
    // empty Vec stands in for "null offset (0)".
    let mut surviving: Vec<(u16, Vec<u8>, Vec<u8>)> = Vec::new();
    for (i, &g_old) in covered.iter().enumerate().take(pair_count) {
        let Some(g_new) = map.map(g_old) else {
            continue;
        };
        let rec_off = records_off + i * 4;
        let entry_off = u16::from_be_bytes([sub[rec_off], sub[rec_off + 1]]) as usize;
        let exit_off = u16::from_be_bytes([sub[rec_off + 2], sub[rec_off + 3]]) as usize;
        let entry_bytes = read_anchor_bytes(sub, entry_off);
        let exit_bytes = read_anchor_bytes(sub, exit_off);
        surviving.push((g_new, entry_bytes, exit_bytes));
    }
    if surviving.is_empty() {
        return None;
    }

    Some(emit_cursive(&surviving))
}

/// Reads an anchor table at `offset` inside `sub`. Returns an empty Vec
/// for null offsets (0). Anchors are 6 bytes (fmt 1), 8 bytes (fmt 2),
/// or 10 bytes (fmt 3).
fn read_anchor_bytes(sub: &[u8], offset: usize) -> Vec<u8> {
    if offset == 0 {
        return Vec::new();
    }
    let Some(slice) = sub.get(offset..) else {
        return Vec::new();
    };
    if slice.len() < 6 {
        return Vec::new();
    }
    let fmt = u16::from_be_bytes([slice[0], slice[1]]);
    let len = match fmt {
        1 => 6,
        2 => 8,
        3 => 10,
        _ => return Vec::new(),
    };
    if slice.len() < len {
        return Vec::new();
    }
    slice[..len].to_vec()
}

fn emit_cursive(surviving: &[(u16, Vec<u8>, Vec<u8>)]) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
    out.extend_from_slice(&(surviving.len() as u16).to_be_bytes());
    let records_start = out.len();
    for _ in 0..surviving.len() {
        out.extend_from_slice(&[0u8; 4]); // entry/exit offset placeholders
    }
    // Anchor bodies, then Coverage.
    for (i, (_g, entry, exit)) in surviving.iter().enumerate() {
        let entry_off: u16 = if entry.is_empty() {
            0
        } else {
            let pos = out.len() as u16;
            out.extend_from_slice(entry);
            pos
        };
        let exit_off: u16 = if exit.is_empty() {
            0
        } else {
            let pos = out.len() as u16;
            out.extend_from_slice(exit);
            pos
        };
        let rec = records_start + i * 4;
        out[rec..rec + 2].copy_from_slice(&entry_off.to_be_bytes());
        out[rec + 2..rec + 4].copy_from_slice(&exit_off.to_be_bytes());
    }
    let cov_off = out.len() as u16;
    let gids: Vec<u16> = surviving.iter().map(|(g, _, _)| *g).collect();
    let cov_bytes = crate::coverage::emit_coverage_from_glyphs(&gids);
    out.extend_from_slice(&cov_bytes);
    out[cov_slot..cov_slot + 2].copy_from_slice(&cov_off.to_be_bytes());
    RewrittenSubtable { bytes: out }
}

// ---------------------------------------------------------------------------
// Type 4 / 5 / 6 — Mark Attachment (Mark-to-Base, Mark-to-Liga, Mark-to-Mark)
// ---------------------------------------------------------------------------

/// Rewrites a mark-attachment subtable. The three lookup types share
/// the same 12-byte header shape:
///
/// ```text
///   u16      posFormat = 1
///   Offset16 markCoverageOffset       (== mark1 for mark-to-mark)
///   Offset16 baseCoverageOffset       (== ligature / mark2 for 5/6)
///   u16      markClassCount
///   Offset16 markArrayOffset
///   Offset16 baseArrayOffset          (== ligatureArray / mark2Array)
/// ```
///
/// `MarkArray` (at `markArrayOffset`):
///
/// ```text
///   u16 markCount
///   MarkRecord records[markCount]:
///     u16      markClass
///     Offset16 markAnchorOffset       (relative to MarkArray)
/// ```
///
/// For type 4 / type 6 the second array (`BaseArray` / `Mark2Array`):
///
/// ```text
///   u16 baseCount
///   BaseRecord records[baseCount]:
///     Offset16 baseAnchorOffsets[markClassCount]   (relative to array;
///                                                   0 = no anchor)
/// ```
///
/// For type 5 (Mark-to-Liga) the second array is a `LigatureArray`:
///
/// ```text
///   u16      ligatureCount
///   Offset16 ligatureAttachOffsets[ligatureCount]   (relative to array)
///
///   LigatureAttach (at each offset):
///     u16 componentCount
///     ComponentRecord records[componentCount]:
///       Offset16 ligatureAnchorOffsets[markClassCount]   (relative to
///                                                         LigatureAttach;
///                                                         0 = no anchor)
/// ```
///
/// We dispatch on the lookup type via [`MarkAttachKind`] passed in by
/// the caller — auto-detecting layout from the bytes alone is fragile
/// because for `markClassCount == 1` a LigatureArray's attach offset
/// can coincide with a valid type-4 anchor offset.
fn rewrite_mark_attach(
    ctx: &RewriterCtx,
    sub: &[u8],
    kind: MarkAttachKind,
) -> Option<RewrittenSubtable> {
    if sub.len() < 12 {
        return None;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    if format != 1 {
        return None;
    }
    let mark_cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let base_cov_off = u16::from_be_bytes([sub[4], sub[5]]) as usize;
    let mark_class_count = u16::from_be_bytes([sub[6], sub[7]]);
    let mark_array_off = u16::from_be_bytes([sub[8], sub[9]]) as usize;
    let base_array_off = u16::from_be_bytes([sub[10], sub[11]]) as usize;

    let mark_cov_bytes = sub.get(mark_cov_off..)?;
    let base_cov_bytes = sub.get(base_cov_off..)?;
    let mark_glyphs = parse_coverage_glyphs(mark_cov_bytes);
    let base_glyphs = parse_coverage_glyphs(base_cov_bytes);
    let map = ctx.gid_map;

    // Filter MarkArray entries by surviving mark glyphs. We need the
    // raw (markClass, anchorBody) for each survivor.
    let mark_array_bytes = sub.get(mark_array_off..)?;
    if mark_array_bytes.len() < 2 {
        return None;
    }
    let mark_count = u16::from_be_bytes([mark_array_bytes[0], mark_array_bytes[1]]) as usize;
    if mark_array_bytes.len() < 2 + mark_count * 4 {
        return None;
    }
    let mark_pair = mark_glyphs.len().min(mark_count);

    // Surviving mark records: (new_mark_gid, mark_class, anchor_bytes).
    let mut surviving_marks: Vec<(u16, u16, Vec<u8>)> = Vec::new();
    for (i, &g_old) in mark_glyphs.iter().enumerate().take(mark_pair) {
        let Some(g_new) = map.map(g_old) else {
            continue;
        };
        let rec_off = 2 + i * 4;
        let mark_class =
            u16::from_be_bytes([mark_array_bytes[rec_off], mark_array_bytes[rec_off + 1]]);
        let anchor_off_rel =
            u16::from_be_bytes([mark_array_bytes[rec_off + 2], mark_array_bytes[rec_off + 3]])
                as usize;
        // Anchor offset is relative to MarkArray base.
        let anchor_bytes = read_anchor_bytes(mark_array_bytes, anchor_off_rel);
        if anchor_bytes.is_empty() {
            // Marks always have anchors — a null offset would be a
            // malformed font. Skip.
            continue;
        }
        surviving_marks.push((g_new, mark_class, anchor_bytes));
    }
    if surviving_marks.is_empty() {
        return None;
    }

    // Filter base/ligature/mark2 array entries by surviving base
    // glyphs. The shape depends on whether each entry has a fixed
    // class-anchor row (type 4 / 6) or a variable-component attach
    // (type 5). We auto-detect by parsing the first 2 bytes as `count`
    // and checking whether the array length matches the type-4 layout
    // (count × markClassCount × 2 + 2 bytes header).
    let base_array_bytes = sub.get(base_array_off..)?;
    if base_array_bytes.len() < 2 {
        return None;
    }
    let base_count = u16::from_be_bytes([base_array_bytes[0], base_array_bytes[1]]) as usize;
    let base_pair = base_glyphs.len().min(base_count);
    let mcc = mark_class_count as usize;
    let is_type4_or_6 = matches!(kind, MarkAttachKind::FixedClassRow);

    let surviving_bases: Vec<SurvivingBase> = if is_type4_or_6 {
        // Per-base array of class-anchored anchors. We collect for
        // each surviving entry its (new_gid, marker_for_type4, anchor
        // body slots) — the marker is empty Vec, the anchor slots are
        // a list of mcc anchor-byte vectors (empty = null).
        let mut out: Vec<SurvivingBase> = Vec::new();
        for (i, &g_old) in base_glyphs.iter().enumerate().take(base_pair) {
            let Some(g_new) = map.map(g_old) else {
                continue;
            };
            let row_off = 2 + i * mcc * 2;
            let mut anchors: Vec<Vec<u8>> = Vec::with_capacity(mcc);
            for c in 0..mcc {
                let slot = row_off + c * 2;
                let anchor_off_rel =
                    u16::from_be_bytes([base_array_bytes[slot], base_array_bytes[slot + 1]])
                        as usize;
                anchors.push(read_anchor_bytes(base_array_bytes, anchor_off_rel));
            }
            out.push((g_new, Vec::new(), anchors));
        }
        out
    } else {
        // Type 5 (Mark-to-Liga) — ligatureArray with attach bodies.
        // For each surviving ligature, capture the LigatureAttach body
        // and re-emit it (we need to remap nothing inside, since
        // anchor offsets are relative to the LigatureAttach itself
        // and travel verbatim). Survivor stored as
        // (new_gid, attach_body_bytes, []).
        let mut out: Vec<SurvivingBase> = Vec::new();
        for (i, &g_old) in base_glyphs.iter().enumerate().take(base_pair) {
            let Some(g_new) = map.map(g_old) else {
                continue;
            };
            let off_slot = 2 + i * 2;
            if base_array_bytes.len() < off_slot + 2 {
                continue;
            }
            let attach_off_rel =
                u16::from_be_bytes([base_array_bytes[off_slot], base_array_bytes[off_slot + 1]])
                    as usize;
            let Some(attach_bytes) = base_array_bytes.get(attach_off_rel..) else {
                continue;
            };
            // Compute the attach body length: u16 componentCount +
            // componentCount × mcc × 2 anchor offsets + the anchor
            // bodies.
            if attach_bytes.len() < 2 {
                continue;
            }
            let component_count = u16::from_be_bytes([attach_bytes[0], attach_bytes[1]]) as usize;
            let comp_records_size = component_count * mcc * 2;
            let comp_records_end = 2 + comp_records_size;
            if attach_bytes.len() < comp_records_end {
                continue;
            }
            // Re-emit the attach body with anchor offsets re-threaded
            // (they're attach-relative). Each surviving anchor body is
            // copied verbatim.
            let mut new_attach = Vec::new();
            new_attach.extend_from_slice(&(component_count as u16).to_be_bytes());
            let new_records_start = new_attach.len();
            for _ in 0..component_count * mcc {
                new_attach.extend_from_slice(&[0u8; 2]);
            }
            for ci in 0..component_count {
                for c in 0..mcc {
                    let slot = 2 + (ci * mcc + c) * 2;
                    let anchor_off_rel =
                        u16::from_be_bytes([attach_bytes[slot], attach_bytes[slot + 1]]) as usize;
                    let anchor_bytes = read_anchor_bytes(attach_bytes, anchor_off_rel);
                    let new_slot = new_records_start + (ci * mcc + c) * 2;
                    if anchor_bytes.is_empty() {
                        // null offset.
                    } else {
                        let new_off = new_attach.len() as u16;
                        new_attach.extend_from_slice(&anchor_bytes);
                        new_attach[new_slot..new_slot + 2].copy_from_slice(&new_off.to_be_bytes());
                    }
                }
            }
            out.push((g_new, new_attach, Vec::new()));
        }
        out
    };

    if surviving_bases.is_empty() {
        return None;
    }

    Some(emit_mark_attach(
        mark_class_count,
        &surviving_marks,
        &surviving_bases,
        is_type4_or_6,
    ))
}

fn emit_mark_attach(
    mark_class_count: u16,
    surviving_marks: &[(u16, u16, Vec<u8>)],
    surviving_bases: &[SurvivingBase],
    is_type4_or_6: bool,
) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    let mark_cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // markCoverageOffset placeholder
    let base_cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // baseCoverageOffset placeholder
    out.extend_from_slice(&mark_class_count.to_be_bytes());
    let mark_array_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // markArrayOffset placeholder
    let base_array_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // baseArrayOffset placeholder

    // MarkArray.
    let mark_array_start = out.len();
    out.extend_from_slice(&(surviving_marks.len() as u16).to_be_bytes());
    let mark_records_start = out.len();
    for _ in 0..surviving_marks.len() {
        out.extend_from_slice(&[0u8; 4]); // markClass + anchorOffset placeholder
    }
    for (i, (_g, mark_class, anchor_bytes)) in surviving_marks.iter().enumerate() {
        let anchor_off = (out.len() - mark_array_start) as u16;
        out.extend_from_slice(anchor_bytes);
        let rec = mark_records_start + i * 4;
        out[rec..rec + 2].copy_from_slice(&mark_class.to_be_bytes());
        out[rec + 2..rec + 4].copy_from_slice(&anchor_off.to_be_bytes());
    }

    // Base/Liga/Mark2 array.
    let base_array_start = out.len();
    out.extend_from_slice(&(surviving_bases.len() as u16).to_be_bytes());
    let mcc = mark_class_count as usize;

    if is_type4_or_6 {
        // Per-base records: mcc anchor offsets each.
        let records_start = out.len();
        for _ in 0..surviving_bases.len() {
            for _ in 0..mcc {
                out.extend_from_slice(&[0u8; 2]);
            }
        }
        for (i, (_g, _attach, anchors)) in surviving_bases.iter().enumerate() {
            for (c, anchor) in anchors.iter().enumerate() {
                if anchor.is_empty() {
                    continue;
                }
                let off = (out.len() - base_array_start) as u16;
                out.extend_from_slice(anchor);
                let slot = records_start + i * mcc * 2 + c * 2;
                out[slot..slot + 2].copy_from_slice(&off.to_be_bytes());
            }
        }
    } else {
        // LigatureArray: one Offset16 per ligature, each pointing at
        // an already-rewritten LigatureAttach body.
        let attach_offsets_start = out.len();
        for _ in 0..surviving_bases.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        for (i, (_g, attach_body, _)) in surviving_bases.iter().enumerate() {
            let attach_off = (out.len() - base_array_start) as u16;
            out.extend_from_slice(attach_body);
            let slot = attach_offsets_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&attach_off.to_be_bytes());
        }
    }

    // Coverages last.
    let mark_cov_off = out.len() as u16;
    let mark_gids: Vec<u16> = surviving_marks.iter().map(|(g, _, _)| *g).collect();
    let mark_cov_bytes = crate::coverage::emit_coverage_from_glyphs(&mark_gids);
    out.extend_from_slice(&mark_cov_bytes);

    let base_cov_off = out.len() as u16;
    let base_gids: Vec<u16> = surviving_bases.iter().map(|(g, _, _)| *g).collect();
    let base_cov_bytes = crate::coverage::emit_coverage_from_glyphs(&base_gids);
    out.extend_from_slice(&base_cov_bytes);

    out[mark_cov_slot..mark_cov_slot + 2].copy_from_slice(&mark_cov_off.to_be_bytes());
    out[base_cov_slot..base_cov_slot + 2].copy_from_slice(&base_cov_off.to_be_bytes());
    out[mark_array_slot..mark_array_slot + 2]
        .copy_from_slice(&(mark_array_start as u16).to_be_bytes());
    out[base_array_slot..base_array_slot + 2]
        .copy_from_slice(&(base_array_start as u16).to_be_bytes());

    RewrittenSubtable { bytes: out }
}

// ---------------------------------------------------------------------------
// Type 9 — Extension Positioning
// ---------------------------------------------------------------------------

/// Rewrites a GPOS Extension subtable. Wrapper layout:
///
/// ```text
///   u16 format = 1
///   u16 extensionLookupType
///   u32 extensionOffset           (relative to extension subtable)
/// ```
///
/// Recurses into the inner subtable using the wrapped lookup type, then
/// re-emits a fresh Extension wrapper around the rewritten inner bytes.
fn rewrite_extension(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 8 {
        return None;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    if format != 1 {
        return None;
    }
    let inner_type = u16::from_be_bytes([sub[2], sub[3]]);
    let inner_off = u32::from_be_bytes([sub[4], sub[5], sub[6], sub[7]]) as usize;
    if inner_type == gpos_type::EXTENSION {
        // Spec disallows Extension referring to Extension.
        return None;
    }
    let inner = sub.get(inner_off..)?;
    let rewritten_inner = rewrite_subtable(ctx, inner_type, inner)?;
    // Re-wrap: inner sits at offset 8 in the new wrapper.
    let mut out = Vec::with_capacity(8 + rewritten_inner.bytes.len());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&inner_type.to_be_bytes());
    out.extend_from_slice(&8u32.to_be_bytes());
    out.extend_from_slice(&rewritten_inner.bytes);
    Some(RewrittenSubtable { bytes: out })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::GidMap;
    use sigilbuzz::tables::gpos::value_record::{X_ADVANCE, X_PLACEMENT};
    use sigilbuzz::tables::gpos::{MarkBasePos, MarkLigaPos, MarkMarkPos, PairPos, SinglePos};

    fn build_coverage_format1(glyphs: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
        for g in glyphs {
            out.extend_from_slice(&g.to_be_bytes());
        }
        out
    }

    fn map_from_pairs(pairs: &[(u16, u16)]) -> GidMap {
        let max_old = pairs.iter().map(|(o, _)| *o).max().unwrap_or(0);
        let mut table = vec![None; (max_old as usize + 1).max(1)];
        for &(old, new) in pairs {
            table[old as usize] = Some(new);
        }
        GidMap::from_table(table)
    }

    // ----- Type 1 — Single Adjustment -----

    fn build_single_adj_format1(covered: &[u16], value_format: u16, fields: &[i16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
        let cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
        out.extend_from_slice(&value_format.to_be_bytes());
        for f in fields {
            out.extend_from_slice(&f.to_be_bytes());
        }
        let cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(covered));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    fn build_single_adj_format2(covered: &[u16], value_format: u16, records: &[&[i16]]) -> Vec<u8> {
        assert_eq!(covered.len(), records.len());
        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes()); // posFormat
        let cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
        out.extend_from_slice(&value_format.to_be_bytes());
        out.extend_from_slice(&(records.len() as u16).to_be_bytes()); // valueCount
        for rec in records {
            for f in *rec {
                out.extend_from_slice(&f.to_be_bytes());
            }
        }
        let cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(covered));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    #[test]
    fn rewrite_single_adj_format1_remaps_coverage() {
        // Covered: 10, 20, 30. Map 10→1, 20→2, drop 30. Shared
        // x_advance = -25 should still apply.
        let bytes = build_single_adj_format1(&[10, 20, 30], X_ADVANCE, &[-25]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_single_adj(&ctx, &bytes).unwrap();
        let parsed = SinglePos::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.adjustment(1).unwrap().x_advance, -25);
        assert_eq!(parsed.adjustment(2).unwrap().x_advance, -25);
        assert!(parsed.adjustment(3).is_none()); // 30 dropped
    }

    #[test]
    fn rewrite_single_adj_format2_drops_corresponding_value() {
        // Three glyphs with per-glyph deltas. Drop the middle one.
        let bytes = build_single_adj_format2(&[10, 20, 30], X_ADVANCE, &[&[-5], &[-10], &[-15]]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (30, 3)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_single_adj(&ctx, &bytes).unwrap();
        let parsed = SinglePos::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.adjustment(1).unwrap().x_advance, -5);
        assert_eq!(parsed.adjustment(3).unwrap().x_advance, -15);
        // gid 2 not covered (was old 20, dropped).
        assert!(parsed.adjustment(2).is_none());
    }

    #[test]
    fn rewrite_single_adj_returns_none_when_all_dropped() {
        let bytes = build_single_adj_format1(&[10, 20], X_ADVANCE, &[-5]);
        let map = map_from_pairs(&[(0, 0)]);
        let ctx = RewriterCtx { gid_map: &map };
        assert!(rewrite_single_adj(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_single_adj_format1_preserves_value_record_with_multiple_fields() {
        // value_format = X_PLACEMENT | X_ADVANCE → two i16 fields.
        let bytes = build_single_adj_format1(&[5], X_PLACEMENT | X_ADVANCE, &[4, -10]);
        let map = map_from_pairs(&[(0, 0), (5, 1)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_single_adj(&ctx, &bytes).unwrap();
        let parsed = SinglePos::parse(&rs.bytes).unwrap();
        let v = parsed.adjustment(1).unwrap();
        assert_eq!(v.x_placement, 4);
        assert_eq!(v.x_advance, -10);
    }

    // ----- Type 2 — Pair Adjustment, format 1 -----

    fn build_pair_pos_format1(covered: &[u16], pairs: &[&[(u16, i16, i16)]]) -> Vec<u8> {
        assert_eq!(covered.len(), pairs.len());
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
        let cov_off_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&X_ADVANCE.to_be_bytes()); // valueFormat1
        out.extend_from_slice(&X_ADVANCE.to_be_bytes()); // valueFormat2
        out.extend_from_slice(&(pairs.len() as u16).to_be_bytes()); // pairSetCount
        let pair_set_offsets_start = out.len();
        for _ in 0..pairs.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        for (i, entries) in pairs.iter().enumerate() {
            let set_start = out.len();
            out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
            for (second, v1, v2) in *entries {
                out.extend_from_slice(&second.to_be_bytes());
                out.extend_from_slice(&v1.to_be_bytes());
                out.extend_from_slice(&v2.to_be_bytes());
            }
            let slot = pair_set_offsets_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&(set_start as u16).to_be_bytes());
        }
        let cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(covered));
        out[cov_off_slot..cov_off_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    #[test]
    fn rewrite_pair_pos_format1_keeps_surviving_pairs() {
        // first 10 → second {15, 25}; first 20 → second {5}.
        // Map: 10→1, 15→2, 20→3, drop 5, drop 25.
        let bytes =
            build_pair_pos_format1(&[10, 20], &[&[(15, -30, 0), (25, 5, 0)], &[(5, -50, 0)]]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (15, 2), (20, 3)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_pair_pos_format1(&ctx, &bytes).unwrap();
        let pp = PairPos::parse(&rs.bytes).unwrap();
        // (1, 2) → -30 survives; (1, 25)/(3, 5) drop.
        let (v1, _) = pp.lookup(1, 2).unwrap();
        assert_eq!(v1.x_advance, -30);
        // First 3 (was 20) had only second 5 which dropped; that
        // PairSet should be gone, so first 3 is not in the new
        // coverage.
        assert!(pp.lookup(3, 5).is_none());
    }

    #[test]
    fn rewrite_pair_pos_format1_returns_none_when_all_drop() {
        let bytes = build_pair_pos_format1(&[10], &[&[(15, -30, 0)]]);
        let map = map_from_pairs(&[(0, 0)]);
        let ctx = RewriterCtx { gid_map: &map };
        assert!(rewrite_pair_pos_format1(&ctx, &bytes).is_none());
    }

    // ----- Type 2 — Pair Adjustment, format 2 -----

    fn build_classdef_format1(start: u16, values: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&start.to_be_bytes());
        out.extend_from_slice(&(values.len() as u16).to_be_bytes());
        for v in values {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    fn build_pair_pos_format2(
        covered: &[u16],
        cd1: &[u8],
        cd2: &[u8],
        matrix: &[&[i16]],
    ) -> Vec<u8> {
        let class1_count = matrix.len() as u16;
        let class2_count = matrix.first().map_or(0, |row| row.len()) as u16;
        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes()); // posFormat
        let cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset
        out.extend_from_slice(&X_ADVANCE.to_be_bytes()); // valueFormat1
        out.extend_from_slice(&0u16.to_be_bytes()); // valueFormat2
        let cd1_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // classDef1Offset
        let cd2_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // classDef2Offset
        out.extend_from_slice(&class1_count.to_be_bytes());
        out.extend_from_slice(&class2_count.to_be_bytes());
        for row in matrix {
            for cell in *row {
                out.extend_from_slice(&cell.to_be_bytes());
            }
        }
        let cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(covered));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        let cd1_start = out.len();
        out.extend_from_slice(cd1);
        out[cd1_slot..cd1_slot + 2].copy_from_slice(&(cd1_start as u16).to_be_bytes());
        let cd2_start = out.len();
        out.extend_from_slice(cd2);
        out[cd2_slot..cd2_slot + 2].copy_from_slice(&(cd2_start as u16).to_be_bytes());
        out
    }

    #[test]
    fn rewrite_pair_pos_format2_pass_through_when_class_structure_preserved() {
        // Coverage: 10, 11. classDef1: both class 1. classDef2: 20→0, 21→1, 22→2.
        // Matrix 2×3: [[0,0,0], [0,-25,-15]].
        // Map every gid to itself but down by 1 (10→9, etc.). Class
        // structure is preserved (we keep all members of each class).
        let cd1 = build_classdef_format1(10, &[1, 1]);
        let cd2 = build_classdef_format1(20, &[0, 1, 2]);
        let matrix: &[&[i16]] = &[&[0, 0, 0], &[0, -25, -15]];
        let bytes = build_pair_pos_format2(&[10, 11], &cd1, &cd2, matrix);

        let map = map_from_pairs(&[(0, 0), (10, 9), (11, 10), (20, 19), (21, 20), (22, 21)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_pair_pos_format2(&ctx, &bytes).unwrap();
        let pp = PairPos::parse(&rs.bytes).unwrap();
        // (9, 20) → class1=1, class2=1 → -25.
        let (v1, _) = pp.lookup(9, 20).unwrap();
        assert_eq!(v1.x_advance, -25);
        // (10, 21) → class1=1, class2=2 → -15.
        let (v1b, _) = pp.lookup(10, 21).unwrap();
        assert_eq!(v1b.x_advance, -15);
    }

    // ----- Type 4 — Mark to Base -----

    fn build_anchor(x: i16, y: i16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format 1
        out.extend_from_slice(&x.to_be_bytes());
        out.extend_from_slice(&y.to_be_bytes());
        out
    }

    #[allow(clippy::type_complexity)]
    fn build_mark_base_pos(
        mark_glyphs: &[u16],
        base_glyphs: &[u16],
        mark_class_count: u16,
        marks: &[(u16, (i16, i16))],
        bases: &[Vec<Option<(i16, i16)>>],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
        let mark_cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        let base_cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&mark_class_count.to_be_bytes());
        let mark_array_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        let base_array_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());

        let mark_array_start = out.len();
        out.extend_from_slice(&(marks.len() as u16).to_be_bytes());
        let mark_records_start = out.len();
        for _ in 0..marks.len() {
            out.extend_from_slice(&[0u8; 4]);
        }
        for (i, (mark_class, (x, y))) in marks.iter().enumerate() {
            let anchor_start = out.len();
            out.extend_from_slice(&build_anchor(*x, *y));
            let rel = (anchor_start - mark_array_start) as u16;
            let rec = mark_records_start + i * 4;
            out[rec..rec + 2].copy_from_slice(&mark_class.to_be_bytes());
            out[rec + 2..rec + 4].copy_from_slice(&rel.to_be_bytes());
        }

        let base_array_start = out.len();
        out.extend_from_slice(&(bases.len() as u16).to_be_bytes());
        let base_records_start = out.len();
        for _ in 0..bases.len() {
            for _ in 0..mark_class_count {
                out.extend_from_slice(&[0u8; 2]);
            }
        }
        for (i, base_row) in bases.iter().enumerate() {
            for (c, slot) in base_row.iter().enumerate() {
                if let Some((x, y)) = slot {
                    let anchor_start = out.len();
                    out.extend_from_slice(&build_anchor(*x, *y));
                    let rel = (anchor_start - base_array_start) as u16;
                    let at = base_records_start + i * (mark_class_count as usize) * 2 + c * 2;
                    out[at..at + 2].copy_from_slice(&rel.to_be_bytes());
                }
            }
        }

        let mark_cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(mark_glyphs));
        let base_cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(base_glyphs));

        out[mark_cov_slot..mark_cov_slot + 2]
            .copy_from_slice(&(mark_cov_start as u16).to_be_bytes());
        out[base_cov_slot..base_cov_slot + 2]
            .copy_from_slice(&(base_cov_start as u16).to_be_bytes());
        out[mark_array_slot..mark_array_slot + 2]
            .copy_from_slice(&(mark_array_start as u16).to_be_bytes());
        out[base_array_slot..base_array_slot + 2]
            .copy_from_slice(&(base_array_start as u16).to_be_bytes());
        out
    }

    #[test]
    fn rewrite_mark_base_keeps_surviving_marks_and_bases() {
        // marks: 20 (class 0, anchor (10,0)), 21 (class 1, anchor (12,0))
        // bases: 5 with anchors (250,500), (260,600) for classes 0/1.
        // Map: 20→1, 21→2, 5→3.
        let bytes = build_mark_base_pos(
            &[20, 21],
            &[5],
            2,
            &[(0, (10, 0)), (1, (12, 0))],
            &[vec![Some((250, 500)), Some((260, 600))]],
        );
        let map = map_from_pairs(&[(0, 0), (5, 3), (20, 1), (21, 2)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_mark_attach(&ctx, &bytes, MarkAttachKind::FixedClassRow).unwrap();
        let mbp = MarkBasePos::parse(&rs.bytes).unwrap();
        let a0 = mbp.attach(1, 3).unwrap();
        let a1 = mbp.attach(2, 3).unwrap();
        assert_eq!(a0.mark_anchor.x, 10);
        assert_eq!(a0.base_anchor.y, 500);
        assert_eq!(a1.mark_anchor.x, 12);
        assert_eq!(a1.base_anchor.y, 600);
    }

    #[test]
    fn rewrite_mark_base_drops_when_marks_drop() {
        let bytes = build_mark_base_pos(&[20], &[5], 1, &[(0, (10, 0))], &[vec![Some((250, 500))]]);
        let map = map_from_pairs(&[(0, 0), (5, 1)]); // mark 20 dropped
        let ctx = RewriterCtx { gid_map: &map };
        assert!(rewrite_mark_attach(&ctx, &bytes, MarkAttachKind::FixedClassRow).is_none());
    }

    #[test]
    fn rewrite_mark_base_drops_when_bases_drop() {
        let bytes = build_mark_base_pos(&[20], &[5], 1, &[(0, (10, 0))], &[vec![Some((250, 500))]]);
        let map = map_from_pairs(&[(0, 0), (20, 1)]); // base 5 dropped
        let ctx = RewriterCtx { gid_map: &map };
        assert!(rewrite_mark_attach(&ctx, &bytes, MarkAttachKind::FixedClassRow).is_none());
    }

    // ----- Type 5 — Mark to Liga -----

    #[allow(clippy::type_complexity)]
    fn build_mark_liga_pos(
        mark_glyphs: &[u16],
        liga_glyphs: &[u16],
        mark_class_count: u16,
        marks: &[(u16, (i16, i16))],
        ligatures: &[Vec<Vec<Option<(i16, i16)>>>],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        let mark_cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        let liga_cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&mark_class_count.to_be_bytes());
        let mark_array_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        let liga_array_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());

        let mark_array_start = out.len();
        out.extend_from_slice(&(marks.len() as u16).to_be_bytes());
        let mark_records_start = out.len();
        for _ in 0..marks.len() {
            out.extend_from_slice(&[0u8; 4]);
        }
        for (i, (mark_class, (x, y))) in marks.iter().enumerate() {
            let anchor_start = out.len();
            out.extend_from_slice(&build_anchor(*x, *y));
            let rel = (anchor_start - mark_array_start) as u16;
            let rec = mark_records_start + i * 4;
            out[rec..rec + 2].copy_from_slice(&mark_class.to_be_bytes());
            out[rec + 2..rec + 4].copy_from_slice(&rel.to_be_bytes());
        }

        let liga_array_start = out.len();
        out.extend_from_slice(&(ligatures.len() as u16).to_be_bytes());
        let attach_slots_start = out.len();
        for _ in 0..ligatures.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        for (i, components) in ligatures.iter().enumerate() {
            let attach_start = out.len();
            out.extend_from_slice(&(components.len() as u16).to_be_bytes());
            let comp_records_start = out.len();
            for _ in 0..components.len() {
                for _ in 0..mark_class_count {
                    out.extend_from_slice(&[0u8; 2]);
                }
            }
            for (c_i, comp) in components.iter().enumerate() {
                for (cls, slot) in comp.iter().enumerate() {
                    if let Some((x, y)) = slot {
                        let anchor_start = out.len();
                        out.extend_from_slice(&build_anchor(*x, *y));
                        let rel = (anchor_start - attach_start) as u16;
                        let at =
                            comp_records_start + c_i * (mark_class_count as usize) * 2 + cls * 2;
                        out[at..at + 2].copy_from_slice(&rel.to_be_bytes());
                    }
                }
            }
            let rel = (attach_start - liga_array_start) as u16;
            let slot = attach_slots_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&rel.to_be_bytes());
        }

        let mark_cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(mark_glyphs));
        let liga_cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(liga_glyphs));

        out[mark_cov_slot..mark_cov_slot + 2]
            .copy_from_slice(&(mark_cov_start as u16).to_be_bytes());
        out[liga_cov_slot..liga_cov_slot + 2]
            .copy_from_slice(&(liga_cov_start as u16).to_be_bytes());
        out[mark_array_slot..mark_array_slot + 2]
            .copy_from_slice(&(mark_array_start as u16).to_be_bytes());
        out[liga_array_slot..liga_array_slot + 2]
            .copy_from_slice(&(liga_array_start as u16).to_be_bytes());
        out
    }

    #[test]
    fn rewrite_mark_liga_keeps_components_intact() {
        // One mark (gid 30, class 0), one ligature (gid 50) with two
        // components, each component has class-0 anchor.
        let bytes = build_mark_liga_pos(
            &[30],
            &[50],
            1,
            &[(0, (5, 0))],
            &[vec![vec![Some((100, 600))], vec![Some((400, 600))]]],
        );
        let map = map_from_pairs(&[(0, 0), (30, 1), (50, 2)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_mark_attach(&ctx, &bytes, MarkAttachKind::LigatureAttach).unwrap();
        let mlp = MarkLigaPos::parse(&rs.bytes).unwrap();
        let a0 = mlp.attach(1, 2, 0).unwrap();
        let a1 = mlp.attach(1, 2, 1).unwrap();
        assert_eq!(a0.base_anchor.x, 100);
        assert_eq!(a1.base_anchor.x, 400);
    }

    // ----- Type 6 — Mark to Mark -----

    #[test]
    fn rewrite_mark_mark_keeps_round_trip() {
        // Mark1 (gid 30, class 0), Mark2 (gid 5) with class-0 anchor.
        // Mark-to-mark uses the same shape as mark-to-base.
        let bytes = build_mark_base_pos(&[30], &[5], 1, &[(0, (5, 0))], &[vec![Some((100, 600))]]);
        let map = map_from_pairs(&[(0, 0), (5, 1), (30, 2)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_mark_attach(&ctx, &bytes, MarkAttachKind::FixedClassRow).unwrap();
        let mmp = MarkMarkPos::parse(&rs.bytes).unwrap();
        let attach = mmp.attach(2, 1).unwrap();
        assert_eq!(attach.base_anchor.x, 100);
        assert_eq!(attach.base_anchor.y, 600);
    }

    // ----- Type 9 — Extension wrapper around inner type 1 -----

    #[test]
    fn rewrite_extension_wraps_inner_single_adj() {
        let inner = build_single_adj_format1(&[10], X_ADVANCE, &[-25]);
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        out.extend_from_slice(&gpos_type::SINGLE_ADJUSTMENT.to_be_bytes());
        out.extend_from_slice(&8u32.to_be_bytes()); // inner offset = 8
        out.extend_from_slice(&inner);
        let map = map_from_pairs(&[(0, 0), (10, 1)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_extension(&ctx, &out).unwrap();
        // Verify the wrapper is preserved and inner parses.
        assert_eq!(&rs.bytes[0..2], &1u16.to_be_bytes());
        let inner_type = u16::from_be_bytes([rs.bytes[2], rs.bytes[3]]);
        assert_eq!(inner_type, gpos_type::SINGLE_ADJUSTMENT);
        let inner_off =
            u32::from_be_bytes([rs.bytes[4], rs.bytes[5], rs.bytes[6], rs.bytes[7]]) as usize;
        let parsed = SinglePos::parse(&rs.bytes[inner_off..]).unwrap();
        assert_eq!(parsed.adjustment(1).unwrap().x_advance, -25);
    }

    // ----- Type 3 — Cursive -----

    fn build_cursive(
        covered: &[u16],
        records: &[(Option<(i16, i16)>, Option<(i16, i16)>)],
    ) -> Vec<u8> {
        assert_eq!(covered.len(), records.len());
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
        let cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&(records.len() as u16).to_be_bytes()); // entryExitCount
        let records_start = out.len();
        for _ in 0..records.len() {
            out.extend_from_slice(&[0u8; 4]); // placeholders
        }
        for (i, (entry, exit)) in records.iter().enumerate() {
            let entry_off = if let Some((x, y)) = entry {
                let pos = out.len() as u16;
                out.extend_from_slice(&build_anchor(*x, *y));
                pos
            } else {
                0
            };
            let exit_off = if let Some((x, y)) = exit {
                let pos = out.len() as u16;
                out.extend_from_slice(&build_anchor(*x, *y));
                pos
            } else {
                0
            };
            let rec = records_start + i * 4;
            out[rec..rec + 2].copy_from_slice(&entry_off.to_be_bytes());
            out[rec + 2..rec + 4].copy_from_slice(&exit_off.to_be_bytes());
        }
        let cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(covered));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    #[test]
    fn rewrite_cursive_preserves_anchors() {
        // Two glyphs with entry/exit anchors.
        let bytes = build_cursive(
            &[10, 20],
            &[
                (Some((0, 0)), Some((100, 0))),
                (Some((0, 0)), Some((200, 0))),
            ],
        );
        let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_cursive(&ctx, &bytes).unwrap();
        // Verify it parses as a valid GPOS subtable header: format 1.
        assert_eq!(&rs.bytes[0..2], &1u16.to_be_bytes());
        let count = u16::from_be_bytes([rs.bytes[4], rs.bytes[5]]);
        assert_eq!(count, 2);
        // Verify Coverage at offset header has glyphs 1, 2.
        let cov_off = u16::from_be_bytes([rs.bytes[2], rs.bytes[3]]) as usize;
        let cov_glyphs = parse_coverage_glyphs(&rs.bytes[cov_off..]);
        assert_eq!(cov_glyphs, vec![1, 2]);
    }

    #[test]
    fn rewrite_cursive_drops_dropped_entries() {
        let bytes = build_cursive(
            &[10, 20],
            &[
                (Some((0, 0)), Some((100, 0))),
                (Some((0, 0)), Some((200, 0))),
            ],
        );
        let map = map_from_pairs(&[(0, 0), (10, 1)]); // 20 dropped
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_cursive(&ctx, &bytes).unwrap();
        let count = u16::from_be_bytes([rs.bytes[4], rs.bytes[5]]);
        assert_eq!(count, 1);
    }

    #[test]
    fn rewrite_cursive_returns_none_when_all_dropped() {
        let bytes = build_cursive(&[10], &[(Some((0, 0)), Some((100, 0)))]);
        let map = map_from_pairs(&[(0, 0)]);
        let ctx = RewriterCtx { gid_map: &map };
        assert!(rewrite_cursive(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_lookup_drops_unsupported_type() {
        let map = map_from_pairs(&[(0, 0), (10, 1)]);
        let ctx = RewriterCtx { gid_map: &map };
        // Lookup type 7 (context) drops.
        let dummy: Vec<&[u8]> = vec![&[0u8; 6]];
        assert!(rewrite_lookup(&ctx, gpos_type::CONTEXT, 0, None, &dummy).is_none());
    }
}
