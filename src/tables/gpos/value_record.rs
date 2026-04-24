//! OpenType `ValueRecord` — the variable-length positioning delta
//! used throughout GPOS.
//!
//! A `ValueRecord`'s shape is controlled by a separate `u16` flags
//! word (the "value format") that lives in the enclosing subtable.
//! Each bit enables one field; the record is laid out in field
//! order, so a record with `valueFormat = 0x0005` carries an
//! `x_placement` then an `x_advance` and nothing else.
//!
//! | Bit    | Field                 | Type       |
//! |--------|-----------------------|------------|
//! | 0x0001 | x_placement           | `i16`      |
//! | 0x0002 | y_placement           | `i16`      |
//! | 0x0004 | x_advance             | `i16`      |
//! | 0x0008 | y_advance             | `i16`      |
//! | 0x0010 | x_placement_device    | `Offset16` (ignored) |
//! | 0x0020 | y_placement_device    | `Offset16` (ignored) |
//! | 0x0040 | x_advance_device      | `Offset16` (ignored) |
//! | 0x0080 | y_advance_device      | `Offset16` (ignored) |
//!
//! Device tables encode ppem-specific deltas for small-size hinting.
//! sigilbuzz does not consume them today — the offsets are parsed
//! and discarded so the record size is still computed correctly but
//! no per-ppem adjustment is applied. The deltas they carry are
//! usually under a pixel; a later milestone can wire them in if a
//! real font needs them.

use crate::error::Result;
use crate::tables::parse::Reader;

/// Bit for the `x_placement` i16 field.
pub const X_PLACEMENT: u16 = 0x0001;
/// Bit for the `y_placement` i16 field.
pub const Y_PLACEMENT: u16 = 0x0002;
/// Bit for the `x_advance` i16 field.
pub const X_ADVANCE: u16 = 0x0004;
/// Bit for the `y_advance` i16 field.
pub const Y_ADVANCE: u16 = 0x0008;
/// Bit for the `x_placement_device` offset field.
pub const X_PLACEMENT_DEVICE: u16 = 0x0010;
/// Bit for the `y_placement_device` offset field.
pub const Y_PLACEMENT_DEVICE: u16 = 0x0020;
/// Bit for the `x_advance_device` offset field.
pub const X_ADVANCE_DEVICE: u16 = 0x0040;
/// Bit for the `y_advance_device` offset field.
pub const Y_ADVANCE_DEVICE: u16 = 0x0080;

/// Bitmask of every field the spec defines. Bits outside this range
/// are reserved and must be zero; sigilbuzz tolerates malformed fonts
/// that leave them set by ignoring them — both `size` and `parse`
/// agree to skip those bits so the two stay in lockstep.
const DEFINED_BITS: u16 = X_PLACEMENT
    | Y_PLACEMENT
    | X_ADVANCE
    | Y_ADVANCE
    | X_PLACEMENT_DEVICE
    | Y_PLACEMENT_DEVICE
    | X_ADVANCE_DEVICE
    | Y_ADVANCE_DEVICE;

/// Decoded positioning delta. Absent fields default to zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValueRecord {
    /// Horizontal placement adjustment.
    pub x_placement: i16,
    /// Vertical placement adjustment.
    pub y_placement: i16,
    /// Horizontal advance adjustment (kerning lives here).
    pub x_advance: i16,
    /// Vertical advance adjustment.
    pub y_advance: i16,
}

impl ValueRecord {
    /// Returns the number of bytes a `ValueRecord` with the given
    /// format word occupies. Each set *defined* bit in `format` is
    /// one i16 (or Offset16, same size), so the size is `2 *
    /// popcount(format & DEFINED_BITS)`. Reserved bits are ignored
    /// — they must agree with [`ValueRecord::parse`], which also
    /// skips them, otherwise a malformed font that sets a reserved
    /// bit would drive `size` and `parse` out of lockstep and
    /// mis-align every subsequent record in an array.
    #[must_use]
    #[inline]
    pub const fn size(format: u16) -> usize {
        (format & DEFINED_BITS).count_ones() as usize * 2
    }

