//! CID-keyed CFF1 subsetting: FDArray and FDSelect rebuild with
//! per-FD subroutine keep-sets.

use alloc::vec::Vec;

use super::charset::extract_kept_charset_sids;
use super::charstring::{cross_fd_globals, rewrite_calls, SubrCall};
use super::emit::{
    emit_charset_auto, emit_fd_select_auto, encode_dict_int, encode_dict_offset_placeholder,
    encode_index, parse_fd_select, patch_dict_offset,
};
use super::reader::{
    private_operands, read_index, read_private_dict, walk_dict, DictEntry, ParsedCff1, OP_CHARSET,
    OP_CHARSTRINGS, OP_ENCODING, OP_FD_ARRAY, OP_FD_SELECT, OP_PRIVATE,
};
use super::serialise_private_dict;
use super::walk::{walk_budget, CharstringWalk, FdWalk};
use crate::SubsetError;

// ----------------------------------------------------------------------------
// CID-keyed CFF1 orchestration.
//
// CID-keyed fonts replace the single Top-DICT-level Private DICT with an
// FDArray (an INDEX of Font DICTs, each carrying its own Private DICT)
// and an FDSelect (a per-gid map naming which Font DICT, and thus which
// Local Subr INDEX, to use for that glyph).
//
// Subsetting an FDArray/FDSelect-shaped CFF1 entails:
//
//   1. Walk FDSelect, capture old FD index per kept gid.
//   2. Build the kept-FD set (union of FDs referenced by the kept gid set).
//   3. For each kept FD, run every kept charstring it owns through its
//      subroutine calls, computing the per-FD subroutine keep-set and
//      the call sites of every body reached (see `walk`).
//   4. Renumber FDs to a 0..N compact range; rewrite FDSelect with the
//      new FD indices.
//   5. Renumber per-FD local subrs at the call sites the walk found,
//      with the FD-specific renumber tables.
//   6. Rebuild the FDArray INDEX with placeholder-patched Font DICTs
//      whose Private DICT (size, off) and Subrs offsets get patched once
//      the layout lands.
//   7. Rewrite the Top DICT: keep CID-specific metadata verbatim
//      (ROS / CIDFontVersion / CIDFontRevision / CIDFontType / UIDBase),
//      rewrite CIDCount to the new kept-gid-count, and emit placeholder
//      offsets for charset (15), CharStrings (17), FDArray (12 36), and
//      FDSelect (12 37).
// ----------------------------------------------------------------------------

// Top DICT operators specific to CID-keyed fonts. CIDCount (0x0C22)
// is the only one we rewrite. All others (CIDFontVersion 0x0C1F,
// CIDFontRevision 0x0C20, CIDFontType 0x0C21, UIDBase 0x0C23, ROS
// 0x0C1E) ride through verbatim via the catch-all branch in
// `serialise_cid_top_dict`.
pub(super) const OP_CID_COUNT: u16 = 0x0C22;

/// Top DICT placeholder slots for CID-keyed fonts.
///
/// CID Top DICT carries charset (15) + CharStrings (17) + FDArray (12 36)
/// + FDSelect (12 37).
///
/// The Private (18) operator is *not* in the CID Top DICT. It lives
/// inside each Font DICT in the FDArray.
#[derive(Debug, Default, Clone)]
struct CidTopDictSlots {
    /// Byte offset of the b0=29 operand byte for op 15 (charset).
    charset_slot: Option<usize>,
    /// Byte offset of the b0=29 operand byte for op 17 (CharStrings).
    char_strings_slot: Option<usize>,
    /// Byte offset of the b0=29 operand byte for op 12 36 (FDArray).
    fd_array_slot: Option<usize>,
    /// Byte offset of the b0=29 operand byte for op 12 37 (FDSelect).
    fd_select_slot: Option<usize>,
}

