//! The ValueRecord side of the GPOS device-slot walk: SinglePos and
//! PairPos (formats 1 and 2) records, with the offset base each one
//! measures its Device and VariationIndex tables from.

use alloc::vec::Vec;

use super::{read_u16, DeviceSlot, SlotVisitor};

/// Defined ValueRecord format bits: bits 0x0001..=0x0080. Mirrors the
/// `DEFINED_BITS` constant in `sigilbuzz::tables::gpos::value_record`.
const VALUE_FORMAT_DEFINED: u16 = 0x00FF;

/// Bit offsets within a ValueRecord, in the order the spec lays them
/// out. The first four (`x_placement` ... `y_advance`) are the static
/// i16 fields; the next four (`x_placement_device` ...
/// `y_advance_device`) are the Offset16 sub-offsets that point at
/// Device / VariationIndex tables relative to the record's parent
/// table (see the module docs for which table that is).
const VR_X_PLACEMENT: u16 = 0x0001;
const VR_Y_PLACEMENT: u16 = 0x0002;
pub(super) const VR_X_ADVANCE: u16 = 0x0004;
const VR_Y_ADVANCE: u16 = 0x0008;
const VR_X_PLACEMENT_DEVICE: u16 = 0x0010;
const VR_Y_PLACEMENT_DEVICE: u16 = 0x0020;
pub(super) const VR_X_ADVANCE_DEVICE: u16 = 0x0040;
const VR_Y_ADVANCE_DEVICE: u16 = 0x0080;

/// Number of bytes a `ValueRecord` with the given format word
/// occupies. Each set defined bit is one i16 (or Offset16, same size).
#[inline]
pub(super) const fn value_record_size(format: u16) -> usize {
    (format & VALUE_FORMAT_DEFINED).count_ones() as usize * 2
}

/// Reports every device slot of the ValueRecord at `vr_pos` for the
/// given `format` word. `base` is the start of the record's parent
/// table (the subtable, or the PairSet for PairPos format 1).
///
/// The ValueRecord's static fields and device offsets follow the
/// format-driven layout:
///
/// ```text
///   i16  x_placement              (if 0x0001)
///   i16  y_placement              (if 0x0002)
///   i16  x_advance                (if 0x0004)
///   i16  y_advance                (if 0x0008)
///   o16  x_placement_device       (if 0x0010)
///   o16  y_placement_device       (if 0x0020)
///   o16  x_advance_device         (if 0x0040)
///   o16  y_advance_device         (if 0x0080)
/// ```
///
/// Each device-offset bit is paired with one static field bit:
/// `0x0010` with `0x0001`, `0x0020` with `0x0002`, `0x0040` with
/// `0x0004`, and `0x0080` with `0x0008`. When a device-offset bit is
/// set but the paired static field bit is not, the slot is reported
/// with `field: None`.
fn visit_value_record(
    buf: &mut [u8],
    base: usize,
    vr_pos: usize,
    format: u16,
    visit: &mut SlotVisitor<'_>,
) {
    let mut cursor = vr_pos;
    let mut fields: [Option<usize>; 4] = [None; 4];
    for (i, bit) in [VR_X_PLACEMENT, VR_Y_PLACEMENT, VR_X_ADVANCE, VR_Y_ADVANCE]
        .into_iter()
        .enumerate()
    {
        if format & bit != 0 {
            fields[i] = Some(cursor);
            cursor += 2;
        }
    }
    for (i, bit) in [
        VR_X_PLACEMENT_DEVICE,
        VR_Y_PLACEMENT_DEVICE,
        VR_X_ADVANCE_DEVICE,
        VR_Y_ADVANCE_DEVICE,
    ]
    .into_iter()
    .enumerate()
    {
        if format & bit != 0 {
            visit(
                buf,
                DeviceSlot {
                    base,
                    field: fields[i],
                    slot: cursor,
                },
            );
            cursor += 2;
        }
    }
}

// ---------------------------------------------------------------------------
// ValueRecord walk (SinglePos / PairPos)
// ---------------------------------------------------------------------------

