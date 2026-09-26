//! Emitter primitive, FDSelect, and charstring renumber tests.

use super::*;
use crate::cff::charstring::encode_int_operand_at_width;

// -- emitter primitive tests ---------------------------------------------

#[test]
fn encode_index_empty() {
    // Empty INDEX is just the count (zero u16), no offSize / offsets.
    let bytes = encode_index(&[]);
    assert_eq!(bytes, alloc::vec![0u8, 0]);
}

#[test]
fn encode_index_single_entry() {
    // count=1, offSize=1, offsets=[1, 4], data=[a,b,c]
    let entry = [0xAAu8, 0xBB, 0xCC];
    let bytes = encode_index(&[&entry]);
    assert_eq!(bytes[0..2], 1u16.to_be_bytes());
    assert_eq!(bytes[2], 1); // offSize
    assert_eq!(bytes[3], 1); // offset[0]
    assert_eq!(bytes[4], 4); // offset[1] = 1 + 3
    assert_eq!(&bytes[5..8], &entry);
}

#[test]
fn encode_index_multi_entry_2byte_offsets() {
    // Total > 255 forces 2-byte offSize.
    let big = alloc::vec![0u8; 300];
    let small = alloc::vec![1u8; 10];
    let bytes = encode_index(&[&big, &small]);
    assert_eq!(bytes[0..2], 2u16.to_be_bytes());
    assert_eq!(bytes[2], 2); // offSize
                             // offset[0] = 1 (u16 BE)
    assert_eq!(bytes[3..5], 1u16.to_be_bytes());
    // offset[1] = 1 + 300 = 301
    assert_eq!(bytes[5..7], 301u16.to_be_bytes());
    // offset[2] = 311
    assert_eq!(bytes[7..9], 311u16.to_be_bytes());
}

#[test]
fn encode_dict_int_round_trip_small() {
    for v in [-107, 0, 107] {
        let enc = encode_dict_int(v);
        assert_eq!(enc.len(), 1);
    }
    for v in [108, 1131] {
        let enc = encode_dict_int(v);
        assert_eq!(enc.len(), 2);
    }
    for v in [-108, -1131] {
        let enc = encode_dict_int(v);
        assert_eq!(enc.len(), 2);
    }
}

#[test]
fn encode_dict_int_uses_5byte_for_large() {
    let enc = encode_dict_int(1_000_000);
    assert_eq!(enc.len(), 5);
    assert_eq!(enc[0], 29);
    assert_eq!(
        i32::from_be_bytes([enc[1], enc[2], enc[3], enc[4]]),
        1_000_000
    );
}

#[test]
fn dict_offset_placeholder_round_trip() {
    // Reserve a placeholder slot, patch it, decode the result.
    let mut buf = alloc::vec![0u8; 10];
    let slot = 2;
    let placeholder = encode_dict_offset_placeholder();
    buf[slot..slot + placeholder.len()].copy_from_slice(&placeholder);
    patch_dict_offset(&mut buf, slot, 0x1234_5678);
    assert_eq!(buf[slot], 29);
    assert_eq!(
        i32::from_be_bytes([buf[slot + 1], buf[slot + 2], buf[slot + 3], buf[slot + 4]]),
        0x1234_5678,
    );
}

#[test]
fn charset_format0_per_gid_sids() {
    // SIDs for gid 1, 2, 3 = [10, 20, 30]
    let bytes = emit_charset_format0(&[10, 20, 30]);
    assert_eq!(bytes[0], 0); // format
    assert_eq!(bytes.len(), 1 + 3 * 2);
    assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 10);
    assert_eq!(u16::from_be_bytes([bytes[3], bytes[4]]), 20);
    assert_eq!(u16::from_be_bytes([bytes[5], bytes[6]]), 30);
}

#[test]
fn charset_format2_collapses_consecutive_runs() {
    // Three consecutive SIDs collapse to one record.
    let bytes = emit_charset_format2(&[100, 101, 102]);
    assert_eq!(bytes[0], 2);
    // first = 100, nLeft = 2
    assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 100);
    assert_eq!(u16::from_be_bytes([bytes[3], bytes[4]]), 2);
    assert_eq!(bytes.len(), 5);
}

