//! MultiItemVariationStore pruning: drops unreferenced delta sets and
//! regions and re-emits the store with remapped indexes.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use super::{build_cff2_index, parse_cff2_index};
use crate::SubsetError;

/// Prunes a `MultiItemVariationStore` to keep only the delta-set
/// entries listed in `referenced` (a set of source `(outer, inner)`
/// pairs). Returns:
///
/// - `BTreeMap<(old_outer, old_inner), (new_outer, new_inner)>`: the
///   index remap callers apply to surviving component records'
///   `MultiVarIdx` slots. Subtables that lose every entry are dropped
///   and the outer-index space collapses; `inner` indices are dense
///   per kept subtable.
/// - `Option<Vec<u8>>`: the rewritten MVS bytes, or `None` when the
///   pruned store has no surviving subtables (caller emits no
///   varStore offset).
///
/// The region list is also pruned: after subtable pruning, every
/// surviving subtable's `region_indexes` is walked to collect the set
/// of regions any kept tuple still references; unreferenced regions
/// are dropped from the region list and surviving subtables' region
/// indexes are renumbered through the remap.
///
/// Tolerates a source `referenced` set that names entries the source
/// MVS doesn't actually have. Those are silently skipped, but they
/// won't appear in the remap either, so the caller's
/// [`rewrite_component_record`](super::rewrite_component_record) will surface the orphan via the
/// `Unsupported` path.
pub(super) type MvsRemap = BTreeMap<(u16, u16), (u16, u16)>;

