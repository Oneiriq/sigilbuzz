//! GSUB byte-level rewriter.
//!
//! Walks every lookup in the source `GSUB` table and produces a new
//! `GSUB` whose Coverage / ClassDef / substitution-target gid
//! references resolve against the new gid namespace defined by the
//! caller's [`GidMap`](crate::layout::GidMap).
//!
//! # Per-lookup-type coverage
//!
//! As of this commit the rewriter ships byte-level support for:
//!
//! - **Type 1 (single-sub)**: formats 1 (delta) and 2 (explicit). Auto-
//!   selects between formats; falls back to format 2 when a remapped
//!   delta would no longer produce contiguous targets.
//! - **Type 2 (multiple-sub)**: format 1. Filters Coverage to surviving
//!   input gids; drops any Sequence whose substitute glyphs are not all
//!   kept (a partial sequence would emit a missing gid), and drops the
//!   subtable when Coverage empties out.
//! - **Type 3 (alternate-sub)**: format 1. Filters Coverage to surviving
//!   input gids; remaps each AlternateSet's surviving alternates;
//!   drops the AlternateSet (and its Coverage entry) when every
//!   alternate dies, and drops the subtable when Coverage empties out.
//! - **Type 4 (ligature-sub)**: format 1. Filters Coverage to surviving
//!   first-component gids, drops any Ligature whose result gid or any
//!   component gid is not kept, drops empty LigatureSets, and drops the
//!   subtable when Coverage empties out. Result + component gids are
//!   remapped through the GidMap.
//! - **Type 5 (context)**: formats 1 (rule-based), 2 (class-based), 3
//!   (coverage-based). Filters first-glyph Coverage / ClassDef / per-
//!   position Coverage through the GidMap; drops any Rule whose input
//!   tail loses a gid; drops nested `SubstLookupRecord`s whose target
//!   lookup dropped (renumber map is wired in by the GSUB driver in a
//!   second pass, see [`context_lookup_type`]). A Rule that ends up
//!   with no records is kept: it is an `ignore sub` rule, or behaves
//!   like one, and still shields the rules after it.
//! - **Type 6 (chained context)**: formats 1, 2, 3. Mirrors type 5
//!   with three sequences (backtrack / input / lookahead). Drops a
//!   ChainRule when any required gid in any of the three sequences is
//!   not kept.
//! - **Type 7 (extension)**: pass-through after rewriting the inner
//!   subtable. Only inner types this module implements are passed
//!   through; everything else drops the lookup.
//! - **Type 8 (reverse chain)**: format 1. Filters Coverage to
//!   surviving input gids whose substitute gid also survives; rewrites
//!   the backtrack and lookahead Coverage arrays; drops the subtable
//!   when any Coverage in the context window empties out. The closure
//!   keeps the substitute of every kept input whose context can still
//!   match (see [`pull_in_substitution_targets`]), so a pair only
//!   loses its substitute when its subtable drops anyway.
//!
//! Every other lookup type drops its lookup. The drop cascade then
//! removes empty subtables, lookups with no surviving subtable,
//! features that name no surviving lookup, and scripts whose features
//! have all been dropped. See [`super::layout`].
//!
//! Issue tracking the remaining lookup types: see the sibling issue
//! filed alongside this module.

use alloc::vec::Vec;

use sigilbuzz::tables::gsub::lookup_type as gsub_type;

use crate::coverage::emit_coverage_from_pairs;
use crate::device::Dedup;
use crate::layout::{parse_coverage_glyphs, RewriterCtx, RewrittenLookup, RewrittenSubtable};
use crate::SubsetError;

