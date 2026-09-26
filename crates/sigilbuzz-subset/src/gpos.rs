//! GPOS byte-level rewriter.
//!
//! Walks every lookup in the source `GPOS` table and produces a new
//! `GPOS` whose Coverage / ClassDef / glyph references resolve against
//! the new gid namespace defined by the caller's
//! [`GidMap`](crate::layout::GidMap). GPOS lookups don't introduce new
//! gids (they only reposition existing ones), so the rewriter is purely
//! a filter + remap pass over the per-type byte layout.
//!
//! # Per-lookup-type coverage
//!
//! The rewriter supports:
//!
//! - **Type 1 (single-adj)**: formats 1 (uniform ValueRecord) and 2
//!   (per-glyph ValueRecord array). Filters Coverage; for fmt 2 drops
//!   the corresponding ValueRecord slots in lockstep. The ValueRecord
//!   bytes themselves travel verbatim. They contain no gid references.
//! - **Type 2 (pair-adj) format 1**: explicit pair entries. Filters
//!   Coverage of the first glyph; for each surviving PairSet, walks
//!   PairValueRecords and drops pairs whose `secondGlyph` was dropped.
//!   Empty PairSets collapse to a Coverage drop. A subtable too big
//!   for its 16-bit offsets is split into runs of first glyphs (see
//!   [`pair_sets`]).
//! - **Type 2 (pair-adj) format 2**: class-based matrix. The rewriter
//!   has two strategies: a fast pass-through that preserves source
//!   class IDs and the matrix bytes verbatim (filtering both ClassDefs
//!   through the GidMap), and a fmt-1 fallback that synthesizes an
//!   explicit pair table from the kept-gid cross-product when class
//!   collapse would otherwise leave the matrix carrying rows/columns
//!   that no surviving gid can reach. The driver picks the fmt-1
//!   fallback for small subsets (<= 8 surviving first-glyphs or kept
//!   first x kept second cross <= 256 cells) and the pass-through path
//!   for larger ones.
//! - **Type 3 (cursive)**: Coverage + EntryExitRecord array. Filters
//!   Coverage; drops corresponding entry/exit slots. Anchors carry no
//!   gid references and are copied whole (identical anchors share one
//!   copy).
//! - **Type 4 / 5 / 6 (mark attachment)**: Mark+Base / Mark+Liga /
//!   Mark1+Mark2 Coverages with parallel MarkArray and BaseArray /
//!   LigatureArray / Mark2Array entries. Filters both Coverages, drops
//!   array entries in lockstep. Anchors and class IDs are copied whole.
//!   A subtable too big for its 16-bit offsets is split by mark class,
//!   and by base glyph within a class (see [`mark_attach`]).
//! - **Type 9 (extension)**: pass-through after rewriting the inner
//!   subtable. Falls back to a lookup drop when the inner type has no
//!   rewriter.
//!
//! - **Type 7 (context positioning)** and **type 8 (chained context
//!   positioning)**: formats 1 / 2 / 3. Their byte layout matches GSUB
//!   types 5 / 6, so they share the GSUB rewriters. Nested
//!   `PosLookupRecord`s are renumbered through the GPOS lookup-list
//!   renumber map driven by the two-pass build in
//!   [`crate::layout::build_gpos`]. Rules left without records
//!   (`ignore pos`) are kept, since they stop the later rules of their
//!   lookup from matching.
//!
//! Every rewriter charges the [`GidMap`] work budget for the records it
//! walks.
//!
//! [`GidMap`]: crate::layout::GidMap
//!
//! # Device and VariationIndex tables
//!
//! ValueRecords and AnchorFormat3 records reach `Device` /
//! `VariationIndex` tables through offsets measured from their parent
//! table (the subtable, the PairSet, or the Anchor). Every rebuilt
//! parent copies the tables its records reference and re-points the
//! offsets; see [`crate::device`].

mod mark_attach;
mod pair_sets;

use alloc::vec::Vec;

use sigilbuzz::tables::gpos::lookup_type as gpos_type;

use crate::coverage::emit_coverage_from_pairs;
use crate::device::{copy_anchor, Dedup};
use crate::layout::{extension_target, read_u16, RewriterCtx, RewrittenLookup, RewrittenSubtable};
use crate::warnings::error_context;
use crate::SubsetError;
use mark_attach::{rewrite_mark_attach, MarkAttachKind};
use pair_sets::{emit_pair_sets, PairSets};

/// Rewrites a single GPOS lookup. Returns `None` if the lookup has no
/// surviving subtables after rewriting (drop cascade will remove the
/// lookup), and an error when a rebuilt subtable outgrows its 16-bit
/// offsets even after splitting (see [`crate::offset16`]). A subtable
/// left out because it cannot be read is reported through `ctx.diag`.
pub(crate) fn rewrite_lookup(
    ctx: &RewriterCtx,
    lookup_type: u16,
    lookup_flag: u16,
    mark_filtering_set: Option<u16>,
    subtable_bodies: &[&[u8]],
) -> Result<Option<RewrittenLookup>, SubsetError> {
    let mut rewritten_subs: Vec<RewrittenSubtable> = Vec::new();

    for &sub_bytes in subtable_bodies {
        if !ctx.gid_map.spend(1) {
            return Ok(None);
        }
        let rewritten = rewrite_subtable(ctx, lookup_type, sub_bytes);
        ctx.offsets
            .check(overflow_context(lookup_type, sub_bytes))?;
        // Charge the output too, so shared offsets cannot multiply the
        // rewritten table past the budget.
        if !ctx
            .gid_map
            .spend(rewritten.iter().map(|rs| rs.bytes.len()).sum())
        {
            return Ok(None);
        }
        if rewritten.is_empty() && !ctx.gid_map.budget_spent() {
            report_unreadable(ctx, lookup_type, sub_bytes);
        }
        rewritten_subs.extend(rewritten);
    }

    if rewritten_subs.is_empty() {
        return Ok(None);
    }

    Ok(Some(RewrittenLookup {
        lookup_type,
        lookup_flag,
        mark_filtering_set,
        subtables: rewritten_subs,
    }))
}

/// Reports a subtable the rewrite left out when the shaper's own parser
/// rejects it as well: it went for being malformed, or for a lookup
/// type no shaper applies, not for losing every glyph. The parser
/// measures nested errors from the nested table, so the warning sits
/// at the start of the subtable, with the parser's reason.
fn report_unreadable(ctx: &RewriterCtx, lookup_type: u16, sub: &[u8]) {
    if let Err(e) = parse_subtable(lookup_type, sub) {
        ctx.diag
            .in_part(sub, 0, error_context(&e), "a lookup subtable");
    }
}

/// Parses `sub`, a subtable of a lookup of `lookup_type`, with the
/// shaper's parser, looking through an Extension wrapper.
fn parse_subtable(lookup_type: u16, sub: &[u8]) -> Result<(), sigilbuzz::Error> {
    use sigilbuzz::tables::gpos as parser;
    match lookup_type {
        gpos_type::SINGLE_ADJUSTMENT => parser::SinglePos::parse(sub).map(drop),
        gpos_type::PAIR_ADJUSTMENT => parser::PairPos::parse(sub).map(drop),
        gpos_type::CURSIVE_ATTACHMENT => parser::CursivePos::parse(sub).map(drop),
        gpos_type::MARK_TO_BASE => parser::MarkBasePos::parse(sub).map(drop),
        gpos_type::MARK_TO_LIGATURE => parser::MarkLigaPos::parse(sub).map(drop),
        gpos_type::MARK_TO_MARK => parser::MarkMarkPos::parse(sub).map(drop),
        gpos_type::CONTEXT => parser::ContextPos::parse(sub).map(drop),
        gpos_type::CHAINED_CONTEXT => parser::ChainContextPos::parse(sub).map(drop),
        gpos_type::EXTENSION => match extension_target(sub)? {
            (gpos_type::EXTENSION, _) => Err(sigilbuzz::Error::Malformed {
                offset: 2,
                context: "Extension subtable wraps another Extension",
            }),
            (inner_type, inner) => parse_subtable(inner_type, inner),
        },
        _ => Err(sigilbuzz::Error::Malformed {
            offset: 0,
            context: "unknown GPOS lookup type",
        }),
    }
}