pub(super) fn prune_multi_var_store(
    src: &[u8],
    referenced: &BTreeSet<(u16, u16)>,
) -> Result<(MvsRemap, Option<Vec<u8>>), SubsetError> {
    let parsed = ParsedMvs::parse(src)
        .map_err(|_| SubsetError::Unsupported("VARC MVS malformed during prune"))?;

    // Group `referenced` by old outer index so we can decide which
    // subtables survive (those with at least one referenced inner).
    let mut by_outer: BTreeMap<u16, BTreeSet<u16>> = BTreeMap::new();
    for &(outer, inner) in referenced {
        by_outer.entry(outer).or_default().insert(inner);
    }

    // Walk source subtables in order; for each, build a list of kept
    // inner indices and assign a new outer index iff the kept list is
    // non-empty.
    let mut new_subtables: Vec<RewrittenMvsSubtable> = Vec::new();
    let mut remap: BTreeMap<(u16, u16), (u16, u16)> = BTreeMap::new();
    for (old_outer, sub) in parsed.subtables.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let old_outer_u16 = old_outer as u16;
        let Some(kept_inners) = by_outer.get(&old_outer_u16) else {
            continue;
        };
        // Keep only inners that exist in the source (defensive: an
        // out-of-range source ref means the source font is malformed).
        let mut kept_pairs: Vec<(u16, &[u8])> = Vec::new();
        for &inner in kept_inners {
            let Some(bytes) = sub.delta_sets.get(inner as usize).copied() else {
                continue;
            };
            kept_pairs.push((inner, bytes));
        }
        if kept_pairs.is_empty() {
            continue;
        }
        // Sorted by inner index: kept_inners is a BTreeSet so already
        // ascending; re-sort defensively in case future paths feed
        // unsorted refs in.
        kept_pairs.sort_by_key(|(i, _)| *i);

        #[allow(clippy::cast_possible_truncation)]
        let new_outer = new_subtables.len() as u16;
        let mut new_delta_sets: Vec<Vec<u8>> = Vec::with_capacity(kept_pairs.len());
        for (new_inner_idx, (old_inner, bytes)) in kept_pairs.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let new_inner = new_inner_idx as u16;
            remap.insert((old_outer_u16, *old_inner), (new_outer, new_inner));
            new_delta_sets.push((*bytes).to_vec());
        }
        new_subtables.push(RewrittenMvsSubtable {
            region_indexes: sub.region_indexes.clone(),
            delta_sets: new_delta_sets,
        });
    }

    if new_subtables.is_empty() {
        return Ok((remap, None));
    }

    // ---- Region-list prune ------------------------------------------
    //
    // After subtable pruning, walk every surviving subtable's
    // `region_indexes` to collect the set of regions any tuple still
    // references. Regions outside this set are unreachable and dropped
    // from the region list; surviving subtables' region indexes are
    // renumbered through the remap.
    //
    // Defensive: a malformed source MVS where a subtable's region
    // index points past the source region list would normally be
    // surfaced by the parser, but we re-check here and skip such
    // entries during region collection. Skipping is safer than failing
    // because the resulting subtable simply carries no contribution from
    // that region, mirroring the parser's tolerant behavior.
    let referenced_regions = collect_referenced_regions(&new_subtables);
    let src_regions = parse_region_list(&parsed.region_list_bytes)
        .map_err(|_| SubsetError::Unsupported("VARC MVS region list malformed during prune"))?;

    // Region remap: old region index -> new region index. Built by
    // walking referenced regions in ascending order so the new region
    // list preserves source order. That keeps output bytes stable for
    // round-trip determinism.
    let mut region_remap: BTreeMap<u16, u16> = BTreeMap::new();
    let mut kept_region_payloads: Vec<&[u8]> = Vec::new();
    for &old_ri in &referenced_regions {
        let Some(payload) = src_regions.get(old_ri as usize).copied() else {
            // Source's tuple referenced a region that doesn't exist:
            // skip it. The subtable's region_indexes will be filtered
            // below and the region effectively contributes zero, which
            // matches the parser's behavior for an OOB region.
            continue;
        };
        #[allow(clippy::cast_possible_truncation)]
        let new_ri = kept_region_payloads.len() as u16;
        region_remap.insert(old_ri, new_ri);
        kept_region_payloads.push(payload);
    }

    // Renumber each surviving subtable's region_indexes through the
    // remap. Drop indexes that lacked a kept region (defensive: if a
    // subtable ends up with zero region indexes after this filter,
    // every region it referenced was orphaned, which shouldn't happen
    // when the subtable prune is correct; we drop the subtable in that
    // case to keep the output structurally valid).
    let mut pruned_subtables: Vec<RewrittenMvsSubtable> = Vec::with_capacity(new_subtables.len());
    let mut outer_remap_collapse: BTreeMap<u16, u16> = BTreeMap::new();
    for (old_outer, sub) in new_subtables.into_iter().enumerate() {
        let mut new_region_indexes: Vec<u16> = Vec::with_capacity(sub.region_indexes.len());
        for ri in &sub.region_indexes {
            if let Some(&new_ri) = region_remap.get(ri) {
                new_region_indexes.push(new_ri);
            }
        }
        if new_region_indexes.is_empty() {
            // Defensive collapse: see comment above.
            continue;
        }
        #[allow(clippy::cast_possible_truncation)]
        let new_outer = pruned_subtables.len() as u16;
        if (old_outer as u16) != new_outer {
            outer_remap_collapse.insert(old_outer as u16, new_outer);
        }
        pruned_subtables.push(RewrittenMvsSubtable {
            region_indexes: new_region_indexes,
            delta_sets: sub.delta_sets,
        });
    }

    // If a subtable was dropped during the region collapse, fold the
    // outer-index shift into the existing `(outer, inner)` remap so the
    // record-rewrite path sees the final outer indices. In practice
    // this branch is dead (the subtable prune above already drops
    // empty subtables) but guards against future edits where a
    // subtable could survive subtable pruning yet collapse here.
    if !outer_remap_collapse.is_empty() {
        for (_, (no, _)) in remap.iter_mut() {
            if let Some(&final_no) = outer_remap_collapse.get(no) {
                *no = final_no;
            }
        }
    }

    if pruned_subtables.is_empty() {
        // Every subtable's regions were orphaned. The MVS becomes
        // effectively region-less and contributes nothing. Drop it
        // entirely so the caller emits no varStore offset.
        return Ok((BTreeMap::new(), None));
    }

    let new_region_list_bytes = build_region_list_bytes(&kept_region_payloads);
    let new_bytes = emit_multi_var_store(&new_region_list_bytes, &pruned_subtables);
    Ok((remap, Some(new_bytes)))
}

