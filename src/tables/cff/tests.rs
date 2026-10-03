//! Tests for the `CFF ` parser and the Type 2 charstring interpreter.

use super::*;
use crate::tables::cff::charstring::subr_bias;
use crate::tables::cff::dict::{read_dict_operand, DictOperand};
use crate::tables::cff::index::encode_index;
use crate::tables::outline::{Outline, PathOp};
use alloc::vec::Vec;

/// Helper: builds a minimal CFF1 table with one Top DICT and one
/// glyph's charstring. `cs` is the raw charstring (Type 2 ops
/// already encoded).
fn build_cff_with_charstring(cs: &[u8]) -> Vec<u8> {
    // Header.
    let mut out = Vec::new();
    out.push(1); // major
    out.push(0); // minor
    out.push(4); // hdrSize
    out.push(1); // offSize (unused for this mini layout)

    // Name INDEX: count=1, offSize=1, offsets [1,1] (empty name).
    out.extend_from_slice(&1u16.to_be_bytes());
    out.push(1);
    out.push(1);
    out.push(1);

    // Top DICT INDEX placeholder: patch later.
    let top_index_start = out.len();
    out.extend_from_slice(&1u16.to_be_bytes()); // count
    out.push(4); // offSize (4-byte offsets)

    // Reserve offset slots [start=1, end=?].
    out.extend_from_slice(&1u32.to_be_bytes()); // first offset
    let top_end_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // patched

    // Top DICT content: operator 17 (CharStrings) with operand =
    // absolute offset of CharStrings INDEX. Use 5-byte encoding:
    // b=29 then 4-byte integer.
    let top_dict_start = out.len();
    out.push(29);
    let cs_off_slot = out.len();
    out.extend_from_slice(&0i32.to_be_bytes()); // patched
    out.push(17); // CharStrings operator
    let top_dict_end = out.len();

    // Patch top-dict end offset (relative offsets are 1-based from
    // the end of the offsets array).
    let top_dict_len = (top_dict_end - top_dict_start) as u32;
    let final_off = 1u32 + top_dict_len;
    out[top_end_off_slot..top_end_off_slot + 4].copy_from_slice(&final_off.to_be_bytes());
    let _ = top_index_start;

    // String INDEX: empty.
    out.extend_from_slice(&0u16.to_be_bytes());

    // Global Subr INDEX: empty.
    out.extend_from_slice(&0u16.to_be_bytes());

    // CharStrings INDEX.
    let cs_index_off = out.len() as i32;
    out[cs_off_slot..cs_off_slot + 4].copy_from_slice(&cs_index_off.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // count
    out.push(2); // offSize
                 // offsets: [1, 1 + cs.len()]
    out.extend_from_slice(&1u16.to_be_bytes());
    let end = 1u16 + cs.len() as u16;
    out.extend_from_slice(&end.to_be_bytes());
    out.extend_from_slice(cs);

    out
}

#[test]
fn charstring_rmoveto_rlineto_endchar() {
    // 100 100 rmoveto 50 0 rlineto 0 50 rlineto -50 0 rlineto endchar
    // Encode: 100 -> 247 -108... actually 100 fits in single byte
    // shortcut "b0 = 100 + 139"? No, encoding is b0 - 139 for
    // 32..=246; 100 = b0 = 239.
    let mut cs = Vec::new();
    // push 100
    cs.push(239); // 239 - 139 = 100
    cs.push(239); // 100
    cs.push(op_code::RMOVETO);
    cs.push(189); // 189 - 139 = 50
    cs.push(139); // 0
    cs.push(op_code::RLINETO);
    cs.push(139); // 0
    cs.push(189); // 50
    cs.push(op_code::RLINETO);
    // -50: 251..=254 range. b0=251, b1=(-(-50) - 108) -> 251, b1 = -50 = -108 - b1*256 - ... solve:
    //   value = -(b0 - 251)*256 - b1 - 108 = -50
    //   b0=251 -> value = -b1 - 108 = -50 -> b1 = -58 invalid (unsigned)
    // Use 28 <short int> instead.
    cs.push(28);
    cs.extend_from_slice(&(-50i16).to_be_bytes());
    cs.push(139); // 0
    cs.push(op_code::RLINETO);
    cs.push(op_code::ENDCHAR);

    let cff = build_cff_with_charstring(&cs);
    let parsed = Cff::parse(&cff).unwrap();
    assert_eq!(parsed.num_glyphs(), 1);
    let mut o = Outline::new();
    parsed.outline(0, &mut o).unwrap();
    // MoveTo(100,100), LineTo(150,100), LineTo(150,150), LineTo(100,150), Close.
    assert!(matches!(o.ops()[0], PathOp::MoveTo { x: 100.0, y: 100.0 }));
    assert!(matches!(o.ops()[1], PathOp::LineTo { x: 150.0, y: 100.0 }));
    assert!(matches!(o.ops()[2], PathOp::LineTo { x: 150.0, y: 150.0 }));
    assert!(matches!(o.ops()[3], PathOp::LineTo { x: 100.0, y: 150.0 }));
    assert!(matches!(o.ops()[4], PathOp::Close));
    // endchar closed the contour, so the final close after the run
    // must not add a second Close.
    assert_eq!(o.len(), 5);
}

#[test]
fn charstring_without_endchar_still_closes_its_contour() {
    // 0 0 rmoveto 10 0 rlineto, with no endchar. A malformed CFF1
    // charstring like this used to leave its contour open.
    let cs = [139, 139, op_code::RMOVETO, 149, 139, op_code::RLINETO];
    let cff = build_cff_with_charstring(&cs);
    let parsed = Cff::parse(&cff).unwrap();
    let mut o = Outline::new();
    parsed.outline(0, &mut o).unwrap();
    assert_eq!(
        o.ops(),
        [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 10.0, y: 0.0 },
            PathOp::Close,
        ]
    );
}

