//! Rewriter for GSUB type 6 (chained context substitution).

use alloc::vec::Vec;

use super::context::{encode_lookup_records, parse_and_remap_lookup_records};
use super::rewrite_coverage_array;
use super::single::{emit_offset_array_subtable, emit_offset_list};
use crate::device::Dedup;
use crate::layout::{RewriterCtx, RewrittenSubtable};

/// Rewrites a GSUB type 6 (Chained Context Substitution) subtable.
pub(crate) fn rewrite_type6(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 2 {
        return None;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    match format {
        1 => rewrite_type6_format1(ctx, sub),
        2 => rewrite_type6_format2(ctx, sub),
        3 => rewrite_type6_format3(ctx, sub),
        _ => None,
    }
}

/// GSUB type 6 format 1 (rule-based chained-context substitution).
///
/// ```text
///   u16      substFormat = 1
///   Offset16 coverageOffset
///   u16      chainRuleSetCount
///   Offset16 chainRuleSetOffsets[chainRuleSetCount]
///
///   ChainRuleSet:
///     u16      chainRuleCount
///     Offset16 chainRuleOffsets[chainRuleCount]   (ChainRuleSet-relative)
///
///   ChainRule:
///     u16 backtrackGlyphCount
///     u16 backtrackSequence[backtrackGlyphCount]   (reverse order)
///     u16 inputGlyphCount             (>= 1; first input is implicit)
///     u16 inputSequence[inputGlyphCount - 1]
///     u16 lookaheadGlyphCount
///     u16 lookaheadSequence[lookaheadGlyphCount]
///     u16 substLookupRecordCount
///     SubstLookupRecord records[substLookupRecordCount]
/// ```
fn rewrite_type6_format1(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 6 {
        return None;
    }
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let set_count = u16::from_be_bytes([sub[4], sub[5]]) as usize;
    let set_offsets_off = 6usize;
    if sub.len() < set_offsets_off + set_count * 2 {
        return None;
    }
    let cov_bytes = sub.get(cov_off..)?;
    let covered = ctx.gid_map.coverage_glyphs(cov_bytes)?;
    let pair_count = covered.len().min(set_count);
    let map = ctx.gid_map;

    let mut surviving_sets: Vec<(u16, Vec<u8>)> = Vec::new();
    for (i, &first_old) in covered.iter().enumerate().take(pair_count) {
        let Some(first_new) = map.map(first_old) else {
            continue;
        };
        let off_off = set_offsets_off + i * 2;
        let set_off_rel = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        if set_off_rel == 0 {
            continue;
        }
        let Some(set_bytes) = sub.get(set_off_rel..) else {
            continue;
        };
        let Some(rewritten_set) = rewrite_type6_rule_set(set_bytes, ctx) else {
            continue;
        };
        surviving_sets.push((first_new, rewritten_set));
    }

    if surviving_sets.is_empty() {
        return None;
    }
    Some(emit_offset_array_subtable(ctx, &surviving_sets))
}

fn rewrite_type6_rule_set(set_bytes: &[u8], ctx: &RewriterCtx) -> Option<Vec<u8>> {
    if set_bytes.len() < 2 {
        return None;
    }
    let rule_count = u16::from_be_bytes([set_bytes[0], set_bytes[1]]) as usize;
    let map = ctx.gid_map;
    if set_bytes.len() < 2 + rule_count * 2 || !map.spend(rule_count) {
        return None;
    }
    let mut surviving_rules: Vec<Vec<u8>> = Vec::new();
    for i in 0..rule_count {
        let off_off = 2 + i * 2;
        let rule_off = u16::from_be_bytes([set_bytes[off_off], set_bytes[off_off + 1]]) as usize;
        let Some(rule_bytes) = set_bytes.get(rule_off..) else {
            continue;
        };
        // Walk the variable-length rule body.
        if rule_bytes.len() < 2 {
            continue;
        }
        let bt_count = u16::from_be_bytes([rule_bytes[0], rule_bytes[1]]) as usize;
        let mut p = 2 + bt_count * 2;
        if rule_bytes.len() < p + 2 {
            continue;
        }
        let backtrack_start = 2;
        let in_count = u16::from_be_bytes([rule_bytes[p], rule_bytes[p + 1]]) as usize;
        p += 2;
        let in_tail = in_count.saturating_sub(1);
        let input_start = p;
        p += in_tail * 2;
        if rule_bytes.len() < p + 2 {
            continue;
        }
        let la_count = u16::from_be_bytes([rule_bytes[p], rule_bytes[p + 1]]) as usize;
        p += 2;
        let lookahead_start = p;
        p += la_count * 2;
        if rule_bytes.len() < p + 2 {
            continue;
        }
        let lookup_count = u16::from_be_bytes([rule_bytes[p], rule_bytes[p + 1]]) as usize;
        p += 2;
        let recs_start = p;
        if rule_bytes.len() < recs_start + lookup_count * 4 {
            continue;
        }
        if !map.spend(bt_count + in_count + la_count + lookup_count) {
            return None;
        }

        // Remap each gid sequence; drop the rule if any required gid
        // dropped (a missing gid in any of the three sequences means
        // the rule could never match).
        let mut new_bt: Vec<u16> = Vec::with_capacity(bt_count);
        let mut all_kept = true;
        for j in 0..bt_count {
            let off = backtrack_start + j * 2;
            let g_old = u16::from_be_bytes([rule_bytes[off], rule_bytes[off + 1]]);
            match map.map(g_old) {
                Some(g_new) => new_bt.push(g_new),
                None => {
                    all_kept = false;
                    break;
                }
            }
        }
        if !all_kept {
            continue;
        }
        let mut new_in: Vec<u16> = Vec::with_capacity(in_tail);
        for j in 0..in_tail {
            let off = input_start + j * 2;
            let g_old = u16::from_be_bytes([rule_bytes[off], rule_bytes[off + 1]]);
            match map.map(g_old) {
                Some(g_new) => new_in.push(g_new),
                None => {
                    all_kept = false;
                    break;
                }
            }
        }
        if !all_kept {
            continue;
        }
        let mut new_la: Vec<u16> = Vec::with_capacity(la_count);
        for j in 0..la_count {
            let off = lookahead_start + j * 2;
            let g_old = u16::from_be_bytes([rule_bytes[off], rule_bytes[off + 1]]);
            match map.map(g_old) {
                Some(g_new) => new_la.push(g_new),
                None => {
                    all_kept = false;
                    break;
                }
            }
        }
        if !all_kept {
            continue;
        }
        let Some(records) = parse_and_remap_lookup_records(
            rule_bytes,
            recs_start,
            lookup_count,
            ctx.lookup_renumber,
        ) else {
            continue;
        };

        // Rule body.
        let mut body = Vec::new();
        body.extend_from_slice(&(new_bt.len() as u16).to_be_bytes());
        for g in &new_bt {
            body.extend_from_slice(&g.to_be_bytes());
        }
        body.extend_from_slice(&(in_count as u16).to_be_bytes());
        for g in &new_in {
            body.extend_from_slice(&g.to_be_bytes());
        }
        body.extend_from_slice(&(new_la.len() as u16).to_be_bytes());
        for g in &new_la {
            body.extend_from_slice(&g.to_be_bytes());
        }
        body.extend_from_slice(&(records.len() as u16).to_be_bytes());
        body.extend_from_slice(&encode_lookup_records(&records));
        surviving_rules.push(body);
    }

    if surviving_rules.is_empty() {
        return None;
    }
    Some(emit_offset_list(ctx, &surviving_rules))
}

/// GSUB type 6 format 2 (class-based chained-context substitution).
///
/// ```text
///   u16      substFormat = 2
///   Offset16 coverageOffset
///   Offset16 backtrackClassDefOffset
///   Offset16 inputClassDefOffset
///   Offset16 lookaheadClassDefOffset
///   u16      chainClassSetCount
///   Offset16 chainClassSetOffsets[chainClassSetCount]
///
///   ChainClassSet:
///     u16      chainClassRuleCount
///     Offset16 chainClassRuleOffsets[chainClassRuleCount]   (set-relative)
///
///   ChainClassRule:
///     u16 backtrackGlyphCount
///     u16 backtrackSequence[count]
///     u16 inputGlyphCount
///     u16 inputSequence[count - 1]
///     u16 lookaheadGlyphCount
///     u16 lookaheadSequence[count]
///     u16 substLookupRecordCount
///     SubstLookupRecord[]
/// ```
fn rewrite_type6_format2(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 12 {
        return None;
    }
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let bt_cd_off = u16::from_be_bytes([sub[4], sub[5]]) as usize;
    let in_cd_off = u16::from_be_bytes([sub[6], sub[7]]) as usize;
    let la_cd_off = u16::from_be_bytes([sub[8], sub[9]]) as usize;
    let set_count = u16::from_be_bytes([sub[10], sub[11]]) as usize;
    let set_offsets_off = 12usize;
    if sub.len() < set_offsets_off + set_count * 2 {
        return None;
    }
    let map = ctx.gid_map;
    let cov_bytes = sub.get(cov_off..)?;
    let covered = ctx.gid_map.coverage_glyphs(cov_bytes)?;
    let new_covered: Vec<u16> = covered.iter().filter_map(|&g| map.map(g)).collect();
    if new_covered.is_empty() {
        return None;
    }

    let bt_pairs_old = ctx.gid_map.classdef_pairs_at(sub, bt_cd_off)?;
    let in_pairs_old = ctx.gid_map.classdef_pairs_at(sub, in_cd_off)?;
    let la_pairs_old = ctx.gid_map.classdef_pairs_at(sub, la_cd_off)?;

    let remap = |pairs: &[(u16, u16)]| -> (Vec<(u16, u16)>, Vec<bool>) {
        let mut new_pairs: Vec<(u16, u16)> = Vec::with_capacity(pairs.len());
        let mut reachable: Vec<bool> = Vec::new();
        for (gid_old, class) in pairs {
            if let Some(gid_new) = map.map(*gid_old) {
                new_pairs.push((gid_new, *class));
                let ci = *class as usize;
                if ci >= reachable.len() {
                    reachable.resize(ci + 1, false);
                }
                reachable[ci] = true;
            }
        }
        if reachable.is_empty() {
            reachable.push(true);
        } else {
            reachable[0] = true;
        }
        (new_pairs, reachable)
    };
    let (bt_pairs_new, bt_reachable) = remap(&bt_pairs_old);
    let (in_pairs_new, in_reachable) = remap(&in_pairs_old);
    let (la_pairs_new, la_reachable) = remap(&la_pairs_old);

    let new_bt_cd = crate::classdef::emit_classdef(&bt_pairs_new);
    let new_in_cd = crate::classdef::emit_classdef(&in_pairs_new);
    let new_la_cd = crate::classdef::emit_classdef(&la_pairs_new);

    let mut surviving_sets: Vec<Option<Vec<u8>>> = Vec::with_capacity(set_count);
    for i in 0..set_count {
        let off_off = set_offsets_off + i * 2;
        let set_off_rel = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        if set_off_rel == 0 {
            surviving_sets.push(None);
            continue;
        }
        if !*in_reachable.get(i).unwrap_or(&false) {
            surviving_sets.push(None);
            continue;
        }
        let Some(set_bytes) = sub.get(set_off_rel..) else {
            surviving_sets.push(None);
            continue;
        };
        surviving_sets.push(rewrite_type6_class_set(
            set_bytes,
            &bt_reachable,
            &in_reachable,
            &la_reachable,
            ctx,
        ));
    }
    if surviving_sets.iter().all(|s| s.is_none()) {
        return None;
    }

    // Emit subtable.
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes());
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    let bt_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    let in_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    let la_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(set_count as u16).to_be_bytes());
    let set_offsets_start = out.len();
    for _ in 0..set_count {
        out.extend_from_slice(&[0u8; 2]);
    }
    let mut bodies = Dedup::default();
    for (i, set_opt) in surviving_sets.iter().enumerate() {
        if let Some(set_body) = set_opt {
            let body_start = bodies.place(&mut out, set_body);
            let slot = set_offsets_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&ctx.off16(body_start).to_be_bytes());
        }
    }
    let cov_off = ctx.off16(out.len());
    out.extend_from_slice(&crate::coverage::emit_coverage_from_glyphs(&new_covered));
    out[cov_slot..cov_slot + 2].copy_from_slice(&cov_off.to_be_bytes());
    let bt_off = ctx.off16(out.len());
    out.extend_from_slice(&new_bt_cd);
    out[bt_slot..bt_slot + 2].copy_from_slice(&bt_off.to_be_bytes());
    let in_off = ctx.off16(out.len());
    out.extend_from_slice(&new_in_cd);
    out[in_slot..in_slot + 2].copy_from_slice(&in_off.to_be_bytes());
    let la_off = ctx.off16(out.len());
    out.extend_from_slice(&new_la_cd);
    out[la_slot..la_slot + 2].copy_from_slice(&la_off.to_be_bytes());
    Some(RewrittenSubtable { bytes: out })
}

