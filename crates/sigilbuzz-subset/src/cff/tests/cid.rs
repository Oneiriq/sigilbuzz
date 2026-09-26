//! CID-keyed CFF1 subset orchestration round trips.

use super::*;
use crate::cff::cid::OP_CID_COUNT;
use crate::cff::reader::read_index;

// -- CID-keyed orchestration ----------------------------------------------

/// Builds a synthetic CID-keyed CFF1 with `n_fds` Font DICTs, where
/// each gid is assigned a Font DICT via the supplied `fd_select` map.
/// `charstrings` carries the per-gid charstring (gid 0 included).
/// Each FD gets an empty Private DICT (no local subrs) for
/// simplicity. This synthesizes the minimum CID-shaped table needed
/// to drive subset_cid_keyed end-to-end.
fn build_synthetic_cid_cff1(charstrings: &[&[u8]], fd_select: &[u8]) -> Vec<u8> {
    assert_eq!(charstrings.len(), fd_select.len());
    let n_fds = (*fd_select.iter().max().unwrap_or(&0) as usize) + 1;
    let cs_index = encode_index(charstrings);

    let global_subr_index = encode_index(&[]);
    let header = alloc::vec![1u8, 0, 4, 1];
    let name_index = encode_index(&[b"CIDSynth"]);
    // String INDEX with one entry: Registry/Ordering both use SID 391
    // (Adobe-Identity-0 default in Adobe TN 5176). For a synthetic
    // build we leave the String INDEX empty and use SID 0 (== `.notdef`)
    // for ROS Registry/Ordering: fontTools tolerates this in CID
    // headers where the parser only checks the operator presence.
    let string_index = encode_index(&[]);

    // Charset: format 0 with CIDs counting from 1 per gid past gid 0.
    let charset_sids: Vec<u16> = (1..(charstrings.len() as u16)).collect();
    let charset_bytes = emit_charset_format0(&charset_sids);

    // FDSelect: format 0 (per-gid u8).
    let fd_select_bytes = emit_fd_select_format0(fd_select);

    // Per-FD Private DICT body (each just one op: `defaultWidthX`,
    // op 20). Source Private DICTs for CID fonts are typically richer,
    // but the orchestration only cares that the body parses + survives.
    let private_bodies: Vec<Vec<u8>> = (0..n_fds)
        .map(|_| alloc::vec![139u8 /* 0 */, 20u8 /* defaultWidthX */])
        .collect();

    // Font DICTs: each carries op 18 (Private size + offset) only.
    // We need the absolute Private DICT offsets, which depend on
    // layout. Build everything with placeholder offsets, then patch.
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
    let fd_array_index = encode_index(&fd_array_refs);

    // Top DICT: ROS (op 12 30), CIDCount (12 34), charset (15),
    // CharStrings (17), FDArray (12 36), FDSelect (12 37). Use
    // placeholders for offset operators.
    let mut top: Vec<u8> = Vec::new();
    // ROS: Registry, Ordering, Supplement (three operands). We
    // encode SID 0, SID 0, integer 0.
    top.extend_from_slice(&encode_dict_int(0));
    top.extend_from_slice(&encode_dict_int(0));
    top.extend_from_slice(&encode_dict_int(0));
    top.push(12);
    top.push(0x1E);
    // CIDCount.
    top.extend_from_slice(&encode_dict_int(charstrings.len() as i32));
    top.push(12);
    top.push(0x22);
    // charset.
    let charset_slot = top.len();
    top.extend_from_slice(&encode_dict_offset_placeholder());
    top.push(15);
    // CharStrings.
    let cs_slot = top.len();
    top.extend_from_slice(&encode_dict_offset_placeholder());
    top.push(17);
    // FDArray.
    let fd_array_slot = top.len();
    top.extend_from_slice(&encode_dict_offset_placeholder());
    top.push(12);
    top.push(0x24);
    // FDSelect.
    let fd_select_slot = top.len();
    top.extend_from_slice(&encode_dict_offset_placeholder());
    top.push(12);
    top.push(0x25);

    let top_dict_index = encode_index(&[&top[..]]);
    let top_dict_body_offset_in_index = {
        let total = 1 + top.len();
        let off_size: usize = if total <= 0xFF { 1 } else { 2 };
        2 + 1 + 2 * off_size
    };

    // Layout: header | name | top idx | string idx | gsubr idx |
    //   charset | FDSelect | CharStrings idx | FDArray idx |
    //   per-FD Private DICT bodies.
    let mut out = Vec::new();
    out.extend_from_slice(&header);
    out.extend_from_slice(&name_index);

    let top_dict_index_start = out.len();
    out.extend_from_slice(&top_dict_index);
    let top_dict_body_abs = top_dict_index_start + top_dict_body_offset_in_index;

    out.extend_from_slice(&string_index);
    out.extend_from_slice(&global_subr_index);

    let charset_abs = out.len();
    out.extend_from_slice(&charset_bytes);

    let fd_select_abs = out.len();
    out.extend_from_slice(&fd_select_bytes);

    let cs_abs = out.len();
    out.extend_from_slice(&cs_index);

    let fd_array_abs = out.len();
    out.extend_from_slice(&fd_array_index);

    // Per-FD Private DICT.
    let fd_index_off_size: usize = {
        let total: usize = font_dict_bodies.iter().map(Vec::len).sum();
        let last_off = 1 + total;
        if last_off <= 0xFF {
            1
        } else {
            2
        }
    };
    let fd_index_data_start = 2 + 1 + (n_fds + 1) * fd_index_off_size;
    let mut fd_body_offsets_in_index: Vec<usize> = Vec::with_capacity(n_fds);
    let mut acc = fd_index_data_start;
    for body in &font_dict_bodies {
        fd_body_offsets_in_index.push(acc);
        acc += body.len();
    }

    let mut per_fd_priv_abs: Vec<usize> = Vec::with_capacity(n_fds);
    let mut per_fd_priv_size: Vec<usize> = Vec::with_capacity(n_fds);
    for pb in &private_bodies {
        per_fd_priv_abs.push(out.len());
        per_fd_priv_size.push(pb.len());
        out.extend_from_slice(pb);
    }

    // Patch top dict slots.
    patch_dict_offset(
        &mut out,
        top_dict_body_abs + charset_slot,
        charset_abs as i32,
    );
    patch_dict_offset(&mut out, top_dict_body_abs + cs_slot, cs_abs as i32);
    patch_dict_offset(
        &mut out,
        top_dict_body_abs + fd_array_slot,
        fd_array_abs as i32,
    );
    patch_dict_offset(
        &mut out,
        top_dict_body_abs + fd_select_slot,
        fd_select_abs as i32,
    );

    // Patch each Font DICT's Private slots.
    for i in 0..n_fds {
        let body_abs_in_out = fd_array_abs + fd_body_offsets_in_index[i];
        let (size_slot, off_slot) = font_dict_priv_slots[i];
        patch_dict_offset(
            &mut out,
            body_abs_in_out + size_slot,
            per_fd_priv_size[i] as i32,
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
fn cid_orchestration_keeps_all_glyphs_when_kept_set_is_full() {
    // 3 glyphs, 1 FD. Subsetting to all gids should round-trip.
    let cs0: &[u8] = &[14u8]; // .notdef = endchar
    let cs1: &[u8] = &[139, 139, 21, 14];
    let cs2: &[u8] = &[139, 14];
    let cff = build_synthetic_cid_cff1(&[cs0, cs1, cs2], &[0, 0, 0]);
    let new_cff = subset_non_identity(&cff, &[0, 1, 2]).unwrap();
    let parsed = parse_cff1(&new_cff).unwrap();
    assert!(parsed.is_cid);
    assert_eq!(parsed.char_strings.len(), 3);
    assert_eq!(parsed.char_strings[0], cs0);
    assert_eq!(parsed.char_strings[1], cs1);
    assert_eq!(parsed.char_strings[2], cs2);
}

#[test]
fn cid_orchestration_drops_unused_glyphs() {
    // 4 glyphs, 1 FD. Subset to [0, 2]. Output charstrings = 2.
    let cs0: &[u8] = &[14u8];
    let cs1: &[u8] = &[139, 139, 21, 14];
    let cs2: &[u8] = &[139, 14];
    let cs3: &[u8] = &[139, 139, 22, 14];
    let cff = build_synthetic_cid_cff1(&[cs0, cs1, cs2, cs3], &[0, 0, 0, 0]);
    let new_cff = subset_non_identity(&cff, &[0, 2]).unwrap();
    let parsed = parse_cff1(&new_cff).unwrap();
    assert!(parsed.is_cid);
    assert_eq!(parsed.char_strings.len(), 2);
    assert_eq!(parsed.char_strings[1], cs2);
}

#[test]
fn cid_orchestration_drops_unused_fd() {
    // 3 glyphs across 2 FDs. Subset to gids whose FDs are all 0;
    // the dropped FD must vanish from the new FDArray.
    let cs0: &[u8] = &[14u8];
    let cs1: &[u8] = &[139, 14];
    let cs2: &[u8] = &[139, 14];
    // FDSelect: gid 0 -> FD 0, gid 1 -> FD 0, gid 2 -> FD 1.
    let cff = build_synthetic_cid_cff1(&[cs0, cs1, cs2], &[0, 0, 1]);
    let new_cff = subset_non_identity(&cff, &[0u16, 1]).unwrap();
    // Re-parse and inspect the FDArray INDEX.
    let parsed = parse_cff1(&new_cff).unwrap();
    assert!(parsed.is_cid);
    let fd_array_off = parsed.fd_array_off.unwrap() as usize;
    let (fda, _) = read_index(&new_cff, fd_array_off).unwrap();
    assert_eq!(fda.len(), 1, "kept FD set should reduce to {{0}}");
}

#[test]
fn cid_orchestration_renumbers_fd_select() {
    // 3 glyphs across 3 FDs (gid i uses FD i). Subset to [0, 2]
    // drops FD 1; FDs 0 and 2 collapse to new FDs 0 and 1.
    let cs0: &[u8] = &[14u8];
    let cs1: &[u8] = &[139, 14];
    let cs2: &[u8] = &[139, 14];
    let cff = build_synthetic_cid_cff1(&[cs0, cs1, cs2], &[0, 1, 2]);
    let new_cff = subset_non_identity(&cff, &[0u16, 2]).unwrap();
    let parsed = parse_cff1(&new_cff).unwrap();
    let fd_select = parse_fd_select(
        &new_cff,
        parsed.fd_select_off.unwrap() as usize,
        parsed.char_strings.len(),
    )
    .unwrap();
    // gid 0's old FD was 0 -> new FD 0.
    // gid 2's old FD was 2 -> new FD 1 (FD 1 was dropped).
    assert_eq!(fd_select, alloc::vec![0u8, 1]);
}

#[test]
fn cid_orchestration_preserves_ros_metadata() {
    // Round-trip a 2-glyph CID font and verify the source's ROS /
    // CIDCount operators ride through.
    let cs0: &[u8] = &[14u8];
    let cs1: &[u8] = &[139, 14];
    let cff = build_synthetic_cid_cff1(&[cs0, cs1], &[0, 0]);
    let new_cff = subset_non_identity(&cff, &[0u16, 1]).unwrap();
    let parsed = parse_cff1(&new_cff).unwrap();
    // ROS still triggers `is_cid`.
    assert!(parsed.is_cid);
    // Top DICT walk finds CIDCount = new_cid_count = 2.
    let entries = walk_dict(parsed.top_dict).unwrap();
    let cid_count_entry = entries.iter().find(|e| e.op == OP_CID_COUNT).unwrap();
    let cid_count = cid_count_entry.operands.last().unwrap().int_value.unwrap();
    assert_eq!(cid_count, 2);
}

/// Builds a CID-keyed CFF1 with `n_fds` Font DICTs, each carrying
/// its own local-subr INDEX, plus a shared global-subr INDEX.
/// `charstrings.len() == fd_select.len()`. `per_fd_locals[fd]` is
/// the local-subr INDEX for FD `fd`. `globals` is the shared
/// global-subr INDEX (one entry per global).
fn build_synthetic_cid_cff1_with_subrs(
    charstrings: &[&[u8]],
    fd_select: &[u8],
    globals: &[&[u8]],
    per_fd_locals: &[Vec<&[u8]>],
) -> Vec<u8> {
    assert_eq!(charstrings.len(), fd_select.len());
    let n_fds = per_fd_locals.len();
    assert!(fd_select.iter().all(|&f| (f as usize) < n_fds));

    let cs_index = encode_index(charstrings);
    let global_subr_index = encode_index(globals);
    let header = alloc::vec![1u8, 0, 4, 1];
    let name_index = encode_index(&[b"CIDSubrSynth"]);
    let string_index = encode_index(&[]);

    let charset_sids: Vec<u16> = (1..(charstrings.len() as u16)).collect();
    let charset_bytes = emit_charset_format0(&charset_sids);
    let fd_select_bytes = emit_fd_select_format0(fd_select);

    // Per-FD local subr INDEX bytes.
    let local_indexes: Vec<Vec<u8>> = per_fd_locals.iter().map(|l| encode_index(l)).collect();

    // Per-FD Private DICT body: minimal `defaultWidthX` op (20)
    // plus an op-19 (Subrs) placeholder when locals exist.
    let mut private_bodies: Vec<Vec<u8>> = Vec::with_capacity(n_fds);
    let mut priv_subrs_slots: Vec<Option<usize>> = Vec::with_capacity(n_fds);
    for locals in per_fd_locals {
        let mut body: Vec<u8> = Vec::new();
        body.push(139); // 0
        body.push(20); // defaultWidthX
        let slot = if locals.is_empty() {
            None
        } else {
            let s = body.len();
            body.extend_from_slice(&encode_dict_offset_placeholder());
            body.push(19); // Subrs
            Some(s)
        };
        priv_subrs_slots.push(slot);
        private_bodies.push(body);
    }

    // Font DICTs: each carries op 18 (Private size + offset) only.
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
    let fd_array_index = encode_index(&fd_array_refs);

    let mut top: Vec<u8> = Vec::new();
    top.extend_from_slice(&encode_dict_int(0));
    top.extend_from_slice(&encode_dict_int(0));
    top.extend_from_slice(&encode_dict_int(0));
    top.push(12);
    top.push(0x1E);
    top.extend_from_slice(&encode_dict_int(charstrings.len() as i32));
    top.push(12);
    top.push(0x22);
    let charset_slot = top.len();
    top.extend_from_slice(&encode_dict_offset_placeholder());
    top.push(15);
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

    let top_dict_index = encode_index(&[&top[..]]);
    let top_dict_body_offset_in_index = {
        let total = 1 + top.len();
        let off_size: usize = if total <= 0xFF { 1 } else { 2 };
        2 + 1 + 2 * off_size
    };

    let mut out = Vec::new();
    out.extend_from_slice(&header);
    out.extend_from_slice(&name_index);

    let top_dict_index_start = out.len();
    out.extend_from_slice(&top_dict_index);
    let top_dict_body_abs = top_dict_index_start + top_dict_body_offset_in_index;

    out.extend_from_slice(&string_index);
    out.extend_from_slice(&global_subr_index);

    let charset_abs = out.len();
    out.extend_from_slice(&charset_bytes);

    let fd_select_abs = out.len();
    out.extend_from_slice(&fd_select_bytes);

    let cs_abs = out.len();
    out.extend_from_slice(&cs_index);

    let fd_array_abs = out.len();
    out.extend_from_slice(&fd_array_index);

    // Compute Font DICT body offsets within FDArray INDEX so we can
    // patch each Font DICT's Private slot below.
    let fd_index_off_size: usize = {
        let total: usize = font_dict_bodies.iter().map(Vec::len).sum();
        let last_off = 1 + total;
        if last_off <= 0xFF {
            1
        } else {
            2
        }
    };
    let fd_index_data_start = 2 + 1 + (n_fds + 1) * fd_index_off_size;
    let mut fd_body_offsets_in_index: Vec<usize> = Vec::with_capacity(n_fds);
    let mut acc = fd_index_data_start;
    for body in &font_dict_bodies {
        fd_body_offsets_in_index.push(acc);
        acc += body.len();
    }

    // Per-FD: Private DICT body, then Local Subr INDEX (when any).
    let mut per_fd_priv_abs: Vec<usize> = Vec::with_capacity(n_fds);
    let mut per_fd_priv_size: Vec<usize> = Vec::with_capacity(n_fds);
    let mut per_fd_local_abs: Vec<Option<usize>> = Vec::with_capacity(n_fds);
    for (i, pb) in private_bodies.iter().enumerate() {
        per_fd_priv_abs.push(out.len());
        per_fd_priv_size.push(pb.len());
        out.extend_from_slice(pb);
        if per_fd_locals[i].is_empty() {
            per_fd_local_abs.push(None);
        } else {
            let abs = out.len();
            out.extend_from_slice(&local_indexes[i]);
            per_fd_local_abs.push(Some(abs));
        }
    }

    patch_dict_offset(
        &mut out,
        top_dict_body_abs + charset_slot,
        charset_abs as i32,
    );
    patch_dict_offset(&mut out, top_dict_body_abs + cs_slot, cs_abs as i32);
    patch_dict_offset(
        &mut out,
        top_dict_body_abs + fd_array_slot,
        fd_array_abs as i32,
    );
    patch_dict_offset(
        &mut out,
        top_dict_body_abs + fd_select_slot,
        fd_select_abs as i32,
    );

    for i in 0..n_fds {
        let body_abs_in_out = fd_array_abs + fd_body_offsets_in_index[i];
        let (size_slot, off_slot) = font_dict_priv_slots[i];
        patch_dict_offset(
            &mut out,
            body_abs_in_out + size_slot,
            per_fd_priv_size[i] as i32,
        );
        patch_dict_offset(
            &mut out,
            body_abs_in_out + off_slot,
            per_fd_priv_abs[i] as i32,
        );
        if let (Some(slot), Some(local_abs)) = (priv_subrs_slots[i], per_fd_local_abs[i]) {
            let priv_abs = per_fd_priv_abs[i];
            patch_dict_offset(&mut out, priv_abs + slot, (local_abs - priv_abs) as i32);
        }
    }

    out
}

#[test]
fn cid_cross_fd_global_calls_local_round_trips() {
    // Synthetic CID-keyed CFF1 with 2 FDs, 2 charstrings (one per
    // FD), 1 global subr that calls local subr 0, and one local
    // subr per FD with a *different* body. After subset to
    // [0, 1], the cross-FD global must be duplicated per FD: each
    // duplicate's `callsubr` resolves to that FD's local subr 0.
    //
    // FD 0 local 0:  rmoveto 0 0  + return (no-op move to origin)
    // FD 1 local 0:  rmoveto 0 0  + return (same body: bytes
    //                              identical so the round-trip is
    //                              easy to assert)
    // global 0:      callsubr 0 + return
    // gid 0 (FD 0):  callgsubr 0 + endchar
    // gid 1 (FD 1):  callgsubr 0 + endchar
    //
    // Global 0's callsubr operand: bias (local count = 1) is 107,
    // so operand `-107` -> local 0. Encode as shortint.
    let mut g0 = alloc::vec![OP_SHORTINT];
    g0.extend_from_slice(&(-107i16).to_be_bytes());
    g0.push(OP_CALLSUBR);
    g0.push(OP_RETURN);

    let local: Vec<u8> = alloc::vec![139, 139, OP_RMOVETO, OP_RETURN];

    // Charstrings call global 0: bias (global count = 1) is 107,
    // operand `-107` -> global 0.
    let mut cs0 = alloc::vec![OP_SHORTINT];
    cs0.extend_from_slice(&(-107i16).to_be_bytes());
    cs0.push(OP_CALLGSUBR);
    cs0.push(OP_ENDCHAR);
    let cs1 = cs0.clone();

    let charstrings: Vec<&[u8]> = alloc::vec![cs0.as_slice(), cs1.as_slice()];
    let globals: Vec<&[u8]> = alloc::vec![g0.as_slice()];
    let per_fd_locals: Vec<Vec<&[u8]>> =
        alloc::vec![alloc::vec![local.as_slice()], alloc::vec![local.as_slice()]];
    let cff =
        build_synthetic_cid_cff1_with_subrs(&charstrings, &[0u8, 1], &globals, &per_fd_locals);

    let new_cff = subset_non_identity(&cff, &[0u16, 1]).unwrap();
    let parsed = parse_cff1(&new_cff).unwrap();
    assert!(parsed.is_cid);
    assert_eq!(parsed.char_strings.len(), 2);
    // Both FDs survive.
    let fd_array_off = parsed.fd_array_off.unwrap() as usize;
    let (fda, _) = read_index(&new_cff, fd_array_off).unwrap();
    assert_eq!(fda.len(), 2);
    // Global INDEX: the lone source global is cross-FD and gets
    // duplicated per kept FD (2 FDs -> 2 duplicates, no canonical
    // copy because the source global is itself cross-FD).
    assert_eq!(parsed.global_subrs.len(), 2);
    // Each duplicate's body must contain a `callsubr` (op 10).
    for body in &parsed.global_subrs {
        assert!(
            body.contains(&OP_CALLSUBR),
            "cross-FD duplicate must retain callsubr",
        );
    }
    // Charstrings call distinct globals (each FD's duplicate). Two
    // gids -> two distinct global indices used.
    let global_count = parsed.global_subrs.len();
    let bias = subr_bias(global_count) as i64;
    let mut targets: Vec<i64> = Vec::new();
    for cs in &parsed.char_strings {
        for call in scan_subr_calls(cs, 0, global_count).unwrap() {
            if call.kind == SubrKind::Global {
                targets.push(i64::from(call.raw_operand) + bias);
            }
        }
    }
    targets.sort_unstable();
    targets.dedup();
    assert_eq!(
        targets.len(),
        2,
        "each FD's charstring routes to its own duplicate"
    );
}

#[test]
fn cid_global_calls_local_unused_fd_drops_duplicate() {
    // 3 gids over 2 FDs, but only gid 0 (FD 0) is kept. The cross-FD
    // global's duplicate for FD 1 must be dropped because no kept
    // caller exercises it. We assert that at most one global slot
    // survives.
    let mut g0 = alloc::vec![OP_SHORTINT];
    g0.extend_from_slice(&(-107i16).to_be_bytes());
    g0.push(OP_CALLSUBR);
    g0.push(OP_RETURN);
    let local: Vec<u8> = alloc::vec![139, 139, OP_RMOVETO, OP_RETURN];

    let mut cs0 = alloc::vec![OP_SHORTINT];
    cs0.extend_from_slice(&(-107i16).to_be_bytes());
    cs0.push(OP_CALLGSUBR);
    cs0.push(OP_ENDCHAR);
    // gid 1 doesn't call anything so the global is unreached from FD 1.
    let cs1: Vec<u8> = alloc::vec![14u8];
    let cs2: Vec<u8> = alloc::vec![14u8];

    let charstrings: Vec<&[u8]> = alloc::vec![cs0.as_slice(), cs1.as_slice(), cs2.as_slice()];
    let globals: Vec<&[u8]> = alloc::vec![g0.as_slice()];
    let per_fd_locals: Vec<Vec<&[u8]>> =
        alloc::vec![alloc::vec![local.as_slice()], alloc::vec![local.as_slice()]];
    let cff =
        build_synthetic_cid_cff1_with_subrs(&charstrings, &[0u8, 1, 1], &globals, &per_fd_locals);

    // Subset to gid 0 only (in addition to the mandatory .notdef
    // from FD 0). Build kept_gids = [0].
    let new_cff = subset_non_identity(&cff, &[0u16]).unwrap();
    let parsed = parse_cff1(&new_cff).unwrap();
    assert_eq!(parsed.char_strings.len(), 1);
    // Exactly one duplicate should remain (the one for FD 0).
    assert_eq!(parsed.global_subrs.len(), 1);
}
