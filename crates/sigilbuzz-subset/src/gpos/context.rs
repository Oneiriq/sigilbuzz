//! Rewriter for GPOS type 7 (context positioning).

use alloc::vec::Vec;

use crate::device::Dedup;
use crate::layout::{classdef_pairs_at, parse_coverage_glyphs, RewriterCtx, RewrittenSubtable};

// ===== GPOS types 7 / 8: contextual / chained-contextual positioning =====
//
// Structurally identical to GSUB types 5 / 6: the same three formats
// (glyph rule sets, class rule sets, coverage arrays) drive a list of
// `PosLookupRecord` entries that re-enter the GPOS dispatcher on a
// match. Each record is `u16 sequenceIndex, u16 lookupListIndex`; the
// second field is patched on the second pass through
// [`crate::layout::build_gpos`] once the GPOS lookup-list renumber is
// known. See [`context_lookup_type`] for the driver hook.
//
// We share the `PatchedLookupRecord` walker with GSUB: the four-byte
// record layout is identical, only the dispatcher target differs.

use crate::gsub::{encode_lookup_records, parse_and_remap_lookup_records};

/// Rewrites a GPOS type 7 (Context Positioning) subtable. Auto-
/// dispatches on the leading u16 format. Mirrors `rewrite_type5` from
/// the GSUB rewriter. Only the lookup-record dispatcher target
/// differs at runtime, the byte layout is identical.
pub(super) fn rewrite_context_pos(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 2 {
        return None;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    match format {
        1 => rewrite_context_pos_format1(ctx, sub),
        2 => rewrite_context_pos_format2(ctx, sub),
        3 => rewrite_context_pos_format3(ctx, sub),
        _ => None,
    }
}

/// GPOS type 7 format 1 (rule-based contextual positioning).
///
/// ```text
///   u16      posFormat = 1
///   Offset16 coverageOffset
///   u16      posRuleSetCount
///   Offset16 posRuleSetOffsets[posRuleSetCount]
///
///   PosRuleSet:
///     u16      posRuleCount
///     Offset16 posRuleOffsets[posRuleCount]      (set-relative)
///
///   PosRule:
///     u16 inputGlyphCount       (>= 1; first input is implicit in Coverage)
///     u16 posLookupRecordCount
///     u16 inputSequence[inputGlyphCount - 1]
///     PosLookupRecord records[posLookupRecordCount]
/// ```
fn rewrite_context_pos_format1(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
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
    let covered = parse_coverage_glyphs(cov_bytes);
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
        let Some(rewritten_set) = rewrite_context_pos_rule_set(set_bytes, ctx) else {
            continue;
        };
        surviving_sets.push((first_new, rewritten_set));
    }

    if surviving_sets.is_empty() {
        return None;
    }
    Some(emit_context_pos_format1(ctx, &surviving_sets))
}

fn rewrite_context_pos_rule_set(set_bytes: &[u8], ctx: &RewriterCtx) -> Option<Vec<u8>> {
    if set_bytes.len() < 2 {
        return None;
    }
    let rule_count = u16::from_be_bytes([set_bytes[0], set_bytes[1]]) as usize;
    if set_bytes.len() < 2 + rule_count * 2 {
        return None;
    }
    let map = ctx.gid_map;

    let mut surviving_rules: Vec<Vec<u8>> = Vec::new();
    for i in 0..rule_count {
        let off_off = 2 + i * 2;
        let rule_off = u16::from_be_bytes([set_bytes[off_off], set_bytes[off_off + 1]]) as usize;
        let Some(rule_bytes) = set_bytes.get(rule_off..) else {
            continue;
        };
        if rule_bytes.len() < 4 {
            continue;
        }
        let glyph_count = u16::from_be_bytes([rule_bytes[0], rule_bytes[1]]) as usize;
        let lookup_count = u16::from_be_bytes([rule_bytes[2], rule_bytes[3]]) as usize;
        if glyph_count == 0 {
            let recs_off = 4;
            let Some(records) = parse_and_remap_lookup_records(
                rule_bytes,
                recs_off,
                lookup_count,
                ctx.lookup_renumber,
            ) else {
                continue;
            };
            let mut body = Vec::with_capacity(4 + records.len() * 4);
            body.extend_from_slice(&0u16.to_be_bytes());
            body.extend_from_slice(&(records.len() as u16).to_be_bytes());
            body.extend_from_slice(&encode_lookup_records(&records));
            surviving_rules.push(body);
            continue;
        }
        let tail = glyph_count - 1;
        let need = 4 + tail * 2 + lookup_count * 4;
        if rule_bytes.len() < need {
            continue;
        }
        let mut new_tail: Vec<u16> = Vec::with_capacity(tail);
        let mut all_kept = true;
        for j in 0..tail {
            let off = 4 + j * 2;
            let g_old = u16::from_be_bytes([rule_bytes[off], rule_bytes[off + 1]]);
            match map.map(g_old) {
                Some(g_new) => new_tail.push(g_new),
                None => {
                    all_kept = false;
                    break;
                }
            }
        }
        if !all_kept {
            continue;
        }
        let recs_off = 4 + tail * 2;
        let Some(records) =
            parse_and_remap_lookup_records(rule_bytes, recs_off, lookup_count, ctx.lookup_renumber)
        else {
            continue;
        };

        let mut body = Vec::with_capacity(4 + tail * 2 + records.len() * 4);
        body.extend_from_slice(&(glyph_count as u16).to_be_bytes());
        body.extend_from_slice(&(records.len() as u16).to_be_bytes());
        for g in &new_tail {
            body.extend_from_slice(&g.to_be_bytes());
        }
        body.extend_from_slice(&encode_lookup_records(&records));
        surviving_rules.push(body);
    }

    if surviving_rules.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    out.extend_from_slice(&(surviving_rules.len() as u16).to_be_bytes());
    let offsets_start = out.len();
    for _ in 0..surviving_rules.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    let mut bodies = Dedup::default();
    for (i, body) in surviving_rules.iter().enumerate() {
        let body_start = bodies.place(&mut out, body);
        let slot = offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&ctx.off16(body_start).to_be_bytes());
    }
    Some(out)
}

