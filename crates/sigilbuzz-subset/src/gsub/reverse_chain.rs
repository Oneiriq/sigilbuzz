//! Rewriter for GSUB type 8 (reverse chaining single substitution).

use alloc::vec::Vec;

use crate::device::Dedup;
use crate::layout::{parse_coverage_glyphs, RewriterCtx, RewrittenSubtable};

/// Rewrites a GSUB type 8 (Reverse Chained Single Substitution) subtable.
///
/// ```text
///   u16      substFormat = 1
///   Offset16 coverageOffset
///   u16      backtrackGlyphCount
///   Offset16 backtrackCoverageOffsets[backtrackGlyphCount]
///   u16      lookaheadGlyphCount
///   Offset16 lookaheadCoverageOffsets[lookaheadGlyphCount]
///   u16      glyphCount                  (== Coverage count)
///   u16      substituteGlyphIDs[glyphCount]
/// ```
pub(super) fn rewrite_type8(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 6 {
        return None;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    if format != 1 {
        return None;
    }
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let map = ctx.gid_map;

    let mut p = 4usize;
    let bt_count = u16::from_be_bytes([sub[p], sub[p + 1]]) as usize;
    p += 2;
    let bt_offs_start = p;
    p += bt_count * 2;
    if sub.len() < p + 2 {
        return None;
    }
    let la_count = u16::from_be_bytes([sub[p], sub[p + 1]]) as usize;
    p += 2;
    let la_offs_start = p;
    p += la_count * 2;
    if sub.len() < p + 2 {
        return None;
    }
    let glyph_count = u16::from_be_bytes([sub[p], sub[p + 1]]) as usize;
    p += 2;
    let subs_start = p;
    if sub.len() < subs_start + glyph_count * 2 {
        return None;
    }

    let cov_bytes = sub.get(cov_off..)?;
    let covered = parse_coverage_glyphs(cov_bytes);
    if covered.len() != glyph_count {
        // Spec requires they match; tolerate mismatch by capping at the
        // smaller of the two on read but treat as malformed for emission.
        ctx.diag.in_part(
            sub,
            p - 2,
            "ReverseChainSingleSubst glyphCount differs from its Coverage",
            "a lookup subtable",
        );
        return None;
    }

    // Pair surviving covered gids with their replacement gids. The
    // closure keeps the substitute of every kept input whose context
    // can still match (see `pull_reverse_chain`), so a pair only loses
    // its substitute here when its context cannot match either, and
    // the context check below then drops the whole subtable.
    let mut new_pairs: Vec<(u16, u16)> = Vec::new();
    for (i, &input_old) in covered.iter().enumerate() {
        let Some(input_new) = map.map(input_old) else {
            continue;
        };
        let off = subs_start + i * 2;
        let sub_old = u16::from_be_bytes([sub[off], sub[off + 1]]);
        let Some(sub_new) = map.map(sub_old) else {
            continue;
        };
        new_pairs.push((input_new, sub_new));
    }
    if new_pairs.is_empty() {
        return None;
    }
    new_pairs.sort_unstable_by_key(|(g, _)| *g);
    new_pairs.dedup_by_key(|(g, _)| *g);

    // Backtrack / lookahead Coverages: every Coverage slot must
    // survive. A reverse-chain rule whose context window has any
    // empty Coverage can never match, so drop the subtable.
    let read_cov_array = |start: usize, count: usize| -> Option<Vec<Vec<u8>>> {
        let mut out = Vec::with_capacity(count);
        for j in 0..count {
            let off_off = start + j * 2;
            let cov_off = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
            let cov_bytes = sub.get(cov_off..)?;
            let covered = parse_coverage_glyphs(cov_bytes);
            let new_covered: Vec<u16> = covered.iter().filter_map(|&g| map.map(g)).collect();
            if new_covered.is_empty() {
                return None;
            }
            out.push(crate::coverage::emit_coverage_from_glyphs(&new_covered));
        }
        Some(out)
    };
    let new_bt = read_cov_array(bt_offs_start, bt_count)?;
    let new_la = read_cov_array(la_offs_start, la_count)?;

    // Emit:
    //   u16      substFormat = 1
    //   Offset16 coverageOffset                 (placeholder)
    //   u16      backtrackGlyphCount
    //   Offset16 backtrackCoverageOffsets[]     (placeholders)
    //   u16      lookaheadGlyphCount
    //   Offset16 lookaheadCoverageOffsets[]     (placeholders)
    //   u16      glyphCount
    //   u16      substituteGlyphIDs[]
    //   coverage bodies (input + bt + la), tightly packed
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(new_bt.len() as u16).to_be_bytes());
    let bt_slots_start = out.len();
    for _ in 0..new_bt.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    out.extend_from_slice(&(new_la.len() as u16).to_be_bytes());
    let la_slots_start = out.len();
    for _ in 0..new_la.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    out.extend_from_slice(&(new_pairs.len() as u16).to_be_bytes());
    for &(_, sub_gid) in &new_pairs {
        out.extend_from_slice(&sub_gid.to_be_bytes());
    }
    // Input coverage.
    let cov_off_new = ctx.off16(out.len());
    let inputs_only: Vec<u16> = new_pairs.iter().map(|&(g, _)| g).collect();
    out.extend_from_slice(&crate::coverage::emit_coverage_from_glyphs(&inputs_only));
    out[cov_slot..cov_slot + 2].copy_from_slice(&cov_off_new.to_be_bytes());
    // Backtrack / lookahead bodies.
    let mut bodies = Dedup::default();
    let mut patch_array = |slots_start: usize, covs: &[Vec<u8>]| {
        for (i, cov) in covs.iter().enumerate() {
            let body_start = ctx.off16(bodies.place(&mut out, cov));
            let slot = slots_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&body_start.to_be_bytes());
        }
    };
    patch_array(bt_slots_start, &new_bt);
    patch_array(la_slots_start, &new_la);
    Some(RewrittenSubtable { bytes: out })
}