/// Rewrites a single GSUB lookup. Returns `None` if the lookup has no
/// surviving subtables after rewriting (drop cascade will remove the
/// lookup), and an error when a rebuilt subtable outgrows its 16-bit
/// offsets (see [`crate::offset16`]).
pub(crate) fn rewrite_lookup(
    ctx: &RewriterCtx,
    lookup_type: u16,
    lookup_flag: u16,
    mark_filtering_set: Option<u16>,
    subtable_bodies: &[&[u8]],
) -> Result<Option<RewrittenLookup>, SubsetError> {
    let mut rewritten_subs: Vec<RewrittenSubtable> = Vec::new();

    for &sub_bytes in subtable_bodies {
        let rewritten = rewrite_subtable(ctx, lookup_type, sub_bytes);
        ctx.offsets
            .check(overflow_context(lookup_type, sub_bytes))?;
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

/// Names the subtable type an Offset16 overflow is reported against,
/// looking through an Extension wrapper.
fn overflow_context(lookup_type: u16, sub: &[u8]) -> &'static str {
    let kind = match (lookup_type, sub.get(2..4)) {
        (gsub_type::EXTENSION, Some(inner)) => u16::from_be_bytes([inner[0], inner[1]]),
        _ => lookup_type,
    };
    match kind {
        gsub_type::SINGLE => "GSUB SingleSubst rewrite: an offset exceeds 64 KiB",
        gsub_type::MULTIPLE => "GSUB MultipleSubst rewrite: an offset exceeds 64 KiB",
        gsub_type::ALTERNATE => "GSUB AlternateSubst rewrite: an offset exceeds 64 KiB",
        gsub_type::LIGATURE => "GSUB LigatureSubst rewrite: an offset exceeds 64 KiB",
        gsub_type::CONTEXT => "GSUB ContextSubst rewrite: an offset exceeds 64 KiB",
        gsub_type::CHAINED_CONTEXT => "GSUB ChainContextSubst rewrite: an offset exceeds 64 KiB",
        _ => "GSUB ReverseChainSingleSubst rewrite: an offset exceeds 64 KiB",
    }
}

fn rewrite_subtable(ctx: &RewriterCtx, lookup_type: u16, sub: &[u8]) -> Option<RewrittenSubtable> {
    match lookup_type {
        gsub_type::SINGLE => rewrite_single(ctx, sub),
        gsub_type::MULTIPLE => rewrite_type2(ctx, sub),
        gsub_type::ALTERNATE => rewrite_type3(ctx, sub),
        gsub_type::LIGATURE => rewrite_type4(ctx, sub),
        gsub_type::CONTEXT => rewrite_type5(ctx, sub),
        gsub_type::CHAINED_CONTEXT => rewrite_type6(ctx, sub),
        gsub_type::EXTENSION => rewrite_extension(ctx, sub),
        gsub_type::REVERSE_CHAINED => rewrite_type8(ctx, sub),
        // Other types drop until their byte-level rewriter ships.
        // The drop cascade in [`crate::layout`] handles propagating
        // the loss up through lookups / features / scripts.
        _ => None,
    }
}

/// Returns the effective GSUB lookup type when the lookup is a
/// context-family type (5, 6, or 8), including the case where it is
/// wrapped in an Extension lookup (type 7). Returns `None` otherwise.
///
/// Used by the GSUB driver to identify lookups whose nested
/// `SubstLookupRecord` indices need to be patched once the renumber
/// map is known.
pub(crate) fn context_lookup_type(lookup: &sigilbuzz::tables::layout::Lookup<'_>) -> Option<u16> {
    let lt = unwrap_extension_lookup_type(lookup);
    match lt {
        gsub_type::CONTEXT | gsub_type::CHAINED_CONTEXT | gsub_type::REVERSE_CHAINED => Some(lt),
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

    // Build the (input -> output) pairs in the *old* gid namespace
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

    Some(emit_single_subtable(ctx, &new_pairs))
}

/// Encodes a single-sub subtable, picking format 1 vs format 2 by
/// byte size. Coverage is emitted directly into the subtable body so
/// callers don't need to track sub-offsets.
fn emit_single_subtable(ctx: &RewriterCtx, pairs: &[(u16, u16)]) -> RewrittenSubtable {
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
        let cov_off = ctx.off16(out.len());
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
        let cov_off = ctx.off16(out.len());
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
        // emit a substitution that points at a dropped slot: there's
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

    Some(emit_type2_subtable(ctx, &surviving))
}

/// Encodes a complete MultipleSubstFormat1 subtable around already-
/// rewritten `(new_input_gid, sequence_bytes)` pairs.
fn emit_type2_subtable(ctx: &RewriterCtx, surviving: &[(u16, Vec<u8>)]) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // substFormat
    let cov_off_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
    out.extend_from_slice(&(surviving.len() as u16).to_be_bytes()); // sequenceCount
    let seq_offsets_start = out.len();
    for _ in 0..surviving.len() {
        out.extend_from_slice(&[0u8; 2]); // sequenceOffset placeholder
    }
    let mut bodies = Dedup::default();
    for (i, (_input_gid, seq_body)) in surviving.iter().enumerate() {
        let body_start = bodies.place(&mut out, seq_body);
        let slot = seq_offsets_start + i * 2;
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
/// Drop rules: looser than type 2 because alternates are user-chosen,
/// so dropping individual entries doesn't break the meaning of the
/// substitution as a whole:
///
/// - Each AlternateSet keeps only the alternates whose gids survived
///   the GidMap (and renumbers them).
/// - A Coverage entry dies if its input gid isn't kept **or** every
///   alternate in its AlternateSet was dropped (an empty AlternateSet
///   isn't useful: fall through to the input glyph rather than emit
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
        // Empty AlternateSet means every alternate dropped: drop the
        // whole Coverage entry. The fall-through is the input glyph
        // unchanged, which is shaping's default behavior anyway.
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

    Some(emit_type3_subtable(ctx, &surviving))
}

/// Encodes a complete AlternateSubstFormat1 subtable around already-
/// rewritten `(new_input_gid, alt_set_bytes)` pairs.
fn emit_type3_subtable(ctx: &RewriterCtx, surviving: &[(u16, Vec<u8>)]) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // substFormat
    let cov_off_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
    out.extend_from_slice(&(surviving.len() as u16).to_be_bytes()); // alternateSetCount
    let alt_offsets_start = out.len();
    for _ in 0..surviving.len() {
        out.extend_from_slice(&[0u8; 2]); // alternateSetOffset placeholder
    }
    let mut bodies = Dedup::default();
    for (i, (_input_gid, alt_body)) in surviving.iter().enumerate() {
        let body_start = bodies.place(&mut out, alt_body);
        let slot = alt_offsets_start + i * 2;
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
/// Drop rules: every condition collapses the affected scope, never a
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
            // First component dropped: the whole LigatureSet goes with
            // it; shaping the input sequence with first_old absent can't
            // fire any of these ligatures anyway.
            continue;
        };
        let off_off = set_offsets_off + i * 2;
        let set_off = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        let Some(set_bytes) = sub.get(set_off..) else {
            continue;
        };
        let Some(rewritten_set) = rewrite_ligature_set(set_bytes, ctx) else {
            continue;
        };
        surviving_sets.push((first_new, rewritten_set));
    }

    if surviving_sets.is_empty() {
        return None;
    }

    Some(emit_type4_subtable(ctx, &surviving_sets))
}

