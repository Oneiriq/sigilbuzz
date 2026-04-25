//! GSUB byte-level rewriter.
//!
//! Walks every lookup in the source `GSUB` table and produces a new
//! `GSUB` whose Coverage / ClassDef / substitution-target gid
//! references resolve against the new gid namespace defined by the
//! caller's [`GidMap`].
//!
//! # Per-lookup-type coverage
//!
//! As of this commit the rewriter ships byte-level support for:
//!
//! - **Type 1 (single-sub)** — formats 1 (delta) and 2 (explicit). Auto-
//!   selects between formats; falls back to format 2 when a remapped
//!   delta would no longer produce contiguous targets.
//! - **Type 7 (extension)** — pass-through after rewriting the inner
//!   subtable. Only inner types this module implements are passed
//!   through; everything else drops the lookup.
//!
//! Every other lookup type drops its lookup. The drop cascade then
//! removes empty subtables, lookups with no surviving subtable,
//! features that name no surviving lookup, and scripts whose features
//! have all been dropped — see [`super::layout`].
//!
//! Issue tracking the remaining lookup types: see the sibling issue
//! filed alongside this module.

use alloc::vec::Vec;

use sigilbuzz::tables::gsub::lookup_type as gsub_type;

use crate::coverage::emit_coverage_from_pairs;
use crate::layout::{parse_coverage_glyphs, RewriterCtx, RewrittenLookup, RewrittenSubtable};

/// Rewrites a single GSUB lookup. Returns `None` if the lookup has no
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
        gsub_type::SINGLE => rewrite_single(ctx, sub),
        gsub_type::EXTENSION => rewrite_extension(ctx, sub),
        // Other types drop until their byte-level rewriter ships.
        // The drop cascade in [`crate::layout`] handles propagating
        // the loss up through lookups / features / scripts.
        _ => None,
    }
}

/// Rewrites a GSUB type 1 (Single Substitution) subtable. Picks the
/// smaller of formats 1 (delta) or 2 (explicit) given the remapped
/// covered/substitute pairs.
fn rewrite_single(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 4 {
        return None;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let cov_bytes = sub.get(cov_off..)?;
    let covered = parse_coverage_glyphs(cov_bytes);

    // Build the (input → output) pairs in the *old* gid namespace
    // first, then drop pairs whose input or output is not in the kept
    // set, then remap.
    let pairs_old: Vec<(u16, u16)> = match format {
        1 => {
            // Format 1: u16 format, Offset16 cov, i16 delta.
            if sub.len() < 6 {
                return None;
            }
            let delta = i16::from_be_bytes([sub[4], sub[5]]);
            covered
                .iter()
                .map(|&g| (g, g.wrapping_add(delta as u16)))
                .collect()
        }
        2 => {
            // Format 2: u16 format, Offset16 cov, u16 glyphCount, u16 substitutes[count].
            if sub.len() < 6 {
                return None;
            }
            let count = u16::from_be_bytes([sub[4], sub[5]]) as usize;
            if count != covered.len() {
                // Malformed: spec requires they match. Skip.
                return None;
            }
            let need = 6 + count * 2;
            if sub.len() < need {
                return None;
            }
            let mut out = Vec::with_capacity(count);
            for (i, &g) in covered.iter().enumerate() {
                let off = 6 + i * 2;
                let s = u16::from_be_bytes([sub[off], sub[off + 1]]);
                out.push((g, s));
            }
            out
        }
        _ => return None,
    };

    // Filter and remap.
    let map = ctx.gid_map;
    let mut new_pairs: Vec<(u16, u16)> = Vec::with_capacity(pairs_old.len());
    for &(input, output) in &pairs_old {
        let Some(new_in) = map.map(input) else {
            continue;
        };
        let Some(new_out) = map.map(output) else {
            continue;
        };
        new_pairs.push((new_in, new_out));
    }
    if new_pairs.is_empty() {
        return None;
    }
    new_pairs.sort_unstable_by_key(|(g, _)| *g);
    new_pairs.dedup_by_key(|(g, _)| *g);

    Some(emit_single_subtable(&new_pairs))
}

/// Encodes a single-sub subtable, picking format 1 vs format 2 by
/// byte size. Coverage is emitted directly into the subtable body so
/// callers don't need to track sub-offsets.
fn emit_single_subtable(pairs: &[(u16, u16)]) -> RewrittenSubtable {
    // Try format 1 (delta). Viable only if every pair's
    // (output - input) wraps to the same i16. We compute the candidate
    // delta from the first pair and verify every other pair matches
    // under wrapping arithmetic.
    let f1_delta: Option<i16> = if pairs.is_empty() {
        None
    } else {
        let (in0, out0) = pairs[0];
        let candidate = out0.wrapping_sub(in0) as i16;
        let viable = pairs
            .iter()
            .all(|&(i, o)| (o.wrapping_sub(i)) as i16 == candidate);
        if viable {
            Some(candidate)
        } else {
            None
        }
    };

    // Format 1 cost: 6 bytes header + Coverage size (emitted right after).
    // Format 2 cost: 6 bytes header + 2 * count + Coverage size.
    // Coverage size is identical between formats so it doesn't tip the
    // decision; we just pick whichever header form is smaller.
    let inputs_only: Vec<(u16, u16)> = pairs
        .iter()
        .enumerate()
        .map(|(i, &(g, _))| (g, i as u16))
        .collect();
    let cov_bytes = emit_coverage_from_pairs(&inputs_only);

    let mut out = Vec::new();
    if let Some(delta) = f1_delta {
        // Format 1.
        out.extend_from_slice(&1u16.to_be_bytes()); // substFormat
                                                    // Coverage offset placeholder.
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&delta.to_be_bytes());
        let cov_off = out.len() as u16;
        out.extend_from_slice(&cov_bytes);
        out[2..4].copy_from_slice(&cov_off.to_be_bytes());
    } else {
        // Format 2.
        out.extend_from_slice(&2u16.to_be_bytes()); // substFormat
        out.extend_from_slice(&0u16.to_be_bytes()); // coverage offset placeholder
        out.extend_from_slice(&(pairs.len() as u16).to_be_bytes()); // glyphCount
        for &(_, sub_gid) in pairs {
            out.extend_from_slice(&sub_gid.to_be_bytes());
        }
        let cov_off = out.len() as u16;
        out.extend_from_slice(&cov_bytes);
        out[2..4].copy_from_slice(&cov_off.to_be_bytes());
    }
    RewrittenSubtable { bytes: out }
}