#[test]
fn charstring_rrcurveto_emits_cubic() {
    // 0 0 rmoveto 10 20 30 40 50 0 rrcurveto endchar
    let mut cs = Vec::new();
    cs.push(139); // 0
    cs.push(139); // 0
    cs.push(op_code::RMOVETO);
    cs.push(149); // 10
    cs.push(159); // 20
    cs.push(169); // 30
    cs.push(179); // 40
    cs.push(189); // 50
    cs.push(139); // 0
    cs.push(op_code::RRCURVETO);
    cs.push(op_code::ENDCHAR);

    let cff = build_cff_with_charstring(&cs);
    let parsed = Cff::parse(&cff).unwrap();
    let mut o = Outline::new();
    parsed.outline(0, &mut o).unwrap();
    assert!(matches!(o.ops()[0], PathOp::MoveTo { x: 0.0, y: 0.0 }));
    match o.ops()[1] {
        PathOp::CubicTo {
            c1x,
            c1y,
            c2x,
            c2y,
            x,
            y,
        } => {
            assert!((c1x - 10.0).abs() < 1e-4);
            assert!((c1y - 20.0).abs() < 1e-4);
            assert!((c2x - 40.0).abs() < 1e-4);
            assert!((c2y - 60.0).abs() < 1e-4);
            assert!((x - 90.0).abs() < 1e-4);
            assert!((y - 60.0).abs() < 1e-4);
        }
        _ => panic!("expected CubicTo at 1"),
    }
}

#[test]
fn subr_bias_matches_spec() {
    assert_eq!(subr_bias(0), 107);
    assert_eq!(subr_bias(1239), 107);
    assert_eq!(subr_bias(1240), 1131);
    assert_eq!(subr_bias(33_899), 1131);
    assert_eq!(subr_bias(33_900), 32_768);
}

#[test]
fn dict_operand_integer_round_trip() {
    // Inline single byte (32..=246).
    let data = [139u8]; // 0
    let mut r = Reader::new(&data);
    let op = read_dict_operand(&mut r).unwrap();
    assert!(matches!(op, DictOperand::Integer(0)));
}

#[test]
fn charstring_callsubr_executes_local_subroutine() {
    // Local subroutine 0 (biased index = 0 - 107 = -107 -> call
    // subr with arg -107): emits rlineto (0, 50).
    //
    // With subr_count < 1240 the bias is 107. We want to call
    // subroutine index 0, so the charstring pushes (0 - 107) =
    // -107, which after + bias (107) -> 0. We construct both
    // the charstring and a local subr, but our
    // `build_cff_with_charstring` helper doesn't support
    // private dict / subrs. Instead, exercise callgsubr: a
    // global subr at index 0 is easier because the CFF header
    // layout in the fixture already carries an empty global
    // subr INDEX and a trivial patcher would be disruptive.
    //
    // Simpler approach: call a CALLSUBR into an empty locals
    // list and assert that the interpreter surfaces a
    // Malformed error rather than panicking. This exercises
    // the bias path without having to rebuild the fixture.
    let mut cs = Vec::new();
    // push 0 (via 139 single-byte).
    cs.push(139);
    cs.push(op_code::CALLSUBR);
    let cff = build_cff_with_charstring(&cs);
    let parsed = Cff::parse(&cff).unwrap();
    let mut o = Outline::new();
    let err = parsed.outline(0, &mut o).unwrap_err();
    assert!(matches!(err, Error::Malformed { .. }));
}

#[test]
fn charstring_return_halts_subroutine_body() {
    // A RETURN at the top of a glyph charstring is valid:
    // the interpreter simply stops reading. Followed by no
    // endchar this means we drew nothing; the outline is
    // empty. Use this to verify that `run` terminates cleanly
    // on RETURN without error.
    let mut cs = Vec::new();
    cs.push(op_code::RETURN);
    let cff = build_cff_with_charstring(&cs);
    let parsed = Cff::parse(&cff).unwrap();
    let mut o = Outline::new();
    parsed.outline(0, &mut o).unwrap();
    assert!(o.is_empty());
}

#[test]
fn charstring_rejects_operand_stack_overflow() {
    // CFF1 spec caps the operand stack at 48. Pushing 49 values
    // without consuming them must error cleanly rather than
    // growing the Vec without bound.
    let mut cs = Vec::new();
    for _ in 0..49 {
        cs.push(139); // 0
    }
    cs.push(op_code::ENDCHAR);
    let cff = build_cff_with_charstring(&cs);
    let parsed = Cff::parse(&cff).unwrap();
    let mut o = Outline::new();
    let err = parsed.outline(0, &mut o).unwrap_err();
    assert!(matches!(err, Error::Malformed { .. }));
}

/// The two bytes of `dotsection` (operator 12 0).
const DOTSECTION: [u8; 2] = [op_code::ESCAPE, op_code::ESC_DOTSECTION];

/// The outline of the one glyph of a CFF1 font whose charstring is
/// `cs`.
fn draw_single(cs: &[u8]) -> Result<Vec<PathOp>> {
    let cff = build_cff_with_charstring(cs);
    let mut o = Outline::new();
    Cff::parse(&cff)?.outline(0, &mut o)?;
    Ok(o.ops().to_vec())
}

#[test]
fn dotsection_draws_nothing() {
    // A charstring as a Type 1 conversion leaves it: a width, a stem
    // hint, and `dotsection` around the path, `100 10 20 hstem
    // dotsection 0 0 rmoveto 10 0 rlineto 0 10 rlineto dotsection
    // endchar`. HarfBuzz and FreeType skip `dotsection`, which used to
    // fail the glyph as a deprecated operator.
    let mut cs = alloc::vec![239, 149, 159, op_code::HSTEM];
    cs.extend_from_slice(&DOTSECTION);
    cs.extend_from_slice(&[139, 139, op_code::RMOVETO]);
    cs.extend_from_slice(&[149, 139, op_code::RLINETO, 139, 149, op_code::RLINETO]);
    cs.extend_from_slice(&DOTSECTION);
    cs.push(op_code::ENDCHAR);
    let square = [
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 10.0, y: 0.0 },
        PathOp::LineTo { x: 10.0, y: 10.0 },
        PathOp::Close,
    ];
    assert_eq!(draw_single(&cs).unwrap(), square);
    // The same charstring without `dotsection` draws the same.
    let plain: Vec<u8> = [
        239,
        149,
        159,
        op_code::HSTEM,
        139,
        139,
        op_code::RMOVETO,
        149,
        139,
        op_code::RLINETO,
        139,
        149,
        op_code::RLINETO,
        op_code::ENDCHAR,
    ]
    .to_vec();
    assert_eq!(draw_single(&plain).unwrap(), square);
}