/// Serializes a CID-keyed Top DICT body. Charset (15), CharStrings (17),
/// FDArray (12 36), and FDSelect (12 37) get 5-byte placeholders. The
/// op `0x0C22` (CIDCount) operand is rewritten to `new_cid_count`. All
/// other operators (ROS, CIDFontVersion, etc.) are preserved verbatim.
fn serialise_cid_top_dict(entries: &[DictEntry], new_cid_count: u32) -> (Vec<u8>, CidTopDictSlots) {
    let mut out = Vec::new();
    let mut slots = CidTopDictSlots::default();
    for e in entries {
        match e.op {
            OP_CHARSET => {
                slots.charset_slot = Some(out.len());
                out.extend_from_slice(&encode_dict_offset_placeholder());
                out.push(15);
            }
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
            OP_CID_COUNT => {
                // Rewrite CIDCount operand to the new value.
                let enc = encode_dict_int(new_cid_count as i32);
                out.extend_from_slice(&enc);
                out.push(12);
                out.push(0x22);
            }
            // Encoding op 16: CID fonts shouldn't have it, but if
            // present preserve verbatim.
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
    // If the source Top DICT didn't carry CIDCount explicitly (rare,
    // most CID fonts emit it), append it now so the subset has accurate
    // glyph-count metadata.
    let had_cid_count = entries.iter().any(|e| e.op == OP_CID_COUNT);
    if !had_cid_count {
        let enc = encode_dict_int(new_cid_count as i32);
        out.extend_from_slice(&enc);
        out.push(12);
        out.push(0x22);
    }
    (out, slots)
}

/// Emits one Font DICT body (used inside the FDArray INDEX). Carries
/// op 18 (Private size + offset) plus whatever other operators the
/// source Font DICT had (FontName etc.). Returns the serialized body
/// plus the byte offsets of the Private DICT size + offset placeholders.
pub(crate) fn serialise_font_dict(entries: &[DictEntry]) -> (Vec<u8>, Option<(usize, usize)>) {
    let mut out = Vec::new();
    let mut private_slot: Option<(usize, usize)> = None;
    let mut had_private = false;
    for e in entries {
        if e.op == OP_PRIVATE {
            had_private = true;
            let size_slot = out.len();
            out.extend_from_slice(&encode_dict_offset_placeholder());
            let off_slot = out.len();
            out.extend_from_slice(&encode_dict_offset_placeholder());
            out.push(18);
            private_slot = Some((size_slot, off_slot));
        } else {
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
    if !had_private {
        // Source Font DICT had no Private. Emit a fresh op 18 entry.
        let size_slot = out.len();
        out.extend_from_slice(&encode_dict_offset_placeholder());
        let off_slot = out.len();
        out.extend_from_slice(&encode_dict_offset_placeholder());
        out.push(18);
        private_slot = Some((size_slot, off_slot));
    }
    (out, private_slot)
}

/// Maps each source FD index to its position in `kept_fds_sorted`, which
/// is also its new FD index. FD indexes are `u8`, so the table covers
/// every possible index. Entries for dropped FDs stay 0 and are never
/// read.
pub(crate) fn kept_fd_positions(kept_fds_sorted: &[u8]) -> [usize; 256] {
    let mut positions = [0usize; 256];
    for (pos, &old_fd) in kept_fds_sorted.iter().enumerate() {
        positions[usize::from(old_fd)] = pos;
    }
    positions
}

/// Looks up the FD-specific duplicate slot for source global `old_g` in
/// a sorted `(old_g, new_slot)` table.
fn override_slot(table: &[(u32, u32)], old_g: usize) -> Option<u32> {
    let old_g = u32::try_from(old_g).ok()?;
    let k = table.binary_search_by_key(&old_g, |&(g, _)| g).ok()?;
    table.get(k).map(|&(_, slot)| slot)
}

/// CID-keyed CFF1 subset orchestration. See module-level header comment
/// above for the high-level walk.
pub(super) fn subset_cid_keyed(
    cff_bytes: &[u8],
    parsed: &ParsedCff1<'_>,
    kept_gids: &[u16],
) -> Result<Vec<u8>, SubsetError> {
    let n_glyphs = parsed.char_strings.len();
    if n_glyphs == 0 {
        return Err(SubsetError::Unsupported("CFF1 source has zero glyphs"));
    }
    for &g in kept_gids {
        if (g as usize) >= n_glyphs {
            return Err(SubsetError::Unsupported(
                "CFF1 kept gid past source numGlyphs",
            ));
        }
    }
    if kept_gids.first() != Some(&0u16) {
        return Err(SubsetError::Unsupported("CFF1 kept gid set must include 0"));
    }

    let fd_array_off = parsed.fd_array_off.ok_or(SubsetError::Unsupported(
        "CFF1 CID-keyed source missing FDArray offset",
    ))? as usize;
    let fd_select_off = parsed.fd_select_off.ok_or(SubsetError::Unsupported(
        "CFF1 CID-keyed source missing FDSelect offset",
    ))? as usize;

    // Parse FDSelect -> per-gid FD index for *every* source gid.
    let source_fd_select = parse_fd_select(cff_bytes, fd_select_off, n_glyphs)?;

    // Parse FDArray -> vector of Font DICT bytes.
    let (fd_array_entries, _) = read_index(cff_bytes, fd_array_off)?;
    if fd_array_entries.is_empty() {
        return Err(SubsetError::Unsupported("CFF1 CID FDArray has zero FDs"));
    }
    if fd_array_entries.len() > 256 {
        return Err(SubsetError::Unsupported("CFF1 CID FDArray > 256 FDs"));
    }

    // For each source FD, capture (Font DICT bytes, parsed entries,
    // optional Private DICT info, Private DICT bytes, Local Subr INDEX).
    struct FdInfo<'a> {
        font_dict_entries: Vec<DictEntry>,
        private_dict: &'a [u8],
        private: Option<(u32, u32)>,
        local_subrs: Vec<&'a [u8]>,
    }
    let mut fd_infos: Vec<FdInfo<'_>> = Vec::with_capacity(fd_array_entries.len());
    for fd_bytes in &fd_array_entries {
        let entries = walk_dict(fd_bytes)?;
        // The last well-formed Private operator wins.
        let priv_info = entries
            .iter()
            .rev()
            .filter(|e| e.op == OP_PRIVATE)
            .find_map(private_operands);
        let (private_dict, local_subrs): (&[u8], Vec<&[u8]>) = match priv_info {
            Some((size, off)) => read_private_dict(
                cff_bytes,
                size,
                off,
                read_index,
                "CFF1 CID Private DICT past end",
            )?,
            None => (&[][..], Vec::new()),
        };
        fd_infos.push(FdInfo {
            font_dict_entries: entries,
            private_dict,
            private: priv_info,
            local_subrs,
        });
    }

    // Step 1+2: walk FDSelect, capture per-kept-gid FD, build kept-FD set.
    // kept_fd_old[i] = source-FD index for kept gid i (in kept_gids order).
    let mut kept_fd_old: Vec<u8> = Vec::with_capacity(kept_gids.len());
    for &g in kept_gids {
        let fd = source_fd_select
            .get(g as usize)
            .copied()
            .ok_or(SubsetError::Unsupported("CFF1 CID FDSelect gid past end"))?;
        if (fd as usize) >= fd_infos.len() {
            return Err(SubsetError::Unsupported(
                "CFF1 CID FDSelect FD index past FDArray length",
            ));
        }
        kept_fd_old.push(fd);
    }
    let mut kept_fds_sorted: Vec<u8> = kept_fd_old.clone();
    kept_fds_sorted.sort_unstable();
    kept_fds_sorted.dedup();

    // FD renumber map: old_fd -> position in `kept_fds_sorted`, which is
    // also the new FD index. Only kept FDs are ever looked up.
    let fd_pos_of = kept_fd_positions(&kept_fds_sorted);
    // Per-kept-gid: new FD index.
    let new_fd_select: Vec<u8> = kept_fd_old
        .iter()
        .map(|&old| fd_pos_of[usize::from(old)] as u8)
        .collect();

    // Step 3: run every kept glyph through its subroutine calls, FD by
    // FD (see `walk`). The subroutines reached are the ones kept: each
    // FD's locals, and the globals, shared across FDs. Each body's call
    // sites are found with its masks sized by the stems its callers
    // declared.
    let mut walk = CharstringWalk::new(&parsed.global_subrs, None, walk_budget(cff_bytes.len()));
    let mut fd_walks: Vec<FdWalk<'_>> = Vec::with_capacity(kept_fds_sorted.len());
    let mut charstring_calls: Vec<Vec<SubrCall>> = alloc::vec![Vec::new(); kept_gids.len()];
    for &old_fd in &kept_fds_sorted {
        let mut fd_walk = walk.fd(&fd_infos[old_fd as usize].local_subrs, 0);
        for (i, &gid) in kept_gids.iter().enumerate() {
            if kept_fd_old[i] == old_fd {
                charstring_calls[i] =
                    walk.glyph(&mut fd_walk, parsed.char_strings[gid as usize])?;
            }
        }
        fd_walks.push(fd_walk);
    }
    let per_fd_kept_local: Vec<Vec<u32>> = fd_walks.iter().map(FdWalk::kept_locals).collect();
    let kept_global_idx = walk.kept_globals();

    let old_global_count = parsed.global_subrs.len();

    // Cross-FD detection: which kept globals reach a `callsubr` either
    // directly or transitively through another global? Such a global
    // resolves locals against the calling FD's local INDEX, so in the
    // rebuilt CFF, where every FD shares the same global INDEX, we
    // must duplicate the global per kept FD that uses it and rewrite
    // each caller's `callgsubr` operand to point at *that* FD's copy.
    let cross_fd_mask = cross_fd_globals(
        old_global_count,
        kept_global_idx.iter().map(|&g| {
            (
                g as usize,
                walk.global_calls(g as usize).unwrap_or_default(),
            )
        }),
    );

    // Per-kept-FD local renumber tables.
    let mut per_fd_local_renumber: Vec<Vec<Option<u32>>> =
        Vec::with_capacity(kept_fds_sorted.len());
    for (i, &old_fd) in kept_fds_sorted.iter().enumerate() {
        let local_count = fd_infos[old_fd as usize].local_subrs.len();
        let mut renumber: Vec<Option<u32>> = alloc::vec![None; local_count];
        for (new_i, &old_i) in per_fd_kept_local[i].iter().enumerate() {
            renumber[old_i as usize] = Some(new_i as u32);
        }
        per_fd_local_renumber.push(renumber);
    }

    // Determine which (cross-FD global, kept-FD) duplicates are needed:
    // one for every cross-FD global a glyph of the FD reached, through
    // its charstring, its locals or other globals. Each FD's target
    // list is sorted by source global index.
    //
    // The rebuilt Global Subr INDEX holds the kept non-cross-FD globals
    // plus one duplicate per (cross-FD global, FD) pair. Its count is a
    // u16, so a layout past 65535 entries cannot be encoded. The check
    // runs while the targets are collected, which also bounds the
    // memory a hostile FDArray can make this step use.
    let n_non_cross = kept_global_idx
        .iter()
        .filter(|&&g| !cross_fd_mask.get(g as usize).copied().unwrap_or(false))
        .count();
    let mut layout_len = n_non_cross;
    let mut per_fd_cross_fd_targets: Vec<Vec<u32>> = Vec::with_capacity(kept_fds_sorted.len());
    for fd_walk in &fd_walks {
        let mut targets: Vec<u32> = Vec::new();
        for g in fd_walk.reached_globals() {
            if cross_fd_mask.get(g as usize).copied().unwrap_or(false) {
                layout_len += 1;
                if layout_len > usize::from(u16::MAX) {
                    return Err(SubsetError::Unsupported(
                        "CFF1 CID rebuilt Global Subr INDEX exceeds 65535 entries",
                    ));
                }
                targets.push(g);
            }
        }
        per_fd_cross_fd_targets.push(targets);
    }

    // Build the new global INDEX layout:
    //   slot [0 .. n_non_cross]            : kept *non-cross-FD* globals
    //   slot [n_non_cross .. n_non_cross+k] : per-FD duplicates (one per
    //                                         (cross-FD global, FD) pair
    //                                         that any caller exercises)
    //
    // `global_renumber[i]` is the new slot for the canonical (non-cross-FD)
    // copy of source global `i`: `Some(slot)` only when global `i` is
    // *kept and not cross-FD*. Cross-FD globals route through
    // `per_fd_cross_fd_override` instead.
    let mut global_renumber: Vec<Option<u32>> = alloc::vec![None; old_global_count];
    let mut new_global_layout: Vec<(u32, Option<u8>)> = Vec::with_capacity(layout_len);
    for &old_i in &kept_global_idx {
        if !cross_fd_mask.get(old_i as usize).copied().unwrap_or(false) {
            let new_slot = new_global_layout.len() as u32;
            if let Some(slot) = global_renumber.get_mut(old_i as usize) {
                *slot = Some(new_slot);
            }
            new_global_layout.push((old_i, None));
        }
    }
    // Per-FD override tables: `(old_g, new_slot)` pairs sorted by
    // `old_g`, one entry per cross-FD global the FD calls.
    let mut per_fd_cross_fd_override: Vec<Vec<(u32, u32)>> =
        Vec::with_capacity(kept_fds_sorted.len());
    for (&old_fd, targets) in kept_fds_sorted.iter().zip(&per_fd_cross_fd_targets) {
        let mut table: Vec<(u32, u32)> = Vec::with_capacity(targets.len());
        for &old_g in targets {
            let new_slot = new_global_layout.len() as u32;
            table.push((old_g, new_slot));
            new_global_layout.push((old_g, Some(old_fd)));
        }
        per_fd_cross_fd_override.push(table);
    }
    let new_global_count = new_global_layout.len();

    // Step 5: rewrite each kept charstring with its FD's local-renumber
    // table + the shared global-renumber table + the FD's cross-FD
    // override table, at the call sites the walk found.
    let mut new_charstrings: Vec<Vec<u8>> = Vec::with_capacity(kept_gids.len());
    for (i, &gid) in kept_gids.iter().enumerate() {
        let old_fd = kept_fd_old[i];
        let new_fd_pos = fd_pos_of[usize::from(old_fd)];
        let fd_local_renumber = &per_fd_local_renumber[new_fd_pos];
        let new_local_count = per_fd_kept_local[new_fd_pos].len();
        let overrides = &per_fd_cross_fd_override[new_fd_pos];
        let cs = rewrite_calls(
            parsed.char_strings[gid as usize],
            &charstring_calls[i],
            new_local_count,
            new_global_count,
            fd_local_renumber,
            &global_renumber,
            |old_g| override_slot(overrides, old_g),
        )?;
        new_charstrings.push(cs);
    }

    // Renumber each kept local subr (per-FD). Locals are FD-scoped so
    // they use their FD's cross-FD override too.
    let mut new_per_fd_local_subrs: Vec<Vec<Vec<u8>>> = Vec::with_capacity(kept_fds_sorted.len());
    for (i, &old_fd) in kept_fds_sorted.iter().enumerate() {
        let fd_local_subrs_old = &fd_infos[old_fd as usize].local_subrs;
        let kept_local_idx = &per_fd_kept_local[i];
        let fd_local_renumber = &per_fd_local_renumber[i];
        let new_local_count = kept_local_idx.len();
        let overrides = &per_fd_cross_fd_override[i];
        let mut new_locals: Vec<Vec<u8>> = Vec::with_capacity(kept_local_idx.len());
        for &idx in kept_local_idx {
            let sub = rewrite_calls(
                fd_local_subrs_old[idx as usize],
                fd_walks[i].local_calls(idx as usize).unwrap_or_default(),
                new_local_count,
                new_global_count,
                fd_local_renumber,
                &global_renumber,
                |old_g| override_slot(overrides, old_g),
            )?;
            new_locals.push(sub);
        }
        new_per_fd_local_subrs.push(new_locals);
    }

    // Build the new global INDEX bodies. For each slot:
    //   * (old_g, None)          : non-cross-FD global, single copy.
    //   * (old_g, Some(old_fd))  : cross-FD duplicate for FD `old_fd`.
    //
    // Non-cross-FD globals only call other globals; we rewrite their
    // `callgsubr` operands using `global_renumber` (cross-FD overrides
    // are inapplicable because by definition this body never calls a
    // local). A non-cross-FD global that calls a cross-FD global would
    // itself become cross-FD by `cross_fd_globals`'s transitive pass,
    // so this branch only sees globals whose entire reach stays inside
    // the non-cross-FD tier.
    //
    // Cross-FD duplicates route local-subr operands through that FD's
    // local-renumber table and global-subr operands through that FD's
    // cross-FD override (so recursive cross-FD calls land on the
    // correct duplicate).
    let mut new_global_subrs: Vec<Vec<u8>> = Vec::with_capacity(new_global_layout.len());
    for &(old_g, dup_for_fd) in &new_global_layout {
        let source = parsed.global_subrs[old_g as usize];
        let calls = walk.global_calls(old_g as usize).unwrap_or_default();
        let body = if let Some(old_fd) = dup_for_fd {
            let fd_pos = fd_pos_of[usize::from(old_fd)];
            let new_local_count = per_fd_kept_local[fd_pos].len();
            let overrides = &per_fd_cross_fd_override[fd_pos];
            rewrite_calls(
                source,
                calls,
                new_local_count,
                new_global_count,
                &per_fd_local_renumber[fd_pos],
                &global_renumber,
                |old_g| override_slot(overrides, old_g),
            )?
        } else {
            // Non-cross-FD: zero-sized local pool because the body
            // never issues a `callsubr`. If it did, the empty local
            // renumber table would surface a hard error.
            rewrite_calls(
                source,
                calls,
                0,
                new_global_count,
                &[],
                &global_renumber,
                |_| None,
            )?
        };
        new_global_subrs.push(body);
    }

    // Step 4: emit FDSelect bytes.
    let fd_select_bytes = emit_fd_select_auto(&new_fd_select);

    // Charset rebuild: CID fonts use SIDs that are CIDs (not String
    // INDEX SIDs), so the per-gid SID is the gid's CID. We project
    // kept_gids to their CIDs by reading the source charset (which maps
    // gid -> CID for CID fonts).
    let charset_per_gid_cids =
        extract_kept_charset_sids(cff_bytes, parsed.charset_off, n_glyphs, kept_gids)?;
    let charset_bytes = emit_charset_auto(&charset_per_gid_cids);

    // Step 6: serialize per-FD Font DICTs with placeholders.
    struct FdEmit {
        font_dict_body: Vec<u8>,
        font_dict_private_slot: Option<(usize, usize)>,
        new_private_body: Vec<u8>,
        new_priv_subrs_slot: Option<usize>,
    }
    let mut fd_emits: Vec<FdEmit> = Vec::with_capacity(kept_fds_sorted.len());
    for (i, &old_fd) in kept_fds_sorted.iter().enumerate() {
        let info = &fd_infos[old_fd as usize];
        let (font_dict_body, font_dict_private_slot) = serialise_font_dict(&info.font_dict_entries);
        // Rebuild Private DICT body: keep entries except op 19, emit
        // op 19 placeholder when we have local subrs.
        let priv_entries = if info.private.is_some() {
            walk_dict(info.private_dict)?
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

    // Build FDArray INDEX (the kept Font DICT bodies).
    let fd_array_refs: Vec<&[u8]> = fd_emits
        .iter()
        .map(|f| f.font_dict_body.as_slice())
        .collect();
    let fd_array_index = encode_index(&fd_array_refs);

    // Compute Font DICT body offsets within the FDArray INDEX.
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
    let fd_count = fd_emits.len();
    let fd_index_data_start = 2 + 1 + (fd_count + 1) * fd_index_off_size;
    let mut fd_body_offsets_in_index: Vec<usize> = Vec::with_capacity(fd_count);
    let mut acc = fd_index_data_start;
    for f in &fd_emits {
        fd_body_offsets_in_index.push(acc);
        acc += f.font_dict_body.len();
    }

    // Top DICT: clone source entries, drop Encoding (CID fonts don't
    // have one), serialize with placeholders.
    let mut top_entries = walk_dict(parsed.top_dict)?;
    top_entries.retain(|e| e.op != OP_ENCODING);
    let new_cid_count = kept_gids.len() as u32;
    let (top_dict_body, top_slots) = serialise_cid_top_dict(&top_entries, new_cid_count);

    // Top DICT INDEX wrapping.
    let top_dict_index = encode_index(&[&top_dict_body[..]]);
    let top_dict_body_offset_in_index = {
        let total = 1 + top_dict_body.len();
        let off_size: usize = if total <= 0xFF {
            1
        } else if total <= 0xFFFF {
            2
        } else if total <= 0x00FF_FFFF {
            3
        } else {
            4
        };
        2 + 1 + 2 * off_size
    };

    // Global Subr INDEX (renumbered globals).
    let global_subr_refs: Vec<&[u8]> = new_global_subrs.iter().map(Vec::as_slice).collect();
    let global_subr_index = encode_index(&global_subr_refs);

    // CharStrings INDEX.
    let cs_refs: Vec<&[u8]> = new_charstrings.iter().map(Vec::as_slice).collect();
    let charstrings_index = encode_index(&cs_refs);

    // Per-FD Local Subr INDEX bytes.
    let per_fd_local_index: Vec<Vec<u8>> = new_per_fd_local_subrs
        .iter()
        .map(|locals| {
            let refs: Vec<&[u8]> = locals.iter().map(Vec::as_slice).collect();
            encode_index(&refs)
        })
        .collect();

    // ---- Layout ---------------------------------------------------------
    // Header -> Name INDEX -> Top DICT INDEX -> String INDEX -> Global Subr
    // INDEX -> charset -> FDSelect -> CharStrings INDEX -> FDArray INDEX ->
    // [for each FD: Private DICT bytes -> Local Subr INDEX bytes (when
    // present)].
    let mut out = Vec::with_capacity(cff_bytes.len());
    out.extend_from_slice(parsed.header);
    out.extend_from_slice(parsed.name_index);

    let top_dict_index_start = out.len();
    out.extend_from_slice(&top_dict_index);
    let top_dict_body_abs = top_dict_index_start + top_dict_body_offset_in_index;

    out.extend_from_slice(parsed.string_index);
    out.extend_from_slice(&global_subr_index);

    let charset_abs = out.len();
    out.extend_from_slice(&charset_bytes);

    let fd_select_abs = out.len();
    out.extend_from_slice(&fd_select_bytes);

    let charstrings_abs = out.len();
    out.extend_from_slice(&charstrings_index);

    let fd_array_abs = out.len();
    out.extend_from_slice(&fd_array_index);

    // Per-FD: write Private DICT body and the corresponding Local Subr
    // INDEX (when locals were kept). Track absolute offsets so we can
    // patch the Font DICT's Private slot.
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

    // ---- Patch Top DICT placeholders ------------------------------------
    if let Some(slot) = top_slots.charset_slot {
        let abs = top_dict_body_abs + slot;
        patch_dict_offset(&mut out, abs, charset_abs as i32);
    }
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

    // ---- Patch Font DICT Private slots and Private DICT Subrs slots -----
    for (i, f) in fd_emits.iter().enumerate() {
        if let Some((size_slot_in_fd_body, off_slot_in_fd_body)) = f.font_dict_private_slot {
            let body_abs_in_out = fd_array_abs + fd_body_offsets_in_index[i];
            let abs_size = body_abs_in_out + size_slot_in_fd_body;
            let abs_off = body_abs_in_out + off_slot_in_fd_body;
            patch_dict_offset(&mut out, abs_size, per_fd_private_size[i] as i32);
            patch_dict_offset(&mut out, abs_off, per_fd_private_abs[i] as i32);
        }
        // Patch Subrs slot in Private DICT, if present.
        if let (Some(slot), Some(local_subr_abs)) =
            (f.new_priv_subrs_slot, per_fd_local_subr_abs[i])
        {
            let private_abs = per_fd_private_abs[i];
            let abs = private_abs + slot;
            let rel = (local_subr_abs - private_abs) as i32;
            patch_dict_offset(&mut out, abs, rel);
        }
    }

    Ok(out)
}
