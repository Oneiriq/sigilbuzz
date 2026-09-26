//! Non-CID CFF1 subset orchestration round trips.

use super::*;

// -- orchestration round-trip tests -------------------------------------

/// Builds a synthetic non-CID CFF1 with `n` glyphs (gid 0 = empty
/// .notdef charstring, gid 1..n carry the supplied charstrings).
/// One global subr + one local subr live in the table even when
/// the charstrings don't reference them, so the rewriter has to
/// drop them.
fn build_synthetic_cff1(charstrings: &[&[u8]]) -> Vec<u8> {
    // Glyph 0 is .notdef. Encode as a minimal endchar charstring.
    let notdef: Vec<u8> = alloc::vec![14u8 /* OP_ENDCHAR */];
    let mut all_cs: Vec<Vec<u8>> = alloc::vec![notdef];
    all_cs.extend(charstrings.iter().map(|s| s.to_vec()));
    let cs_refs: Vec<&[u8]> = all_cs.iter().map(Vec::as_slice).collect();

    let header = alloc::vec![1u8, 0, 4, 1]; // CFF major=1 minor=0 hdrSize=4 offSize=1.
    let name_index = encode_index(&[b"Synthetic"]);
    let string_index = encode_index(&[]);
    let global_subr_index = encode_index(&[]);

    // Charset: explicit format 0 with SIDs counting up from 1 per
    // glyph past gid 0. SIDs land in the predefined ISOAdobe range
    // so we don't need to grow the String INDEX.
    let charset_sids: Vec<u16> = (1..(all_cs.len() as u16)).collect();
    let charset_bytes = emit_charset_format0(&charset_sids);

    // Encoding format 0 with one byte per gid past gid 0.
    let codes: Vec<u8> = (1..all_cs.len()).map(|i| i as u8).collect();
    let encoding_bytes = emit_encoding_format0(&codes);

    let cs_index = encode_index(&cs_refs);

    // Private DICT carries op 19 (Subrs) referencing one local subr
    // (an empty `RETURN` body) so the orchestration has subrs to
    // drop. Body uses a 5-byte placeholder offset that we patch
    // later.
    let local_subrs: Vec<&[u8]> = alloc::vec![&[11u8 /* OP_RETURN */] as &[u8]];
    let local_subr_index = encode_index(&local_subrs);

    // Build Top DICT with placeholder offsets for charset (15),
    // Encoding (16), CharStrings (17), Private (18=size+off pair).
    // We patch the placeholders once layout is known.
    let mut top_dict: Vec<u8> = Vec::new();
    let charset_slot = top_dict.len();
    top_dict.extend_from_slice(&encode_dict_offset_placeholder());
    top_dict.push(15);
    let encoding_slot = top_dict.len();
    top_dict.extend_from_slice(&encode_dict_offset_placeholder());
    top_dict.push(16);
    let charstrings_slot = top_dict.len();
    top_dict.extend_from_slice(&encode_dict_offset_placeholder());
    top_dict.push(17);
    let priv_size_slot = top_dict.len();
    top_dict.extend_from_slice(&encode_dict_offset_placeholder());
    let priv_off_slot = top_dict.len();
    top_dict.extend_from_slice(&encode_dict_offset_placeholder());
    top_dict.push(18);

    // Private DICT body: op 19 with placeholder offset.
    let mut private_dict: Vec<u8> = Vec::new();
    let priv_subrs_slot = private_dict.len();
    private_dict.extend_from_slice(&encode_dict_offset_placeholder());
    private_dict.push(19);

    // Top DICT INDEX.
    let top_dict_index = encode_index(&[&top_dict[..]]);
    // We need the body offset within the index for patching.
    let top_dict_body_offset_in_index = {
        let total = 1 + top_dict.len();
        let off_size: usize = if total <= 0xFF { 1 } else { 2 };
        2 + 1 + 2 * off_size
    };

    // Layout: header | name | top dict idx | string idx | gsubr idx
    //       | encoding | charset | charstrings idx | private dict | local subr idx.
    let mut out = Vec::new();
    out.extend_from_slice(&header);
    out.extend_from_slice(&name_index);

    let top_dict_index_start = out.len();
    out.extend_from_slice(&top_dict_index);
    let top_dict_body_abs = top_dict_index_start + top_dict_body_offset_in_index;

    out.extend_from_slice(&string_index);
    out.extend_from_slice(&global_subr_index);

    let encoding_abs = out.len();
    out.extend_from_slice(&encoding_bytes);

    let charset_abs = out.len();
    out.extend_from_slice(&charset_bytes);

    let cs_abs = out.len();
    out.extend_from_slice(&cs_index);

    let private_abs = out.len();
    let private_size = private_dict.len();
    out.extend_from_slice(&private_dict);

    let local_subr_abs = out.len();
    out.extend_from_slice(&local_subr_index);

    // Patch Top DICT placeholder offsets.
    patch_dict_offset(
        &mut out,
        top_dict_body_abs + charset_slot,
        charset_abs as i32,
    );
    patch_dict_offset(
        &mut out,
        top_dict_body_abs + encoding_slot,
        encoding_abs as i32,
    );
    patch_dict_offset(
        &mut out,
        top_dict_body_abs + charstrings_slot,
        cs_abs as i32,
    );
    patch_dict_offset(
        &mut out,
        top_dict_body_abs + priv_size_slot,
        private_size as i32,
    );
    patch_dict_offset(
        &mut out,
        top_dict_body_abs + priv_off_slot,
        private_abs as i32,
    );
    // Patch Private DICT op 19 (Subrs) offset relative to Private DICT start.
    patch_dict_offset(
        &mut out,
        private_abs + priv_subrs_slot,
        (local_subr_abs - private_abs) as i32,
    );

    out
}

