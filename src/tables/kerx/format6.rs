//! `kerx` format 6: simple n x m kerning array indexed by row and
//! column lookups.

use crate::error::{Error, Result};
use crate::tables::layout::state_table::lookup_class;

/// Format 6: simple n x m kerning array. Mirrors format 2 but the
/// row / column lookup tables yield direct indices (not pre-multiplied
/// byte offsets) and the array is sized by `rowCount * columnCount`.
/// All offsets are relative to the subtable origin (the 12-byte
/// common header).
#[derive(Debug, Clone, Copy)]
pub(super) struct Format6<'a> {
    sub: &'a [u8],
    row_count: u16,
    column_count: u16,
    row_index_off: usize,
    col_index_off: usize,
    array_off: usize,
}

/// Parses one format-6 subtable body. Body layout is documented at
/// the module level; offsets are relative to the subtable origin
/// (the 12-byte common header sits at byte 0). The "long values"
/// flag is rejected with `Ok(None)`: sigilbuzz only implements the
/// short-value variant, which is what shipping AAT fonts use.
pub(super) fn parse_format6(
    data: &[u8],
    sub_start: usize,
    sub_end: usize,
) -> Result<Option<Format6<'_>>> {
    let body_start = sub_start + 12;
    // 4 (flags) + 2 (rowCount) + 2 (columnCount) + 4 + 4 + 4 = 20 bytes
    // for the short-value header. The optional kerningVector u32 only
    // applies when long-values is set, which we reject anyway.
    if body_start + 20 > sub_end {
        return Err(Error::Truncated {
            offset: body_start,
            context: "kerx format 6 header",
        });
    }
    let flags = u32::from_be_bytes([
        data[body_start],
        data[body_start + 1],
        data[body_start + 2],
        data[body_start + 3],
    ]);
    // Bit 0 = long-values: cells become i32 instead of i16, and a
    // `kerningVector` offset follows the array. Real AAT fonts don't
    // ship this. Drop the subtable rather than half-implement it.
    if flags & 0x0000_0001 != 0 {
        return Ok(None);
    }
    let row_count = u16::from_be_bytes([data[body_start + 4], data[body_start + 5]]);
    let column_count = u16::from_be_bytes([data[body_start + 6], data[body_start + 7]]);
    let row_index_off = u32::from_be_bytes([
        data[body_start + 8],
        data[body_start + 9],
        data[body_start + 10],
        data[body_start + 11],
    ]) as usize;
    let col_index_off = u32::from_be_bytes([
        data[body_start + 12],
        data[body_start + 13],
        data[body_start + 14],
        data[body_start + 15],
    ]) as usize;
    let array_off = u32::from_be_bytes([
        data[body_start + 16],
        data[body_start + 17],
        data[body_start + 18],
        data[body_start + 19],
    ]) as usize;

    let sub_len = sub_end - sub_start;
    if row_index_off >= sub_len || col_index_off >= sub_len || array_off >= sub_len {
        return Ok(None);
    }
    // Validate that the declared `rowCount * columnCount` i16 grid
    // actually fits inside the subtable. A pathological font that
    // claims a 1000x1000 grid in a 64-byte payload would otherwise
    // parse cleanly and only fail per-cell at apply time, leaking the
    // garbage subtable into [`Kerx::subtable_count`] and forcing every
    // `kern()` lookup to walk a doomed find() path. Reject up front.
    let array_bytes = (row_count as usize)
        .checked_mul(column_count as usize)
        .and_then(|cells| cells.checked_mul(2));
    let Some(array_bytes) = array_bytes else {
        return Ok(None);
    };
    match array_off.checked_add(array_bytes) {
        Some(end) if end <= sub_len => {}
        _ => return Ok(None),
    }
    let sub = &data[sub_start..sub_end];
    Ok(Some(Format6 {
        sub,
        row_count,
        column_count,
        row_index_off,
        col_index_off,
        array_off,
    }))
}

impl Format6<'_> {
    /// Resolves `(left, right)` through the row / column index
    /// lookups and reads the i16 cell at
    /// `array + (row * column_count + column) * 2`. Returns `None`
    /// for any defensive failure (bad lookup format, glyph in a
    /// reserved class, cell outside the subtable). Matches format 2's
    /// "no rule = zero" convention so the kern path doesn't need a
    /// distinct sentinel.
    pub(super) fn find(&self, left: u16, right: u16, num_glyphs: u16) -> Option<i16> {
        let row_table = self.sub.get(self.row_index_off..)?;
        let col_table = self.sub.get(self.col_index_off..)?;

        let row = lookup_class(row_table, left, num_glyphs).ok()?;
        let col = lookup_class(col_table, right, num_glyphs).ok()?;

        // Reserved-class sentinels (1 / 2 / 3) typically fall on row
        // or column 0 in real fonts. Read the cell as-is and let
        // the array's natural zero slots handle the case. If either
        // index lands past the declared row / column count, treat the
        // pair as "no rule".
        if row >= self.row_count || col >= self.column_count {
            return None;
        }
        let cell_idx = usize::from(row) * usize::from(self.column_count) + usize::from(col);
        let cell_off = self.array_off.checked_add(cell_idx * 2)?;
        let bytes = self.sub.get(cell_off..cell_off + 2)?;
        Some(i16::from_be_bytes([bytes[0], bytes[1]]))
    }
}