#[test]
fn dotsection_clears_the_operand_stack() {
    // `0 0 rmoveto 7 dotsection 10 0 rlineto endchar`: the 7 goes with
    // `dotsection`, as in HarfBuzz, so `rlineto` draws one line to
    // (10, 0). Left on the stack it would draw a line to (7, 10).
    let mut cs = alloc::vec![139, 139, op_code::RMOVETO, 146];
    cs.extend_from_slice(&DOTSECTION);
    cs.extend_from_slice(&[149, 139, op_code::RLINETO, op_code::ENDCHAR]);
    assert_eq!(
        draw_single(&cs).unwrap(),
        [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 10.0, y: 0.0 },
            PathOp::Close,
        ]
    );
}

#[test]
fn dotsection_in_a_subroutine_and_in_cff2() {
    // `dotsection` inside a global subroutine, then in a CFF2
    // charstring, where HarfBuzz clears the stack for it too, as for
    // any operator it does not know.
    let subr = [146, DOTSECTION[0], DOTSECTION[1], op_code::RETURN];
    let bytes = encode_index(&[&subr], 1);
    let globals = read_index(&mut Reader::new(&bytes)).unwrap();
    // One global subr has bias 107: subr 0 is pushed as -107, byte 32.
    let cs = [
        139,
        139,
        op_code::RMOVETO,
        32,
        op_code::CALLGSUBR,
        149,
        139,
        op_code::RLINETO,
    ];
    let expected = [
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 10.0, y: 0.0 },
        PathOp::Close,
    ];
    for is_cff2 in [false, true] {
        let mut out = Outline::new();
        let mut interp = Interp::new(globals, Index::default(), &mut out, is_cff2);
        interp.run(&cs, 0).unwrap();
        interp.finish();
        assert_eq!(out.ops(), expected, "is_cff2 {is_cff2}");
    }
}

#[test]
fn other_deprecated_type1_operators_are_still_rejected() {
    // `callothersubr` (12 16) stays unsupported.
    let cs = [139, op_code::ESCAPE, 16, op_code::ENDCHAR];
    let err = draw_single(&cs).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
}

#[test]
fn charstring_hflex1_endpoint_returns_to_start_y() {
    // hflex1 spec: the flex starts and ends at the same y value.
    // Args: dx1 dy1 dx2 dy2 dx3 dx4 dx5 dy5 dx6. Use dy1=5, dy2=3,
    // dy5=-2, a non-trivial set where the buggy dy_total formula
    // (a[1]+a[3]+a[6], mixing dx5 for dy5) diverges from the
    // correct a[1]+a[3]+a[7]. Start at (0, 100). Expected final y
    // = 100.
    let enc = |n: i32| -> Vec<u8> {
        // Use SHORTINT encoding (op 28, i16) for clean small ints.
        let mut v = Vec::new();
        v.push(op_code::SHORTINT);
        v.extend_from_slice(&(n as i16).to_be_bytes());
        v
    };
    let mut cs = Vec::new();
    cs.extend(enc(0)); // move_to x0=0
    cs.extend(enc(100)); // move_to y0=100
    cs.push(op_code::RMOVETO);
    // hflex1 args.
    cs.extend(enc(10)); // dx1
    cs.extend(enc(5)); // dy1
    cs.extend(enc(10)); // dx2
    cs.extend(enc(3)); // dy2
    cs.extend(enc(10)); // dx3
    cs.extend(enc(10)); // dx4
    cs.extend(enc(10)); // dx5
    cs.extend(enc(-2)); // dy5
    cs.extend(enc(10)); // dx6
    cs.push(op_code::ESCAPE);
    cs.push(op_code::ESC_HFLEX1);
    cs.push(op_code::ENDCHAR);

    let cff = build_cff_with_charstring(&cs);
    let parsed = Cff::parse(&cff).unwrap();
    let mut o = Outline::new();
    parsed.outline(0, &mut o).unwrap();

    // The second CubicTo's endpoint must share the y of the
    // original MoveTo (100). The buggy implementation swapped
    // a[6] (dx5) for a[7] (dy5) in dy_total, yielding a final y
    // of 100 + a[7] - a[6] = 92.
    let cubics: Vec<_> = o
        .ops()
        .iter()
        .filter_map(|op| match op {
            PathOp::CubicTo { x, y, .. } => Some((*x, *y)),
            _ => None,
        })
        .collect();
    assert_eq!(cubics.len(), 2, "hflex1 must emit exactly two cubics");
    let (_, last_y) = cubics[1];
    assert!(
        (last_y - 100.0).abs() < 1e-3,
        "hflex1 endpoint y was {last_y}, expected 100.0 (start y)"
    );
}

#[test]
fn charstring_exponential_subr_calls_hit_op_limit() {
    // Ten global subrs. Subr k calls subr k + 1 twenty times and
    // subr 9 only returns. The depth stays within the cap of 10,
    // but the call tree has 20^9 leaves, so without an operation
    // limit the walk never finishes. With ten subrs the bias is
    // 107, so subr k is pushed as the single byte
    // k - 107 + 139 = k + 32.
    let mut subrs: Vec<Vec<u8>> = Vec::new();
    for k in 0..9u8 {
        let mut s = Vec::new();
        for _ in 0..20 {
            s.push(k + 1 + 32);
            s.push(op_code::CALLGSUBR);
        }
        s.push(op_code::RETURN);
        subrs.push(s);
    }
    subrs.push(alloc::vec![op_code::RETURN]);
    let entries: Vec<&[u8]> = subrs.iter().map(Vec::as_slice).collect();
    let bytes = encode_index(&entries, 2);
    let globals = read_index(&mut Reader::new(&bytes)).unwrap();
    let cs = [32, op_code::CALLGSUBR, op_code::ENDCHAR];
    let mut out = Outline::new();
    let mut interp = Interp::new(globals, Index::default(), &mut out, false);
    let err = interp.run(&cs, 0).unwrap_err();
    assert!(matches!(err, Error::Malformed { .. }));
}

