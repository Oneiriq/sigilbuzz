//! Rewriter for GPOS type 3 (cursive attachment).

use alloc::vec::Vec;

use crate::device::{copy_anchor, Dedup};
use crate::layout::{parse_coverage_glyphs, RewriterCtx, RewrittenSubtable};

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
pub(super) fn rewrite_cursive(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
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
