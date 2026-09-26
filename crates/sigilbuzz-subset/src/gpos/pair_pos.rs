//! Rewriter for GPOS type 2 (pair adjustment): format 1 pair sets and
//! the format 2 class matrix, with its format 1 fallback.

use alloc::vec::Vec;

use super::pair_sets::{emit_pair_sets, PairSets};
use super::single_adj::{carry_devices, value_record_size};

use crate::layout::{classdef_pairs_at, parse_coverage_glyphs, RewriterCtx, RewrittenSubtable};

// ---------------------------------------------------------------------------
// Type 2: Pair Adjustment
// ---------------------------------------------------------------------------

/// Rewrites a PairPos subtable. Dispatches on format. Format 1 output
/// (including the format 2 fallback) may be split into several
/// subtables, see [`pair_sets`](super::pair_sets).
pub(super) fn rewrite_pair_pos(ctx: &RewriterCtx, sub: &[u8]) -> Vec<RewrittenSubtable> {
    match sub.get(..2) {
        Some([0, 1]) => rewrite_pair_pos_format1(ctx, sub),
        Some([0, 2]) => rewrite_pair_pos_format2(ctx, sub).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Rewrites PairPos format 1.
///
/// Layout:
///
/// ```text
///   u16      posFormat = 1
///   Offset16 coverageOffset
///   u16      valueFormat1
///   u16      valueFormat2
///   u16      pairSetCount         (== Coverage entry count)
///   Offset16 pairSetOffsets[pairSetCount]
///
///   PairSet:
///     u16             pairValueCount
///     PairValueRecord pairValueRecords[pairValueCount]
///
///   PairValueRecord:
///     u16         secondGlyph
///     ValueRecord valueRecord1   (sized by valueFormat1)
///     ValueRecord valueRecord2   (sized by valueFormat2)
/// ```
pub(super) fn rewrite_pair_pos_format1(ctx: &RewriterCtx, sub: &[u8]) -> Vec<RewrittenSubtable> {
    match read_pair_pos_format1(ctx, sub) {
        Some((value_format1, value_format2, sets)) => {
            emit_pair_sets(ctx, value_format1, value_format2, sets)
        }
        None => Vec::new(),
    }
}

/// Reads the kept first glyphs of a PairPos format 1 subtable with
/// their rebuilt PairSets. `None` when nothing survives.
fn read_pair_pos_format1(ctx: &RewriterCtx, sub: &[u8]) -> Option<(u16, u16, PairSets)> {
    if sub.len() < 10 {
        return None;
    }
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let value_format1 = u16::from_be_bytes([sub[4], sub[5]]);
    let value_format2 = u16::from_be_bytes([sub[6], sub[7]]);
    let pair_set_count = u16::from_be_bytes([sub[8], sub[9]]) as usize;
    let set_offsets_off = 10usize;
    if sub.len() < set_offsets_off + pair_set_count * 2 {
        return None;
    }
    let cov_bytes = sub.get(cov_off..)?;
    let covered = parse_coverage_glyphs(cov_bytes);
    let pair_count = covered.len().min(pair_set_count);

    let v1_size = value_record_size(value_format1);
    let v2_size = value_record_size(value_format2);
    let pvr_size = 2 + v1_size + v2_size;

    let map = ctx.gid_map;
    // (new_first_gid, encoded_pair_set_bytes) for surviving entries.
    let mut surviving_sets: Vec<(u16, Vec<u8>)> = Vec::new();

    for (i, &first_old) in covered.iter().enumerate().take(pair_count) {
        let Some(first_new) = map.map(first_old) else {
            continue;
        };
        let off_off = set_offsets_off + i * 2;
        let set_off = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        let Some(set_bytes) = sub.get(set_off..) else {
            continue;
        };
        if set_bytes.len() < 2 {
            continue;
        }
        let pair_value_count = u16::from_be_bytes([set_bytes[0], set_bytes[1]]) as usize;
        let need = 2 + pair_value_count * pvr_size;
        if set_bytes.len() < need {
            continue;
        }
        // Filter PairValueRecords by surviving secondGlyph.
        let mut survivors: Vec<(u16, Vec<u8>)> = Vec::new();
        for j in 0..pair_value_count {
            let off = 2 + j * pvr_size;
            let second_old = u16::from_be_bytes([set_bytes[off], set_bytes[off + 1]]);
            let Some(second_new) = map.map(second_old) else {
                continue;
            };
            // Bytes following the secondGlyph are the two ValueRecords;
            // both are gid-independent so they travel verbatim.
            let body_off = off + 2;
            let body_end = body_off + v1_size + v2_size;
            let body = set_bytes[body_off..body_end].to_vec();
            survivors.push((second_new, body));
        }
        if survivors.is_empty() {
            continue;
        }
        // Spec requires PairValueRecords sorted by secondGlyph; sort
        // and dedupe.
        survivors.sort_by_key(|(g, _)| *g);
        survivors.dedup_by_key(|(g, _)| *g);
        let mut new_set = encode_pair_set(&survivors, v1_size, v2_size);
        // PairValueRecord device offsets are relative to the PairSet,
        // so the tables travel inside the rebuilt PairSet body.
        let formats = [(0, value_format1), (v1_size, value_format2)];
        carry_devices(
            ctx,
            &mut new_set,
            set_bytes,
            4,
            survivors.len(),
            pvr_size,
            &formats,
        );
        surviving_sets.push((first_new, new_set));
    }

    if surviving_sets.is_empty() {
        return None;
    }
    Some((value_format1, value_format2, surviving_sets))
}

fn encode_pair_set(survivors: &[(u16, Vec<u8>)], _v1: usize, _v2: usize) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(survivors.len() as u16).to_be_bytes());
    for (second, body) in survivors {
        out.extend_from_slice(&second.to_be_bytes());
        out.extend_from_slice(body);
    }
    out
}