/// Builds an MVS region-list block from kept region payload slices.
/// Each `payload` is the raw bytes of one region (`u16 axisCount` +
/// axis triples), as returned by [`parse_region_list`]. Output layout:
///
/// ```text
///   u16       regionCount
///   Offset32  variationRegionOffsets[regionCount] (relative to block start)
///   <region payloads, concatenated in input order>
/// ```
pub(super) fn build_region_list_bytes(payloads: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    #[allow(clippy::cast_possible_truncation)]
    let count = payloads.len() as u16;
    out.extend_from_slice(&count.to_be_bytes());
    let off_table_start = out.len();
    for _ in payloads {
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    let mut starts: Vec<u32> = Vec::with_capacity(payloads.len());
    for p in payloads {
        #[allow(clippy::cast_possible_truncation)]
        let start = out.len() as u32;
        starts.push(start);
        out.extend_from_slice(p);
    }
    for (i, s) in starts.iter().enumerate() {
        let slot = off_table_start + i * 4;
        out[slot..slot + 4].copy_from_slice(&s.to_be_bytes());
    }
    out
}

/// One subtable in the rewritten MVS. After the region-list pruning
/// pass, `region_indexes` are renumbered into the new region-index
/// space; the verbatim copy left here by the subtable prune is
/// rewritten before [`emit_multi_var_store`] is called.
pub(super) struct RewrittenMvsSubtable {
    pub(super) region_indexes: Vec<u16>,
    pub(super) delta_sets: Vec<Vec<u8>>,
}

/// Walks every surviving subtable's `region_indexes` and returns the
/// set of regions any tuple still references. Drives the region list
/// prune: anything not in this set is unreachable after the MVS
/// subtable prune and can be dropped.
pub(super) fn collect_referenced_regions(subtables: &[RewrittenMvsSubtable]) -> BTreeSet<u16> {
    let mut out: BTreeSet<u16> = BTreeSet::new();
    for sub in subtables {
        for &ri in &sub.region_indexes {
            out.insert(ri);
        }
    }
    out
}

/// Parses an MVS region-list block into one byte slice per region. The
/// returned slices cover each region's body (`u16 axisCount` + axis
/// triples). They're spliced verbatim into the new region list, so the
/// pruner doesn't need to decode F2DOT14 coords.
///
/// The block layout (mirrors `src/tables/multi_var_store.rs`):
///
/// ```text
///   u16       regionCount
///   Offset32  variationRegionOffsets[regionCount] (relative to block start)
///   <region payloads>
/// ```
pub(super) fn parse_region_list(bytes: &[u8]) -> Result<Vec<&[u8]>, &'static str> {
    if bytes.len() < 2 {
        return Err("MVS region list header truncated");
    }
    let count = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
    let off_table_end = 2 + count * 4;
    if bytes.len() < off_table_end {
        return Err("MVS region list offsets truncated");
    }
    // Read region offsets (relative to region-list start). Their
    // ascending order plus the block end give us each region's byte
    // span.
    let mut offsets: Vec<usize> = Vec::with_capacity(count);
    for i in 0..count {
        let p = 2 + i * 4;
        let v = u32::from_be_bytes([bytes[p], bytes[p + 1], bytes[p + 2], bytes[p + 3]]) as usize;
        if v < off_table_end || v > bytes.len() {
            return Err("MVS region offset OOB");
        }
        offsets.push(v);
    }
    // Each region body extends to the next-greater offset, or to end.
    let mut sorted_bounds: Vec<usize> = offsets.clone();
    sorted_bounds.push(bytes.len());
    sorted_bounds.sort_unstable();
    sorted_bounds.dedup();
    let mut regions: Vec<&[u8]> = Vec::with_capacity(count);
    for &start in &offsets {
        let end = sorted_bounds
            .iter()
            .copied()
            .find(|m| *m > start)
            .unwrap_or(bytes.len());
        let region = bytes.get(start..end).ok_or("MVS region body OOB")?;
        // Sanity: at least the axisCount u16 must fit.
        if region.len() < 2 {
            return Err("MVS region axisCount truncated");
        }
        let axis_count = u16::from_be_bytes([region[0], region[1]]) as usize;
        let need = 2 + axis_count * 8;
        if region.len() < need {
            return Err("MVS region axes truncated");
        }
        // Trim any trailing padding the source may have between
        // regions: emit only the region's structural bytes so the
        // rewriter produces a tightly-packed region list.
        regions.push(&region[..need]);
    }
    Ok(regions)
}

/// Lightweight parse of the MVS used by the pruner. Mirrors the layout
/// in `src/tables/multi_var_store.rs`. We keep references to the
/// region-list bytes so the rewriter can splice them back in verbatim.
struct ParsedMvs<'a> {
    /// Bytes of the region list block: header (regionCount + offset
    /// table) + every region payload, concatenated as in the source.
    /// The pruner re-emits these as-is.
    region_list_bytes: Vec<u8>,
    subtables: Vec<ParsedMvsSubtable<'a>>,
}

struct ParsedMvsSubtable<'a> {
    region_indexes: Vec<u16>,
    delta_sets: Vec<&'a [u8]>,
}

impl<'a> ParsedMvs<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, &'static str> {
        if data.len() < 8 {
            return Err("MVS header truncated");
        }
        let format = u16::from_be_bytes([data[0], data[1]]);
        if format != 1 {
            return Err("MVS unsupported format");
        }
        let region_list_off = u32::from_be_bytes([data[2], data[3], data[4], data[5]]) as usize;
        let subtable_count = u16::from_be_bytes([data[6], data[7]]) as usize;
        let mut subtable_offsets: Vec<usize> = Vec::with_capacity(subtable_count);
        for i in 0..subtable_count {
            let off = 8 + i * 4;
            if off + 4 > data.len() {
                return Err("MVS subtable offset OOB");
            }
            let v = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
                as usize;
            subtable_offsets.push(v);
        }