/// Names the subtable type an Offset16 overflow is reported against,
/// looking through an Extension wrapper.
fn overflow_context(lookup_type: u16, sub: &[u8]) -> &'static str {
    let kind = match (lookup_type, sub.get(2..4)) {
        (gpos_type::EXTENSION, Some(inner)) => u16::from_be_bytes([inner[0], inner[1]]),
        _ => lookup_type,
    };
    match kind {
        gpos_type::SINGLE_ADJUSTMENT => "GPOS SinglePos rewrite: an offset exceeds 64 KiB",
        gpos_type::PAIR_ADJUSTMENT => "GPOS PairPos rewrite: an offset exceeds 64 KiB",
        gpos_type::CURSIVE_ATTACHMENT => "GPOS CursivePos rewrite: an offset exceeds 64 KiB",
        gpos_type::MARK_TO_BASE => "GPOS MarkBasePos rewrite: one mark class and base overflow",
        gpos_type::MARK_TO_LIGATURE => {
            "GPOS MarkLigPos rewrite: one mark class and ligature overflow"
        }
        gpos_type::MARK_TO_MARK => "GPOS MarkMarkPos rewrite: one mark class and mark overflow",
        gpos_type::CONTEXT => "GPOS ContextPos rewrite: an offset exceeds 64 KiB",
        _ => "GPOS ChainContextPos rewrite: an offset exceeds 64 KiB",
    }
}

/// Rewrites one subtable. Mark attachment and PairPos format 1
/// subtables may come back split into several; every other type
/// yields at most one.
fn rewrite_subtable(ctx: &RewriterCtx, lookup_type: u16, sub: &[u8]) -> Vec<RewrittenSubtable> {
    match lookup_type {
        gpos_type::SINGLE_ADJUSTMENT => rewrite_single_adj(ctx, sub).into_iter().collect(),
        gpos_type::PAIR_ADJUSTMENT => rewrite_pair_pos(ctx, sub),
        gpos_type::CURSIVE_ATTACHMENT => rewrite_cursive(ctx, sub).into_iter().collect(),
        gpos_type::MARK_TO_BASE | gpos_type::MARK_TO_MARK => {
            rewrite_mark_attach(ctx, sub, MarkAttachKind::FixedClassRow)
        }
        gpos_type::MARK_TO_LIGATURE => {
            rewrite_mark_attach(ctx, sub, MarkAttachKind::LigatureAttach)
        }
        gpos_type::CONTEXT => rewrite_context_pos(ctx, sub).into_iter().collect(),
        gpos_type::CHAINED_CONTEXT => rewrite_chain_context_pos(ctx, sub).into_iter().collect(),
        gpos_type::EXTENSION => rewrite_extension(ctx, sub),
        _ => Vec::new(),
    }
}

/// Returns the effective GPOS lookup type when the lookup is a
/// context-family type (7 or 8), including the case where it is
/// wrapped in an Extension lookup (type 9). Returns `None` otherwise.
///
/// Used by the GPOS driver to identify lookups whose nested
/// `PosLookupRecord` indices need to be patched once the lookup-list
/// renumber map is known. Mirrors GSUB's `context_lookup_type`.
pub(crate) fn context_lookup_type(lookup: &sigilbuzz::tables::layout::Lookup<'_>) -> Option<u16> {
    let lt = unwrap_extension_lookup_type(lookup);
    match lt {
        gpos_type::CONTEXT | gpos_type::CHAINED_CONTEXT => Some(lt),
        _ => None,
    }
}

/// Peels a single Extension wrapper to expose the inner lookup type.
/// Returns the lookup's own type when no Extension is present, the
/// inner extensionLookupType otherwise.
fn unwrap_extension_lookup_type(lookup: &sigilbuzz::tables::layout::Lookup<'_>) -> u16 {
    if lookup.lookup_type() != gpos_type::EXTENSION {
        return lookup.lookup_type();
    }
    lookup
        .subtable_bytes(0)
        .and_then(|sub| read_u16(sub, 2))
        .unwrap_or(lookup.lookup_type())
}

// ---------------------------------------------------------------------------
// Type 1: Single Adjustment
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
    let map = ctx.gid_map;
    let covered = map.coverage_glyphs(cov_bytes)?;
    let stride = value_record_size(value_format);

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
            let mut rs = emit_single_adj_format1(ctx, value_format, value_bytes, &surviving_gids);
            carry_devices(ctx, &mut rs.bytes, sub, 6, 1, stride, &[(0, value_format)]);
            Some(rs)
        }
        2 => {
            // Per-glyph ValueRecord array right after the header.
            let value_count = u16::from_be_bytes([*sub.get(6)?, *sub.get(7)?]) as usize;
            let values_off = 8usize;
            let need = values_off + value_count * stride;
            if sub.len() < need {
                return None;
            }
            // Spec requires Coverage entry count == valueCount; if the
            // source is malformed we cap.
            let pair_count = covered.len().min(value_count);
            if covered.len() > value_count {
                ctx.diag.in_part(
                    sub,
                    6,
                    "SinglePos format 2 Coverage lists more glyphs than valueCount",
                    "the adjustments of the extra glyphs",
                );
            }

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
            let mut rs = emit_single_adj_format2(ctx, value_format, &surviving);
            let count = surviving.len();
            carry_devices(
                ctx,
                &mut rs.bytes,
                sub,
                8,
                count,
                stride,
                &[(0, value_format)],
            );
            Some(rs)
        }
        _ => None,
    }
}

/// Copies the Device / VariationIndex tables referenced by the
/// ValueRecords that were copied verbatim into `out`, the rebuilt
/// parent table, and re-points their offsets. `src_parent` is the
/// source table the offsets are measured from: the subtable, or the
/// PairSet for PairPos format 1. The records sit in `count` groups of
/// `stride` bytes starting at `first`, laid out per `records`. See
/// [`crate::device::relocate_value_records`]. A copy the rebuilt parent
/// cannot address is recorded in [`RewriterCtx::offsets`].
fn carry_devices(
    ctx: &RewriterCtx,
    out: &mut Vec<u8>,
    src_parent: &[u8],
    first: usize,
    count: usize,
    stride: usize,
    records: &[(usize, u16)],
) {
    let run = crate::device::RecordRun {
        first,
        count,
        stride,
        records,
        keep_variations: ctx.keep_variations,
    };
    if !crate::device::relocate_value_records(out, 0, src_parent, &run, &ctx.diag) {
        ctx.offsets.record();
    }
}

fn emit_single_adj_format1(
    ctx: &RewriterCtx,
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
    let cov_off = ctx.off16(out.len());
    let cov_bytes = crate::coverage::emit_coverage_from_glyphs(surviving_gids);
    out.extend_from_slice(&cov_bytes);
    out[cov_slot..cov_slot + 2].copy_from_slice(&cov_off.to_be_bytes());
    RewrittenSubtable { bytes: out }
}

fn emit_single_adj_format2(
    ctx: &RewriterCtx,
    value_format: u16,
    surviving: &[(u16, Vec<u8>)],
) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes()); // posFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
    out.extend_from_slice(&value_format.to_be_bytes());
    out.extend_from_slice(&(surviving.len() as u16).to_be_bytes()); // valueCount
    for (_, body) in surviving {
        out.extend_from_slice(body);
    }
    let cov_off = ctx.off16(out.len());
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
// Type 2: Pair Adjustment
// ---------------------------------------------------------------------------