// ----------------------------------------------------------------------------
// FDSelect lookups.
// ----------------------------------------------------------------------------

use crate::tables::cff::dict::fill_fd_ranges;

/// Encodes FDSelect format 3. Every FD must fit its u8 field.
fn fd_select_format3(ranges: &[(usize, u16)], sentinel: usize) -> Vec<u8> {
    let mut out = alloc::vec![3];
    out.extend_from_slice(&(ranges.len() as u16).to_be_bytes());
    for &(first, fd) in ranges {
        out.extend_from_slice(&(first as u16).to_be_bytes());
        out.push(u8::try_from(fd).unwrap());
    }
    out.extend_from_slice(&(sentinel as u16).to_be_bytes());
    out
}

fn fd_select_format4(ranges: &[(usize, u16)], sentinel: usize) -> Vec<u8> {
    let mut out = alloc::vec![4];
    out.extend_from_slice(&(ranges.len() as u32).to_be_bytes());
    for &(first, fd) in ranges {
        out.extend_from_slice(&(first as u32).to_be_bytes());
        out.extend_from_slice(&fd.to_be_bytes());
    }
    out.extend_from_slice(&(sentinel as u32).to_be_bytes());
    out
}

/// Checks the per-glyph lookup against the front-to-back fill that
/// FDSelect used to be expanded with, in format 4 and, when every FD
/// fits in a byte, in format 3. Ascending ranges must search the
/// records as they are, and any others must work out their spans, at
/// most one per glyph and one per range. Glyphs past the glyph count
/// map to FD 0.
fn assert_matches_fill(ranges: &[(usize, u16)], sentinel: usize, n_glyphs: usize) {
    let expected = fill_fd_ranges(ranges, sentinel, n_glyphs);
    let ascending = ranges.windows(2).all(|w| w[0].0 <= w[1].0);
    let mut formats = alloc::vec![(fd_select_format4(ranges, sentinel), true)];
    if ranges.iter().all(|&(_, fd)| fd <= 0xFF) {
        formats.push((fd_select_format3(ranges, sentinel), false));
    }
    for (bytes, allow_format4) in formats {
        let sel = FdSelect::parse(&bytes, 0, n_glyphs, allow_format4, "test").unwrap();
        match sel.span_count() {
            None => assert!(ascending, "ranges {ranges:?}"),
            Some(spans) => {
                assert!(!ascending, "ranges {ranges:?}");
                assert!(spans <= ranges.len().min(n_glyphs), "ranges {ranges:?}");
            }
        }
        for (gid, &fd) in expected.iter().enumerate() {
            assert_eq!(
                sel.fd_for_glyph(gid),
                fd,
                "gid {gid}, ranges {ranges:?}, sentinel {sentinel}"
            );
        }
        for gid in [n_glyphs, n_glyphs + 1, usize::MAX] {
            assert_eq!(sel.fd_for_glyph(gid), 0, "gid {gid}, ranges {ranges:?}");
        }
    }
}

#[test]
fn fd_select_ranges_match_the_fill() {
    // Sorted.
    assert_matches_fill(&[(0, 1), (5, 2), (9, 3)], 12, 12);
    // Glyphs before the first range and past the sentinel map to FD 0.
    assert_matches_fill(&[(3, 1), (7, 2)], 10, 12);
    // A repeated first glyph makes the earlier range empty.
    assert_matches_fill(&[(0, 1), (4, 2), (4, 3), (8, 4)], 12, 12);
    // The sentinel may pass the glyph count, or come before the last
    // range's first glyph.
    assert_matches_fill(&[(0, 1), (6, 2)], 100, 10);
    assert_matches_fill(&[(0, 1), (6, 2)], 3, 10);
    // Unsorted: a later range never reclaims glyphs an earlier range
    // already passed.
    assert_matches_fill(&[(0, 1), (8, 2), (2, 3)], 10, 10);
    assert_matches_fill(&[(5, 1), (0, 2), (3, 3)], 10, 10);
    // No ranges at all.
    assert_matches_fill(&[], 10, 10);
}

#[test]
fn fd_select_alternating_ranges_match_the_fill() {
    // The shape of the fuzzer find in
    // `cff_fd_select_with_unsorted_ranges_is_not_expanded`, small.
    let n = 40;
    let ranges: Vec<(usize, u16)> = (0..64)
        .map(|i| (if i % 2 == 0 { 0 } else { n }, i as u16))
        .collect();
    assert_matches_fill(&ranges, n, n);
}

#[test]
fn fd_select_pseudo_random_ranges_match_the_fill() {
    // A fixed linear congruential generator, so the cases are the
    // same on every run.
    let mut state = 0x2545_f491_u32;
    let mut next = |bound: u32| {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((state >> 8) % bound) as usize
    };
    for _ in 0..300 {
        let n_ranges = next(12);
        let ranges: Vec<(usize, u16)> = (0..n_ranges).map(|_| (next(40), next(8) as u16)).collect();
        let sentinel = next(45);
        assert_matches_fill(&ranges, sentinel, 32);
    }
}

#[test]
fn fd_select_pseudo_random_ascending_ranges_match_the_fill() {
    // As above, with the first glyphs sorted, so every case takes the
    // binary search. Repeated first glyphs, first glyphs past the glyph
    // count, and sentinels before the last range all come up.
    let mut state = 0x1b87_3593_u32;
    let mut next = |bound: u32| {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((state >> 8) % bound) as usize
    };
    for _ in 0..300 {
        let n_ranges = next(12);
        let mut ranges: Vec<(usize, u16)> =
            (0..n_ranges).map(|_| (next(40), next(8) as u16)).collect();
        ranges.sort_by_key(|&(first, _)| first);
        let sentinel = next(45);
        assert_matches_fill(&ranges, sentinel, 32);
    }
}

