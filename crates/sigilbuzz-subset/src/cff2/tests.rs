//! Unit tests for the CFF2 scanner integration and subset
//! orchestration.

use super::*;
use crate::cff::{
    emit_fd_select_format0, encode_index_cff2, encode_int_operand, patch_dict_offset,
    scan_subr_calls, subr_bias,
};

mod partial;

#[test]
fn cff2_charstring_without_endchar_is_walked_to_eof() {
    // CFF2 charstrings have no terminating endchar: the
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
fn build_synthetic_cff2(charstrings: &[&[u8]], fd_select: &[u8], vstore: Option<&[u8]>) -> Vec<u8> {
    build_synthetic_cff2_sharing(charstrings, fd_select, vstore, None)
}

/// [`build_synthetic_cff2`], with every Font DICT naming the one
/// Private DICT `shared` when it is given.
fn build_synthetic_cff2_sharing(
    charstrings: &[&[u8]],
    fd_select: &[u8],
    vstore: Option<&[u8]>,
    shared: Option<&[u8]>,
) -> Vec<u8> {
    assert_eq!(charstrings.len(), fd_select.len());
    let n_fds = (*fd_select.iter().max().unwrap_or(&0) as usize) + 1;
    let cs_index = encode_index_cff2(charstrings);
    let global_subr_index = encode_index_cff2(&[]);
    let fd_select_bytes = emit_fd_select_format0(fd_select);

    // Per-FD Private DICTs (one op for shape), or the one shared.
    let private_bodies: Vec<Vec<u8>> = match shared {
        Some(body) => alloc::vec![body.to_vec()],
        None => (0..n_fds)
            .map(|_| alloc::vec![139u8 /* 0 */, 20u8 /* defaultWidthX */])
            .collect(),
    };

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

    // Top DICT: CharStrings, FDArray, FDSelect, optional VariationStore.
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
        let p = if shared.is_some() { 0 } else { i };
        patch_dict_offset(
            &mut out,
            body_abs_in_out + size_slot,
            private_bodies[p].len() as i32,
        );
        patch_dict_offset(
            &mut out,
            body_abs_in_out + off_slot,
            per_fd_priv_abs[p] as i32,
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
    // VariationStore content is opaque to the orchestration:
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

/// An ItemVariationStore body (no CFF2 length prefix) with one subtable
/// per entry of `regions`, naming that many one-axis regions.
fn store_with_region_counts(regions: &[u16]) -> Vec<u8> {
    let region_count = regions.iter().copied().max().unwrap_or(0);
    let header_len = 8 + 4 * regions.len();
    let region_list_len = 4 + 6 * usize::from(region_count);
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(header_len as u32).to_be_bytes());
    out.extend_from_slice(&(regions.len() as u16).to_be_bytes());
    let mut sub_off = header_len + region_list_len;
    for &r in regions {
        out.extend_from_slice(&(sub_off as u32).to_be_bytes());
        sub_off += 6 + 2 * usize::from(r);
    }
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&region_count.to_be_bytes());
    for _ in 0..region_count {
        for v in [0i16, 0x4000, 0x4000] {
            out.extend_from_slice(&v.to_be_bytes());
        }
    }
    for &r in regions {
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&r.to_be_bytes());
        for i in 0..r {
            out.extend_from_slice(&i.to_be_bytes());
        }
    }
    out
}

/// Pushes of `values`.
fn pushes(values: &[i32]) -> Vec<u8> {
    values.iter().flat_map(|&v| encode_int_operand(v)).collect()
}

/// After a hint mask: a mask byte, then `5 hmoveto`. Read with a
/// second mask byte, the shortint opcode is taken for it and the
/// reserved opcode 0 follows.
const ONE_BYTE_MASK_THEN_MOVE: [u8; 5] = [9, 28, 0, 5, 22];