/// Rewrites PairPos format 2 (class-based matrix).
///
/// Layout:
///
/// ```text
///   u16      posFormat = 2
///   Offset16 coverageOffset
///   u16      valueFormat1
///   u16      valueFormat2
///   Offset16 classDef1Offset
///   Offset16 classDef2Offset
///   u16      class1Count
///   u16      class2Count
///   Class1Record class1Records[class1Count]:
///     Class2Record class2Records[class2Count]:
///       ValueRecord valueRecord1   (sized by valueFormat1)
///       ValueRecord valueRecord2   (sized by valueFormat2)
/// ```
///
/// # Drop policy
///
/// Two strategies, picked by [`should_use_format1_fallback`]:
///
/// 1. **Fmt-2 pass-through** (the cheap path). When the kept-gid set
///    spans enough classes that synthesizing explicit pairs would be
///    bytes-heavy, we walk both ClassDefs and rewrite them through the
///    GidMap. Source class IDs are preserved verbatim: `emit_classdef`
///    keeps `(new_gid, original_class)` so matrix indices stay valid;
///    the matrix bytes travel verbatim.
/// 2. **Fmt-1 fallback** (the precise path). When the surviving first
///    x second cross-product is small, we enumerate every kept pair,
///    look up its `(class1, class2)` in the source ClassDefs, read the
///    source matrix cell, drop pairs that resolve to all-zero
///    ValueRecords, and emit a brand-new fmt-1 PairPos. This is the
///    safest answer when class collapse would otherwise leave the new
///    matrix carrying rows/columns that no surviving gid can reach.
pub(super) fn rewrite_pair_pos_format2(
    ctx: &RewriterCtx,
    sub: &[u8],
) -> Option<Vec<RewrittenSubtable>> {
    if sub.len() < 16 {
        return None;
    }
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let value_format1 = u16::from_be_bytes([sub[4], sub[5]]);
    let value_format2 = u16::from_be_bytes([sub[6], sub[7]]);
    let cd1_off = u16::from_be_bytes([sub[8], sub[9]]) as usize;
    let cd2_off = u16::from_be_bytes([sub[10], sub[11]]) as usize;
    let class1_count = u16::from_be_bytes([sub[12], sub[13]]);
    let class2_count = u16::from_be_bytes([sub[14], sub[15]]);
    let records_off = 16usize;

    let v_pair = value_record_size(value_format1) + value_record_size(value_format2);
    let class2_stride = v_pair;
    let class1_stride = class2_count as usize * class2_stride;
    let need = records_off + class1_count as usize * class1_stride;
    if sub.len() < need {
        return None;
    }

    let cov_bytes = sub.get(cov_off..)?;
    let covered = parse_coverage_glyphs(cov_bytes);
    let cd1_pairs = classdef_pairs_at(sub, cd1_off)?;
    let cd2_pairs = classdef_pairs_at(sub, cd2_off)?;

    let map = ctx.gid_map;

    // Build kept (old, new) lists for both axes. The first-axis kept
    // set is Coverage ∩ kept-gids; the second-axis kept set spans every
    // gid the source's classDef2 mentions whose new gid survives.
    let mut surviving_first: Vec<(u16, u16)> = Vec::new(); // (old, new)
    for &g in &covered {
        if let Some(new) = map.map(g) {
            surviving_first.push((g, new));
        }
    }
    if surviving_first.is_empty() {
        return None;
    }
    let mut surviving_cov: Vec<u16> = surviving_first.iter().map(|&(_, n)| n).collect();
    surviving_cov.sort_unstable();
    surviving_cov.dedup();

    // Class-collapse fallback: when synthesizing an explicit fmt-1
    // table would be cheaper or strictly more correct (e.g. the
    // surviving first x second cross-product is small enough that
    // class-pair indirection no longer pays off), enumerate every
    // (first, second) pair from the kept sets, resolve its
    // (class1, class2) via the source ClassDefs, and read the source
    // matrix cell directly.
    if should_use_format1_fallback(&surviving_first, &cd2_pairs, map) {
        return rewrite_pair_pos_format2_to_format1(
            ctx,
            sub,
            &surviving_first,
            &cd2_pairs,
            value_format1,
            value_format2,
            class1_count,
            class2_count,
            records_off,
            cd1_off,
            cd2_off,
            map,
        );
    }

    // Filter ClassDefs: keep (new_gid, class) pairs whose class is
    // still valid in the matrix (< classNCount). We do not renumber
    // classes. The matrix bytes are preserved verbatim.
    let surviving_cd1: Vec<(u16, u16)> = cd1_pairs
        .iter()
        .filter_map(|&(g, c)| {
            let new = map.map(g)?;
            if c < class1_count {
                Some((new, c))
            } else {
                None
            }
        })
        .collect();
    let surviving_cd2: Vec<(u16, u16)> = cd2_pairs
        .iter()
        .filter_map(|&(g, c)| {
            let new = map.map(g)?;
            if c < class2_count {
                Some((new, c))
            } else {
                None
            }
        })
        .collect();

    let cd1_bytes_new = crate::classdef::emit_classdef(&surviving_cd1);
    let cd2_bytes_new = crate::classdef::emit_classdef(&surviving_cd2);

    // Matrix bytes travel verbatim: neither ValueRecord field nor
    // class indices changed.
    let matrix_bytes =
        sub[records_off..records_off + class1_count as usize * class1_stride].to_vec();

    let mut rs = emit_pair_pos_format2(
        ctx,
        value_format1,
        value_format2,
        class1_count,
        class2_count,
        &surviving_cov,
        &cd1_bytes_new,
        &cd2_bytes_new,
        &matrix_bytes,
    );
    let cells = class1_count as usize * class2_count as usize;
    let v1_size = value_record_size(value_format1);
    let formats = [(0, value_format1), (v1_size, value_format2)];
    carry_devices(
        ctx,
        &mut rs.bytes,
        sub,
        records_off,
        cells,
        v_pair,
        &formats,
    );
    Some(alloc::vec![rs])
}