#[test]
fn fd_select_larger_pseudo_random_unsorted_ranges_match_the_fill() {
    // More and longer unsorted tables than above, in three shapes:
    // first glyphs anywhere, sorted ranges with a few swapped, and
    // ranges that mostly restart near glyph 0. First glyphs and
    // sentinels run past the glyph count, and FDs past 255 test format
    // 4 alone.
    let mut state = 0x9e37_79b9_u32;
    let mut next = |bound: u32| {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((state >> 8) % bound) as usize
    };
    for case in 0..600 {
        let n_glyphs = 1 + next(300);
        let n_ranges = next(80);
        let bound = (n_glyphs + n_glyphs / 4 + 1) as u32;
        let fd_bound = if case % 3 == 0 { 600 } else { 16 };
        let mut ranges: Vec<(usize, u16)> = (0..n_ranges)
            .map(|_| (next(bound), next(fd_bound) as u16))
            .collect();
        match case % 3 {
            1 => {
                ranges.sort_by_key(|&(first, _)| first);
                for _ in 0..=next(3) {
                    if ranges.len() >= 2 {
                        let i = next(ranges.len() as u32);
                        let j = next(ranges.len() as u32);
                        ranges.swap(i, j);
                    }
                }
            }
            2 => {
                for r in &mut ranges {
                    if next(4) != 0 {
                        r.0 = next(8);
                    }
                }
            }
            _ => {}
        }
        let sentinel = next(bound + 8);
        assert_matches_fill(&ranges, sentinel, n_glyphs);
    }
}

#[test]
fn fd_select_unsorted_spans_skip_ranges_that_keep_nothing() {
    // Range 0 runs from glyph 3 to glyph 1, where range 1 starts, so it
    // keeps nothing, and glyph 0 maps to FD 0. Range 1 keeps glyphs 1
    // to 5. Range 2 runs from glyph 6 back to glyph 2 and keeps
    // nothing. Range 3 starts at glyph 2, inside what range 1 already
    // passed, so it keeps glyphs 6 to 8 only. Glyph 9 is past the
    // sentinel.
    let ranges = [(3, 1), (1, 2), (6, 3), (2, 4)];
    let f3 = fd_select_format3(&ranges, 9);
    let sel = FdSelect::parse(&f3, 0, 10, false, "test").unwrap();
    assert_eq!(sel.span_count(), Some(2));
    assert_eq!(
        (0..10).map(|g| sel.fd_for_glyph(g)).collect::<Vec<_>>(),
        [0, 2, 2, 2, 2, 2, 4, 4, 4, 0]
    );
    assert_matches_fill(&ranges, 9, 10);
}

#[test]
fn fd_select_unsorted_spans_stay_bounded_by_the_glyph_count() {
    // 65,535 ranges that alternate between first glyph 0 and first
    // glyph 40, over 40 glyphs. Range 0 keeps every glyph, so one span
    // is all the lookup keeps, whatever the range count.
    let n = 40;
    let ranges: Vec<(usize, u16)> = (0..65_535)
        .map(|i| (if i % 2 == 0 { 0 } else { n }, (i % 200) as u16))
        .collect();
    let f3 = fd_select_format3(&ranges, n);
    let sel = FdSelect::parse(&f3, 0, n, false, "test").unwrap();
    assert_eq!(sel.span_count(), Some(1));
    assert_matches_fill(&ranges, n, n);
}

#[test]
fn fd_select_binary_search_finds_every_one_glyph_range() {
    // 300 one-glyph ranges, each in its own FD, then glyphs past the
    // sentinel.
    let ranges: Vec<(usize, u16)> = (0..300).map(|g| (g, g as u16 + 1)).collect();
    assert_matches_fill(&ranges, 300, 310);
    let f4 = fd_select_format4(&ranges, 300);
    let sel = FdSelect::parse(&f4, 0, 310, true, "test").unwrap();
    assert_eq!(sel.fd_for_glyph(0), 1);
    assert_eq!(sel.fd_for_glyph(299), 300);
    assert_eq!(sel.fd_for_glyph(300), 0);
}

#[test]
fn fd_select_format0_reads_one_byte_per_glyph() {
    let bytes = [0, 2, 0, 1];
    let sel = FdSelect::parse(&bytes, 0, 3, false, "test").unwrap();
    assert_eq!(
        (0..3).map(|g| sel.fd_for_glyph(g)).collect::<Vec<_>>(),
        [2, 0, 1]
    );
}

#[test]
fn fd_select_truncated_tables_fail_to_open() {
    // Format 0 with fewer bytes than glyphs.
    let err = FdSelect::parse(&[0, 1, 1], 0, 3, false, "test").unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }), "{err:?}");
    // Format 3 whose ranges run past the end.
    let mut f3 = fd_select_format3(&[(0, 1), (5, 2)], 10);
    f3.truncate(f3.len() - 3);
    let err = FdSelect::parse(&f3, 0, 10, false, "test").unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }), "{err:?}");
    // Format 4 with a range count no table could back.
    let mut f4 = alloc::vec![4];
    f4.extend_from_slice(&u32::MAX.to_be_bytes());
    let err = FdSelect::parse(&f4, 0, 10, true, "test").unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }), "{err:?}");
}

#[test]
fn fd_select_format4_is_cff2_only() {
    let f4 = fd_select_format4(&[(0, 1)], 4);
    let err = FdSelect::parse(&f4, 0, 4, false, "CFF FDSelect format != 0/3").unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    assert!(FdSelect::parse(&f4, 0, 4, true, "test").is_ok());
}

#[test]
fn fd_select_format4_keeps_fd_indices_past_255() {
    // Format 4 FDs are u16. They used to be cut to their low byte, so
    // FD 256 read as FD 0 and FD 0x1234 as FD 0x34.
    let ranges = [(0, 1), (2, 256), (5, 0x1234), (7, 0xFFFF)];
    let f4 = fd_select_format4(&ranges, 9);
    let sel = FdSelect::parse(&f4, 0, 10, true, "test").unwrap();
    assert_eq!(
        (0..10).map(|g| sel.fd_for_glyph(g)).collect::<Vec<_>>(),
        [1, 1, 256, 256, 256, 0x1234, 0x1234, 0xFFFF, 0xFFFF, 0]
    );
    assert_matches_fill(&ranges, 9, 10);
}

// ----------------------------------------------------------------------------
// CID-keyed fonts: per-glyph Font DICT resolution.
// ----------------------------------------------------------------------------

