//! CFF1 subsetting helpers and byte-level emitter primitives.
//!
//! # Analysis layer
//!
//! - [`subr_bias`]: canonical 107 / 1131 / 32768 bias table from the
//!   Type 2 spec, indexed by subroutine count.
//! - [`scan_subr_calls`]: walks a Type 2 charstring's bytes and yields
//!   the (kind, biased index) pair for every `callsubr` / `callgsubr`
//!   it executes. Handles single-byte (32..=246), two-byte positive
//!   (247..=250) / negative (251..=254), `shortint` (op 28), `fixed`
//!   (op 255) push forms, and `hintmask` / `cntrmask` tail bytes.
//! - [`compute_kept_subrs`]: fixed-point closure that returns the
//!   transitive local + global subroutine keep-set for a kept-gid
//!   charstring slice.
//!
//! # Emitter primitives
//!
//! - [`encode_index`]: serializes a CFF INDEX (header + offsets +
//!   payload), picking the smallest valid `offSize`.
//! - [`encode_int_operand`]: Type 2 operand push, smallest valid form.
//! - [`encode_dict_int`]: DICT-context integer encoding (used by Top
//!   DICT and Private DICT serializers).
//! - [`emit_charset_format0`] / [`emit_charset_format2`] /
//!   [`emit_charset_auto`]: charset rebuild with auto format selection.
//! - [`emit_encoding_format0`] / [`emit_encoding_format1`] /
//!   [`emit_encoding_auto`]: Encoding rebuild with auto format
//!   selection (CFF1 only; CFF2 omits Encoding).
//! - [`renumber_subr_call`]: patches a single `callsubr` / `callgsubr`
//!   operand push in a charstring slice using the bias-adjusted target.
//! - [`renumber_charstring`]: runs [`scan_subr_calls`] over a
//!   charstring and rewrites every call site against caller-supplied
//!   `(old_index -> new_index)` maps for both subr kinds.
//!
//! # Subset entry
//!
//! [`subset_non_identity`] is the orchestration that wires the
//! analysis layer + emitter primitives end-to-end. It dispatches by
//! source shape: non-CID CFF1 sources walk the single-Private path
//! (Top DICT walk, kept-charstring + transitive subroutine keep-set,
//! renumber-in-place, layout in deterministic order: Header / Name
//! INDEX / Top DICT INDEX / String INDEX / Global Subr INDEX /
//! Encoding / charset / CharStrings INDEX / Private DICT / Local Subr
//! INDEX, then deferred-offset placeholder patches). CID-keyed sources
//! (FDArray + FDSelect present in the source Top DICT) route through
//! `subset_cid_keyed`, which adds an FDArray INDEX rebuild + FDSelect
//! rewrite + per-FD subroutine keep-set on top of the same primitives.
//!
//! The crate's [`crate::subset`] entry routes non-identity CFF1 via
//! this path and the layout-rebuild driver next door in `crate::lib`.
//! CFF2 non-identity uses the mirror flow in [`crate::cff2`],
//! structurally a strict simplification of the CID-keyed CFF1 layout
//! (no Name / String / Encoding / charset INDEXes; single inline Top
//! DICT; VariationStore rides through verbatim).
//!
//! The emitter primitives are exercised by unit tests covering the
//! bias-renumber boundaries (107 / 1131 / 32768), Top DICT
//! serialization idempotence, charset / encoding format auto-pick,
//! and full-orchestration round-trips (kept-only-requested-glyphs,
//! drops-unused-subrs, preserves-name-and-string-indexes,
//! renumbers-callsubr-when-local-subrs-kept, rejects-CID-keyed,
//! rejects-kept-set-without-gid0).

use alloc::vec::Vec;

use crate::SubsetError;

mod charset;
mod charstring;
mod cid;
mod emit;
mod reader;
mod seac;

pub use charstring::{
    compute_kept_subrs, encode_int_operand, renumber_charstring, renumber_subr_call,
    scan_subr_calls, subr_bias, SubrCall, SubrKind,
};
pub(crate) use cid::{kept_fd_positions, serialise_font_dict};
pub use emit::{
    emit_charset_auto, emit_charset_format0, emit_charset_format2, emit_encoding_auto,
    emit_encoding_format0, emit_encoding_format1, emit_fd_select_auto, emit_fd_select_format0,
    emit_fd_select_format3, encode_dict_int, encode_dict_offset_placeholder, encode_index,
    parse_fd_select, patch_dict_offset,
};
pub use reader::encode_index_cff2;
pub(crate) use reader::{
    private_operands, read_index_cff2, read_private_dict, walk_dict, DictEntry, OP_CHARSTRINGS,
    OP_FD_ARRAY, OP_FD_SELECT, OP_PRIVATE, OP_SUBRS, OP_VSTORE,
};
pub(crate) use seac::SeacClosure;