/// Rewrites a GSUB type 7 (Extension) subtable. The Extension wrapper
/// is u16 format=1, u16 extensionLookupType, u32 extensionOffset. We
/// recurse into the inner subtable using the wrapped lookup type, then
/// re-emit a fresh Extension subtable around the rewritten inner bytes.
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
    if inner_type == gsub_type::EXTENSION {
        // Spec disallows Extension referring to Extension.
        return None;
    }
    let inner = sub.get(inner_off..)?;
    let rewritten_inner = rewrite_subtable(ctx, inner_type, inner)?;

    // Re-wrap. The extension subtable points the inner bytes at offset
    // 8 (immediately after the wrapper).
    let mut out = Vec::with_capacity(8 + rewritten_inner.bytes.len());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&inner_type.to_be_bytes());
    out.extend_from_slice(&8u32.to_be_bytes());
    out.extend_from_slice(&rewritten_inner.bytes);
    Some(RewrittenSubtable { bytes: out })
}

/// Walks a parsed GSUB table and pulls in implicit substitution
/// targets (type 1/2/3) for every kept input glyph. The closure walker
/// already pulls in ligature components (type 4) and mark-base
/// partners; this fills in the substitution-target side.
///
/// Iterates the source GSUB lookups; for each kept input glyph that a
/// type-1/2/3 lookup covers, marks the substitution output(s) as kept.
/// Mutates `keep` in place and returns whether anything was added so
/// the caller can decide to re-run the closure pass.
pub(crate) fn pull_in_substitution_targets(face: &sigilbuzz::Face<'_>, keep: &mut [bool]) -> bool {
    let Ok(Some(gsub)) = face.gsub() else {
        return false;
    };
    let lookups = gsub.lookup_list();
    let mut changed = false;
    for li in 0..lookups.len() {
        let Some(lookup) = lookups.get(li) else {
            continue;
        };
        let lt = unwrap_extension_lookup_type(&lookup);
        for si in 0..lookup.subtable_count() {
            let Some(sub) = subtable_with_extension(&lookup, si) else {
                continue;
            };
            match lt {
                gsub_type::SINGLE => {
                    changed |= pull_single(sub, keep);
                }
                gsub_type::MULTIPLE => {
                    changed |= pull_multiple(sub, keep);
                }
                gsub_type::ALTERNATE => {
                    changed |= pull_alternate_default(sub, keep);
                }
                _ => {}
            }
        }
    }
    changed
}