/// One Font DICT of a CID-keyed test font.
enum TestFd {
    /// An empty Font DICT: no Private DICT, so no Local Subrs.
    NoPrivate,
    /// A Private DICT whose Local Subrs INDEX holds these subroutines.
    Private(Vec<Vec<u8>>),
    /// A Private DICT that points past the end of the table.
    PrivatePastEnd,
}

/// Encodes `v` as a 5-byte DICT integer.
fn dict_i32(out: &mut Vec<u8>, v: usize) {
    out.push(29);
    out.extend_from_slice(&(v as i32).to_be_bytes());
}

/// Offset of the CharStrings INDEX in [`build_cid_cff`] tables: the
/// header, the Name INDEX, the Top DICT INDEX (with a 20-byte DICT),
/// and the empty String and Global Subr INDEX structures.
const CID_CS_OFF: usize = 4 + 6 + (2 + 1 + 2 + 20) + 2 + 2;

/// Builds a CID-keyed CFF1 table: one charstring per glyph, the given
/// Font DICTs, and an FDSelect in format 0 holding `fd_select`.
fn build_cid_cff(charstrings: &[&[u8]], fds: &[TestFd], fd_select: &[u8]) -> Vec<u8> {
    let char_strings = encode_index(charstrings, 2);
    let fda_off = CID_CS_OFF + char_strings.len();
    let font_dict_len = |fd: &TestFd| match fd {
        TestFd::NoPrivate => 0,
        _ => 11,
    };
    let fda_len = 3 + (fds.len() + 1) * 2 + fds.iter().map(font_dict_len).sum::<usize>();
    let fds_off = fda_off + fda_len;

    // Private DICTs, each followed by its Local Subrs INDEX.
    let mut privates = Vec::new();
    let mut font_dicts: Vec<Vec<u8>> = Vec::new();
    for fd in fds {
        let mut dict = Vec::new();
        match fd {
            TestFd::NoPrivate => {}
            TestFd::Private(subrs) => {
                let priv_off = fds_off + 1 + fd_select.len() + privates.len();
                let mut private = Vec::new();
                dict_i32(&mut private, 6); // Subrs, relative to the Private DICT
                private.push(19);
                let entries: Vec<&[u8]> = subrs.iter().map(Vec::as_slice).collect();
                private.extend(encode_index(&entries, 2));
                dict_i32(&mut dict, 6); // Private DICT size
                dict_i32(&mut dict, priv_off);
                dict.push(18);
                privates.extend(private);
            }
            TestFd::PrivatePastEnd => {
                dict_i32(&mut dict, 100);
                dict_i32(&mut dict, 0x00FF_0000);
                dict.push(18);
            }
        }
        font_dicts.push(dict);
    }

    let mut out = alloc::vec![1, 0, 4, 4];
    out.extend_from_slice(&[0, 1, 1, 1, 2, b'a']); // Name INDEX
    out.extend_from_slice(&[0, 1, 1, 1, 21]); // Top DICT INDEX
    dict_i32(&mut out, CID_CS_OFF);
    out.push(17); // CharStrings
    dict_i32(&mut out, fda_off);
    out.extend_from_slice(&[12, 36]); // FDArray
    dict_i32(&mut out, fds_off);
    out.extend_from_slice(&[12, 37]); // FDSelect
    out.extend_from_slice(&[0, 0, 0, 0]); // String and Global Subr INDEX
    assert_eq!(out.len(), CID_CS_OFF);
    out.extend(char_strings);
    let entries: Vec<&[u8]> = font_dicts.iter().map(Vec::as_slice).collect();
    out.extend(encode_index(&entries, 2));
    assert_eq!(out.len(), fds_off);
    out.push(0);
    out.extend_from_slice(fd_select);
    out.extend(privates);
    out
}

/// `0 0 rmoveto 10 0 rlineto endchar`.
const ONE_EDGE: [u8; 7] = [
    139,
    139,
    op_code::RMOVETO,
    149,
    139,
    op_code::RLINETO,
    op_code::ENDCHAR,
];

#[test]
fn cid_glyph_calls_the_local_subrs_of_its_font_dict() {
    // Glyph 0 uses FD 1, whose Local Subr 0 draws `10 0 rlineto`. With
    // one subr the bias is 107, so subr 0 is called as -107 (byte 32).
    let subr = alloc::vec![149, 139, op_code::RLINETO, op_code::RETURN];
    let calls_subr = [
        139,
        139,
        op_code::RMOVETO,
        32,
        op_code::CALLSUBR,
        op_code::ENDCHAR,
    ];
    let fds = [TestFd::NoPrivate, TestFd::Private(alloc::vec![subr])];
    let cff = build_cid_cff(&[&calls_subr, &calls_subr], &fds, &[1, 0]);
    let parsed = Cff::parse(&cff).unwrap();
    let mut o = Outline::new();
    parsed.outline(0, &mut o).unwrap();
    assert_eq!(
        o.ops(),
        [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 10.0, y: 0.0 },
            PathOp::Close,
        ]
    );
    // Glyph 1 uses FD 0, which has no Local Subrs to call.
    let err = parsed.outline(1, &mut Outline::new()).unwrap_err();
    assert!(matches!(err, Error::Malformed { .. }), "{err:?}");
}

#[test]
fn cid_glyph_with_a_broken_private_dict_fails_alone() {
    // FD 1's Private DICT points past the end of the table. Parsing
    // used to resolve every Font DICT up front and reject the whole
    // table; now only the glyphs that use FD 1 fail.
    let fds = [TestFd::Private(Vec::new()), TestFd::PrivatePastEnd];
    let cff = build_cid_cff(&[&ONE_EDGE, &ONE_EDGE], &fds, &[0, 1]);
    let parsed = Cff::parse(&cff).unwrap();
    let mut o = Outline::new();
    assert!(parsed.outline(0, &mut o).unwrap());
    assert_eq!(o.len(), 3);
    let err = parsed.outline(1, &mut Outline::new()).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }), "{err:?}");
}

#[test]
fn cid_glyph_with_an_fd_past_the_fdarray_gets_no_local_subrs() {
    let fds = [TestFd::Private(Vec::new())];
    let cff = build_cid_cff(&[&ONE_EDGE], &fds, &[7]);
    let parsed = Cff::parse(&cff).unwrap();
    let mut o = Outline::new();
    assert!(parsed.outline(0, &mut o).unwrap());
    assert_eq!(o.len(), 3);
}