        // Region list: bytes from `region_list_off` to the start of
        // the next block. The region list contains its own offset
        // array; for the pruner we don't need to decode regions, just
        // capture the byte range.
        let region_list_end = compute_block_end(
            data.len(),
            region_list_off,
            &[region_list_off]
                .iter()
                .chain(subtable_offsets.iter())
                .copied()
                .collect::<Vec<_>>(),
        );
        let region_list_bytes = data
            .get(region_list_off..region_list_end)
            .ok_or("MVS region list OOB")?
            .to_vec();

        // Subtables.
        let mut markers: Vec<usize> = subtable_offsets.clone();
        markers.push(region_list_off);
        markers.push(data.len());
        let mut subtables: Vec<ParsedMvsSubtable<'a>> = Vec::with_capacity(subtable_count);
        for &off in &subtable_offsets {
            let sub_end = compute_block_end(data.len(), off, &markers);
            let block = data.get(off..sub_end).ok_or("MVS subtable OOB")?;
            if block.len() < 3 {
                return Err("MVS subtable header truncated");
            }
            if block[0] != 1 {
                return Err("MVS unsupported subtable format");
            }
            let region_index_count = u16::from_be_bytes([block[1], block[2]]) as usize;
            let need = 3 + region_index_count * 2;
            if block.len() < need {
                return Err("MVS subtable region indexes OOB");
            }
            let mut region_indexes: Vec<u16> = Vec::with_capacity(region_index_count);
            for i in 0..region_index_count {
                let p = 3 + i * 2;
                region_indexes.push(u16::from_be_bytes([block[p], block[p + 1]]));
            }
            let idx_start = need;
            // Use the absolute offset within `data` so the returned
            // slices outlive `block` (they borrow from `data`, lifetime
            // `'a`). `parse_cff2_index` takes a single slice and
            // returns sub-slices of it; we feed it the tail of `data`
            // starting at this subtable's CFF2 INDEX block.
            let abs_off = off + idx_start;
            let idx_block: &'a [u8] = data.get(abs_off..sub_end).ok_or("MVS delta index OOB")?;
            let delta_sets: Vec<&'a [u8]> = if idx_block.len() < 4 {
                Vec::new()
            } else {
                parse_cff2_index(idx_block).map_err(|_| "MVS delta CFF2 INDEX malformed")?
            };
            subtables.push(ParsedMvsSubtable {
                region_indexes,
                delta_sets,
            });
        }

        Ok(Self {
            region_list_bytes,
            subtables,
        })
    }
}

/// Returns the end offset of a block that starts at `start`, given a
/// list of all block start offsets in the table. The block ends at the
/// next-greater offset, or at `data_len` if none follow.
fn compute_block_end(data_len: usize, start: usize, all_offsets: &[usize]) -> usize {
    let mut next = data_len;
    for &o in all_offsets {
        if o > start && o < next {
            next = o;
        }
    }
    next
}

/// Re-emits the MVS bytes from rewritten subtables. Region list is
/// spliced in verbatim from the source. Subtable offsets are computed
/// fresh; each subtable carries its CFF2 INDEX of delta-set bytes.
fn emit_multi_var_store(region_list_bytes: &[u8], subtables: &[RewrittenMvsSubtable]) -> Vec<u8> {
    // Header layout:
    //   u16  format = 1
    //   u32  regionListOffset
    //   u16  subtableCount
    //   u32  subtableOffsets[subtableCount]
    let header_len = 2 + 4 + 2 + subtables.len() * 4;

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    let region_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    #[allow(clippy::cast_possible_truncation)]
    let subtable_count = subtables.len() as u16;
    out.extend_from_slice(&subtable_count.to_be_bytes());
    let sub_off_slots_start = out.len();
    for _ in subtables {
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    debug_assert_eq!(out.len(), header_len);

    // Region list directly follows the header, 4-byte aligned (the
    // header already ends on a 4-byte boundary because subtable
    // offsets are u32).
    #[allow(clippy::cast_possible_truncation)]
    let region_off = out.len() as u32;
    out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_off.to_be_bytes());
    out.extend_from_slice(region_list_bytes);
    while out.len() % 4 != 0 {
        out.push(0);
    }

    // Subtables, each preceded by 4-byte alignment.
    for (i, sub) in subtables.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let sub_off = out.len() as u32;
        let slot = sub_off_slots_start + i * 4;
        out[slot..slot + 4].copy_from_slice(&sub_off.to_be_bytes());
        out.push(1); // format
        #[allow(clippy::cast_possible_truncation)]
        let ric = sub.region_indexes.len() as u16;
        out.extend_from_slice(&ric.to_be_bytes());
        for ri in &sub.region_indexes {
            out.extend_from_slice(&ri.to_be_bytes());
        }
        out.extend_from_slice(&build_cff2_index(&sub.delta_sets));
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    out
}
