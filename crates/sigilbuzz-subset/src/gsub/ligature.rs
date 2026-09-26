//! Rewriter for GSUB type 4 (ligature substitution).

use alloc::vec::Vec;

use crate::coverage::emit_coverage_from_pairs;
use crate::device::Dedup;
use crate::layout::{parse_coverage_glyphs, RewriterCtx, RewrittenSubtable};

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
pub(super) fn rewrite_type4(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
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
    if first_components.len() > set_count {
        ctx.diag.in_part(
            sub,
            4,
            "LigatureSubst Coverage lists more glyphs than ligatureSetCount",
            "the ligatures of the extra glyphs",
        );
    }

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