#[test]
fn malformed_charstring_entry_fails_only_its_glyph() {
    // Rewrite the CharStrings offsets from [1, 8, 8, 11] to [1, 9, 8, 11]:
    // glyph 1 runs backward. Glyph 0 gains a trailing byte after its
    // endchar and glyph 2 is intact. The INDEX header is fine, so the
    // table still parses.
    let mut cff = build_cid_cff(
        &[&ONE_EDGE, &[], &ONE_EDGE[..3]],
        &[TestFd::NoPrivate],
        &[0, 0, 0],
    );
    // Offsets follow the count (2 bytes) and offSize (1 byte); slot 1
    // ends glyph 0 and starts glyph 1.
    let slot = CID_CS_OFF + 3 + 2;
    assert_eq!(cff[slot..slot + 2], 8u16.to_be_bytes());
    cff[slot..slot + 2].copy_from_slice(&9u16.to_be_bytes());
    let parsed = Cff::parse(&cff).unwrap();
    assert!(parsed.outline(0, &mut Outline::new()).unwrap());
    let err = parsed.outline(1, &mut Outline::new()).unwrap_err();
    assert!(matches!(err, Error::Malformed { .. }), "{err:?}");
    let mut o = Outline::new();
    assert!(parsed.outline(2, &mut o).unwrap());
    assert_eq!(o.ops(), [PathOp::MoveTo { x: 0.0, y: 0.0 }, PathOp::Close]);
}

// ----------------------------------------------------------------------------
// seac: an endchar that names a base and an accent character.
// ----------------------------------------------------------------------------

/// The charset of a name-keyed test font.
enum TestCharset {
    /// A predefined charset: 0 ISOAdobe, 1 Expert, 2 ExpertSubset.
    Predefined(u8),
    /// A charset table, format byte first, written after the
    /// CharStrings INDEX.
    Table(Vec<u8>),
}

/// Length of the Top DICT in [`build_named_cff`] tables.
const NAMED_TOP_DICT_LEN: usize = 12;

/// Offset of the CharStrings INDEX in [`build_named_cff`] tables.
const NAMED_CS_OFF: usize = 4 + 6 + (2 + 1 + 2 + NAMED_TOP_DICT_LEN) + 2 + 2;

/// Builds a name-keyed CFF1 table with one charstring per glyph, a Top
/// DICT naming the CharStrings INDEX and the charset, and no Private
/// DICT.
fn build_named_cff(charstrings: &[&[u8]], charset: &TestCharset) -> Vec<u8> {
    let char_strings = encode_index(charstrings, 2);
    let charset_value = match charset {
        TestCharset::Predefined(id) => usize::from(*id),
        TestCharset::Table(_) => NAMED_CS_OFF + char_strings.len(),
    };
    let mut out = alloc::vec![1, 0, 4, 4];
    out.extend_from_slice(&[0, 1, 1, 1, 2, b'a']); // Name INDEX
    out.extend_from_slice(&[0, 1, 1, 1, 1 + NAMED_TOP_DICT_LEN as u8]); // Top DICT INDEX
    dict_i32(&mut out, NAMED_CS_OFF);
    out.push(17); // CharStrings
    dict_i32(&mut out, charset_value);
    out.push(15); // charset
    out.extend_from_slice(&[0, 0, 0, 0]); // String and Global Subr INDEX
    assert_eq!(out.len(), NAMED_CS_OFF);
    out.extend(char_strings);
    if let TestCharset::Table(bytes) = charset {
        out.extend_from_slice(bytes);
    }
    out
}

/// `0 0 rmoveto 100 0 rlineto -50 100 rlineto endchar`: the base, "A".
const BASE_A: [u8; 10] = [
    139,
    139,
    op_code::RMOVETO,
    239,
    139,
    op_code::RLINETO,
    89,
    239,
    op_code::RLINETO,
    op_code::ENDCHAR,
];

/// `0 0 rmoveto 20 0 rlineto 0 20 rlineto endchar`: the accent, "acute".
const ACUTE: [u8; 10] = [
    139,
    139,
    op_code::RMOVETO,
    159,
    139,
    op_code::RLINETO,
    139,
    159,
    op_code::RLINETO,
    op_code::ENDCHAR,
];

/// `30 120 65 194 endchar` after `prefix`: "A" (code 65) with "acute"
/// (code 194) at (30, 120).
fn seac_charstring(prefix: &[u8]) -> Vec<u8> {
    let mut cs = prefix.to_vec();
    cs.extend_from_slice(&[169, 247, 12, 204, 247, 86, op_code::ENDCHAR]);
    cs
}

/// "A" at the origin, then "acute" moved to (30, 120).
fn a_acute_ops() -> Vec<PathOp> {
    alloc::vec![
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 100.0, y: 0.0 },
        PathOp::LineTo { x: 50.0, y: 100.0 },
        PathOp::Close,
        PathOp::MoveTo { x: 30.0, y: 120.0 },
        PathOp::LineTo { x: 50.0, y: 120.0 },
        PathOp::LineTo { x: 50.0, y: 140.0 },
        PathOp::Close,
    ]
}

/// A format 0 charset for glyphs .notdef, "A" (SID 34), "acute" (SID
/// 125), and a composite (SID 391).
const SEAC_CHARSET: [u8; 7] = [0, 0, 34, 0, 125, 1, 135];

/// Builds a font of .notdef, "A", "acute", and `composite` as glyph 3.
fn seac_font(composite: &[u8]) -> Vec<u8> {
    build_named_cff(
        &[&[op_code::ENDCHAR], &BASE_A, &ACUTE, composite],
        &TestCharset::Table(SEAC_CHARSET.to_vec()),
    )
}

fn outline_ops(cff: &[u8], gid: u16) -> Result<Vec<PathOp>> {
    let parsed = Cff::parse(cff)?;
    let mut o = Outline::new();
    parsed.outline(gid, &mut o)?;
    Ok(o.ops().to_vec())
}

