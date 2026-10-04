//! Tests for the charstring walk, on hand-built charstrings and stores.

use super::*;
use crate::cff::charstring::encode_int_operand;

const BIG: usize = 1 << 20;

/// Push of integer `v`.
fn int(v: i32) -> Vec<u8> {
    encode_int_operand(v)
}

/// Concatenates pushes and opcodes.
fn cs(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

/// The push of the biased number that calls subroutine `index` of a
/// list of `count` subroutines.
fn subr_number(index: i32, count: usize) -> Vec<u8> {
    int(index - subr_bias(count))
}

/// An ItemVariationStore with one subtable per entry of `regions`, the
/// subtable naming that many regions, over a one-axis region list.
fn store(regions: &[u16]) -> Vec<u8> {
    let region_count = regions.iter().copied().max().unwrap_or(0);
    let subtables = regions.len();
    // Header: format, region list offset, subtable count, offsets.
    let header_len = 2 + 4 + 2 + 4 * subtables;
    let region_list_len = 4 + 6 * usize::from(region_count);
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(header_len as u32).to_be_bytes());
    out.extend_from_slice(&(subtables as u16).to_be_bytes());
    let mut sub_off = header_len + region_list_len;
    for &r in regions {
        out.extend_from_slice(&(sub_off as u32).to_be_bytes());
        sub_off += 6 + 2 * usize::from(r);
    }
    out.extend_from_slice(&1u16.to_be_bytes()); // axis count
    out.extend_from_slice(&region_count.to_be_bytes());
    for _ in 0..region_count {
        for v in [0i16, 0x4000, 0x4000] {
            out.extend_from_slice(&v.to_be_bytes());
        }
    }
    for &r in regions {
        out.extend_from_slice(&0u16.to_be_bytes()); // item count
        out.extend_from_slice(&0u16.to_be_bytes()); // word delta count
        out.extend_from_slice(&r.to_be_bytes());
        for i in 0..r {
            out.extend_from_slice(&i.to_be_bytes());
        }
    }
    out
}

/// What one glyph's walk found: its own calls, and the local and global
/// subroutines it reached.
type Walked = (Vec<SubrCall>, Vec<u32>, Vec<u32>);

/// Runs `charstring` as the only glyph of a CFF1 table.
fn walk_cff1<'a>(
    charstring: &[u8],
    locals: &'a [&'a [u8]],
    globals: &'a [&'a [u8]],
) -> Result<Walked, SubsetError> {
    let mut walk = CharstringWalk::new(globals, None, BIG);
    let mut fd = walk.fd(locals, 0);
    let calls = walk.glyph(&mut fd, charstring)?;
    Ok((calls, fd.kept_locals(), walk.kept_globals()))
}

#[test]
fn blended_stems_size_the_mask() {
    // `10 20 1 2 3 4 2 blend hstemhm`: two regions, so `blend` leaves
    // 10 and 20, one stem, and the mask takes one byte. A mask byte of
    // 9 is a reserved opcode: reading the mask as empty fails there.
    let ivs = store(&[2]);
    let locals_body = [OP_RETURN];
    let locals: [&[u8]; 1] = [&locals_body];
    let charstring = cs(&[
        &int(10),
        &int(20),
        &int(1),
        &int(2),
        &int(3),
        &int(4),
        &int(2),
        &[OP_BLEND, OP_HSTEMHM, OP_HINTMASK, 9],
        &subr_number(0, 1),
        &[OP_CALLSUBR],
    ]);
    let mut walk = CharstringWalk::new(&[], Some(BlendRegions::new(Some(&ivs))), BIG);
    let mut fd = walk.fd(&locals, 0);
    let calls = walk.glyph(&mut fd, &charstring).expect("walk");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].kind, SubrKind::Local);
    assert_eq!(calls[0].index_after_bias, 0);
    assert_eq!(calls[0].operand_byte_offset, charstring.len() - 2);
    assert_eq!(fd.kept_locals(), [0]);
    // The per-body scanner clears the stack at `blend` and misreads.
    assert!(super::super::scan_subr_calls(&charstring, 1, 0).is_err());
}

