//! Rewriter for GPOS type 1 (single adjustment), and the ValueRecord
//! device carrier the pair rewriter shares.

use alloc::vec::Vec;

use crate::coverage::emit_coverage_from_pairs;
use crate::layout::{parse_coverage_glyphs, RewriterCtx, RewrittenSubtable};

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
pub(super) fn rewrite_single_adj(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
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
pub(super) fn carry_devices(
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
pub(super) fn value_record_size(format: u16) -> usize {
    // Each set defined bit is one i16 (or Offset16, same size).
    // Defined bits are 0x0001..=0x0080.
    const DEFINED: u16 = 0x00FF;
    (format & DEFINED).count_ones() as usize * 2
}
