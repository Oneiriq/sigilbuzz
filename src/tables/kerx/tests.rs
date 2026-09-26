//! Tests for the `kerx` parser: table-level parsing and formats 0
//! and 2, plus the shared fixture builders. Formats 1, 4 and 6 have
//! their own child modules.

use super::*;
use alloc::vec;

mod format1;
mod format4;
mod format6;

fn build_kerx_format0(pairs: &[(u16, u16, i16)]) -> Vec<u8> {
    let pair_bytes = pairs.len() * 6;
    let body_len = 16 + pair_bytes; // 4 * u32 + pairs
    let sub_len = 12 + body_len;

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes()); // version
    out.extend_from_slice(&0u16.to_be_bytes()); // pad
    out.extend_from_slice(&1u32.to_be_bytes()); // nTables

    // Subtable header.
    out.extend_from_slice(&(sub_len as u32).to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // coverage: horizontal, format 0
    out.extend_from_slice(&0u32.to_be_bytes()); // tupleCount

    // Format 0 body.
    out.extend_from_slice(&(pairs.len() as u32).to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // searchRange
    out.extend_from_slice(&0u32.to_be_bytes()); // entrySelector
    out.extend_from_slice(&0u32.to_be_bytes()); // rangeShift
    for (l, r, v) in pairs {
        out.extend_from_slice(&l.to_be_bytes());
        out.extend_from_slice(&r.to_be_bytes());
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}

/// Builds a one-subtable kerx with format 2, two left classes
/// (mapped via lookup format 0) and two right classes. `n_glyphs`
/// is the synthetic font's glyph count (keeps the format-0 table
/// dense). `left_classes[i]` is the class for gid `i` (0-based);
/// same for `right_classes`. `matrix[l][r]` is the i16 delta.
fn build_kerx_format2(
    n_glyphs: u16,
    left_classes: &[u16],
    right_classes: &[u16],
    matrix: &[Vec<i16>],
) -> Vec<u8> {
    let n_left = matrix.len() as u32;
    let n_right = matrix[0].len() as u32;
    let row_width = n_right * 2;

    // Left table (format 0): each cell already pre-multiplied
    // by row_width.
    let mut left_lookup: Vec<u8> = Vec::new();
    left_lookup.extend_from_slice(&0u16.to_be_bytes()); // format 0
    for &c in left_classes {
        let off = (u32::from(c) * row_width) as u16;
        left_lookup.extend_from_slice(&off.to_be_bytes());
    }
    // Right table (format 0): each cell pre-multiplied by 2.
    let mut right_lookup: Vec<u8> = Vec::new();
    right_lookup.extend_from_slice(&0u16.to_be_bytes());
    for &c in right_classes {
        let off: u16 = c * 2;
        right_lookup.extend_from_slice(&off.to_be_bytes());
    }

    // Body layout (relative to subtable start):
    //   0  : 12 B common header
    //   12 : 16 B fmt2 header (rowWidth, leftOff, rightOff, arrOff)
    //   28 : left lookup
    //   .. : right lookup
    //   .. : kerning array
    let header_size = 12 + 16;
    let left_off = header_size;
    let right_off = left_off + left_lookup.len();
    let array_off = right_off + right_lookup.len();
    let array_bytes = (n_left * row_width) as usize;
    let sub_len = array_off + array_bytes;

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes()); // version
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes()); // nTables

    // Common subtable header.
    out.extend_from_slice(&(sub_len as u32).to_be_bytes());
    out.extend_from_slice(&2u32.to_be_bytes()); // coverage: format 2
    out.extend_from_slice(&0u32.to_be_bytes()); // tupleCount

    // fmt2 header.
    out.extend_from_slice(&row_width.to_be_bytes());
    out.extend_from_slice(&(left_off as u32).to_be_bytes());
    out.extend_from_slice(&(right_off as u32).to_be_bytes());
    out.extend_from_slice(&(array_off as u32).to_be_bytes());

    out.extend_from_slice(&left_lookup);
    out.extend_from_slice(&right_lookup);
    for row in matrix {
        for v in row {
            out.extend_from_slice(&v.to_be_bytes());
        }
    }

    // Sanity: caller's class arrays must cover n_glyphs.
    assert_eq!(left_classes.len(), n_glyphs as usize);
    assert_eq!(right_classes.len(), n_glyphs as usize);
    out
}

