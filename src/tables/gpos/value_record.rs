//! OpenType `ValueRecord`: the variable-length positioning delta
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
//! | 0x0010 | x_placement_device    | `Offset16` -> Device / VariationIndex |
//! | 0x0020 | y_placement_device    | `Offset16` -> Device / VariationIndex |
//! | 0x0040 | x_advance_device      | `Offset16` -> Device / VariationIndex |
//! | 0x0080 | y_advance_device      | `Offset16` -> Device / VariationIndex |
//!
//! Each device offset is relative to the start of the enclosing
//! subtable (PairPos, SinglePos, ...). When the referenced table has
//! `deltaFormat = 0x8000` it is a `VariationIndex`: an outer/inner
//! pair indexing into GDEF's shared `ItemVariationStore`, which is
//! how variable-font kerning actually scales with axis coords. The
//! non-variation `Device` shape encodes per-ppem hinting deltas;
//! sigilbuzz parses them but does not apply them (we run in design
//! units).
//!
//! The record carries the raw `u16` offsets verbatim so the shaper
//! can resolve them against the subtable data slice at apply time,
//! see [`resolve_variation_delta`].

use crate::error::Result;
use crate::tables::layout::DeviceOrVariationIndex;
use crate::tables::parse::Reader;
use crate::tables::variation_store::ItemVariationStore;

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
/// that leave them set by ignoring them. Both `size` and `parse`
/// agree to skip those bits so the two stay in lockstep.
const DEFINED_BITS: u16 = X_PLACEMENT
    | Y_PLACEMENT
    | X_ADVANCE
    | Y_ADVANCE
    | X_PLACEMENT_DEVICE
    | Y_PLACEMENT_DEVICE
    | X_ADVANCE_DEVICE
    | Y_ADVANCE_DEVICE;

/// Decoded positioning delta. Absent fields default to zero; absent
/// device offsets default to zero (the spec's "no sub-table" sentinel).
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
    /// Raw `xPlaDeviceOffset` bytes from the record. `0` means the
    /// format bit was clear (or pointed at null). Resolve against
    /// the enclosing subtable via [`resolve_variation_delta`].
    pub x_placement_device_off: u16,
    /// Raw `yPlaDeviceOffset`.
    pub y_placement_device_off: u16,
    /// Raw `xAdvDeviceOffset`. The one kerning deltas actually use.
    pub x_advance_device_off: u16,
    /// Raw `yAdvDeviceOffset`.
    pub y_advance_device_off: u16,
}

impl ValueRecord {
    /// Returns the number of bytes a `ValueRecord` with the given
    /// format word occupies. Each set *defined* bit in `format` is
    /// one i16 (or Offset16, same size), so the size is `2 *
    /// popcount(format & DEFINED_BITS)`. Reserved bits are ignored
    /// because they must agree with [`ValueRecord::parse`], which also
    /// skips them, otherwise a malformed font that sets a reserved
    /// bit would drive `size` and `parse` out of lockstep and
    /// mis-align every subsequent record in an array.
    #[must_use]
    #[inline]
    pub const fn size(format: u16) -> usize {
        (format & DEFINED_BITS).count_ones() as usize * 2
    }

    /// Parses a `ValueRecord` from `data`, advancing `reader` past
    /// exactly `size(format)` bytes. Device-table offsets are
    /// captured verbatim so a later resolver can follow them against
    /// the enclosing subtable.
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
        if format & X_PLACEMENT_DEVICE != 0 {
            v.x_placement_device_off = reader.read_u16()?;
        }
        if format & Y_PLACEMENT_DEVICE != 0 {
            v.y_placement_device_off = reader.read_u16()?;
        }
        if format & X_ADVANCE_DEVICE != 0 {
            v.x_advance_device_off = reader.read_u16()?;
        }
        if format & Y_ADVANCE_DEVICE != 0 {
            v.y_advance_device_off = reader.read_u16()?;
        }
        Ok(v)
    }
}