#[test]
fn charset_format2_handles_sid_at_u16_boundary() {
    // A SID of 0xFFFF followed by an unrelated SID used to overflow
    // when the run extender computed `sids[j-1] + 1`. The boundary
    // SID must terminate the run cleanly without panicking.
    let bytes = emit_charset_format2(&[0xFFFFu16, 100u16]);
    // Two single-entry records: (first=0xFFFF, nLeft=0) and
    // (first=100, nLeft=0).
    assert_eq!(bytes[0], 2);
    assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 0xFFFF);
    assert_eq!(u16::from_be_bytes([bytes[3], bytes[4]]), 0);
    assert_eq!(u16::from_be_bytes([bytes[5], bytes[6]]), 100);
    assert_eq!(u16::from_be_bytes([bytes[7], bytes[8]]), 0);
}

#[test]
fn charset_auto_picks_format2_on_dense_runs() {
    // Long consecutive run: format 2 wins (one 4-byte record vs.
    // n*2-byte format 0).
    let sids: Vec<u16> = (1..=20).collect();
    let auto = emit_charset_auto(&sids);
    // Format 2: 1 + 4 = 5 bytes. Format 0: 1 + 40 = 41.
    assert_eq!(auto[0], 2);
    assert_eq!(auto.len(), 5);
}

#[test]
fn charset_auto_picks_format0_on_random() {
    // Random SIDs (every-other): format 2 needs 4 bytes per record
    // = 4n; format 0 needs 2n + 1. Format 0 wins.
    let sids: Vec<u16> = (0..10).map(|i| i * 7).collect();
    let auto = emit_charset_auto(&sids);
    assert_eq!(auto[0], 0);
}

#[test]
fn encoding_format0_per_gid_codes() {
    let bytes = emit_encoding_format0(&[0x41, 0x42, 0x43]);
    assert_eq!(bytes[0], 0);
    assert_eq!(bytes[1], 3);
    assert_eq!(&bytes[2..5], &[0x41u8, 0x42, 0x43]);
}

#[test]
fn encoding_format1_collapses_consecutive() {
    let bytes = emit_encoding_format1(&[0x40, 0x41, 0x42, 0x43]);
    assert_eq!(bytes[0], 1);
    assert_eq!(bytes[1], 1); // one range
    assert_eq!(bytes[2], 0x40); // first
    assert_eq!(bytes[3], 3); // nLeft
}

#[test]
fn encoding_auto_picks_format1_on_dense() {
    let codes: Vec<u8> = (0x20..=0x7E).collect();
    let auto = emit_encoding_auto(&codes);
    // Format 0: 1 + 1 + 95 = 97. Format 1: 1 + 1 + 2 = 4.
    assert_eq!(auto[0], 1);
    assert!(auto.len() < 10);
}

// -- FDSelect tests ------------------------------------------------------

#[test]
fn fd_select_format0_round_trip() {
    // Per-gid FD indices [0, 0, 1, 1, 0]. Format 0 emits format
    // byte + raw bytes.
    let per_gid = alloc::vec![0u8, 0, 1, 1, 0];
    let bytes = emit_fd_select_format0(&per_gid);
    assert_eq!(bytes.len(), 6);
    assert_eq!(bytes[0], 0);
    assert_eq!(&bytes[1..], &per_gid[..]);
    let parsed = parse_fd_select(&bytes, 0, per_gid.len()).unwrap();
    assert_eq!(parsed, per_gid);
}

#[test]
fn fd_select_format3_collapses_runs() {
    // Per-gid FD indices: 5 zeros then 3 ones. Two ranges + sentinel.
    let per_gid = alloc::vec![0u8, 0, 0, 0, 0, 1, 1, 1];
    let bytes = emit_fd_select_format3(&per_gid);
    // Header: format(1) + nRanges(2) = 3 bytes.
    // Two ranges: 2 * 3 = 6.
    // Sentinel: 2.
    assert_eq!(bytes.len(), 3 + 6 + 2);
    assert_eq!(bytes[0], 3);
    assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 2); // 2 ranges
                                                             // First range: gid 0 -> fd 0
    assert_eq!(u16::from_be_bytes([bytes[3], bytes[4]]), 0);
    assert_eq!(bytes[5], 0);
    // Second range: gid 5 -> fd 1
    assert_eq!(u16::from_be_bytes([bytes[6], bytes[7]]), 5);
    assert_eq!(bytes[8], 1);
    // Sentinel = nGlyphs = 8
    assert_eq!(u16::from_be_bytes([bytes[9], bytes[10]]), 8);

    let parsed = parse_fd_select(&bytes, 0, per_gid.len()).unwrap();
    assert_eq!(parsed, per_gid);
}

