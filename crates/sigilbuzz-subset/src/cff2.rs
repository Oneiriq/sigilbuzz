//! CFF2 subsetting helpers.
//!
//! CFF2 is CFF1 trimmed: no Name INDEX, no String INDEX, no Encoding,
//! no charset, no `endchar` operator. Only one Top DICT, stored
//! directly (no enclosing INDEX). VariationStore is optional. Every
//! CFF2 font is implicitly CID-keyed (FDArray mandatory). FDSelect is
//! conventionally required, but Adobe's CFF2 emitter elides it when
//! a font carries a single FontDict — the [`parse_cff2`] reader
//! synthesises the implicit "every gid → FD 0" mapping in that case.
//!
//! The Type 2 charstring scanner from [`crate::cff`] already accepts
//! both flavours — it stops at `OP_RETURN` / `OP_ENDCHAR` /
//! end-of-stream, and recognises `vsindex` / `blend` so CFF2-specific
//! ops don't confuse the operand-stack tracking. The byte-level emitter
//! primitives ([`crate::cff::encode_index`],
//! [`crate::cff::encode_dict_int`], [`crate::cff::renumber_charstring`])
//! are shared with CFF1 — CFF2's smaller surface (no Name / String /
//! Encoding / charset INDEXes) means the emitter is a strict subset of
//! the CFF1 layout pass.
//!
//! # Header
//!
//! ```text
//!   u8   major = 2
//!   u8   minor
//!   u8   hdrSize
//!   u16  topDictLength
//! ```
//!
//! Top DICT follows immediately. CharStrings INDEX, Global Subr INDEX,
//! FDArray, FDSelect, and the VariationStore are referenced by absolute
//! offset from that dict.

use alloc::vec::Vec;

use crate::cff::{
    compute_kept_subrs, emit_fd_select_auto, encode_dict_offset_placeholder, encode_index_cff2,
    parse_fd_select, patch_dict_offset, read_index_cff2, renumber_charstring, serialise_font_dict,
    serialise_private_dict, walk_dict, DictEntry, OP_CHARSTRINGS, OP_FD_ARRAY, OP_FD_SELECT,
    OP_PRIVATE, OP_SUBRS, OP_VSTORE,
};
use crate::SubsetError;

use sigilbuzz::tables::variation_store::ItemVariationStore;

/// CFF2 Top DICT placeholder slots.
///
/// CFF2 always carries CharStrings (op 17), FDArray (op 12 36),
/// FDSelect (op 12 37), and optionally VariationStore (op 24). All four
/// are absolute offsets from the start of the table, patched after
/// downstream sections land.
#[derive(Debug, Default, Clone)]
struct Cff2TopDictSlots {
    char_strings_slot: Option<usize>,
    fd_array_slot: Option<usize>,
    fd_select_slot: Option<usize>,
    vstore_slot: Option<usize>,
}

/// Serialises the CFF2 Top DICT body. CharStrings (17), FDArray (12 36),
/// FDSelect (12 37), and VariationStore (24) get 5-byte placeholders.
/// Other operators ride through verbatim.
fn serialise_cff2_top_dict(entries: &[DictEntry]) -> (Vec<u8>, Cff2TopDictSlots) {
    let mut out = Vec::new();
    let mut slots = Cff2TopDictSlots::default();
    for e in entries {
        match e.op {
            OP_CHARSTRINGS => {
                slots.char_strings_slot = Some(out.len());
                out.extend_from_slice(&encode_dict_offset_placeholder());
                out.push(17);
            }
            OP_FD_ARRAY => {
                slots.fd_array_slot = Some(out.len());
                out.extend_from_slice(&encode_dict_offset_placeholder());
                out.push(12);
                out.push(0x24);
            }
            OP_FD_SELECT => {
                slots.fd_select_slot = Some(out.len());
                out.extend_from_slice(&encode_dict_offset_placeholder());
                out.push(12);
                out.push(0x25);
            }
            OP_VSTORE => {
                slots.vstore_slot = Some(out.len());
                out.extend_from_slice(&encode_dict_offset_placeholder());
                out.push(24);
            }
            _ => {
                for o in &e.operands {
                    out.extend_from_slice(&o.raw);
                }
                if e.op >= 0x0C00 {
                    out.push(12);
                    out.push(e.op as u8);
                } else {
                    out.push(e.op as u8);
                }
            }
        }
    }
    (out, slots)
}

/// Captures the source CFF2 layout in raw form.
struct ParsedCff2<'a> {
    /// Header bytes (5+ bytes). Reproduced into the output with a
    /// rewritten topDictLength field.
    hdr_size: usize,
    /// Source top DICT body bytes.
    top_dict: &'a [u8],
    /// Charstrings INDEX entries.
    char_strings: Vec<&'a [u8]>,
    /// Global Subr INDEX entries.
    global_subrs: Vec<&'a [u8]>,
    /// FDArray INDEX entries (Font DICT bodies).
    fd_array: Vec<&'a [u8]>,
    /// Per-FD: (Private DICT bytes, Local Subrs INDEX entries).
    per_fd_private: Vec<&'a [u8]>,
    per_fd_local_subrs: Vec<Vec<&'a [u8]>>,
    /// FDSelect parsed into per-gid FD indices.
    fd_select: Vec<u8>,
    /// VariationStore bytes (the u16-length-prefixed payload, when
    /// present). Includes the 2-byte length prefix.
    vstore_blob: Option<&'a [u8]>,
}