#[test]
fn orchestration_keeps_only_requested_glyphs() {
    // 3 glyphs: gid 0 .notdef, gid 1 = simple endchar, gid 2 = different endchar.
    // After subset to [0, 2], the new CFF should list 2 charstrings
    // and the kept charstring's bytes should still resolve.
    let cs1: &[u8] = &[139, 139, 21, 14]; // 0 0 rmoveto endchar
    let cs2: &[u8] = &[139, 14]; // 0 endchar (1 arg, allowed: width)
    let cff = build_synthetic_cff1(&[cs1, cs2]);

    let kept = alloc::vec![0u16, 2];
    let new_cff = subset_non_identity(&cff, &kept).unwrap();

    // Re-parse the rewritten CFF to assert the CharStrings INDEX
    // shrank to 2 and the second entry equals cs2.
    let parsed = parse_cff1(&new_cff).unwrap();
    assert_eq!(parsed.char_strings.len(), 2);
    // Gid 0 = .notdef (preserved verbatim from source notdef).
    assert_eq!(parsed.char_strings[0], &[14u8]);
    assert_eq!(parsed.char_strings[1], cs2);
}

#[test]
fn orchestration_drops_unused_subrs() {
    // Source carries one local subr that no kept charstring calls;
    // after subset the local subr INDEX must be empty.
    let cs1: &[u8] = &[139, 139, 21, 14];
    let cff = build_synthetic_cff1(&[cs1]);
    let new_cff = subset_non_identity(&cff, &[0, 1]).unwrap();
    let parsed = parse_cff1(&new_cff).unwrap();
    assert!(parsed.local_subrs.is_empty());
}

#[test]
fn orchestration_preserves_name_and_string_indexes() {
    let cs1: &[u8] = &[139, 14];
    let cff = build_synthetic_cff1(&[cs1]);
    let new_cff = subset_non_identity(&cff, &[0, 1]).unwrap();

    let orig = parse_cff1(&cff).unwrap();
    let rebuilt = parse_cff1(&new_cff).unwrap();
    assert_eq!(orig.name_index, rebuilt.name_index);
    assert_eq!(orig.string_index, rebuilt.string_index);
    assert_eq!(orig.header, rebuilt.header);
}

#[test]
fn orchestration_rejects_kept_set_without_gid0() {
    let cs1: &[u8] = &[139, 14];
    let cff = build_synthetic_cff1(&[cs1]);
    let r = subset_non_identity(&cff, &[1u16]);
    assert!(r.is_err());
}