#[test]
fn fd_select_format3_singleton_range() {
    // Single FD across all gids -> one range.
    let per_gid = alloc::vec![0u8; 10];
    let bytes = emit_fd_select_format3(&per_gid);
    assert_eq!(bytes[0], 3);
    assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 1);
    // First range: gid 0 -> fd 0
    assert_eq!(u16::from_be_bytes([bytes[3], bytes[4]]), 0);
    assert_eq!(bytes[5], 0);
    // Sentinel = 10
    assert_eq!(u16::from_be_bytes([bytes[6], bytes[7]]), 10);

    let parsed = parse_fd_select(&bytes, 0, per_gid.len()).unwrap();
    assert_eq!(parsed, per_gid);
}

#[test]
fn fd_select_auto_picks_format3_on_uniform() {
    // 100 zeros: format 0 is 1 + 100 = 101 bytes, format 3 is
    // 3 + 3 + 2 = 8 bytes. Format 3 wins.
    let per_gid = alloc::vec![0u8; 100];
    let auto = emit_fd_select_auto(&per_gid);
    assert_eq!(auto[0], 3);
    assert!(auto.len() < 20);
}

#[test]
fn fd_select_auto_picks_format0_on_alternating() {
    // 10 alternating values: format 0 is 1 + 10 = 11 bytes,
    // format 3 is 3 + 30 + 2 = 35 bytes. Format 0 wins.
    let per_gid: Vec<u8> = (0..10u8).map(|i| i & 1).collect();
    let auto = emit_fd_select_auto(&per_gid);
    assert_eq!(auto[0], 0);
    assert_eq!(auto.len(), 11);
}

#[test]
fn parse_fd_select_format0_short_errors() {
    // Format 0 with declared length but missing bytes.
    let bytes = alloc::vec![0u8, 1, 2]; // 3 bytes total: format byte + 2 entries
    let r = parse_fd_select(&bytes, 0, 5);
    assert!(r.is_err());
}

#[test]
fn parse_fd_select_unknown_format_errors() {
    let bytes = alloc::vec![1u8, 0, 0]; // format 1 not supported by FDSelect
    let r = parse_fd_select(&bytes, 0, 1);
    assert!(r.is_err());
}

#[test]
fn encoding_format0_caps_at_u8_boundary() {
    // Inputs longer than 255 entries can't be represented (nCodes
    // is a u8). The emitter must clamp the data run to match the
    // count it advertises, otherwise downstream parsers treat the
    // overflow bytes as the next CFF section.
    let codes: Vec<u8> = (0..300u32).map(|c| c as u8).collect();
    let bytes = emit_encoding_format0(&codes);
    let count = bytes[1];
    let data_len = bytes.len() - 2;
    assert_eq!(count, 255);
    assert_eq!(data_len, 255);
}

#[test]
fn encoding_format1_caps_at_u8_range_boundary() {
    // 600 alternating codes form 600 single-entry ranges. Format 1's
    // nRanges is a u8 so at most 255 ranges can be encoded; the
    // emitter must stop appending before the count overflows.
    let codes: Vec<u8> = (0..600u32)
        .map(|c| if c.is_multiple_of(2) { 1 } else { 100 })
        .collect();
    let bytes = emit_encoding_format1(&codes);
    let n_ranges = bytes[1];
    let body_bytes = bytes.len() - 2;
    assert_eq!(n_ranges, 255);
    assert_eq!(body_bytes, 255 * 2);
}