fn parse_cff2(data: &[u8]) -> Result<ParsedCff2<'_>, SubsetError> {
    if data.len() < 5 {
        return Err(SubsetError::Unsupported("CFF2 header truncated"));
    }
    let major = data[0];
    if major != 2 {
        return Err(SubsetError::Unsupported("CFF2 major version != 2"));
    }
    let hdr_size = data[2] as usize;
    if hdr_size < 5 {
        return Err(SubsetError::Unsupported("CFF2 hdrSize < 5"));
    }
    let top_dict_length = u16::from_be_bytes([data[3], data[4]]) as usize;
    if data.len() < hdr_size + top_dict_length {
        return Err(SubsetError::Unsupported("CFF2 Top DICT past end"));
    }
    let top_dict = &data[hdr_size..hdr_size + top_dict_length];

    // Walk Top DICT for offsets.
    let entries = walk_dict(top_dict)?;
    let mut cs_off: Option<u32> = None;
    let mut fd_array_off: Option<u32> = None;
    let mut fd_select_off: Option<u32> = None;
    let mut vstore_off: Option<u32> = None;
    for e in &entries {
        match e.op {
            OP_CHARSTRINGS => {
                if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                    if v >= 0 {
                        cs_off = Some(v as u32);
                    }
                }
            }
            OP_FD_ARRAY => {
                if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                    if v >= 0 {
                        fd_array_off = Some(v as u32);
                    }
                }
            }
            OP_FD_SELECT => {
                if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                    if v >= 0 {
                        fd_select_off = Some(v as u32);
                    }
                }
            }
            OP_VSTORE => {
                if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                    if v >= 0 {
                        vstore_off = Some(v as u32);
                    }
                }
            }
            _ => {}
        }
    }

    // Global Subr INDEX is immediately after the Top DICT.
    let g_pos = hdr_size + top_dict_length;
    let (global_subrs, _) = read_index_cff2(data, g_pos)?;

    let cs_off = cs_off.ok_or(SubsetError::Unsupported(
        "CFF2 Top DICT missing CharStrings",
    ))? as usize;
    let (char_strings, _) = read_index_cff2(data, cs_off)?;
    let n_glyphs = char_strings.len();

    let fd_array_off =
        fd_array_off.ok_or(SubsetError::Unsupported("CFF2 Top DICT missing FDArray"))? as usize;
    let (fd_array, _) = read_index_cff2(data, fd_array_off)?;

    // Walk each Font DICT for its Private offset.
    let mut per_fd_private: Vec<&[u8]> = Vec::with_capacity(fd_array.len());
    let mut per_fd_local_subrs: Vec<Vec<&[u8]>> = Vec::with_capacity(fd_array.len());
    for fd_bytes in &fd_array {
        let fd_entries = walk_dict(fd_bytes)?;
        let mut priv_info: Option<(u32, u32)> = None;
        for e in &fd_entries {
            if e.op == OP_PRIVATE && e.operands.len() >= 2 {
                let s = e.operands[e.operands.len() - 2].int_value;
                let o = e.operands[e.operands.len() - 1].int_value;
                if let (Some(sv), Some(ov)) = (s, o) {
                    if sv >= 0 && ov >= 0 {
                        priv_info = Some((sv as u32, ov as u32));
                    }
                }
            }
        }
        let (priv_bytes, locals) = if let Some((size, off)) = priv_info {
            let off_u = off as usize;
            let size_u = size as usize;
            if off_u + size_u > data.len() {
                return Err(SubsetError::Unsupported("CFF2 Private DICT past end"));
            }
            let priv_bytes = &data[off_u..off_u + size_u];
            let priv_entries = walk_dict(priv_bytes)?;
            let mut subrs_rel: Option<u32> = None;
            for e in &priv_entries {
                if e.op == OP_SUBRS {
                    if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                        if v >= 0 {
                            subrs_rel = Some(v as u32);
                        }
                    }
                }
            }
            if let Some(rel) = subrs_rel {
                let abs = off_u + rel as usize;
                let (locals, _) = read_index_cff2(data, abs)?;
                (priv_bytes, locals)
            } else {
                (priv_bytes, Vec::new())
            }
        } else {
            (&[][..], Vec::new())
        };
        per_fd_private.push(priv_bytes);
        per_fd_local_subrs.push(locals);
    }

    // Adobe's CFF2 builds elide FDSelect when the font has a single
    // FontDict — the spec marks FDSelect optional in that case. When
    // missing AND the FDArray has exactly one entry, synthesise the
    // implicit "every gid → FD 0" mapping; any other shape is a
    // genuinely malformed CFF2 (multi-FD without FDSelect cannot
    // round-trip).
    let fd_select = if let Some(off) = fd_select_off {
        parse_fd_select(data, off as usize, n_glyphs)?
    } else if fd_array.len() == 1 {
        alloc::vec![0u8; n_glyphs]
    } else {
        return Err(SubsetError::Unsupported(
            "CFF2 multi-FD source missing FDSelect",
        ));
    };

    let vstore_blob = if let Some(off) = vstore_off {
        let off_u = off as usize;
        if off_u + 2 > data.len() {
            return Err(SubsetError::Unsupported("CFF2 VariationStore truncated"));
        }
        let len = u16::from_be_bytes([data[off_u], data[off_u + 1]]) as usize;
        let end = off_u + 2 + len;
        if end > data.len() {
            return Err(SubsetError::Unsupported("CFF2 VariationStore past end"));
        }
        Some(&data[off_u..end])
    } else {
        None
    };

    Ok(ParsedCff2 {
        hdr_size,
        top_dict,
        char_strings,
        global_subrs,
        fd_array,
        per_fd_private,
        per_fd_local_subrs,
        fd_select,
        vstore_blob,
    })
}

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
/// are accepted: the parser synthesises an implicit "every gid → FD 0"
/// mapping, and the rebuilder emits a real FDSelect format-0 in the
/// output (the spec requires FDSelect on disk when CIDCount > 0,
/// which any non-empty CFF2 satisfies).
///
/// Unreachable subroutines are pruned. Per-kept-FD `compute_kept_subrs`
/// runs the transitive closure over each FD's kept charstrings against
/// that FD's local INDEX; the global keep-set is the union across FDs.
/// The byte-stable charstring renumber pads narrower natural-width
/// operands back to the source's original byte slot
/// ([`crate::cff::encode_int_operand_at_width`]), so a renumbered call
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

    let mut fd_renumber: Vec<Option<u8>> = alloc::vec![None; parsed.fd_array.len()];
    for (new_i, &old_i) in kept_fds_sorted.iter().enumerate() {
        fd_renumber[old_i as usize] = Some(new_i as u8);
    }
    let new_fd_select: Vec<u8> = kept_fd_old
        .iter()
        .map(|&old| fd_renumber[old as usize].unwrap())
        .collect();

    // Step 3: per-kept-FD subroutine keep-set. Each kept FD runs its
    // own `compute_kept_subrs` over the kept charstrings that route to
    // that FD against its FD-private local INDEX; the global keep-set
    // is the union across all kept FDs. The byte-stable renumber path
    // pads narrower-natural operands back to the source's original
    // byte slot when there's a wider form available (3-byte shortint,
    // 5-byte fixed, the two 2-byte forms). The single hole — a
    // `-107..=107` value that has to land in a 2-byte slot — has no
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

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
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
    let mut new_charstrings: Vec<Vec<u8>> = Vec::with_capacity(kept_gids.len());
    for (i, &gid) in kept_gids.iter().enumerate() {
        let old_fd = kept_fd_old[i];
        let new_fd_pos = kept_fds_sorted.iter().position(|&f| f == old_fd).unwrap();
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

    // Renumber globals — pass an empty local table; calls into locals
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
        let priv_entries = if !parsed.per_fd_private[old_fd as usize].is_empty() {
            walk_dict(parsed.per_fd_private[old_fd as usize])?
        } else {
            Vec::new()
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

    // Top DICT — reuse source entries, swap targeted ops with placeholders.
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
    // Header → Top DICT → Global Subr INDEX → FDSelect → CharStrings
    // INDEX → FDArray INDEX → [for each FD: Private DICT → Local Subr
    // INDEX (when present)] → VariationStore (when present).
    //
    // Header carries the rebuilt topDictLength, so we serialise the
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

// ----------------------------------------------------------------------------
// Blend baking (instancing).
// ----------------------------------------------------------------------------

/// Bakes the `blend` operator out of every CharString in `cff_bytes` at
/// `coords`, producing a new CFF2 table that no longer references its
/// VariationStore. The result is a static CFF2 (no `blend`, no
/// `vsindex`) carrying the same outlines the source would draw at
/// `coords`.
///
/// Implementation strategy:
///
/// 1. **Inline-expand subroutines** into each charstring. CFF2 `blend`
///    can occur in a subr while masters were pushed by the caller —
///    a stack-tracking pass that doesn't inline would have to model
///    cross-subr stack flows, which is more bookkeeping than just
///    pasting the body. Inlining also lets us drop the local + global
///    Subr INDEX entirely.
/// 2. **Resolve `blend`**: pop `n`, then `n*nRegions` deltas, then `n`
///    masters; emit only the `n` resolved scalars
///    (`master + Σ scalar(coords)·delta`). The trailing count and the
///    delta operands are dropped.
/// 3. **Strip `vsindex`**: tracks which IVS subtable subsequent
///    `blend`s read from. Dropped from the output (no blend remains).
/// 4. Re-encode all push operands. After blend resolution masters can
///    become non-integer floats; we emit them via the `b0=255` 16.16
///    fixed form when fractional, integer forms otherwise.
///
/// The output drops VariationStore, GlobalSubr INDEX entries, and per-FD
/// LocalSubr INDEX entries. Top DICT keeps CharStrings / FDArray /
/// FDSelect; the VariationStore operator is omitted.
pub fn bake_at_coords(cff_bytes: &[u8], coords: &[f32]) -> Result<Vec<u8>, SubsetError> {
    let parsed = parse_cff2(cff_bytes)?;
    let n_glyphs = parsed.char_strings.len();
    if n_glyphs == 0 {
        return Err(SubsetError::Unsupported("CFF2 source has zero glyphs"));
    }

    // Parse the VariationStore (when present) so we can resolve blends.
    let ivs = if let Some(blob) = parsed.vstore_blob {
        // blob includes the 2-byte length prefix; the underlying
        // ItemVariationStore parser consumes the body without the
        // prefix.
        if blob.len() < 2 {
            return Err(SubsetError::Unsupported(
                "CFF2 VariationStore blob too short",
            ));
        }
        Some(
            ItemVariationStore::parse(&blob[2..])
                .map_err(|_| SubsetError::Unsupported("CFF2 VariationStore parse failed"))?,
        )
    } else {
        None
    };

    // Per-FD: bake every charstring with subr inlining and blend
    // resolution.
    let mut new_charstrings: Vec<Vec<u8>> = Vec::with_capacity(n_glyphs);
    for (gid, cs) in parsed.char_strings.iter().enumerate() {
        let fd = parsed
            .fd_select
            .get(gid)
            .copied()
            .ok_or(SubsetError::Unsupported("CFF2 FDSelect gid past end"))?;
        let local_subrs = parsed
            .per_fd_local_subrs
            .get(fd as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut baker = Baker::new(coords, ivs.as_ref(), &parsed.global_subrs, local_subrs);
        let baked = baker.bake_charstring(cs)?;
        new_charstrings.push(baked);
    }

    // Per-FD Font DICT bodies. We re-emit each Font DICT pointing at
    // its source Private DICT body (with Subrs op stripped — there are
    // no subrs anymore).
    struct FdBakeEmit {
        font_dict_body: Vec<u8>,
        font_dict_private_slot: Option<(usize, usize)>,
        new_private_body: Vec<u8>,
    }
    let mut fd_emits: Vec<FdBakeEmit> = Vec::with_capacity(parsed.fd_array.len());
    for fd_bytes in &parsed.fd_array {
        let fd_entries = walk_dict(fd_bytes)?;
        let (font_dict_body, font_dict_private_slot) = serialise_font_dict(&fd_entries);
        let priv_entries = if !parsed.per_fd_private[fd_emits.len()].is_empty() {
            walk_dict(parsed.per_fd_private[fd_emits.len()])?
        } else {
            Vec::new()
        };
        // No local subrs survive the bake.
        let (new_private_body, _) = serialise_private_dict(&priv_entries, false);
        fd_emits.push(FdBakeEmit {
            font_dict_body,
            font_dict_private_slot,
            new_private_body,
        });
    }

    // Empty global subrs after baking.
    let global_subr_index = encode_index_cff2(&[]);

    // FDArray INDEX.
    let fd_array_refs: Vec<&[u8]> = fd_emits
        .iter()
        .map(|f| f.font_dict_body.as_slice())
        .collect();
    let fd_array_index = encode_index_cff2(&fd_array_refs);

    // FDSelect re-emit (carry the source mapping; gid namespace is
    // unchanged).
    let fd_select_bytes = emit_fd_select_auto(&parsed.fd_select);

    // CharStrings INDEX.
    let cs_refs: Vec<&[u8]> = new_charstrings.iter().map(Vec::as_slice).collect();
    let charstrings_index = encode_index_cff2(&cs_refs);

    // Top DICT — VariationStore op omitted from the rebuilt body.
    let top_entries: Vec<DictEntry> = walk_dict(parsed.top_dict)?
        .into_iter()
        .filter(|e| e.op != OP_VSTORE)
        .collect();
    let (top_dict_body, top_slots) = serialise_cff2_top_dict(&top_entries);

    // Layout:
    //   header → Top DICT → Global Subr INDEX → FDSelect →
    //   CharStrings INDEX → FDArray INDEX → [per-FD: Private DICT].
    let mut out = Vec::with_capacity(cff_bytes.len() / 2);
    out.push(2u8);
    out.push(0u8);
    out.push(parsed.hdr_size as u8);
    out.extend_from_slice(&(top_dict_body.len() as u16).to_be_bytes());
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
    let fd_index_data_start = 4 + 1 + (fd_count + 1) * fd_index_off_size;
    let mut fd_body_offsets_in_index: Vec<usize> = Vec::with_capacity(fd_count);
    let mut acc = fd_index_data_start;
    for f in &fd_emits {
        fd_body_offsets_in_index.push(acc);
        acc += f.font_dict_body.len();
    }

    let mut per_fd_private_abs: Vec<usize> = Vec::with_capacity(fd_count);
    let mut per_fd_private_size: Vec<usize> = Vec::with_capacity(fd_count);
    for f in &fd_emits {
        per_fd_private_abs.push(out.len());
        per_fd_private_size.push(f.new_private_body.len());
        out.extend_from_slice(&f.new_private_body);
    }

    // Patch Top DICT placeholders.
    if let Some(slot) = top_slots.char_strings_slot {
        patch_dict_offset(&mut out, top_dict_body_abs + slot, charstrings_abs as i32);
    }
    if let Some(slot) = top_slots.fd_array_slot {
        patch_dict_offset(&mut out, top_dict_body_abs + slot, fd_array_abs as i32);
    }
    if let Some(slot) = top_slots.fd_select_slot {
        patch_dict_offset(&mut out, top_dict_body_abs + slot, fd_select_abs as i32);
    }
    debug_assert!(top_slots.vstore_slot.is_none());

    // Patch Font DICT Private slots.
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
    }

    Ok(out)
}

// Type 2 op codes consumed by the baker. Duplicates of crate::cff
// constants kept private to this module — the baker only reads, never
// renumbers.
const OP_HSTEM: u8 = 1;
const OP_VSTEM: u8 = 3;
const OP_VMOVETO: u8 = 4;
const OP_RLINETO: u8 = 5;
const OP_HLINETO: u8 = 6;
const OP_VLINETO: u8 = 7;
const OP_RRCURVETO: u8 = 8;
const OP_CALLSUBR: u8 = 10;
const OP_RETURN: u8 = 11;
const OP_ESCAPE: u8 = 12;
const OP_VSINDEX: u8 = 15;
const OP_BLEND: u8 = 16;
const OP_HSTEMHM: u8 = 18;
const OP_HINTMASK: u8 = 19;
const OP_CNTRMASK: u8 = 20;
const OP_RMOVETO: u8 = 21;
const OP_HMOVETO: u8 = 22;
const OP_VSTEMHM: u8 = 23;
const OP_RCURVELINE: u8 = 24;
const OP_RLINECURVE: u8 = 25;
const OP_VVCURVETO: u8 = 26;
const OP_HHCURVETO: u8 = 27;
const OP_SHORTINT: u8 = 28;
const OP_CALLGSUBR: u8 = 29;
const OP_VHCURVETO: u8 = 30;
const OP_HVCURVETO: u8 = 31;

const MAX_BAKE_DEPTH: u8 = 10;

/// CFF2 charstring baker. Walks a charstring's operand stream
/// resolving `blend` to scalars and inlining `callsubr` /
/// `callgsubr`. Emits a fresh charstring with no blend / vsindex /
/// subr-call ops.
struct Baker<'a> {
    coords: &'a [f32],
    ivs: Option<&'a ItemVariationStore<'a>>,
    global_subrs: &'a [&'a [u8]],
    local_subrs: &'a [&'a [u8]],
    /// Operand stack — floats so blend deltas don't lose precision.
    stack: Vec<f32>,
    /// Output charstring bytes.
    out: Vec<u8>,
    /// Current vsindex (which IVS subtable blend draws from).
    vsindex: u16,
    /// Running stem count, for hintmask / cntrmask tail size.
    stem_count: u32,
    /// Set after the first move/hint operator (used by stem tracking).
    seen_first_op: bool,
}