use charset::{extract_kept_charset_sids, extract_kept_encoding_codes};
use cid::subset_cid_keyed;
use reader::{parse_cff1, OP_CHARSET, OP_ENCODING};

// ----------------------------------------------------------------------------
// Top DICT / Private DICT serializers.
//
// Both rebuild a DICT from a captured [`DictEntry`] list, preserving every
// non-targeted operator verbatim. Targeted operators (charset / Encoding /
// CharStrings / Private / Subrs) get a 5-byte placeholder operand the
// patcher overwrites with the real value once layout is known.
// ----------------------------------------------------------------------------

/// Top DICT placeholder slots discovered while serializing. The
/// orchestration patches each slot with the real offset once the
/// section layout is fixed.
#[derive(Debug, Default, Clone)]
struct TopDictSlots {
    /// Byte offset of the b0=29 operand byte for op 15 (charset).
    charset_slot: Option<usize>,
    /// Byte offset of the b0=29 operand byte for op 16 (Encoding).
    encoding_slot: Option<usize>,
    /// Byte offset of the b0=29 operand byte for op 17 (CharStrings).
    char_strings_slot: Option<usize>,
    /// Byte offsets of the size + offset b0=29 operand bytes for op 18.
    private_slot: Option<(usize, usize)>,
}

/// Serializes a Top DICT body, rewriting the targeted operator
/// operands to 5-byte placeholders. Returns the body bytes plus the
/// slot offsets the patcher needs.
fn serialise_top_dict(
    entries: &[DictEntry],
    rebuild_charset: bool,
    rebuild_encoding: bool,
) -> (Vec<u8>, TopDictSlots) {
    let mut out = Vec::new();
    let mut slots = TopDictSlots::default();
    for e in entries {
        // Targeted operators drop their original operands and get
        // placeholders instead. Every other operator keeps its
        // operands verbatim.
        match e.op {
            OP_CHARSET if rebuild_charset => {
                slots.charset_slot = Some(out.len());
                out.extend_from_slice(&encode_dict_offset_placeholder());
            }
            OP_ENCODING if rebuild_encoding => {
                slots.encoding_slot = Some(out.len());
                out.extend_from_slice(&encode_dict_offset_placeholder());
            }
            OP_CHARSTRINGS => {
                slots.char_strings_slot = Some(out.len());
                out.extend_from_slice(&encode_dict_offset_placeholder());
            }
            OP_PRIVATE => {
                let size_slot = out.len();
                out.extend_from_slice(&encode_dict_offset_placeholder());
                let off_slot = out.len();
                out.extend_from_slice(&encode_dict_offset_placeholder());
                slots.private_slot = Some((size_slot, off_slot));
            }
            _ => {
                for o in &e.operands {
                    out.extend_from_slice(&o.raw);
                }
            }
        }
        // Emit operator bytes.
        if e.op >= 0x0C00 {
            out.push(12);
            out.push(e.op as u8);
        } else {
            out.push(e.op as u8);
        }
    }
    (out, slots)
}

/// Private DICT placeholder slots. Only the Subrs (op 19) operand is
/// patched at this layer.
#[derive(Debug, Default, Clone)]
pub(crate) struct PrivateDictSlots {
    /// Byte offset of the b0=29 operand byte for op 19 (Subrs).
    pub(crate) subrs_slot: Option<usize>,
}

