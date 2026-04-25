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
//! - **Type 2 (multiple-sub)** — format 1. Filters Coverage to surviving
//!   input gids; drops any Sequence whose substitute glyphs are not all
//!   kept (a partial sequence would emit a missing gid), and drops the
//!   subtable when Coverage empties out.
//! - **Type 3 (alternate-sub)** — format 1. Filters Coverage to surviving
//!   input gids; remaps each AlternateSet's surviving alternates;
//!   drops the AlternateSet (and its Coverage entry) when every
//!   alternate dies, and drops the subtable when Coverage empties out.
//! - **Type 4 (ligature-sub)** — format 1. Filters Coverage to surviving
//!   first-component gids, drops any Ligature whose result gid or any
//!   component gid is not kept, drops empty LigatureSets, and drops the
//!   subtable when Coverage empties out. Result + component gids are
//!   remapped through the GidMap.
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
        gsub_type::MULTIPLE => rewrite_type2(ctx, sub),
        gsub_type::ALTERNATE => rewrite_type3(ctx, sub),
        gsub_type::LIGATURE => rewrite_type4(ctx, sub),
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

/// Rewrites a GSUB type 2 (Multiple Substitution) subtable.
///
/// Format 1 layout:
///
/// ```text
///   u16      substFormat = 1
///   Offset16 coverageOffset
///   u16      sequenceCount
///   Offset16 sequenceOffsets[sequenceCount]
///
///   Sequence:
///     u16 glyphCount
///     u16 substituteGlyphIDs[glyphCount]
/// ```
///
/// Drop rules:
///
/// - A Coverage entry dies if its input gid isn't in the GidMap **or**
///   any substitute glyph in its Sequence isn't kept. A partial
///   substitution would emit a missing gid which has no defined
///   meaning in the new namespace.
/// - The subtable dies when Coverage becomes empty.
fn rewrite_type2(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 6 {
        return None;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    if format != 1 {
        return None;
    }
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let seq_count = u16::from_be_bytes([sub[4], sub[5]]) as usize;
    let seq_offsets_off = 6usize;
    if sub.len() < seq_offsets_off + seq_count * 2 {
        return None;
    }
    let cov_bytes = sub.get(cov_off..)?;
    let covered = parse_coverage_glyphs(cov_bytes);
    let pair_count = covered.len().min(seq_count);

    let map = ctx.gid_map;
    // (new_input_gid, encoded_sequence_bytes) per surviving Coverage entry.
    let mut surviving: Vec<(u16, Vec<u8>)> = Vec::new();

    for (i, &input_old) in covered.iter().enumerate().take(pair_count) {
        let Some(input_new) = map.map(input_old) else {
            continue;
        };
        let off_off = seq_offsets_off + i * 2;
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
        // Every substitute must be kept. A missing output gid would
        // emit a substitution that points at a dropped slot — there's
        // no graceful degrade here, mirror type-4's all-or-nothing
        // ligature drop.
        let mut new_seq: Vec<u16> = Vec::with_capacity(glyph_count);
        let mut all_kept = true;
        for j in 0..glyph_count {
            let off = 2 + j * 2;
            let g_old = u16::from_be_bytes([seq_bytes[off], seq_bytes[off + 1]]);
            match map.map(g_old) {
                Some(g_new) => new_seq.push(g_new),
                None => {
                    all_kept = false;
                    break;
                }
            }
        }
        if !all_kept {
            continue;
        }

        // Encode the rewritten Sequence body.
        let mut body = Vec::with_capacity(2 + new_seq.len() * 2);
        body.extend_from_slice(&(new_seq.len() as u16).to_be_bytes());
        for g in &new_seq {
            body.extend_from_slice(&g.to_be_bytes());
        }
        surviving.push((input_new, body));
    }

    if surviving.is_empty() {
        return None;
    }

    Some(emit_type2_subtable(&surviving))
}

/// Encodes a complete MultipleSubstFormat1 subtable around already-
/// rewritten `(new_input_gid, sequence_bytes)` pairs.
fn emit_type2_subtable(surviving: &[(u16, Vec<u8>)]) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // substFormat
    let cov_off_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
    out.extend_from_slice(&(surviving.len() as u16).to_be_bytes()); // sequenceCount
    let seq_offsets_start = out.len();
    for _ in 0..surviving.len() {
        out.extend_from_slice(&[0u8; 2]); // sequenceOffset placeholder
    }
    for (i, (_input_gid, seq_body)) in surviving.iter().enumerate() {
        let body_start = out.len();
        out.extend_from_slice(seq_body);
        let slot = seq_offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
    }
    let pairs: Vec<(u16, u16)> = surviving
        .iter()
        .enumerate()
        .map(|(i, (g, _))| (*g, i as u16))
        .collect();
    let cov_bytes = emit_coverage_from_pairs(&pairs);
    let cov_off = out.len() as u16;
    out.extend_from_slice(&cov_bytes);
    out[cov_off_slot..cov_off_slot + 2].copy_from_slice(&cov_off.to_be_bytes());
    RewrittenSubtable { bytes: out }
}