#[test]
fn orchestration_rejects_cid_keyed_source() {
    // Build a synthetic CFF1 with op 0x0C24 (FDArray) in the Top
    // DICT and assert the orchestration declines.
    let cs1: &[u8] = &[139, 14];
    let mut cff = build_synthetic_cff1(&[cs1]);
    // Inject a fake FDArray op into the Top DICT body. Easiest:
    // append a (0 12 36) operand+op tail to the Top DICT body in
    // place, but since Top DICT INDEX wraps the body the simplest
    // way is to walk the bytes and inject the op there. For this
    // test we cheat and use parse_cff1's `is_cid` path indirectly
    // by asserting the explicit subset_non_identity error on a
    // hand-built tiny CFF that includes the FDArray op.
    //
    // The easier check: walk the Top DICT we already serialized,
    // find the b0=29 5-byte `Encoding` placeholder (16) and
    // overwrite the operator byte to op 12+0x24 (FDArray).
    let _ = &mut cff; // keep the unused-mut lint quiet.

    // Hand-build a minimal CFF that carries op 0x0C24 in the Top DICT.
    let header = alloc::vec![1u8, 0, 4, 1];
    let name_index = encode_index(&[b"X"]);
    let string_index = encode_index(&[]);
    let global_subr_index = encode_index(&[]);
    let cs_index = encode_index(&[&[14u8] as &[u8]]);
    let mut top: Vec<u8> = Vec::new();
    // CharStrings op (placeholder).
    let cs_slot = top.len();
    top.extend_from_slice(&encode_dict_offset_placeholder());
    top.push(17);
    // FDArray op (operand 0, op 12 36).
    top.extend_from_slice(&encode_dict_int(0));
    top.push(12);
    top.push(0x24);
    let top_idx = encode_index(&[&top[..]]);
    let mut buf = Vec::new();
    buf.extend_from_slice(&header);
    buf.extend_from_slice(&name_index);
    let top_idx_start = buf.len();
    buf.extend_from_slice(&top_idx);
    let top_body_off = {
        let total = 1 + top.len();
        let off_size: usize = if total <= 0xFF { 1 } else { 2 };
        2 + 1 + 2 * off_size
    };
    buf.extend_from_slice(&string_index);
    buf.extend_from_slice(&global_subr_index);
    let cs_abs = buf.len();
    buf.extend_from_slice(&cs_index);
    patch_dict_offset(
        &mut buf,
        top_idx_start + top_body_off + cs_slot,
        cs_abs as i32,
    );

    let r = subset_non_identity(&buf, &[0u16]);
    assert!(matches!(r, Err(SubsetError::Unsupported(_))));
}

#[test]
fn orchestration_renumbers_callsubr_when_local_subrs_kept() {
    // Source: 1 local subr (a no-op rmoveto+return body), one
    // charstring that calls that local subr (callsubr 0). After
    // subset, the local subr survives at the same logical index 0
    // (only one kept) but its byte position in the new INDEX may
    // differ. The rewriter still walks the call site and patches
    // the operand even for a no-op renumber.
    //
    // Build: subr at logical index 0 -> call with operand (0 - 107)
    // = -107 (single byte: 32). Charstring calls subr 0 -> push -107
    // (single byte 32) + callsubr.
    let cs: Vec<u8> = alloc::vec![32u8 /* push -107 */, 10 /* OP_CALLSUBR */, 14];

    // Build a CFF identical in shape to build_synthetic_cff1 but
    // wire the local subr to be referenced by the charstring.
    // build_synthetic_cff1 already provides a single local subr (a
    // RETURN body) and a charstring that doesn't reference it; we
    // just supply our subroutine-calling charstring instead.
    let cff = build_synthetic_cff1(&[&cs]);

    // Subset to all glyphs (kept set [0,1]).
    let new_cff = subset_non_identity(&cff, &[0u16, 1]).unwrap();
    let parsed = parse_cff1(&new_cff).unwrap();
    // The kept local subr is still there.
    assert_eq!(parsed.local_subrs.len(), 1);
    // Charstring 1 still has callsubr at byte 1. The operand byte
    // re-encoded at original width (1 byte).
    let kept_cs = parsed.char_strings[1];
    assert_eq!(kept_cs.len(), cs.len());
    assert_eq!(kept_cs[1], 10); // OP_CALLSUBR preserved.
    assert_eq!(kept_cs[2], 14); // OP_ENDCHAR preserved.
}