#[test]
fn private_dict_vsindex_picks_the_region_count() {
    // Seven stems, then `10 1 2 3 1 blend 20 hstem`. Subtable 1 has
    // three regions: `blend` leaves 10, one more stem, eight in all,
    // and the mask takes one byte before the call. Subtable 0's one
    // region would leave 10 1 2: nine stems, a two-byte mask, and the
    // call's number read as mask.
    let ivs = store(&[1, 3]);
    let local_body = [OP_RETURN];
    let locals: [&[u8]; 1] = [&local_body];
    let stems: Vec<u8> = (0..14).flat_map(int).collect();
    let charstring = cs(&[
        &stems,
        &[OP_HSTEMHM],
        &[10, 1, 2, 3, 1].map(int).concat(),
        &[OP_BLEND],
        &int(20),
        &[OP_HSTEM, OP_HINTMASK, 9],
        &subr_number(0, 1),
        &[OP_CALLSUBR],
    ]);
    let mut walk = CharstringWalk::new(&[], Some(BlendRegions::new(Some(&ivs))), BIG);
    let mut fd = walk.fd(&locals, 1);
    let calls = walk.glyph(&mut fd, &charstring).expect("walk");
    assert_eq!(calls.len(), 1);
    let mut fd = walk.fd(&locals, 0);
    assert!(walk.glyph(&mut fd, &charstring).is_err());
}

#[test]
fn vsindex_past_the_store_blends_no_deltas() {
    // `7 vsindex` names no subtable: `blend` takes only its count and
    // leaves both defaults, one stem.
    let ivs = store(&[4]);
    let charstring = cs(&[
        &int(7),
        &[OP_VSINDEX],
        &int(10),
        &int(20),
        &int(2),
        &[OP_BLEND, OP_HSTEMHM, OP_HINTMASK, 9],
    ]);
    let mut walk = CharstringWalk::new(&[], Some(BlendRegions::new(Some(&ivs))), BIG);
    let mut fd = walk.fd(&[], 0);
    walk.glyph(&mut fd, &charstring).expect("walk");
    // A negative vsindex saturates to 0, which has four regions.
    let negative = cs(&[
        &int(-3),
        &[OP_VSINDEX],
        &int(10),
        &[1, 2, 3, 4].map(int).concat(),
        &int(1),
        &[OP_BLEND],
        &int(20),
        &[OP_HSTEMHM, OP_HINTMASK, 9],
    ]);
    walk.glyph(&mut fd, &negative).expect("walk");
}

#[test]
fn a_missing_store_blends_no_deltas() {
    let charstring = cs(&[
        &int(10),
        &int(20),
        &int(2),
        &[OP_BLEND, OP_HSTEMHM, OP_HINTMASK, 9],
    ]);
    for bytes in [None, Some(&[0u8, 9][..])] {
        let mut walk = CharstringWalk::new(&[], Some(BlendRegions::new(bytes)), BIG);
        let mut fd = walk.fd(&[], 0);
        walk.glyph(&mut fd, &charstring).expect("walk");
    }
}

#[test]
fn stems_of_the_caller_size_a_mask_in_a_subroutine() {
    // The charstring declares nine stems, then calls local 0, whose
    // mask therefore takes two bytes before its call to local 1.
    let stems: Vec<u8> = (0..18).flat_map(int).collect();
    let local_1 = [OP_RETURN];
    let local_0 = cs(&[
        &[OP_HINTMASK, 9, 9],
        &subr_number(1, 2),
        &[OP_CALLSUBR, OP_RETURN],
    ]);
    let locals: [&[u8]; 2] = [&local_0, &local_1];
    let charstring = cs(&[
        &stems,
        &[OP_HSTEMHM],
        &subr_number(0, 2),
        &[OP_CALLSUBR, OP_ENDCHAR],
    ]);
    let mut walk = CharstringWalk::new(&[], None, BIG);
    let mut fd = walk.fd(&locals, 0);
    let calls = walk.glyph(&mut fd, &charstring).expect("walk");
    assert_eq!(calls.len(), 1);
    assert_eq!(fd.kept_locals(), [0, 1]);
    let inner = fd.local_calls(0).expect("local 0 read");
    assert_eq!(inner.len(), 1);
    assert_eq!(inner[0].operand_byte_offset, 3);
    assert_eq!(inner[0].index_after_bias, 1);
    // Read on its own, local 0's mask is empty and its bytes misread.
    assert!(super::super::scan_subr_calls(&local_0, 2, 0).is_err());
}