    /// Parses a `ValueRecord` from `data`, advancing `reader` past
    /// exactly `size(format)` bytes. Device-table offsets are read
    /// (so the reader ends at the right place) but discarded.
    pub fn parse(reader: &mut Reader<'_>, format: u16) -> Result<Self> {
        let mut v = Self::default();
        if format & X_PLACEMENT != 0 {
            v.x_placement = reader.read_i16()?;
        }
        if format & Y_PLACEMENT != 0 {
            v.y_placement = reader.read_i16()?;
        }
        if format & X_ADVANCE != 0 {
            v.x_advance = reader.read_i16()?;
        }
        if format & Y_ADVANCE != 0 {
            v.y_advance = reader.read_i16()?;
        }
        // Device offsets — read to advance the cursor, then ignore.
        if format & X_PLACEMENT_DEVICE != 0 {
            let _ = reader.read_u16()?;
        }
        if format & Y_PLACEMENT_DEVICE != 0 {
            let _ = reader.read_u16()?;
        }
        if format & X_ADVANCE_DEVICE != 0 {
            let _ = reader.read_u16()?;
        }
        if format & Y_ADVANCE_DEVICE != 0 {
            let _ = reader.read_u16()?;
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn make_reader(bytes: &[u8]) -> Reader<'_> {
        Reader::new(bytes)
    }

    #[test]
    fn size_is_two_times_popcount() {
        assert_eq!(ValueRecord::size(0), 0);
        assert_eq!(ValueRecord::size(X_ADVANCE), 2);
        assert_eq!(ValueRecord::size(X_ADVANCE | Y_ADVANCE), 4);
        assert_eq!(ValueRecord::size(0xFF), 16);
    }

    #[test]
    fn empty_format_parses_to_default() {
        let bytes = [];
        let mut r = make_reader(&bytes);
        let v = ValueRecord::parse(&mut r, 0).unwrap();
        assert_eq!(v, ValueRecord::default());
    }

    #[test]
    fn x_advance_only_reads_one_i16() {
        let bytes = [0xFF, 0xE0]; // -32
        let mut r = make_reader(&bytes);
        let v = ValueRecord::parse(&mut r, X_ADVANCE).unwrap();
        assert_eq!(v.x_advance, -32);
        assert_eq!(v.x_placement, 0);
    }

    #[test]
    fn all_positioning_fields_read_in_order() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1i16.to_be_bytes()); // x_placement
        bytes.extend_from_slice(&2i16.to_be_bytes()); // y_placement
        bytes.extend_from_slice(&3i16.to_be_bytes()); // x_advance
        bytes.extend_from_slice(&4i16.to_be_bytes()); // y_advance
        let mut r = make_reader(&bytes);
        let v =
            ValueRecord::parse(&mut r, X_PLACEMENT | Y_PLACEMENT | X_ADVANCE | Y_ADVANCE).unwrap();
        assert_eq!(v.x_placement, 1);
        assert_eq!(v.y_placement, 2);
        assert_eq!(v.x_advance, 3);
        assert_eq!(v.y_advance, 4);
    }

    #[test]
    fn device_offsets_are_consumed_but_ignored() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&10i16.to_be_bytes()); // x_advance
        bytes.extend_from_slice(&0xDEADu16.to_be_bytes()); // x_advance_device
        let mut r = make_reader(&bytes);
        let v = ValueRecord::parse(&mut r, X_ADVANCE | X_ADVANCE_DEVICE).unwrap();
        assert_eq!(v.x_advance, 10);
        assert_eq!(r.position(), 4);
    }

    #[test]
    fn truncated_body_surfaces_as_error() {
        let bytes = [0x00]; // format wants i16 but only one byte
        let mut r = make_reader(&bytes);
        assert!(ValueRecord::parse(&mut r, X_ADVANCE).is_err());
    }

    #[test]
    fn reserved_bits_are_ignored_by_size_and_parse_consistently() {
        // Bits 0x0100..=0x8000 are reserved. A malformed font that
        // leaves any of them set must not drive `size` past what
        // `parse` actually consumes, otherwise array strides in
        // PairPos / SinglePos format 2 would mis-align every record
        // after the first.
        let malformed = X_ADVANCE | 0x0100 | 0x8000;
        assert_eq!(ValueRecord::size(malformed), ValueRecord::size(X_ADVANCE));

        let bytes = [0xFF, 0xE0]; // -32 as i16, exactly what X_ADVANCE consumes
        let mut r = make_reader(&bytes);
        let v = ValueRecord::parse(&mut r, malformed).unwrap();
        assert_eq!(v.x_advance, -32);
        // Cursor moved exactly as `size` predicted — no reserved
        // bit crept in to advance it further.
        assert_eq!(r.position(), ValueRecord::size(malformed));
    }
}
