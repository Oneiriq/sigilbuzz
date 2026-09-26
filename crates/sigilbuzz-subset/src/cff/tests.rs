//! Unit tests for the CFF charstring analysis layer.

use super::charstring::{
    compute_cross_fd_globals, decode_operand, OP_CALLGSUBR, OP_CALLSUBR, OP_ENDCHAR, OP_ESCAPE,
    OP_HINTMASK, OP_HSTEM, OP_RETURN, OP_RMOVETO, OP_SHORTINT,
};
use super::*;

mod cid;
mod emit;
mod orchestration;

#[test]
fn bias_at_spec_boundaries() {
    assert_eq!(subr_bias(0), 107);
    assert_eq!(subr_bias(1239), 107);
    assert_eq!(subr_bias(1240), 1131);
    assert_eq!(subr_bias(33_899), 1131);
    assert_eq!(subr_bias(33_900), 32_768);
}

#[test]
fn encode_decode_round_trip_small() {
    for v in [-107, -50, 0, 50, 107] {
        let enc = encode_int_operand(v);
        assert_eq!(enc.len(), 1);
        let (dec, len) = decode_operand(&enc, 0).unwrap();
        assert_eq!(dec, v);
        assert_eq!(len, 1);
    }
}

#[test]
fn encode_decode_round_trip_two_byte_pos() {
    for v in [108, 500, 1131] {
        let enc = encode_int_operand(v);
        assert_eq!(enc.len(), 2);
        let (dec, len) = decode_operand(&enc, 0).unwrap();
        assert_eq!(dec, v);
        assert_eq!(len, 2);
    }
}

#[test]
fn encode_decode_round_trip_two_byte_neg() {
    for v in [-108, -500, -1131] {
        let enc = encode_int_operand(v);
        assert_eq!(enc.len(), 2);
        let (dec, len) = decode_operand(&enc, 0).unwrap();
        assert_eq!(dec, v);
        assert_eq!(len, 2);
    }
}

#[test]
fn encode_shortint_for_mid_range() {
    for v in [1132, -1132, 5000, -5000, 32_000, -32_000] {
        let enc = encode_int_operand(v);
        assert_eq!(enc.len(), 3);
        assert_eq!(enc[0], OP_SHORTINT);
        // Scanner decodes shortint inline, not via decode_operand.
        let raw = i16::from_be_bytes([enc[1], enc[2]]);
        assert_eq!(i32::from(raw), v);
    }
}

#[test]
fn scan_finds_callsubr_at_bias_minus_107() {
    // Push 0 (operand encoded as 139, single byte), then callsubr.
    // local_count = 0 -> bias = 107 -> index_after_bias = 107.
    let cs = [139, OP_CALLSUBR, OP_ENDCHAR];
    let calls = scan_subr_calls(&cs, 0, 0).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].kind, SubrKind::Local);
    assert_eq!(calls[0].index_after_bias, 107);
    assert_eq!(calls[0].raw_operand, 0);
    assert_eq!(calls[0].operand_byte_offset, 0);
    assert_eq!(calls[0].operand_byte_len, 1);
}

#[test]
fn scan_finds_callgsubr_with_negative_bias() {
    // Push -100 via two-byte (251..=254) form. value = -100
    // requires b0=251, value = -(0)*256 - b1 - 108 = -100 -> b1 =
    // -8 which is out of range; -100 doesn't encode in two bytes.
    // Use shortint instead: op 28 + i16(-100).
    let mut cs = alloc::vec![OP_SHORTINT];
    cs.extend_from_slice(&(-100i16).to_be_bytes());
    cs.push(OP_CALLGSUBR);
    cs.push(OP_ENDCHAR);
    // global_count = 0 -> bias = 107 -> index = -100 + 107 = 7.
    let calls = scan_subr_calls(&cs, 0, 0).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].kind, SubrKind::Global);
    assert_eq!(calls[0].index_after_bias, 7);
    assert_eq!(calls[0].raw_operand, -100);
    assert_eq!(calls[0].operand_byte_len, 3);
}