impl<'a> Baker<'a> {
    fn new(
        coords: &'a [f32],
        ivs: Option<&'a ItemVariationStore<'a>>,
        global_subrs: &'a [&'a [u8]],
        local_subrs: &'a [&'a [u8]],
    ) -> Self {
        Self {
            coords,
            ivs,
            global_subrs,
            local_subrs,
            stack: Vec::new(),
            out: Vec::new(),
            vsindex: 0,
            stem_count: 0,
            seen_first_op: false,
        }
    }

    fn bake_charstring(&mut self, cs: &[u8]) -> Result<Vec<u8>, SubsetError> {
        self.run(cs, 0)?;
        Ok(core::mem::take(&mut self.out))
    }

    fn run(&mut self, code: &[u8], depth: u8) -> Result<(), SubsetError> {
        if depth > MAX_BAKE_DEPTH {
            return Err(SubsetError::Unsupported(
                "CFF2 bake: subroutine depth exceeded",
            ));
        }
        let mut pos = 0;
        while pos < code.len() {
            let b0 = code[pos];
            if b0 >= 32 {
                let (val, len) = decode_operand_f32(code, pos)
                    .ok_or(SubsetError::Unsupported("CFF2 bake: operand truncated"))?;
                self.stack.push(val);
                pos += len;
                continue;
            }
            if b0 == OP_SHORTINT {
                if pos + 3 > code.len() {
                    return Err(SubsetError::Unsupported("CFF2 bake: shortint truncated"));
                }
                let v = i16::from_be_bytes([code[pos + 1], code[pos + 2]]);
                self.stack.push(f32::from(v));
                pos += 3;
                continue;
            }
            // Operator.
            match b0 {
                OP_BLEND => {
                    self.apply_blend()?;
                    pos += 1;
                }
                OP_VSINDEX => {
                    // Pop the new vsindex; emit nothing.
                    let v = self.stack.pop().ok_or(SubsetError::Unsupported(
                        "CFF2 bake: vsindex without operand",
                    ))?;
                    if v < 0.0 || v > f32::from(u16::MAX) {
                        return Err(SubsetError::Unsupported(
                            "CFF2 bake: vsindex operand out of range",
                        ));
                    }
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    {
                        self.vsindex = v as u16;
                    }
                    pos += 1;
                }
                OP_CALLSUBR => {
                    let idx = self.stack.pop().ok_or(SubsetError::Unsupported(
                        "CFF2 bake: callsubr without operand",
                    ))?;
                    #[allow(clippy::cast_possible_truncation)]
                    let raw = idx as i32;
                    let bias = subr_bias(self.local_subrs.len());
                    let abs = raw + bias;
                    if abs < 0 || (abs as usize) >= self.local_subrs.len() {
                        return Err(SubsetError::Unsupported(
                            "CFF2 bake: callsubr index out of range",
                        ));
                    }
                    let body = self.local_subrs[abs as usize];
                    self.run(body, depth + 1)?;
                    pos += 1;
                }
                OP_CALLGSUBR => {
                    let idx = self.stack.pop().ok_or(SubsetError::Unsupported(
                        "CFF2 bake: callgsubr without operand",
                    ))?;
                    #[allow(clippy::cast_possible_truncation)]
                    let raw = idx as i32;
                    let bias = subr_bias(self.global_subrs.len());
                    let abs = raw + bias;
                    if abs < 0 || (abs as usize) >= self.global_subrs.len() {
                        return Err(SubsetError::Unsupported(
                            "CFF2 bake: callgsubr index out of range",
                        ));
                    }
                    let body = self.global_subrs[abs as usize];
                    self.run(body, depth + 1)?;
                    pos += 1;
                }
                OP_RETURN => {
                    // CFF2 charstrings don't terminate on return; this
                    // appears inside a subr body. Stop walking the
                    // current body.
                    self.stack.clear();
                    return Ok(());
                }
                OP_HSTEM | OP_VSTEM | OP_HSTEMHM | OP_VSTEMHM => {
                    let n_pairs = (self.stack.len() as u32) / 2;
                    self.stem_count += n_pairs;
                    self.flush_stack();
                    self.out.push(b0);
                    self.seen_first_op = true;
                    pos += 1;
                }
                OP_HINTMASK | OP_CNTRMASK => {
                    let extra_pairs = (self.stack.len() as u32) / 2;
                    self.stem_count += extra_pairs;
                    self.flush_stack();
                    self.out.push(b0);
                    self.seen_first_op = true;
                    let mask_bytes = (self.stem_count as usize).div_ceil(8);
                    if pos + 1 + mask_bytes > code.len() {
                        return Err(SubsetError::Unsupported(
                            "CFF2 bake: hintmask tail truncated",
                        ));
                    }
                    self.out
                        .extend_from_slice(&code[pos + 1..pos + 1 + mask_bytes]);
                    pos += 1 + mask_bytes;
                }
                OP_ESCAPE => {
                    if pos + 2 > code.len() {
                        return Err(SubsetError::Unsupported("CFF2 bake: escape truncated"));
                    }
                    self.flush_stack();
                    self.out.push(b0);
                    self.out.push(code[pos + 1]);
                    self.seen_first_op = true;
                    pos += 2;
                }
                OP_RMOVETO | OP_HMOVETO | OP_VMOVETO | OP_RLINETO | OP_HLINETO | OP_VLINETO
                | OP_RRCURVETO | OP_HHCURVETO | OP_VVCURVETO | OP_HVCURVETO | OP_VHCURVETO
                | OP_RCURVELINE | OP_RLINECURVE => {
                    self.flush_stack();
                    self.out.push(b0);
                    self.seen_first_op = true;
                    pos += 1;
                }
                _ => {
                    return Err(SubsetError::Unsupported(
                        "CFF2 bake: unknown charstring operator",
                    ));
                }
            }
        }
        Ok(())
    }

    fn apply_blend(&mut self) -> Result<(), SubsetError> {
        // Stack: n master values, n*nRegions delta values, n itself on top.
        let n_raw = self.stack.pop().ok_or(SubsetError::Unsupported(
            "CFF2 bake: blend without count operand",
        ))?;
        if n_raw < 0.0 {
            return Err(SubsetError::Unsupported(
                "CFF2 bake: blend count operand negative",
            ));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = n_raw as usize;
        if n == 0 {
            return Ok(());
        }
        let n_regions = self
            .ivs
            .and_then(|s| s.variation_region_count(self.vsindex))
            .map(|c| c as usize)
            .unwrap_or(0);
        let total_deltas = n * n_regions;
        if self.stack.len() < n + total_deltas {
            return Err(SubsetError::Unsupported("CFF2 bake: blend stack underflow"));
        }
        let scalars = self
            .ivs
            .and_then(|s| s.region_scalars(self.vsindex, self.coords))
            .unwrap_or_default();
        let start = self.stack.len() - n - total_deltas;
        for i in 0..n {
            let mut accum = 0.0_f32;
            for j in 0..n_regions {
                let delta = self.stack[start + n + i * n_regions + j];
                if let Some(&s) = scalars.get(j) {
                    accum += s * delta;
                }
            }
            self.stack[start + i] += accum;
        }
        self.stack.truncate(start + n);
        Ok(())
    }

    /// Emits the operands currently sitting on the operand stack as
    /// CFF2 push bytes, then clears the stack. The numbers that
    /// survive blend resolution may be fractional; we use the 16.16
    /// fixed form (`b0=255`) when needed and the integer forms
    /// otherwise.
    fn flush_stack(&mut self) {
        let stack = core::mem::take(&mut self.stack);
        for v in stack {
            encode_charstring_number(v, &mut self.out);
        }
    }
}

/// Decodes a single push operand at `data[pos..]` into an f32 plus
/// byte-length. Mirrors `crate::cff::decode_operand` but preserves
/// fractional values from the 16.16-fixed (b0=255) form.
fn decode_operand_f32(data: &[u8], pos: usize) -> Option<(f32, usize)> {
    let b0 = *data.get(pos)?;
    if (32..=246).contains(&b0) {
        Some(((i32::from(b0) - 139) as f32, 1))
    } else if (247..=250).contains(&b0) {
        let b1 = *data.get(pos + 1)?;
        Some((
            ((i32::from(b0) - 247) * 256 + i32::from(b1) + 108) as f32,
            2,
        ))
    } else if (251..=254).contains(&b0) {
        let b1 = *data.get(pos + 1)?;
        Some((
            (-(i32::from(b0) - 251) * 256 - i32::from(b1) - 108) as f32,
            2,
        ))
    } else if b0 == OP_SHORTINT {
        // Type 2 shortint: 2-byte big-endian i16 follows. Required for
        // any integer in `[-32768, -1132]` ∪ `[1132, 32767]` (#197) —
        // omitting this branch breaks fonts with ≥ 1240 subrs whose
        // call indices spill into shortint encoding.
        let b1 = *data.get(pos + 1)?;
        let b2 = *data.get(pos + 2)?;
        let raw = i16::from_be_bytes([b1, b2]);
        Some((f32::from(raw), 3))
    } else if b0 == 255 {
        let bytes = data.get(pos + 1..pos + 5)?;
        let raw = i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        Some((raw as f32 / 65536.0, 5))
    } else {
        None
    }
}

/// Encodes a numeric value as a Type 2 push, using the shortest valid
/// form for the integer case and the 16.16 fixed form when fractional.
fn encode_charstring_number(v: f32, out: &mut Vec<u8>) {
    // If `v` is an exact integer in the i16 range, use an integer push.
    let rounded = v.round();
    let is_integer = (v - rounded).abs() < 1e-6;
    if is_integer && (-32768.0..=32767.0).contains(&rounded) {
        #[allow(clippy::cast_possible_truncation)]
        let iv = rounded as i32;
        if (-107..=107).contains(&iv) {
            out.push((iv + 139) as u8);
        } else if (108..=1131).contains(&iv) {
            let v0 = iv - 108;
            out.push(((v0 >> 8) + 247) as u8);
            out.push((v0 & 0xff) as u8);
        } else if (-1131..=-108).contains(&iv) {
            let v0 = -iv - 108;
            out.push(((v0 >> 8) + 251) as u8);
            out.push((v0 & 0xff) as u8);
        } else {
            #[allow(clippy::cast_possible_truncation)]
            let bytes = (iv as i16).to_be_bytes();
            out.push(OP_SHORTINT);
            out.push(bytes[0]);
            out.push(bytes[1]);
        }
    } else {
        // 16.16 fixed.
        #[allow(clippy::cast_possible_truncation)]
        let raw = (v * 65536.0).round() as i32;
        out.push(255);
        out.extend_from_slice(&raw.to_be_bytes());
    }
}

fn subr_bias(count: usize) -> i32 {
    if count < 1240 {
        107
    } else if count < 33_900 {
        1131
    } else {
        32_768
    }
}

// ----------------------------------------------------------------------------
// Partial-instancing CFF2 bake.
// ----------------------------------------------------------------------------

/// Per-source-subtable surviving slot info for the CFF2 partial-bake
/// charstring rewrite.
struct CffSubtableSurvivors {
    /// New outer index in the trimmed VarStore.
    new_outer: u16,
    /// Surviving slots in source-slot order: `(source_slot, scalar)`.
    /// Source delta at `source_slot` becomes `scalar * delta` in the
    /// rewritten blend.
    surviving: alloc::vec::Vec<(u16, f32)>,
}