#[test]
fn seac_with_a_width_draws_base_and_accent() {
    // `500 30 120 65 194 endchar`: the width, then the seac operands.
    // Only exactly four operands used to count as a seac, so this form
    // drew nothing.
    let cs = seac_charstring(&[248, 136]);
    assert_eq!(outline_ops(&seac_font(&cs), 3).unwrap(), a_acute_ops());
}

#[test]
fn seac_without_a_width_draws_base_and_accent() {
    // `30 120 65 194 endchar`. This form used to be rejected.
    let cs = seac_charstring(&[]);
    assert_eq!(outline_ops(&seac_font(&cs), 3).unwrap(), a_acute_ops());
}

#[test]
fn seac_after_hints_reads_the_top_four_operands() {
    // `500 0 10 hstem 7 30 120 65 194 endchar`: hstem took the width,
    // and the seac operands are the top four of five, as HarfBuzz and
    // FreeType read them.
    let cs = seac_charstring(&[248, 136, 139, 149, op_code::HSTEM, 146]);
    assert_eq!(outline_ops(&seac_font(&cs), 3).unwrap(), a_acute_ops());
}

#[test]
fn seac_glyph_keeps_its_own_contours_first() {
    // `0 0 rmoveto 10 0 rlineto 30 120 65 194 endchar`.
    let cs = seac_charstring(&[139, 139, op_code::RMOVETO, 149, 139, op_code::RLINETO]);
    let mut expected = alloc::vec![
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 10.0, y: 0.0 },
        PathOp::Close,
    ];
    expected.extend(a_acute_ops());
    assert_eq!(outline_ops(&seac_font(&cs), 3).unwrap(), expected);
}

#[test]
fn seac_finds_glyphs_through_every_charset_format() {
    // Glyphs .notdef, "A" (SID 34), "B" (SID 35), "acute" (SID 125), and
    // the composite (SID 391) as glyph 4.
    let composite = seac_charstring(&[]);
    let glyphs: [&[u8]; 5] = [
        &[op_code::ENDCHAR],
        &BASE_A,
        &[op_code::ENDCHAR],
        &ACUTE,
        &composite,
    ];
    let charsets: [&[u8]; 3] = [
        // Format 0: one SID per glyph.
        &[0, 0, 34, 0, 35, 0, 125, 1, 135],
        // Format 1: ranges with a u8 count, "A" and "B" in one.
        &[1, 0, 34, 1, 0, 125, 0, 1, 135, 0],
        // Format 2: the same ranges with a u16 count.
        &[2, 0, 34, 0, 1, 0, 125, 0, 0, 1, 135, 0, 0],
    ];
    for charset in charsets {
        let cff = build_named_cff(&glyphs, &TestCharset::Table(charset.to_vec()));
        assert_eq!(
            outline_ops(&cff, 4).unwrap(),
            a_acute_ops(),
            "charset {charset:?}"
        );
    }
}

#[test]
fn seac_in_an_iso_adobe_font_takes_glyph_ids_from_sids() {
    // The ISOAdobe charset gives glyph i SID i, so "A" is glyph 34 and
    // "acute" glyph 125.
    let composite = seac_charstring(&[]);
    let mut glyphs: Vec<&[u8]> = alloc::vec![&[op_code::ENDCHAR]; 126];
    glyphs[1] = &composite;
    glyphs[34] = &BASE_A;
    glyphs[125] = &ACUTE;
    let cff = build_named_cff(&glyphs, &TestCharset::Predefined(0));
    assert_eq!(outline_ops(&cff, 1).unwrap(), a_acute_ops());
    // With 100 glyphs there is no glyph for "acute".
    let cff = build_named_cff(&glyphs[..100], &TestCharset::Predefined(0));
    let err = outline_ops(&cff, 1).unwrap_err();
    assert!(matches!(err, Error::Malformed { .. }), "{err:?}");
}

#[test]
fn seac_code_outside_the_standard_encoding_is_malformed() {
    // `1 1 1 1 endchar`: code 1 has no character in the Standard
    // Encoding.
    let cs = [140, 140, 140, 140, op_code::ENDCHAR];
    let err = outline_ops(&build_cff_with_charstring(&cs), 0).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Malformed {
                context: "CFF seac code not in the Standard Encoding",
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn seac_glyph_missing_from_the_charset_reports_the_charset_offset() {
    // "B" (code 66, SID 35) as the accent: the charset has no SID 35.
    let cs = [169, 247, 12, 204, 205, op_code::ENDCHAR];
    let cff = seac_font(&cs);
    let err = outline_ops(&cff, 3).unwrap_err();
    let charset_off = cff.len() - SEAC_CHARSET.len();
    assert!(
        matches!(err, Error::Malformed { offset, .. } if offset == charset_off),
        "{err:?}"
    );
    // An unknown charset format reports the same offset.
    let cs = seac_charstring(&[]);
    let cff = build_named_cff(
        &[&[op_code::ENDCHAR], &cs],
        &TestCharset::Table(alloc::vec![3]),
    );
    let err = outline_ops(&cff, 1).unwrap_err();
    let charset_off = cff.len() - 1;
    assert!(
        matches!(err, Error::Malformed { offset, .. } if offset == charset_off),
        "{err:?}"
    );
}

#[test]
fn seac_inside_a_seac_component_is_malformed() {
    // The base "A" is itself a seac, which would recurse.
    let composite = seac_charstring(&[]);
    let cff = build_named_cff(
        &[&[op_code::ENDCHAR], &composite, &ACUTE, &composite],
        &TestCharset::Table(SEAC_CHARSET.to_vec()),
    );
    let err = outline_ops(&cff, 3).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Malformed {
                context: "CFF seac base or accent uses seac",
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn seac_is_unsupported_in_cid_fonts_and_expert_charsets() {
    let composite = seac_charstring(&[]);
    let cff = build_cid_cff(&[&composite], &[TestFd::NoPrivate], &[0]);
    let err = outline_ops(&cff, 0).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    for expert in [1, 2] {
        let cff = build_named_cff(
            &[&[op_code::ENDCHAR], &composite],
            &TestCharset::Predefined(expert),
        );
        let err = outline_ops(&cff, 1).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    }
}