#[test]
fn scan_uses_1131_bias_at_1240_count() {
    // local_count = 1240 -> bias = 1131. operand = -1131 -> index 0.
    // -1131 encodes as two-byte negative: b0=254, value = -(3)*256 - b1 - 108 = -1131
    //   -> -768 - b1 - 108 = -1131 -> b1 = 255.
    let cs = [254u8, 255, OP_CALLSUBR, OP_ENDCHAR];
    let calls = scan_subr_calls(&cs, 1240, 0).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].index_after_bias, 0);
    assert_eq!(calls[0].raw_operand, -1131);
}

#[test]
fn scan_uses_32768_bias_at_33900_count() {
    // local_count = 33_900 -> bias = 32_768. operand = -32_768 -> index 0.
    // -32_768 needs shortint encoding (out of two-byte range).
    let mut cs = alloc::vec![OP_SHORTINT];
    cs.extend_from_slice(&(-32_768i16).to_be_bytes());
    cs.push(OP_CALLSUBR);
    cs.push(OP_ENDCHAR);
    let calls = scan_subr_calls(&cs, 33_900, 0).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].index_after_bias, 0);
}

#[test]
fn scan_skips_hintmask_tail_bytes() {
    // Push 100 200 hstem hintmask <mask byte> 0 callsubr endchar.
    // Stem pair count = 1 -> hintmask reads ceil(1/8) = 1 mask byte.
    // Then 0 callsubr resolves to local subr 0 -> bias 107.
    let cs: Vec<u8> = alloc::vec![
        239, // 100 (single-byte form: 239 - 139 = 100)
        247, // 200 = (247-247)*256 + 92 + 108 -> b1 = 92
        92,
        OP_HSTEM,
        OP_HINTMASK,
        0xff, // 1 mask byte
        139,  // 0
        OP_CALLSUBR,
        OP_ENDCHAR,
    ];
    let calls = scan_subr_calls(&cs, 0, 0).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].index_after_bias, 107);
}

#[test]
fn scan_handles_escape_op() {
    // 0 0 rmoveto, then escape + arbitrary subop, then push 0
    // callsubr endchar. Scanner should clear the stack on escape
    // and still detect the call.
    let cs = [
        139, // 0
        139, // 0
        OP_RMOVETO,
        OP_ESCAPE,
        34, // hflex (any escape op)
        139,
        OP_CALLSUBR,
        OP_ENDCHAR,
    ];
    let calls = scan_subr_calls(&cs, 0, 0).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].index_after_bias, 107);
}

#[test]
fn compute_kept_subrs_transitively() {
    // Local subr 0 calls local subr 1; charstring calls local
    // subr 0; both subrs must end up in the keep set.
    // Bias = 107 (count < 1240), so call to subr 0 has raw
    // operand (0 - 107) = -107 and call to subr 1 has raw
    // operand (1 - 107) = -106. Both fall outside the two-byte
    // negative range (-1131..=-108) so we encode via shortint.
    let mut local_0 = alloc::vec![OP_SHORTINT];
    local_0.extend_from_slice(&(-106i16).to_be_bytes());
    local_0.push(OP_CALLSUBR);
    local_0.push(OP_RETURN);

    let local_1 = alloc::vec![
        139u8, /* 0 */
        139u8, /* 0 */
        OP_RMOVETO, OP_RETURN
    ];

    let mut cs = alloc::vec![OP_SHORTINT];
    cs.extend_from_slice(&(-107i16).to_be_bytes());
    cs.push(OP_CALLSUBR);
    cs.push(OP_ENDCHAR);

    let local_refs: Vec<&[u8]> = alloc::vec![local_0.as_slice(), local_1.as_slice()];
    let global_refs: Vec<&[u8]> = Vec::new();
    let cs_refs: Vec<&[u8]> = alloc::vec![cs.as_slice()];

    let (kl, kg) = compute_kept_subrs(&cs_refs, &local_refs, &global_refs).unwrap();
    assert_eq!(kl, alloc::vec![0u32, 1]);
    assert!(kg.is_empty());
}

