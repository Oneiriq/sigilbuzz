//! MultiItemVariationStore pruning: drops unreferenced delta sets and
//! regions and re-emits the store with remapped indexes.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use super::{next_marker, parse_cff2_index, read_u32, try_build_cff2_index};
use crate::util::{WorkBudget, WORK_LIMIT};
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
///
/// Subtables and regions may share bytes in the source, and each kept
/// copy is written out separately. The copies are charged to a work
/// budget, and a store that would grow past it is rejected.
pub(super) type MvsRemap = BTreeMap<(u16, u16), (u16, u16)>;

pub(super) fn prune_multi_var_store(
    src: &[u8],
    referenced: &BTreeSet<(u16, u16)>,
) -> Result<(MvsRemap, Option<Vec<u8>>), SubsetError> {
    const TOO_LARGE: SubsetError =
        SubsetError::Unsupported("VARC MVS too large to prune; subtables share data");
    let budget = WorkBudget::new(WORK_LIMIT);
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
    for (old_outer, sub) in parsed.subtables() {
        let Some(kept_inners) = by_outer.get(&old_outer) else {
            continue;
        };
        // Keep only inners that exist in the source (defensive: an
        // out-of-range source ref means the source font is malformed).
        // `kept_inners` is a BTreeSet, so the pairs come out sorted by
        // inner index.
        let kept_pairs: Vec<(u16, &[u8])> = kept_inners
            .iter()
            .filter_map(|&inner| Some((inner, *sub.delta_sets.get(inner as usize)?)))
            .collect();
        if kept_pairs.is_empty() {
            continue;
        }
        let copied: usize = kept_pairs.iter().map(|(_, b)| b.len()).sum();
        if !budget.spend(sub.region_indexes.len() + copied) {
            return Err(TOO_LARGE);
        }

        // At most one subtable per source outer index, so this fits.
        let new_outer = new_subtables.len() as u16;
        let mut new_delta_sets: Vec<Vec<u8>> = Vec::with_capacity(kept_pairs.len());
        for (new_inner_idx, (old_inner, bytes)) in kept_pairs.iter().enumerate() {
            // At most one entry per source inner index, so this fits.
            let new_inner = new_inner_idx as u16;
            remap.insert((old_outer, *old_inner), (new_outer, new_inner));
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
        if !budget.spend(payload.len()) {
            return Err(TOO_LARGE);
        }
        // At most one entry per source region index, so this fits.
        let new_ri = kept_region_payloads.len() as u16;
        region_remap.insert(old_ri, new_ri);
        kept_region_payloads.push(payload);
    }

    // Renumber each surviving subtable's region_indexes through the
    // remap. Drop indexes that lacked a kept region. A subtable left
    // with zero region indexes has only orphaned regions, so it is
    // dropped to keep the output structurally valid.
    let mut pruned_subtables: Vec<RewrittenMvsSubtable> = Vec::with_capacity(new_subtables.len());
    let mut outer_remap_collapse: BTreeMap<u16, u16> = BTreeMap::new();
    for (old_outer, sub) in new_subtables.into_iter().enumerate() {
        let new_region_indexes: Vec<u16> = sub
            .region_indexes
            .iter()
            .filter_map(|ri| region_remap.get(ri).copied())
            .collect();
        if new_region_indexes.is_empty() {
            continue;
        }
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
    // record-rewrite path sees the final outer indices.
    if !outer_remap_collapse.is_empty() {
        for (no, _) in remap.values_mut() {
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
    let new_bytes = emit_multi_var_store(&new_region_list_bytes, &pruned_subtables)
        .ok_or(SubsetError::Unsupported("VARC MVS exceeds 4 GiB"))?;
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
///
/// Callers pass at most one payload per source region index, so the
/// count fits in 16 bits, and the prune budget keeps the block far
/// below 4 GiB.
pub(super) fn build_region_list_bytes(payloads: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    let count = payloads.len() as u16;
    out.extend_from_slice(&count.to_be_bytes());
    let off_table_start = out.len();
    out.resize(off_table_start + payloads.len() * 4, 0);
    for (i, p) in payloads.iter().enumerate() {
        let start = out.len() as u32;
        let slot = off_table_start + i * 4;
        if let Some(dst) = out.get_mut(slot..slot + 4) {
            dst.copy_from_slice(&start.to_be_bytes());
        }
        out.extend_from_slice(p);
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
    let count =
        usize::from(crate::layout::read_u16(bytes, 0).ok_or("MVS region list header truncated")?);
    let off_table_end = 2 + count * 4;
    let offset_table = bytes
        .get(2..off_table_end)
        .ok_or("MVS region list offsets truncated")?;
    // Read region offsets (relative to region-list start). Their
    // ascending order plus the block end give us each region's byte
    // span.
    let mut offsets: Vec<usize> = Vec::with_capacity(count);
    for c in offset_table.chunks_exact(4) {
        let v = u32::from_be_bytes([c[0], c[1], c[2], c[3]]) as usize;
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
        let end = next_marker(&sorted_bounds, start, bytes.len());
        let region = bytes.get(start..end).ok_or("MVS region body OOB")?;
        // Sanity: at least the axisCount u16 must fit.
        let axis_count = usize::from(
            crate::layout::read_u16(region, 0).ok_or("MVS region axisCount truncated")?,
        );
        // Trim any trailing padding the source may have between
        // regions: emit only the region's structural bytes so the
        // rewriter produces a tightly-packed region list.
        let body = region
            .get(..2 + axis_count * 8)
            .ok_or("MVS region axes truncated")?;
        regions.push(body);
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
    /// For each source outer index, the position of its parsed body in
    /// `bodies`. Subtables that share an offset share one body.
    subtable_body: Vec<usize>,
    bodies: Vec<ParsedMvsSubtable<'a>>,
}

struct ParsedMvsSubtable<'a> {
    region_indexes: Vec<u16>,
    delta_sets: Vec<&'a [u8]>,
}

impl<'a> ParsedMvs<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, &'static str> {
        let (Some(format), Ok(region_list_off), Some(subtable_count)) = (
            crate::layout::read_u16(data, 0),
            read_u32(data, 2),
            crate::layout::read_u16(data, 6),
        ) else {
            return Err("MVS header truncated");
        };
        if format != 1 {
            return Err("MVS unsupported format");
        }
        let region_list_off = region_list_off as usize;
        let subtable_count = usize::from(subtable_count);
        let offset_table = data
            .get(8..8 + subtable_count * 4)
            .ok_or("MVS subtable offset OOB")?;
        let subtable_offsets: Vec<usize> = offset_table
            .chunks_exact(4)
            .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]) as usize)
            .collect();

        // Every block ends at the next-greater block start, or at the
        // end of the data.
        let mut markers: Vec<usize> = subtable_offsets.clone();
        markers.push(region_list_off);
        markers.sort_unstable();
        markers.dedup();

        // Region list: bytes from `region_list_off` to the start of
        // the next block. The region list contains its own offset
        // array; for the pruner we don't need to decode regions, just
        // capture the byte range.
        let region_list_end = next_marker(&markers, region_list_off, data.len());
        let region_list_bytes = data
            .get(region_list_off..region_list_end)
            .ok_or("MVS region list OOB")?
            .to_vec();

        // Subtables. Blocks between distinct markers never overlap, so
        // parsing each distinct offset once keeps the work linear in
        // the data size.
        let mut body_at: BTreeMap<usize, usize> = BTreeMap::new();
        let mut bodies: Vec<ParsedMvsSubtable<'a>> = Vec::new();
        let mut subtable_body: Vec<usize> = Vec::with_capacity(subtable_count);
        for &off in &subtable_offsets {
            if let Some(&body) = body_at.get(&off) {
                subtable_body.push(body);
                continue;
            }
            let sub_end = next_marker(&markers, off, data.len());
            let parsed = Self::parse_subtable(data, off, sub_end)?;
            body_at.insert(off, bodies.len());
            subtable_body.push(bodies.len());
            bodies.push(parsed);
        }

        Ok(Self {
            region_list_bytes,
            subtable_body,
            bodies,
        })
    }

    /// Parses the subtable stored in `data[off..sub_end]`.
    fn parse_subtable(
        data: &'a [u8],
        off: usize,
        sub_end: usize,
    ) -> Result<ParsedMvsSubtable<'a>, &'static str> {
        let block = data.get(off..sub_end).ok_or("MVS subtable OOB")?;
        let (Some(&subtable_format), Some(region_index_count)) =
            (block.first(), crate::layout::read_u16(block, 1))
        else {
            return Err("MVS subtable header truncated");
        };
        if subtable_format != 1 {
            return Err("MVS unsupported subtable format");
        }
        let need = 3 + usize::from(region_index_count) * 2;
        let region_indexes: Vec<u16> = block
            .get(3..need)
            .ok_or("MVS subtable region indexes OOB")?
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        // Borrow the delta INDEX from `data` so the returned slices
        // carry lifetime `'a`.
        let idx_block: &'a [u8] = data.get(off + need..sub_end).ok_or("MVS delta index OOB")?;
        let delta_sets: Vec<&'a [u8]> = if idx_block.len() < 4 {
            Vec::new()
        } else {
            parse_cff2_index(idx_block).map_err(|_| "MVS delta CFF2 INDEX malformed")?
        };
        Ok(ParsedMvsSubtable {
            region_indexes,
            delta_sets,
        })
    }

    /// Iterates `(outer_index, subtable)` in source order.
    fn subtables(&self) -> impl Iterator<Item = (u16, &ParsedMvsSubtable<'a>)> + '_ {
        self.subtable_body
            .iter()
            .enumerate()
            .filter_map(|(outer, &body)| Some((u16::try_from(outer).ok()?, self.bodies.get(body)?)))
    }
}

/// Re-emits the MVS bytes from rewritten subtables. Region list is
/// spliced in verbatim from the source. Subtable offsets are computed
/// fresh; each subtable carries its CFF2 INDEX of delta-set bytes.
/// Returns `None` when the store does not fit its 32-bit offsets.
fn emit_multi_var_store(
    region_list_bytes: &[u8],
    subtables: &[RewrittenMvsSubtable],
) -> Option<Vec<u8>> {
    // Header layout:
    //   u16  format = 1
    //   u32  regionListOffset
    //   u16  subtableCount
    //   u32  subtableOffsets[subtableCount]
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    let region_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    let subtable_count = u16::try_from(subtables.len()).ok()?;
    out.extend_from_slice(&subtable_count.to_be_bytes());
    let sub_off_slots_start = out.len();
    out.resize(sub_off_slots_start + subtables.len() * 4, 0);

    // Region list directly follows the header, 4-byte aligned (the
    // header already ends on a 4-byte boundary because subtable
    // offsets are u32).
    let region_off = u32::try_from(out.len()).ok()?;
    out.get_mut(region_off_slot..region_off_slot + 4)?
        .copy_from_slice(&region_off.to_be_bytes());
    out.extend_from_slice(region_list_bytes);
    while out.len() % 4 != 0 {
        out.push(0);
    }

    // Subtables, each preceded by 4-byte alignment.
    for (i, sub) in subtables.iter().enumerate() {
        let sub_off = u32::try_from(out.len()).ok()?;
        let slot = sub_off_slots_start + i * 4;
        out.get_mut(slot..slot + 4)?
            .copy_from_slice(&sub_off.to_be_bytes());
        out.push(1); // format
        let ric = u16::try_from(sub.region_indexes.len()).ok()?;
        out.extend_from_slice(&ric.to_be_bytes());
        for ri in &sub.region_indexes {
            out.extend_from_slice(&ri.to_be_bytes());
        }
        out.extend_from_slice(&try_build_cff2_index(&sub.delta_sets)?);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    Some(out)
}