#[allow(clippy::too_many_arguments)]
fn emit_pair_pos_format2(
    ctx: &RewriterCtx,
    value_format1: u16,
    value_format2: u16,
    class1_count: u16,
    class2_count: u16,
    surviving_cov: &[u16],
    cd1_bytes: &[u8],
    cd2_bytes: &[u8],
    matrix_bytes: &[u8],
) -> RewrittenSubtable {
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes()); // posFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
    out.extend_from_slice(&value_format1.to_be_bytes());
    out.extend_from_slice(&value_format2.to_be_bytes());
    let cd1_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // classDef1Offset placeholder
    let cd2_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // classDef2Offset placeholder
    out.extend_from_slice(&class1_count.to_be_bytes());
    out.extend_from_slice(&class2_count.to_be_bytes());
    out.extend_from_slice(matrix_bytes);

    let cov_start = ctx.off16(out.len());
    let cov_emitted = crate::coverage::emit_coverage_from_glyphs(surviving_cov);
    out.extend_from_slice(&cov_emitted);
    out[cov_slot..cov_slot + 2].copy_from_slice(&cov_start.to_be_bytes());

    let cd1_start = ctx.off16(out.len());
    out.extend_from_slice(cd1_bytes);
    out[cd1_slot..cd1_slot + 2].copy_from_slice(&cd1_start.to_be_bytes());

    let cd2_start = ctx.off16(out.len());
    out.extend_from_slice(cd2_bytes);
    out[cd2_slot..cd2_slot + 2].copy_from_slice(&cd2_start.to_be_bytes());

    RewrittenSubtable { bytes: out }
}