/// Rewrites a PairPos subtable. Dispatches on format. Format 1 output
/// (including the format 2 fallback) may be split into several
/// subtables, see [`pair_sets`].
fn rewrite_pair_pos(ctx: &RewriterCtx, sub: &[u8]) -> Vec<RewrittenSubtable> {
    match sub.get(..2) {
        Some([0, 1]) => rewrite_pair_pos_format1(ctx, sub),
        Some([0, 2]) => rewrite_pair_pos_format2(ctx, sub).unwrap_or_default(),
        _ => Vec::new(),
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
fn rewrite_pair_pos_format1(ctx: &RewriterCtx, sub: &[u8]) -> Vec<RewrittenSubtable> {
    match read_pair_pos_format1(ctx, sub) {
        Some((value_format1, value_format2, sets)) => {
            emit_pair_sets(ctx, value_format1, value_format2, sets)
        }
        None => Vec::new(),
    }
}

/// Reads the kept first glyphs of a PairPos format 1 subtable with
/// their rebuilt PairSets. `None` when nothing survives.
fn read_pair_pos_format1(ctx: &RewriterCtx, sub: &[u8]) -> Option<(u16, u16, PairSets)> {
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
    let covered = ctx.gid_map.coverage_glyphs(cov_bytes)?;
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
        if !map.spend(pair_value_count) {
            return None;
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
        let mut new_set = encode_pair_set(&survivors, v1_size, v2_size);
        // PairValueRecord device offsets are relative to the PairSet,
        // so the tables travel inside the rebuilt PairSet body.
        let formats = [(0, value_format1), (v1_size, value_format2)];
        carry_devices(
            ctx,
            &mut new_set,
            set_bytes,
            4,
            survivors.len(),
            pvr_size,
            &formats,
        );
        surviving_sets.push((first_new, new_set));
    }

    if surviving_sets.is_empty() {
        return None;
    }
    Some((value_format1, value_format2, surviving_sets))
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
/// Two strategies, picked by [`should_use_format1_fallback`]:
///
/// 1. **Fmt-2 pass-through** (the cheap path). When the kept-gid set
///    spans enough classes that synthesizing explicit pairs would be
///    bytes-heavy, we walk both ClassDefs and rewrite them through the
///    GidMap. Source class IDs are preserved verbatim: `emit_classdef`
///    keeps `(new_gid, original_class)` so matrix indices stay valid;
///    the matrix bytes travel verbatim.
/// 2. **Fmt-1 fallback** (the precise path). When the surviving first
///    x second cross-product is small, we enumerate every kept pair,
///    look up its `(class1, class2)` in the source ClassDefs, read the
///    source matrix cell, drop pairs that resolve to all-zero
///    ValueRecords, and emit a brand-new fmt-1 PairPos. This is the
///    safest answer when class collapse would otherwise leave the new
///    matrix carrying rows/columns that no surviving gid can reach.
fn rewrite_pair_pos_format2(ctx: &RewriterCtx, sub: &[u8]) -> Option<Vec<RewrittenSubtable>> {
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
    // The matrix can exceed a 32-bit `usize` on hostile counts, so the
    // size is computed with checked math.
    let matrix_len = usize::from(class1_count)
        .checked_mul(usize::from(class2_count))?
        .checked_mul(v_pair)?;
    let matrix = sub.get(records_off..records_off.checked_add(matrix_len)?)?;

    let map = ctx.gid_map;
    let cov_bytes = sub.get(cov_off..)?;
    let covered = map.coverage_glyphs(cov_bytes)?;
    let cd1_pairs = map.classdef_pairs_at(sub, cd1_off)?;
    let cd2_pairs = map.classdef_pairs_at(sub, cd2_off)?;
    if !map.spend(matrix.len()) {
        return None;
    }

    // Build kept (old, new) lists for both axes. The first-axis kept
    // set is Coverage ∩ kept-gids; the second-axis kept set spans every
    // gid the source's classDef2 mentions whose new gid survives.
    let mut surviving_first: Vec<(u16, u16)> = Vec::new(); // (old, new)
    for &g in &covered {
        if let Some(new) = map.map(g) {
            surviving_first.push((g, new));
        }
    }
    if surviving_first.is_empty() {
        return None;
    }
    let mut surviving_cov: Vec<u16> = surviving_first.iter().map(|&(_, n)| n).collect();
    surviving_cov.sort_unstable();
    surviving_cov.dedup();

    // Class-collapse fallback: when synthesizing an explicit fmt-1
    // table would be cheaper or strictly more correct (e.g. the
    // surviving first x second cross-product is small enough that
    // class-pair indirection no longer pays off), enumerate every
    // (first, second) pair from the kept sets, resolve its
    // (class1, class2) via the source ClassDefs, and read the source
    // matrix cell directly.
    if should_use_format1_fallback(&surviving_first, map) {
        let matrix = PairClassMatrix {
            cells: matrix,
            class1_count,
            class2_count,
            cell_size: v_pair,
        };
        return rewrite_pair_pos_format2_to_format1(
            ctx,
            sub,
            &surviving_first,
            &cd1_pairs,
            &cd2_pairs,
            value_format1,
            value_format2,
            &matrix,
        );
    }

    // Filter ClassDefs: keep (new_gid, class) pairs whose class is
    // still valid in the matrix (< classNCount). We do not renumber
    // classes. The matrix bytes are preserved verbatim.
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

    // Matrix bytes travel verbatim: neither ValueRecord field nor
    // class indices changed.
    let matrix_bytes = matrix.to_vec();

    let mut rs = emit_pair_pos_format2(
        ctx,
        value_format1,
        value_format2,
        class1_count,
        class2_count,
        &surviving_cov,
        &cd1_bytes_new,
        &cd2_bytes_new,
        &matrix_bytes,
    );
    let cells = class1_count as usize * class2_count as usize;
    let v1_size = value_record_size(value_format1);
    let formats = [(0, value_format1), (v1_size, value_format2)];
    carry_devices(
        ctx,
        &mut rs.bytes,
        sub,
        records_off,
        cells,
        v_pair,
        &formats,
    );
    Some(alloc::vec![rs])
}

#[allow(clippy::too_many_arguments)]
fn emit_pair_pos_format2(
    ctx: &RewriterCtx,
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

    let cov_start = ctx.off16(out.len());
    let cov_emitted = crate::coverage::emit_coverage_from_glyphs(surviving_cov);
    out.extend_from_slice(&cov_emitted);
    out[cov_slot..cov_slot + 2].copy_from_slice(&cov_start.to_be_bytes());

    let cd1_start = ctx.off16(out.len());
    out.extend_from_slice(cd1_bytes);
    out[cd1_slot..cd1_slot + 2].copy_from_slice(&cd1_start.to_be_bytes());

    let cd2_start = ctx.off16(out.len());
    out.extend_from_slice(cd2_bytes);
    out[cd2_slot..cd2_slot + 2].copy_from_slice(&cd2_start.to_be_bytes());

    RewrittenSubtable { bytes: out }
}

/// Heuristic: pick the fmt-1 fallback when the surviving first x second
/// cross-product is small enough that emitting an explicit pair table
/// is competitive with carrying the full class matrix verbatim.
///
/// The fallback fires when:
/// - Coverage shrunk to <= 8 first-glyphs (small kerning subsets, e.g.
///   the {A, V} case), or
/// - the kept first x kept second cross-product fits a 256-cell budget,
///   so the explicit pair table can't bloat past the source matrix.
///
/// Otherwise we use the cheap fmt-2 pass-through path. The matrix
/// indices in that path remain valid because [`emit_classdef`](crate::emit_classdef)
/// preserves source class IDs verbatim.
fn should_use_format1_fallback(
    surviving_first: &[(u16, u16)],
    map: &crate::layout::GidMap,
) -> bool {
    // Kept-set size approximates the second-glyph universe (every kept
    // gid is a candidate second glyph through classDef2's class-0
    // default, even if it isn't listed explicitly).
    let kept_count = map.kept_len();
    let cross = surviving_first.len() * kept_count.max(1);
    surviving_first.len() <= 8 || cross <= 256
}

/// One PairPos fmt-1 first-glyph set: `(new_first_gid, [(new_second_gid, value_pair_bytes)])`.
type PairPosFmt1Set = (u16, Vec<(u16, Vec<u8>)>);

/// The Class1Record matrix of a PairPos format 2 subtable.
struct PairClassMatrix<'a> {
    /// `class1_count * class2_count` cells of `cell_size` bytes each.
    cells: &'a [u8],
    class1_count: u16,
    class2_count: u16,
    /// Bytes per cell: both ValueRecords.
    cell_size: usize,
}

impl<'a> PairClassMatrix<'a> {
    /// Returns the raw value-pair bytes at `(c1, c2)`, or `None` when
    /// either class is out of range.
    fn cell(&self, c1: u16, c2: u16) -> Option<&'a [u8]> {
        if c1 >= self.class1_count || c2 >= self.class2_count {
            return None;
        }
        let index = usize::from(c1) * usize::from(self.class2_count) + usize::from(c2);
        let off = index.checked_mul(self.cell_size)?;
        self.cells.get(off..off.checked_add(self.cell_size)?)
    }
}

/// Sorts ClassDef `(gid, class)` pairs by gid for [`class_in`]. The
/// sort is stable, so when a malformed table lists a glyph twice the
/// entry that comes first in the table stays first.
fn sorted_by_gid(pairs: &[(u16, u16)]) -> Vec<(u16, u16)> {
    let mut sorted = pairs.to_vec();
    sorted.sort_by_key(|&(g, _)| g);
    sorted
}

/// Looks up the class of `gid` in `(gid, class)` pairs sorted by
/// [`sorted_by_gid`]. Unlisted glyphs are class 0.
fn class_in(sorted: &[(u16, u16)], gid: u16) -> u16 {
    let i = sorted.partition_point(|&(g, _)| g < gid);
    match sorted.get(i) {
        Some(&(g, class)) if g == gid => class,
        _ => 0,
    }
}

/// Synthesizes a fmt-1 PairPos around the kept-gid cross-product.
/// Walks every `(first_old, first_new) * (second_old)` and reads the
/// source matrix cell at `(class1, class2)`. Drops pairs whose source
/// cell is all-zero (no kerning to preserve). The lookup answer is
/// then equivalent to "not covered". The cross-product is charged to
/// the work budget up front.
#[allow(clippy::too_many_arguments)]
fn rewrite_pair_pos_format2_to_format1(
    ctx: &RewriterCtx,
    sub: &[u8],
    surviving_first: &[(u16, u16)],
    cd1_pairs: &[(u16, u16)],
    cd2_pairs: &[(u16, u16)],
    value_format1: u16,
    value_format2: u16,
    matrix: &PairClassMatrix<'_>,
) -> Option<Vec<RewrittenSubtable>> {
    let map = ctx.gid_map;
    let v1_size = value_record_size(value_format1);
    let v2_size = value_record_size(value_format2);

    // Enumerate the kept-gid universe as the candidate second-glyph
    // set. Classes 1..N appear in `cd2_pairs`, but class 0 (the
    // "everything else" bucket) carries any gid the source classDef2
    // doesn't list explicitly, and class-0 columns can still hold
    // non-zero kerning. Walking the GidMap directly catches that. Each
    // second glyph's class is resolved once, outside the pair loop.
    let cd1_sorted = sorted_by_gid(cd1_pairs);
    let cd2_sorted = sorted_by_gid(cd2_pairs);
    let kept_seconds: Vec<(u16, u16)> = map
        .iter_kept()
        .map(|(second_old, second_new)| (second_new, class_in(&cd2_sorted, second_old)))
        .collect();
    if !map.spend(surviving_first.len().saturating_mul(kept_seconds.len())) {
        return None;
    }

    // Build (first_new, [(second_new, value_pair_bytes), ...]).
    let mut out_sets: Vec<PairPosFmt1Set> = Vec::new();
    for &(first_old, first_new) in surviving_first {
        let c1 = class_in(&cd1_sorted, first_old);
        let mut entries: Vec<(u16, Vec<u8>)> = Vec::new();
        for &(second_new, c2) in &kept_seconds {
            let Some(cell) = matrix.cell(c1, c2) else {
                continue;
            };
            // Drop all-zero cells: no kerning to carry.
            if cell.iter().all(|b| *b == 0) {
                continue;
            }
            entries.push((second_new, cell.to_vec()));
        }
        if entries.is_empty() {
            continue;
        }
        entries.sort_by_key(|(g, _)| *g);
        entries.dedup_by_key(|(g, _)| *g);
        out_sets.push((first_new, entries));
    }

    if out_sets.is_empty() {
        return None;
    }
    out_sets.sort_by_key(|(g, _)| *g);
    out_sets.dedup_by_key(|(g, _)| *g);

    // Emit fmt-1 PairPos around `out_sets`.
    Some(emit_pair_pos_format1_from_sets(
        ctx,
        value_format1,
        value_format2,
        v1_size,
        v2_size,
        &out_sets,
        sub,
    ))
}

/// Emits a fmt-1 PairPos given pre-encoded value-pair bodies. Each
/// body is the concatenation of the source's two ValueRecord byte
/// sequences (which are gid-independent and travel verbatim). Their
/// device offsets are measured from `src_parent`, the source format 2
/// subtable; the tables are copied into each new PairSet, which is the
/// base format 1 measures them from.
fn emit_pair_pos_format1_from_sets(
    ctx: &RewriterCtx,
    value_format1: u16,
    value_format2: u16,
    v1_size: usize,
    v2_size: usize,
    sets: &[PairPosFmt1Set],
    src_parent: &[u8],
) -> Vec<RewrittenSubtable> {
    let formats = [(0, value_format1), (v1_size, value_format2)];
    // Re-shape sets into the encoder's expected
    // `(first_new, encoded_pair_set_body)` format.
    let mut surviving_sets: Vec<(u16, Vec<u8>)> = Vec::with_capacity(sets.len());
    for (first_new, entries) in sets {
        let mut set_body = Vec::with_capacity(2 + entries.len() * 6);
        set_body.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        for (second, body) in entries {
            set_body.extend_from_slice(&second.to_be_bytes());
            set_body.extend_from_slice(body);
        }
        let stride = 2 + v1_size + v2_size;
        carry_devices(
            ctx,
            &mut set_body,
            src_parent,
            4,
            entries.len(),
            stride,
            &formats,
        );
        surviving_sets.push((*first_new, set_body));
    }
    emit_pair_sets(ctx, value_format1, value_format2, surviving_sets)
}

// ---------------------------------------------------------------------------
// Type 3: Cursive Attachment
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
    let map = ctx.gid_map;
    let covered = map.coverage_glyphs(cov_bytes)?;
    let pair_count = covered.len().min(count);

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
        let entry_bytes = copy_anchor(sub, entry_off, ctx.keep_variations, &ctx.diag);
        let exit_bytes = copy_anchor(sub, exit_off, ctx.keep_variations, &ctx.diag);
        surviving.push((g_new, entry_bytes, exit_bytes));
    }
    if surviving.is_empty() {
        return None;
    }

    Some(emit_cursive(ctx, &surviving))
}