#[test]
fn operands_the_caller_left_count_as_an_implicit_vstem() {
    // 8 stems, then the caller leaves two operands: the mask in the
    // subroutine counts nine stems and takes two bytes.
    let stems: Vec<u8> = (0..16).flat_map(int).collect();
    let local_0 = [OP_HINTMASK, 9, 9, OP_RETURN];
    let locals: [&[u8]; 1] = [&local_0];
    let charstring = cs(&[
        &stems,
        &[OP_HSTEMHM],
        &int(5),
        &int(6),
        &subr_number(0, 1),
        &[OP_CALLSUBR, OP_ENDCHAR],
    ]);
    walk_cff1(&charstring, &locals, &[]).expect("walk");
}

#[test]
fn stems_a_subroutine_declares_size_the_callers_masks() {
    let stems: Vec<u8> = (0..18).flat_map(int).collect();
    let local_0 = cs(&[&stems, &[OP_HSTEMHM, OP_RETURN]]);
    let locals: [&[u8]; 1] = [&local_0];
    let charstring = cs(&[
        &subr_number(0, 1),
        &[OP_CALLSUBR, OP_HINTMASK, 9, 9, OP_ENDCHAR],
    ]);
    walk_cff1(&charstring, &locals, &[]).expect("walk");
}

#[test]
fn the_mask_size_is_fixed_at_the_first_mask() {
    // Eight stems give one-byte masks; a stem declared after the first
    // mask does not grow the next one, as HarfBuzz reads it. A
    // two-byte mask would swallow the call's number.
    let local_body = [OP_RETURN];
    let locals: [&[u8]; 1] = [&local_body];
    let stems: Vec<u8> = (0..16).flat_map(int).collect();
    let charstring = cs(&[
        &stems,
        &[OP_HSTEMHM, OP_HINTMASK, 9],
        &int(1),
        &int(2),
        &[OP_HSTEM, OP_HINTMASK, 9],
        &subr_number(0, 1),
        &[OP_CALLSUBR, OP_ENDCHAR],
    ]);
    let (calls, _, _) = walk_cff1(&charstring, &locals, &[]).expect("walk");
    assert_eq!(calls.len(), 1);
}

#[test]
fn a_subroutine_is_read_once_and_kept_once() {
    let local_1 = [OP_RETURN];
    let local_0 = cs(&[&subr_number(1, 3), &[OP_CALLSUBR, OP_RETURN]]);
    let local_2 = [OP_RETURN];
    let locals: [&[u8]; 3] = [&local_0, &local_1, &local_2];
    let glyph = cs(&[&subr_number(0, 3), &[OP_CALLSUBR, OP_ENDCHAR]]);
    let mut walk = CharstringWalk::new(&[], None, BIG);
    let mut fd = walk.fd(&locals, 0);
    walk.glyph(&mut fd, &glyph).expect("first glyph");
    walk.glyph(&mut fd, &glyph).expect("second glyph");
    assert_eq!(fd.kept_locals(), [0, 1]);
    assert_eq!(fd.local_calls(0).map(<[_]>::len), Some(1));
    assert_eq!(fd.local_calls(2), None);
}

#[test]
fn endchar_in_a_subroutine_ends_the_glyph() {
    // Local 0 ends the glyph, so the call to local 1 after it is never
    // reached.
    let local_0 = [OP_ENDCHAR];
    let local_1 = [OP_RETURN];
    let locals: [&[u8]; 2] = [&local_0, &local_1];
    let charstring = cs(&[
        &subr_number(0, 2),
        &[OP_CALLSUBR],
        &subr_number(1, 2),
        &[OP_CALLSUBR],
    ]);
    let (calls, kept_locals, _) = walk_cff1(&charstring, &locals, &[]).expect("walk");
    assert_eq!(calls.len(), 1);
    assert_eq!(kept_locals, [0]);
}