#[test]
fn cff2_subset_reads_blended_stem_hints() {
    // `10 20 1 2 3 4 2 blend hstemhm`: the store's two regions leave
    // one stem, so the mask takes one byte. The subset failed with
    // "CFF unknown charstring operator" when `blend` cleared the stack.
    let ivs = store_with_region_counts(&[2]);
    let mut cs1 = pushes(&[10, 20, 1, 2, 3, 4, 2]);
    cs1.extend_from_slice(&[16, 18, 19]); // blend hstemhm hintmask
    cs1.extend_from_slice(&ONE_BYTE_MASK_THEN_MOVE);
    let cs0: &[u8] = &[];
    let cff = build_synthetic_cff2(&[cs0, &cs1], &[0, 0], Some(&ivs));
    let new_cff = subset_non_identity(&cff, &[0, 1]).unwrap();
    let parsed = parse_cff2(&new_cff).unwrap();
    assert_eq!(parsed.char_strings[1], cs1.as_slice());
}

#[test]
fn cff2_subset_blends_with_the_private_dict_vsindex() {
    // Seven stems, then `10 1 2 3 1 blend 20 hstem`. The Private DICT's
    // `1 vsindex` picks the three-region subtable: one more stem, eight
    // in all, and a one-byte mask. Without it the one-region subtable
    // leaves nine stems and a two-byte mask, which misreads.
    let ivs = store_with_region_counts(&[1, 3]);
    let mut cs1 = pushes(&(0..14).collect::<Vec<_>>());
    cs1.push(18); // hstemhm
    cs1.extend_from_slice(&pushes(&[10, 1, 2, 3, 1]));
    cs1.push(16); // blend
    cs1.extend_from_slice(&pushes(&[20]));
    cs1.extend_from_slice(&[1, 19]); // hstem hintmask
    cs1.extend_from_slice(&ONE_BYTE_MASK_THEN_MOVE);
    let cs0: &[u8] = &[];
    let with_vsindex: &[u8] = &[140, 22]; // 1 vsindex
    let cff = build_synthetic_cff2_sharing(&[cs0, &cs1], &[0, 0], Some(&ivs), Some(with_vsindex));
    let new_cff = subset_non_identity(&cff, &[0, 1]).unwrap();
    let parsed = parse_cff2(&new_cff).unwrap();
    assert_eq!(parsed.char_strings[1], cs1.as_slice());
    assert_eq!(parsed.per_fd_vsindex, [1]);

    let without: &[u8] = &[139, 20]; // 0 defaultWidthX
    let cff = build_synthetic_cff2_sharing(&[cs0, &cs1], &[0, 0], Some(&ivs), Some(without));
    assert!(subset_non_identity(&cff, &[0, 1]).is_err());
}

#[test]
fn cff2_subset_sizes_a_mask_in_a_subroutine_by_blended_stems() {
    // Glyph 1 blends nine stems and calls local 0, whose mask takes
    // two bytes before its call to local 2; local 1 goes.
    let ivs = store_with_region_counts(&[2]);
    let defaults: Vec<i32> = (0..18).collect();
    let deltas: Vec<i32> = (0..36).map(|v| v % 7).collect();
    let mut cs1 = pushes(&defaults);
    cs1.extend_from_slice(&pushes(&deltas));
    cs1.extend_from_slice(&pushes(&[18]));
    cs1.extend_from_slice(&[16, 18]); // blend hstemhm
    cs1.extend_from_slice(&pushes(&[-107]));
    cs1.push(10); // callsubr local 0
    let mut local_0 = alloc::vec![19, 9, 9]; // hintmask, two bytes
    local_0.extend_from_slice(&pushes(&[2 - 107]));
    local_0.push(10); // callsubr local 2
    let local_1: &[u8] = &[11];
    let local_2: &[u8] = &[139, 139, 21];
    let cs0: &[u8] = &[];
    let cff = partial::build_synthetic_cff2_with_local_subrs(
        &[cs0, &cs1],
        &[0, 0],
        &[&local_0, local_1, local_2],
        Some(&ivs),
    );
    let new_cff = subset_non_identity(&cff, &[0, 1]).unwrap();
    let parsed = parse_cff2(&new_cff).unwrap();
    assert_eq!(parsed.char_strings[1], cs1.as_slice());
    let locals = &parsed.per_fd_local_subrs[0];
    assert_eq!(locals.len(), 2);
    let mut renumbered = local_0.clone();
    renumbered[3..4].copy_from_slice(&pushes(&[1 - 107]));
    assert_eq!(locals[0], renumbered.as_slice());
    assert_eq!(locals[1], local_2);
}