/// Rounds the float delta the variation store returned to the nearest
/// design-unit integer. Matches the add-0.5/subtract-0.5 rule the
/// HVAR pipeline already uses so the two stay in byte-for-byte
/// lockstep.
#[must_use]
fn round_delta(delta: f32) -> i32 {
    // `as` saturates on overflow and maps NaN to zero, so no delta
    // the variation store returns can panic here.
    if delta >= 0.0 {
        (delta + 0.5) as i32
    } else {
        (delta - 0.5) as i32
    }
}

/// Resolves one `Device` / `VariationIndex` slot against the
/// enclosing subtable bytes and the shared `ItemVariationStore`.
///
/// - `device_off == 0` -> the value record did not carry this slot
///   (or the spec-blessed "null"). Returns `0`.
/// - The referenced table is a `Device` (per-ppem hinting). sigilbuzz
///   runs in design units, so return `0`.
/// - The referenced table is a `VariationIndex` and `store` is
///   `Some`. Returns the rounded delta at `coords`.
/// - The referenced table is a `VariationIndex` but GDEF did not
///   expose an IVS. The font is malformed or built for a hinting-
///   only consumer; return `0` so we fail soft.
/// - The offset is past the end or the header is malformed: return
///   `0`. The spec says an invalid Device table should degrade
///   quietly.
#[must_use]
pub fn resolve_variation_delta(
    subtable_bytes: &[u8],
    device_off: u16,
    store: Option<&ItemVariationStore<'_>>,
    coords: &[f32],
) -> i32 {
    if device_off == 0 {
        return 0;
    }
    let Ok(Some(dv)) = DeviceOrVariationIndex::parse_from(subtable_bytes, device_off) else {
        return 0;
    };
    match dv {
        DeviceOrVariationIndex::VariationIndex { outer, inner } => match store {
            Some(s) => round_delta(s.delta(outer, inner, coords)),
            None => 0,
        },
        // Device (hinting): the shaper has no ppem. The subtable is
        // parsed for completeness but contributes nothing here.
        DeviceOrVariationIndex::Device { .. } => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
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
    fn device_offsets_are_captured_verbatim() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&10i16.to_be_bytes()); // x_advance
        bytes.extend_from_slice(&0xDEADu16.to_be_bytes()); // x_advance_device
        let mut r = make_reader(&bytes);
        let v = ValueRecord::parse(&mut r, X_ADVANCE | X_ADVANCE_DEVICE).unwrap();
        assert_eq!(v.x_advance, 10);
        assert_eq!(v.x_advance_device_off, 0xDEAD);
        assert_eq!(v.y_advance_device_off, 0);
        assert_eq!(r.position(), 4);
    }

    #[test]
    fn all_four_device_offsets_parse_independently() {
        let mut bytes = Vec::new();
        // Positioning fields come first, then the four device slots.
        bytes.extend_from_slice(&0i16.to_be_bytes()); // x_placement
        bytes.extend_from_slice(&0i16.to_be_bytes()); // y_placement
        bytes.extend_from_slice(&0i16.to_be_bytes()); // x_advance
        bytes.extend_from_slice(&0i16.to_be_bytes()); // y_advance
        bytes.extend_from_slice(&0x0010u16.to_be_bytes()); // x_placement_device
        bytes.extend_from_slice(&0x0020u16.to_be_bytes()); // y_placement_device
        bytes.extend_from_slice(&0x0040u16.to_be_bytes()); // x_advance_device
        bytes.extend_from_slice(&0x0080u16.to_be_bytes()); // y_advance_device
        let format = X_PLACEMENT
            | Y_PLACEMENT
            | X_ADVANCE
            | Y_ADVANCE
            | X_PLACEMENT_DEVICE
            | Y_PLACEMENT_DEVICE
            | X_ADVANCE_DEVICE
            | Y_ADVANCE_DEVICE;
        let mut r = make_reader(&bytes);
        let v = ValueRecord::parse(&mut r, format).unwrap();
        assert_eq!(v.x_placement_device_off, 0x0010);
        assert_eq!(v.y_placement_device_off, 0x0020);
        assert_eq!(v.x_advance_device_off, 0x0040);
        assert_eq!(v.y_advance_device_off, 0x0080);
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
        // Cursor moved exactly as `size` predicted: no reserved
        // bit crept in to advance it further.
        assert_eq!(r.position(), ValueRecord::size(malformed));
    }

    // -------- resolve_variation_delta --------

    fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
        let raw = (v * 16384.0).round() as i16;
        out.extend_from_slice(&raw.to_be_bytes());
    }

    fn build_ivs_one_region_one_item(delta: i16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        let region_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // subtable count
        let subtable_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());

        let region_start = out.len() as u32;
        out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_start.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
        out.extend_from_slice(&1u16.to_be_bytes()); // regionCount
        write_f2dot14(&mut out, 0.0);
        write_f2dot14(&mut out, 1.0);
        write_f2dot14(&mut out, 1.0);

        let sub_start = out.len() as u32;
        out[subtable_slot..subtable_slot + 4].copy_from_slice(&sub_start.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // itemCount
        out.extend_from_slice(&1u16.to_be_bytes()); // wordDeltaCount
        out.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
        out.extend_from_slice(&0u16.to_be_bytes()); // region index 0
        out.extend_from_slice(&delta.to_be_bytes());
        out
    }

    /// Builds a subtable stub that has a VariationIndex table at
    /// `device_off` pointing at `(outer, inner)`.
    fn build_subtable_with_variation_index(device_off: u16, outer: u16, inner: u16) -> Vec<u8> {
        let mut out = vec![0u8; device_off as usize];
        out.extend_from_slice(&outer.to_be_bytes());
        out.extend_from_slice(&inner.to_be_bytes());
        out.extend_from_slice(&0x8000u16.to_be_bytes()); // deltaFormat = VARIATION_INDEX
        out
    }

    #[test]
    fn resolve_zero_offset_is_zero_delta() {
        // A ValueRecord that never set the device bit leaves the slot
        // as 0; the resolver must not follow it.
        let out = resolve_variation_delta(&[], 0, None, &[]);
        assert_eq!(out, 0);
    }

    #[test]
    fn resolve_variation_index_reads_from_store() {
        // delta = 80 at coord 1.0 -> rounded to 80.
        let ivs = build_ivs_one_region_one_item(80);
        let store = ItemVariationStore::parse(&ivs).unwrap();
        let subtable = build_subtable_with_variation_index(8, 0, 0);
        let got = resolve_variation_delta(&subtable, 8, Some(&store), &[1.0]);
        assert_eq!(got, 80);
    }

    #[test]
    fn resolve_variation_index_scales_with_coord() {
        // delta = 100 at coord 0.25 -> 25, at coord 0.75 -> 75.
        let ivs = build_ivs_one_region_one_item(100);
        let store = ItemVariationStore::parse(&ivs).unwrap();
        let subtable = build_subtable_with_variation_index(16, 0, 0);
        assert_eq!(
            resolve_variation_delta(&subtable, 16, Some(&store), &[0.25]),
            25
        );
        assert_eq!(
            resolve_variation_delta(&subtable, 16, Some(&store), &[0.75]),
            75
        );
    }

    #[test]
    fn resolve_without_ivs_yields_zero_even_for_variation_index() {
        // Font malformed or hinting-only: GDEF has no IVS. We refuse
        // to materialize a delta without a store.
        let subtable = build_subtable_with_variation_index(8, 0, 0);
        assert_eq!(resolve_variation_delta(&subtable, 8, None, &[0.5]), 0);
    }

    #[test]
    fn resolve_device_table_yields_zero_because_shaper_has_no_ppem() {
        // Build a subtable with a Device table (format 3) at offset 4.
        let mut subtable = vec![0u8; 4];
        subtable.extend_from_slice(&8u16.to_be_bytes()); // startSize
        subtable.extend_from_slice(&16u16.to_be_bytes()); // endSize
        subtable.extend_from_slice(&3u16.to_be_bytes()); // deltaFormat = Device
        subtable.extend_from_slice(&0u16.to_be_bytes()); // one delta word
        assert_eq!(resolve_variation_delta(&subtable, 4, None, &[]), 0);
    }

    #[test]
    fn resolve_past_end_is_zero_delta() {
        // Malformed font: offset dangles past the subtable. Degrade
        // silently instead of panicking.
        let subtable = [0u8; 4];
        assert_eq!(resolve_variation_delta(&subtable, 100, None, &[]), 0);
    }
}