/// Re-emits a CFF2 table with its `VariationStore` partially trimmed
/// to the Keep axes and every `blend` operator rewritten to reference
/// the new region count. The `vsindex` operator and source axis order
/// inside surviving subtables are preserved (subtables that collapse
/// entirely are elided; `vsindex` ops that pointed at them are
/// dropped, and any blend that runs against a collapsed subtable
/// emits no blend — its masters survive untouched, matching the
/// post-execution stack of `n, 0, blend`).
///
/// `coords` and `pins` follow the same shape as
/// [`crate::instance::bake_ivs_partial`]: one entry per source axis,
/// with Pin axes folded into the surviving deltas at `coords[i]` and
/// Keep axes carried through the new VarStore unchanged.
///
/// The bake inlines local + global subroutines into each charstring
/// (mirroring [`bake_at_coords`]'s strategy) so the rebuilt CFF2
/// carries empty Subr INDEXes — vsindex tracking inside subroutines
/// would otherwise require cross-call stack modelling.
///
/// # Errors
///
/// Returns [`SubsetError::Unsupported`] when the source is malformed,
/// when its VarStore parse fails, or when a charstring references a
/// dropped subroutine.
pub(crate) fn bake_cff2_partial(
    cff_bytes: &[u8],
    coords: &[f32],
    pins: &[crate::instance::AxisPin],
) -> Result<Vec<u8>, SubsetError> {
    let parsed = parse_cff2(cff_bytes)?;
    let n_glyphs = parsed.char_strings.len();
    if n_glyphs == 0 {
        return Err(SubsetError::Unsupported("CFF2 source has zero glyphs"));
    }

    // No VarStore → there are no blend ops to rewrite (CFF2 charstrings
    // can't blend without a VarStore). Re-emit the source as-is so the
    // caller's table-list always gets a deterministic CFF2 buffer.
    let Some(vstore_blob) = parsed.vstore_blob else {
        return Ok(cff_bytes.to_vec());
    };
    if vstore_blob.len() < 2 {
        return Err(SubsetError::Unsupported(
            "CFF2 VariationStore blob too short",
        ));
    }
    let src_ivs_bytes = &vstore_blob[2..];

    // Build the trimmed IVS via the IVS-bearing-table primitive.
    let (new_ivs_bytes, _remap) = crate::instance::bake_ivs_partial(src_ivs_bytes, coords, pins)
        .ok_or(SubsetError::Unsupported(
            "CFF2 VarStore partial bake failed",
        ))?;

    // Compute per-source-subtable surviving-slot info for the
    // charstring rewrite. We re-walk the source IVS rather than
    // extending bake_ivs_partial's return shape — the walk is cheap
    // (O(subtables × regions)) and keeps the IVS-bearing-table API
    // narrow.
    let survivors = compute_subtable_survivors(src_ivs_bytes, coords, pins).ok_or(
        SubsetError::Unsupported("CFF2 VarStore region projection failed"),
    )?;

    // Source IVS handle for blend stack arithmetic
    // (variation_region_count tells us how many deltas the source
    // blend op popped).
    let src_ivs = ItemVariationStore::parse(src_ivs_bytes)
        .map_err(|_| SubsetError::Unsupported("CFF2 VariationStore parse failed"))?;

    // Per-FD: rewrite each charstring with subr inlining + blend
    // rewrite.
    let mut new_charstrings: Vec<Vec<u8>> = Vec::with_capacity(n_glyphs);
    for (gid, cs) in parsed.char_strings.iter().enumerate() {
        let fd = parsed
            .fd_select
            .get(gid)
            .copied()
            .ok_or(SubsetError::Unsupported("CFF2 FDSelect gid past end"))?;
        let local_subrs = parsed
            .per_fd_local_subrs
            .get(fd as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut rewriter =
            PartialBaker::new(&src_ivs, &survivors, &parsed.global_subrs, local_subrs);
        let baked = rewriter.bake_charstring(cs)?;
        new_charstrings.push(baked);
    }

    // Per-FD Font DICT bodies. We re-emit each Font DICT pointing at
    // its source Private DICT body with the Subrs operator stripped —
    // no local subrs survive after inlining.
    struct FdEmit {
        font_dict_body: Vec<u8>,
        font_dict_private_slot: Option<(usize, usize)>,
        new_private_body: Vec<u8>,
    }
    let mut fd_emits: Vec<FdEmit> = Vec::with_capacity(parsed.fd_array.len());
    for (i, fd_bytes) in parsed.fd_array.iter().enumerate() {
        let fd_entries = walk_dict(fd_bytes)?;
        let (font_dict_body, font_dict_private_slot) = serialise_font_dict(&fd_entries);
        let priv_entries = if !parsed.per_fd_private[i].is_empty() {
            walk_dict(parsed.per_fd_private[i])?
        } else {
            Vec::new()
        };
        let (new_private_body, _) = serialise_private_dict(&priv_entries, false);
        fd_emits.push(FdEmit {
            font_dict_body,
            font_dict_private_slot,
            new_private_body,
        });
    }

    // Empty Subr INDEXes (subrs were inlined).
    let global_subr_index = encode_index_cff2(&[]);
    let fd_array_refs: Vec<&[u8]> = fd_emits
        .iter()
        .map(|f| f.font_dict_body.as_slice())
        .collect();
    let fd_array_index = encode_index_cff2(&fd_array_refs);
    let fd_select_bytes = emit_fd_select_auto(&parsed.fd_select);
    let cs_refs: Vec<&[u8]> = new_charstrings.iter().map(Vec::as_slice).collect();
    let charstrings_index = encode_index_cff2(&cs_refs);

    // Top DICT — keep VariationStore op (we still emit a VarStore).
    let top_entries: Vec<DictEntry> = walk_dict(parsed.top_dict)?;
    let (top_dict_body, top_slots) = serialise_cff2_top_dict(&top_entries);

    // Layout:
    //   header → Top DICT → Global Subr INDEX → FDSelect →
    //   CharStrings INDEX → FDArray INDEX → [per-FD: Private DICT] →
    //   VariationStore (length-prefixed).
    let mut out = Vec::with_capacity(cff_bytes.len());
    out.push(2u8);
    out.push(0u8);
    out.push(parsed.hdr_size as u8);
    out.extend_from_slice(&(top_dict_body.len() as u16).to_be_bytes());
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
    let fd_index_data_start = 4 + 1 + (fd_count + 1) * fd_index_off_size;
    let mut fd_body_offsets_in_index: Vec<usize> = Vec::with_capacity(fd_count);
    let mut acc = fd_index_data_start;
    for f in &fd_emits {
        fd_body_offsets_in_index.push(acc);
        acc += f.font_dict_body.len();
    }

    let mut per_fd_private_abs: Vec<usize> = Vec::with_capacity(fd_count);
    let mut per_fd_private_size: Vec<usize> = Vec::with_capacity(fd_count);
    for f in &fd_emits {
        per_fd_private_abs.push(out.len());
        per_fd_private_size.push(f.new_private_body.len());
        out.extend_from_slice(&f.new_private_body);
    }

    // VariationStore: 2-byte length prefix + new IVS body.
    let vstore_abs = out.len();
    let new_ivs_len: u16 = new_ivs_bytes
        .len()
        .try_into()
        .map_err(|_| SubsetError::Unsupported("CFF2 partial VarStore overflows u16 length"))?;
    out.extend_from_slice(&new_ivs_len.to_be_bytes());
    out.extend_from_slice(&new_ivs_bytes);

    // Patch Top DICT placeholders.
    if let Some(slot) = top_slots.char_strings_slot {
        patch_dict_offset(&mut out, top_dict_body_abs + slot, charstrings_abs as i32);
    }
    if let Some(slot) = top_slots.fd_array_slot {
        patch_dict_offset(&mut out, top_dict_body_abs + slot, fd_array_abs as i32);
    }
    if let Some(slot) = top_slots.fd_select_slot {
        patch_dict_offset(&mut out, top_dict_body_abs + slot, fd_select_abs as i32);
    }
    if let Some(slot) = top_slots.vstore_slot {
        patch_dict_offset(&mut out, top_dict_body_abs + slot, vstore_abs as i32);
    }

    // Patch Font DICT Private slots.
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
    }

    Ok(out)
}

/// Walks the source IVS to build per-source-subtable surviving-slot
/// info for the partial CFF2 bake. Mirrors the per-region/per-subtable
/// projection inside [`crate::instance::bake_ivs_partial`] but
/// surfaces the per-subtable shape the charstring rewrite needs.
///
/// Returns one entry per source subtable: `Some(survivors)` when the
/// subtable contains at least one surviving region (its blend ops can
/// re-emit), or `None` when every region drops (blend ops against
/// this subtable degenerate to a no-op).
///
/// Returns `None` (the outer `Option`) on malformed input — every
/// length and offset matches what `bake_ivs_partial` accepts.
fn compute_subtable_survivors(
    ivs_bytes: &[u8],
    coords: &[f32],
    pins: &[crate::instance::AxisPin],
) -> Option<Vec<Option<CffSubtableSurvivors>>> {
    if ivs_bytes.len() < 8 {
        return None;
    }
    let format = u16::from_be_bytes([ivs_bytes[0], ivs_bytes[1]]);
    if format != 1 {
        return None;
    }
    let region_list_off =
        u32::from_be_bytes([ivs_bytes[2], ivs_bytes[3], ivs_bytes[4], ivs_bytes[5]]) as usize;
    let subtable_count = u16::from_be_bytes([ivs_bytes[6], ivs_bytes[7]]) as usize;
    if ivs_bytes.len() < 8 + subtable_count * 4 {
        return None;
    }
    let mut subtable_offsets: Vec<usize> = Vec::with_capacity(subtable_count);
    for i in 0..subtable_count {
        let off = u32::from_be_bytes([
            ivs_bytes[8 + i * 4],
            ivs_bytes[8 + i * 4 + 1],
            ivs_bytes[8 + i * 4 + 2],
            ivs_bytes[8 + i * 4 + 3],
        ]) as usize;
        subtable_offsets.push(off);
    }

    if ivs_bytes.len() < region_list_off + 4 {
        return None;
    }
    let axis_count =
        u16::from_be_bytes([ivs_bytes[region_list_off], ivs_bytes[region_list_off + 1]]) as usize;
    let region_count = u16::from_be_bytes([
        ivs_bytes[region_list_off + 2],
        ivs_bytes[region_list_off + 3],
    ]) as usize;
    if pins.len() != axis_count || coords.len() != axis_count {
        return None;
    }
    let regions_start = region_list_off + 4;
    let region_size = axis_count * 6;
    if ivs_bytes.len() < regions_start + region_count * region_size {
        return None;
    }

    // Project each region onto Keep axes; track new index + scalar.
    // None → dropped at pin coords.
    let mut region_remap: Vec<Option<(u16, f32)>> = Vec::with_capacity(region_count);
    let mut next_new_idx: u16 = 0;
    for ri in 0..region_count {
        let base = regions_start + ri * region_size;
        let mut region: alloc::vec::Vec<(f32, f32, f32)> =
            alloc::vec::Vec::with_capacity(axis_count);
        for axis_i in 0..axis_count {
            let off = base + axis_i * 6;
            let s = read_f2dot14_at(ivs_bytes, off);
            let p = read_f2dot14_at(ivs_bytes, off + 2);
            let e = read_f2dot14_at(ivs_bytes, off + 4);
            region.push((s, p, e));
        }
        match crate::instance::project_region_onto_kept_axes(&region, pins, coords) {
            Some(p) => {
                region_remap.push(Some((next_new_idx, p.pin_scalar)));
                next_new_idx += 1;
            }
            None => region_remap.push(None),
        }
    }

    let mut per_subtable: Vec<Option<CffSubtableSurvivors>> = Vec::with_capacity(subtable_count);
    let mut new_outer: u16 = 0;
    for sub_off in &subtable_offsets {
        let sub_off = *sub_off;
        if ivs_bytes.len() < sub_off + 6 {
            return None;
        }
        let item_count = u16::from_be_bytes([ivs_bytes[sub_off], ivs_bytes[sub_off + 1]]);
        let region_index_count =
            u16::from_be_bytes([ivs_bytes[sub_off + 4], ivs_bytes[sub_off + 5]]) as usize;
        let ri_start = sub_off + 6;
        if ivs_bytes.len() < ri_start + region_index_count * 2 {
            return None;
        }
        let mut surviving: alloc::vec::Vec<(u16, f32)> = alloc::vec::Vec::new();
        for slot in 0..region_index_count {
            let old_ri = u16::from_be_bytes([
                ivs_bytes[ri_start + slot * 2],
                ivs_bytes[ri_start + slot * 2 + 1],
            ]);
            if let Some(Some((_new_ri, scalar))) = region_remap.get(old_ri as usize) {
                #[allow(clippy::cast_possible_truncation)]
                surviving.push((slot as u16, *scalar));
            }
        }
        if item_count == 0 || surviving.is_empty() {
            per_subtable.push(None);
        } else {
            per_subtable.push(Some(CffSubtableSurvivors {
                new_outer,
                surviving,
            }));
            new_outer += 1;
        }
    }
    Some(per_subtable)
}

