//! Unit tests for the seac closure.

use super::*;
use crate::cff::{emit_charset_format0, encode_dict_offset_placeholder, encode_index};
use crate::cff::{encode_int_operand, patch_dict_offset};
use alloc::vec;

/// A name-keyed CFF1 table: `.notdef`, then one glyph per charstring
/// of `glyphs` with its SID, `global` and `local` subroutines, and the
/// charset written out (format 0), or the predefined ISOAdobe charset
/// when `iso_adobe` is set.
fn cff1(glyphs: &[(u16, Vec<u8>)], global: &[&[u8]], local: &[&[u8]], iso_adobe: bool) -> Vec<u8> {
    let mut charstrings: Vec<&[u8]> = vec![&[14]];
    charstrings.extend(glyphs.iter().map(|(_, cs)| cs.as_slice()));
    let sids: Vec<u16> = glyphs.iter().map(|&(sid, _)| sid).collect();

    let mut top: Vec<u8> = Vec::new();
    let charset_slot = (!iso_adobe).then(|| {
        let at = top.len();
        top.extend_from_slice(&encode_dict_offset_placeholder());
        top.push(15);
        at
    });
    let charstrings_slot = top.len();
    top.extend_from_slice(&encode_dict_offset_placeholder());
    top.push(17);
    let private_size_slot = top.len();
    top.extend_from_slice(&encode_dict_offset_placeholder());
    let private_off_slot = top.len();
    top.extend_from_slice(&encode_dict_offset_placeholder());
    top.push(18);
    // Private DICT: just the Subrs offset, relative to the DICT.
    let mut private = encode_dict_offset_placeholder();
    private.push(19);

    let mut out = vec![1u8, 0, 4, 1];
    out.extend_from_slice(&encode_index(&[b"Test"]));
    let top_index = encode_index(&[&top[..]]);
    // Count, offSize, two offsets of one byte, then the DICT.
    let top_at = out.len() + 5;
    out.extend_from_slice(&top_index);
    out.extend_from_slice(&encode_index(&[])); // String INDEX
    out.extend_from_slice(&encode_index(global));
    let charset_at = out.len();
    if !iso_adobe {
        out.extend_from_slice(&emit_charset_format0(&sids));
    }
    let charstrings_at = out.len();
    out.extend_from_slice(&encode_index(&charstrings));
    let private_at = out.len();
    out.extend_from_slice(&private);
    let local_at = out.len();
    out.extend_from_slice(&encode_index(local));

    if let Some(slot) = charset_slot {
        patch_dict_offset(&mut out, top_at + slot, charset_at as i32);
    }
    patch_dict_offset(&mut out, top_at + charstrings_slot, charstrings_at as i32);
    patch_dict_offset(&mut out, top_at + private_size_slot, private.len() as i32);
    patch_dict_offset(&mut out, top_at + private_off_slot, private_at as i32);
    patch_dict_offset(&mut out, private_at, (local_at - private_at) as i32);
    out
}

/// A charstring pushing `operands` and ending in `op`.
fn cs(operands: &[i32], op: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = operands
        .iter()
        .flat_map(|&v| encode_int_operand(v))
        .collect();
    out.extend_from_slice(op);
    out
}

/// A plain glyph: a move, a line, and `endchar`.
fn plain() -> Vec<u8> {
    let mut out = cs(&[10, 10], &[21]);
    out.extend(cs(&[100, 0], &[5]));
    out.push(14);
    out
}

// Standard Encoding codes and SIDs: `A` is code 65, SID 34; `acute` is
// code 194, SID 125; `grave` is code 193, SID 124.
const A: (i32, u16) = (65, 34);
const ACUTE: (i32, u16) = (194, 125);
const GRAVE: (i32, u16) = (193, 124);

/// The glyphs the seac closure of `cff` keeps from `kept`.
fn closure(cff: &[u8], kept: &[usize], n: usize) -> Vec<usize> {
    let mut keep = vec![false; n];
    for &g in kept {
        keep[g] = true;
    }
    if let Some(mut seac) = SeacClosure::new(cff) {
        seac.expand(&mut keep);
        // A second pass runs nothing again and adds nothing.
        assert!(!seac.expand(&mut keep));
    }
    (0..n).filter(|&g| keep[g]).collect()
}

