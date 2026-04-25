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
}