/// Reads an F2DOT14 from `data[off..]`.
fn read_f2dot14_at(data: &[u8], off: usize) -> f32 {
    let raw = i16::from_be_bytes([data[off], data[off + 1]]);
    f32::from(raw) / 16384.0
}

// CFF2 charstring partial-rewrite baker. Walks the source charstring
// re-emitting every push operand verbatim and every operator
// verbatim, with `blend` (and the surrounding masters / deltas)
// rewritten to reference the surviving regions only. `vsindex` ops
// are dropped from the output and re-emitted lazily, immediately
// before each blend whose surviving subtable's `new_outer` differs
// from the last value we wrote.
//
// Subroutines are inlined into the output charstring so the rebuilt
// CFF2 carries empty Subr INDEXes — cross-call vsindex tracking
// would otherwise need stack modelling.
struct PartialBaker<'a> {
    src_ivs: &'a ItemVariationStore<'a>,
    survivors: &'a [Option<CffSubtableSurvivors>],
    global_subrs: &'a [&'a [u8]],
    local_subrs: &'a [&'a [u8]],
    out: Vec<u8>,
    /// Per stack entry: byte position in `out` where this entry's push
    /// began. Non-push values (results of a prior blend) carry the
    /// position of the original master push that fed that blend —
    /// the master bytes survive the truncate and remain the "anchor"
    /// for a later blend's truncate.
    stack_starts: Vec<usize>,
    /// Active source-vsindex (mirrors the source's running vsindex).
    src_vsindex: u16,
    /// Last-emitted new-outer in `out`. We emit `new_outer, vsindex`
    /// before each blend whose surviving subtable's `new_outer`
    /// differs from the last value we wrote.
    last_emitted_new_outer: Option<u16>,
    stem_count: u32,
}