fn emit_cursive(ctx: &RewriterCtx, surviving: &[(u16, Vec<u8>, Vec<u8>)]) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
    out.extend_from_slice(&(surviving.len() as u16).to_be_bytes());
    let records_start = out.len();
    for _ in 0..surviving.len() {
        out.extend_from_slice(&[0u8; 4]); // entry/exit offset placeholders
    }
    // Coverage first, so the anchors after it are the only part that
    // can push an offset out of 16-bit reach.
    let cov_off = ctx.off16(out.len());
    out[2..4].copy_from_slice(&cov_off.to_be_bytes());
    let gids: Vec<u16> = surviving.iter().map(|(g, _, _)| *g).collect();
    out.extend_from_slice(&crate::coverage::emit_coverage_from_glyphs(&gids));
    // Anchor bodies; identical anchors share one copy.
    let mut anchors = Dedup::default();
    for (i, (_g, entry, exit)) in surviving.iter().enumerate() {
        let rec = records_start + i * 4;
        for (slot, anchor) in [(rec, entry), (rec + 2, exit)] {
            if !anchor.is_empty() {
                let at = ctx.off16(anchors.place(&mut out, anchor));
                out[slot..slot + 2].copy_from_slice(&at.to_be_bytes());
            }
        }
    }
    RewrittenSubtable { bytes: out }
}

// ---------------------------------------------------------------------------
// Type 9: Extension Positioning
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
/// re-emits a fresh Extension wrapper around each rewritten inner
/// subtable (a split inner subtable gets one wrapper per piece).
fn rewrite_extension(ctx: &RewriterCtx, sub: &[u8]) -> Vec<RewrittenSubtable> {
    let (Some(&[0, 1]), Some(kind), Some(off)) = (sub.get(..2), sub.get(2..4), sub.get(4..8))
    else {
        return Vec::new();
    };
    let inner_type = u16::from_be_bytes([kind[0], kind[1]]);
    let inner_off = u32::from_be_bytes([off[0], off[1], off[2], off[3]]) as usize;
    // Spec disallows Extension referring to Extension.
    let Some(inner) = sub
        .get(inner_off..)
        .filter(|_| inner_type != gpos_type::EXTENSION)
    else {
        return Vec::new();
    };
    rewrite_subtable(ctx, inner_type, inner)
        .into_iter()
        .map(|piece| {
            // Re-wrap: inner sits at offset 8 in the new wrapper.
            let mut out = Vec::with_capacity(8 + piece.bytes.len());
            out.extend_from_slice(&1u16.to_be_bytes());
            out.extend_from_slice(&inner_type.to_be_bytes());
            out.extend_from_slice(&8u32.to_be_bytes());
            out.extend_from_slice(&piece.bytes);
            RewrittenSubtable { bytes: out }
        })
        .collect()
}