/// Heuristic: pick the fmt-1 fallback when the surviving first x second
/// cross-product is small enough that emitting an explicit pair table
/// is competitive with carrying the full class matrix verbatim.
///
/// The fallback fires when:
/// - Coverage shrunk to <= 8 first-glyphs (small kerning subsets, e.g.
///   the {A, V} case), or
/// - the kept first x kept second cross-product fits a 256-cell budget,
///   so the explicit pair table can't bloat past the source matrix.
///
/// Otherwise we use the cheap fmt-2 pass-through path. The matrix
/// indices in that path remain valid because [`emit_classdef`](crate::emit_classdef)
/// preserves source class IDs verbatim.
fn should_use_format1_fallback(
    surviving_first: &[(u16, u16)],
    _cd2_pairs: &[(u16, u16)],
    map: &crate::layout::GidMap,
) -> bool {
    // Kept-set size approximates the second-glyph universe (every kept
    // gid is a candidate second glyph through classDef2's class-0
    // default, even if it isn't listed explicitly).
    let kept_count = map.iter_kept().count();
    let cross = surviving_first.len() * kept_count.max(1);
    surviving_first.len() <= 8 || cross <= 256
}

/// One PairPos fmt-1 first-glyph set: `(new_first_gid, [(new_second_gid, value_pair_bytes)])`.
type PairPosFmt1Set = (u16, Vec<(u16, Vec<u8>)>);

/// Synthesizes a fmt-1 PairPos around the kept-gid cross-product.
/// Walks every `(first_old, first_new) * (second_old)` and reads the
/// source matrix cell at `(class1, class2)`. Drops pairs whose source
/// cell is all-zero (no kerning to preserve). The lookup answer is
/// then equivalent to "not covered".
#[allow(clippy::too_many_arguments)]
fn rewrite_pair_pos_format2_to_format1(
    ctx: &RewriterCtx,
    sub: &[u8],
    surviving_first: &[(u16, u16)],
    cd2_pairs: &[(u16, u16)],
    value_format1: u16,
    value_format2: u16,
    class1_count: u16,
    class2_count: u16,
    records_off: usize,
    cd1_off: usize,
    cd2_off: usize,
    map: &crate::layout::GidMap,
) -> Option<Vec<RewrittenSubtable>> {
    let v1_size = value_record_size(value_format1);
    let v2_size = value_record_size(value_format2);
    let v_pair = v1_size + v2_size;
    let class2_stride = v_pair;
    let class1_stride = class2_count as usize * class2_stride;

    // Build a quick (gid -> class) lookup for both ClassDefs by parsing
    // them from the source bytes once. Class 0 is the implicit default.
    let cd1_class_of = |gid: u16| -> u16 { class_of_gid(sub, cd1_off, gid) };
    let cd2_class_of = |gid: u16| -> u16 { class_of_gid(sub, cd2_off, gid) };

    // Enumerate the kept-gid universe as the candidate second-glyph
    // set. Classes 1..N appear in `cd2_pairs`, but class 0 (the
    // "everything else" bucket) carries any gid the source classDef2
    // doesn't list explicitly, and class-0 columns can still hold
    // non-zero kerning. Walking the GidMap directly catches that.
    let _ = cd2_pairs; // class lookups happen via cd2_class_of below.
    let kept_seconds: Vec<(u16, u16)> = map.iter_kept().collect();

    // Compute the cell at (class1, class2). Returns the raw value-pair
    // bytes when both indices are in range, an empty slice otherwise.
    let cell_bytes = |c1: u16, c2: u16| -> Option<&[u8]> {
        if c1 >= class1_count || c2 >= class2_count {
            return None;
        }
        let off = records_off + c1 as usize * class1_stride + c2 as usize * class2_stride;
        sub.get(off..off + v_pair)
    };

    // Build (first_new, [(second_new, value_pair_bytes), ...]).
    let mut out_sets: Vec<PairPosFmt1Set> = Vec::new();
    for &(first_old, first_new) in surviving_first {
        let c1 = cd1_class_of(first_old);
        let mut entries: Vec<(u16, Vec<u8>)> = Vec::new();
        for &(second_old, second_new) in &kept_seconds {
            let c2 = cd2_class_of(second_old);
            let Some(cell) = cell_bytes(c1, c2) else {
                continue;
            };
            // Drop all-zero cells: no kerning to carry.
            if cell.iter().all(|b| *b == 0) {
                continue;
            }
            entries.push((second_new, cell.to_vec()));
        }
        if entries.is_empty() {
            continue;
        }
        entries.sort_by_key(|(g, _)| *g);
        entries.dedup_by_key(|(g, _)| *g);
        out_sets.push((first_new, entries));
    }

    if out_sets.is_empty() {
        return None;
    }
    out_sets.sort_by_key(|(g, _)| *g);
    out_sets.dedup_by_key(|(g, _)| *g);

    // Emit fmt-1 PairPos around `out_sets`.
    Some(emit_pair_pos_format1_from_sets(
        ctx,
        value_format1,
        value_format2,
        v1_size,
        v2_size,
        &out_sets,
        sub,
    ))
}