#[test]
fn format0_binary_search_finds_pairs() {
    let bytes = build_kerx_format0(&[(10, 20, -30), (10, 30, -5), (40, 5, 7)]);
    let k = Kerx::parse(&bytes, 256).unwrap();
    assert_eq!(k.version(), 2);
    assert_eq!(k.kern(10, 20), -30);
    assert_eq!(k.kern(40, 5), 7);
    assert_eq!(k.kern(99, 99), 0);
    assert_eq!(k.subtable_pair_kern(0, 10, 20), Some(-30));
    assert_eq!(k.subtable_pair_kern(0, 99, 99), Some(0));
    assert_eq!(k.subtable_pair_kern(1, 10, 20), None);
}

#[test]
fn empty_kerx_table_yields_no_subtables() {
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(&2u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes()); // nTables
    let k = Kerx::parse(&bytes, 0).unwrap();
    assert_eq!(k.subtable_count(), 0);
    assert_eq!(k.kern(1, 2), 0);
}

#[test]
fn vertical_subtable_is_skipped() {
    // Build a 2-subtable kerx: first horizontal, second vertical
    // (coverage bit 31 set). The vertical one should be dropped.
    let sub_body_len = 16 + 6; // one pair
    let sub_len = 12 + sub_body_len;
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(&2u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&2u32.to_be_bytes()); // 2 subtables

    for (coverage, value) in [(0u32, -10i16), (COVERAGE_VERTICAL, 99i16)] {
        bytes.extend_from_slice(&(sub_len as u32).to_be_bytes());
        bytes.extend_from_slice(&coverage.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes()); // tupleCount
        bytes.extend_from_slice(&1u32.to_be_bytes()); // nPairs
        bytes.extend_from_slice(&[0u8; 12]);
        bytes.extend_from_slice(&10u16.to_be_bytes());
        bytes.extend_from_slice(&20u16.to_be_bytes());
        bytes.extend_from_slice(&value.to_be_bytes());
    }
    let k = Kerx::parse(&bytes, 256).unwrap();
    assert_eq!(k.subtable_count(), 1);
    assert_eq!(k.kern(10, 20), -10);
}

#[test]
fn rejects_unknown_version() {
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(&5u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());
    assert!(matches!(
        Kerx::parse(&bytes, 0),
        Err(Error::Unsupported { .. })
    ));
}

#[test]
fn format2_compound_class_lookup_resolves_pairs() {
    // 4-glyph synthetic font:
    //   gid 0 .notdef       -> left class 0, right class 0
    //   gid 1 A             -> left class 1, right class 0
    //   gid 2 B             -> left class 1, right class 0
    //   gid 3 V             -> left class 0, right class 1
    // Matrix [left][right]:
    //   [[ 0,   0],
    //    [-30,-50]]
    // So (A, V) and (B, V) both kern by -50, while every other
    // pair is zero (and therefore never matches).
    let bytes = build_kerx_format2(
        4,
        &[0, 1, 1, 0],
        &[0, 0, 0, 1],
        &[vec![0, 0], vec![-30, -50]],
    );
    let k = Kerx::parse(&bytes, 4).unwrap();
    assert_eq!(k.subtable_count(), 1);
    assert_eq!(k.kern(1, 3), -50, "A-V pair via classes (1, 1)");
    assert_eq!(k.kern(2, 3), -50, "B-V pair via classes (1, 1)");
    assert_eq!(k.kern(1, 1), -30, "A-A pair via classes (1, 0)");
    assert_eq!(k.kern(0, 0), 0, ".notdef pair -> row 0 default");
    assert_eq!(k.kern(3, 1), 0, "V-A reversed pair -> row 0 default");
}

#[test]
fn format2_with_zero_cell_returns_zero() {
    // Pair lands on a zero entry: kern() must still return 0
    // without surfacing a parser error.
    let bytes = build_kerx_format2(3, &[0, 1, 1], &[0, 1, 1], &[vec![0, 0], vec![0, 7]]);
    let k = Kerx::parse(&bytes, 3).unwrap();
    assert_eq!(k.kern(1, 0), 0); // left class 1, right class 0 -> 0
    assert_eq!(k.kern(2, 2), 7); // left class 1, right class 1
}

#[test]
fn format2_bad_class_offset_silently_drops_subtable() {
    // Build a valid fmt2 then clobber the leftClassTable offset
    // to point past the subtable. parse() must still succeed and
    // simply skip the subtable rather than fail the table.
    let mut bytes = build_kerx_format2(3, &[0, 1, 1], &[0, 1, 1], &[vec![0, 0], vec![0, 7]]);
    // Subtable starts at offset 8 (kerx header size). fmt2
    // header at offset 8 + 12 = 20; leftClassTable u32 lives at
    // offset 24.
    let bad = u32::MAX.to_be_bytes();
    bytes[24] = bad[0];
    bytes[25] = bad[1];
    bytes[26] = bad[2];
    bytes[27] = bad[3];
    let k = Kerx::parse(&bytes, 3).unwrap();
    assert_eq!(k.subtable_count(), 0);
}