#[test]
fn renumber_charstring_rewrites_callsubr() {
    // Charstring: push 0 (operand=0, raw=0+139=139), callsubr,
    // endchar. With local_count=0 -> bias=107 -> resolves to subr
    // index 107. Renumber map: subr 107 -> new index 5. New
    // local_count = 0 -> bias = 107 -> new_raw = 5 - 107 = -102.
    // -102 fits a single byte (-107..=107) so the renumber-at-width
    // helper will repad to a 1-byte form (which equals the
    // original).
    let mut cs = alloc::vec![139u8, OP_CALLSUBR, OP_ENDCHAR];
    // Build the renumber table: index 107 -> Some(5).
    let mut local_renumber = alloc::vec![None::<u32>; 200];
    local_renumber[107] = Some(5);
    let global_renumber: Vec<Option<u32>> = Vec::new();
    renumber_charstring(&mut cs, 0, 0, 0, 0, &local_renumber, &global_renumber).unwrap();
    // The new operand should encode -102: single byte = -102 + 139 = 37.
    assert_eq!(cs[0], 37);
    assert_eq!(cs[1], OP_CALLSUBR);
}

#[test]
fn renumber_charstring_pads_to_original_width() {
    // Original push uses shortint (3 bytes). New value would
    // naturally fit a single byte. Renumber must pad to 3 bytes
    // (still shortint) so the byte span is preserved.
    // Charstring: shortint 1000, callsubr, endchar.
    // bias=107, index_after_bias = 1000 + 107 = 1107.
    let mut cs = alloc::vec![OP_SHORTINT];
    cs.extend_from_slice(&1000i16.to_be_bytes());
    cs.push(OP_CALLSUBR);
    cs.push(OP_ENDCHAR);
    let mut local_renumber = alloc::vec![None::<u32>; 2000];
    // Map subr 1107 -> new index 5. New bias 107 -> new raw = -102.
    local_renumber[1107] = Some(5);
    let global_renumber: Vec<Option<u32>> = Vec::new();
    renumber_charstring(&mut cs, 0, 0, 0, 0, &local_renumber, &global_renumber).unwrap();
    // Should still be 3-byte shortint encoding.
    assert_eq!(cs[0], OP_SHORTINT);
    let new_val = i16::from_be_bytes([cs[1], cs[2]]);
    assert_eq!(i32::from(new_val), -102);
    assert_eq!(cs[3], OP_CALLSUBR);
}

#[test]
fn renumber_charstring_pads_two_byte_natural_to_three_byte_slot() {
    // #167 padding case: original operand encoded at 3-byte
    // shortint width, new natural-width operand falls into the
    // 2-byte form. Renumber must repad the 2-byte natural back to
    // a 3-byte shortint so the byte span stays stable.
    // Charstring: shortint 1000, callsubr, endchar.
    // bias = 107; index_after_bias = 1000 + 107 = 1107.
    let mut cs = alloc::vec![OP_SHORTINT];
    cs.extend_from_slice(&1000i16.to_be_bytes());
    cs.push(OP_CALLSUBR);
    cs.push(OP_ENDCHAR);
    let mut local_renumber = alloc::vec![None::<u32>; 2000];
    // Map subr 1107 -> new index 307. New bias 107 -> new raw = 200,
    // whose natural minimum width is 2 bytes (108..=1131). Renumber
    // must pad to the original 3-byte shortint width.
    local_renumber[1107] = Some(307);
    let global_renumber: Vec<Option<u32>> = Vec::new();
    renumber_charstring(&mut cs, 0, 0, 0, 0, &local_renumber, &global_renumber).unwrap();
    // Still 3-byte shortint, now carrying 200.
    assert_eq!(cs[0], OP_SHORTINT);
    let new_val = i16::from_be_bytes([cs[1], cs[2]]);
    assert_eq!(i32::from(new_val), 200);
    assert_eq!(cs[3], OP_CALLSUBR);
}

#[test]
fn encode_int_operand_at_width_two_byte_natural_in_range() {
    // 200 fits both natural-width 2 (247-250 form) and target=3
    // (shortint). When target=2 we should produce the 2-byte form
    // unchanged.
    let bytes = encode_int_operand_at_width(200, 2).unwrap();
    assert_eq!(bytes.len(), 2);
    // First byte = 247 + (200-108) >> 8 = 247 + 0 = 247.
    assert_eq!(bytes[0], 247);
    assert_eq!(bytes[1], (200 - 108) as u8);

    // -200 maps to the 251-254 form (negative 2-byte).
    let bytes = encode_int_operand_at_width(-200, 2).unwrap();
    assert_eq!(bytes.len(), 2);
    assert_eq!(bytes[0], 251);
    assert_eq!(bytes[1], (200 - 108) as u8);
}