/// Reads a single (gid -> class) value from a ClassDef stored at
/// `cd_off` inside `sub`. Returns 0 (the implicit default) on any
/// parse failure.
fn class_of_gid(sub: &[u8], cd_off: usize, gid: u16) -> u16 {
    // A null offset is the empty ClassDef: every glyph is class 0.
    if cd_off == 0 {
        return 0;
    }
    let Some(cd) = sub.get(cd_off..) else {
        return 0;
    };
    if cd.len() < 2 {
        return 0;
    }
    let format = u16::from_be_bytes([cd[0], cd[1]]);
    match format {
        1 => {
            if cd.len() < 6 {
                return 0;
            }
            let start = u16::from_be_bytes([cd[2], cd[3]]);
            let count = u16::from_be_bytes([cd[4], cd[5]]) as usize;
            if gid < start {
                return 0;
            }
            let idx = (gid - start) as usize;
            if idx >= count {
                return 0;
            }
            let off = 6 + idx * 2;
            if cd.len() < off + 2 {
                return 0;
            }
            u16::from_be_bytes([cd[off], cd[off + 1]])
        }
        2 => {
            if cd.len() < 4 {
                return 0;
            }
            let count = u16::from_be_bytes([cd[2], cd[3]]) as usize;
            for i in 0..count {
                let off = 4 + i * 6;
                if cd.len() < off + 6 {
                    return 0;
                }
                let start = u16::from_be_bytes([cd[off], cd[off + 1]]);
                let end = u16::from_be_bytes([cd[off + 2], cd[off + 3]]);
                let class = u16::from_be_bytes([cd[off + 4], cd[off + 5]]);
                if gid >= start && gid <= end {
                    return class;
                }
            }
            0
        }
        _ => 0,
    }
}

/// Emits a fmt-1 PairPos given pre-encoded value-pair bodies. Each
/// body is the concatenation of the source's two ValueRecord byte
/// sequences (which are gid-independent and travel verbatim). Their
/// device offsets are measured from `src_parent`, the source format 2
/// subtable; the tables are copied into each new PairSet, which is the
/// base format 1 measures them from.
fn emit_pair_pos_format1_from_sets(
    ctx: &RewriterCtx,
    value_format1: u16,
    value_format2: u16,
    v1_size: usize,
    v2_size: usize,
    sets: &[PairPosFmt1Set],
    src_parent: &[u8],
) -> Vec<RewrittenSubtable> {
    let formats = [(0, value_format1), (v1_size, value_format2)];
    // Re-shape sets into the encoder's expected
    // `(first_new, encoded_pair_set_body)` format.
    let mut surviving_sets: Vec<(u16, Vec<u8>)> = Vec::with_capacity(sets.len());
    for (first_new, entries) in sets {
        let mut set_body = Vec::with_capacity(2 + entries.len() * 6);
        set_body.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        for (second, body) in entries {
            set_body.extend_from_slice(&second.to_be_bytes());
            set_body.extend_from_slice(body);
        }
        let stride = 2 + v1_size + v2_size;
        carry_devices(
            ctx,
            &mut set_body,
            src_parent,
            4,
            entries.len(),
            stride,
            &formats,
        );
        surviving_sets.push((*first_new, set_body));
    }
    emit_pair_sets(ctx, value_format1, value_format2, surviving_sets)
}