fn rewrite_type6_class_set(
    set_bytes: &[u8],
    bt_reachable: &[bool],
    in_reachable: &[bool],
    la_reachable: &[bool],
    ctx: &RewriterCtx,
) -> Option<Vec<u8>> {
    if set_bytes.len() < 2 {
        return None;
    }
    let rule_count = u16::from_be_bytes([set_bytes[0], set_bytes[1]]) as usize;
    if set_bytes.len() < 2 + rule_count * 2 || !ctx.gid_map.spend(rule_count) {
        return None;
    }
    let class_reachable = |reachable: &[bool], c: u16| -> bool {
        c == 0 || reachable.get(c as usize).copied().unwrap_or(false)
    };
    let mut surviving_rules: Vec<Vec<u8>> = Vec::new();
    for i in 0..rule_count {
        let off_off = 2 + i * 2;
        let rule_off = u16::from_be_bytes([set_bytes[off_off], set_bytes[off_off + 1]]) as usize;
        let Some(rule_bytes) = set_bytes.get(rule_off..) else {
            continue;
        };
        if rule_bytes.len() < 2 {
            continue;
        }
        let bt_count = u16::from_be_bytes([rule_bytes[0], rule_bytes[1]]) as usize;
        let mut p = 2 + bt_count * 2;
        if rule_bytes.len() < p + 2 {
            continue;
        }
        let bt_start = 2;
        let in_count = u16::from_be_bytes([rule_bytes[p], rule_bytes[p + 1]]) as usize;
        p += 2;
        let in_tail = in_count.saturating_sub(1);
        let in_start = p;
        p += in_tail * 2;
        if rule_bytes.len() < p + 2 {
            continue;
        }
        let la_count = u16::from_be_bytes([rule_bytes[p], rule_bytes[p + 1]]) as usize;
        p += 2;
        let la_start = p;
        p += la_count * 2;
        if rule_bytes.len() < p + 2 {
            continue;
        }
        let lookup_count = u16::from_be_bytes([rule_bytes[p], rule_bytes[p + 1]]) as usize;
        p += 2;
        let recs_start = p;
        if rule_bytes.len() < recs_start + lookup_count * 4 {
            continue;
        }
        if !ctx
            .gid_map
            .spend(bt_count + in_count + la_count + lookup_count)
        {
            return None;
        }

        let mut all_reachable = true;
        let mut bt_classes: Vec<u16> = Vec::with_capacity(bt_count);
        for j in 0..bt_count {
            let off = bt_start + j * 2;
            let c = u16::from_be_bytes([rule_bytes[off], rule_bytes[off + 1]]);
            if !class_reachable(bt_reachable, c) {
                all_reachable = false;
                break;
            }
            bt_classes.push(c);
        }
        if !all_reachable {
            continue;
        }
        let mut in_classes: Vec<u16> = Vec::with_capacity(in_tail);
        for j in 0..in_tail {
            let off = in_start + j * 2;
            let c = u16::from_be_bytes([rule_bytes[off], rule_bytes[off + 1]]);
            if !class_reachable(in_reachable, c) {
                all_reachable = false;
                break;
            }
            in_classes.push(c);
        }
        if !all_reachable {
            continue;
        }
        let mut la_classes: Vec<u16> = Vec::with_capacity(la_count);
        for j in 0..la_count {
            let off = la_start + j * 2;
            let c = u16::from_be_bytes([rule_bytes[off], rule_bytes[off + 1]]);
            if !class_reachable(la_reachable, c) {
                all_reachable = false;
                break;
            }
            la_classes.push(c);
        }
        if !all_reachable {
            continue;
        }
        let Some(records) = parse_and_remap_lookup_records(
            rule_bytes,
            recs_start,
            lookup_count,
            ctx.lookup_renumber,
        ) else {
            continue;
        };

        let mut body = Vec::new();
        body.extend_from_slice(&(bt_classes.len() as u16).to_be_bytes());
        for c in &bt_classes {
            body.extend_from_slice(&c.to_be_bytes());
        }
        body.extend_from_slice(&(in_count as u16).to_be_bytes());
        for c in &in_classes {
            body.extend_from_slice(&c.to_be_bytes());
        }
        body.extend_from_slice(&(la_classes.len() as u16).to_be_bytes());
        for c in &la_classes {
            body.extend_from_slice(&c.to_be_bytes());
        }
        body.extend_from_slice(&(records.len() as u16).to_be_bytes());
        body.extend_from_slice(&encode_lookup_records(&records));
        surviving_rules.push(body);
    }
    if surviving_rules.is_empty() {
        return None;
    }
    Some(emit_offset_list(ctx, &surviving_rules))
}