#[test]
fn encode_int_operand_at_width_two_byte_unfixable() {
    // No 2-byte representation exists for -107..=107.
    let r = encode_int_operand_at_width(50, 2);
    assert!(r.is_err());
}

#[test]
fn encode_int_operand_at_width_one_byte_natural_no_change() {
    // 50 fits 1-byte natural form; target=1 returns the 1-byte
    // encoding unchanged.
    let bytes = encode_int_operand_at_width(50, 1).unwrap();
    assert_eq!(bytes, alloc::vec![(50 + 139) as u8]);
    // 5 also fits: same path.
    let bytes = encode_int_operand_at_width(5, 1).unwrap();
    assert_eq!(bytes, alloc::vec![(5 + 139) as u8]);
}

#[test]
fn encode_int_operand_at_width_five_byte_in_range_round_trips() {
    // Op 255 (5-byte fixed) is 16.16: i16 integer part + u16
    // fractional. Values that fit i16 round-trip cleanly: the high
    // 16 bits are the integer, the low 16 are zero (we never
    // re-emit a fractional component for a renumbered subr index).
    let bytes = encode_int_operand_at_width(12_345, 5).unwrap();
    assert_eq!(bytes.len(), 5);
    assert_eq!(bytes[0], 255);
    let raw = i32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
    assert_eq!(raw >> 16, 12_345);
    assert_eq!(raw & 0xFFFF, 0);

    let bytes = encode_int_operand_at_width(-12_345, 5).unwrap();
    let raw = i32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
    assert_eq!(raw >> 16, -12_345);
    assert_eq!(raw & 0xFFFF, 0);
}

#[test]
fn encode_int_operand_at_width_refuses_i16_overflow() {
    // Regression for #187: values outside i16 used to silently
    // wrap on `(v as i64) << 16 as i32` in the 5-byte fixed
    // branch, producing a corrupt subroutine index. Op 255 is
    // 16.16 (i16 integer + u16 fractional) so anything past i16
    // has no Type 2 charstring representation; refusing surfaces
    // the gap to `subset_non_identity`'s verbatim-fallback path.
    for &target in &[1usize, 2, 3, 5] {
        assert!(encode_int_operand_at_width(100_000, target).is_err());
        assert!(encode_int_operand_at_width(-100_000, target).is_err());
        assert!(encode_int_operand_at_width(i32::MAX, target).is_err());
        assert!(encode_int_operand_at_width(i32::MIN, target).is_err());
    }
    // Boundary values fit by construction at the 5-byte width.
    assert!(encode_int_operand_at_width(i32::from(i16::MAX), 5).is_ok());
    assert!(encode_int_operand_at_width(i32::from(i16::MIN), 5).is_ok());
}

#[test]
fn renumber_charstring_errors_on_dropped_subr() {
    // Charstring calls subr that's marked dropped: must error.
    let mut cs = alloc::vec![139u8, OP_CALLSUBR];
    let local_renumber = alloc::vec![None::<u32>; 200];
    let global_renumber: Vec<Option<u32>> = Vec::new();
    let r = renumber_charstring(&mut cs, 0, 0, 0, 0, &local_renumber, &global_renumber);
    assert!(r.is_err());
}

#[test]
fn encode_index_round_trips_through_simple_parser() {
    // Build an INDEX, then walk its bytes per spec.
    let entries: alloc::vec::Vec<&[u8]> = alloc::vec![&b"abc"[..], &b"defgh"[..], &b"i"[..]];
    let bytes = encode_index(&entries);
    let count = u16::from_be_bytes([bytes[0], bytes[1]]);
    assert_eq!(count, 3);
    let off_size = bytes[2] as usize;
    // Compute decoded offsets.
    let mut offsets = Vec::with_capacity(4);
    for i in 0..=count as usize {
        let start = 3 + i * off_size;
        let mut v = 0u32;
        for &b in &bytes[start..start + off_size] {
            v = (v << 8) | u32::from(b);
        }
        offsets.push(v);
    }
    let data_start = 3 + (count as usize + 1) * off_size;
    // Reconstruct entries.
    for i in 0..count as usize {
        let s = data_start + offsets[i] as usize - 1;
        let e = data_start + offsets[i + 1] as usize - 1;
        assert_eq!(&bytes[s..e], entries[i]);
    }
}
