//! Tests for the partial-instancing CFF2 bake.

use super::*;
use sigilbuzz::tables::variation_store::ItemVariationStore;

// --------------------------------------------------------------
// bake_cff2_partial: VarStore + blend rewrite.
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
    // (master), 100 (delta), 1 (count), blend -> resolved value
    // becomes the next pushed scalar. Then endchar/return-equivalent
    // (CFF2 doesn't endchar; we let the implicit eof end the CS).
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
    //   0 -> 139, 100 -> push 100 (107..=1131 range; 100 < 108 so
    //         it's one byte 239=139+100? wait: 100 is in [-107,
    //         107] range, so 100+139=239)
    //   1 -> 140, blend op = 16. rmoveto = 21. hmoveto = 22.
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
    // CFF2 without VariationStore: bake is a no-op (the source
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
    // dropped subtable. The subtable collapse -> no blend
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
    // Subtable collapsed -> zero subtables in the new IVS.
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
/// pattern: subrs commonly stash deltas / masters for the caller
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
    // and call it with raw=0 (1-byte push 139). 0 + 107 = 107 -> subr 107.
    let mut local_subrs_storage: Vec<&[u8]> = Vec::new();
    for _ in 0..107 {
        local_subrs_storage.push(&[11u8]); // empty subr -> return
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
    // Build Private DICTs with placeholder Subrs offsets, patched
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
/// ranges and the 5-byte real-number form. Real fonts with >= 1240
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
    // Region peaks at (1, 1). Pin wght=0.5 -> scalar 0.5. Source
    // delta 100 -> output delta 50. The rebuilt charstring's blend
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

/// Local subroutines 0..=9 where subroutine `k` calls subroutine
/// `k - 1` ten times and subroutine 0 is empty. Inlining a call to
/// subroutine 9 expands to a billion calls of subroutine 0.
fn subr_bomb() -> Vec<Vec<u8>> {
    let mut subrs: Vec<Vec<u8>> = alloc::vec![Vec::new()];
    for k in 1..10u8 {
        // The bias is 107 for fewer than 1240 subrs, so index
        // `k - 1` is pushed as the single byte (k - 1) - 107 + 139.
        subrs.push([k - 1 + 32, 10].repeat(10));
    }
    subrs
}

/// Charstring that calls local subroutine 9 of [`subr_bomb`].
const CALL_SUBR_9: &[u8] = &[9 + 32, 10];

#[test]
fn bake_at_coords_stops_exponential_subr_expansion() {
    let subrs = subr_bomb();
    let refs: Vec<&[u8]> = subrs.iter().map(Vec::as_slice).collect();
    let cff = build_synthetic_cff2_with_local_subrs(&[CALL_SUBR_9], &[0], &refs, None);
    let r = bake_at_coords(&cff, &[]);
    assert!(matches!(r, Err(SubsetError::Unsupported(_))), "{r:?}");
}

#[test]
fn bake_cff2_partial_stops_exponential_subr_expansion() {
    let ivs = build_ivs1_for_cff2(
        &[[(0.0, 1.0, 1.0)]],
        &[(alloc::vec![0], alloc::vec![alloc::vec![50i16]])],
    );
    let subrs = subr_bomb();
    let refs: Vec<&[u8]> = subrs.iter().map(Vec::as_slice).collect();
    let cff = build_synthetic_cff2_with_local_subrs(&[CALL_SUBR_9], &[0], &refs, Some(&ivs));
    let r = bake_cff2_partial(&cff, &[1.0], &[AxisPin::Keep]);
    assert!(matches!(r, Err(SubsetError::Unsupported(_))), "{r:?}");
}

#[test]
fn bake_cff2_partial_blend_on_missing_subtable_keeps_masters() {
    // The store has one subtable, but the charstring selects
    // subtable 3 before blending. With no regions to read, the
    // blend must leave its master in place instead of indexing past
    // the operand stack.
    let ivs = build_ivs1_for_cff2(
        &[[(0.0, 1.0, 1.0)]],
        &[(alloc::vec![0], alloc::vec![alloc::vec![50i16]])],
    );
    // 0 0 rmoveto, 3 vsindex, 0 1 blend, hmoveto.
    let cs: &[u8] = &[139, 139, 21, 142, 15, 139, 140, 16, 22];
    let cff = build_synthetic_cff2(&[cs], &[0], Some(&ivs));
    let new_cff = bake_cff2_partial(&cff, &[1.0], &[AxisPin::Keep]).expect("partial bake");
    let parsed = parse_cff2(&new_cff).expect("parse");
    assert_eq!(parsed.char_strings[0], &[139u8, 139, 21, 139, 22][..]);
}

#[test]
fn bake_at_coords_rejects_callsubr_operand_past_i32() {
    // 8000 regions that all peak at the bake coordinate. Nine
    // chained blends of 8000 deltas of 32767 each push the operand
    // past i32::MAX before a callsubr consumes it.
    const REGIONS: u16 = 8000;
    let regions: Vec<[(f32, f32, f32); 1]> = (0..REGIONS).map(|_| [(0.0, 1.0, 1.0)]).collect();
    let ivs = build_ivs1_for_cff2(&regions, &[((0..REGIONS).collect(), Vec::new())]);
    let max_shortint = [28u8, 0x7F, 0xFF];
    let mut cs: Vec<u8> = max_shortint.to_vec();
    for _ in 0..9 {
        for _ in 0..REGIONS {
            cs.extend_from_slice(&max_shortint);
        }
        cs.extend_from_slice(&[140, 16]); // 1 blend
    }
    cs.push(10); // callsubr
    let cff = build_synthetic_cff2(&[cs.as_slice()], &[0], Some(&ivs));
    let r = bake_at_coords(&cff, &[1.0]);
    assert!(matches!(r, Err(SubsetError::Unsupported(_))), "{r:?}");
}

#[test]
fn bakes_write_a_shared_private_dict_once() {
    // 255 Font DICTs name one Private DICT of 1,001 bytes. Both bakes
    // read it once and write it once, and every Font DICT points at
    // that copy, so the output stays the size of the input rather than
    // holding 255 copies.
    let mut private: Vec<u8> = alloc::vec![139u8; 1000];
    private.push(6); // BlueValues
    let cs: &[u8] = &[139, 139, 21];
    let charstrings: Vec<&[u8]> = alloc::vec![cs; 255];
    let fd_select: Vec<u8> = (0..=254).collect();
    let ivs = build_ivs2_for_cff2(
        &[[(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)]],
        &[(alloc::vec![0], alloc::vec![])],
    );
    let cff = build_synthetic_cff2_sharing(&charstrings, &fd_select, Some(&ivs), Some(&private));
    let parsed = parse_cff2(&cff).expect("source parses");
    assert!(parsed.private_of.iter().all(|&j| j == 0));
    assert!(parsed
        .per_fd_local_subrs
        .iter()
        .all(|l| alloc::rc::Rc::ptr_eq(l, &parsed.per_fd_local_subrs[0])));
    let full = bake_at_coords(&cff, &[0.5, 0.5]).expect("full bake");
    let partial =
        bake_cff2_partial(&cff, &[1.0, 0.0], &[AxisPin::Pin, AxisPin::Keep]).expect("partial bake");
    for out in [&full, &partial] {
        let baked = parse_cff2(out).expect("bake parses");
        assert_eq!(baked.fd_array.len(), 255);
        assert!(baked.private_of.iter().all(|&j| j == 0), "one Private DICT");
        assert_eq!(baked.per_fd_private[254].len(), private.len());
        assert!(
            out.len() < cff.len() + 64,
            "{} bytes from {}",
            out.len(),
            cff.len()
        );
    }
}

#[test]
fn bake_cff2_partial_moves_pinned_only_deltas_into_the_masters() {
    // Region 0 lies on axis 0 alone, region 1 on axis 1 alone. Pinning
    // axis 0 at 1 leaves region 0 with no peak on the kept axis, so it
    // applies alike at every kept coordinate, the new default included,
    // where renderers apply no variations: its delta moves into the
    // master and leaves the blend.
    let ivs = build_ivs2_for_cff2(
        &[
            [(0.0, 1.0, 1.0), (0.0, 0.0, 0.0)],
            [(0.0, 0.0, 0.0), (0.0, 1.0, 1.0)],
        ],
        &[(alloc::vec![0, 1], alloc::vec![])],
    );
    let cs: &[u8] = &[
        239, 159, 149, 140, 16, // 100 20 10 1 blend
        139, 21, // 0 rmoveto
    ];
    let cff = build_synthetic_cff2(&[cs], &[0], Some(&ivs));
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let out = bake_cff2_partial(&cff, &[1.0, 0.0], &pins).expect("partial bake");
    let parsed = parse_cff2(&out).expect("parse");
    let store = ItemVariationStore::parse(&parsed.vstore_blob.expect("vstore")[2..]).unwrap();
    assert_eq!(store.region_count(), 1, "the pinned-only region is gone");
    // 120 (100 + 20), then the kept delta 10, 1 blend, 0 rmoveto.
    assert_eq!(parsed.char_strings[0], &[247, 12, 149, 140, 16, 139, 21]);

    // A subtable on the pinned axis alone keeps no blend: the master
    // holds the pinned value.
    let ivs = build_ivs2_for_cff2(
        &[[(0.0, 1.0, 1.0), (0.0, 0.0, 0.0)]],
        &[(alloc::vec![0], alloc::vec![])],
    );
    let cs: &[u8] = &[239, 159, 140, 16, 139, 21]; // 100 20 1 blend 0 rmoveto
    let cff = build_synthetic_cff2(&[cs], &[0], Some(&ivs));
    let out = bake_cff2_partial(&cff, &[0.5, 0.0], &pins).expect("partial bake");
    let parsed = parse_cff2(&out).expect("parse");
    assert_eq!(parsed.char_strings[0], &[247, 2, 139, 21], "110, 0 rmoveto");
}

#[test]
fn a_null_subtable_offset_numbers_cff2_blends_as_the_store_does() {
    // Subtable 0's offset is null: the projection reads it as empty and
    // elides it, so subtable 1 becomes the store's subtable 0, and a
    // blend after `1 vsindex` names subtable 0 (no vsindex at all).
    let mut ivs = build_ivs2_for_cff2(
        &[[(0.0, 0.0, 0.0), (0.0, 1.0, 1.0)]],
        &[
            (alloc::vec![0], alloc::vec![]),
            (alloc::vec![0], alloc::vec![]),
        ],
    );
    ivs[8..12].copy_from_slice(&0u32.to_be_bytes());
    let cs: &[u8] = &[
        140, 15, // 1 vsindex
        239, 149, 140, 16, // 100 10 1 blend
        139, 21, // 0 rmoveto
    ];
    let cff = build_synthetic_cff2(&[cs], &[0], Some(&ivs));
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let out = bake_cff2_partial(&cff, &[1.0, 0.0], &pins).expect("partial bake");
    let parsed = parse_cff2(&out).expect("parse");
    let store = ItemVariationStore::parse(&parsed.vstore_blob.expect("vstore")[2..]).unwrap();
    assert_eq!(store.subtable_count(), 1);
    assert_eq!(parsed.char_strings[0], &[239, 149, 140, 16, 139, 21]);
}

/// Two axes, one subtable: region 0 lies on axis 0 alone and region 1
/// on axis 1 alone. Pinning axis 0 at 1 folds region 0's deltas into
/// the masters and keeps region 1's.
fn one_region_per_axis() -> Vec<u8> {
    build_ivs2_for_cff2(
        &[
            [(0.0, 1.0, 1.0), (0.0, 0.0, 0.0)],
            [(0.0, 0.0, 0.0), (0.0, 1.0, 1.0)],
        ],
        &[(alloc::vec![0, 1], alloc::vec![])],
    )
}

/// `0.5` and `-0.5` as 16.16 pushes.
const HALF: &[u8] = &[255, 0, 0, 0x80, 0];
const MINUS_HALF: &[u8] = &[255, 0xFF, 0xFF, 0x80, 0];

/// A charstring of `stale` zeros no operator reads, a master of 0, and
/// `pairs` pairs of chained one-master blends, `fold 0 1 blend -fold 0
/// 1 blend`, each taking the last blend's result as its master; then
/// `hmoveto`. With axis 0 pinned at 1, every blend moves `fold` or
/// `-fold` into that first master.
fn chained_blends(stale: usize, pairs: usize, fold: &[u8], minus_fold: &[u8]) -> Vec<u8> {
    let mut cs = alloc::vec![139u8; stale + 1];
    for _ in 0..pairs {
        cs.extend_from_slice(fold);
        cs.extend_from_slice(&[139, 140, 16]);
        cs.extend_from_slice(minus_fold);
        cs.extend_from_slice(&[139, 140, 16]);
    }
    cs.push(22);
    cs
}

/// Bakes `cs` with axis 0 pinned at 1 and axis 1 kept: the tokens the
/// bake charges and the baked charstring.
fn chain_work(cs: &[u8]) -> (usize, Vec<u8>) {
    let cff = build_synthetic_cff2(&[cs], &[0], Some(&one_region_per_axis()));
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let (out, left) =
        crate::cff2::partial::bake_cff2_partial_within(&cff, &[1.0, 0.0], &pins, usize::MAX)
            .expect("partial bake");
    let parsed = parse_cff2(&out).expect("parse");
    (usize::MAX - left, parsed.char_strings[0].to_vec())
}

#[test]
fn chained_blends_fold_into_their_master_at_the_same_work_per_blend() {
    // Every blend folds into the first master, whose push sits before
    // everything the chain writes and changes length with each fold (1
    // byte for 0, 5 for 0.5; 1 and 2 bytes for 0 and 1, integers). The
    // fold rewrites it in place, so each pair of blends costs the same
    // whatever the chain's length: its 8 tokens, and for each blend a
    // delta folded and one written.
    let integers: (&[u8], &[u8]) = (&[140], &[138]);
    for (fold, minus_fold) in [(HALF, MINUS_HALF), integers] {
        let (short, _) = chain_work(&chained_blends(0, 1_000, fold, minus_fold));
        let (long, cs) = chain_work(&chained_blends(0, 50_000, fold, minus_fold));
        assert_eq!(long - short, 49_000 * 12, "12 tokens a pair");
        assert_eq!(short, 1_000 * 12 + 2, "and the master and hmoveto");
        // The folds cancel: the master is back at 0, and each blend
        // keeps region 1's delta.
        let mut expected = alloc::vec![139u8];
        for _ in 0..100_000 {
            expected.extend_from_slice(&[139, 140, 16]);
        }
        expected.push(22);
        assert_eq!(cs, expected);
    }
}

#[test]
fn stale_operands_under_a_chain_add_no_work_per_blend() {
    // 500 operands sit under the chain until hmoveto clears them. Each
    // costs its push and nothing per blend.
    for pairs in [1_000, 10_000] {
        let (bare, _) = chain_work(&chained_blends(0, pairs, HALF, MINUS_HALF));
        let (stale, cs) = chain_work(&chained_blends(500, pairs, HALF, MINUS_HALF));
        assert_eq!(stale - bare, 500);
        assert_eq!(cs[..502], [139u8; 502]);
        assert_eq!(cs.len(), 502 + 2 * pairs * 3);
    }
}

#[test]
fn an_operand_past_the_cff2_stack_limit_fails_the_bake() {
    // CFF2 allows 513 operands; HarfBuzz's interpreter stops at the
    // 514th push, and so does the bake, whether the pushes come from
    // the charstring or from the subroutines it calls.
    let ivs = one_region_per_axis();
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let overflows = |r: Result<Vec<u8>, SubsetError>| matches!(r, Err(SubsetError::Unsupported(m)) if m.contains("operand stack overflow"));
    let mut cs = alloc::vec![139u8; 513];
    cs.push(22);
    let cff = build_synthetic_cff2(&[&cs], &[0], Some(&ivs));
    let out = bake_cff2_partial(&cff, &[1.0, 0.0], &pins).expect("513 operands");
    assert_eq!(parse_cff2(&out).unwrap().char_strings[0], &cs[..]);
    cs.insert(0, 139);
    let cff = build_synthetic_cff2(&[&cs], &[0], Some(&ivs));
    assert!(overflows(bake_cff2_partial(&cff, &[1.0, 0.0], &pins)));

    // Subroutine 0 pushes 100 zeros and returns; six calls push 600.
    let mut subr = alloc::vec![139u8; 100];
    subr.push(11);
    let cs: Vec<u8> = [32u8, 10].repeat(6).into_iter().chain([22]).collect();
    let cff = build_synthetic_cff2_with_local_subrs(&[&cs], &[0], &[&subr], Some(&ivs));
    assert!(overflows(bake_cff2_partial(&cff, &[1.0, 0.0], &pins)));
}
