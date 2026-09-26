//! Rewriter for GSUB type 5 (context substitution), and the nested
//! lookup-record walker the context rewriters of GSUB and GPOS share.

use alloc::vec::Vec;

use crate::coverage::emit_coverage_from_pairs;
use crate::device::Dedup;
use crate::layout::{parse_coverage_glyphs, RewriterCtx, RewrittenSubtable};

// ===== GSUB types 5 / 6 / 8: contextual / chained / reverse-chain =====
//
// These types wire glyph-stream context into the substitution pipeline.
// All three formats of types 5 and 6 carry `SubstLookupRecord` entries
// whose `lookupListIndex` references a sibling lookup; the rewriter
// preserves those indices on the first pass (the renumber map is not
// known yet) and patches them on a second pass driven by
// [`crate::layout::build_gsub`]. See [`context_lookup_type`] for the
// driver hook.

/// `(sequence_index, lookup_list_index)` rewritten through the GidMap
/// and lookup-renumber.
///
/// `lookup_list_index` is the *original* index when the renumber map
/// is `None` (first pass); on the second pass the caller has
/// populated `lookup_renumber` and dropped indices have been filtered
/// out before reaching this struct.
pub(crate) struct PatchedLookupRecord {
    pub(crate) sequence_index: u16,
    pub(crate) lookup_list_index: u16,
}

/// Walks `count` `SubstLookupRecord` entries from `bytes` starting at
/// `off`. When `lookup_renumber` is `Some`, drops any record whose
/// target lookup is `None` (it did not survive the rewrite) and remaps
/// survivors through the map. Returns the surviving records. Each
/// record is 4 bytes: `u16 sequence_index, u16 lookup_list_index`.
///
/// An empty result does not make the rule disposable. A rule without
/// records is how `ignore sub` / `ignore pos` statements compile: it
/// applies nothing, but once it matches, the shaper moves on without
/// trying the later rules and subtables of the lookup at that
/// position. Dropping it would let those later rules fire where the
/// source font suppressed them, so every caller keeps the rule.
///
/// Shared between GSUB context (types 5 / 6 / 8) and GPOS context
/// (types 7 / 8). `PosLookupRecord` has the same 4-byte layout as
/// `SubstLookupRecord`.
pub(crate) fn parse_and_remap_lookup_records(
    bytes: &[u8],
    off: usize,
    count: usize,
    lookup_renumber: Option<&[Option<u16>]>,
) -> Option<Vec<PatchedLookupRecord>> {
    let need = off + count * 4;
    if bytes.len() < need {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let p = off + i * 4;
        let seq = u16::from_be_bytes([bytes[p], bytes[p + 1]]);
        let li = u16::from_be_bytes([bytes[p + 2], bytes[p + 3]]);
        let new_li = match lookup_renumber {
            Some(map) => match map.get(li as usize) {
                Some(Some(n)) => *n,
                // Dropped target: drop this record only. The rule
                // itself stays, see above.
                _ => continue,
            },
            None => li,
        };
        out.push(PatchedLookupRecord {
            sequence_index: seq,
            lookup_list_index: new_li,
        });
    }
    Some(out)
}

pub(crate) fn encode_lookup_records(records: &[PatchedLookupRecord]) -> Vec<u8> {
    let mut out = Vec::with_capacity(records.len() * 4);
    for r in records {
        out.extend_from_slice(&r.sequence_index.to_be_bytes());
        out.extend_from_slice(&r.lookup_list_index.to_be_bytes());
    }
    out
}