/// Rewrites a GSUB type 3 (Alternate Substitution) subtable.
///
/// Format 1 layout:
///
/// ```text
///   u16      substFormat = 1
///   Offset16 coverageOffset
///   u16      alternateSetCount
///   Offset16 alternateSetOffsets[alternateSetCount]
///
///   AlternateSet:
///     u16 glyphCount
///     u16 alternateGlyphIDs[glyphCount]
/// ```
///
/// Drop rules — looser than type 2 because alternates are user-chosen,
/// so dropping individual entries doesn't break the meaning of the
/// substitution as a whole:
///
/// - Each AlternateSet keeps only the alternates whose gids survived
///   the GidMap (and renumbers them).
/// - A Coverage entry dies if its input gid isn't kept **or** every
///   alternate in its AlternateSet was dropped (an empty AlternateSet
///   isn't useful — fall through to the input glyph rather than emit
///   a degenerate set).
/// - The subtable dies when Coverage empties out.
fn rewrite_type3(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 6 {
        return None;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    if format != 1 {
        return None;
    }
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let alt_set_count = u16::from_be_bytes([sub[4], sub[5]]) as usize;
    let alt_offsets_off = 6usize;
    if sub.len() < alt_offsets_off + alt_set_count * 2 {
        return None;
    }
    let cov_bytes = sub.get(cov_off..)?;
    let covered = parse_coverage_glyphs(cov_bytes);
    let pair_count = covered.len().min(alt_set_count);

    let map = ctx.gid_map;
    // (new_input_gid, encoded_alternate_set_bytes) per surviving Coverage entry.
    let mut surviving: Vec<(u16, Vec<u8>)> = Vec::new();

    for (i, &input_old) in covered.iter().enumerate().take(pair_count) {
        let Some(input_new) = map.map(input_old) else {
            continue;
        };
        let off_off = alt_offsets_off + i * 2;
        let alt_off = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        let Some(alt_bytes) = sub.get(alt_off..) else {
            continue;
        };
        if alt_bytes.len() < 2 {
            continue;
        }
        let glyph_count = u16::from_be_bytes([alt_bytes[0], alt_bytes[1]]) as usize;
        let need = 2 + glyph_count * 2;
        if alt_bytes.len() < need {
            continue;
        }
        // Filter alternates to those that survive; remap survivors.
        let mut new_alts: Vec<u16> = Vec::with_capacity(glyph_count);
        for j in 0..glyph_count {
            let off = 2 + j * 2;
            let g_old = u16::from_be_bytes([alt_bytes[off], alt_bytes[off + 1]]);
            if let Some(g_new) = map.map(g_old) {
                new_alts.push(g_new);
            }
        }
        // Empty AlternateSet means every alternate dropped — drop the
        // whole Coverage entry. The fall-through is the input glyph
        // unchanged, which is shaping's default behaviour anyway.
        if new_alts.is_empty() {
            continue;
        }
        let mut body = Vec::with_capacity(2 + new_alts.len() * 2);
        body.extend_from_slice(&(new_alts.len() as u16).to_be_bytes());
        for g in &new_alts {
            body.extend_from_slice(&g.to_be_bytes());
        }
        surviving.push((input_new, body));
    }

    if surviving.is_empty() {
        return None;
    }

    Some(emit_type3_subtable(&surviving))
}

/// Encodes a complete AlternateSubstFormat1 subtable around already-
/// rewritten `(new_input_gid, alt_set_bytes)` pairs.
fn emit_type3_subtable(surviving: &[(u16, Vec<u8>)]) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // substFormat
    let cov_off_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
    out.extend_from_slice(&(surviving.len() as u16).to_be_bytes()); // alternateSetCount
    let alt_offsets_start = out.len();
    for _ in 0..surviving.len() {
        out.extend_from_slice(&[0u8; 2]); // alternateSetOffset placeholder
    }
    for (i, (_input_gid, alt_body)) in surviving.iter().enumerate() {
        let body_start = out.len();
        out.extend_from_slice(alt_body);
        let slot = alt_offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
    }
    let pairs: Vec<(u16, u16)> = surviving
        .iter()
        .enumerate()
        .map(|(i, (g, _))| (*g, i as u16))
        .collect();
    let cov_bytes = emit_coverage_from_pairs(&pairs);
    let cov_off = out.len() as u16;
    out.extend_from_slice(&cov_bytes);
    out[cov_off_slot..cov_off_slot + 2].copy_from_slice(&cov_off.to_be_bytes());
    RewrittenSubtable { bytes: out }
}