fn unwrap_extension_lookup_type(lookup: &sigilbuzz::tables::layout::Lookup<'_>) -> u16 {
    if lookup.lookup_type() != gsub_type::EXTENSION {
        return lookup.lookup_type();
    }
    let Some(sub) = lookup.subtable_bytes(0) else {
        return lookup.lookup_type();
    };
    if sub.len() < 8 {
        return lookup.lookup_type();
    }
    u16::from_be_bytes([sub[2], sub[3]])
}

fn subtable_with_extension<'a>(
    lookup: &sigilbuzz::tables::layout::Lookup<'a>,
    index: u16,
) -> Option<&'a [u8]> {
    let sub = lookup.subtable_bytes(index)?;
    if lookup.lookup_type() != gsub_type::EXTENSION {
        return Some(sub);
    }
    if sub.len() < 8 {
        return None;
    }
    let ext_off = u32::from_be_bytes([sub[4], sub[5], sub[6], sub[7]]) as usize;
    sub.get(ext_off..)
}

/// Pulls in the substitute glyph for every kept input glyph in a
/// type-1 (single-sub) subtable.
fn pull_single(sub: &[u8], keep: &mut [bool]) -> bool {
    if sub.len() < 4 {
        return false;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let Some(cov_bytes) = sub.get(cov_off..) else {
        return false;
    };
    let covered = parse_coverage_glyphs(cov_bytes);
    let mut changed = false;
    match format {
        1 => {
            if sub.len() < 6 {
                return false;
            }
            let delta = i16::from_be_bytes([sub[4], sub[5]]);
            for &g in &covered {
                if (g as usize) >= keep.len() || !keep[g as usize] {
                    continue;
                }
                let target = g.wrapping_add(delta as u16);
                if (target as usize) < keep.len() && !keep[target as usize] {
                    keep[target as usize] = true;
                    changed = true;
                }
            }
        }
        2 => {
            if sub.len() < 6 {
                return false;
            }
            let count = u16::from_be_bytes([sub[4], sub[5]]) as usize;
            if count != covered.len() {
                return false;
            }
            let need = 6 + count * 2;
            if sub.len() < need {
                return false;
            }
            for (i, &g) in covered.iter().enumerate() {
                if (g as usize) >= keep.len() || !keep[g as usize] {
                    continue;
                }
                let off = 6 + i * 2;
                let target = u16::from_be_bytes([sub[off], sub[off + 1]]);
                if (target as usize) < keep.len() && !keep[target as usize] {
                    keep[target as usize] = true;
                    changed = true;
                }
            }
        }
        _ => {}
    }
    changed
}

/// Pulls in every substitute in the sequence for every kept input in a
/// type-2 (multiple-sub) subtable.
fn pull_multiple(sub: &[u8], keep: &mut [bool]) -> bool {
    if sub.len() < 6 {
        return false;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    if format != 1 {
        return false;
    }
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let seq_count = u16::from_be_bytes([sub[4], sub[5]]) as usize;
    let Some(cov_bytes) = sub.get(cov_off..) else {
        return false;
    };
    let covered = parse_coverage_glyphs(cov_bytes);
    if covered.len() != seq_count {
        return false;
    }
    let mut changed = false;
    for (i, &g) in covered.iter().enumerate() {
        if (g as usize) >= keep.len() || !keep[g as usize] {
            continue;
        }
        let off_off = 6 + i * 2;
        if off_off + 2 > sub.len() {
            return changed;
        }
        let seq_off = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        let Some(seq_bytes) = sub.get(seq_off..) else {
            continue;
        };
        if seq_bytes.len() < 2 {
            continue;
        }
        let glyph_count = u16::from_be_bytes([seq_bytes[0], seq_bytes[1]]) as usize;
        let need = 2 + glyph_count * 2;
        if seq_bytes.len() < need {
            continue;
        }
        for j in 0..glyph_count {
            let goff = 2 + j * 2;
            let target = u16::from_be_bytes([seq_bytes[goff], seq_bytes[goff + 1]]);
            if (target as usize) < keep.len() && !keep[target as usize] {
                keep[target as usize] = true;
                changed = true;
            }
        }
    }
    changed
}

/// Pulls in only the *default* alternate (index 0) for every kept input
/// in a type-3 (alternate-sub) subtable. User-selected alternates ride
/// in only when their alternate-set output happens to be reachable
/// some other way.
fn pull_alternate_default(sub: &[u8], keep: &mut [bool]) -> bool {
    if sub.len() < 6 {
        return false;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    if format != 1 {
        return false;
    }
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let alt_set_count = u16::from_be_bytes([sub[4], sub[5]]) as usize;
    let Some(cov_bytes) = sub.get(cov_off..) else {
        return false;
    };
    let covered = parse_coverage_glyphs(cov_bytes);
    if covered.len() != alt_set_count {
        return false;
    }
    let mut changed = false;
    for (i, &g) in covered.iter().enumerate() {
        if (g as usize) >= keep.len() || !keep[g as usize] {
            continue;
        }
        let off_off = 6 + i * 2;
        if off_off + 2 > sub.len() {
            return changed;
        }
        let alt_off = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        let Some(alt_bytes) = sub.get(alt_off..) else {
            continue;
        };
        if alt_bytes.len() < 4 {
            continue;
        }
        let glyph_count = u16::from_be_bytes([alt_bytes[0], alt_bytes[1]]) as usize;
        if glyph_count == 0 {
            continue;
        }
        let target = u16::from_be_bytes([alt_bytes[2], alt_bytes[3]]);
        if (target as usize) < keep.len() && !keep[target as usize] {
            keep[target as usize] = true;
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::GidMap;
    use alloc::{vec, vec::Vec};
    use sigilbuzz::tables::layout::Coverage as CoverageParser;

    fn build_coverage_format1(glyphs: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
        for g in glyphs {
            out.extend_from_slice(&g.to_be_bytes());
        }
        out
    }

    fn build_single_format1(covered: &[u16], delta: i16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        out.extend_from_slice(&0u16.to_be_bytes()); // cov off placeholder
        out.extend_from_slice(&delta.to_be_bytes());
        let cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(covered));
        out[2..4].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    fn build_single_format2(covered: &[u16], substitutes: &[u16]) -> Vec<u8> {
        assert_eq!(covered.len(), substitutes.len());
        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes()); // format
        out.extend_from_slice(&0u16.to_be_bytes()); // cov off placeholder
        out.extend_from_slice(&(substitutes.len() as u16).to_be_bytes());
        for s in substitutes {
            out.extend_from_slice(&s.to_be_bytes());
        }
        let cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(covered));
        out[2..4].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    fn map_from_pairs(pairs: &[(u16, u16)]) -> GidMap {
        // Build a GidMap by old gid → new gid; gids not in `pairs` map
        // to None.
        let max_old = pairs.iter().map(|(o, _)| *o).max().unwrap_or(0);
        let mut table = vec![None; (max_old as usize + 1).max(1)];
        for &(old, new) in pairs {
            table[old as usize] = Some(new);
        }
        GidMap::from_table(table)
    }

    #[test]
    fn rewrite_single_format1_remaps_delta() {
        // A → small-cap-A, B → small-cap-B, C → small-cap-C.
        // Old: covered {65,66,67}, delta +200 → outputs 265,266,267.
        let bytes = build_single_format1(&[65, 66, 67], 200);
        // New gid map: 65→1, 66→2, 67→3, 265→4, 266→5, 267→6 (.notdef stays at 0).
        let map = map_from_pairs(&[
            (0, 0),
            (65, 1),
            (66, 2),
            (67, 3),
            (265, 4),
            (266, 5),
            (267, 6),
        ]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_single(&ctx, &bytes).unwrap();
        // Verify the rewritten subtable parses and applies correctly.
        let parsed = sigilbuzz::tables::gsub::Single::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.apply(1), Some(4));
        assert_eq!(parsed.apply(2), Some(5));
        assert_eq!(parsed.apply(3), Some(6));
        assert_eq!(parsed.apply(0), None);
    }

    #[test]
    fn rewrite_single_format1_falls_back_to_format2_when_delta_breaks() {
        // After remap, the deltas no longer line up. Old: covered
        // {10, 20, 30}, delta +5 → outputs 15, 25, 35. New gid map
        // jumbles them: 10→1, 20→2, 30→3, 15→7, 25→9, 35→11. Now
        // input→output deltas are 6, 7, 8 — no constant.
        let bytes = build_single_format1(&[10, 20, 30], 5);
        let map = map_from_pairs(&[
            (0, 0),
            (10, 1),
            (20, 2),
            (30, 3),
            (15, 7),
            (25, 9),
            (35, 11),
        ]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_single(&ctx, &bytes).unwrap();
        // Format must be 2 because no constant delta works.
        assert_eq!(&rs.bytes[0..2], &2u16.to_be_bytes());
        let parsed = sigilbuzz::tables::gsub::Single::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.apply(1), Some(7));
        assert_eq!(parsed.apply(2), Some(9));
        assert_eq!(parsed.apply(3), Some(11));
    }

    #[test]
    fn rewrite_single_format2_drops_pairs_with_dropped_input() {
        // Format 2: 10→100, 20→200, 30→300. Drop input 20 from the
        // kept set. New gid map: 10→1, 30→3, 100→11, 300→33.
        let bytes = build_single_format2(&[10, 20, 30], &[100, 200, 300]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (30, 3), (100, 11), (300, 33)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_single(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Single::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.apply(1), Some(11));
        // Input 2 (the new gid for 20) is not in the map at all because
        // 20 was dropped — so it can't be in the rewritten coverage.
        assert!(parsed.apply(2).is_none() || parsed.apply(2) == Some(0));
        assert_eq!(parsed.apply(3), Some(33));
    }

    #[test]
    fn rewrite_single_drops_pairs_with_dropped_output() {
        // 10→100 stays, 20→200 dies because 200 is dropped.
        let bytes = build_single_format2(&[10, 20], &[100, 200]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2), (100, 11)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_single(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Single::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.apply(1), Some(11));
        // gid 2 (was 20) is not covered because 200 dropped.
        let cov_off = u16::from_be_bytes([rs.bytes[2], rs.bytes[3]]) as usize;
        let cov = CoverageParser::parse(&rs.bytes[cov_off..]).unwrap();
        assert!(cov.index_of(2).is_none());
    }

    #[test]
    fn rewrite_single_returns_none_when_all_pairs_drop() {
        let bytes = build_single_format2(&[10, 20], &[100, 200]);
        let map = map_from_pairs(&[(0, 0)]);
        let ctx = RewriterCtx { gid_map: &map };
        assert!(rewrite_single(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_extension_recurses_into_inner_single_sub() {
        // Build an Extension wrapper around a single-sub format 1.
        let inner = build_single_format1(&[10, 11], 5);
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format=1
        out.extend_from_slice(&gsub_type::SINGLE.to_be_bytes());
        out.extend_from_slice(&8u32.to_be_bytes()); // inner offset = 8
        out.extend_from_slice(&inner);
        let map = map_from_pairs(&[(0, 0), (10, 1), (11, 2), (15, 7), (16, 8)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_extension(&ctx, &out).unwrap();
        // Output must still be an Extension wrapper around a single-sub.
        assert_eq!(&rs.bytes[0..2], &1u16.to_be_bytes());
        let inner_type = u16::from_be_bytes([rs.bytes[2], rs.bytes[3]]);
        assert_eq!(inner_type, gsub_type::SINGLE);
        let inner_off =
            u32::from_be_bytes([rs.bytes[4], rs.bytes[5], rs.bytes[6], rs.bytes[7]]) as usize;
        let parsed = sigilbuzz::tables::gsub::Single::parse(&rs.bytes[inner_off..]).unwrap();
        assert_eq!(parsed.apply(1), Some(7));
        assert_eq!(parsed.apply(2), Some(8));
    }

    #[test]
    fn pull_single_extends_keep_set() {
        // covered {10}, delta +5 → output 15. Mark 10 kept; pull should
        // mark 15 kept.
        let bytes = build_single_format1(&[10], 5);
        let mut keep = vec![false; 32];
        keep[10] = true;
        let changed = pull_single(&bytes, &mut keep);
        assert!(changed);
        assert!(keep[15]);
    }

    #[test]
    fn pull_single_no_op_when_input_dropped() {
        let bytes = build_single_format1(&[10], 5);
        let mut keep = vec![false; 32];
        // 10 not kept → 15 not pulled.
        let changed = pull_single(&bytes, &mut keep);
        assert!(!changed);
        assert!(!keep[15]);
    }
}