/// GSUB type 6 format 3 (coverage-based chained-context substitution).
///
/// ```text
///   u16 substFormat = 3
///   u16 backtrackGlyphCount
///   Offset16 backtrackCoverageOffsets[count]
///   u16 inputGlyphCount
///   Offset16 inputCoverageOffsets[count]
///   u16 lookaheadGlyphCount
///   Offset16 lookaheadCoverageOffsets[count]
///   u16 substLookupRecordCount
///   SubstLookupRecord records[count]
/// ```
fn rewrite_type6_format3(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 4 {
        return None;
    }
    let map = ctx.gid_map;

    // Walk variable-length header.
    let mut p = 2usize;
    let bt_count = u16::from_be_bytes([sub[p], sub[p + 1]]) as usize;
    p += 2;
    let bt_offs_start = p;
    p += bt_count * 2;
    if sub.len() < p + 2 {
        return None;
    }
    let in_count = u16::from_be_bytes([sub[p], sub[p + 1]]) as usize;
    p += 2;
    let in_offs_start = p;
    p += in_count * 2;
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
    let lookup_count = u16::from_be_bytes([sub[p], sub[p + 1]]) as usize;
    p += 2;
    let recs_start = p;
    if sub.len() < recs_start + lookup_count * 4 {
        return None;
    }

    let new_bt = rewrite_coverage_array(map, sub, bt_offs_start, bt_count)?;
    let new_in = rewrite_coverage_array(map, sub, in_offs_start, in_count)?;
    let new_la = rewrite_coverage_array(map, sub, la_offs_start, la_count)?;

    let records =
        parse_and_remap_lookup_records(sub, recs_start, lookup_count, ctx.lookup_renumber)?;

    // Emit:
    //   header bytes
    //   coverageOffsets placeholders for bt / in / la
    //   substLookupRecord[]
    //   coverage bodies tightly packed
    let mut out = Vec::new();
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&(bt_count as u16).to_be_bytes());
    let bt_slots_start = out.len();
    for _ in 0..bt_count {
        out.extend_from_slice(&[0u8; 2]);
    }
    out.extend_from_slice(&(in_count as u16).to_be_bytes());
    let in_slots_start = out.len();
    for _ in 0..in_count {
        out.extend_from_slice(&[0u8; 2]);
    }
    out.extend_from_slice(&(la_count as u16).to_be_bytes());
    let la_slots_start = out.len();
    for _ in 0..la_count {
        out.extend_from_slice(&[0u8; 2]);
    }
    out.extend_from_slice(&(records.len() as u16).to_be_bytes());
    out.extend_from_slice(&encode_lookup_records(&records));

    let mut bodies = Dedup::default();
    let mut patch_array = |slots_start: usize, covs: &[Vec<u8>]| {
        for (i, cov) in covs.iter().enumerate() {
            let body_start = ctx.off16(bodies.place(&mut out, cov));
            let slot = slots_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&body_start.to_be_bytes());
        }
    };
    patch_array(bt_slots_start, &new_bt);
    patch_array(in_slots_start, &new_in);
    patch_array(la_slots_start, &new_la);
    Some(RewrittenSubtable { bytes: out })
}