/// Rewrites a GSUB type 5 (Context Substitution) subtable. Auto-
/// dispatches on the leading u16 format.
pub(super) fn rewrite_type5(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 2 {
        return None;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    match format {
        1 => rewrite_type5_format1(ctx, sub),
        2 => rewrite_type5_format2(ctx, sub),
        3 => rewrite_type5_format3(ctx, sub),
        _ => None,
    }
}

/// GSUB type 5 format 1 (rule-based contextual substitution).
///
/// ```text
///   u16      substFormat = 1
///   Offset16 coverageOffset
///   u16      ruleSetCount
///   Offset16 ruleSetOffsets[ruleSetCount]
///
///   RuleSet:
///     u16      ruleCount
///     Offset16 ruleOffsets[ruleCount]      (RuleSet-relative)
///
///   Rule:
///     u16 inputGlyphCount       (>= 1; first input is implicit in Coverage)
///     u16 substLookupRecordCount
///     u16 inputSequence[inputGlyphCount - 1]
///     SubstLookupRecord records[substLookupRecordCount]
/// ```
fn rewrite_type5_format1(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
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

    // (new_first_gid, encoded RuleSet body) per surviving Coverage entry.
    let mut surviving_sets: Vec<(u16, Vec<u8>)> = Vec::new();

    for (i, &first_old) in covered.iter().enumerate().take(pair_count) {
        let Some(first_new) = map.map(first_old) else {
            continue;
        };
        let off_off = set_offsets_off + i * 2;
        let set_off_rel = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        if set_off_rel == 0 {
            // NULL ruleset: drop along with the Coverage entry; an
            // empty ruleset gives no rule for `first_old`, which is the
            // same as not covering it.
            continue;
        }
        let Some(set_bytes) = sub.get(set_off_rel..) else {
            continue;
        };
        let Some(rewritten_set) = rewrite_type5_rule_set(set_bytes, ctx) else {
            continue;
        };
        surviving_sets.push((first_new, rewritten_set));
    }

    if surviving_sets.is_empty() {
        return None;
    }
    Some(emit_context_format1(ctx, &surviving_sets))
}

fn rewrite_type5_rule_set(set_bytes: &[u8], ctx: &RewriterCtx) -> Option<Vec<u8>> {
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
            // Zero-input rule: preserve as-is (parsing tolerates it).
            // Patch nested-lookups only.
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
        // Remap input tail glyph ids; drop the rule if any tail gid
        // dropped. A missing input means the rule could never match
        // in the new namespace anyway.
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
        // A rule left without records still matches and still stops
        // the later rules of this lookup, so it stays (see
        // `parse_and_remap_lookup_records`).

        // Rule body: u16 glyphCount, u16 substLookupRecordCount,
        //            u16 input_tail[count-1], SubstLookupRecord[].
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
    // Emit RuleSet:
    //   u16      ruleCount
    //   Offset16 ruleOffsets[ruleCount]    (RuleSet-relative)
    //   Rule[] bodies
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

/// Emits a Context Substitution Format 1 subtable around the
/// `(new_first_gid, rule_set_bytes)` pairs produced by
/// [`rewrite_type5_rule_set`].
pub(super) fn emit_context_format1(
    ctx: &RewriterCtx,
    surviving: &[(u16, Vec<u8>)],
) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // substFormat
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
    let cov_bytes = emit_coverage_from_pairs(&pairs);
    let cov_off = ctx.off16(out.len());
    out.extend_from_slice(&cov_bytes);
    out[cov_off_slot..cov_off_slot + 2].copy_from_slice(&cov_off.to_be_bytes());
    RewrittenSubtable { bytes: out }
}

/// GSUB type 5 format 2 (class-based contextual substitution).
///
/// ```text
///   u16      substFormat = 2
///   Offset16 coverageOffset
///   Offset16 classDefOffset
///   u16      classSetCount
///   Offset16 classSetOffsets[classSetCount]
///
///   ClassSet:
///     u16      classRuleCount
///     Offset16 classRuleOffsets[classRuleCount]   (ClassSet-relative)
///
///   ClassRule:
///     u16 glyphCount               (>= 1)
///     u16 substLookupRecordCount
///     u16 inputClasses[glyphCount - 1]
///     SubstLookupRecord records[substLookupRecordCount]
/// ```
///
/// Class indices stay numeric. They index the source ClassDef's class
/// enumeration. Re-emitting the source ClassDef with only surviving
/// glyphs (via [`crate::classdef::emit_classdef`]) preserves those
/// numeric class ids; ClassRule indices remain valid as long as they
/// still appear in the rebuilt ClassDef. A ClassSet whose entire input
/// class is no longer reachable (no glyph in that class survived) is
/// dropped.
fn rewrite_type5_format2(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
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
    let cd_pairs_old = crate::layout::classdef_pairs_at(sub, cd_off)?;
    let map = ctx.gid_map;

    // Filter Coverage to surviving first glyphs and rebuild it.
    let new_covered: Vec<u16> = covered.iter().filter_map(|&g| map.map(g)).collect();
    if new_covered.is_empty() {
        return None;
    }

    // Filter ClassDef pairs to surviving glyphs and remap. Track which
    // class ids still have at least one glyph; class indices in
    // ClassRule.input_classes_tail that are no longer reachable cause
    // the rule to die (it could never match in the new namespace).
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
    // Class 0 ("everything else") is reachable iff some surviving
    // glyph isn't otherwise classified. We treat class 0 as always
    // reachable conservatively: a rule referencing class 0 simply
    // means "any other glyph", which is satisfied by .notdef alone.
    if reachable_classes.is_empty() {
        reachable_classes.push(true);
    } else {
        reachable_classes[0] = true;
    }
    let new_cd_bytes = crate::classdef::emit_classdef(&cd_pairs_new);

    // Walk class sets. ClassSet index `i` corresponds to first-glyph
    // class `i`; an unreachable class i means no surviving glyph hits
    // it via Coverage + ClassDef, so its set drops outright.
    let mut surviving_sets: Vec<Option<Vec<u8>>> = Vec::with_capacity(set_count);
    for i in 0..set_count {
        let off_off = set_offsets_off + i * 2;
        let set_off_rel = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        if set_off_rel == 0 {
            surviving_sets.push(None);
            continue;
        }
        // The class itself must be reachable for any rule under it to
        // ever fire.
        if !*reachable_classes.get(i).unwrap_or(&false) {
            surviving_sets.push(None);
            continue;
        }
        let Some(set_bytes) = sub.get(set_off_rel..) else {
            surviving_sets.push(None);
            continue;
        };
        surviving_sets.push(rewrite_type5_class_set(set_bytes, &reachable_classes, ctx));
    }
    if surviving_sets.iter().all(|s| s.is_none()) {
        return None;
    }

    // Emit subtable.
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes()); // substFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset
    let cd_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // classDefOffset
    out.extend_from_slice(&(set_count as u16).to_be_bytes()); // classSetCount
    let set_offsets_start = out.len();
    for _ in 0..set_count {
        out.extend_from_slice(&[0u8; 2]); // placeholder
    }
    let mut bodies = Dedup::default();
    for (i, set_opt) in surviving_sets.iter().enumerate() {
        if let Some(set_body) = set_opt {
            let body_start = bodies.place(&mut out, set_body);
            let slot = set_offsets_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&ctx.off16(body_start).to_be_bytes());
        }
        // Else leave the slot at zero (NULL ClassSet).
    }
    // Coverage.
    let cov_off = ctx.off16(out.len());
    let cov_emitted = crate::coverage::emit_coverage_from_glyphs(&new_covered);
    out.extend_from_slice(&cov_emitted);
    out[cov_slot..cov_slot + 2].copy_from_slice(&cov_off.to_be_bytes());
    // ClassDef.
    let cd_off = ctx.off16(out.len());
    out.extend_from_slice(&new_cd_bytes);
    out[cd_slot..cd_slot + 2].copy_from_slice(&cd_off.to_be_bytes());
    Some(RewrittenSubtable { bytes: out })
}

fn rewrite_type5_class_set(
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
        // Every input class must remain reachable.
        let mut all_reachable = true;
        let mut new_tail: Vec<u16> = Vec::with_capacity(tail);
        for j in 0..tail {
            let off = 4 + j * 2;
            let c = u16::from_be_bytes([rule_bytes[off], rule_bytes[off + 1]]);
            if !reachable_classes.get(c as usize).copied().unwrap_or(false) {
                // Special-case class 0: always treated as reachable.
                if c != 0 {
                    all_reachable = false;
                    break;
                }
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

/// GSUB type 5 format 3 (coverage-based contextual substitution).
///
/// ```text
///   u16 substFormat = 3
///   u16 glyphCount
///   u16 substLookupRecordCount
///   Offset16 coverageOffsets[glyphCount]    (subtable-relative)
///   SubstLookupRecord records[substLookupRecordCount]
/// ```
fn rewrite_type5_format3(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
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

    // Rewrite each input Coverage. If any becomes empty, drop the whole
    // subtable. A context rule with no possible match for one position
    // can't fire.
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

    // Emit:
    //   header (6 bytes)
    //   coverageOffsets[glyph_count]  (placeholders, patched in)
    //   substLookupRecord[record_count]
    //   coverage bodies (in iteration order)
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
