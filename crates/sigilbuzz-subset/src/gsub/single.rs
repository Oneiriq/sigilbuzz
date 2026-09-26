//! Rewriters for the one-glyph-in GSUB lookups: type 1 (single),
//! type 2 (multiple) and type 3 (alternate) substitution.

use alloc::vec::Vec;

use crate::coverage::emit_coverage_from_pairs;
use crate::device::Dedup;
use crate::layout::{RewriterCtx, RewrittenSubtable};

/// Rewrites a GSUB type 1 (Single Substitution) subtable. Picks the
/// smaller of formats 1 (delta) or 2 (explicit) given the remapped
/// covered/substitute pairs.
pub(super) fn rewrite_single(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 4 {
        return None;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let cov_bytes = sub.get(cov_off..)?;
    let covered = ctx.gid_map.coverage_glyphs(cov_bytes)?;

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
                ctx.diag.in_part(
                    sub,
                    4,
                    "SingleSubst format 2 glyphCount differs from its Coverage",
                    "a lookup subtable",
                );
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
    let f1_delta: Option<i16> = pairs.first().and_then(|&(in0, out0)| {
        let candidate = out0.wrapping_sub(in0) as i16;
        pairs
            .iter()
            .all(|&(i, o)| (o.wrapping_sub(i)) as i16 == candidate)
            .then_some(candidate)
    });

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
        out.extend_from_slice(&0u16.to_be_bytes()); // coverage offset placeholder
        out.extend_from_slice(&delta.to_be_bytes());
        let cov_off = ctx.off16(out.len());
        out.extend_from_slice(&cov_bytes);
        out[2..4].copy_from_slice(&cov_off.to_be_bytes());
    } else {
        // Format 2.
        out.extend_from_slice(&2u16.to_be_bytes()); // substFormat
        out.extend_from_slice(&0u16.to_be_bytes()); // coverage offset placeholder
        out.extend_from_slice(&ctx.count16(pairs.len()).to_be_bytes()); // glyphCount
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
pub(super) fn rewrite_type2(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
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
    let covered = ctx.gid_map.coverage_glyphs(cov_bytes)?;
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
        if !map.spend(glyph_count) {
            return None;
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

    Some(emit_offset_array_subtable(ctx, &surviving))
}

/// Encodes the shared layout of MultipleSubstFormat1,
/// AlternateSubstFormat1, LigatureSubstFormat1, and the format 1
/// context subtables around already-rewritten `(new_first_gid, body)`
/// pairs:
///
/// ```text
///   u16      format = 1
///   Offset16 coverageOffset
///   u16      count
///   Offset16 offsets[count]      (subtable-relative)
///   bodies, in input order, identical bodies sharing one copy
///   Coverage, appended last
/// ```
///
/// Each gid is paired with its index in the offset array.
/// `emit_coverage_from_pairs` sorts by gid and falls back to format 2
/// when those indices aren't a 0..N sequence after sorting. An offset
/// or count past 16 bits is recorded in `ctx.offsets`.
pub(super) fn emit_offset_array_subtable(
    ctx: &RewriterCtx,
    surviving: &[(u16, Vec<u8>)],
) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
    out.extend_from_slice(&ctx.count16(surviving.len()).to_be_bytes());
    let offsets_start = out.len();
    out.resize(offsets_start + surviving.len() * 2, 0);
    let mut bodies = Dedup::default();
    for (i, (_first_gid, body)) in surviving.iter().enumerate() {
        let body_start = bodies.place(&mut out, body);
        let slot = offsets_start + i * 2;
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
    out[2..4].copy_from_slice(&cov_off.to_be_bytes());
    RewrittenSubtable { bytes: out }
}

/// Encodes a `u16 count` + `Offset16 offsets[count]` list followed by
/// the bodies, identical bodies sharing one copy. Offsets are relative
/// to the start of the list. This is the shape of RuleSet, ClassSet,
/// and their chained counterparts. An offset or count past 16 bits is
/// recorded in `ctx.offsets`.
pub(super) fn emit_offset_list(ctx: &RewriterCtx, bodies_in: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&ctx.count16(bodies_in.len()).to_be_bytes());
    let offsets_start = out.len();
    out.resize(offsets_start + bodies_in.len() * 2, 0);
    let mut bodies = Dedup::default();
    for (i, body) in bodies_in.iter().enumerate() {
        let body_start = bodies.place(&mut out, body);
        let slot = offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&ctx.off16(body_start).to_be_bytes());
    }
    out
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
pub(super) fn rewrite_type3(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
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
    let covered = ctx.gid_map.coverage_glyphs(cov_bytes)?;
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
        if !map.spend(glyph_count) {
            return None;
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

    Some(emit_offset_array_subtable(ctx, &surviving))
}