impl<'a> PartialBaker<'a> {
    fn new(
        src_ivs: &'a ItemVariationStore<'a>,
        survivors: &'a [Option<CffSubtableSurvivors>],
        global_subrs: &'a [&'a [u8]],
        local_subrs: &'a [&'a [u8]],
    ) -> Self {
        Self {
            src_ivs,
            survivors,
            global_subrs,
            local_subrs,
            out: Vec::new(),
            stack_starts: Vec::new(),
            src_vsindex: 0,
            last_emitted_new_outer: None,
            stem_count: 0,
        }
    }

    fn bake_charstring(&mut self, cs: &[u8]) -> Result<Vec<u8>, SubsetError> {
        self.run(cs, 0)?;
        Ok(core::mem::take(&mut self.out))
    }

    fn run(&mut self, code: &[u8], depth: u8) -> Result<(), SubsetError> {
        if depth > MAX_BAKE_DEPTH {
            return Err(SubsetError::Unsupported(
                "CFF2 partial bake: subroutine depth exceeded",
            ));
        }
        let mut pos = 0;
        while pos < code.len() {
            let b0 = code[pos];
            if b0 >= 32 {
                let len = match b0 {
                    32..=246 => 1,
                    247..=254 => 2,
                    255 => 5,
                    _ => unreachable!(),
                };
                if pos + len > code.len() {
                    return Err(SubsetError::Unsupported(
                        "CFF2 partial bake: push operand truncated",
                    ));
                }
                self.stack_starts.push(self.out.len());
                self.out.extend_from_slice(&code[pos..pos + len]);
                pos += len;
                continue;
            }
            if b0 == OP_SHORTINT {
                if pos + 3 > code.len() {
                    return Err(SubsetError::Unsupported(
                        "CFF2 partial bake: shortint truncated",
                    ));
                }
                self.stack_starts.push(self.out.len());
                self.out.extend_from_slice(&code[pos..pos + 3]);
                pos += 3;
                continue;
            }
            match b0 {
                OP_BLEND => {
                    self.apply_blend()?;
                    pos += 1;
                }
                OP_VSINDEX => {
                    let start = self.stack_starts.pop().ok_or(SubsetError::Unsupported(
                        "CFF2 partial bake: vsindex without operand",
                    ))?;
                    let v = decode_operand_f32(&self.out, start)
                        .ok_or(SubsetError::Unsupported(
                            "CFF2 partial bake: vsindex operand decode failed",
                        ))?
                        .0;
                    if !(0.0..=f32::from(u16::MAX)).contains(&v) {
                        return Err(SubsetError::Unsupported(
                            "CFF2 partial bake: vsindex operand out of range",
                        ));
                    }
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    {
                        self.src_vsindex = v as u16;
                    }
                    self.out.truncate(start);
                    pos += 1;
                }
                OP_CALLSUBR => {
                    let start = self.stack_starts.pop().ok_or(SubsetError::Unsupported(
                        "CFF2 partial bake: callsubr without operand",
                    ))?;
                    let raw = decode_operand_f32(&self.out, start)
                        .ok_or(SubsetError::Unsupported(
                            "CFF2 partial bake: callsubr operand decode failed",
                        ))?
                        .0;
                    self.out.truncate(start);
                    #[allow(clippy::cast_possible_truncation)]
                    let raw_i = raw as i32;
                    let bias = subr_bias(self.local_subrs.len());
                    let abs = raw_i + bias;
                    if abs < 0 || (abs as usize) >= self.local_subrs.len() {
                        return Err(SubsetError::Unsupported(
                            "CFF2 partial bake: callsubr index out of range",
                        ));
                    }
                    let body = self.local_subrs[abs as usize];
                    self.run(body, depth + 1)?;
                    pos += 1;
                }
                OP_CALLGSUBR => {
                    let start = self.stack_starts.pop().ok_or(SubsetError::Unsupported(
                        "CFF2 partial bake: callgsubr without operand",
                    ))?;
                    let raw = decode_operand_f32(&self.out, start)
                        .ok_or(SubsetError::Unsupported(
                            "CFF2 partial bake: callgsubr operand decode failed",
                        ))?
                        .0;
                    self.out.truncate(start);
                    #[allow(clippy::cast_possible_truncation)]
                    let raw_i = raw as i32;
                    let bias = subr_bias(self.global_subrs.len());
                    let abs = raw_i + bias;
                    if abs < 0 || (abs as usize) >= self.global_subrs.len() {
                        return Err(SubsetError::Unsupported(
                            "CFF2 partial bake: callgsubr index out of range",
                        ));
                    }
                    let body = self.global_subrs[abs as usize];
                    self.run(body, depth + 1)?;
                    pos += 1;
                }
                OP_RETURN => {
                    // CFF2 subroutines do NOT own the caller's stack —
                    // they may leave operands on it for the caller to
                    // consume (#198). Clearing `stack_starts` here used
                    // to corrupt the caller's tracking and made any
                    // subr-pushes-deltas-then-returns pattern fail with
                    // "blend without count operand". Just hand control
                    // back; the inliner's caller continues from the
                    // current stack state.
                    return Ok(());
                }
                OP_HSTEM | OP_VSTEM | OP_HSTEMHM | OP_VSTEMHM => {
                    let n_pairs = (self.stack_starts.len() as u32) / 2;
                    self.stem_count += n_pairs;
                    self.stack_starts.clear();
                    self.out.push(b0);
                    pos += 1;
                }
                OP_HINTMASK | OP_CNTRMASK => {
                    let extra_pairs = (self.stack_starts.len() as u32) / 2;
                    self.stem_count += extra_pairs;
                    self.stack_starts.clear();
                    self.out.push(b0);
                    let mask_bytes = (self.stem_count as usize).div_ceil(8);
                    if pos + 1 + mask_bytes > code.len() {
                        return Err(SubsetError::Unsupported(
                            "CFF2 partial bake: hintmask tail truncated",
                        ));
                    }
                    self.out
                        .extend_from_slice(&code[pos + 1..pos + 1 + mask_bytes]);
                    pos += 1 + mask_bytes;
                }
                OP_ESCAPE => {
                    if pos + 2 > code.len() {
                        return Err(SubsetError::Unsupported(
                            "CFF2 partial bake: escape truncated",
                        ));
                    }
                    self.stack_starts.clear();
                    self.out.push(b0);
                    self.out.push(code[pos + 1]);
                    pos += 2;
                }
                OP_RMOVETO | OP_HMOVETO | OP_VMOVETO | OP_RLINETO | OP_HLINETO | OP_VLINETO
                | OP_RRCURVETO | OP_HHCURVETO | OP_VVCURVETO | OP_HVCURVETO | OP_VHCURVETO
                | OP_RCURVELINE | OP_RLINECURVE => {
                    self.stack_starts.clear();
                    self.out.push(b0);
                    pos += 1;
                }
                _ => {
                    return Err(SubsetError::Unsupported(
                        "CFF2 partial bake: unknown charstring operator",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Rewrites the trailing `n masters | n*old_k deltas | count |
    /// blend` block in `out` to `[vsindex] | n masters | n*new_k
    /// scaled deltas | count | blend`. When the active subtable
    /// collapsed entirely (no surviving regions), drops the deltas +
    /// count entirely and emits no blend (the masters become the
    /// post-blend stack values directly — equivalent to `n, 0, blend`
    /// post-execution).
    fn apply_blend(&mut self) -> Result<(), SubsetError> {
        // Pop the count operand.
        let count_start = self.stack_starts.pop().ok_or(SubsetError::Unsupported(
            "CFF2 partial bake: blend without count operand",
        ))?;
        let n_raw = decode_operand_f32(&self.out, count_start)
            .ok_or(SubsetError::Unsupported(
                "CFF2 partial bake: blend count decode failed",
            ))?
            .0;
        // Strip the count operand from `out` — we'll re-emit it below.
        self.out.truncate(count_start);
        if !(0.0..=f32::from(u16::MAX)).contains(&n_raw) {
            return Err(SubsetError::Unsupported(
                "CFF2 partial bake: blend count out of range",
            ));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = n_raw as usize;
        if n == 0 {
            return Ok(());
        }

        // Source subtable info.
        let old_k = self
            .src_ivs
            .variation_region_count(self.src_vsindex)
            .map(usize::from)
            .unwrap_or(0);
        let total_deltas = n * old_k;
        if self.stack_starts.len() < n + total_deltas {
            return Err(SubsetError::Unsupported(
                "CFF2 partial bake: blend stack underflow",
            ));
        }

        // Decode every delta operand from out (these are still live in
        // the byte stream; we'll truncate over them shortly).
        let delta_first_idx = self.stack_starts.len() - total_deltas;
        let mut src_deltas: Vec<f32> = Vec::with_capacity(total_deltas);
        for slot in 0..total_deltas {
            let start = self.stack_starts[delta_first_idx + slot];
            let v = decode_operand_f32(&self.out, start)
                .ok_or(SubsetError::Unsupported(
                    "CFF2 partial bake: delta decode failed",
                ))?
                .0;
            src_deltas.push(v);
        }

        // Truncate `out` to the byte position before the first delta
        // push — the n masters' bytes survive; everything from the
        // first delta to the end of the count operand is gone. Drop
        // the corresponding `stack_starts` entries.
        let truncate_to = self.stack_starts[delta_first_idx];
        self.stack_starts.truncate(delta_first_idx);
        self.out.truncate(truncate_to);

        // Look up the surviving subtable.
        let survivor = self
            .survivors
            .get(self.src_vsindex as usize)
            .and_then(|s| s.as_ref());
        let Some(survivor) = survivor else {
            // Subtable collapsed: emit no blend at all. The n masters
            // already sit in `out`. They'll be consumed by the next
            // outline op verbatim — equivalent to executing
            // `n, 0, blend` (count consumed, masters intact).
            return Ok(());
        };

        // Emit `new_outer, vsindex` before this blend when the active
        // outer-in-output differs. CFF2 default vsindex is 0 — if the
        // surviving outer is also 0 and we haven't emitted vsindex
        // yet, the prefix is a no-op.
        let need_vsindex = match self.last_emitted_new_outer {
            Some(prev) => prev != survivor.new_outer,
            None => survivor.new_outer != 0,
        };
        if need_vsindex {
            encode_charstring_number(f32::from(survivor.new_outer), &mut self.out);
            self.out.push(OP_VSINDEX);
            self.last_emitted_new_outer = Some(survivor.new_outer);
        }

        // Emit the new deltas in source-slot order, each scaled by the
        // pin_scalar.
        for i in 0..n {
            for &(slot, scalar) in &survivor.surviving {
                let src = src_deltas[i * old_k + slot as usize];
                let scaled = src * scalar;
                encode_charstring_number(scaled, &mut self.out);
            }
        }
        // Emit the count operand and blend op.
        encode_charstring_number(n as f32, &mut self.out);
        self.out.push(OP_BLEND);

        // The post-blend stack carries n result values. Their
        // `stack_starts` are the original master push positions; the
        // truncate above left those bytes in place. A subsequent
        // blend that pops these masters as its own masters will
        // truncate to the original master positions, leaving the
        // already-emitted [masters][deltas][count][BLEND] block alone
        // — sound for chained blend ops that build on prior blend
        // results.
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::cff::{emit_fd_select_format0, encode_int_operand, scan_subr_calls, subr_bias};

    #[test]
    fn cff2_charstring_without_endchar_is_walked_to_eof() {
        // CFF2 charstrings have no terminating endchar — the
        // scanner must complete on end-of-stream. Charstring: push 0,
        // callsubr (resolves to local subr 0 with default bias 107).
        let cs = [139u8, 10 /* OP_CALLSUBR */];
        let calls = scan_subr_calls(&cs, 0, 0).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].index_after_bias, 107);
    }

    #[test]
    fn cff2_blend_and_vsindex_dont_break_scanner() {
        // 0 0 rmoveto vsindex blend: the scanner clears the stack
        // and steps past these ops. Then 0 callsubr.
        let cs = [
            139, 139, 21, // rmoveto
            139, 15, // vsindex (consumes one)
            139, 16, // blend
            139, 10, // callsubr
        ];
        let calls = scan_subr_calls(&cs, 0, 0).unwrap();
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn analysis_helpers_reach_cff2_module() {
        // CFF2 inherits the bias / scanner / encoder helpers from
        // crate::cff. Smoke-check the re-exports compile and round
        // trip an integer operand encode through the shared encoder.
        assert_eq!(subr_bias(0), 107);
        let _enc = encode_int_operand(50);
    }

    /// Builds a synthetic CFF2 with `n_fds` Font DICTs, one charstring
    /// per gid, an explicit FDSelect map, no local subrs, and an
    /// optional VariationStore blob.
    fn build_synthetic_cff2(
        charstrings: &[&[u8]],
        fd_select: &[u8],
        vstore: Option<&[u8]>,
    ) -> Vec<u8> {
        assert_eq!(charstrings.len(), fd_select.len());
        let n_fds = (*fd_select.iter().max().unwrap_or(&0) as usize) + 1;
        let cs_index = encode_index_cff2(charstrings);
        let global_subr_index = encode_index_cff2(&[]);
        let fd_select_bytes = emit_fd_select_format0(fd_select);

        // Per-FD Private DICTs (one op for shape).
        let private_bodies: Vec<Vec<u8>> = (0..n_fds)
            .map(|_| alloc::vec![139u8 /* 0 */, 20u8 /* defaultWidthX */])
            .collect();

        // Font DICTs with placeholder Private (size + off) operands.
        let mut font_dict_bodies: Vec<Vec<u8>> = Vec::with_capacity(n_fds);
        let mut font_dict_priv_slots: Vec<(usize, usize)> = Vec::with_capacity(n_fds);
        for _ in 0..n_fds {
            let mut body = Vec::new();
            let size_slot = body.len();
            body.extend_from_slice(&encode_dict_offset_placeholder());
            let off_slot = body.len();
            body.extend_from_slice(&encode_dict_offset_placeholder());
            body.push(18);
            font_dict_priv_slots.push((size_slot, off_slot));
            font_dict_bodies.push(body);
        }

        let fd_array_refs: Vec<&[u8]> = font_dict_bodies.iter().map(Vec::as_slice).collect();
        let fd_array_index = encode_index_cff2(&fd_array_refs);

        // Top DICT — CharStrings, FDArray, FDSelect, optional VariationStore.
        let mut top: Vec<u8> = Vec::new();
        let cs_slot = top.len();
        top.extend_from_slice(&encode_dict_offset_placeholder());
        top.push(17);
        let fd_array_slot = top.len();
        top.extend_from_slice(&encode_dict_offset_placeholder());
        top.push(12);
        top.push(0x24);
        let fd_select_slot = top.len();
        top.extend_from_slice(&encode_dict_offset_placeholder());
        top.push(12);
        top.push(0x25);
        let vstore_slot = if vstore.is_some() {
            let s = top.len();
            top.extend_from_slice(&encode_dict_offset_placeholder());
            top.push(24);
            Some(s)
        } else {
            None
        };

        // Layout: header(5) | top dict | gsubr idx | fd_select | cs | fd array
        //   | per-FD private | optional vstore.
        let mut out = Vec::new();
        out.push(2u8); // major
        out.push(0u8); // minor
        out.push(5u8); // hdrSize
        let top_len = top.len() as u16;
        out.extend_from_slice(&top_len.to_be_bytes());
        let top_abs = out.len();
        out.extend_from_slice(&top);
        out.extend_from_slice(&global_subr_index);
        let fd_select_abs = out.len();
        out.extend_from_slice(&fd_select_bytes);
        let cs_abs = out.len();
        out.extend_from_slice(&cs_index);
        let fd_array_abs = out.len();
        out.extend_from_slice(&fd_array_index);

        // Per-FD private offsets within fd_array_index.
        let fd_index_off_size: usize = {
            let total: usize = font_dict_bodies.iter().map(Vec::len).sum();
            let last_off = 1 + total;
            if last_off <= 0xFF {
                1
            } else {
                2
            }
        };
        // CFF2 INDEX header: 4-byte u32 count + 1-byte offSize.
        let fd_index_data_start = 4 + 1 + (n_fds + 1) * fd_index_off_size;
        let mut fd_body_offsets_in_index: Vec<usize> = Vec::with_capacity(n_fds);
        let mut acc = fd_index_data_start;
        for body in &font_dict_bodies {
            fd_body_offsets_in_index.push(acc);
            acc += body.len();
        }

        let mut per_fd_priv_abs: Vec<usize> = Vec::with_capacity(n_fds);
        for pb in &private_bodies {
            per_fd_priv_abs.push(out.len());
            out.extend_from_slice(pb);
        }

        let vstore_abs = if let Some(v) = vstore {
            let a = out.len();
            // Wrap with u16 length prefix.
            let len = v.len() as u16;
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(v);
            Some(a)
        } else {
            None
        };

        // Patch top dict slots.
        patch_dict_offset(&mut out, top_abs + cs_slot, cs_abs as i32);
        patch_dict_offset(&mut out, top_abs + fd_array_slot, fd_array_abs as i32);
        patch_dict_offset(&mut out, top_abs + fd_select_slot, fd_select_abs as i32);
        if let (Some(slot), Some(vabs)) = (vstore_slot, vstore_abs) {
            patch_dict_offset(&mut out, top_abs + slot, vabs as i32);
        }

        // Patch each Font DICT's Private slots.
        for i in 0..n_fds {
            let body_abs_in_out = fd_array_abs + fd_body_offsets_in_index[i];
            let (size_slot, off_slot) = font_dict_priv_slots[i];
            patch_dict_offset(
                &mut out,
                body_abs_in_out + size_slot,
                private_bodies[i].len() as i32,
            );
            patch_dict_offset(
                &mut out,
                body_abs_in_out + off_slot,
                per_fd_priv_abs[i] as i32,
            );
        }

        out
    }

    #[test]
    fn cff2_orchestration_keeps_all_glyphs_when_kept_set_is_full() {
        // 3 glyphs, 1 FD. Subset to all gids = identity in shape.
        let cs0: &[u8] = &[14u8];
        let cs1: &[u8] = &[139, 139, 21];
        let cs2: &[u8] = &[139];
        let cff = build_synthetic_cff2(&[cs0, cs1, cs2], &[0, 0, 0], None);
        let new_cff = subset_non_identity(&cff, &[0, 1, 2]).unwrap();
        let parsed = parse_cff2(&new_cff).unwrap();
        assert_eq!(parsed.char_strings.len(), 3);
        assert_eq!(parsed.char_strings[0], cs0);
        assert_eq!(parsed.char_strings[1], cs1);
        assert_eq!(parsed.char_strings[2], cs2);
        assert_eq!(parsed.fd_array.len(), 1);
    }

    #[test]
    fn cff2_orchestration_drops_unused_glyphs() {
        let cs0: &[u8] = &[14u8];
        let cs1: &[u8] = &[139, 139, 21];
        let cs2: &[u8] = &[139];
        let cs3: &[u8] = &[139, 139, 22];
        let cff = build_synthetic_cff2(&[cs0, cs1, cs2, cs3], &[0, 0, 0, 0], None);
        let new_cff = subset_non_identity(&cff, &[0, 2]).unwrap();
        let parsed = parse_cff2(&new_cff).unwrap();
        assert_eq!(parsed.char_strings.len(), 2);
        assert_eq!(parsed.char_strings[1], cs2);
    }

    #[test]
    fn cff2_orchestration_drops_unused_fd() {
        // 3 glyphs across 2 FDs. Subset to gids whose FDs are all 0.
        let cs0: &[u8] = &[14u8];
        let cs1: &[u8] = &[139];
        let cs2: &[u8] = &[139];
        let cff = build_synthetic_cff2(&[cs0, cs1, cs2], &[0, 0, 1], None);
        let new_cff = subset_non_identity(&cff, &[0, 1]).unwrap();
        let parsed = parse_cff2(&new_cff).unwrap();
        assert_eq!(parsed.fd_array.len(), 1, "FD 1 should drop");
    }

    #[test]
    fn cff2_orchestration_renumbers_fd_select() {
        let cs0: &[u8] = &[14u8];
        let cs1: &[u8] = &[139];
        let cs2: &[u8] = &[139];
        let cff = build_synthetic_cff2(&[cs0, cs1, cs2], &[0, 1, 2], None);
        let new_cff = subset_non_identity(&cff, &[0, 2]).unwrap();
        let parsed = parse_cff2(&new_cff).unwrap();
        assert_eq!(parsed.fd_select, alloc::vec![0u8, 1]);
    }

    #[test]
    fn cff2_orchestration_preserves_vstore() {
        // VariationStore content is opaque to the orchestration —
        // ensure the bytes ride through the round trip verbatim.
        let cs0: &[u8] = &[14u8];
        let cs1: &[u8] = &[139];
        let vstore = alloc::vec![0u8, 1, 2, 3, 4, 5];
        let cff = build_synthetic_cff2(&[cs0, cs1], &[0, 0], Some(&vstore));
        let new_cff = subset_non_identity(&cff, &[0u16, 1]).unwrap();
        let parsed = parse_cff2(&new_cff).unwrap();
        // vstore_blob includes u16 length prefix + payload.
        let blob = parsed.vstore_blob.unwrap();
        assert_eq!(u16::from_be_bytes([blob[0], blob[1]]), vstore.len() as u16);
        assert_eq!(&blob[2..], &vstore[..]);
    }

    #[test]
    fn cff2_orchestration_rejects_kept_set_without_gid0() {
        let cs0: &[u8] = &[14u8];
        let cs1: &[u8] = &[139];
        let cff = build_synthetic_cff2(&[cs0, cs1], &[0, 0], None);
        let r = subset_non_identity(&cff, &[1u16]);
        assert!(r.is_err());
    }

    // --------------------------------------------------------------
    // bake_cff2_partial — VarStore + blend rewrite.
    // --------------------------------------------------------------

    use crate::instance::AxisPin;

    /// Builds a 2-axis IVS body (no length prefix; the CFF2 caller
    /// adds the prefix). Mirrors the tests in `instance.rs` but lives
    /// inside cff2 so we can drive `bake_cff2_partial` end-to-end.
    fn build_ivs2_for_cff2(
        regions: &[[(f32, f32, f32); 2]],
        subtables: &[(Vec<u16>, Vec<Vec<i16>>)],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        let region_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(subtables.len() as u16).to_be_bytes());
        let sub_slot_start = out.len();
        for _ in 0..subtables.len() {
            out.extend_from_slice(&0u32.to_be_bytes());
        }
        let region_off = out.len() as u32;
        out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_off.to_be_bytes());
        out.extend_from_slice(&2u16.to_be_bytes()); // axisCount
        out.extend_from_slice(&(regions.len() as u16).to_be_bytes());
        for region in regions {
            for (s, p, e) in region {
                let s_raw = (*s * 16384.0).round() as i16;
                let p_raw = (*p * 16384.0).round() as i16;
                let e_raw = (*e * 16384.0).round() as i16;
                out.extend_from_slice(&s_raw.to_be_bytes());
                out.extend_from_slice(&p_raw.to_be_bytes());
                out.extend_from_slice(&e_raw.to_be_bytes());
            }
        }
        for (i, (region_indexes, rows)) in subtables.iter().enumerate() {
            let sub_off = out.len() as u32;
            let slot = sub_slot_start + i * 4;
            out[slot..slot + 4].copy_from_slice(&sub_off.to_be_bytes());
            out.extend_from_slice(&(rows.len() as u16).to_be_bytes()); // itemCount
            out.extend_from_slice(&(region_indexes.len() as u16).to_be_bytes()); // wordDeltaCount
            out.extend_from_slice(&(region_indexes.len() as u16).to_be_bytes()); // regionIndexCount
            for ri in region_indexes {
                out.extend_from_slice(&ri.to_be_bytes());
            }
            for row in rows {
                assert_eq!(row.len(), region_indexes.len());
                for v in row {
                    out.extend_from_slice(&v.to_be_bytes());
                }
            }
        }
        out
    }

    #[test]
    fn bake_cff2_partial_pin_one_axis_keep_other_trims_varstore_axis_count() {
        // 2-axis IVS, one region peaking at (1, 1), one subtable with
        // one delta of 100. Charstring: 0 0 rmoveto, then push 0
        // (master), 100 (delta), 1 (count), blend → resolved value
        // becomes the next pushed scalar. Then endchar/return-equivalent
        // — CFF2 doesn't endchar; we let the implicit eof end the CS.
        //
        // Pin wght=1.0 (scalar 1.0 at peak), keep wdth.
        // Output IVS axisCount must be 1 (only wdth survives). Blend
        // op stays in the rebuilt charstring with new_k = 1 region.
        let ivs = build_ivs2_for_cff2(
            &[[(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)]],
            &[(alloc::vec![0], alloc::vec![alloc::vec![100]])],
        );
        // Charstring: 0 0 rmoveto, then push master(0), delta(100),
        // count(1), blend, then 0 hmoveto for shape (move op flushes
        // stack). Encoded values:
        //   0 → 139, 100 → push 100 (107..=1131 range; 100 < 108 so
        //         it's one byte 239=139+100? wait: 100 is in [-107,
        //         107] range, so 100+139=239)
        //   1 → 140, blend op = 16. rmoveto = 21. hmoveto = 22.
        let cs0: &[u8] = &[
            139, 139, 21, // 0 0 rmoveto
            139, 239, 140, 16, // 0 100 1 blend  (push master + delta + count + BLEND)
            139, 22, // 0 hmoveto
        ];
        let cff = build_synthetic_cff2(&[cs0], &[0], Some(&ivs));
        let pins = [AxisPin::Pin, AxisPin::Keep];
        let coords = [1.0, 0.0];
        let new_cff = bake_cff2_partial(&cff, &coords, &pins).expect("partial bake");
        let parsed = parse_cff2(&new_cff).expect("parse new cff2");
        let blob = parsed.vstore_blob.expect("varstore survives");
        let store = ItemVariationStore::parse(&blob[2..]).expect("parse new ivs");
        assert_eq!(store.axis_count(), 1, "axis trimmed to wdth");
        assert_eq!(store.region_count(), 1, "region survives");
    }

    #[test]
    fn bake_cff2_partial_no_varstore_passes_through() {
        // CFF2 without VariationStore → bake is a no-op (the source
        // bytes round-trip unchanged). Charstring rewrite has nothing
        // to do because charstrings can't blend without an IVS.
        let cs0: &[u8] = &[139, 139, 21];
        let cff = build_synthetic_cff2(&[cs0], &[0], None);
        let pins = [AxisPin::Keep];
        let coords = [0.0_f32];
        let new_cff = bake_cff2_partial(&cff, &coords, &pins).expect("no-op partial bake");
        assert_eq!(new_cff, cff, "no-VarStore path returns source bytes");
    }

    #[test]
    fn bake_cff2_partial_pin_outside_region_drops_blend() {
        // Region peaks at wght=1, wdth=1. Pin wght=0 (region drops
        // because the wght axis support is 0 at coord 0 with peak 1).
        // The rebuilt charstring's blend op must NOT reference the
        // dropped subtable — the subtable collapse → no blend
        // emitted; the master survives as the post-blend value.
        let ivs = build_ivs2_for_cff2(
            &[[(0.5, 1.0, 1.0), (0.0, 1.0, 1.0)]],
            &[(alloc::vec![0], alloc::vec![alloc::vec![100]])],
        );
        // Charstring with one blend op against subtable 0.
        let cs0: &[u8] = &[
            139, 139, 21, // 0 0 rmoveto
            139, 239, 140, 16, // 0 100 1 blend
            22, // hmoveto (consumes whatever's on stack as dx)
        ];
        let cff = build_synthetic_cff2(&[cs0], &[0], Some(&ivs));
        let pins = [AxisPin::Pin, AxisPin::Keep];
        let coords = [0.0, 0.0];
        let new_cff = bake_cff2_partial(&cff, &coords, &pins).expect("partial bake");
        let parsed = parse_cff2(&new_cff).expect("parse");
        let blob = parsed.vstore_blob.expect("vstore present");
        let store = ItemVariationStore::parse(&blob[2..]).expect("ivs parses");
        // Subtable collapsed → zero subtables in the new IVS.
        assert_eq!(store.subtable_count(), 0, "subtable elided");
        // Charstring's BLEND op must be gone (subtable collapsed).
        let cs_baked = parsed.char_strings[0];
        assert!(
            !cs_baked.contains(&16u8),
            "blend op dropped when subtable collapses; got {:?}",
            cs_baked
        );
    }

    /// Regression for #198: when an inlined local subroutine returns
    /// while leaving operands on the stack (a perfectly legal CFF2
    /// pattern — subrs commonly stash deltas / masters for the caller
    /// to blend against), `OP_RETURN` used to call
    /// `self.stack_starts.clear()`, discarding the caller's tracking
    /// for those operands. The next blend in the caller would then
    /// underflow / mis-decode operands and surface as an opaque error
    /// or worse: silently emit a malformed charstring.
    #[test]
    fn bake_cff2_partial_subr_return_preserves_caller_stack_tracking() {
        // 1-axis IVS, one region peaking at (1.0,). One subtable, one
        // delta (50). Charstring:
        //   0 0 rmoveto
        //   0 callsubr        # subr pushes "0 50 1" then returns
        //   blend             # caller consumes 0(master) 50(delta) 1(count)
        //   22                # hmoveto (consumes blend result)
        //
        // Local subr 0 body: 139 (push 0), 189 (push 50), 140 (push 1),
        // 11 (return). Operands stay on the stack across the return.
        let ivs = build_ivs1_for_cff2(
            &[[(0.0, 1.0, 1.0)]],
            &[(alloc::vec![0], alloc::vec![alloc::vec![50i16]])],
        );
        // Bias for count<1240 is 107. Calling subr 0 requires push -107
        // which encodes via SHORTINT only (a separate bug #197). Sidestep:
        // build a font with N=108 subrs, place the trampoline at index 107,
        // and call it with raw=0 (1-byte push 139). 0 + 107 = 107 → subr 107.
        let mut local_subrs_storage: Vec<&[u8]> = Vec::new();
        for _ in 0..107 {
            local_subrs_storage.push(&[11u8]); // empty subr → return
        }
        // Subr 107: pushes 0, 50, 1, then returns. Stack on return: 3 entries.
        local_subrs_storage.push(&[139u8, 189, 140, 11]);
        let cff = build_synthetic_cff2_with_local_subrs(
            &[
                // gid 0 charstring: 0 0 rmoveto, push 0 (=139), callsubr (10),
                // blend (16), hmoveto (22).
                &[139u8, 139, 21, 139, 10, 16, 22],
            ],
            &[0u8],
            &local_subrs_storage,
            Some(&ivs),
        );
        let pins = [AxisPin::Pin];
        let coords = [1.0_f32];
        // Must NOT error. Bug: stack_starts gets cleared by OP_RETURN
        // even though the subr left "0 50 1" on the stack for the
        // caller's blend, so the blend underflows and surfaces as
        // "blend without count operand".
        let _new_cff =
            bake_cff2_partial(&cff, &coords, &pins).expect("inlined subr return preserves stack");
    }

    /// Builds a 1-axis IVS body.
    fn build_ivs1_for_cff2(
        regions: &[[(f32, f32, f32); 1]],
        subtables: &[(Vec<u16>, Vec<Vec<i16>>)],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        let region_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(subtables.len() as u16).to_be_bytes());
        let sub_slot_start = out.len();
        for _ in 0..subtables.len() {
            out.extend_from_slice(&0u32.to_be_bytes());
        }
        let region_off = out.len() as u32;
        out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_off.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
        out.extend_from_slice(&(regions.len() as u16).to_be_bytes());
        for region in regions {
            for (s, p, e) in region {
                let s_raw = (*s * 16384.0).round() as i16;
                let p_raw = (*p * 16384.0).round() as i16;
                let e_raw = (*e * 16384.0).round() as i16;
                out.extend_from_slice(&s_raw.to_be_bytes());
                out.extend_from_slice(&p_raw.to_be_bytes());
                out.extend_from_slice(&e_raw.to_be_bytes());
            }
        }
        for (i, (region_indexes, rows)) in subtables.iter().enumerate() {
            let sub_off = out.len() as u32;
            let slot = sub_slot_start + i * 4;
            out[slot..slot + 4].copy_from_slice(&sub_off.to_be_bytes());
            out.extend_from_slice(&(rows.len() as u16).to_be_bytes()); // itemCount
            out.extend_from_slice(&(region_indexes.len() as u16).to_be_bytes()); // wordDeltaCount
            out.extend_from_slice(&(region_indexes.len() as u16).to_be_bytes()); // regionIndexCount
            for ri in region_indexes {
                out.extend_from_slice(&ri.to_be_bytes());
            }
            for row in rows {
                for v in row {
                    out.extend_from_slice(&v.to_be_bytes());
                }
            }
        }
        out
    }

    /// Builds a synthetic CFF2 with local subroutines per Font DICT.
    fn build_synthetic_cff2_with_local_subrs(
        charstrings: &[&[u8]],
        fd_select: &[u8],
        local_subrs: &[&[u8]],
        vstore: Option<&[u8]>,
    ) -> Vec<u8> {
        assert_eq!(charstrings.len(), fd_select.len());
        let n_fds = (*fd_select.iter().max().unwrap_or(&0) as usize) + 1;
        let cs_index = encode_index_cff2(charstrings);
        let global_subr_index = encode_index_cff2(&[]);
        let fd_select_bytes = emit_fd_select_format0(fd_select);

        // Per-FD Private DICT: must include Subrs op (19) pointing at a
        // local subr INDEX in the same blob. Each Private DICT is laid
        // out as: [subrs offset placeholder] 19 [defaultWidth=0] 20.
        let local_subr_index = encode_index_cff2(local_subrs);
        // Build Private DICTs with placeholder Subrs offsets — patched
        // post-layout. Use a 5-byte op255 placeholder so the offset
        // slot is fixed-width (matching the rest of the test scaffold).
        let private_subrs_slot_in_body = 0usize;
        let private_bodies: Vec<Vec<u8>> = (0..n_fds)
            .map(|_| {
                let mut p = Vec::new();
                p.extend_from_slice(&encode_dict_offset_placeholder()); // Subrs offset
                p.push(19); // op = Subrs
                p.push(139); // defaultWidthX = 0
                p.push(20);
                p
            })
            .collect();

        let mut font_dict_bodies: Vec<Vec<u8>> = Vec::with_capacity(n_fds);
        let mut font_dict_priv_slots: Vec<(usize, usize)> = Vec::with_capacity(n_fds);
        for _ in 0..n_fds {
            let mut body = Vec::new();
            let size_slot = body.len();
            body.extend_from_slice(&encode_dict_offset_placeholder());
            let off_slot = body.len();
            body.extend_from_slice(&encode_dict_offset_placeholder());
            body.push(18);
            font_dict_priv_slots.push((size_slot, off_slot));
            font_dict_bodies.push(body);
        }

        let fd_array_refs: Vec<&[u8]> = font_dict_bodies.iter().map(Vec::as_slice).collect();
        let fd_array_index = encode_index_cff2(&fd_array_refs);

        // Top DICT.
        let mut top: Vec<u8> = Vec::new();
        let cs_slot = top.len();
        top.extend_from_slice(&encode_dict_offset_placeholder());
        top.push(17);
        let fd_array_slot = top.len();
        top.extend_from_slice(&encode_dict_offset_placeholder());
        top.push(12);
        top.push(0x24);
        let fd_select_slot = top.len();
        top.extend_from_slice(&encode_dict_offset_placeholder());
        top.push(12);
        top.push(0x25);
        let vstore_slot = if vstore.is_some() {
            let s = top.len();
            top.extend_from_slice(&encode_dict_offset_placeholder());
            top.push(24);
            Some(s)
        } else {
            None
        };

        let mut out = Vec::new();
        out.push(2u8);
        out.push(0u8);
        out.push(5u8);
        let top_len = top.len() as u16;
        out.extend_from_slice(&top_len.to_be_bytes());
        let top_abs = out.len();
        out.extend_from_slice(&top);
        out.extend_from_slice(&global_subr_index);
        let fd_select_abs = out.len();
        out.extend_from_slice(&fd_select_bytes);
        let cs_abs = out.len();
        out.extend_from_slice(&cs_index);
        let fd_array_abs = out.len();
        out.extend_from_slice(&fd_array_index);

        let fd_index_off_size: usize = {
            let total: usize = font_dict_bodies.iter().map(Vec::len).sum();
            let last_off = 1 + total;
            if last_off <= 0xFF {
                1
            } else {
                2
            }
        };
        let fd_index_data_start = 4 + 1 + (n_fds + 1) * fd_index_off_size;
        let mut fd_body_offsets_in_index: Vec<usize> = Vec::with_capacity(n_fds);
        let mut acc = fd_index_data_start;
        for body in &font_dict_bodies {
            fd_body_offsets_in_index.push(acc);
            acc += body.len();
        }

        let mut per_fd_priv_abs: Vec<usize> = Vec::with_capacity(n_fds);
        let mut per_fd_subr_abs: Vec<usize> = Vec::with_capacity(n_fds);
        for pb in &private_bodies {
            let priv_abs = out.len();
            per_fd_priv_abs.push(priv_abs);
            out.extend_from_slice(pb);
            // Local subr INDEX immediately follows each Private DICT.
            // The Private DICT's Subrs op holds an OFFSET RELATIVE TO
            // THE START OF THE PRIVATE DICT (CFF spec).
            let subr_abs = out.len();
            per_fd_subr_abs.push(subr_abs);
            out.extend_from_slice(&local_subr_index);
        }

        let vstore_abs = if let Some(v) = vstore {
            let a = out.len();
            let len = v.len() as u16;
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(v);
            Some(a)
        } else {
            None
        };

        // Patch top dict.
        patch_dict_offset(&mut out, top_abs + cs_slot, cs_abs as i32);
        patch_dict_offset(&mut out, top_abs + fd_array_slot, fd_array_abs as i32);
        patch_dict_offset(&mut out, top_abs + fd_select_slot, fd_select_abs as i32);
        if let (Some(slot), Some(vabs)) = (vstore_slot, vstore_abs) {
            patch_dict_offset(&mut out, top_abs + slot, vabs as i32);
        }

        // Patch each Font DICT's Private slots and Private DICT's Subrs slot.
        for i in 0..n_fds {
            let body_abs_in_out = fd_array_abs + fd_body_offsets_in_index[i];
            let (size_slot, off_slot) = font_dict_priv_slots[i];
            patch_dict_offset(
                &mut out,
                body_abs_in_out + size_slot,
                private_bodies[i].len() as i32,
            );
            patch_dict_offset(
                &mut out,
                body_abs_in_out + off_slot,
                per_fd_priv_abs[i] as i32,
            );
            // Subrs offset: relative to the Private DICT's start.
            let subr_rel = per_fd_subr_abs[i] - per_fd_priv_abs[i];
            patch_dict_offset(
                &mut out,
                per_fd_priv_abs[i] + private_subrs_slot_in_body,
                subr_rel as i32,
            );
        }

        out
    }

    /// Regression for #197: a charstring whose blend operand was
    /// pushed via the 3-byte `OP_SHORTINT` form (b0=28, then 2-byte
    /// big-endian i16) used to fail with "blend count decode failed"
    /// because `decode_operand_f32` only matched the 1- and 2-byte push
    /// ranges and the 5-byte real-number form. Real fonts with ≥ 1240
    /// subrs route call indices through SHORTINT, and any blend whose
    /// delta count happens to land at e.g. 5000 also uses SHORTINT.
    #[test]
    fn decode_operand_f32_handles_shortint() {
        // 28, 0x0B, 0xB8 = shortint 3000.
        let v = decode_operand_f32(&[28, 0x0B, 0xB8], 0).expect("shortint decodes");
        assert!((v.0 - 3000.0).abs() < 1e-6);
        assert_eq!(v.1, 3);
        // 28, 0xFF, 0x9C = shortint -100.
        let v2 = decode_operand_f32(&[28, 0xFF, 0x9C], 0).expect("shortint negative decodes");
        assert!((v2.0 - (-100.0)).abs() < 1e-6);
        assert_eq!(v2.1, 3);
    }

    #[test]
    fn bake_cff2_partial_scales_blend_delta_by_pin_scalar() {
        // Region peaks at (1, 1). Pin wght=0.5 → scalar 0.5. Source
        // delta 100 → output delta 50. The rebuilt charstring's blend
        // pushes 50 (the scaled delta) instead of 100.
        let ivs = build_ivs2_for_cff2(
            &[[(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)]],
            &[(alloc::vec![0], alloc::vec![alloc::vec![100]])],
        );
        let cs0: &[u8] = &[
            139, 139, 21, // 0 0 rmoveto
            139, 239, 140, 16, // 0 100 1 blend
            22, // hmoveto
        ];
        let cff = build_synthetic_cff2(&[cs0], &[0], Some(&ivs));
        let pins = [AxisPin::Pin, AxisPin::Keep];
        let coords = [0.5, 0.0];
        let new_cff = bake_cff2_partial(&cff, &coords, &pins).expect("partial bake");
        let parsed = parse_cff2(&new_cff).expect("parse");
        let cs = parsed.char_strings[0];
        // The new charstring contains the master (0 = byte 139) and
        // the scaled delta. 50 encodes as 139+50 = 189.
        assert!(
            cs.contains(&189u8),
            "scaled delta 50 (encoded 189) must appear; got {:?}",
            cs
        );
        // Blend op survives.
        assert!(cs.contains(&16u8), "blend op survives partial bake");
    }
}
