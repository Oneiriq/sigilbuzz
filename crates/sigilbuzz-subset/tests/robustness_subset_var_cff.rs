//! Hostile-input regressions for the CFF / CFF2 subsetter and its public
//! byte-level helpers. Each input is hand-built and used to panic, hang,
//! or allocate without bound.

use sigilbuzz::Face;
use sigilbuzz_subset::{
    compute_kept_subrs, encode_int_operand, parse_fd_select, patch_dict_offset, renumber_subr_call,
    subr_bias, subset, subset_cff1_non_identity, subset_cff2_non_identity, SubrCall, SubrKind,
    SubsetError, SubsetInput,
};

/// CFF2 CharStrings INDEX with two entries whose middle offset (200)
/// points past the final offset (2). Only one data byte exists.
const CFF2_INDEX_WITH_OFFSET_PAST_END: &[u8] = &[0, 0, 0, 2, 1, 1, 200, 2, 0xAA];

/// Encodes a 5-byte DICT integer operand.
fn dict_int(v: i32) -> Vec<u8> {
    let mut out = vec![29];
    out.extend_from_slice(&v.to_be_bytes());
    out
}

/// CFF2 table whose Top DICT points CharStrings at `charstrings`.
fn cff2_with_charstrings_index(charstrings: &[u8]) -> Vec<u8> {
    // Top DICT: <offset> 17 (CharStrings). Header is 5 bytes, the Top
    // DICT is 6, and an empty Global Subr INDEX (4 bytes) follows.
    let cs_off = 5 + 6 + 4;
    let mut top = dict_int(cs_off);
    top.push(17);
    let mut out = vec![2, 0, 5];
    out.extend_from_slice(&(top.len() as u16).to_be_bytes());
    out.extend_from_slice(&top);
    out.extend_from_slice(&[0, 0, 0, 0]); // empty Global Subr INDEX
    out.extend_from_slice(charstrings);
    out
}

/// CFF1 table whose Top DICT points CharStrings at `charstrings`.
fn cff1_with_charstrings_index(charstrings: &[u8]) -> Vec<u8> {
    let name_index: &[u8] = &[0, 1, 1, 1, 2, b'X'];
    // Top DICT INDEX with one 6-byte entry: <offset> 17.
    let top_index_len = 2 + 1 + 2 + 6;
    let string_index: &[u8] = &[0, 0];
    let global_subr_index: &[u8] = &[0, 0];
    let cs_off = 4 + name_index.len() + top_index_len + string_index.len() + 2;
    let mut top = dict_int(cs_off as i32);
    top.push(17);
    let mut out = vec![1, 0, 4, 1];
    out.extend_from_slice(name_index);
    out.extend_from_slice(&[0, 1, 1, 1, 1 + top.len() as u8]);
    out.extend_from_slice(&top);
    out.extend_from_slice(string_index);
    out.extend_from_slice(global_subr_index);
    out.extend_from_slice(charstrings);
    out
}

#[test]
fn cff2_index_offset_past_final_offset_is_rejected() {
    let cff2 = cff2_with_charstrings_index(CFF2_INDEX_WITH_OFFSET_PAST_END);
    let r = subset_cff2_non_identity(&cff2, &[0]);
    assert!(matches!(r, Err(SubsetError::Unsupported(_))), "{r:?}");
}

#[test]
fn cff1_index_offset_past_final_offset_is_rejected() {
    let index: &[u8] = &[0, 2, 1, 1, 200, 2, 0xAA];
    let cff = cff1_with_charstrings_index(index);
    let r = subset_cff1_non_identity(&cff, &[0]);
    assert!(matches!(r, Err(SubsetError::Unsupported(_))), "{r:?}");
}

#[test]
fn subset_of_font_with_bad_cff2_index_is_an_error() {
    // The fuzzer reached the INDEX reader through `subset` on a font
    // whose only outline table was such a CFF2.
    let cff2 = cff2_with_charstrings_index(CFF2_INDEX_WITH_OFFSET_PAST_END);
    let mut maxp = Vec::new();
    maxp.extend_from_slice(&0x0000_5000u32.to_be_bytes());
    maxp.extend_from_slice(&2u16.to_be_bytes()); // numGlyphs
    let mut font = Vec::new();
    let tables: [([u8; 4], &[u8]); 2] = [(*b"CFF2", &cff2), (*b"maxp", &maxp)];
    font.extend_from_slice(b"OTTO");
    font.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    font.extend_from_slice(&[0, 32, 0, 1, 0, 0]); // searchRange etc.
    let mut offset = 12 + 16 * tables.len();
    for (tag, bytes) in &tables {
        font.extend_from_slice(tag);
        font.extend_from_slice(&0u32.to_be_bytes()); // checksum
        font.extend_from_slice(&(offset as u32).to_be_bytes());
        font.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        offset += bytes.len().next_multiple_of(4);
    }
    for (_, bytes) in &tables {
        font.extend_from_slice(bytes);
        font.resize(font.len().next_multiple_of(4), 0);
    }
    let face = Face::parse_bytes(&font, 0).expect("directory parses");
    let r = subset(
        &face,
        &SubsetInput {
            gids: vec![0],
            ..Default::default()
        },
    );
    assert!(r.is_err(), "{r:?}");
}

#[test]
fn compute_kept_subrs_handles_long_descending_chain() {
    // Local subr i calls local subr i - 1, and the charstring calls the
    // last one. A fixed-point pass over all subrs discovers one new
    // subr per pass, which is quadratic in the chain length.
    const N: usize = 20_000;
    let bias = subr_bias(N);
    let mut subrs: Vec<Vec<u8>> = vec![vec![11]]; // subr 0: return
    for i in 1..N {
        let mut body = encode_int_operand(i as i32 - 1 - bias);
        body.extend_from_slice(&[10, 11]); // callsubr, return
        subrs.push(body);
    }
    let mut charstring = encode_int_operand(N as i32 - 1 - bias);
    charstring.extend_from_slice(&[10, 14]); // callsubr, endchar
    let local: Vec<&[u8]> = subrs.iter().map(Vec::as_slice).collect();
    let (kept_local, kept_global) =
        compute_kept_subrs(&[&charstring], &local, &[]).expect("closure");
    assert_eq!(kept_local.len(), N);
    assert!(kept_global.is_empty());
}

#[test]
fn patch_dict_offset_ignores_slot_past_buffer() {
    // The placeholder byte is present but its 4 operand bytes are not.
    let mut buf = [29u8, 0, 0];
    patch_dict_offset(&mut buf, 0, 1234);
    assert_eq!(buf, [29, 0, 0]);
}

#[test]
fn renumber_subr_call_rejects_span_that_overflows() {
    let mut charstring = [139u8, 10];
    let call = SubrCall {
        kind: SubrKind::Local,
        index_after_bias: 107,
        raw_operand: 0,
        operand_byte_offset: usize::MAX,
        operand_byte_len: 2,
    };
    let r = renumber_subr_call(&mut charstring, &call, 0);
    assert!(matches!(r, Err(SubsetError::Unsupported(_))), "{r:?}");
}

#[test]
fn parse_fd_select_rejects_glyph_count_that_overflows() {
    let data = [0u8, 0, 0];
    let r = parse_fd_select(&data, 0, usize::MAX);
    assert!(matches!(r, Err(SubsetError::Unsupported(_))), "{r:?}");
}