pub(super) fn emit_context_pos_format1(
    ctx: &RewriterCtx,
    surviving: &[(u16, Vec<u8>)],
) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    let cov_off_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverage offset placeholder
    out.extend_from_slice(&(surviving.len() as u16).to_be_bytes()); // ruleSetCount
    let set_offsets_start = out.len();
    for _ in 0..surviving.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    let mut bodies = Dedup::default();
    for (i, (_first, set_body)) in surviving.iter().enumerate() {
        let body_start = bodies.place(&mut out, set_body);
        let slot = set_offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&ctx.off16(body_start).to_be_bytes());
    }
    let pairs: Vec<(u16, u16)> = surviving
        .iter()
        .enumerate()
        .map(|(i, (g, _))| (*g, i as u16))
        .collect();
    let cov_bytes = crate::coverage::emit_coverage_from_pairs(&pairs);
    let cov_off = ctx.off16(out.len());
    out.extend_from_slice(&cov_bytes);
    out[cov_off_slot..cov_off_slot + 2].copy_from_slice(&cov_off.to_be_bytes());
    RewrittenSubtable { bytes: out }
}

/// GPOS type 7 format 2 (class-based contextual positioning).
fn rewrite_context_pos_format2(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 8 {
        return None;
    }
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let cd_off = u16::from_be_bytes([sub[4], sub[5]]) as usize;
    let set_count = u16::from_be_bytes([sub[6], sub[7]]) as usize;
    let set_offsets_off = 8usize;
    if sub.len() < set_offsets_off + set_count * 2 {
        return None;
    }
    let cov_bytes = sub.get(cov_off..)?;
    let covered = parse_coverage_glyphs(cov_bytes);
    let cd_pairs_old = classdef_pairs_at(sub, cd_off)?;
    let map = ctx.gid_map;

    let new_covered: Vec<u16> = covered.iter().filter_map(|&g| map.map(g)).collect();
    if new_covered.is_empty() {
        return None;
    }

    let mut cd_pairs_new: Vec<(u16, u16)> = Vec::with_capacity(cd_pairs_old.len());
    let mut reachable_classes: Vec<bool> = Vec::new();
    for (gid_old, class) in &cd_pairs_old {
        if let Some(gid_new) = map.map(*gid_old) {
            cd_pairs_new.push((gid_new, *class));
            let ci = *class as usize;
            if ci >= reachable_classes.len() {
                reachable_classes.resize(ci + 1, false);
            }
            reachable_classes[ci] = true;
        }
    }
    if reachable_classes.is_empty() {
        reachable_classes.push(true);
    } else {
        reachable_classes[0] = true;
    }
    let new_cd_bytes = crate::classdef::emit_classdef(&cd_pairs_new);

    let mut surviving_sets: Vec<Option<Vec<u8>>> = Vec::with_capacity(set_count);
    for i in 0..set_count {
        let off_off = set_offsets_off + i * 2;
        let set_off_rel = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        if set_off_rel == 0 {
            surviving_sets.push(None);
            continue;
        }
        if !*reachable_classes.get(i).unwrap_or(&false) {
            surviving_sets.push(None);
            continue;
        }
        let Some(set_bytes) = sub.get(set_off_rel..) else {
            surviving_sets.push(None);
            continue;
        };
        surviving_sets.push(rewrite_context_pos_class_set(
            set_bytes,
            &reachable_classes,
            ctx,
        ));
    }
    if surviving_sets.iter().all(|s| s.is_none()) {
        return None;
    }

    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes()); // posFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    let cd_slot = out.len();
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
    let cov_emitted = crate::coverage::emit_coverage_from_glyphs(&new_covered);
    out.extend_from_slice(&cov_emitted);
    out[cov_slot..cov_slot + 2].copy_from_slice(&cov_off.to_be_bytes());
    let cd_off = ctx.off16(out.len());
    out.extend_from_slice(&new_cd_bytes);
    out[cd_slot..cd_slot + 2].copy_from_slice(&cd_off.to_be_bytes());
    Some(RewrittenSubtable { bytes: out })
}

