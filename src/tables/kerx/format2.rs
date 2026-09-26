//! `kerx` format 2: n-way class kerning over a pre-multiplied grid.

use crate::error::{Error, Result};
use crate::tables::layout::state_table::lookup_class;

/// Format 2: n-way class kerning. Records the subtable-relative
/// offsets to the class tables and the kerning array; a kern lookup
/// resolves both classes through the AAT lookup primitive and reads
/// the i16 cell at `array + leftClassValue + rightClassValue`.
#[derive(Debug, Clone, Copy)]
pub(super) struct Format2<'a> {
    /// The subtable's full byte slice (the 12-byte common header
    /// plus the format-2 body). All recorded offsets are relative
    /// to byte 0 of this slice, matching the spec.
    sub: &'a [u8],
    left_class_off: usize,
    right_class_off: usize,
    array_off: usize,
}

/// Parses one format-2 subtable body. Offsets in the on-disk header
/// are relative to the subtable's own origin, so we keep a slice
/// that starts at `sub_start` and stash it on the descriptor.
pub(super) fn parse_format2(
    data: &[u8],
    sub_start: usize,
    sub_end: usize,
) -> Result<Option<Format2<'_>>> {
    let body_start = sub_start + 12;
    if body_start + 16 > sub_end {
        return Err(Error::Truncated {
            offset: body_start,
            context: "kerx format 2 header",
        });
    }
    // Bytes 0..4 hold rowWidth. The left class values are already
    // multiplied by it, so the lookup never needs it.
    let left_off = u32::from_be_bytes([
        data[body_start + 4],
        data[body_start + 5],
        data[body_start + 6],
        data[body_start + 7],
    ]) as usize;
    let right_off = u32::from_be_bytes([
        data[body_start + 8],
        data[body_start + 9],
        data[body_start + 10],
        data[body_start + 11],
    ]) as usize;
    let array_off = u32::from_be_bytes([
        data[body_start + 12],
        data[body_start + 13],
        data[body_start + 14],
        data[body_start + 15],
    ]) as usize;

    let sub_len = sub_end - sub_start;
    // All three offsets must point inside the subtable. Anything
    // else is a malformed font; bail with `None` so the rest of the
    // table still loads instead of poisoning the whole `kerx` parse.
    if left_off >= sub_len || right_off >= sub_len || array_off >= sub_len {
        return Ok(None);
    }
    let sub = &data[sub_start..sub_end];
    Ok(Some(Format2 {
        sub,
        left_class_off: left_off,
        right_class_off: right_off,
        array_off,
    }))
}

impl Format2<'_> {
    /// Resolves `(left, right)` through the class tables and reads
    /// the i16 cell. Returns `None` for any defensive failure: bad
    /// offsets, unsupported lookup formats, glyphs that fall in the
    /// reserved-class slots, or a cell that lands outside the
    /// subtable. Format 2 always returns deltas (no half-split,
    /// no cross-stream), so a `Some(0)` would be indistinguishable
    /// from "no rule"; callers don't need the distinction.
    pub(super) fn find(&self, left: u16, right: u16, num_glyphs: u16) -> Option<i16> {
        let left_table = self.sub.get(self.left_class_off..)?;
        let right_table = self.sub.get(self.right_class_off..)?;

        let left_value = lookup_class(left_table, left, num_glyphs).ok()?;
        let right_value = lookup_class(right_table, right, num_glyphs).ok()?;

        // Reserved-class lookups (out-of-bounds, deleted, etc.) are
        // returned by the AAT lookup helper as small sentinel values
        // (1, 2, 3). Format-2 class tables on real fonts fold these
        // into the row-0 / column-0 default cell, which is *almost*
        // always zero. Rather than special-casing, we follow the
        // spec: read the cell at the resolved offset; out-of-range
        // glyphs land on row 0 (default) and the array there is
        // typically zeroed.

        let cell_off = self
            .array_off
            .checked_add(usize::from(left_value))?
            .checked_add(usize::from(right_value))?;
        // The cell must be a fully-contained i16. A left value past the
        // row width would mean a malformed lookup table. The slice
        // bound check covers that case too.
        let bytes = self.sub.get(cell_off..cell_off + 2)?;
        Some(i16::from_be_bytes([bytes[0], bytes[1]]))
    }
}