#[test]
fn globals_are_kept_per_fd_reached() {
    let global_0 = [OP_RETURN];
    let global_1 = [OP_RETURN];
    let globals: [&[u8]; 2] = [&global_0, &global_1];
    let calls_0 = cs(&[&subr_number(0, 2), &[OP_CALLGSUBR, OP_ENDCHAR]]);
    let calls_1 = cs(&[&subr_number(1, 2), &[OP_CALLGSUBR, OP_ENDCHAR]]);
    let mut walk = CharstringWalk::new(&globals, None, BIG);
    let mut fd_a = walk.fd(&[], 0);
    walk.glyph(&mut fd_a, &calls_1).expect("walk");
    walk.glyph(&mut fd_a, &calls_1).expect("walk");
    let mut fd_b = walk.fd(&[], 0);
    walk.glyph(&mut fd_b, &calls_0).expect("walk");
    walk.glyph(&mut fd_b, &calls_1).expect("walk");
    assert_eq!(fd_a.reached_globals(), [1]);
    assert_eq!(fd_b.reached_globals(), [0, 1]);
    assert_eq!(walk.kept_globals(), [0, 1]);
}

/// A chain of `depth` local subroutines, each calling the next, and a
/// charstring calling the first.
fn chain(depth: usize) -> (Vec<Vec<u8>>, Vec<u8>) {
    let locals: Vec<Vec<u8>> = (0..depth)
        .map(|i| {
            if i + 1 < depth {
                cs(&[&subr_number(i as i32 + 1, depth), &[OP_CALLSUBR, OP_RETURN]])
            } else {
                alloc::vec![OP_RETURN]
            }
        })
        .collect();
    let charstring = cs(&[&subr_number(0, depth), &[OP_CALLSUBR, OP_ENDCHAR]]);
    (locals, charstring)
}

#[test]
fn subroutines_nest_ten_deep() {
    let (locals, charstring) = chain(10);
    let refs: Vec<&[u8]> = locals.iter().map(Vec::as_slice).collect();
    let (_, kept, _) = walk_cff1(&charstring, &refs, &[]).expect("ten deep");
    assert_eq!(kept.len(), 10);
    let (locals, charstring) = chain(11);
    let refs: Vec<&[u8]> = locals.iter().map(Vec::as_slice).collect();
    let r = walk_cff1(&charstring, &refs, &[]);
    assert_eq!(
        r.unwrap_err(),
        SubsetError::Unsupported("CFF subroutines nested more than 10 deep")
    );
}

#[test]
fn a_subroutine_reaching_itself_fails() {
    let local_0 = cs(&[&subr_number(0, 1), &[OP_CALLSUBR, OP_RETURN]]);
    let locals: [&[u8]; 1] = [&local_0];
    let charstring = cs(&[&subr_number(0, 1), &[OP_CALLSUBR, OP_ENDCHAR]]);
    let r = walk_cff1(&charstring, &locals, &[]);
    assert_eq!(
        r.unwrap_err(),
        SubsetError::Unsupported("CFF subroutine reaches itself")
    );
}

#[test]
fn the_stack_holds_513_operands() {
    let mut full = int(0).repeat(MAX_STACK);
    full.push(OP_ENDCHAR);
    walk_cff1(&full, &[], &[]).expect("513 operands");
    let mut over = int(0).repeat(MAX_STACK + 1);
    over.push(OP_ENDCHAR);
    assert!(walk_cff1(&over, &[], &[]).is_err());
}

#[test]
fn a_spent_budget_fails() {
    // Local 0 calls local 1 ten times, local 1 calls local 2 ten
    // times, and so on: a few hundred bytes run 10^9 tokens.
    let depth = 9;
    let locals: Vec<Vec<u8>> = (0..depth)
        .map(|i| {
            if i + 1 < depth {
                let call = cs(&[&subr_number(i as i32 + 1, depth), &[OP_CALLSUBR]]);
                let mut body = call.repeat(10);
                body.push(OP_RETURN);
                body
            } else {
                alloc::vec![OP_RETURN]
            }
        })
        .collect();
    let refs: Vec<&[u8]> = locals.iter().map(Vec::as_slice).collect();
    let charstring = cs(&[&subr_number(0, depth), &[OP_CALLSUBR, OP_ENDCHAR]]);
    let mut walk = CharstringWalk::new(&[], None, walk_budget(64));
    let mut fd = walk.fd(&refs, 0);
    let r = walk.glyph(&mut fd, &charstring);
    assert_eq!(r.unwrap_err(), WORK_EXCEEDED);
}

