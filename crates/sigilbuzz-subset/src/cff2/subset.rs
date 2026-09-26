//! CFF2 subsetting: rebuilds CharStrings, FDArray, FDSelect and the
//! Subr INDEXes for a kept-gid set.

use alloc::vec::Vec;

use super::{parse_cff2, serialise_cff2_top_dict, ParsedCff2};
use crate::cff::{
    compute_kept_subrs, emit_fd_select_auto, encode_index_cff2, kept_fd_positions,
    patch_dict_offset, renumber_charstring, serialise_font_dict, serialise_private_dict, walk_dict,
};
use crate::SubsetError;

/// Rebuilds a CFF2 table for the kept-gid subset.
///
/// CFF2's smaller surface (no Name / String / Encoding / charset
/// INDEXes, single inline Top DICT) means the orchestration here is a
/// strict simplification of [`crate::cff::subset_non_identity`]'s CID
/// path: walk FDSelect, renumber FDs, rebuild CharStrings + per-FD
/// Local Subr + Global Subr INDEXes, emit a fresh FDArray + FDSelect,
/// and patch Top DICT offsets to land the rebuilt sections in
/// deterministic order. The VariationStore (when present) rides
/// through verbatim.
///
/// Adobe-style CFF2 builds (single FontDict, no explicit FDSelect)
/// are accepted: the parser synthesizes an implicit "every gid -> FD 0"
/// mapping, and the rebuilder emits a real FDSelect format-0 in the
/// output (the spec requires FDSelect on disk when CIDCount > 0,
/// which any non-empty CFF2 satisfies).
///
/// Unreachable subroutines are pruned. Per-kept-FD `compute_kept_subrs`
/// runs the transitive closure over each FD's kept charstrings against
/// that FD's local INDEX; the global keep-set is the union across FDs.
/// The byte-stable charstring renumber pads narrower natural-width
/// operands back to the source's original byte slot
/// (`cff::encode_int_operand_at_width`), so a renumbered call
/// site never shifts the surrounding charstring even when the new
/// (post-bias) operand value is smaller than the original.
///
/// `kept_gids` must be sorted ascending and contain gid 0.
///
/// # Errors
///
/// Returns [`SubsetError::Unsupported`] when the source is malformed,
/// the kept set lacks gid 0, or a charstring references a dropped
/// subroutine.
pub fn subset_non_identity(cff_bytes: &[u8], kept_gids: &[u16]) -> Result<Vec<u8>, SubsetError> {
    let parsed = parse_cff2(cff_bytes)?;
    let n_glyphs = parsed.char_strings.len();
    if n_glyphs == 0 {
        return Err(SubsetError::Unsupported("CFF2 source has zero glyphs"));
    }
    for &g in kept_gids {
        if (g as usize) >= n_glyphs {
            return Err(SubsetError::Unsupported(
                "CFF2 kept gid past source numGlyphs",
            ));
        }
    }
    if kept_gids.first() != Some(&0u16) {
        return Err(SubsetError::Unsupported("CFF2 kept gid set must include 0"));
    }
    if parsed.fd_array.len() > 256 {
        return Err(SubsetError::Unsupported("CFF2 FDArray > 256 FDs"));
    }

    // Step 1+2: per-kept-gid old FD; sorted unique FD keep-set.
    let mut kept_fd_old: Vec<u8> = Vec::with_capacity(kept_gids.len());
    for &g in kept_gids {
        let fd = parsed
            .fd_select
            .get(g as usize)
            .copied()
            .ok_or(SubsetError::Unsupported("CFF2 FDSelect gid past end"))?;
        if (fd as usize) >= parsed.fd_array.len() {
            return Err(SubsetError::Unsupported(
                "CFF2 FDSelect FD index past FDArray length",
            ));
        }
        kept_fd_old.push(fd);
    }
    let mut kept_fds_sorted: Vec<u8> = kept_fd_old.clone();
    kept_fds_sorted.sort_unstable();
    kept_fds_sorted.dedup();

    let fd_pos_of = kept_fd_positions(&kept_fds_sorted);
    let new_fd_select: Vec<u8> = kept_fd_old
        .iter()
        .map(|&old| fd_pos_of[usize::from(old)] as u8)
        .collect();

    // Step 3: per-kept-FD subroutine keep-set. Each kept FD runs its
    // own `compute_kept_subrs` over the kept charstrings that route to
    // that FD against its FD-private local INDEX; the global keep-set
    // is the union across all kept FDs. The byte-stable renumber path
    // pads narrower-natural operands back to the source's original
    // byte slot when there's a wider form available (3-byte shortint,
    // 5-byte fixed, the two 2-byte forms). The single hole, a
    // `-107..=107` value that has to land in a 2-byte slot, has no
    // 2-byte representation in Type 2 charstrings, so for that case
    // the rewriter falls back to keeping every source subroutine
    // verbatim with identity renumbering (preserving the source
    // bias). The fallback is detected lazily inside
    // [`emit_with_keep_set`]: try the pruned attempt; on the specific
    // "cannot pad operand" error retry with identity.
    let (pruned_per_fd_kept_local, pruned_kept_global_idx) = {
        let mut kept_global_set: alloc::vec::Vec<bool> =
            alloc::vec![false; parsed.global_subrs.len()];
        let mut per_fd_kept_local: Vec<Vec<u32>> = Vec::with_capacity(kept_fds_sorted.len());
        for &old_fd in &kept_fds_sorted {
            let mut cs_for_this_fd: Vec<&[u8]> = Vec::new();
            for (i, &gid) in kept_gids.iter().enumerate() {
                if kept_fd_old[i] == old_fd {
                    cs_for_this_fd.push(parsed.char_strings[gid as usize]);
                }
            }
            let fd_local_subrs = &parsed.per_fd_local_subrs[old_fd as usize];
            let (kept_local, kept_global) =
                compute_kept_subrs(&cs_for_this_fd, fd_local_subrs, &parsed.global_subrs)?;
            for &gi in &kept_global {
                kept_global_set[gi as usize] = true;
            }
            per_fd_kept_local.push(kept_local);
        }
        let kept_global_idx: Vec<u32> = kept_global_set
            .iter()
            .enumerate()
            .filter_map(|(i, &k)| if k { Some(i as u32) } else { None })
            .collect();
        (per_fd_kept_local, kept_global_idx)
    };

    // Identity fallback keep-set: every source subr survives.
    let identity_per_fd_kept_local: Vec<Vec<u32>> = kept_fds_sorted
        .iter()
        .map(|&old_fd| (0..parsed.per_fd_local_subrs[old_fd as usize].len() as u32).collect())
        .collect();
    let identity_kept_global_idx: Vec<u32> = (0..parsed.global_subrs.len() as u32).collect();

    match emit_with_keep_set(
        cff_bytes,
        &parsed,
        kept_gids,
        &kept_fd_old,
        &kept_fds_sorted,
        &new_fd_select,
        &pruned_per_fd_kept_local,
        &pruned_kept_global_idx,
    ) {
        Ok(out) => Ok(out),
        Err(SubsetError::Unsupported("CFF renumber: cannot pad operand to original width")) => {
            emit_with_keep_set(
                cff_bytes,
                &parsed,
                kept_gids,
                &kept_fd_old,
                &kept_fds_sorted,
                &new_fd_select,
                &identity_per_fd_kept_local,
                &identity_kept_global_idx,
            )
        }
        Err(e) => Err(e),
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_with_keep_set(
    cff_bytes: &[u8],
    parsed: &ParsedCff2<'_>,
    kept_gids: &[u16],
    kept_fd_old: &[u8],
    kept_fds_sorted: &[u8],
    new_fd_select: &[u8],
    per_fd_kept_local: &[Vec<u32>],
    kept_global_idx: &[u32],
) -> Result<Vec<u8>, SubsetError> {
    let mut global_renumber: Vec<Option<u32>> = alloc::vec![None; parsed.global_subrs.len()];
    for (new_i, &old_i) in kept_global_idx.iter().enumerate() {
        global_renumber[old_i as usize] = Some(new_i as u32);
    }
    let new_global_count = kept_global_idx.len();
    let old_global_count = parsed.global_subrs.len();

    let mut per_fd_local_renumber: Vec<Vec<Option<u32>>> =
        Vec::with_capacity(kept_fds_sorted.len());
    for (i, &old_fd) in kept_fds_sorted.iter().enumerate() {
        let local_count = parsed.per_fd_local_subrs[old_fd as usize].len();
        let mut renumber: Vec<Option<u32>> = alloc::vec![None; local_count];
        for (new_i, &old_i) in per_fd_kept_local[i].iter().enumerate() {
            renumber[old_i as usize] = Some(new_i as u32);
        }
        per_fd_local_renumber.push(renumber);
    }

    // Step 5: rewrite each kept charstring.
    let fd_pos_of = kept_fd_positions(kept_fds_sorted);
    let mut new_charstrings: Vec<Vec<u8>> = Vec::with_capacity(kept_gids.len());
    for (i, &gid) in kept_gids.iter().enumerate() {
        let old_fd = kept_fd_old[i];
        let new_fd_pos = fd_pos_of[usize::from(old_fd)];
        let fd_local_subrs_old = &parsed.per_fd_local_subrs[old_fd as usize];
        let fd_local_renumber = &per_fd_local_renumber[new_fd_pos];
        let new_local_count = per_fd_kept_local[new_fd_pos].len();
        let mut cs = parsed.char_strings[gid as usize].to_vec();
        renumber_charstring(
            &mut cs,
            fd_local_subrs_old.len(),
            old_global_count,
            new_local_count,
            new_global_count,
            fd_local_renumber,
            &global_renumber,
        )?;
        new_charstrings.push(cs);
    }

    // Per-FD local subrs (renumbered).
    let mut new_per_fd_local_subrs: Vec<Vec<Vec<u8>>> = Vec::with_capacity(kept_fds_sorted.len());
    for (i, &old_fd) in kept_fds_sorted.iter().enumerate() {
        let fd_local_subrs_old = &parsed.per_fd_local_subrs[old_fd as usize];
        let kept_local_idx = &per_fd_kept_local[i];
        let fd_local_renumber = &per_fd_local_renumber[i];
        let new_local_count = kept_local_idx.len();
        let mut new_locals: Vec<Vec<u8>> = kept_local_idx
            .iter()
            .map(|&idx| fd_local_subrs_old[idx as usize].to_vec())
            .collect();
        for sub in &mut new_locals {
            renumber_charstring(
                sub,
                fd_local_subrs_old.len(),
                old_global_count,
                new_local_count,
                new_global_count,
                fd_local_renumber,
                &global_renumber,
            )?;
        }
        new_per_fd_local_subrs.push(new_locals);
    }

    // Renumber globals: pass an empty local table; calls into locals
    // from globals would cross-FD-collide and surface a hard error.
    let mut new_global_subrs: Vec<Vec<u8>> = kept_global_idx
        .iter()
        .map(|&i| parsed.global_subrs[i as usize].to_vec())
        .collect();
    let empty_local: Vec<Option<u32>> = Vec::new();
    for sub in &mut new_global_subrs {
        renumber_charstring(
            sub,
            0,
            old_global_count,
            0,
            new_global_count,
            &empty_local,
            &global_renumber,
        )?;
    }

    // FDSelect bytes.
    let fd_select_bytes = emit_fd_select_auto(new_fd_select);

    // Per-FD Font DICT body + Private DICT body.
    struct FdEmit {
        font_dict_body: Vec<u8>,
        font_dict_private_slot: Option<(usize, usize)>,
        new_private_body: Vec<u8>,
        new_priv_subrs_slot: Option<usize>,
    }
    let mut fd_emits: Vec<FdEmit> = Vec::with_capacity(kept_fds_sorted.len());
    for (i, &old_fd) in kept_fds_sorted.iter().enumerate() {
        let fd_bytes = parsed.fd_array[old_fd as usize];
        let fd_entries = walk_dict(fd_bytes)?;
        let (font_dict_body, font_dict_private_slot) = serialise_font_dict(&fd_entries);
        let priv_entries = if parsed.per_fd_private[old_fd as usize].is_empty() {
            Vec::new()
        } else {
            walk_dict(parsed.per_fd_private[old_fd as usize])?
        };
        let emit_subrs = !new_per_fd_local_subrs[i].is_empty();
        let (new_private_body, priv_slots) = serialise_private_dict(&priv_entries, emit_subrs);
        fd_emits.push(FdEmit {
            font_dict_body,
            font_dict_private_slot,
            new_private_body,
            new_priv_subrs_slot: priv_slots.subrs_slot,
        });
    }

    let fd_array_refs: Vec<&[u8]> = fd_emits
        .iter()
        .map(|f| f.font_dict_body.as_slice())
        .collect();
    let fd_array_index = encode_index_cff2(&fd_array_refs);

    let fd_count = fd_emits.len();
    let fd_index_off_size: usize = {
        let total: usize = fd_emits.iter().map(|f| f.font_dict_body.len()).sum();
        let last_off = 1 + total;
        if last_off <= 0xFF {
            1
        } else if last_off <= 0xFFFF {
            2
        } else if last_off <= 0x00FF_FFFF {
            3
        } else {
            4
        }
    };
    // CFF2 INDEX header: 4-byte u32 count + 1-byte offSize.
    let fd_index_data_start = 4 + 1 + (fd_count + 1) * fd_index_off_size;
    let mut fd_body_offsets_in_index: Vec<usize> = Vec::with_capacity(fd_count);
    let mut acc = fd_index_data_start;
    for f in &fd_emits {
        fd_body_offsets_in_index.push(acc);
        acc += f.font_dict_body.len();
    }

    // Top DICT: reuse source entries, swap targeted ops with placeholders.
    let top_entries = walk_dict(parsed.top_dict)?;
    let (top_dict_body, top_slots) = serialise_cff2_top_dict(&top_entries);

    // Global Subr INDEX (renumbered globals).
    let global_subr_refs: Vec<&[u8]> = new_global_subrs.iter().map(Vec::as_slice).collect();
    let global_subr_index = encode_index_cff2(&global_subr_refs);

    // CharStrings INDEX.
    let cs_refs: Vec<&[u8]> = new_charstrings.iter().map(Vec::as_slice).collect();
    let charstrings_index = encode_index_cff2(&cs_refs);

    let per_fd_local_index: Vec<Vec<u8>> = new_per_fd_local_subrs
        .iter()
        .map(|locals| {
            let refs: Vec<&[u8]> = locals.iter().map(Vec::as_slice).collect();
            encode_index_cff2(&refs)
        })
        .collect();

    // ---- Layout ---------------------------------------------------------
    // Header -> Top DICT -> Global Subr INDEX -> FDSelect -> CharStrings
    // INDEX -> FDArray INDEX -> [for each FD: Private DICT -> Local Subr
    // INDEX (when present)] -> VariationStore (when present).
    //
    // Header carries the rebuilt topDictLength, so we serialize the
    // header last using the new top_dict_body length.
    let mut out = Vec::with_capacity(cff_bytes.len());
    let new_top_dict_length = top_dict_body.len() as u16;
    let new_hdr_size = parsed.hdr_size as u8;
    out.push(2u8); // major
    out.push(0u8); // minor
    out.push(new_hdr_size);
    out.extend_from_slice(&new_top_dict_length.to_be_bytes());
    // If the source carried extra header bytes (hdr_size > 5), pad with
    // zero. Most CFF2 fonts in the wild use hdrSize = 5 exactly.
    while out.len() < parsed.hdr_size {
        out.push(0);
    }

    let top_dict_body_abs = out.len();
    out.extend_from_slice(&top_dict_body);

    out.extend_from_slice(&global_subr_index);

    let fd_select_abs = out.len();
    out.extend_from_slice(&fd_select_bytes);

    let charstrings_abs = out.len();
    out.extend_from_slice(&charstrings_index);

    let fd_array_abs = out.len();
    out.extend_from_slice(&fd_array_index);

    let mut per_fd_private_abs: Vec<usize> = Vec::with_capacity(fd_count);
    let mut per_fd_private_size: Vec<usize> = Vec::with_capacity(fd_count);
    let mut per_fd_local_subr_abs: Vec<Option<usize>> = Vec::with_capacity(fd_count);
    for (i, f) in fd_emits.iter().enumerate() {
        let private_abs = out.len();
        out.extend_from_slice(&f.new_private_body);
        per_fd_private_abs.push(private_abs);
        per_fd_private_size.push(f.new_private_body.len());
        if !per_fd_local_index[i].is_empty()
            && !new_per_fd_local_subrs[i].is_empty()
            && f.new_priv_subrs_slot.is_some()
        {
            let abs = out.len();
            out.extend_from_slice(&per_fd_local_index[i]);
            per_fd_local_subr_abs.push(Some(abs));
        } else {
            per_fd_local_subr_abs.push(None);
        }
    }

    let vstore_abs = if let Some(blob) = parsed.vstore_blob {
        let abs = out.len();
        out.extend_from_slice(blob);
        Some(abs)
    } else {
        None
    };

    // Patch Top DICT placeholders.
    if let Some(slot) = top_slots.char_strings_slot {
        let abs = top_dict_body_abs + slot;
        patch_dict_offset(&mut out, abs, charstrings_abs as i32);
    }
    if let Some(slot) = top_slots.fd_array_slot {
        let abs = top_dict_body_abs + slot;
        patch_dict_offset(&mut out, abs, fd_array_abs as i32);
    }
    if let Some(slot) = top_slots.fd_select_slot {
        let abs = top_dict_body_abs + slot;
        patch_dict_offset(&mut out, abs, fd_select_abs as i32);
    }
    if let (Some(slot), Some(vabs)) = (top_slots.vstore_slot, vstore_abs) {
        let abs = top_dict_body_abs + slot;
        patch_dict_offset(&mut out, abs, vabs as i32);
    }

    // Patch Font DICT Private slots and Subrs slots.
    for (i, f) in fd_emits.iter().enumerate() {
        if let Some((size_slot, off_slot)) = f.font_dict_private_slot {
            let body_abs_in_out = fd_array_abs + fd_body_offsets_in_index[i];
            patch_dict_offset(
                &mut out,
                body_abs_in_out + size_slot,
                per_fd_private_size[i] as i32,
            );
            patch_dict_offset(
                &mut out,
                body_abs_in_out + off_slot,
                per_fd_private_abs[i] as i32,
            );
        }
        if let (Some(slot), Some(local_abs)) = (f.new_priv_subrs_slot, per_fd_local_subr_abs[i]) {
            let private_abs = per_fd_private_abs[i];
            let abs = private_abs + slot;
            let rel = (local_abs - private_abs) as i32;
            patch_dict_offset(&mut out, abs, rel);
        }
    }

    Ok(out)
}