// ===== GPOS types 7 / 8: contextual / chained-contextual positioning =====
//
// Structurally identical to GSUB types 5 / 6: the same three formats
// (glyph rule sets, class rule sets, coverage arrays) drive a list of
// `PosLookupRecord` entries that re-enter the GPOS dispatcher on a
// match. Each record is `u16 sequenceIndex, u16 lookupListIndex`; the
// second field is patched on the second pass through
// [`crate::layout::build_gpos`] once the GPOS lookup-list renumber is
// known. See [`context_lookup_type`] for the driver hook. Only the
// dispatcher target of the nested records differs, so the GSUB
// rewriters handle both tables.

/// Rewrites a GPOS type 7 (Context Positioning) subtable, formats 1 /
/// 2 / 3. Shares the GSUB type 5 rewriter.
fn rewrite_context_pos(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    crate::gsub::rewrite_type5(ctx, sub)
}

/// Rewrites a GPOS type 8 (Chained Context Positioning) subtable,
/// formats 1 / 2 / 3. Shares the GSUB type 6 rewriter.
fn rewrite_chain_context_pos(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    crate::gsub::rewrite_type6(ctx, sub)
}

#[cfg(test)]
mod device_tests;

#[cfg(test)]
mod split_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{parse_coverage_glyphs, GidMap};
    use alloc::vec;
    use sigilbuzz::tables::gpos::value_record::{X_ADVANCE, X_PLACEMENT};
    use sigilbuzz::tables::gpos::{MarkBasePos, MarkLigaPos, MarkMarkPos, PairPos, SinglePos};

    /// The single subtable a rewrite produced; the fixtures here are
    /// far too small to be split.
    fn one(mut pieces: Vec<RewrittenSubtable>) -> RewrittenSubtable {
        assert_eq!(pieces.len(), 1, "expected one subtable");
        pieces.remove(0)
    }

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

    // ----- Type 1: Single Adjustment -----

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
        // Covered: 10, 20, 30. Map 10->1, 20->2, drop 30. Shared
        // x_advance = -25 should still apply.
        let bytes = build_single_adj_format1(&[10, 20, 30], X_ADVANCE, &[-25]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2)]);
        let ctx = RewriterCtx::new(&map, None);
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
        let ctx = RewriterCtx::new(&map, None);
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
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_single_adj(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_single_adj_format1_preserves_value_record_with_multiple_fields() {
        // value_format = X_PLACEMENT | X_ADVANCE -> two i16 fields.
        let bytes = build_single_adj_format1(&[5], X_PLACEMENT | X_ADVANCE, &[4, -10]);
        let map = map_from_pairs(&[(0, 0), (5, 1)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_single_adj(&ctx, &bytes).unwrap();
        let parsed = SinglePos::parse(&rs.bytes).unwrap();
        let v = parsed.adjustment(1).unwrap();
        assert_eq!(v.x_placement, 4);
        assert_eq!(v.x_advance, -10);
    }

    // ----- Type 2: Pair Adjustment, format 1 -----

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
        // first 10 -> second {15, 25}; first 20 -> second {5}.
        // Map: 10->1, 15->2, 20->3, drop 5, drop 25.
        let bytes =
            build_pair_pos_format1(&[10, 20], &[&[(15, -30, 0), (25, 5, 0)], &[(5, -50, 0)]]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (15, 2), (20, 3)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = one(rewrite_pair_pos_format1(&ctx, &bytes));
        let pp = PairPos::parse(&rs.bytes).unwrap();
        // (1, 2) -> -30 survives; (1, 25)/(3, 5) drop.
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
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_pair_pos_format1(&ctx, &bytes).is_empty());
    }

    // ----- Type 2: Pair Adjustment, format 2 -----

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
        // Coverage: 10, 11. classDef1: both class 1. classDef2: 20->0, 21->1, 22->2.
        // Matrix 2x3: [[0,0,0], [0,-25,-15]].
        // Map every gid to itself but down by 1 (10->9, etc.). Class
        // structure is preserved (we keep all members of each class).
        let cd1 = build_classdef_format1(10, &[1, 1]);
        let cd2 = build_classdef_format1(20, &[0, 1, 2]);
        let matrix: &[&[i16]] = &[&[0, 0, 0], &[0, -25, -15]];
        let bytes = build_pair_pos_format2(&[10, 11], &cd1, &cd2, matrix);

        let map = map_from_pairs(&[(0, 0), (10, 9), (11, 10), (20, 19), (21, 20), (22, 21)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = one(rewrite_pair_pos_format2(&ctx, &bytes).unwrap());
        let pp = PairPos::parse(&rs.bytes).unwrap();
        // (9, 20) -> class1=1, class2=1 -> -25.
        let (v1, _) = pp.lookup(9, 20).unwrap();
        assert_eq!(v1.x_advance, -25);
        // (10, 21) -> class1=1, class2=2 -> -15.
        let (v1b, _) = pp.lookup(10, 21).unwrap();
        assert_eq!(v1b.x_advance, -15);
    }

    // ----- Type 4: Mark to Base -----

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
        // Map: 20->1, 21->2, 5->3.
        let bytes = build_mark_base_pos(
            &[20, 21],
            &[5],
            2,
            &[(0, (10, 0)), (1, (12, 0))],
            &[vec![Some((250, 500)), Some((260, 600))]],
        );
        let map = map_from_pairs(&[(0, 0), (5, 3), (20, 1), (21, 2)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = one(rewrite_mark_attach(
            &ctx,
            &bytes,
            MarkAttachKind::FixedClassRow,
        ));
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
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_mark_attach(&ctx, &bytes, MarkAttachKind::FixedClassRow).is_empty());
    }

    #[test]
    fn rewrite_mark_base_drops_when_bases_drop() {
        let bytes = build_mark_base_pos(&[20], &[5], 1, &[(0, (10, 0))], &[vec![Some((250, 500))]]);
        let map = map_from_pairs(&[(0, 0), (20, 1)]); // base 5 dropped
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_mark_attach(&ctx, &bytes, MarkAttachKind::FixedClassRow).is_empty());
    }

    // ----- Type 5: Mark to Liga -----

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
        let ctx = RewriterCtx::new(&map, None);
        let rs = one(rewrite_mark_attach(
            &ctx,
            &bytes,
            MarkAttachKind::LigatureAttach,
        ));
        let mlp = MarkLigaPos::parse(&rs.bytes).unwrap();
        let a0 = mlp.attach(1, 2, 0).unwrap();
        let a1 = mlp.attach(1, 2, 1).unwrap();
        assert_eq!(a0.base_anchor.x, 100);
        assert_eq!(a1.base_anchor.x, 400);
    }

    // ----- Type 6: Mark to Mark -----

    #[test]
    fn rewrite_mark_mark_keeps_round_trip() {
        // Mark1 (gid 30, class 0), Mark2 (gid 5) with class-0 anchor.
        // Mark-to-mark uses the same shape as mark-to-base.
        let bytes = build_mark_base_pos(&[30], &[5], 1, &[(0, (5, 0))], &[vec![Some((100, 600))]]);
        let map = map_from_pairs(&[(0, 0), (5, 1), (30, 2)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = one(rewrite_mark_attach(
            &ctx,
            &bytes,
            MarkAttachKind::FixedClassRow,
        ));
        let mmp = MarkMarkPos::parse(&rs.bytes).unwrap();
        let attach = mmp.attach(2, 1).unwrap();
        assert_eq!(attach.base_anchor.x, 100);
        assert_eq!(attach.base_anchor.y, 600);
    }

    // ----- Type 9: Extension wrapper around inner type 1 -----

    #[test]
    fn rewrite_extension_wraps_inner_single_adj() {
        let inner = build_single_adj_format1(&[10], X_ADVANCE, &[-25]);
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        out.extend_from_slice(&gpos_type::SINGLE_ADJUSTMENT.to_be_bytes());
        out.extend_from_slice(&8u32.to_be_bytes()); // inner offset = 8
        out.extend_from_slice(&inner);
        let map = map_from_pairs(&[(0, 0), (10, 1)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = one(rewrite_extension(&ctx, &out));
        // Verify the wrapper is preserved and inner parses.
        assert_eq!(&rs.bytes[0..2], &1u16.to_be_bytes());
        let inner_type = u16::from_be_bytes([rs.bytes[2], rs.bytes[3]]);
        assert_eq!(inner_type, gpos_type::SINGLE_ADJUSTMENT);
        let inner_off =
            u32::from_be_bytes([rs.bytes[4], rs.bytes[5], rs.bytes[6], rs.bytes[7]]) as usize;
        let parsed = SinglePos::parse(&rs.bytes[inner_off..]).unwrap();
        assert_eq!(parsed.adjustment(1).unwrap().x_advance, -25);
    }

    // ----- Type 3: Cursive -----

    type CursiveAnchor = Option<(i16, i16)>;

    fn build_cursive(covered: &[u16], records: &[(CursiveAnchor, CursiveAnchor)]) -> Vec<u8> {
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
        let ctx = RewriterCtx::new(&map, None);
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
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_cursive(&ctx, &bytes).unwrap();
        let count = u16::from_be_bytes([rs.bytes[4], rs.bytes[5]]);
        assert_eq!(count, 1);
    }

    #[test]
    fn rewrite_cursive_returns_none_when_all_dropped() {
        let bytes = build_cursive(&[10], &[(Some((0, 0)), Some((100, 0)))]);
        let map = map_from_pairs(&[(0, 0)]);
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_cursive(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_lookup_drops_malformed_context() {
        let map = map_from_pairs(&[(0, 0), (10, 1)]);
        let ctx = RewriterCtx::new(&map, None);
        // Format 0 inside a context subtable is unknown. It falls
        // out as None and the cascade drops the lookup.
        let dummy: Vec<&[u8]> = vec![&[0u8; 6]];
        assert!(rewrite_lookup(&ctx, gpos_type::CONTEXT, 0, None, &dummy)
            .unwrap()
            .is_none());
    }

    // ----- Type 7: Context Positioning -----

    fn build_pos_lookup_records(records: &[(u16, u16)]) -> Vec<u8> {
        let mut out = Vec::with_capacity(records.len() * 4);
        for (s, l) in records {
            out.extend_from_slice(&s.to_be_bytes());
            out.extend_from_slice(&l.to_be_bytes());
        }
        out
    }

    /// One PosRule's `(input_tail_gids, lookup_records)` pair.
    type ContextPosRule = (Vec<u16>, Vec<(u16, u16)>);

    /// Builds a fmt-1 context-positioning subtable around one
    /// PosRuleSet per Coverage entry. Each rule lists the input tail
    /// gids + a single PosLookupRecord.
    fn build_context_pos_format1(
        covered: &[u16],
        rules_per_set: &[Vec<ContextPosRule>],
    ) -> Vec<u8> {
        assert_eq!(covered.len(), rules_per_set.len());
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
        let cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&(rules_per_set.len() as u16).to_be_bytes());
        let set_offsets_start = out.len();
        for _ in 0..rules_per_set.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        for (i, rules) in rules_per_set.iter().enumerate() {
            let set_start = out.len();
            // RuleSet: u16 ruleCount + Offset16[count] + bodies.
            out.extend_from_slice(&(rules.len() as u16).to_be_bytes());
            let rule_off_start = out.len();
            for _ in 0..rules.len() {
                out.extend_from_slice(&[0u8; 2]);
            }
            for (ri, (input_tail, recs)) in rules.iter().enumerate() {
                let body_start = out.len();
                let glyph_count = (input_tail.len() + 1) as u16;
                out.extend_from_slice(&glyph_count.to_be_bytes());
                out.extend_from_slice(&(recs.len() as u16).to_be_bytes());
                for g in input_tail {
                    out.extend_from_slice(&g.to_be_bytes());
                }
                out.extend_from_slice(&build_pos_lookup_records(recs));
                let rel = (body_start - set_start) as u16;
                let slot = rule_off_start + ri * 2;
                out[slot..slot + 2].copy_from_slice(&rel.to_be_bytes());
            }
            let slot = set_offsets_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&(set_start as u16).to_be_bytes());
        }
        let cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(covered));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    #[test]
    fn rewrite_context_pos_format1_remaps_input_tail_and_lookup_index() {
        // Coverage [10]; one rule with input tail [20], one PosLookupRecord
        // (sequence_index=0, lookup_list_index=3).
        let bytes = build_context_pos_format1(&[10], &[vec![(vec![20], vec![(0, 3)])]]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2)]);
        let renumber: Vec<Option<u16>> = vec![Some(0), Some(1), Some(2), Some(7)];
        let ctx = RewriterCtx::new(&map, Some(&renumber));
        let rs = rewrite_context_pos(&ctx, &bytes).unwrap();
        // Subtable parses through the shared layout::Context1 helper.
        let parsed = sigilbuzz::tables::gpos::ContextPos::parse(&rs.bytes).unwrap();
        assert!(matches!(
            parsed,
            sigilbuzz::tables::gpos::ContextPos::Format1(_)
        ));
        // Walk the rewritten bytes manually to verify the lookup index.
        // Layout: u16 fmt, Offset16 cov, u16 setCount, Offset16[count],
        // then sets with rules. We pluck the first set's first rule.
        let set_off = u16::from_be_bytes([rs.bytes[6], rs.bytes[7]]) as usize;
        let rule_off = u16::from_be_bytes([rs.bytes[set_off + 2], rs.bytes[set_off + 3]]) as usize;
        let rule_abs = set_off + rule_off;
        // Rule: u16 glyphCount, u16 recCount, u16 tail[count-1], records[]
        let glyph_count = u16::from_be_bytes([rs.bytes[rule_abs], rs.bytes[rule_abs + 1]]);
        assert_eq!(glyph_count, 2);
        let rec_count = u16::from_be_bytes([rs.bytes[rule_abs + 2], rs.bytes[rule_abs + 3]]);
        assert_eq!(rec_count, 1);
        // Tail (1 entry): the remapped 20 -> 2.
        let tail0 = u16::from_be_bytes([rs.bytes[rule_abs + 4], rs.bytes[rule_abs + 5]]);
        assert_eq!(tail0, 2);
        // Record: sequence=0, lookup=7 (renumbered from 3).
        let seq = u16::from_be_bytes([rs.bytes[rule_abs + 6], rs.bytes[rule_abs + 7]]);
        let li = u16::from_be_bytes([rs.bytes[rule_abs + 8], rs.bytes[rule_abs + 9]]);
        assert_eq!(seq, 0);
        assert_eq!(li, 7);
    }

    #[test]
    fn rewrite_context_pos_format1_drops_when_input_tail_drops() {
        let bytes = build_context_pos_format1(&[10], &[vec![(vec![20], vec![(0, 3)])]]);
        // Drop gid 20: the rule can't fire.
        let map = map_from_pairs(&[(0, 0), (10, 1)]);
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_context_pos(&ctx, &bytes).is_none());
    }

    /// Builds a fmt-3 context-positioning subtable.
    fn build_context_pos_format3(coverages: &[Vec<u16>], records: &[(u16, u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&3u16.to_be_bytes()); // posFormat
        out.extend_from_slice(&(coverages.len() as u16).to_be_bytes());
        out.extend_from_slice(&(records.len() as u16).to_be_bytes());
        let cov_slots = out.len();
        for _ in 0..coverages.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        out.extend_from_slice(&build_pos_lookup_records(records));
        for (i, gs) in coverages.iter().enumerate() {
            let body_start = out.len() as u16;
            out.extend_from_slice(&build_coverage_format1(gs));
            let slot = cov_slots + i * 2;
            out[slot..slot + 2].copy_from_slice(&body_start.to_be_bytes());
        }
        out
    }

    #[test]
    fn rewrite_context_pos_format3_remaps_lookup_index() {
        let bytes = build_context_pos_format3(&[vec![10, 11], vec![20]], &[(0, 1), (1, 5)]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (11, 2), (20, 3)]);
        // renumber drops 5 -> record at sequence_index=1 falls out, only
        // the (0, 1) record with new lookup index 9 survives.
        let renumber: Vec<Option<u16>> = vec![
            Some(0u16),
            Some(9u16),
            Some(2u16),
            Some(3u16),
            Some(4u16),
            None,
        ];
        let ctx = RewriterCtx::new(&map, Some(&renumber));
        let rs = rewrite_context_pos(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gpos::ContextPos::parse(&rs.bytes).unwrap();
        assert!(matches!(
            parsed,
            sigilbuzz::tables::gpos::ContextPos::Format3(_)
        ));
        // Pluck the surviving record. Layout: u16 fmt, u16 glyphCount,
        // u16 recCount, Offset16[glyphCount], record[recCount].
        let glyph_count = u16::from_be_bytes([rs.bytes[2], rs.bytes[3]]);
        assert_eq!(glyph_count, 2);
        let rec_count = u16::from_be_bytes([rs.bytes[4], rs.bytes[5]]);
        assert_eq!(rec_count, 1);
        let recs_start = 6 + glyph_count as usize * 2;
        let li = u16::from_be_bytes([rs.bytes[recs_start + 2], rs.bytes[recs_start + 3]]);
        assert_eq!(li, 9);
    }

    #[test]
    fn rewrite_context_pos_format3_keeps_rule_when_record_targets_dropped() {
        // Left without records the rule acts as `ignore pos`: it still
        // stops the later subtables of its lookup, so it must stay.
        let bytes = build_context_pos_format3(&[vec![10]], &[(0, 5)]);
        let map = map_from_pairs(&[(0, 0), (10, 1)]);
        let renumber: Vec<Option<u16>> = vec![None, None, None, None, None, None];
        let ctx = RewriterCtx::new(&map, Some(&renumber));
        let rs = rewrite_context_pos(&ctx, &bytes).expect("the rule survives");
        let glyph_count = u16::from_be_bytes([rs.bytes[2], rs.bytes[3]]);
        let rec_count = u16::from_be_bytes([rs.bytes[4], rs.bytes[5]]);
        assert_eq!((glyph_count, rec_count), (1, 0));
    }

    #[test]
    fn rewrite_chain_context_pos_format3_keeps_source_ignore_rule() {
        // Compiled from `ignore pos a b' c;`: no records to begin with.
        let bytes = build_chain_context_pos_format3(&[vec![5]], &[vec![10]], &[vec![30]], &[]);
        let map = map_from_pairs(&[(0, 0), (5, 50), (10, 100), (30, 300)]);
        let renumber: Vec<Option<u16>> = vec![Some(0)];
        let ctx = RewriterCtx::new(&map, Some(&renumber));
        let rs = rewrite_chain_context_pos(&ctx, &bytes).expect("the ignore rule survives");
        // u16 fmt, then per sequence a count and its Offset16s, then
        // the record count.
        let at = |pos: usize| u16::from_be_bytes([rs.bytes[pos], rs.bytes[pos + 1]]);
        assert_eq!((at(2), at(6), at(10), at(14)), (1, 1, 1, 0));
    }

    // ----- Type 8: Chained Context Positioning -----

    /// Builds a fmt-3 chained-context positioning subtable.
    fn build_chain_context_pos_format3(
        backtrack: &[Vec<u16>],
        input: &[Vec<u16>],
        lookahead: &[Vec<u16>],
        records: &[(u16, u16)],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&3u16.to_be_bytes());
        out.extend_from_slice(&(backtrack.len() as u16).to_be_bytes());
        let bt_slots = out.len();
        for _ in 0..backtrack.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        out.extend_from_slice(&(input.len() as u16).to_be_bytes());
        let in_slots = out.len();
        for _ in 0..input.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        out.extend_from_slice(&(lookahead.len() as u16).to_be_bytes());
        let la_slots = out.len();
        for _ in 0..lookahead.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        out.extend_from_slice(&(records.len() as u16).to_be_bytes());
        out.extend_from_slice(&build_pos_lookup_records(records));
        let mut patch = |slots: usize, covs: &[Vec<u16>]| {
            for (i, gs) in covs.iter().enumerate() {
                let body_start = out.len() as u16;
                out.extend_from_slice(&build_coverage_format1(gs));
                let slot = slots + i * 2;
                out[slot..slot + 2].copy_from_slice(&body_start.to_be_bytes());
            }
        };
        patch(bt_slots, backtrack);
        patch(in_slots, input);
        patch(la_slots, lookahead);
        out
    }

    #[test]
    fn rewrite_chain_context_pos_format3_keeps_when_all_survive() {
        let bytes = build_chain_context_pos_format3(
            &[vec![5]],
            &[vec![10, 11], vec![12]],
            &[vec![30]],
            &[(0, 1)],
        );
        let map = map_from_pairs(&[(0, 0), (5, 50), (10, 100), (11, 101), (12, 102), (30, 300)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_chain_context_pos(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gpos::ChainContextPos::parse(&rs.bytes).unwrap();
        assert!(matches!(
            parsed,
            sigilbuzz::tables::gpos::ChainContextPos::Format3(_)
        ));
    }

    #[test]
    fn rewrite_chain_context_pos_format3_drops_when_backtrack_empties() {
        let bytes =
            build_chain_context_pos_format3(&[vec![5]], &[vec![10]], &[vec![30]], &[(0, 1)]);
        // Drop gid 5: backtrack coverage empties -> subtable dies.
        let map = map_from_pairs(&[(0, 0), (10, 100), (30, 300)]);
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_chain_context_pos(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_chain_context_pos_format3_renumbers_lookup_index() {
        let bytes = build_chain_context_pos_format3(&[], &[vec![10]], &[], &[(0, 5)]);
        let map = map_from_pairs(&[(0, 0), (10, 100)]);
        let renumber: Vec<Option<u16>> =
            vec![Some(0), Some(1), Some(2), Some(3), Some(4), Some(11)];
        let ctx = RewriterCtx::new(&map, Some(&renumber));
        let rs = rewrite_chain_context_pos(&ctx, &bytes).unwrap();
        // Format-3 chain layout: u16 fmt, u16 btCount, Offset16[bt],
        // u16 inCount, Offset16[in], u16 laCount, Offset16[la],
        // u16 recCount, record[recCount], coverages.
        let mut p = 2usize;
        let bt_count = u16::from_be_bytes([rs.bytes[p], rs.bytes[p + 1]]) as usize;
        p += 2 + bt_count * 2;
        let in_count = u16::from_be_bytes([rs.bytes[p], rs.bytes[p + 1]]) as usize;
        p += 2 + in_count * 2;
        let la_count = u16::from_be_bytes([rs.bytes[p], rs.bytes[p + 1]]) as usize;
        p += 2 + la_count * 2;
        let _rec_count = u16::from_be_bytes([rs.bytes[p], rs.bytes[p + 1]]);
        p += 2;
        let li = u16::from_be_bytes([rs.bytes[p + 2], rs.bytes[p + 3]]);
        assert_eq!(li, 11);
    }

    #[test]
    fn rewrite_chain_context_pos_via_dispatcher() {
        let bytes = build_chain_context_pos_format3(&[], &[vec![10]], &[], &[(0, 1)]);
        let map = map_from_pairs(&[(0, 0), (10, 100)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = one(rewrite_subtable(&ctx, gpos_type::CHAINED_CONTEXT, &bytes));
        assert!(sigilbuzz::tables::gpos::ChainContextPos::parse(&rs.bytes).is_ok());
    }

    #[test]
    fn rewrite_context_pos_via_dispatcher() {
        let bytes = build_context_pos_format3(&[vec![10]], &[(0, 1)]);
        let map = map_from_pairs(&[(0, 0), (10, 100)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = one(rewrite_subtable(&ctx, gpos_type::CONTEXT, &bytes));
        assert!(sigilbuzz::tables::gpos::ContextPos::parse(&rs.bytes).is_ok());
    }

    // ----- PairPos format 2: class collapse via fmt-1 fallback -----

    #[test]
    fn rewrite_pair_pos_format2_class_collapse_uses_format1_fallback() {
        // Source: 4 first-glyphs in 2 classes (10/11 -> class 1, 12/13 ->
        // class 2); 4 second-glyphs in 2 classes (20/21 -> class 1,
        // 22/23 -> class 2). Matrix:
        //
        //   class1=0: [0, 0, 0]
        //   class1=1: [0, -10, -20]
        //   class1=2: [0, -30, -40]
        //
        // Drop 11 and 13: the kept set spans class 1 (via 10) and
        // class 2 (via 12) on the first axis, but only class 1
        // (via 20) and class 2 (via 22) on the second.
        let cd1 = build_classdef_format1(10, &[1, 1, 2, 2]);
        let cd2 = build_classdef_format1(20, &[1, 1, 2, 2]);
        let matrix: &[&[i16]] = &[&[0, 0, 0], &[0, -10, -20], &[0, -30, -40]];
        let bytes = build_pair_pos_format2(&[10, 11, 12, 13], &cd1, &cd2, matrix);

        // Drop 11 and 13.
        let map = map_from_pairs(&[(0, 0), (10, 1), (12, 2), (20, 3), (22, 4)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = one(rewrite_pair_pos_format2(&ctx, &bytes).unwrap());
        let pp = PairPos::parse(&rs.bytes).unwrap();
        // (1, 3) -> original (10, 20), class1=1 x class2=1 -> -10.
        let (v1, _) = pp.lookup(1, 3).unwrap();
        assert_eq!(v1.x_advance, -10);
        // (1, 4) -> (10, 22), class1=1 x class2=2 -> -20.
        let (v1b, _) = pp.lookup(1, 4).unwrap();
        assert_eq!(v1b.x_advance, -20);
        // (2, 3) -> (12, 20), class1=2 x class2=1 -> -30.
        let (v1c, _) = pp.lookup(2, 3).unwrap();
        assert_eq!(v1c.x_advance, -30);
        // (2, 4) -> (12, 22), class1=2 x class2=2 -> -40.
        let (v1d, _) = pp.lookup(2, 4).unwrap();
        assert_eq!(v1d.x_advance, -40);
    }

    #[test]
    fn rewrite_pair_pos_format2_class_collapse_drops_zero_kerning() {
        // Same shape as above but matrix[1][1] = 0. The (1, 3) pair
        // should drop because the surviving cell is all-zero.
        let cd1 = build_classdef_format1(10, &[1, 1]);
        let cd2 = build_classdef_format1(20, &[1, 1]);
        let matrix: &[&[i16]] = &[&[0, 0], &[0, 0]];
        let bytes = build_pair_pos_format2(&[10, 11], &cd1, &cd2, matrix);
        let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2)]);
        let ctx = RewriterCtx::new(&map, None);
        // Every cell is zero -> no surviving pairs -> subtable drops.
        assert!(rewrite_pair_pos_format2(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_pair_pos_format2_pass_through_for_large_subsets() {
        // When the kept set is large the heuristic in
        // `should_use_format1_fallback` keeps us on the fmt-2
        // pass-through. We exercise that path by building a kept-gid
        // set whose first x second cross product blows past the 256
        // budget. The matrix bytes survive verbatim through the
        // pass-through path.
        let covered: Vec<u16> = (10..=30).collect();
        let cd1_classes: Vec<u16> = covered.iter().map(|_| 1).collect();
        let cd1 = build_classdef_format1(10, &cd1_classes);
        let cd2 = build_classdef_format1(40, &alloc::vec![1u16; 21]);
        let matrix: &[&[i16]] = &[&[0, 0], &[0, -25]];
        let bytes = build_pair_pos_format2(&covered, &cd1, &cd2, matrix);

        // Keep everything (large kept set; cross product = 21 * ~21 = 441 > 256).
        let mut pairs: Vec<(u16, u16)> = alloc::vec![(0, 0)];
        for g in 10..=30 {
            pairs.push((g, g - 9));
        }
        for g in 40..=60 {
            pairs.push((g, g - 18));
        }
        let map = map_from_pairs(&pairs);
        let ctx = RewriterCtx::new(&map, None);
        let rs = one(rewrite_pair_pos_format2(&ctx, &bytes).unwrap());
        let pp = PairPos::parse(&rs.bytes).unwrap();
        // (1, 22) -> original (10, 40): class1=1, class2=1 -> -25.
        let (v1, _) = pp.lookup(1, 22).unwrap();
        assert_eq!(v1.x_advance, -25);
    }

    #[test]
    fn rewrite_single_adj_format2_without_value_count_returns_none() {
        // posFormat 2 needs a valueCount at bytes 6..8. A subtable that
        // ends after the 6-byte shared prefix used to index past the end.
        let bytes = [0u8, 2, 0, 6, 0, 1];
        let map = map_from_pairs(&[(0, 0)]);
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_single_adj(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_mark_base_with_truncated_base_array_drops_the_base() {
        // BaseArray claims one base with four anchor offsets, but the
        // subtable ends after the first offset. Reading the missing row
        // used to index past the end.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u16.to_be_bytes()); // posFormat
        bytes.extend_from_slice(&12u16.to_be_bytes()); // markCoverage
        bytes.extend_from_slice(&18u16.to_be_bytes()); // baseCoverage
        bytes.extend_from_slice(&4u16.to_be_bytes()); // markClassCount
        bytes.extend_from_slice(&24u16.to_be_bytes()); // markArray
        bytes.extend_from_slice(&36u16.to_be_bytes()); // baseArray
        bytes.extend_from_slice(&build_coverage_format1(&[10])); // 12..18
        bytes.extend_from_slice(&build_coverage_format1(&[20])); // 18..24
                                                                 // MarkArray at 24: one record, class 0, anchor at +6.
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&6u16.to_be_bytes());
        bytes.extend_from_slice(&build_anchor(5, 7)); // 30..36
                                                      // BaseArray at 36: baseCount 1, then a single anchor offset.
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());

        let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2)]);
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_mark_attach(&ctx, &bytes, MarkAttachKind::FixedClassRow).is_empty());
    }

    #[test]
    fn rewrite_pair_pos_format2_fallback_handles_a_large_class_def2() {
        // One first glyph triggers the fmt-1 fallback, and every one of
        // 65535 kept glyphs looks up its class in a ClassDef with one
        // range per glyph. A linear scan per lookup made this take
        // billions of steps.
        let range_count: u16 = u16::MAX;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes()); // posFormat
        bytes.extend_from_slice(&20u16.to_be_bytes()); // coverage
        bytes.extend_from_slice(&X_ADVANCE.to_be_bytes()); // valueFormat1
        bytes.extend_from_slice(&0u16.to_be_bytes()); // valueFormat2
        bytes.extend_from_slice(&26u16.to_be_bytes()); // classDef1
        bytes.extend_from_slice(&30u16.to_be_bytes()); // classDef2
        bytes.extend_from_slice(&1u16.to_be_bytes()); // class1Count
        bytes.extend_from_slice(&2u16.to_be_bytes()); // class2Count
        bytes.extend_from_slice(&0i16.to_be_bytes()); // cell (0, 0)
        bytes.extend_from_slice(&(-30i16).to_be_bytes()); // cell (0, 1)
        bytes.extend_from_slice(&build_coverage_format1(&[5])); // 20..26
        bytes.extend_from_slice(&2u16.to_be_bytes()); // classDef1: empty format 2
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&2u16.to_be_bytes()); // classDef2 at 30
        bytes.extend_from_slice(&range_count.to_be_bytes());
        for g in 0..range_count {
            bytes.extend_from_slice(&g.to_be_bytes());
            bytes.extend_from_slice(&g.to_be_bytes());
            bytes.extend_from_slice(&1u16.to_be_bytes());
        }

        let table: Vec<Option<u16>> = (0..range_count).map(Some).collect();
        let map = GidMap::from_table(table);
        let ctx = RewriterCtx::new(&map, None);
        // Every kept glyph kerns after glyph 5. The PairSet is large, but
        // only its start has to lie within 16 bits of the subtable, so
        // one subtable holds it. The point is that the rewrite finishes.
        let subs = rewrite_pair_pos_format2(&ctx, &bytes).unwrap_or_default();
        assert_eq!(subs.len(), 1);
        assert!(ctx.offsets.check("pair pos").is_ok());
    }
}