#[test]
fn truncated_subtable_length_does_not_poison_following_subtables() {
    // Two subtables: the first declares a `length` field that
    // covers only the 12-byte header (no body), too short for
    // any format-0 / format-2 body to fit. The whole-table
    // parse must still succeed and surface the *second*
    // subtable's pair, instead of bailing out and producing
    // zero kerning for the entire font.
    //
    // Pre-fix: `parse_format0` returns `Err(Truncated)` from
    // inside the loop, the `?` propagates, and the kerx parse
    // fails, even though the second subtable is well-formed.
    let pair_bytes = 6;
    let good_body = 16 + pair_bytes;
    let good_sub_len = 12 + good_body;
    // Bad subtable length: 12 (header only). Body is missing.
    let bad_sub_len: u32 = 12;

    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(&2u16.to_be_bytes()); // version
    bytes.extend_from_slice(&0u16.to_be_bytes()); // pad
    bytes.extend_from_slice(&2u32.to_be_bytes()); // nTables = 2

    // Subtable 1: malformed. Header says 12 bytes total, no body.
    bytes.extend_from_slice(&bad_sub_len.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes()); // coverage: format 0, horizontal
    bytes.extend_from_slice(&0u32.to_be_bytes()); // tupleCount

    // Subtable 2: well-formed format 0 with one pair (10, 20) -> -42.
    bytes.extend_from_slice(&(good_sub_len as u32).to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes()); // coverage: format 0
    bytes.extend_from_slice(&0u32.to_be_bytes()); // tupleCount
    bytes.extend_from_slice(&1u32.to_be_bytes()); // nPairs
    bytes.extend_from_slice(&[0u8; 12]); // search hints
    bytes.extend_from_slice(&10u16.to_be_bytes());
    bytes.extend_from_slice(&20u16.to_be_bytes());
    bytes.extend_from_slice(&(-42i16).to_be_bytes());

    let k = Kerx::parse(&bytes, 256).expect("kerx parse must not fail");
    assert_eq!(
        k.subtable_count(),
        1,
        "malformed subtable should be skipped, well-formed one kept"
    );
    assert_eq!(k.kern(10, 20), -42, "well-formed subtable's pair lookup");
}

#[test]
fn subtable_length_smaller_than_header_does_not_loop_or_overlap() {
    // Pathological: subtable length declared as 5 bytes, smaller
    // than its own 12-byte common header. After the header read
    // the cursor sits at sub_start + 12, but `seek(sub_end)` would
    // jump backwards to sub_start + 5, parking the next iteration
    // mid-header. The parser must refuse to seek backwards (or
    // skip the subtable cleanly) so a malformed font cannot drag
    // the rest of `kerx` into garbage territory.
    let pair_bytes = 6;
    let good_body = 16 + pair_bytes;
    let good_sub_len = 12 + good_body;

    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(&2u16.to_be_bytes()); // version
    bytes.extend_from_slice(&0u16.to_be_bytes()); // pad
    bytes.extend_from_slice(&2u32.to_be_bytes()); // nTables

    // Subtable 1: length = 5. Body would overlap header.
    bytes.extend_from_slice(&5u32.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());

    // Subtable 2: well-formed.
    bytes.extend_from_slice(&(good_sub_len as u32).to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(&1u32.to_be_bytes()); // nPairs
    bytes.extend_from_slice(&[0u8; 12]);
    bytes.extend_from_slice(&10u16.to_be_bytes());
    bytes.extend_from_slice(&20u16.to_be_bytes());
    bytes.extend_from_slice(&(-7i16).to_be_bytes());

    let k = Kerx::parse(&bytes, 256).expect("kerx parse must not fail");
    // The sub-header-size subtable is dropped; the well-formed
    // one is preserved.
    assert_eq!(k.subtable_count(), 1);
    assert_eq!(k.kern(10, 20), -7);
}

/// Builds an AAT lookup-table format 6 (sorted glyph->class
/// pairs). Mirrors the helper in `state_table.rs::tests` since
/// `mod tests` is private to its module.
fn build_lookup_format6(pairs: &[(u16, u16)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&6u16.to_be_bytes()); // format
    out.extend_from_slice(&4u16.to_be_bytes()); // unitSize
    out.extend_from_slice(&(pairs.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0u8; 6]); // search hints
    for (g, v) in pairs {
        out.extend_from_slice(&g.to_be_bytes());
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}