#[test]
fn a_seac_glyph_keeps_its_base_and_accent() {
    // gid 1 A, gid 2 grave, gid 3 acute, gid 4 Aacute (SID 200, an
    // arbitrary standard name) drawn by seac.
    let seac = cs(&[0, 180, A.0, ACUTE.0], &[14]);
    let font = cff1(
        &[
            (A.1, plain()),
            (GRAVE.1, plain()),
            (ACUTE.1, plain()),
            (200, seac),
        ],
        &[],
        &[],
        false,
    );
    assert_eq!(closure(&font, &[0, 4], 5), vec![0, 1, 3, 4]);
    // A glyph that is not a seac keeps nothing more.
    assert_eq!(closure(&font, &[0, 2], 5), vec![0, 2]);
}

#[test]
fn a_width_below_the_seac_operands_is_skipped() {
    let seac = cs(&[480, -5, 180, A.0, GRAVE.0], &[14]);
    let font = cff1(
        &[(A.1, plain()), (GRAVE.1, plain()), (300, seac)],
        &[],
        &[],
        false,
    );
    assert_eq!(closure(&font, &[3], 4), vec![1, 2, 3]);
}

#[test]
fn a_seac_inside_a_subroutine_after_hints_is_found() {
    // The charstring declares two stems, uses a hint mask (one data
    // byte), and calls local subroutine 0, which calls global
    // subroutine 0, which ends the glyph with the seac.
    let global_seac = cs(&[0, 100, A.0, ACUTE.0], &[14]);
    let mut local = cs(&[-107], &[29]); // callgsubr 0
    local.push(11);
    let mut glyph = cs(&[10, 20, 30, 40], &[1]); // hstem
    glyph.extend([19, 0b1100_0000]); // hintmask
    glyph.extend(cs(&[-107], &[10])); // callsubr 0
    let font = cff1(
        &[(A.1, plain()), (ACUTE.1, plain()), (201, glyph)],
        &[&global_seac],
        &[&local],
        false,
    );
    assert_eq!(closure(&font, &[3], 4), vec![1, 2, 3]);
}

#[test]
fn the_iso_adobe_charset_maps_sids_to_their_own_glyphs() {
    // With the predefined charset, glyph i has SID i: SID 34 (A) is
    // glyph 34 and SID 124 (grave) glyph 124.
    let mut glyphs: Vec<(u16, Vec<u8>)> = (1..=150).map(|sid| (sid, plain())).collect();
    glyphs[149] = (150, cs(&[0, 0, A.0, GRAVE.0], &[14]));
    let font = cff1(&glyphs, &[], &[], true);
    assert_eq!(closure(&font, &[150], 151), vec![34, 124, 150]);
}

#[test]
fn codes_without_a_glyph_or_outside_the_encoding_add_nothing() {
    // Code 300 is past the encoding, code 128 undefined in it, and
    // grave has no glyph in this font.
    let font = cff1(
        &[
            (A.1, plain()),
            (202, cs(&[0, 0, 300, 128], &[14])),
            (203, cs(&[0, 0, A.0, GRAVE.0], &[14])),
        ],
        &[],
        &[],
        false,
    );
    assert_eq!(closure(&font, &[2], 4), vec![2]);
    assert_eq!(closure(&font, &[3], 4), vec![1, 3]);
}

#[test]
fn charstrings_that_cannot_run_add_nothing() {
    for broken in [
        cs(&[0, 0, A.0], &[10]),         // callsubr past the end
        vec![28, 0],                     // truncated shortint
        cs(&[0, 0, A.0, ACUTE.0], &[2]), // reserved operator
    ] {
        let font = cff1(
            &[(A.1, plain()), (ACUTE.1, plain()), (204, broken)],
            &[],
            &[],
            false,
        );
        assert_eq!(closure(&font, &[3], 4), vec![3]);
    }
    // Subroutines that call themselves stop at the nesting limit.
    let mut recursive = cs(&[-107], &[10]);
    recursive.push(11);
    let font = cff1(
        &[(A.1, plain()), (205, cs(&[-107], &[10]))],
        &[],
        &[&recursive],
        false,
    );
    assert_eq!(closure(&font, &[2], 3), vec![2]);
    // A table that does not parse has no seac closure.
    assert!(SeacClosure::new(&[1, 0, 4]).is_none());
}

#[test]
fn standard_sids_follow_the_encoding() {
    assert_eq!(standard_sid(32.0), Some(1));
    assert_eq!(standard_sid(65.9), Some(34), "a fraction truncates");
    assert_eq!(standard_sid(127.0), None);
    assert_eq!(standard_sid(251.0), Some(149));
    assert_eq!(standard_sid(-1.0), None);
    assert_eq!(standard_sid(256.0), None);
}