/// Serializes a Private DICT body. Op 19 (Subrs), present iff the
/// Private DICT had a Subrs reference, gets a 5-byte placeholder.
/// When the source had no op 19 but the orchestration is emitting
/// local subrs, an op 19 entry is appended.
pub(crate) fn serialise_private_dict(
    entries: &[DictEntry],
    emit_subrs_op: bool,
) -> (Vec<u8>, PrivateDictSlots) {
    let mut out = Vec::new();
    let mut slots = PrivateDictSlots::default();
    let mut had_subrs = false;
    for e in entries {
        if e.op == OP_SUBRS {
            had_subrs = true;
            if emit_subrs_op {
                slots.subrs_slot = Some(out.len());
                out.extend_from_slice(&encode_dict_offset_placeholder());
                out.push(OP_SUBRS as u8);
            }
            // When the orchestration drops local subrs, omit op 19
            // entirely (no operand emitted, no operator byte).
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
    if emit_subrs_op && !had_subrs {
        // Source Private DICT had no Subrs operand but we're emitting
        // local subrs. Append a fresh op 19 entry.
        slots.subrs_slot = Some(out.len());
        out.extend_from_slice(&encode_dict_offset_placeholder());
        out.push(OP_SUBRS as u8);
    }
    (out, slots)
}

// ----------------------------------------------------------------------------
// CFF1 non-identity subset entry.
// ----------------------------------------------------------------------------

/// Rebuilds a non-identity CFF1 table for the kept-gid subset.
///
/// Wires together the Top DICT walker + capture-offsets, the
/// CharStrings INDEX rebuild with in-place subr renumber, the local +
/// global Subr INDEX rebuilds, and the section-layout placeholder-patch
/// driver. Charset and Encoding are rebuilt via [`emit_charset_auto`]
/// and [`emit_encoding_auto`] when the source uses an explicit
/// (non-predefined) table; predefined-charset sources get an explicit
/// rebuild to preserve the kept-gid SID mapping.
///
/// CID-keyed sources (FDArray / FDSelect present) route through
/// `subset_cid_keyed`, which also rebuilds the FDArray INDEX and
/// rewrites FDSelect. Sources with predefined Expert / ExpertSubset
/// charsets are declined.
///
/// `kept_gids` must be sorted ascending and contain gid 0.
///
/// # Errors
///
/// Returns [`SubsetError::Unsupported`] when the source is malformed or
/// uses a feature the orchestration does not rewrite.
pub fn subset_non_identity(cff_bytes: &[u8], kept_gids: &[u16]) -> Result<Vec<u8>, SubsetError> {
    let parsed = parse_cff1(cff_bytes)?;
    if parsed.is_cid {
        return subset_cid_keyed(cff_bytes, &parsed, kept_gids);
    }

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

    // Kept charstrings (in new compacted gid order).
    let kept_charstrings: Vec<&[u8]> = kept_gids
        .iter()
        .map(|&g| parsed.char_strings[g as usize])
        .collect();

    // Subroutine keep-set (transitive closure).
    let (kept_local_idx, kept_global_idx) =
        compute_kept_subrs(&kept_charstrings, &parsed.local_subrs, &parsed.global_subrs)?;

    // Build old -> new renumber tables.
    let mut local_renumber: Vec<Option<u32>> = alloc::vec![None; parsed.local_subrs.len()];
    for (new_i, &old_i) in kept_local_idx.iter().enumerate() {
        local_renumber[old_i as usize] = Some(new_i as u32);
    }
    let mut global_renumber: Vec<Option<u32>> = alloc::vec![None; parsed.global_subrs.len()];
    for (new_i, &old_i) in kept_global_idx.iter().enumerate() {
        global_renumber[old_i as usize] = Some(new_i as u32);
    }

    let new_local_count = kept_local_idx.len();
    let new_global_count = kept_global_idx.len();
    let old_local_count = parsed.local_subrs.len();
    let old_global_count = parsed.global_subrs.len();

    // Rewrite each kept charstring (cloned, then mutated).
    let mut new_charstrings: Vec<Vec<u8>> = kept_charstrings.iter().map(|s| s.to_vec()).collect();
    for cs in &mut new_charstrings {
        renumber_charstring(
            cs,
            old_local_count,
            old_global_count,
            new_local_count,
            new_global_count,
            &local_renumber,
            &global_renumber,
        )?;
    }

    // Rewrite each kept local subr.
    let mut new_local_subrs: Vec<Vec<u8>> = kept_local_idx
        .iter()
        .map(|&i| parsed.local_subrs[i as usize].to_vec())
        .collect();
    for sub in &mut new_local_subrs {
        renumber_charstring(
            sub,
            old_local_count,
            old_global_count,
            new_local_count,
            new_global_count,
            &local_renumber,
            &global_renumber,
        )?;
    }

    // Rewrite each kept global subr.
    let mut new_global_subrs: Vec<Vec<u8>> = kept_global_idx
        .iter()
        .map(|&i| parsed.global_subrs[i as usize].to_vec())
        .collect();
    for sub in &mut new_global_subrs {
        renumber_charstring(
            sub,
            old_local_count,
            old_global_count,
            new_local_count,
            new_global_count,
            &local_renumber,
            &global_renumber,
        )?;
    }

    // Charset rebuild: required whenever there's at least one kept
    // glyph past gid 0.
    let charset_per_gid =
        extract_kept_charset_sids(cff_bytes, parsed.charset_off, n_glyphs, kept_gids)?;
    let charset_bytes = emit_charset_auto(&charset_per_gid);

    // Source charset offset 0/1/2 means predefined. After rewrite we
    // emit an explicit table (no longer predefined), so the Top DICT
    // op 15 needs an explicit offset.
    let rebuild_charset = true;

    // Encoding: emitted only when the source actually had an
    // Encoding (op 16 present) or used the default (Standard, off = 0).
    // To keep the rewrite faithful, emit a fresh Encoding whenever the
    // kept set is non-trivial. For predefined Encoding we emit a
    // best-effort all-zero codes table so the round-trip succeeds; cmap
    // handles the real character mapping for shaping.
    let need_charset_for_encoding = &charset_per_gid;
    let encoding_codes = extract_kept_encoding_codes(
        cff_bytes,
        parsed.encoding_off,
        need_charset_for_encoding,
        kept_gids,
    )?;
    let encoding_bytes = emit_encoding_auto(&encoding_codes);
    let rebuild_encoding = true;

    // Top DICT rebuild.
    let top_dict_entries = walk_dict(parsed.top_dict)?;
    let (top_dict_body, top_slots) =
        serialise_top_dict(&top_dict_entries, rebuild_charset, rebuild_encoding);

    // Top DICT INDEX wrapping the rebuilt Top DICT body.
    let top_dict_index = encode_index(&[&top_dict_body]);

    // Global Subr INDEX (renumbered globals).
    let global_subr_refs: Vec<&[u8]> = new_global_subrs.iter().map(Vec::as_slice).collect();
    let global_subr_index = encode_index(&global_subr_refs);

    // CharStrings INDEX (renumbered charstrings).
    let charstrings_refs: Vec<&[u8]> = new_charstrings.iter().map(Vec::as_slice).collect();
    let charstrings_index = encode_index(&charstrings_refs);

    // Local Subr INDEX (renumbered locals).
    let local_subr_refs: Vec<&[u8]> = new_local_subrs.iter().map(Vec::as_slice).collect();
    let local_subr_index = encode_index(&local_subr_refs);

    // Private DICT rebuild: only when source had one.
    let (private_body, priv_slots) = if parsed.private.is_some() {
        let entries = walk_dict(parsed.private_dict)?;
        let emit_subrs = !new_local_subrs.is_empty();
        serialise_private_dict(&entries, emit_subrs)
    } else {
        (Vec::new(), PrivateDictSlots::default())
    };

    // ---- Layout ---------------------------------------------------------
    // Header -> Name INDEX -> Top DICT INDEX -> String INDEX -> Global Subr
    // INDEX -> Encoding -> charset -> CharStrings INDEX -> Private DICT ->
    // Local Subr INDEX. Top DICT INDEX precedes Encoding/charset so we
    // can patch its placeholders once the downstream sections land.
    let mut out = Vec::with_capacity(cff_bytes.len());
    out.extend_from_slice(parsed.header);
    out.extend_from_slice(parsed.name_index);

    // Top DICT INDEX position. We need the offset of the Top DICT
    // *body* within the output, since `top_slots` are relative to the
    // body. Find that by counting INDEX header + offsets.
    let top_dict_index_start = out.len();
    let top_dict_body_offset_in_index = {
        // INDEX layout: count(2) + offSize(1) + offsets(2 * offSize) + data.
        // We have count=1 -> 2 + 1 + 2*off_size = body_offset_in_index.
        if top_dict_body.is_empty() {
            return Err(SubsetError::Unsupported("CFF1 rebuilt Top DICT empty"));
        }
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
    out.extend_from_slice(&top_dict_index);
    let top_dict_body_abs = top_dict_index_start + top_dict_body_offset_in_index;

    out.extend_from_slice(parsed.string_index);
    out.extend_from_slice(&global_subr_index);

    let encoding_abs = out.len();
    out.extend_from_slice(&encoding_bytes);

    let charset_abs = out.len();
    out.extend_from_slice(&charset_bytes);

    let charstrings_abs = out.len();
    out.extend_from_slice(&charstrings_index);

    let private_abs = out.len();
    let private_size = private_body.len();
    out.extend_from_slice(&private_body);

    let local_subr_abs = out.len();
    out.extend_from_slice(&local_subr_index);

    // ---- Patch Top DICT placeholders ------------------------------------
    if let Some(slot) = top_slots.charset_slot {
        let abs = top_dict_body_abs + slot;
        patch_dict_offset(&mut out, abs, charset_abs as i32);
    }
    if let Some(slot) = top_slots.encoding_slot {
        let abs = top_dict_body_abs + slot;
        patch_dict_offset(&mut out, abs, encoding_abs as i32);
    }
    if let Some(slot) = top_slots.char_strings_slot {
        let abs = top_dict_body_abs + slot;
        patch_dict_offset(&mut out, abs, charstrings_abs as i32);
    }
    if let Some((size_slot, off_slot)) = top_slots.private_slot {
        let abs_size = top_dict_body_abs + size_slot;
        let abs_off = top_dict_body_abs + off_slot;
        patch_dict_offset(&mut out, abs_size, private_size as i32);
        patch_dict_offset(&mut out, abs_off, private_abs as i32);
    }

    // ---- Patch Private DICT op 19 (Subrs) -------------------------------
    if let Some(slot) = priv_slots.subrs_slot {
        let abs = private_abs + slot;
        let rel = (local_subr_abs - private_abs) as i32;
        patch_dict_offset(&mut out, abs, rel);
    }

    Ok(out)
}

#[cfg(test)]
mod tests;