/// Rewrites a single LigatureSet. Returns `None` when every ligature in
/// the set drops (caller propagates that to "Coverage entry dies").
fn rewrite_ligature_set(set_bytes: &[u8], ctx: &RewriterCtx) -> Option<Vec<u8>> {
    let map = ctx.gid_map;
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
        // Result gid must survive. Otherwise the substitution has
        // nowhere to go.
        let Some(lig_glyph_new) = map.map(lig_glyph_old) else {
            continue;
        };
        // Every tail component must survive. A single missing piece
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
        out[slot..slot + 2].copy_from_slice(&ctx.off16(body_start).to_be_bytes());
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
fn emit_type4_subtable(ctx: &RewriterCtx, surviving: &[(u16, Vec<u8>)]) -> RewrittenSubtable {
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
    let mut bodies = Dedup::default();
    for (i, (_first_gid, set_body)) in surviving.iter().enumerate() {
        let body_start = bodies.place(&mut out, set_body);
        let slot = set_offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&ctx.off16(body_start).to_be_bytes());
    }

    // Coverage. Pair every kept first-gid with its index in the
    // ligatureSetOffsets array: emit_coverage_from_pairs sorts by gid
    // and falls back to format 2 when those indices aren't a 0..N
    // sequence after sorting.
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
fn rewrite_type5(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
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
fn emit_context_format1(ctx: &RewriterCtx, surviving: &[(u16, Vec<u8>)]) -> RewrittenSubtable {
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
    let cd_bytes = sub.get(cd_off..)?;
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
    let cd_pairs_old = crate::layout::parse_classdef_pairs_from_bytes(cd_bytes);
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

/// Rewrites a GSUB type 6 (Chained Context Substitution) subtable.
fn rewrite_type6(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
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
        let Some(rewritten_set) = rewrite_type6_rule_set(set_bytes, ctx) else {
            continue;
        };
        surviving_sets.push((first_new, rewritten_set));
    }

    if surviving_sets.is_empty() {
        return None;
    }
    Some(emit_context_format1(ctx, &surviving_sets))
}

fn rewrite_type6_rule_set(set_bytes: &[u8], ctx: &RewriterCtx) -> Option<Vec<u8>> {
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
    let covered = parse_coverage_glyphs(cov_bytes);
    let new_covered: Vec<u16> = covered.iter().filter_map(|&g| map.map(g)).collect();
    if new_covered.is_empty() {
        return None;
    }

    let bt_cd_bytes = sub.get(bt_cd_off..)?;
    let in_cd_bytes = sub.get(in_cd_off..)?;
    let la_cd_bytes = sub.get(la_cd_off..)?;

    let bt_pairs_old = crate::layout::parse_classdef_pairs_from_bytes(bt_cd_bytes);
    let in_pairs_old = crate::layout::parse_classdef_pairs_from_bytes(in_cd_bytes);
    let la_pairs_old = crate::layout::parse_classdef_pairs_from_bytes(la_cd_bytes);

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
    if set_bytes.len() < 2 + rule_count * 2 {
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
    let new_in = read_cov_array(in_offs_start, in_count)?;
    let new_la = read_cov_array(la_offs_start, la_count)?;

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
fn rewrite_type8(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
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
/// targets (types 1, 2, 3 and 8) for every kept input glyph. The
/// closure walker already pulls in ligature components (type 4) and
/// mark-base partners; this fills in the substitution-target side.
///
/// Iterates the source GSUB lookups; for each kept input glyph that a
/// type-1/2/3/8 lookup covers, marks the substitution output(s) as
/// kept. Mutates `keep` in place and returns whether anything was
/// added so the caller can decide to re-run the closure pass.
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
                gsub_type::REVERSE_CHAINED => {
                    changed |= pull_reverse_chain(sub, keep);
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

/// Pulls in the substitute of every kept input glyph in a type-8
/// (reverse chaining single substitution) subtable whose context can
/// still match, the rule HarfBuzz's closure applies: every backtrack
/// and lookahead Coverage must list at least one kept glyph. A context
/// that has lost every glyph at one of its positions can never match,
/// and [`rewrite_type8`] drops that subtable, so its substitutes are
/// not needed. The closure loop reruns this pass, so context glyphs
/// kept later still bring the substitutes in.
///
/// The layout is the one [`rewrite_type8`] reads; a subtable whose
/// Coverage and substitute counts disagree is skipped, as there.
fn pull_reverse_chain(sub: &[u8], keep: &mut [bool]) -> bool {
    let read = |pos: usize| -> Option<usize> {
        let b = sub.get(pos..pos.checked_add(2)?)?;
        Some(usize::from(u16::from_be_bytes([b[0], b[1]])))
    };
    let is_kept = |g: u16| keep.get(usize::from(g)).copied().unwrap_or(false);
    let context_can_match = |first_slot: usize, count: usize| {
        (0..count).all(|j| {
            read(first_slot + j * 2)
                .and_then(|off| sub.get(off..))
                .is_some_and(|cov| parse_coverage_glyphs(cov).into_iter().any(is_kept))
        })
    };
    if read(0) != Some(1) {
        return false;
    }
    let (Some(cov_off), Some(bt_count)) = (read(2), read(4)) else {
        return false;
    };
    let la_count_at = 6 + bt_count * 2;
    let Some(la_count) = read(la_count_at) else {
        return false;
    };
    let glyph_count_at = la_count_at + 2 + la_count * 2;
    let Some(glyph_count) = read(glyph_count_at) else {
        return false;
    };
    let Some(covered) = sub.get(cov_off..).map(parse_coverage_glyphs) else {
        return false;
    };
    if covered.len() != glyph_count
        || !context_can_match(6, bt_count)
        || !context_can_match(la_count_at + 2, la_count)
    {
        return false;
    }
    let mut targets = Vec::new();
    for (i, &g) in covered.iter().enumerate() {
        if !is_kept(g) {
            continue;
        }
        let Some(target) = read(glyph_count_at + 2 + i * 2) else {
            return false;
        };
        targets.push(target);
    }
    let mut changed = false;
    for target in targets {
        if let Some(slot) = keep.get_mut(target) {
            changed |= !*slot;
            *slot = true;
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
        // Build a GidMap by old gid -> new gid; gids not in `pairs` map
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
        // A -> small-cap-A, B -> small-cap-B, C -> small-cap-C.
        // Old: covered {65,66,67}, delta +200 -> outputs 265,266,267.
        let bytes = build_single_format1(&[65, 66, 67], 200);
        // New gid map: 65->1, 66->2, 67->3, 265->4, 266->5, 267->6 (.notdef stays at 0).
        let map = map_from_pairs(&[
            (0, 0),
            (65, 1),
            (66, 2),
            (67, 3),
            (265, 4),
            (266, 5),
            (267, 6),
        ]);
        let ctx = RewriterCtx::new(&map, None);
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
        // {10, 20, 30}, delta +5 -> outputs 15, 25, 35. New gid map
        // jumbles them: 10->1, 20->2, 30->3, 15->7, 25->9, 35->11. Now
        // input->output deltas are 6, 7, 8 (no constant).
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
        let ctx = RewriterCtx::new(&map, None);
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
        // Format 2: 10->100, 20->200, 30->300. Drop input 20 from the
        // kept set. New gid map: 10->1, 30->3, 100->11, 300->33.
        let bytes = build_single_format2(&[10, 20, 30], &[100, 200, 300]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (30, 3), (100, 11), (300, 33)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_single(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Single::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.apply(1), Some(11));
        // Input 2 (the new gid for 20) is not in the map at all because
        // 20 was dropped, so it can't be in the rewritten coverage.
        assert!(parsed.apply(2).is_none() || parsed.apply(2) == Some(0));
        assert_eq!(parsed.apply(3), Some(33));
    }

    #[test]
    fn rewrite_single_drops_pairs_with_dropped_output() {
        // 10->100 stays, 20->200 dies because 200 is dropped.
        let bytes = build_single_format2(&[10, 20], &[100, 200]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2), (100, 11)]);
        let ctx = RewriterCtx::new(&map, None);
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
        let ctx = RewriterCtx::new(&map, None);
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
        let ctx = RewriterCtx::new(&map, None);
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
        // covered {10}, delta +5 -> output 15. Mark 10 kept; pull should
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
        // 10 not kept -> 15 not pulled.
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
        // f=10, i=20 -> fi=100. Every gid is kept and renumbered down by
        // 1: 10->9, 20->19, 100->99.
        let bytes = build_type4_subtable(&[(10, vec![(100, vec![20])])]);
        let map = map_from_pairs(&[(0, 0), (10, 9), (20, 19), (100, 99)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_type4(&ctx, &bytes).unwrap();

        // Round-trip through the parser to validate semantics.
        let parsed = sigilbuzz::tables::gsub::Ligature::parse(&rs.bytes).unwrap();
        let (out_gid, span) = parsed.apply(&[9, 19, 30]).unwrap();
        assert_eq!(out_gid, 99);
        assert_eq!(span, 2);
    }

    #[test]
    fn rewrite_type4_drops_ligature_when_result_gid_drops() {
        // f=10, i=20 -> fi=100; the result gid 100 is not in the map, so
        // the ligature must die. Coverage must lose the entry too: no
        // surviving LigatureSet anchors it.
        let bytes = build_type4_subtable(&[(10, vec![(100, vec![20])])]);
        let map = map_from_pairs(&[(0, 0), (10, 9), (20, 19)]);
        let ctx = RewriterCtx::new(&map, None);
        // Only one Ligature in one LigatureSet; that ligature dies, so
        // the LigatureSet is empty, the Coverage entry dies, the
        // Coverage empties, the subtable dies.
        assert!(rewrite_type4(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type4_drops_ligature_when_component_drops() {
        // 10 + 20 + 30 -> 100; component 20 dropped -> entire ligature
        // dies (single missing component kills the rule).
        let bytes = build_type4_subtable(&[(10, vec![(100, vec![20, 30])])]);
        let map = map_from_pairs(&[(0, 0), (10, 9), (30, 29), (100, 99)]);
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_type4(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type4_partial_ligature_set_survives() {
        // First-component=10 has two ligatures: (10+20->100) and
        // (10+30->200). Drop component 30 -> second ligature dies, first
        // survives. LigatureSet stays, Coverage entry stays.
        let bytes = build_type4_subtable(&[(10, vec![(100, vec![20]), (200, vec![30])])]);
        let map = map_from_pairs(&[
            (0, 0),
            (10, 9),
            (20, 19),
            (100, 99),
            (200, 199), // 200 stays mapped, but its tail (30) is dropped
        ]);
        let ctx = RewriterCtx::new(&map, None);
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
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_type4(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Ligature::parse(&rs.bytes).unwrap();
        // The 40-rooted ligature still fires.
        let (out_gid, span) = parsed.apply(&[39, 49]).unwrap();
        assert_eq!(out_gid, 199);
        assert_eq!(span, 2);
        // The 10-rooted ligature is gone. Its first component is no
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
        let ctx = RewriterCtx::new(&map, None);
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
        let ctx = RewriterCtx::new(&map, None);
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
        let ctx = RewriterCtx::new(&map, None);
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
        let ctx = RewriterCtx::new(&map, None);
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
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_type2(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Multiple::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.apply(99), Some(vec![39, 49, 59]));
        assert!(parsed.apply(99).is_some());
        assert!(parsed.apply(0).is_none());
    }

    #[test]
    fn rewrite_type2_drops_sequence_when_substitute_drops() {
        // 100 -> [40, 50, 60] but 50 is dropped. The whole Sequence
        // dies because emitting [40, ?, 60] would point at a missing
        // gid.
        let bytes = build_type2_subtable(&[(100, vec![40, 50, 60])]);
        let map = map_from_pairs(&[(0, 0), (40, 39), (60, 59), (100, 99)]);
        let ctx = RewriterCtx::new(&map, None);
        // Single-entry subtable; that entry dies -> subtable dies.
        assert!(rewrite_type2(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type2_drops_entry_when_input_drops() {
        // Two entries; drop input 100 entirely -> first entry vanishes,
        // second entry survives.
        let bytes = build_type2_subtable(&[(100, vec![40, 50]), (200, vec![70])]);
        let map = map_from_pairs(&[
            (0, 0),
            (40, 39),
            (50, 49),
            (70, 69),
            (200, 199),
            // 100 not in the map -> its Coverage entry dies.
        ]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_type2(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Multiple::parse(&rs.bytes).unwrap();
        // Surviving entry: input 199 -> [69].
        assert_eq!(parsed.apply(199), Some(vec![69]));
        // The dropped entry's input gid (100 was renumbered to nothing)
        // is gone: neither old nor any other gid produces a hit.
        assert!(parsed.apply(99).is_none());
    }

    #[test]
    fn rewrite_type2_returns_none_when_coverage_empties() {
        let bytes = build_type2_subtable(&[(100, vec![40, 50])]);
        let map = map_from_pairs(&[(0, 0)]);
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_type2(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type2_via_dispatcher() {
        let bytes = build_type2_subtable(&[(100, vec![40, 50])]);
        let map = map_from_pairs(&[(0, 0), (40, 39), (50, 49), (100, 99)]);
        let ctx = RewriterCtx::new(&map, None);
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
        let ctx = RewriterCtx::new(&map, None);
        let a = rewrite_type2(&ctx, &bytes).unwrap();
        let b = rewrite_type2(&ctx, &bytes).unwrap();
        assert_eq!(a.bytes, b.bytes);
    }

    #[test]
    fn pull_multiple_extends_keep_set() {
        // Closure walker: input 100 is kept -> every substitute in the
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
        // 100 not kept -> no outputs pulled.
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
        let map = map_from_pairs(&[(0, 0), (10, 9), (100, 99), (101, 100), (102, 101)]);
        let ctx = RewriterCtx::new(&map, None);
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
        let ctx = RewriterCtx::new(&map, None);
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
        let ctx = RewriterCtx::new(&map, None);
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
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_type3(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type3_via_dispatcher() {
        let bytes = build_type3_subtable(&[(10, vec![100, 101])]);
        let map = map_from_pairs(&[(0, 0), (10, 9), (100, 99), (101, 100)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_subtable(&ctx, gsub_type::ALTERNATE, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Alternate::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.apply(9, 0), Some(99));
        assert_eq!(parsed.apply(9, 1), Some(100));
    }

    #[test]
    fn rewrite_type3_is_byte_deterministic() {
        let bytes = build_type3_subtable(&[(10, vec![100, 101]), (20, vec![200, 201, 202])]);
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
        let ctx = RewriterCtx::new(&map, None);
        let a = rewrite_type3(&ctx, &bytes).unwrap();
        let b = rewrite_type3(&ctx, &bytes).unwrap();
        assert_eq!(a.bytes, b.bytes);
    }

    #[test]
    fn pull_alternate_default_extends_keep_set_with_first_alternate_only() {
        // Closure walker: input 10 is kept -> only the *first* alternate
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
        // 10 not kept -> no outputs pulled.
        let changed = pull_alternate_default(&bytes, &mut keep);
        assert!(!changed);
        assert!(!keep[100]);
    }

    // ===== GSUB type 5 (Context Substitution) rewriter tests =====
    //
    // Build helpers for the three formats. Real fonts almost always
    // pick format 3, but the rewriter has independent paths for all
    // three so we exercise each.

    /// Builds a format-1 type-5 subtable.
    /// `sets[i]` = (first_gid, [(input_tail, [(seq_idx, lookup_idx)])])
    #[allow(clippy::type_complexity)]
    fn build_type5_format1(sets: &[(u16, Vec<(Vec<u16>, Vec<(u16, u16)>)>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // substFormat
        let cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // coverage off placeholder
        out.extend_from_slice(&(sets.len() as u16).to_be_bytes()); // ruleSetCount
        let set_offs_start = out.len();
        for _ in 0..sets.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        for (i, (_first, rules)) in sets.iter().enumerate() {
            let set_start = out.len();
            // RuleSet
            out.extend_from_slice(&(rules.len() as u16).to_be_bytes());
            let rule_offs_start = out.len();
            for _ in 0..rules.len() {
                out.extend_from_slice(&[0u8; 2]);
            }
            for (j, (input_tail, lookups)) in rules.iter().enumerate() {
                let rule_start = out.len();
                let glyph_count = (input_tail.len() + 1) as u16;
                out.extend_from_slice(&glyph_count.to_be_bytes());
                out.extend_from_slice(&(lookups.len() as u16).to_be_bytes());
                for g in input_tail {
                    out.extend_from_slice(&g.to_be_bytes());
                }
                for (s, l) in lookups {
                    out.extend_from_slice(&s.to_be_bytes());
                    out.extend_from_slice(&l.to_be_bytes());
                }
                let rel = (rule_start - set_start) as u16;
                let slot = rule_offs_start + j * 2;
                out[slot..slot + 2].copy_from_slice(&rel.to_be_bytes());
            }
            let set_off_slot = set_offs_start + i * 2;
            out[set_off_slot..set_off_slot + 2].copy_from_slice(&(set_start as u16).to_be_bytes());
        }
        let cov_start = out.len();
        let firsts: Vec<u16> = sets.iter().map(|(g, _)| *g).collect();
        out.extend_from_slice(&build_coverage_format1(&firsts));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    /// Builds a format-3 type-5 subtable from per-position glyph sets +
    /// SubstLookupRecord list.
    fn build_type5_format3(input: &[Vec<u16>], records: &[(u16, u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&3u16.to_be_bytes()); // substFormat
        out.extend_from_slice(&(input.len() as u16).to_be_bytes()); // glyphCount
        out.extend_from_slice(&(records.len() as u16).to_be_bytes()); // substLookupRecordCount
        let cov_offs_start = out.len();
        for _ in 0..input.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        for (s, l) in records {
            out.extend_from_slice(&s.to_be_bytes());
            out.extend_from_slice(&l.to_be_bytes());
        }
        for (i, gs) in input.iter().enumerate() {
            let body_start = out.len() as u16;
            out.extend_from_slice(&build_coverage_format1(gs));
            let slot = cov_offs_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&body_start.to_be_bytes());
        }
        out
    }

    #[test]
    fn rewrite_type5_format1_keeps_all_when_every_gid_survives() {
        // First gid 10 has one rule: tail [20], one nested lookup at seq 0.
        let bytes = build_type5_format1(&[(10, vec![(vec![20], vec![(0, 1)])])]);
        let map = map_from_pairs(&[(0, 0), (10, 9), (20, 19)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_type5(&ctx, &bytes).unwrap();
        // Round-trip through the parser to validate semantics.
        let parsed = sigilbuzz::tables::gsub::Context::parse(&rs.bytes).unwrap();
        match parsed {
            sigilbuzz::tables::gsub::Context::Format1(c) => {
                let cov = c.coverage();
                assert_eq!(cov.index_of(9), Some(0));
                assert!(cov.index_of(10).is_none());
            }
            _ => panic!("expected format 1"),
        }
    }

    #[test]
    fn rewrite_type5_format1_drops_rule_when_input_tail_drops() {
        // First=10, tail=[20]: drop 20. Single rule dies, set dies,
        // coverage entry dies, subtable dies.
        let bytes = build_type5_format1(&[(10, vec![(vec![20], vec![(0, 1)])])]);
        let map = map_from_pairs(&[(0, 0), (10, 9)]);
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_type5(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type5_format1_drops_when_first_glyph_drops() {
        let bytes = build_type5_format1(&[(10, vec![(vec![20], vec![(0, 1)])])]);
        let map = map_from_pairs(&[(0, 0), (20, 19)]);
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_type5(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type5_format1_drops_record_when_target_lookup_drops() {
        // Two SubstLookupRecords, indices 1 and 2. Renumber drops 2 ->
        // record list survives with one entry.
        let bytes = build_type5_format1(&[(10, vec![(vec![20], vec![(0, 1), (1, 2)])])]);
        let map = map_from_pairs(&[(0, 0), (10, 9), (20, 19)]);
        let renumber = vec![Some(0u16), Some(0u16), None]; // index 2 dropped
        let ctx = RewriterCtx::new(&map, Some(&renumber));
        let rs = rewrite_type5(&ctx, &bytes).unwrap();
        // Verify only one record survives by checking byte size: rule
        // body grows by 4 bytes per record, so we just confirm the
        // subtable parses.
        assert!(sigilbuzz::tables::gsub::Context::parse(&rs.bytes).is_ok());
    }

    #[test]
    fn rewrite_type5_format1_keeps_rule_when_all_records_drop() {
        // A rule without records is an `ignore sub` rule: it still
        // matches and still shields the rules after it.
        let bytes = build_type5_format1(&[(10, vec![(vec![20], vec![(0, 5)])])]);
        let map = map_from_pairs(&[(0, 0), (10, 9), (20, 19)]);
        let renumber = vec![None, None, None, None, None, None]; // all dropped
        let ctx = RewriterCtx::new(&map, Some(&renumber));
        let rs = rewrite_type5(&ctx, &bytes).expect("the rule survives");
        let at = |pos: usize| usize::from(u16::from_be_bytes([rs.bytes[pos], rs.bytes[pos + 1]]));
        let set = at(6);
        assert_eq!(at(set), 1, "one rule in the set");
        let rule = set + at(set + 2);
        assert_eq!(at(rule), 2, "glyphCount");
        assert_eq!(at(rule + 2), 0, "no records left");
        assert_eq!(at(rule + 4), 19, "input tail remapped");
    }

    #[test]
    fn rewrite_type5_format3_keeps_all_when_every_gid_survives() {
        // input positions: [10, 11], [20, 21]; one record (0, 1).
        let bytes = build_type5_format3(&[vec![10, 11], vec![20, 21]], &[(0, 1)]);
        let map = map_from_pairs(&[(0, 0), (10, 100), (11, 101), (20, 200), (21, 201)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_type5(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Context::parse(&rs.bytes).unwrap();
        if let sigilbuzz::tables::gsub::Context::Format3(c) = parsed {
            let inputs = c.input();
            assert_eq!(inputs.len(), 2);
            assert!(inputs[0].contains(100));
            assert!(inputs[0].contains(101));
            assert!(inputs[1].contains(200));
            assert!(inputs[1].contains(201));
        } else {
            panic!("expected format 3");
        }
    }

    #[test]
    fn rewrite_type5_format3_drops_subtable_when_any_input_position_empties() {
        // Drop both glyphs at position 1 -> that coverage empties -> subtable dies.
        let bytes = build_type5_format3(&[vec![10, 11], vec![20, 21]], &[(0, 1)]);
        let map = map_from_pairs(&[(0, 0), (10, 100), (11, 101)]);
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_type5(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type5_format3_remaps_lookup_indices() {
        let bytes = build_type5_format3(&[vec![10], vec![20]], &[(0, 3), (1, 5)]);
        let map = map_from_pairs(&[(0, 0), (10, 100), (20, 200)]);
        let renumber = vec![
            Some(0u16),
            Some(1u16),
            Some(2u16),
            Some(7u16),
            Some(8u16),
            Some(9u16),
        ];
        let ctx = RewriterCtx::new(&map, Some(&renumber));
        let rs = rewrite_type5(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Context::parse(&rs.bytes).unwrap();
        if let sigilbuzz::tables::gsub::Context::Format3(c) = parsed {
            let recs = c.lookups();
            // recs[0].lookup_list_index was 3, renumber[3] = 7
            // recs[1].lookup_list_index was 5, renumber[5] = 9
            assert_eq!(recs[0].lookup_list_index, 7);
            assert_eq!(recs[1].lookup_list_index, 9);
        } else {
            panic!("expected format 3");
        }
    }

    #[test]
    fn rewrite_type5_format3_is_byte_deterministic() {
        let bytes = build_type5_format3(&[vec![10, 11], vec![20]], &[(0, 1), (1, 2)]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (11, 2), (20, 3)]);
        let ctx = RewriterCtx::new(&map, None);
        let a = rewrite_type5(&ctx, &bytes).unwrap();
        let b = rewrite_type5(&ctx, &bytes).unwrap();
        assert_eq!(a.bytes, b.bytes);
    }

    // ===== GSUB type 6 (Chained Context Substitution) rewriter tests =====

    /// Builds a format-3 type-6 subtable around explicit per-position
    /// glyph arrays + SubstLookupRecord list.
    fn build_type6_format3(
        backtrack: &[Vec<u16>],
        input: &[Vec<u16>],
        lookahead: &[Vec<u16>],
        records: &[(u16, u16)],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&3u16.to_be_bytes()); // substFormat
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
        for (s, l) in records {
            out.extend_from_slice(&s.to_be_bytes());
            out.extend_from_slice(&l.to_be_bytes());
        }
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
    fn rewrite_type6_format3_keeps_all_when_every_gid_survives() {
        // 1 backtrack position covering {5}, 2 input positions covering
        // {10, 11} / {12}, 1 lookahead position covering {30}.
        let bytes = build_type6_format3(
            &[vec![5]],
            &[vec![10, 11], vec![12]],
            &[vec![30]],
            &[(0, 1)],
        );
        let map = map_from_pairs(&[(0, 0), (5, 50), (10, 100), (11, 101), (12, 102), (30, 300)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_type6(&ctx, &bytes).unwrap();
        // Format-3 chained context parses through ChainContext.
        let parsed = sigilbuzz::tables::gsub::ChainContext::parse(&rs.bytes).unwrap();
        let (bt, inp, la) = parsed.context_len();
        assert_eq!((bt, inp, la), (1, 2, 1));
    }

    #[test]
    fn rewrite_type6_format3_drops_when_backtrack_position_empties() {
        // Backtrack [5]: drop 5 -> backtrack coverage empties -> subtable dies.
        let bytes = build_type6_format3(&[vec![5]], &[vec![10]], &[vec![30]], &[(0, 1)]);
        let map = map_from_pairs(&[(0, 0), (10, 100), (30, 300)]);
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_type6(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type6_format3_drops_when_lookahead_position_empties() {
        let bytes = build_type6_format3(&[vec![5]], &[vec![10]], &[vec![30]], &[(0, 1)]);
        let map = map_from_pairs(&[(0, 0), (5, 50), (10, 100)]);
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_type6(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type6_format3_drops_record_when_target_lookup_drops() {
        // Two records (0, 1) and (1, 5); renumber drops 5.
        let bytes = build_type6_format3(&[], &[vec![10]], &[], &[(0, 1), (1, 5)]);
        let map = map_from_pairs(&[(0, 0), (10, 100)]);
        let renumber = vec![
            Some(0u16),
            Some(1u16),
            Some(2u16),
            Some(3u16),
            Some(4u16),
            None,
        ];
        let ctx = RewriterCtx::new(&map, Some(&renumber));
        let rs = rewrite_type6(&ctx, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::ChainContext::parse(&rs.bytes).unwrap();
        // One record survives.
        assert_eq!(parsed.substitutions().len(), 1);
        assert_eq!(parsed.substitutions()[0].lookup_list_index, 1);
    }

    #[test]
    fn rewrite_type6_format3_keeps_subtable_when_all_records_drop() {
        // The subtable becomes an `ignore sub` rule, which still stops
        // the later subtables of its lookup from matching.
        let bytes = build_type6_format3(&[vec![5]], &[vec![10]], &[], &[(0, 1), (1, 5)]);
        let map = map_from_pairs(&[(0, 0), (5, 50), (10, 100)]);
        let renumber = vec![None, None, None, None, None, None];
        let ctx = RewriterCtx::new(&map, Some(&renumber));
        let rs = rewrite_type6(&ctx, &bytes).expect("the subtable survives");
        let parsed = sigilbuzz::tables::gsub::ChainContext::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.context_len(), (1, 1, 0));
        assert!(parsed.substitutions().is_empty());
    }

    #[test]
    fn rewrite_type6_format3_keeps_source_ignore_rule() {
        // Compiled from `ignore sub a b' c;`: no records at all, even
        // before any lookup drops.
        let bytes = build_type6_format3(&[vec![5]], &[vec![10]], &[vec![30]], &[]);
        let map = map_from_pairs(&[(0, 0), (5, 50), (10, 100), (30, 300)]);
        for renumber in [None, Some(alloc::vec![Some(0u16)])] {
            let ctx = RewriterCtx::new(&map, renumber.as_deref());
            let rs = rewrite_type6(&ctx, &bytes).expect("the ignore rule survives");
            let parsed = sigilbuzz::tables::gsub::ChainContext::parse(&rs.bytes).unwrap();
            assert_eq!(parsed.context_len(), (1, 1, 1));
            assert!(parsed.substitutions().is_empty());
        }
    }

    #[test]
    fn rewrite_type6_format3_is_byte_deterministic() {
        let bytes = build_type6_format3(&[vec![5]], &[vec![10, 11]], &[vec![30]], &[(0, 1)]);
        let map = map_from_pairs(&[(0, 0), (5, 50), (10, 100), (11, 101), (30, 300)]);
        let ctx = RewriterCtx::new(&map, None);
        let a = rewrite_type6(&ctx, &bytes).unwrap();
        let b = rewrite_type6(&ctx, &bytes).unwrap();
        assert_eq!(a.bytes, b.bytes);
    }

    #[test]
    fn rewrite_type6_via_dispatcher() {
        let bytes = build_type6_format3(&[], &[vec![10]], &[], &[(0, 1)]);
        let map = map_from_pairs(&[(0, 0), (10, 100)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_subtable(&ctx, gsub_type::CHAINED_CONTEXT, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::ChainContext::parse(&rs.bytes);
        assert!(parsed.is_ok());
    }

    // ===== GSUB type 8 (Reverse Chained Single Substitution) rewriter tests =====

    /// Builds a format-1 type-8 subtable. `subs[i]` is the substitute
    /// for `input[i]`.
    fn build_type8(
        input: &[u16],
        backtrack: &[Vec<u16>],
        lookahead: &[Vec<u16>],
        subs: &[u16],
    ) -> Vec<u8> {
        assert_eq!(input.len(), subs.len());
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // substFormat
        let cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // coverage placeholder
        out.extend_from_slice(&(backtrack.len() as u16).to_be_bytes());
        let bt_slots = out.len();
        for _ in 0..backtrack.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        out.extend_from_slice(&(lookahead.len() as u16).to_be_bytes());
        let la_slots = out.len();
        for _ in 0..lookahead.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        out.extend_from_slice(&(subs.len() as u16).to_be_bytes());
        for s in subs {
            out.extend_from_slice(&s.to_be_bytes());
        }
        let cov_off = out.len();
        out.extend_from_slice(&build_coverage_format1(input));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());
        let mut patch = |slots: usize, covs: &[Vec<u16>]| {
            for (i, gs) in covs.iter().enumerate() {
                let body_start = out.len() as u16;
                out.extend_from_slice(&build_coverage_format1(gs));
                let slot = slots + i * 2;
                out[slot..slot + 2].copy_from_slice(&body_start.to_be_bytes());
            }
        };
        patch(bt_slots, backtrack);
        patch(la_slots, lookahead);
        out
    }

    #[test]
    fn rewrite_type8_keeps_all_when_every_gid_survives() {
        // Coverage {10}, backtrack [{5}], lookahead [{30}], substitute {100}.
        let bytes = build_type8(&[10], &[vec![5]], &[vec![30]], &[100]);
        let map = map_from_pairs(&[(0, 0), (5, 50), (10, 1), (30, 3), (100, 99)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_type8(&ctx, &bytes).unwrap();
        let rc = sigilbuzz::tables::gsub::ReverseChain::parse(&rs.bytes).unwrap();
        // Apply with surrounding context: [50, 1, 3] -> 99.
        assert_eq!(rc.apply(&[50, 1, 3], 1), Some(99));
    }

    #[test]
    fn rewrite_type8_drops_pair_when_substitute_drops() {
        // Coverage {10, 20}, substitutes {100, 200}; drop 200.
        let bytes = build_type8(&[10, 20], &[], &[], &[100, 200]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2), (100, 99)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_type8(&ctx, &bytes).unwrap();
        let rc = sigilbuzz::tables::gsub::ReverseChain::parse(&rs.bytes).unwrap();
        // 1 still substitutes to 99; 2 (was 20) is no longer covered.
        assert_eq!(rc.apply(&[1], 0), Some(99));
        assert_eq!(rc.apply(&[2], 0), None);
    }

    #[test]
    fn rewrite_type8_drops_pair_when_input_drops() {
        let bytes = build_type8(&[10, 20], &[], &[], &[100, 200]);
        let map = map_from_pairs(&[(0, 0), (20, 2), (200, 199)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_type8(&ctx, &bytes).unwrap();
        let rc = sigilbuzz::tables::gsub::ReverseChain::parse(&rs.bytes).unwrap();
        assert_eq!(rc.apply(&[2], 0), Some(199));
    }

    #[test]
    fn rewrite_type8_drops_subtable_when_input_coverage_empties() {
        let bytes = build_type8(&[10], &[], &[], &[100]);
        let map = map_from_pairs(&[(0, 0)]);
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_type8(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type8_drops_subtable_when_backtrack_coverage_empties() {
        // Backtrack [{5}]: drop 5 -> backtrack coverage empties -> subtable dies.
        let bytes = build_type8(&[10], &[vec![5]], &[], &[100]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (100, 99)]);
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_type8(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type8_drops_subtable_when_lookahead_coverage_empties() {
        let bytes = build_type8(&[10], &[], &[vec![30]], &[100]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (100, 99)]);
        let ctx = RewriterCtx::new(&map, None);
        assert!(rewrite_type8(&ctx, &bytes).is_none());
    }

    #[test]
    fn rewrite_type8_is_byte_deterministic() {
        let bytes = build_type8(&[10, 11], &[vec![5]], &[vec![30]], &[100, 101]);
        let map = map_from_pairs(&[
            (0, 0),
            (5, 50),
            (10, 1),
            (11, 2),
            (30, 3),
            (100, 99),
            (101, 98),
        ]);
        let ctx = RewriterCtx::new(&map, None);
        let a = rewrite_type8(&ctx, &bytes).unwrap();
        let b = rewrite_type8(&ctx, &bytes).unwrap();
        assert_eq!(a.bytes, b.bytes);
    }

    #[test]
    fn rewrite_type8_via_dispatcher() {
        let bytes = build_type8(&[10], &[], &[], &[100]);
        let map = map_from_pairs(&[(0, 0), (10, 1), (100, 99)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_subtable(&ctx, gsub_type::REVERSE_CHAINED, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::ReverseChain::parse(&rs.bytes);
        assert!(parsed.is_ok());
    }

    /// A keep bitset over glyphs `0..256` with `kept` set.
    fn keep_set(kept: &[u16]) -> Vec<bool> {
        let mut keep = vec![false; 256];
        for &g in kept {
            keep[usize::from(g)] = true;
        }
        keep
    }

    #[test]
    fn closure_pulls_reverse_chain_substitutes_of_kept_inputs() {
        // 10 -> 100 and 11 -> 101 after a 5 and before a 30 or 31.
        let bytes = build_type8(&[10, 11], &[vec![5]], &[vec![30, 31]], &[100, 101]);
        let mut keep = keep_set(&[5, 10, 31]);
        assert!(pull_reverse_chain(&bytes, &mut keep));
        assert!(keep[100], "the kept input's substitute joins the closure");
        assert!(!keep[101], "an input that is not kept brings nothing in");
        assert!(
            !pull_reverse_chain(&bytes, &mut keep),
            "a second pass adds nothing"
        );
    }

    #[test]
    fn closure_skips_reverse_chain_rules_whose_context_cannot_match() {
        let bytes = build_type8(&[10], &[vec![5]], &[vec![30]], &[100]);
        for kept in [&[10u16, 30][..], &[5, 10], &[10]] {
            let mut keep = keep_set(kept);
            assert!(!pull_reverse_chain(&bytes, &mut keep), "kept {kept:?}");
            assert!(!keep[100], "kept {kept:?}");
        }
    }

    #[test]
    fn closure_then_rewrite_keeps_the_reverse_chain_substitution() {
        // Keeping the input and its context glyphs is enough: the
        // closure adds the substitute and the rewrite keeps the pair.
        let bytes = build_type8(&[10], &[vec![5]], &[vec![30]], &[100]);
        let mut keep = keep_set(&[0, 5, 10, 30]);
        pull_reverse_chain(&bytes, &mut keep);
        let kept: Vec<u16> = (0..256u16).filter(|&g| keep[usize::from(g)]).collect();
        let map = GidMap::from_kept(&kept);
        let rs = rewrite_type8(&RewriterCtx::new(&map, None), &bytes).expect("subtable survives");
        let rc = sigilbuzz::tables::gsub::ReverseChain::parse(&rs.bytes).unwrap();
        let new = |g: u16| map.map(g).unwrap();
        assert_eq!(
            rc.apply(&[new(5), new(10), new(30)], 1),
            Some(new(100)),
            "the subset still substitutes in context"
        );
    }

    #[test]
    fn closure_ignores_truncated_reverse_chain_subtables() {
        let bytes = build_type8(&[10], &[vec![5]], &[vec![30]], &[100]);
        for len in 0..bytes.len() {
            let mut keep = keep_set(&[5, 10, 30]);
            let before = keep.clone();
            pull_reverse_chain(&bytes[..len], &mut keep);
            assert_eq!(keep, before, "cut at {len}");
        }
    }

    #[test]
    fn rewrite_type5_via_dispatcher() {
        let bytes = build_type5_format3(&[vec![10]], &[(0, 1)]);
        let map = map_from_pairs(&[(0, 0), (10, 100)]);
        let ctx = RewriterCtx::new(&map, None);
        let rs = rewrite_subtable(&ctx, gsub_type::CONTEXT, &bytes).unwrap();
        let parsed = sigilbuzz::tables::gsub::Context::parse(&rs.bytes);
        assert!(parsed.is_ok());
    }
}
