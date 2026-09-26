//! Tests for `kerx` format 6 (simple n x m kerning array).

use super::*;

// -----------------------------------------------------------------
// Format 6: simple n x m kerning array.
// -----------------------------------------------------------------

/// Builds a one-subtable kerx with format 6, two row classes and
/// three column classes. `n_glyphs` is the synthetic font's glyph
/// count. `row_classes[i]` is the row index for gid `i` (0-based);
/// same for `col_classes`. `matrix[r][c]` is the i16 delta.
fn build_kerx_format6(
    n_glyphs: u16,
    row_classes: &[u16],
    col_classes: &[u16],
    matrix: &[Vec<i16>],
) -> Vec<u8> {
    let row_count = matrix.len() as u16;
    let column_count = matrix[0].len() as u16;

    // Row / column lookup tables (format 0): direct row / column
    // index per glyph.
    let mut row_lookup: Vec<u8> = Vec::new();
    row_lookup.extend_from_slice(&0u16.to_be_bytes()); // format 0
    for &c in row_classes {
        row_lookup.extend_from_slice(&c.to_be_bytes());
    }
    let mut col_lookup: Vec<u8> = Vec::new();
    col_lookup.extend_from_slice(&0u16.to_be_bytes());
    for &c in col_classes {
        col_lookup.extend_from_slice(&c.to_be_bytes());
    }

    // Body layout (relative to subtable start):
    //   0  : 12 B common header
    //   12 : 20 B fmt6 header
    //   32 : row lookup
    //   .. : col lookup
    //   .. : kerning array (row_count x column_count i16s)
    let header_size = 12 + 20;
    let row_off = header_size;
    let col_off = row_off + row_lookup.len();
    let array_off = col_off + col_lookup.len();
    let array_bytes = (row_count as usize) * (column_count as usize) * 2;
    let sub_len = array_off + array_bytes;

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes()); // version
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes()); // nTables

    // Common subtable header.
    out.extend_from_slice(&(sub_len as u32).to_be_bytes());
    out.extend_from_slice(&6u32.to_be_bytes()); // coverage: format 6
    out.extend_from_slice(&0u32.to_be_bytes()); // tupleCount

    // fmt6 header.
    out.extend_from_slice(&0u32.to_be_bytes()); // flags (no long values)
    out.extend_from_slice(&row_count.to_be_bytes());
    out.extend_from_slice(&column_count.to_be_bytes());
    out.extend_from_slice(&(row_off as u32).to_be_bytes());
    out.extend_from_slice(&(col_off as u32).to_be_bytes());
    out.extend_from_slice(&(array_off as u32).to_be_bytes());

    out.extend_from_slice(&row_lookup);
    out.extend_from_slice(&col_lookup);
    for row in matrix {
        for v in row {
            out.extend_from_slice(&v.to_be_bytes());
        }
    }

    // Sanity: caller's class arrays must cover n_glyphs.
    assert_eq!(row_classes.len(), n_glyphs as usize);
    assert_eq!(col_classes.len(), n_glyphs as usize);
    out
}

#[test]
fn format6_simple_grid_resolves_pairs() {
    // 4-glyph synthetic font:
    //   gid 0 .notdef -> row 0, col 0
    //   gid 1 A       -> row 1, col 0
    //   gid 2 B       -> row 1, col 0
    //   gid 3 V       -> row 0, col 1
    // Matrix [row][col]:
    //   [[ 0,   0,   0],
    //    [-30,-50, -70]]
    let bytes = build_kerx_format6(
        4,
        &[0, 1, 1, 0],
        &[0, 0, 0, 1],
        &[vec![0, 0, 0], vec![-30, -50, -70]],
    );
    let k = Kerx::parse(&bytes, 4).unwrap();
    assert_eq!(k.subtable_count(), 1);
    assert_eq!(k.kern(1, 3), -50, "A-V via (row 1, col 1)");
    assert_eq!(k.kern(2, 3), -50, "B-V shares row 1");
    assert_eq!(k.kern(1, 1), -30, "A-A via (row 1, col 0)");
    assert_eq!(k.kern(0, 0), 0, ".notdef pair -> row 0 default");
    assert_eq!(k.kern(3, 1), 0, "V-A reversed pair -> row 0 default");
}

#[test]
fn format6_rejects_long_values_flag() {
    // Build a valid fmt6, then set the long-values flag bit so the
    // parse path drops the subtable cleanly. The subtable becomes
    // unusable but parse() must still succeed.
    let mut bytes = build_kerx_format6(3, &[0, 1, 1], &[0, 1, 1], &[vec![0, 0], vec![0, 7]]);
    // Subtable starts at offset 8 (kerx header). fmt6 header at
    // offset 8 + 12 = 20; flags u32 lives there.
    bytes[20..24].copy_from_slice(&0x0000_0001u32.to_be_bytes());
    let k = Kerx::parse(&bytes, 3).unwrap();
    assert_eq!(k.subtable_count(), 0, "long-values flag drops the subtable");
}

#[test]
fn format6_bad_offset_silently_drops_subtable() {
    // Clobber the rowIndexTable u32 to point past the subtable:
    // parse must still succeed and skip the subtable.
    let mut bytes = build_kerx_format6(3, &[0, 1, 1], &[0, 1, 1], &[vec![0, 0], vec![0, 7]]);
    // fmt6 header at offset 20; rowIndexTable at +8 = 28.
    let bad = u32::MAX.to_be_bytes();
    bytes[28..32].copy_from_slice(&bad);
    let k = Kerx::parse(&bytes, 3).unwrap();
    assert_eq!(k.subtable_count(), 0);
}

#[test]
fn format6_index_past_grid_returns_zero() {
    // Lookups can yield indices beyond the declared row / column
    // count; the find path must fall through to "no rule = 0"
    // rather than panic on the cell read.
    let bytes = build_kerx_format6(
        3,
        &[0, 99, 1], // gid 1 -> row 99 (past row_count=2)
        &[0, 0, 1],
        &[vec![0, 0], vec![0, 7]],
    );
    let k = Kerx::parse(&bytes, 3).unwrap();
    assert_eq!(k.kern(1, 2), 0, "out-of-range row index falls through");
}

#[test]
fn format6_oversized_grid_drops_subtable() {
    // Build a valid 2x3 fmt6 then bump rowCount to 1000 so the
    // declared array (1000 * 3 * 2 = 6000 bytes) blows past the
    // subtable's payload. Parse must drop the subtable cleanly
    // rather than retain a doomed find() path.
    let mut bytes = build_kerx_format6(3, &[0, 1, 1], &[0, 0, 1], &[vec![0, 0], vec![0, 7]]);
    // Subtable starts at offset 8 (kerx header); fmt6 header at
    // offset 8 + 12 = 20; rowCount u16 at +4 = 24.
    bytes[24..26].copy_from_slice(&1000u16.to_be_bytes());
    let k = Kerx::parse(&bytes, 3).unwrap();
    assert_eq!(
        k.subtable_count(),
        0,
        "oversized rowCount x columnCount must drop the subtable"
    );
}