#[test]
fn malformed_charstrings_fail() {
    let ivs = store(&[2]);
    let local_body = cs(&[&[OP_CALLSUBR, OP_RETURN]]);
    let locals: [&[u8]; 1] = [&local_body];
    let cases: [(&str, Vec<u8>); 10] = [
        // `blend` short of its defaults and deltas.
        ("blend short", cs(&[&int(10), &int(1), &[OP_BLEND]])),
        (
            "blend count negative",
            cs(&[&int(10), &int(-1), &[OP_BLEND]]),
        ),
        ("blend without count", alloc::vec![OP_BLEND]),
        ("vsindex without operand", alloc::vec![OP_VSINDEX]),
        ("callsubr without operand", alloc::vec![OP_CALLSUBR]),
        // The subroutine number of local 0's call is pushed by the
        // charstring, not by local 0.
        (
            "number from another body",
            cs(&[&int(5), &subr_number(0, 1), &[OP_CALLSUBR]]),
        ),
        (
            "index past the end",
            cs(&[&subr_number(1, 1), &[OP_CALLSUBR]]),
        ),
        (
            "mask past the end",
            cs(&[&int(1), &int(2), &[OP_HSTEM, OP_HINTMASK]]),
        ),
        ("reserved opcode", alloc::vec![9]),
        ("escape truncated", alloc::vec![OP_ESCAPE]),
    ];
    for (what, charstring) in cases {
        let mut walk = CharstringWalk::new(&[], Some(BlendRegions::new(Some(&ivs))), BIG);
        let mut fd = walk.fd(&locals, 0);
        assert!(walk.glyph(&mut fd, &charstring).is_err(), "{what}");
    }
    // A blended value is no subroutine number.
    let blended = cs(&[
        &int(-107),
        &int(0),
        &int(0),
        &int(1),
        &[OP_BLEND, OP_CALLSUBR],
    ]);
    let mut walk = CharstringWalk::new(&[], Some(BlendRegions::new(Some(&ivs))), BIG);
    let mut fd = walk.fd(&locals, 0);
    assert!(walk.glyph(&mut fd, &blended).is_err());
}

#[test]
fn cff1_clears_the_stack_at_blend_and_vsindex() {
    // Opcodes 15 and 16 mean nothing in CFF1: they clear the stack, as
    // the per-body scanner reads them.
    let charstring = cs(&[
        &int(3),
        &[OP_VSINDEX],
        &int(1),
        &[OP_BLEND, OP_HINTMASK, OP_ENDCHAR],
    ]);
    walk_cff1(&charstring, &[], &[]).expect("walk");
}

#[test]
fn a_blend_charges_the_values_it_leaves() {
    // Without regions `512 blend` leaves its 512 values where they are,
    // so it could run again and again for two tokens. It charges the
    // 512 values: 512 pushes, the count, the operator and 512 more.
    let mut charstring = int(0).repeat(512);
    charstring.extend_from_slice(&int(512));
    charstring.push(OP_BLEND);
    for budget in [1026, 1025] {
        let mut walk = CharstringWalk::new(&[], Some(BlendRegions::new(None)), budget);
        let mut fd = walk.fd(&[], 0);
        let r = walk.glyph(&mut fd, &charstring);
        assert_eq!(r.is_ok(), budget == 1026, "budget {budget}");
    }
    // A long run of them spends a table-sized budget.
    let mut repeated = int(0).repeat(512);
    for _ in 0..10_000 {
        repeated.extend_from_slice(&int(512));
        repeated.push(OP_BLEND);
    }
    let mut walk = CharstringWalk::new(
        &[],
        Some(BlendRegions::new(None)),
        walk_budget(repeated.len()),
    );
    let mut fd = walk.fd(&[], 0);
    assert_eq!(walk.glyph(&mut fd, &repeated).unwrap_err(), WORK_EXCEEDED);
}