/// Walks every ValueRecord device slot in a SinglePos subtable
/// starting at `sub_off` within `gpos_buf`. Device offsets are
/// relative to the subtable.
pub(super) fn walk_single_pos(gpos_buf: &mut [u8], sub_off: usize, visit: &mut SlotVisitor<'_>) {
    let Some(sub) = gpos_buf.get_mut(sub_off..) else {
        return;
    };
    let (Some(format), Some(value_format)) = (read_u16(sub, 0), read_u16(sub, 4)) else {
        return;
    };
    if value_format & 0x00F0 == 0 {
        // No device-offset fields: nothing to report.
        return;
    }
    let stride = value_record_size(value_format);
    let (first, count) = match format {
        // One shared ValueRecord right after the 6-byte header.
        1 => (6usize, 1usize),
        // Per-glyph array after the 8-byte header.
        2 => match read_u16(sub, 6) {
            Some(n) => (8, n as usize),
            None => return,
        },
        _ => return,
    };
    if sub.len() < first + count * stride {
        return;
    }
    for i in 0..count {
        visit_value_record(sub, 0, first + i * stride, value_format, visit);
    }
}

/// Walks every ValueRecord device slot in a PairPos subtable starting
/// at `sub_off` within `gpos_buf`.
pub(super) fn walk_pair_pos(gpos_buf: &mut [u8], sub_off: usize, visit: &mut SlotVisitor<'_>) {
    let Some(sub) = gpos_buf.get_mut(sub_off..) else {
        return;
    };
    match read_u16(sub, 0) {
        Some(1) => walk_pair_pos_format1(sub, visit),
        Some(2) => walk_pair_pos_format2(sub, visit),
        _ => {}
    }
}

/// PairPos format 1. The ValueRecords live inside PairSet tables and
/// their device offsets are relative to the PairSet, not the subtable.
fn walk_pair_pos_format1(sub: &mut [u8], visit: &mut SlotVisitor<'_>) {
    if sub.len() < 10 {
        return;
    }
    let (Some(vf1), Some(vf2), Some(pair_set_count)) =
        (read_u16(sub, 4), read_u16(sub, 6), read_u16(sub, 8))
    else {
        return;
    };
    if (vf1 | vf2) & 0x00F0 == 0 {
        return;
    }
    let v1_size = value_record_size(vf1);
    let pvr_size = 2 + v1_size + value_record_size(vf2);
    let set_offsets_off = 10usize;
    if sub.len() < set_offsets_off + pair_set_count as usize * 2 {
        return;
    }
    let set_offs: Vec<usize> = (0..pair_set_count as usize)
        .filter_map(|i| read_u16(sub, set_offsets_off + i * 2).map(usize::from))
        .collect();
    for set_off in set_offs {
        let Some(pair_value_count) = read_u16(sub, set_off).map(usize::from) else {
            continue;
        };
        if set_off + 2 + pair_value_count * pvr_size > sub.len() {
            continue;
        }
        for j in 0..pair_value_count {
            // ValueRecord1 starts after the 2-byte secondGlyph.
            let vr1_pos = set_off + 2 + j * pvr_size + 2;
            visit_value_record(sub, set_off, vr1_pos, vf1, visit);
            visit_value_record(sub, set_off, vr1_pos + v1_size, vf2, visit);
        }
    }
}

/// PairPos format 2. The class matrix sits inline in the subtable and
/// its device offsets are relative to the subtable.
fn walk_pair_pos_format2(sub: &mut [u8], visit: &mut SlotVisitor<'_>) {
    if sub.len() < 16 {
        return;
    }
    let (Some(vf1), Some(vf2), Some(class1_count), Some(class2_count)) = (
        read_u16(sub, 4),
        read_u16(sub, 6),
        read_u16(sub, 12),
        read_u16(sub, 14),
    ) else {
        return;
    };
    if (vf1 | vf2) & 0x00F0 == 0 {
        return;
    }
    let v1_size = value_record_size(vf1);
    let cell_size = v1_size + value_record_size(vf2);
    let cells = class1_count as usize * class2_count as usize;
    let records_off = 16usize;
    if sub.len() < records_off + cells * cell_size {
        return;
    }
    for k in 0..cells {
        let vr1_pos = records_off + k * cell_size;
        visit_value_record(sub, 0, vr1_pos, vf1, visit);
        visit_value_record(sub, 0, vr1_pos + v1_size, vf2, visit);
    }
}