/// Rewrites a GSUB type 4 (Ligature Substitution) subtable.
///
/// The byte layout (format 1, the only format the spec defines):
///
/// ```text
///   u16      substFormat = 1
///   Offset16 coverageOffset           (first component glyphs)
///   u16      ligatureSetCount         (== Coverage entry count)
///   Offset16 ligatureSetOffsets[ligatureSetCount]
///
///   LigatureSet:
///     u16      ligatureCount
///     Offset16 ligatureOffsets[ligatureCount]
///
///   Ligature:
///     u16 ligatureGlyph
///     u16 componentCount             (including the first / coverage one)
///     u16 componentGlyphIDs[componentCount - 1]
/// ```
///
/// Drop rules — every condition collapses the affected scope, never a
/// silent rewrite:
///
/// - A Ligature dies if its `ligatureGlyph` is not in the GidMap **or**
///   any tail component is not in the GidMap. There is no graceful
///   degrade: a single missing component would change which input
///   sequences match, breaking shaping correctness.
/// - A LigatureSet dies if every Ligature inside it died.
/// - A Coverage entry dies if its corresponding LigatureSet died **or**
///   its first-component gid is not in the GidMap.
/// - The whole subtable dies when Coverage empties out.
///
/// Survivors get their `ligatureGlyph` and `componentGlyphIDs` rewritten
/// through the GidMap; LigatureSets are re-emitted with offsets pointing
/// at the new bodies.
fn rewrite_type4(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 6 {
        return None;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    if format != 1 {
        return None;
    }
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let set_count = u16::from_be_bytes([sub[4], sub[5]]) as usize;
    let set_offsets_off = 6usize;
    if sub.len() < set_offsets_off + set_count * 2 {
        return None;
    }
    let cov_bytes = sub.get(cov_off..)?;
    let first_components = parse_coverage_glyphs(cov_bytes);
    // Spec requires Coverage entry count == ligatureSetCount; tolerate
    // a malformed source by capping at the smaller of the two.
    let pair_count = first_components.len().min(set_count);

    let map = ctx.gid_map;
    // (new_first_gid, encoded_ligature_set_bytes) for every surviving
    // Coverage entry. Order is preserved as we iterate so we can hand
    // (gid, index) pairs to `emit_coverage_from_pairs` afterwards.
    let mut surviving_sets: Vec<(u16, Vec<u8>)> = Vec::new();

    for (i, &first_old) in first_components.iter().enumerate().take(pair_count) {
        let Some(first_new) = map.map(first_old) else {
            // First component dropped — the whole LigatureSet goes with
            // it; shaping the input sequence with first_old absent can't
            // fire any of these ligatures anyway.
            continue;
        };
        let off_off = set_offsets_off + i * 2;
        let set_off = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        let Some(set_bytes) = sub.get(set_off..) else {
            continue;
        };
        let Some(rewritten_set) = rewrite_ligature_set(set_bytes, map) else {
            continue;
        };
        surviving_sets.push((first_new, rewritten_set));
    }

    if surviving_sets.is_empty() {
        return None;
    }

    Some(emit_type4_subtable(&surviving_sets))
}

/// Rewrites a single LigatureSet. Returns `None` when every ligature in
/// the set drops (caller propagates that to "Coverage entry dies").
fn rewrite_ligature_set(set_bytes: &[u8], map: &crate::layout::GidMap) -> Option<Vec<u8>> {
    if set_bytes.len() < 2 {
        return None;
    }
    let lig_count = u16::from_be_bytes([set_bytes[0], set_bytes[1]]) as usize;
    if set_bytes.len() < 2 + lig_count * 2 {
        return None;
    }
    // Each surviving ligature: (new ligatureGlyph, new componentCount,
    // new tail componentGlyphIDs).
    let mut survivors: Vec<(u16, u16, Vec<u16>)> = Vec::new();

    for i in 0..lig_count {
        let off_off = 2 + i * 2;
        let lig_off = u16::from_be_bytes([set_bytes[off_off], set_bytes[off_off + 1]]) as usize;
        let Some(lig_bytes) = set_bytes.get(lig_off..) else {
            continue;
        };
        if lig_bytes.len() < 4 {
            continue;
        }
        let lig_glyph_old = u16::from_be_bytes([lig_bytes[0], lig_bytes[1]]);
        let component_count = u16::from_be_bytes([lig_bytes[2], lig_bytes[3]]);
        if component_count == 0 {
            continue;
        }
        let tail = (component_count - 1) as usize;
        let need = 4 + tail * 2;
        if lig_bytes.len() < need {
            continue;
        }
        // Result gid must survive — otherwise the substitution has
        // nowhere to go.
        let Some(lig_glyph_new) = map.map(lig_glyph_old) else {
            continue;
        };
        // Every tail component must survive — a single missing piece
        // changes which input sequences match. Drop the whole ligature.
        let mut new_tail: Vec<u16> = Vec::with_capacity(tail);
        let mut all_kept = true;
        for j in 0..tail {
            let off = 4 + j * 2;
            let g_old = u16::from_be_bytes([lig_bytes[off], lig_bytes[off + 1]]);
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
        survivors.push((lig_glyph_new, component_count, new_tail));
    }

    if survivors.is_empty() {
        return None;
    }

    // Encode the LigatureSet:
    //   u16 ligatureCount
    //   Offset16 ligatureOffsets[ligatureCount]
    //   Ligature[] bodies (tightly packed in the same order)
    let mut out = Vec::new();
    out.extend_from_slice(&(survivors.len() as u16).to_be_bytes());
    let offsets_start = out.len();
    for _ in 0..survivors.len() {
        out.extend_from_slice(&[0u8; 2]); // placeholder
    }
    for (i, (lig_glyph, component_count, tail)) in survivors.iter().enumerate() {
        let body_start = out.len();
        out.extend_from_slice(&lig_glyph.to_be_bytes());
        out.extend_from_slice(&component_count.to_be_bytes());
        for c in tail {
            out.extend_from_slice(&c.to_be_bytes());
        }
        let slot = offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
    }
    Some(out)
}

/// Encodes a complete LigatureSubst format-1 subtable around the
/// already-rewritten `(first_gid_new, ligature_set_bytes)` pairs.
///
/// Layout we emit:
///   - 6-byte header (format, coverageOffset placeholder, setCount)
///   - LigatureSet offsets array (one Offset16 per surviving entry)
///   - LigatureSet bodies tightly packed in input order
///   - Coverage table appended last, its offset patched into the header
fn emit_type4_subtable(surviving: &[(u16, Vec<u8>)]) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // substFormat
    let cov_off_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
    out.extend_from_slice(&(surviving.len() as u16).to_be_bytes()); // ligatureSetCount
    let set_offsets_start = out.len();
    for _ in 0..surviving.len() {
        out.extend_from_slice(&[0u8; 2]); // ligatureSetOffset placeholder
    }

    // LigatureSet bodies, in iteration order so Coverage indices match.
    for (i, (_first_gid, set_body)) in surviving.iter().enumerate() {
        let body_start = out.len();
        out.extend_from_slice(set_body);
        let slot = set_offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
    }

    // Coverage. Pair every kept first-gid with its index in the
    // ligatureSetOffsets array — emit_coverage_from_pairs sorts by gid
    // and falls back to format 2 when those indices aren't a 0..N
    // sequence after sorting.
    let pairs: Vec<(u16, u16)> = surviving
        .iter()
        .enumerate()
        .map(|(i, (g, _))| (*g, i as u16))
        .collect();
    let cov_bytes = emit_coverage_from_pairs(&pairs);
    let cov_off = out.len() as u16;
    out.extend_from_slice(&cov_bytes);
    out[cov_off_slot..cov_off_slot + 2].copy_from_slice(&cov_off.to_be_bytes());

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

    // ===== GSUB type 4 (Ligature Substitution) rewriter tests =====

    fn build_ligature(ligature_glyph: u16, tail_components: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&ligature_glyph.to_be_bytes());
        out.extend_from_slice(&((tail_components.len() + 1) as u16).to_be_bytes());
        for c in tail_components {
            out.extend_from_slice(&c.to_be_bytes());
        }
        out
    }

    fn build_ligature_set(ligatures: &[(u16, Vec<u16>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(ligatures.len() as u16).to_be_bytes());
        let offsets_start = out.len();
        for _ in 0..ligatures.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        for (i, (lg, tail)) in ligatures.iter().enumerate() {
            let body_start = out.len();
            out.extend_from_slice(&build_ligature(*lg, tail));
            let slot = offsets_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
        }
        out
    }

    /// Builds a type-4 subtable; `sets` is `(first_gid, [(out, tail)])`.
    #[allow(clippy::type_complexity)]
    fn build_type4_subtable(sets: &[(u16, Vec<(u16, Vec<u16>)>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        let cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // coverage offset placeholder
        out.extend_from_slice(&(sets.len() as u16).to_be_bytes());
        let set_offsets_start = out.len();
        for _ in 0..sets.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        for (i, (_first, ligs)) in sets.iter().enumerate() {
            let body_start = out.len();
            out.extend_from_slice(&build_ligature_set(ligs));
            let slot = set_offsets_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
        }
        let cov_start = out.len();
        let first_glyphs: Vec<u16> = sets.iter().map(|(f, _)| *f).collect();
        out.extend_from_slice(&build_coverage_format1(&first_glyphs));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    #[test]
    fn rewrite_type4_keeps_all_when_every_gid_survives() {
        // f=10, i=20 → fi=100. Every gid is kept and renumbered down by
        // 1: 10→9, 20→19, 100→99.
        let bytes = build_type4_subtable(&[(10, vec![(100, vec![20])])]);
        let map = map_from_pairs(&[(0, 0), (10, 9), (20, 19), (100, 99)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_type4(&ctx, &bytes).unwrap();

        // Round-trip through the parser to validate semantics.
        let parsed = sigilbuzz::tables::gsub::Ligature::parse(&rs.bytes).unwrap();
        let (out_gid, span) = parsed.apply(&[9, 19, 30]).unwrap();
        assert_eq!(out_gid, 99);
        assert_eq!(span, 2);
    }

    #[test]
    fn rewrite_type4_drops_ligature_when_result_gid_drops() {
        // f=10, i=20 → fi=100; the result gid 100 is not in the map, so
        // the ligature must die. Coverage must lose the entry too — no
        // surviving LigatureSet anchors it.
        let bytes = build_type4_subtable(&[(10, vec![(100, vec![20])])]);
        let map = map_from_pairs(&[(0, 0), (10, 9), (20, 19)]);
        let ctx = RewriterCtx { gid_map: &map };
        // Only one Ligature in one LigatureSet; that ligature dies, so
        // the LigatureSet is empty, the Coverage entry dies, the
        // Coverage empties, the subtable dies.
        assert!(rewrite_type4(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type4_drops_ligature_when_component_drops() {
        // 10 + 20 + 30 → 100; component 20 dropped → entire ligature
        // dies (single missing component kills the rule).
        let bytes = build_type4_subtable(&[(10, vec![(100, vec![20, 30])])]);
        let map = map_from_pairs(&[(0, 0), (10, 9), (30, 29), (100, 99)]);
        let ctx = RewriterCtx { gid_map: &map };
        assert!(rewrite_type4(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type4_partial_ligature_set_survives() {
        // First-component=10 has two ligatures: (10+20→100) and
        // (10+30→200). Drop component 30 → second ligature dies, first
        // survives. LigatureSet stays, Coverage entry stays.
        let bytes = build_type4_subtable(&[(10, vec![(100, vec![20]), (200, vec![30])])]);
        let map = map_from_pairs(&[
            (0, 0),
            (10, 9),
            (20, 19),
            (100, 99),
            (200, 199), // 200 stays mapped, but its tail (30) is dropped
        ]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_type4(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Ligature::parse(&rs.bytes).unwrap();
        // 9, 19 still fires.
        let (out_gid, span) = parsed.apply(&[9, 19]).unwrap();
        assert_eq!(out_gid, 99);
        assert_eq!(span, 2);
        // The 30-component ligature is gone; matching 9, then anything
        // other than 19, must miss.
        assert!(parsed.apply(&[9, 200]).is_none());
    }

    #[test]
    fn rewrite_type4_drops_subtable_when_first_component_drops() {
        // Two LigatureSets (first-components 10 and 40), but only the
        // 40-set has a survivable ligature. Dropping all of 10's
        // ligatures (output 100 dropped) collapses that Coverage entry;
        // dropping 40 itself collapses the second.
        let bytes =
            build_type4_subtable(&[(10, vec![(100, vec![20])]), (40, vec![(200, vec![50])])]);
        // Map keeps everything *except* 10 (first comp drops) and 100
        // (output of the only 10-ligature drops). 40 + 50 + 200 stay.
        let map = map_from_pairs(&[(0, 0), (40, 39), (50, 49), (200, 199), (20, 19)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_type4(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Ligature::parse(&rs.bytes).unwrap();
        // The 40-rooted ligature still fires.
        let (out_gid, span) = parsed.apply(&[39, 49]).unwrap();
        assert_eq!(out_gid, 199);
        assert_eq!(span, 2);
        // The 10-rooted ligature is gone — its first component is no
        // longer in Coverage.
        let cov_off = u16::from_be_bytes([rs.bytes[2], rs.bytes[3]]) as usize;
        let cov = CoverageParser::parse(&rs.bytes[cov_off..]).unwrap();
        // The new first-component gid for 40 is 39; 10's new gid would
        // be 9 if it survived, but it didn't.
        assert!(cov.index_of(39).is_some());
        assert!(cov.index_of(9).is_none());
    }

    #[test]
    fn rewrite_type4_returns_none_when_coverage_empties() {
        // Single-set, single-ligature subtable; drop everything.
        let bytes = build_type4_subtable(&[(10, vec![(100, vec![20])])]);
        let map = map_from_pairs(&[(0, 0)]);
        let ctx = RewriterCtx { gid_map: &map };
        assert!(rewrite_type4(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type4_locks_byte_layout_for_kept_all_case() {
        // Lock down the exact byte counts to catch accidental layout
        // regressions. Single-set, single-ligature, identity-ish remap
        // (every gid kept).
        //
        // Expected layout:
        //   header           = 6 bytes (format + cov off + setCount)
        //   ligSetOffsets    = 2 bytes (one set)
        //   LigatureSet body = 2 bytes (ligCount) + 2 bytes (ligOff)
        //                     + 4 bytes (Ligature: lig glyph + cc)
        //                     + 2 bytes (one tail component)
        //                    = 10 bytes
        //   Coverage fmt 1   = 4 bytes header + 2 bytes (one gid)
        //                    = 6 bytes
        //   Total            = 6 + 2 + 10 + 6 = 24 bytes
        let bytes = build_type4_subtable(&[(10, vec![(100, vec![20])])]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2), (100, 3)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_type4(&ctx, &bytes).unwrap();
        assert_eq!(
            rs.bytes.len(),
            24,
            "expected 24-byte type-4 subtable, got {}",
            rs.bytes.len()
        );
    }

    #[test]
    fn rewrite_type4_is_byte_deterministic() {
        let bytes = build_type4_subtable(&[
            (10, vec![(100, vec![20]), (101, vec![25])]),
            (40, vec![(200, vec![50])]),
        ]);
        let map = map_from_pairs(&[
            (0, 0),
            (10, 1),
            (20, 2),
            (25, 3),
            (40, 4),
            (50, 5),
            (100, 6),
            (101, 7),
            (200, 8),
        ]);
        let ctx = RewriterCtx { gid_map: &map };
        let a = rewrite_type4(&ctx, &bytes).unwrap();
        let b = rewrite_type4(&ctx, &bytes).unwrap();
        assert_eq!(a.bytes, b.bytes);
    }

    #[test]
    fn rewrite_type4_via_dispatcher() {
        // Ensure the per-type dispatcher routes lookup_type=4 into
        // rewrite_type4 and not a fall-through drop.
        let bytes = build_type4_subtable(&[(10, vec![(100, vec![20])])]);
        let map = map_from_pairs(&[(0, 0), (10, 9), (20, 19), (100, 99)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_subtable(&ctx, gsub_type::LIGATURE, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Ligature::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.apply(&[9, 19]).unwrap(), (99, 2));
    }

    // ===== GSUB type 2 (Multiple Substitution) rewriter tests =====

    /// Builds a type-2 subtable; `entries` is `(input_gid, [substitute_gid])`.
    fn build_type2_subtable(entries: &[(u16, Vec<u16>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        let cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // cov off placeholder
        out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        let seq_offsets_start = out.len();
        for _ in 0..entries.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        for (i, (_input, seq)) in entries.iter().enumerate() {
            let body_start = out.len();
            out.extend_from_slice(&(seq.len() as u16).to_be_bytes());
            for g in seq {
                out.extend_from_slice(&g.to_be_bytes());
            }
            let slot = seq_offsets_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
        }
        let cov_start = out.len();
        let inputs: Vec<u16> = entries.iter().map(|(g, _)| *g).collect();
        out.extend_from_slice(&build_coverage_format1(&inputs));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    #[test]
    fn rewrite_type2_keeps_all_when_every_gid_survives() {
        // Input gid 100 decomposes to [40, 50, 60]. Renumber down by 1.
        let bytes = build_type2_subtable(&[(100, vec![40, 50, 60])]);
        let map = map_from_pairs(&[(0, 0), (40, 39), (50, 49), (60, 59), (100, 99)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_type2(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Multiple::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.apply(99), Some(vec![39, 49, 59]));
        assert!(parsed.apply(99).is_some());
        assert!(parsed.apply(0).is_none());
    }

    #[test]
    fn rewrite_type2_drops_sequence_when_substitute_drops() {
        // 100 → [40, 50, 60] but 50 is dropped. The whole Sequence
        // dies because emitting [40, ?, 60] would point at a missing
        // gid.
        let bytes = build_type2_subtable(&[(100, vec![40, 50, 60])]);
        let map = map_from_pairs(&[(0, 0), (40, 39), (60, 59), (100, 99)]);
        let ctx = RewriterCtx { gid_map: &map };
        // Single-entry subtable; that entry dies → subtable dies.
        assert!(rewrite_type2(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type2_drops_entry_when_input_drops() {
        // Two entries; drop input 100 entirely → first entry vanishes,
        // second entry survives.
        let bytes = build_type2_subtable(&[(100, vec![40, 50]), (200, vec![70])]);
        let map = map_from_pairs(&[
            (0, 0),
            (40, 39),
            (50, 49),
            (70, 69),
            (200, 199),
            // 100 not in the map → its Coverage entry dies.
        ]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_type2(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Multiple::parse(&rs.bytes).unwrap();
        // Surviving entry: input 199 → [69].
        assert_eq!(parsed.apply(199), Some(vec![69]));
        // The dropped entry's input gid (100 was renumbered to nothing)
        // — neither old nor any other gid produces a hit.
        assert!(parsed.apply(99).is_none());
    }

    #[test]
    fn rewrite_type2_returns_none_when_coverage_empties() {
        let bytes = build_type2_subtable(&[(100, vec![40, 50])]);
        let map = map_from_pairs(&[(0, 0)]);
        let ctx = RewriterCtx { gid_map: &map };
        assert!(rewrite_type2(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type2_via_dispatcher() {
        let bytes = build_type2_subtable(&[(100, vec![40, 50])]);
        let map = map_from_pairs(&[(0, 0), (40, 39), (50, 49), (100, 99)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_subtable(&ctx, gsub_type::MULTIPLE, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Multiple::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.apply(99), Some(vec![39, 49]));
    }

    #[test]
    fn rewrite_type2_is_byte_deterministic() {
        let bytes = build_type2_subtable(&[
            (100, vec![40, 50]),
            (200, vec![70, 80, 90]),
            (300, vec![60]),
        ]);
        let map = map_from_pairs(&[
            (0, 0),
            (40, 1),
            (50, 2),
            (60, 3),
            (70, 4),
            (80, 5),
            (90, 6),
            (100, 7),
            (200, 8),
            (300, 9),
        ]);
        let ctx = RewriterCtx { gid_map: &map };
        let a = rewrite_type2(&ctx, &bytes).unwrap();
        let b = rewrite_type2(&ctx, &bytes).unwrap();
        assert_eq!(a.bytes, b.bytes);
    }

    #[test]
    fn pull_multiple_extends_keep_set() {
        // Closure walker: input 100 is kept → every substitute in the
        // sequence gets pulled in.
        let bytes = build_type2_subtable(&[(100, vec![40, 50, 60])]);
        let mut keep = vec![false; 256];
        keep[100] = true;
        let changed = pull_multiple(&bytes, &mut keep);
        assert!(changed);
        assert!(keep[40]);
        assert!(keep[50]);
        assert!(keep[60]);
    }

    #[test]
    fn pull_multiple_no_op_when_input_dropped() {
        let bytes = build_type2_subtable(&[(100, vec![40, 50])]);
        let mut keep = vec![false; 256];
        // 100 not kept → no outputs pulled.
        let changed = pull_multiple(&bytes, &mut keep);
        assert!(!changed);
        assert!(!keep[40]);
        assert!(!keep[50]);
    }

    // ===== GSUB type 3 (Alternate Substitution) rewriter tests =====

    /// Builds a type-3 subtable; `entries` is `(input_gid, [alternate_gid])`.
    fn build_type3_subtable(entries: &[(u16, Vec<u16>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        let cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // cov off placeholder
        out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        let alt_offsets_start = out.len();
        for _ in 0..entries.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        for (i, (_input, alts)) in entries.iter().enumerate() {
            let body_start = out.len();
            out.extend_from_slice(&(alts.len() as u16).to_be_bytes());
            for g in alts {
                out.extend_from_slice(&g.to_be_bytes());
            }
            let slot = alt_offsets_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
        }
        let cov_start = out.len();
        let inputs: Vec<u16> = entries.iter().map(|(g, _)| *g).collect();
        out.extend_from_slice(&build_coverage_format1(&inputs));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    #[test]
    fn rewrite_type3_keeps_all_when_every_gid_survives() {
        // Input 10 has alternates [100, 101, 102].
        let bytes = build_type3_subtable(&[(10, vec![100, 101, 102])]);
        let map =
            map_from_pairs(&[(0, 0), (10, 9), (100, 99), (101, 100), (102, 101)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_type3(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Alternate::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.apply(9, 0), Some(99));
        assert_eq!(parsed.apply(9, 1), Some(100));
        assert_eq!(parsed.apply(9, 2), Some(101));
        assert!(parsed.apply(9, 3).is_none());
    }

    #[test]
    fn rewrite_type3_filters_partial_alternate_set() {
        // Input 10 has alternates [100, 101, 102]; 101 is dropped. The
        // surviving set is [100, 102] (renumbered).
        let bytes = build_type3_subtable(&[(10, vec![100, 101, 102])]);
        let map = map_from_pairs(&[(0, 0), (10, 9), (100, 99), (102, 101)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_type3(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Alternate::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.apply(9, 0), Some(99));
        assert_eq!(parsed.apply(9, 1), Some(101));
        assert!(parsed.apply(9, 2).is_none());
    }

    #[test]
    fn rewrite_type3_drops_entry_when_all_alternates_drop() {
        // Two Coverage entries; the first's alternates all drop, the
        // second survives untouched.
        let bytes = build_type3_subtable(&[(10, vec![100, 101]), (20, vec![200])]);
        let map = map_from_pairs(&[(0, 0), (10, 9), (20, 19), (200, 199)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_type3(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Alternate::parse(&rs.bytes).unwrap();
        // First entry's input (gid 9) is no longer covered.
        assert!(parsed.apply(9, 0).is_none());
        // Second entry survives.
        assert_eq!(parsed.apply(19, 0), Some(199));
    }

    #[test]
    fn rewrite_type3_returns_none_when_coverage_empties() {
        let bytes = build_type3_subtable(&[(10, vec![100, 101])]);
        let map = map_from_pairs(&[(0, 0)]);
        let ctx = RewriterCtx { gid_map: &map };
        assert!(rewrite_type3(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type3_via_dispatcher() {
        let bytes = build_type3_subtable(&[(10, vec![100, 101])]);
        let map = map_from_pairs(&[(0, 0), (10, 9), (100, 99), (101, 100)]);
        let ctx = RewriterCtx { gid_map: &map };
        let rs = rewrite_subtable(&ctx, gsub_type::ALTERNATE, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Alternate::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.apply(9, 0), Some(99));
        assert_eq!(parsed.apply(9, 1), Some(100));
    }

    #[test]
    fn rewrite_type3_is_byte_deterministic() {
        let bytes = build_type3_subtable(&[
            (10, vec![100, 101]),
            (20, vec![200, 201, 202]),
        ]);
        let map = map_from_pairs(&[
            (0, 0),
            (10, 1),
            (20, 2),
            (100, 3),
            (101, 4),
            (200, 5),
            (201, 6),
            (202, 7),
        ]);
        let ctx = RewriterCtx { gid_map: &map };
        let a = rewrite_type3(&ctx, &bytes).unwrap();
        let b = rewrite_type3(&ctx, &bytes).unwrap();
        assert_eq!(a.bytes, b.bytes);
    }

    #[test]
    fn pull_alternate_default_extends_keep_set_with_first_alternate_only() {
        // Closure walker: input 10 is kept → only the *first* alternate
        // (100) gets pulled in. The remaining alternates (101, 102) stay
        // dropped unless the caller requested them explicitly.
        let bytes = build_type3_subtable(&[(10, vec![100, 101, 102])]);
        let mut keep = vec![false; 256];
        keep[10] = true;
        let changed = pull_alternate_default(&bytes, &mut keep);
        assert!(changed);
        assert!(
            keep[100],
            "default alternate (index 0 = gid 100) must be pulled in"
        );
        assert!(
            !keep[101],
            "non-default alternate gid 101 must NOT be auto-pulled"
        );
        assert!(
            !keep[102],
            "non-default alternate gid 102 must NOT be auto-pulled"
        );
    }

    #[test]
    fn pull_alternate_default_no_op_when_input_dropped() {
        let bytes = build_type3_subtable(&[(10, vec![100])]);
        let mut keep = vec![false; 256];
        // 10 not kept → no outputs pulled.
        let changed = pull_alternate_default(&bytes, &mut keep);
        assert!(!changed);
        assert!(!keep[100]);
    }
}