#[test]
fn compute_kept_subrs_drops_unused() {
    // Three local subrs, charstring calls only subr 1.
    let mut cs = alloc::vec![OP_SHORTINT];
    cs.extend_from_slice(&(-106i16).to_be_bytes()); // call subr 1
    cs.push(OP_CALLSUBR);
    cs.push(OP_ENDCHAR);

    let stub = alloc::vec![OP_RETURN];
    let local_refs: Vec<&[u8]> = alloc::vec![&stub, &stub, &stub];
    let global_refs: Vec<&[u8]> = Vec::new();
    let cs_refs: Vec<&[u8]> = alloc::vec![cs.as_slice()];

    let (kl, _) = compute_kept_subrs(&cs_refs, &local_refs, &global_refs).unwrap();
    assert_eq!(kl, alloc::vec![1u32]);
}

#[test]
fn cross_fd_detection_flags_direct_local_caller() {
    // Global 0 calls local 0 (cross-FD). Global 1 is purely
    // graphical (no calls). Detection should flag only global 0.
    let mut g0 = alloc::vec![OP_SHORTINT];
    g0.extend_from_slice(&(-107i16).to_be_bytes()); // local 0 (bias 107)
    g0.push(OP_CALLSUBR);
    g0.push(OP_RETURN);
    let g1 = alloc::vec![139u8, 139u8, OP_RMOVETO, OP_RETURN];

    let globals: Vec<&[u8]> = alloc::vec![g0.as_slice(), g1.as_slice()];
    let is_cross = compute_cross_fd_globals(&globals, 1).unwrap();
    assert_eq!(is_cross, alloc::vec![true, false]);
}

#[test]
fn cross_fd_detection_propagates_through_global_chain() {
    // Global 0 calls local 0 (cross-FD).
    // Global 1 calls global 0 (transitively cross-FD).
    // Global 2 calls global 1 (transitively cross-FD).
    // Global 3 is graphical only.
    let mut g0 = alloc::vec![OP_SHORTINT];
    g0.extend_from_slice(&(-107i16).to_be_bytes()); // local 0
    g0.push(OP_CALLSUBR);
    g0.push(OP_RETURN);

    let mut g1 = alloc::vec![OP_SHORTINT];
    g1.extend_from_slice(&(-107i16).to_be_bytes()); // global 0 (bias 107, count < 1240)
    g1.push(OP_CALLGSUBR);
    g1.push(OP_RETURN);

    let mut g2 = alloc::vec![OP_SHORTINT];
    g2.extend_from_slice(&(-106i16).to_be_bytes()); // global 1
    g2.push(OP_CALLGSUBR);
    g2.push(OP_RETURN);

    let g3 = alloc::vec![139u8, 139u8, OP_RMOVETO, OP_RETURN];

    let globals: Vec<&[u8]> =
        alloc::vec![g0.as_slice(), g1.as_slice(), g2.as_slice(), g3.as_slice(),];
    let is_cross = compute_cross_fd_globals(&globals, 1).unwrap();
    assert_eq!(is_cross, alloc::vec![true, true, true, false]);
}

#[test]
fn cross_fd_detection_clean_when_no_global_calls_local() {
    // Two globals, neither calls a local; charstring would not be
    // cross-FD even if it does (we only inspect globals here).
    let g0 = alloc::vec![139u8, 139u8, OP_RMOVETO, OP_RETURN];
    let mut g1 = alloc::vec![OP_SHORTINT];
    g1.extend_from_slice(&(-107i16).to_be_bytes()); // global 0
    g1.push(OP_CALLGSUBR);
    g1.push(OP_RETURN);
    let globals: Vec<&[u8]> = alloc::vec![g0.as_slice(), g1.as_slice()];
    let is_cross = compute_cross_fd_globals(&globals, 4).unwrap();
    assert_eq!(is_cross, alloc::vec![false, false]);
}

#[test]
fn scan_truncated_operand_errors() {
    // 247 expects a follow-up byte; truncating it is malformed.
    let cs = [247u8];
    let r = scan_subr_calls(&cs, 0, 0);
    assert!(r.is_err());
}

#[test]
fn scan_unknown_op_errors() {
    // op 9 is reserved -> must error.
    let cs = [9u8];
    let r = scan_subr_calls(&cs, 0, 0);
    assert!(r.is_err());
}