fn rewrite_context_pos_class_set(
    set_bytes: &[u8],
    reachable_classes: &[bool],
    ctx: &RewriterCtx,
) -> Option<Vec<u8>> {
    if set_bytes.len() < 2 {
        return None;
    }
    let rule_count = u16::from_be_bytes([set_bytes[0], set_bytes[1]]) as usize;
    if set_bytes.len() < 2 + rule_count * 2 {
        return None;
    }
    let mut surviving_rules: Vec<Vec<u8>> = Vec::new();
    for i in 0..rule_count {
        let off_off = 2 + i * 2;
        let rule_off = u16::from_be_bytes([set_bytes[off_off], set_bytes[off_off + 1]]) as usize;
        let Some(rule_bytes) = set_bytes.get(rule_off..) else {
            continue;
        };
        if rule_bytes.len() < 4 {
            continue;
        }
        let glyph_count = u16::from_be_bytes([rule_bytes[0], rule_bytes[1]]) as usize;
        let lookup_count = u16::from_be_bytes([rule_bytes[2], rule_bytes[3]]) as usize;
        let tail = glyph_count.saturating_sub(1);
        let need = 4 + tail * 2 + lookup_count * 4;
        if rule_bytes.len() < need {
            continue;
        }
        let mut all_reachable = true;
        let mut new_tail: Vec<u16> = Vec::with_capacity(tail);
        for j in 0..tail {
            let off = 4 + j * 2;
            let c = u16::from_be_bytes([rule_bytes[off], rule_bytes[off + 1]]);
            if !reachable_classes.get(c as usize).copied().unwrap_or(false) && c != 0 {
                all_reachable = false;
                break;
            }
            new_tail.push(c);
        }
        if !all_reachable {
            continue;
        }
        let recs_off = 4 + tail * 2;
        let Some(records) =
            parse_and_remap_lookup_records(rule_bytes, recs_off, lookup_count, ctx.lookup_renumber)
        else {
            continue;
        };
        let mut body = Vec::with_capacity(4 + tail * 2 + records.len() * 4);
        body.extend_from_slice(&(glyph_count as u16).to_be_bytes());
        body.extend_from_slice(&(records.len() as u16).to_be_bytes());
        for c in &new_tail {
            body.extend_from_slice(&c.to_be_bytes());
        }
        body.extend_from_slice(&encode_lookup_records(&records));
        surviving_rules.push(body);
    }
    if surviving_rules.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    out.extend_from_slice(&(surviving_rules.len() as u16).to_be_bytes());
    let offsets_start = out.len();
    for _ in 0..surviving_rules.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    let mut bodies = Dedup::default();
    for (i, body) in surviving_rules.iter().enumerate() {
        let body_start = bodies.place(&mut out, body);
        let slot = offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&ctx.off16(body_start).to_be_bytes());
    }
    Some(out)
}

/// GPOS type 7 format 3 (coverage-based contextual positioning).
///
/// ```text
///   u16 posFormat = 3
///   u16 glyphCount
///   u16 posLookupRecordCount
///   Offset16 coverageOffsets[glyphCount]
///   PosLookupRecord records[posLookupRecordCount]
/// ```
fn rewrite_context_pos_format3(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 6 {
        return None;
    }
    let glyph_count = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let lookup_count = u16::from_be_bytes([sub[4], sub[5]]) as usize;
    let cov_offs_off = 6usize;
    let need = cov_offs_off + glyph_count * 2 + lookup_count * 4;
    if sub.len() < need {
        return None;
    }
    let map = ctx.gid_map;

    let mut new_cov_bytes: Vec<Vec<u8>> = Vec::with_capacity(glyph_count);
    for j in 0..glyph_count {
        let off_off = cov_offs_off + j * 2;
        let cov_off = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        let cov_bytes = sub.get(cov_off..)?;
        let covered = parse_coverage_glyphs(cov_bytes);
        let new_covered: Vec<u16> = covered.iter().filter_map(|&g| map.map(g)).collect();
        if new_covered.is_empty() {
            return None;
        }
        new_cov_bytes.push(crate::coverage::emit_coverage_from_glyphs(&new_covered));
    }

    let recs_off = cov_offs_off + glyph_count * 2;
    let records = parse_and_remap_lookup_records(sub, recs_off, lookup_count, ctx.lookup_renumber)?;

    let mut out = Vec::new();
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&(glyph_count as u16).to_be_bytes());
    out.extend_from_slice(&(records.len() as u16).to_be_bytes());
    let cov_offs_start = out.len();
    for _ in 0..glyph_count {
        out.extend_from_slice(&[0u8; 2]);
    }
    out.extend_from_slice(&encode_lookup_records(&records));
    let mut bodies = Dedup::default();
    for (i, cov) in new_cov_bytes.iter().enumerate() {
        let body_start = ctx.off16(bodies.place(&mut out, cov));
        let slot = cov_offs_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&body_start.to_be_bytes());
    }
    Some(RewrittenSubtable { bytes: out })
}
