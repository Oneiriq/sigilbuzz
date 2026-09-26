//! GPOS variation-bake: folds `VariationIndex` deltas into static
//! `ValueRecord` fields and `Anchor` coordinates at a chosen coord
//! vector. (#175)
//!
//! # Why
//!
//! Variable-font GPOS subtables carry per-field `Device` /
//! `VariationIndex` sub-offsets. When `deltaFormat == 0x8000` the
//! offset names an `(outer, inner)` row in `GDEF.ItemVariationStore`
//! whose region-weighted delta scales the static field at the user's
//! axis coords. Instancing (the operation that collapses a variable
//! font to a static one at a chosen instance) cannot leave those
//! references intact: the `ItemVariationStore` they point at is
//! dropped along with the rest of the variable-font surface.
//!
//! For every supported lookup type we walk the subtable's device
//! slots, look up each `VariationIndex` in the source
//! `ItemVariationStore`, resolve the delta at the bake's `coords`, fold
//! the rounded result into the static field (saturating-add), and zero
//! the offset slot so a downstream consumer cannot follow it.
//!
//! # Offset bases
//!
//! A `Device` / `VariationIndex` offset is measured from the immediate
//! parent table, which is not always the lookup subtable:
//!
//! - ValueRecord in SinglePos, or in the PairPos format 2 class
//!   matrix: the subtable.
//! - ValueRecord in a PairPos format 1 `PairValueRecord`: the
//!   `PairSet` table.
//! - AnchorFormat3 `xDeviceOffset` / `yDeviceOffset`: the `Anchor`
//!   table.
//!
//! The walk records that base in every [`DeviceSlot`] it reports, so
//! the fold and the other visitors never assume the subtable start.
//!
//! # Coverage
//!
//! - **PairPos format 1**: explicit pair entries, 2 ValueRecords per
//!   `PairValueRecord`.
//! - **PairPos format 2**: class-pair matrix, 2 ValueRecords per
//!   `Class1Record * Class2Record` cell.
//! - **SinglePos format 1**: uniform `ValueRecord` shared across the
//!   coverage.
//! - **SinglePos format 2**: per-coverage-entry `ValueRecord` array.
//! - **CursivePos**: per-glyph `EntryExitRecord` with two anchor
//!   offsets. Anchor format 3 carries x/yDevice slots that the bake
//!   resolves the same way ValueRecord device slots are resolved.
//! - **MarkBasePos / MarkMarkPos**: `MarkArray` + `BaseArray`
//!   (`Mark2Array` for type 6) anchor matrices. Same anchor walk.
//! - **MarkLigPos**: `MarkArray` + `LigatureArray` of per-component
//!   anchor matrices. Same anchor walk per component.
//!
//! Lookup types we do not bake (`Context`, `ChainContext`) ride through
//! verbatim. Only their parent table bytes are copied; nested subtables
//! we do not understand are not touched.
//!
//! # Determinism
//!
//! The bake patches a writable copy of the source GPOS bytes in place.
//! Every `VariationIndex` resolution rounds through the same
//! `add-0.5/subtract-0.5` rule as the HVAR / MVAR bakes so the three
//! stay in byte-for-byte lockstep. Saturating addition guards against
//! ValueRecord field overflow on extreme coords.

use alloc::vec::Vec;

use sigilbuzz::tables::variation_store::ItemVariationStore;

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
const VR_X_ADVANCE: u16 = 0x0004;
const VR_Y_ADVANCE: u16 = 0x0008;
const VR_X_PLACEMENT_DEVICE: u16 = 0x0010;
const VR_Y_PLACEMENT_DEVICE: u16 = 0x0020;
const VR_X_ADVANCE_DEVICE: u16 = 0x0040;
const VR_Y_ADVANCE_DEVICE: u16 = 0x0080;

/// `deltaFormat` sentinel that turns a Device-shaped offset into a
/// `VariationIndex`. Mirrors
/// `sigilbuzz::tables::layout::device::VARIATION_INDEX_DELTA_FORMAT`
/// and is repeated here so the bake does not pull a runtime parser dep on
/// the layout module.
pub(crate) const VARIATION_INDEX_DELTA_FORMAT: u16 = 0x8000;

/// Number of bytes a `ValueRecord` with the given format word
/// occupies. Each set defined bit is one i16 (or Offset16, same size).
#[inline]
const fn value_record_size(format: u16) -> usize {
    (format & VALUE_FORMAT_DEFINED).count_ones() as usize * 2
}

/// Rounds the variation store's float delta to the nearest design-unit
/// integer. Matches the rule the HVAR/MVAR/value_record pipelines use
/// so the four stay in byte-for-byte lockstep.
#[must_use]
#[inline]
fn round_delta(delta: f32) -> i32 {
    #[allow(clippy::cast_possible_truncation)]
    if delta >= 0.0 {
        (delta + 0.5) as i32
    } else {
        (delta - 0.5) as i32
    }
}

fn read_u16(buf: &[u8], pos: usize) -> Option<u16> {
    let bytes = buf.get(pos..pos.checked_add(2)?)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn write_u16(buf: &mut [u8], pos: usize, value: u16) {
    if let Some(bytes) = pos.checked_add(2).and_then(|end| buf.get_mut(pos..end)) {
        bytes.copy_from_slice(&value.to_be_bytes());
    }
}

/// One `Device` / `VariationIndex` offset slot reported by the GPOS
/// walk. Every position is a byte index into the buffer the visitor
/// receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeviceSlot {
    /// Start of the table the slot's offset is measured from: the
    /// Anchor, the PairSet, or the subtable (see the module docs).
    pub base: usize,
    /// The static i16 field the slot adjusts. `None` when a
    /// ValueRecord sets a device bit without the paired value bit.
    pub field: Option<usize>,
    /// The Offset16 slot itself.
    pub slot: usize,
}

impl DeviceSlot {
    /// Position of the Device / VariationIndex table the slot names,
    /// or `None` when the slot is null (offset 0) or unreadable.
    pub(crate) fn target(self, buf: &[u8]) -> Option<usize> {
        let raw = read_u16(buf, self.slot)?;
        if raw == 0 {
            return None;
        }
        self.base.checked_add(raw as usize)
    }

    /// `deltaFormat` of the referenced table, or `None` when the slot
    /// is null or the table header does not fit in `buf`.
    pub(crate) fn delta_format(self, buf: &[u8]) -> Option<u16> {
        read_u16(buf, self.target(buf)?.checked_add(4)?)
    }

    /// Zeros the offset slot.
    pub(crate) fn clear(self, buf: &mut [u8]) {
        write_u16(buf, self.slot, 0);
    }
}

/// Visitor the GPOS walk calls once per device slot it finds.
pub(crate) type SlotVisitor<'v> = dyn FnMut(&mut [u8], DeviceSlot) + 'v;

/// Folds the `VariationIndex` a device slot references into its static
/// i16 field and zeros the offset slot.
///
/// Behavior:
///
/// - If the offset slot is zero (the spec's "absent" sentinel): no-op.
/// - If the offset points past the buffer, the header is malformed,
///   or the referenced table is a `Device` (per-ppem hinting, not a
///   `VariationIndex`): zero the offset slot and leave the static
///   field alone. Folding a Device into the static field would
///   silently warp design-unit metrics; the "ship as static" intent is
///   to preserve them.
/// - If the referenced table is a `VariationIndex` and `store` is
///   `Some`, look up `(outer, inner)`, resolve at `coords`, round, and
///   saturating-add to the static field. Then zero the offset slot.
/// - If the referenced table is a `VariationIndex` but `store` is
///   `None` (font has GPOS variations but no GDEF.IVS, malformed):
///   zero the offset slot, leave the static field alone.
fn fold_one_field(
    buf: &mut [u8],
    slot: DeviceSlot,
    store: Option<&ItemVariationStore<'_>>,
    coords: &[f32],
) {
    let Some(target) = slot.target(buf) else {
        // Null slot: nothing to fold or sever.
        return;
    };
    let delta_format = slot.delta_format(buf);
    // Always zero the offset, even when we can't resolve the delta:
    // the GDEF.IVS prune that follows would leave it pointing at an
    // orphan otherwise.
    slot.clear(buf);

    if delta_format != Some(VARIATION_INDEX_DELTA_FORMAT) {
        // Plain Device, or a header past the end: leave the static
        // field alone.
        return;
    }
    let (Some(outer), Some(inner)) = (read_u16(buf, target), read_u16(buf, target + 2)) else {
        return;
    };
    let Some(store) = store else {
        return;
    };
    let scaled = round_delta(store.delta(outer, inner, coords));
    let Some(field) = slot.field else {
        return;
    };
    let Some(cur) = read_u16(buf, field) else {
        return;
    };
    if scaled == 0 {
        return;
    }
    let new = i32::from(cur as i16)
        .saturating_add(scaled)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX));
    #[allow(clippy::cast_possible_truncation)]
    let new_i16 = new as i16;
    write_u16(buf, field, new_i16 as u16);
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
// Anchor walk (Mark*/Cursive)
// ---------------------------------------------------------------------------

/// Reports the device slots of the Anchor at `anchor_off` (relative to
/// `buf`).
///
/// AnchorFormat layout per the OpenType spec:
///
/// ```text
///   format 1: u16 format=1, i16 xCoord, i16 yCoord                (6 B)
///   format 2: u16 format=2, i16 xCoord, i16 yCoord, u16 anchorPt  (8 B)
///   format 3: u16 format=3, i16 xCoord, i16 yCoord,
///             o16 xDeviceOffset, o16 yDeviceOffset               (10 B)
/// ```
///
/// Only format 3 carries device slots, and its `xDeviceOffset` /
/// `yDeviceOffset` are measured from the start of the Anchor table,
/// not from the enclosing subtable. Formats 1 and 2 have no variation
/// surface: early return. An `anchor_off` of 0 is the spec's "absent"
/// sentinel.
fn visit_anchor(buf: &mut [u8], anchor_off: usize, visit: &mut SlotVisitor<'_>) {
    if anchor_off == 0 || read_u16(buf, anchor_off) != Some(3) {
        // Format 1 / 2: no Device/VariationIndex slots. Format 0 or
        // anything > 3 is malformed; ride through.
        return;
    }
    if anchor_off.saturating_add(10) > buf.len() {
        return;
    }
    for (field, slot) in [(2, 6), (4, 8)] {
        visit(
            buf,
            DeviceSlot {
                base: anchor_off,
                field: Some(anchor_off + field),
                slot: anchor_off + slot,
            },
        );
    }
}

/// Walks every Anchor in a CursivePos subtable starting at `sub_off`
/// within `gpos_buf`.
///
/// Layout (CursivePos format 1):
/// ```text
///   u16 posFormat = 1
///   u16 coverageOffset
///   u16 entryExitCount
///   EntryExitRecord[entryExitCount]:
///     u16 entryAnchorOffset   (relative to the subtable)
///     u16 exitAnchorOffset    (relative to the subtable)
/// ```
fn walk_cursive_pos(gpos_buf: &mut [u8], sub_off: usize, visit: &mut SlotVisitor<'_>) {
    let Some(sub) = gpos_buf.get_mut(sub_off..) else {
        return;
    };
    if read_u16(sub, 0) != Some(1) {
        return;
    }
    let Some(entry_exit_count) = read_u16(sub, 4).map(usize::from) else {
        return;
    };
    let records_off = 6usize;
    if sub.len() < records_off + entry_exit_count * 4 {
        return;
    }
    // Collect anchor offsets first so the visitor may mutate freely.
    let anchor_offs: Vec<usize> = (0..entry_exit_count * 2)
        .filter_map(|i| read_u16(sub, records_off + i * 2).map(usize::from))
        .collect();
    for off in anchor_offs {
        visit_anchor(sub, off, visit);
    }
}

/// Walks every Anchor in a `MarkArray` at `mark_array_off` (relative to
/// `sub`).
///
/// MarkArray layout:
/// ```text
///   u16 markCount
///   MarkRecord[markCount]:
///     u16 class
///     u16 markAnchorOffset (relative to MarkArray start)
/// ```
fn walk_mark_array(sub: &mut [u8], mark_array_off: usize, visit: &mut SlotVisitor<'_>) {
    let Some(mark_count) = read_u16(sub, mark_array_off).map(usize::from) else {
        return;
    };
    let records_off = mark_array_off + 2;
    if records_off + mark_count * 4 > sub.len() {
        return;
    }
    let anchor_offs: Vec<usize> = (0..mark_count)
        .filter_map(|i| read_u16(sub, records_off + i * 4 + 2).map(usize::from))
        .filter(|&rel| rel != 0)
        .map(|rel| mark_array_off + rel)
        .collect();
    for off in anchor_offs {
        visit_anchor(sub, off, visit);
    }
}

/// Walks every Anchor in a `BaseArray` (or `Mark2Array`, same shape).
///
/// BaseArray layout:
/// ```text
///   u16 baseCount
///   BaseRecord[baseCount]:
///     u16 baseAnchorOffsets[markClassCount]   (each relative to BaseArray)
/// ```
fn walk_base_or_mark2_array(
    sub: &mut [u8],
    base_array_off: usize,
    mark_class_count: usize,
    visit: &mut SlotVisitor<'_>,
) {
    let Some(base_count) = read_u16(sub, base_array_off).map(usize::from) else {
        return;
    };
    let records_off = base_array_off + 2;
    let total = base_count * mark_class_count;
    if records_off + total * 2 > sub.len() {
        return;
    }
    let anchor_offs: Vec<usize> = (0..total)
        .filter_map(|i| read_u16(sub, records_off + i * 2).map(usize::from))
        .filter(|&rel| rel != 0)
        .map(|rel| base_array_off + rel)
        .collect();
    for off in anchor_offs {
        visit_anchor(sub, off, visit);
    }
}

/// Walks every Anchor in a MarkBasePos or MarkMarkPos subtable starting
/// at `sub_off`. Both share one header shape:
///
/// ```text
///   u16 posFormat = 1
///   u16 markCoverageOffset     (mark1CoverageOffset for type 6)
///   u16 baseCoverageOffset     (mark2CoverageOffset for type 6)
///   u16 markClassCount
///   o16 markArrayOffset        (mark1ArrayOffset for type 6)
///   o16 baseArrayOffset        (mark2ArrayOffset for type 6)
/// ```
fn walk_mark_base_or_mark_pos(gpos_buf: &mut [u8], sub_off: usize, visit: &mut SlotVisitor<'_>) {
    let Some(sub) = gpos_buf.get_mut(sub_off..) else {
        return;
    };
    if sub.len() < 12 || read_u16(sub, 0) != Some(1) {
        return;
    }
    let (Some(mcc), Some(mark_array_off), Some(base_array_off)) =
        (read_u16(sub, 6), read_u16(sub, 8), read_u16(sub, 10))
    else {
        return;
    };
    walk_mark_array(sub, mark_array_off as usize, visit);
    walk_base_or_mark2_array(sub, base_array_off as usize, mcc as usize, visit);
}

/// Walks every Anchor in a MarkLigPos subtable starting at `sub_off`.
///
/// Layout (MarkLigPos format 1):
/// ```text
///   u16 posFormat = 1
///   u16 markCoverageOffset
///   u16 ligatureCoverageOffset
///   u16 markClassCount
///   o16 markArrayOffset
///   o16 ligatureArrayOffset
///
///   LigatureArray (at ligatureArrayOffset):
///     u16 ligatureCount
///     o16 ligatureAttachOffsets[ligatureCount]   (relative to LigatureArray)
///
///   LigatureAttach (at each ligatureAttachOffset):
///     u16 componentCount
///     ComponentRecord[componentCount]:
///       o16 ligatureAnchorOffsets[markClassCount]  (relative to LigatureAttach)
/// ```
fn walk_mark_lig_pos(gpos_buf: &mut [u8], sub_off: usize, visit: &mut SlotVisitor<'_>) {
    let Some(sub) = gpos_buf.get_mut(sub_off..) else {
        return;
    };
    if sub.len() < 12 || read_u16(sub, 0) != Some(1) {
        return;
    }
    let (Some(mcc), Some(mark_array_off), Some(lig_array_off)) =
        (read_u16(sub, 6), read_u16(sub, 8), read_u16(sub, 10))
    else {
        return;
    };
    let mark_class_count = mcc as usize;
    let lig_array_off = lig_array_off as usize;

    // MarkArray walks like the other Mark* lookups.
    walk_mark_array(sub, mark_array_off as usize, visit);

    // LigatureArray: collect every ComponentRecord's anchor offsets,
    // then visit them in one pass to keep the borrows simple.
    let Some(lig_count) = read_u16(sub, lig_array_off).map(usize::from) else {
        return;
    };
    let lig_attach_offs_start = lig_array_off + 2;
    if lig_attach_offs_start + lig_count * 2 > sub.len() {
        return;
    }
    let mut anchor_abs: Vec<usize> = Vec::new();
    for i in 0..lig_count {
        let rel = read_u16(sub, lig_attach_offs_start + i * 2).unwrap_or(0) as usize;
        if rel == 0 {
            continue;
        }
        let la_off = lig_array_off + rel;
        let Some(comp_count) = read_u16(sub, la_off).map(usize::from) else {
            continue;
        };
        let comps_off = la_off + 2;
        if comps_off + comp_count * mark_class_count * 2 > sub.len() {
            continue;
        }
        for k in 0..comp_count * mark_class_count {
            let rel = read_u16(sub, comps_off + k * 2).unwrap_or(0) as usize;
            if rel != 0 {
                // ligatureAnchorOffsets are relative to LigatureAttach.
                anchor_abs.push(la_off + rel);
            }
        }
    }
    for off in anchor_abs {
        visit_anchor(sub, off, visit);
    }
}

// ---------------------------------------------------------------------------
// ValueRecord walk (SinglePos / PairPos)
// ---------------------------------------------------------------------------

/// Walks every ValueRecord device slot in a SinglePos subtable
/// starting at `sub_off` within `gpos_buf`. Device offsets are
/// relative to the subtable.
fn walk_single_pos(gpos_buf: &mut [u8], sub_off: usize, visit: &mut SlotVisitor<'_>) {
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
fn walk_pair_pos(gpos_buf: &mut [u8], sub_off: usize, visit: &mut SlotVisitor<'_>) {
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

// ---------------------------------------------------------------------------
// Top-level driver
// ---------------------------------------------------------------------------

/// Walks one subtable of the given (non-extension) lookup type.
fn walk_subtable(buf: &mut [u8], lookup_type: u16, sub_abs: usize, visit: &mut SlotVisitor<'_>) {
    match lookup_type {
        1 => walk_single_pos(buf, sub_abs, visit),
        2 => walk_pair_pos(buf, sub_abs, visit),
        3 => walk_cursive_pos(buf, sub_abs, visit),
        4 | 6 => walk_mark_base_or_mark_pos(buf, sub_abs, visit),
        5 => walk_mark_lig_pos(buf, sub_abs, visit),
        // Context (7) / ChainContext (8) carry no device slots of
        // their own; the lookups they dispatch to are reached through
        // the LookupList loop.
        _ => {}
    }
}

/// Walks every lookup in a GPOS table and reports each
/// `Device` / `VariationIndex` slot of the supported lookup types
/// (SinglePos, PairPos, CursivePos, MarkBasePos, MarkLigPos,
/// MarkMarkPos, and Extension wrappers around any of them) to `visit`.
///
/// Slots are reported in lookup order, subtable order, then record
/// order, so a visitor that mutates the buffer sees a deterministic
/// sequence. A slot shared by several records (compilers dedupe
/// identical anchors) is reported once per referencing record.
///
/// Returns `false` when the GPOS header or LookupList is malformed and
/// nothing was walked.
pub(crate) fn walk_gpos_device_slots(gpos: &mut [u8], visit: &mut SlotVisitor<'_>) -> bool {
    if gpos.len() < 10 || read_u16(gpos, 0) != Some(1) {
        return false;
    }
    let Some(lookup_list_off) = read_u16(gpos, 8).map(usize::from) else {
        return false;
    };
    let Some(lookup_count) = read_u16(gpos, lookup_list_off).map(usize::from) else {
        return false;
    };
    let offsets_start = lookup_list_off + 2;
    if offsets_start + lookup_count * 2 > gpos.len() {
        return false;
    }
    for li in 0..lookup_count {
        let Some(lookup_off) = read_u16(gpos, offsets_start + li * 2).map(usize::from) else {
            continue;
        };
        let lookup_base = lookup_list_off + lookup_off;
        let (Some(lookup_type), Some(subtable_count)) =
            (read_u16(gpos, lookup_base), read_u16(gpos, lookup_base + 4))
        else {
            continue;
        };
        let subtable_offsets_off = lookup_base + 6;
        if subtable_offsets_off + subtable_count as usize * 2 > gpos.len() {
            continue;
        }
        for si in 0..subtable_count as usize {
            let Some(sub_rel) = read_u16(gpos, subtable_offsets_off + si * 2) else {
                continue;
            };
            let sub_abs = lookup_base + sub_rel as usize;
            if sub_abs >= gpos.len() {
                continue;
            }
            if lookup_type != 9 {
                walk_subtable(gpos, lookup_type, sub_abs, visit);
                continue;
            }
            // Type 9: Extension. u16 format, u16 extensionLookupType,
            // Offset32 extensionOffset (relative to the extension
            // subtable). Recurse into the inner subtable so every
            // supported type wrapped in an Extension is covered too.
            let (Some(ext_type), Some(hi), Some(lo)) = (
                read_u16(gpos, sub_abs + 2),
                read_u16(gpos, sub_abs + 4),
                read_u16(gpos, sub_abs + 6),
            ) else {
                continue;
            };
            let ext_off = (usize::from(hi) << 16) | usize::from(lo);
            let Some(inner_abs) = sub_abs.checked_add(ext_off) else {
                continue;
            };
            if inner_abs < gpos.len() && ext_type != 9 {
                walk_subtable(gpos, ext_type, inner_abs, visit);
            }
        }
    }
    true
}

/// Folds every supported `VariationIndex` in the source GPOS into the
/// static field it adjusts at `coords` and zeros the offset slot.
/// Lookup types we do not understand ride through verbatim.
///
/// Returns `Some(new_gpos_bytes)` when the source carries a parseable
/// GPOS header, else `None` (caller passes through). The returned
/// table is byte-for-byte identical to the source for every byte we
/// did not touch. Only the fields we folded into and the offset slots
/// we zeroed change.
pub(crate) fn bake_gpos_at_coords(
    gpos_bytes: &[u8],
    store: Option<&ItemVariationStore<'_>>,
    coords: &[f32],
) -> Option<Vec<u8>> {
    let mut buf = gpos_bytes.to_vec();
    let walked = walk_gpos_device_slots(&mut buf, &mut |b, slot| {
        fold_one_field(b, slot, store, coords);
    });
    walked.then_some(buf)
}

/// Zeros every device slot of a GPOS table that points at a
/// `VariationIndex`. Static fields are left alone, which is exact at
/// the default instance. Per-ppem hinting `Device` slots are kept.
///
/// Used by the subsetter when `retain_variations` is off, so the
/// static output never references the `ItemVariationStore` it drops.
pub(crate) fn strip_variation_indices(gpos: &mut [u8]) {
    walk_gpos_device_slots(gpos, &mut |b, slot| {
        if slot.delta_format(b) == Some(VARIATION_INDEX_DELTA_FORMAT) {
            slot.clear(b);
        }
    });
}

#[cfg(test)]
mod offset_base_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// A device slot at `slot` whose offset is measured from byte 0
    /// and that adjusts the i16 at `field`.
    fn subtable_slot(field: usize, slot: usize) -> DeviceSlot {
        DeviceSlot {
            base: 0,
            field: Some(field),
            slot,
        }
    }

    /// Points the AnchorFormat3 `xDeviceOffset` slot at `x_dev_pos` at
    /// the table at `table_pos`. The anchor starts 6 bytes before the
    /// slot, and the offset is measured from there.
    fn set_x_device(gpos: &mut [u8], x_dev_pos: usize, table_pos: usize) {
        let rel = (table_pos - (x_dev_pos - 6)) as u16;
        gpos[x_dev_pos..x_dev_pos + 2].copy_from_slice(&rel.to_be_bytes());
    }

    /// Same as [`set_x_device`] for the `yDeviceOffset` slot, which
    /// sits 8 bytes into the anchor.
    fn set_y_device(gpos: &mut [u8], y_dev_pos: usize, table_pos: usize) {
        let rel = (table_pos - (y_dev_pos - 8)) as u16;
        gpos[y_dev_pos..y_dev_pos + 2].copy_from_slice(&rel.to_be_bytes());
    }

    /// Folds the Anchor at `anchor_off` the way the bake does.
    fn fold_anchor_variations(
        buf: &mut [u8],
        anchor_off: usize,
        store: Option<&ItemVariationStore<'_>>,
        coords: &[f32],
    ) {
        visit_anchor(buf, anchor_off, &mut |b, slot| {
            fold_one_field(b, slot, store, coords);
        });
    }

    /// Builds a one-region one-item ItemVariationStore: at coord 1.0
    /// the single item resolves to `delta`; at 0.0 it resolves to 0;
    /// linear in between.
    fn build_ivs_one_region_one_item(delta: i16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        let region_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // subtable count
        let subtable_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());

        #[allow(clippy::cast_possible_truncation)]
        let region_start = out.len() as u32;
        out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_start.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
        out.extend_from_slice(&1u16.to_be_bytes()); // regionCount
                                                    // F2DOT14 (start, peak, end) = (0.0, 1.0, 1.0)
        out.extend_from_slice(&0i16.to_be_bytes());
        out.extend_from_slice(&16384i16.to_be_bytes());
        out.extend_from_slice(&16384i16.to_be_bytes());

        #[allow(clippy::cast_possible_truncation)]
        let sub_start = out.len() as u32;
        out[subtable_slot..subtable_slot + 4].copy_from_slice(&sub_start.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // itemCount
        out.extend_from_slice(&1u16.to_be_bytes()); // wordDeltaCount
        out.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
        out.extend_from_slice(&0u16.to_be_bytes()); // region index 0
        out.extend_from_slice(&delta.to_be_bytes());
        out
    }

    #[test]
    fn value_record_size_matches_popcount() {
        assert_eq!(value_record_size(0), 0);
        assert_eq!(value_record_size(VR_X_ADVANCE), 2);
        assert_eq!(value_record_size(VR_X_ADVANCE | VR_X_ADVANCE_DEVICE), 4);
        assert_eq!(value_record_size(0xFF), 16);
    }

    #[test]
    fn fold_one_field_zeros_absent_offset_noop() {
        let mut buf = vec![0u8; 8];
        // Static field at 0..2 starts at 100; offset slot at 4..6 is
        // zero (absent). Fold must be a no-op.
        buf[0..2].copy_from_slice(&100i16.to_be_bytes());
        let ivs_bytes = build_ivs_one_region_one_item(80);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        fold_one_field(&mut buf, subtable_slot(0, 4), Some(&store), &[1.0]);
        let cur = i16::from_be_bytes([buf[0], buf[1]]);
        assert_eq!(cur, 100);
    }

    #[test]
    fn fold_one_field_resolves_variation_index_and_zeros_offset() {
        // Static field at 0..2 = 50; offset slot at 4..6 = 8 (points
        // at the VariationIndex header at byte 8). At coord 1.0 the
        // delta is 80 -> 50 + 80 = 130. After fold the offset slot is
        // zero.
        let mut buf = vec![0u8; 14];
        buf[0..2].copy_from_slice(&50i16.to_be_bytes());
        buf[4..6].copy_from_slice(&8u16.to_be_bytes());
        // VariationIndex at byte 8: outer=0, inner=0, deltaFormat=0x8000.
        buf[8..10].copy_from_slice(&0u16.to_be_bytes());
        buf[10..12].copy_from_slice(&0u16.to_be_bytes());
        buf[12..14].copy_from_slice(&0x8000u16.to_be_bytes());
        let ivs_bytes = build_ivs_one_region_one_item(80);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        fold_one_field(&mut buf, subtable_slot(0, 4), Some(&store), &[1.0]);
        let cur = i16::from_be_bytes([buf[0], buf[1]]);
        assert_eq!(cur, 130);
        assert_eq!(buf[4], 0);
        assert_eq!(buf[5], 0);
    }

    #[test]
    fn fold_one_field_device_table_zeros_offset_only() {
        // Device-shape (deltaFormat = 3): must zero the offset but
        // leave the static field alone.
        let mut buf = vec![0u8; 14];
        buf[0..2].copy_from_slice(&50i16.to_be_bytes());
        buf[4..6].copy_from_slice(&8u16.to_be_bytes());
        buf[8..10].copy_from_slice(&8u16.to_be_bytes()); // startSize
        buf[10..12].copy_from_slice(&16u16.to_be_bytes()); // endSize
        buf[12..14].copy_from_slice(&3u16.to_be_bytes()); // deltaFormat = Device
        fold_one_field(&mut buf, subtable_slot(0, 4), None, &[]);
        let cur = i16::from_be_bytes([buf[0], buf[1]]);
        assert_eq!(cur, 50);
        assert_eq!(buf[4], 0);
        assert_eq!(buf[5], 0);
    }

    #[test]
    fn fold_one_field_variation_without_store_zeros_offset_only() {
        let mut buf = vec![0u8; 14];
        buf[0..2].copy_from_slice(&50i16.to_be_bytes());
        buf[4..6].copy_from_slice(&8u16.to_be_bytes());
        buf[8..10].copy_from_slice(&0u16.to_be_bytes());
        buf[10..12].copy_from_slice(&0u16.to_be_bytes());
        buf[12..14].copy_from_slice(&0x8000u16.to_be_bytes());
        fold_one_field(&mut buf, subtable_slot(0, 4), None, &[1.0]);
        let cur = i16::from_be_bytes([buf[0], buf[1]]);
        assert_eq!(cur, 50);
        assert_eq!(buf[4], 0);
        assert_eq!(buf[5], 0);
    }

    #[test]
    fn fold_one_field_saturates_at_i16_max() {
        let mut buf = vec![0u8; 14];
        buf[0..2].copy_from_slice(&30000i16.to_be_bytes());
        buf[4..6].copy_from_slice(&8u16.to_be_bytes());
        buf[8..10].copy_from_slice(&0u16.to_be_bytes());
        buf[10..12].copy_from_slice(&0u16.to_be_bytes());
        buf[12..14].copy_from_slice(&0x8000u16.to_be_bytes());
        // delta = 30000 -> 30000 + 30000 saturates at i16::MAX (32767).
        let ivs_bytes = build_ivs_one_region_one_item(30000);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        fold_one_field(&mut buf, subtable_slot(0, 4), Some(&store), &[1.0]);
        let cur = i16::from_be_bytes([buf[0], buf[1]]);
        assert_eq!(cur, i16::MAX);
    }

    /// PairPos fmt 1 round-trip: build a single AV pair whose
    /// `valueRecord1.x_advance` carries a `VariationIndex` to a
    /// non-zero IVS delta. After fold at coord 1.0 the static
    /// `x_advance` equals `source + delta`, and the device offset is
    /// zero.
    #[test]
    fn pair_pos_fmt1_x_advance_variation_folds() {
        // Lay out: GPOS header 10 bytes
        // + LookupList at offset 10:
        //   u16 lookupCount = 1
        //   u16 lookupOffset[0] = 4 (relative to LookupList start)
        // + Lookup at offset 14:
        //   u16 lookupType = 2
        //   u16 lookupFlag = 0
        //   u16 subtableCount = 1
        //   u16 subtableOffset[0] = 8 (relative to Lookup start)
        // + PairPos subtable at offset 22.

        let mut gpos = Vec::new();
        // Header
        gpos.extend_from_slice(&1u16.to_be_bytes()); // major
        gpos.extend_from_slice(&0u16.to_be_bytes()); // minor
        gpos.extend_from_slice(&100u16.to_be_bytes()); // scriptListOff (unused)
        gpos.extend_from_slice(&100u16.to_be_bytes()); // featureListOff (unused)
        gpos.extend_from_slice(&10u16.to_be_bytes()); // lookupListOff
                                                      // LookupList
        gpos.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
        gpos.extend_from_slice(&4u16.to_be_bytes()); // lookupOffset[0]
                                                     // Lookup at 14
        gpos.extend_from_slice(&2u16.to_be_bytes()); // lookupType
        gpos.extend_from_slice(&0u16.to_be_bytes()); // flag
        gpos.extend_from_slice(&1u16.to_be_bytes()); // subtableCount
        gpos.extend_from_slice(&8u16.to_be_bytes()); // subtableOffset[0]
                                                     // PairPos at 22, sub_off = 22.
        let sub_off = gpos.len();
        let value_format1 = VR_X_ADVANCE | VR_X_ADVANCE_DEVICE; // 0x44
        let value_format2 = 0u16;
        gpos.extend_from_slice(&1u16.to_be_bytes()); // posFormat
        gpos.extend_from_slice(&0u16.to_be_bytes()); // coverageOff (filled below)
        gpos.extend_from_slice(&value_format1.to_be_bytes());
        gpos.extend_from_slice(&value_format2.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes()); // pairSetCount
        gpos.extend_from_slice(&0u16.to_be_bytes()); // pairSetOffset (filled below)
                                                     // PairSet
        let pair_set_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&1u16.to_be_bytes()); // pairValueCount
        gpos.extend_from_slice(&60u16.to_be_bytes()); // secondGlyph
                                                      // ValueRecord1: x_advance (i16) + x_advance_device (offset16).
        let x_advance_pos = gpos.len();
        gpos.extend_from_slice(&(-50i16).to_be_bytes()); // x_advance source
        let device_off_pos = gpos.len();
        // Will fill device_off below: points at the VariationIndex
        // header that we tack on at the end of the subtable.
        gpos.extend_from_slice(&0u16.to_be_bytes());
        // ValueRecord2 is empty (format2 == 0).

        // Coverage at the end of the subtable.
        let coverage_rel = (gpos.len() - sub_off) as u16;
        // Coverage format 1, glyphCount 1, glyph 50.
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&50u16.to_be_bytes());

        // VariationIndex at the end: outer=0, inner=0, deltaFormat=0x8000.
        let vi_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0x8000u16.to_be_bytes());

        // Patch slots.
        gpos[sub_off + 2..sub_off + 4].copy_from_slice(&coverage_rel.to_be_bytes());
        gpos[sub_off + 10..sub_off + 12].copy_from_slice(&pair_set_rel.to_be_bytes());
        // PairValueRecord device offsets are relative to the PairSet.
        gpos[device_off_pos..device_off_pos + 2]
            .copy_from_slice(&(vi_rel - pair_set_rel).to_be_bytes());

        // Build IVS and run the bake at coord 1.0.
        let ivs_bytes = build_ivs_one_region_one_item(75);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let baked = bake_gpos_at_coords(&gpos, Some(&store), &[1.0]).unwrap();

        // x_advance: -50 + 75 = 25.
        let baked_x = i16::from_be_bytes([baked[x_advance_pos], baked[x_advance_pos + 1]]);
        assert_eq!(baked_x, 25);
        // device offset slot zeroed.
        let baked_off = u16::from_be_bytes([baked[device_off_pos], baked[device_off_pos + 1]]);
        assert_eq!(baked_off, 0);
    }

    /// SinglePos fmt 2 round-trip: per-glyph ValueRecord array, each
    /// with an x_advance variation. Verify every entry's static field
    /// gets the delta and every device offset slot is zeroed.
    #[test]
    fn single_pos_fmt2_x_advance_variation_folds_for_every_entry() {
        let mut gpos = Vec::new();
        gpos.extend_from_slice(&1u16.to_be_bytes()); // major
        gpos.extend_from_slice(&0u16.to_be_bytes()); // minor
        gpos.extend_from_slice(&100u16.to_be_bytes());
        gpos.extend_from_slice(&100u16.to_be_bytes());
        gpos.extend_from_slice(&10u16.to_be_bytes()); // lookupListOff
        gpos.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
        gpos.extend_from_slice(&4u16.to_be_bytes()); // lookupOffset[0]
        gpos.extend_from_slice(&1u16.to_be_bytes()); // lookupType = SinglePos
        gpos.extend_from_slice(&0u16.to_be_bytes()); // flag
        gpos.extend_from_slice(&1u16.to_be_bytes()); // subtableCount
        gpos.extend_from_slice(&8u16.to_be_bytes()); // subtableOffset[0]

        let sub_off = gpos.len();
        let value_format = VR_X_ADVANCE | VR_X_ADVANCE_DEVICE; // 0x44
        gpos.extend_from_slice(&2u16.to_be_bytes()); // posFormat = 2
        gpos.extend_from_slice(&0u16.to_be_bytes()); // coverageOff (fill below)
        gpos.extend_from_slice(&value_format.to_be_bytes());
        gpos.extend_from_slice(&2u16.to_be_bytes()); // valueCount = 2
                                                     // Two ValueRecords: each is 4 bytes (i16 + o16).
        let vr0_pos = gpos.len();
        gpos.extend_from_slice(&10i16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes()); // device off (fill below)
        let vr1_pos = gpos.len();
        gpos.extend_from_slice(&20i16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes()); // device off (fill below)

        let coverage_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&1u16.to_be_bytes()); // cov format 1
        gpos.extend_from_slice(&2u16.to_be_bytes()); // glyph count
        gpos.extend_from_slice(&30u16.to_be_bytes());
        gpos.extend_from_slice(&31u16.to_be_bytes());

        let vi_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0x8000u16.to_be_bytes());

        gpos[sub_off + 2..sub_off + 4].copy_from_slice(&coverage_rel.to_be_bytes());
        gpos[vr0_pos + 2..vr0_pos + 4].copy_from_slice(&vi_rel.to_be_bytes());
        gpos[vr1_pos + 2..vr1_pos + 4].copy_from_slice(&vi_rel.to_be_bytes());

        let ivs_bytes = build_ivs_one_region_one_item(40);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let baked = bake_gpos_at_coords(&gpos, Some(&store), &[1.0]).unwrap();

        let v0 = i16::from_be_bytes([baked[vr0_pos], baked[vr0_pos + 1]]);
        let v1 = i16::from_be_bytes([baked[vr1_pos], baked[vr1_pos + 1]]);
        assert_eq!(v0, 50);
        assert_eq!(v1, 60);
        let off0 = u16::from_be_bytes([baked[vr0_pos + 2], baked[vr0_pos + 3]]);
        let off1 = u16::from_be_bytes([baked[vr1_pos + 2], baked[vr1_pos + 3]]);
        assert_eq!(off0, 0);
        assert_eq!(off1, 0);
    }

    /// No-IVS source: the bake must still walk and zero VariationIndex
    /// offsets even though it cannot resolve a delta. This is the
    /// "GDEF.IVS will be pruned next" path. Leaving the offsets
    /// dangling would re-create the orphan that #173 already shipped.
    #[test]
    fn bake_without_ivs_zeros_offsets_without_changing_static_fields() {
        // Reuse the pair-pos fixture from above without an IVS.
        let mut gpos = Vec::new();
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&100u16.to_be_bytes());
        gpos.extend_from_slice(&100u16.to_be_bytes());
        gpos.extend_from_slice(&10u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&4u16.to_be_bytes());
        gpos.extend_from_slice(&2u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&8u16.to_be_bytes());
        let sub_off = gpos.len();
        let vf1 = VR_X_ADVANCE | VR_X_ADVANCE_DEVICE;
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&vf1.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        let pair_set_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&60u16.to_be_bytes());
        let x_advance_pos = gpos.len();
        gpos.extend_from_slice(&(-50i16).to_be_bytes());
        let device_off_pos = gpos.len();
        gpos.extend_from_slice(&0u16.to_be_bytes());
        let coverage_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&50u16.to_be_bytes());
        let vi_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0x8000u16.to_be_bytes());
        gpos[sub_off + 2..sub_off + 4].copy_from_slice(&coverage_rel.to_be_bytes());
        gpos[sub_off + 10..sub_off + 12].copy_from_slice(&pair_set_rel.to_be_bytes());
        // PairValueRecord device offsets are relative to the PairSet.
        gpos[device_off_pos..device_off_pos + 2]
            .copy_from_slice(&(vi_rel - pair_set_rel).to_be_bytes());

        let baked = bake_gpos_at_coords(&gpos, None, &[1.0]).unwrap();
        // Static field unchanged.
        let baked_x = i16::from_be_bytes([baked[x_advance_pos], baked[x_advance_pos + 1]]);
        assert_eq!(baked_x, -50);
        // Offset zeroed.
        let baked_off = u16::from_be_bytes([baked[device_off_pos], baked[device_off_pos + 1]]);
        assert_eq!(baked_off, 0);
    }

    // -----------------------------------------------------------------
    // Mark*/Cursive anchor fold tests
    // -----------------------------------------------------------------

    /// AnchorFormat 1 has no device slots. The fold must be a pure
    /// no-op on every byte.
    #[test]
    fn fold_anchor_format1_is_noop() {
        // Pad 4 bytes up front so anchor_off != 0 (the helper treats
        // an offset of 0 as the spec's "absent" sentinel).
        let mut buf = vec![0u8; 4];
        buf.extend_from_slice(&1u16.to_be_bytes());
        buf.extend_from_slice(&300i16.to_be_bytes());
        buf.extend_from_slice(&(-200i16).to_be_bytes());
        let original = buf.clone();
        let ivs_bytes = build_ivs_one_region_one_item(40);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        fold_anchor_variations(&mut buf, 4, Some(&store), &[1.0]);
        assert_eq!(buf, original);
    }

    /// AnchorFormat 2 (contour-point hint) carries no device slots:
    /// fold must leave every byte untouched.
    #[test]
    fn fold_anchor_format2_is_noop() {
        let mut buf = vec![0u8; 4];
        buf.extend_from_slice(&2u16.to_be_bytes());
        buf.extend_from_slice(&50i16.to_be_bytes());
        buf.extend_from_slice(&75i16.to_be_bytes());
        buf.extend_from_slice(&42u16.to_be_bytes()); // anchorPoint
        let original = buf.clone();
        let ivs_bytes = build_ivs_one_region_one_item(40);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        fold_anchor_variations(&mut buf, 4, Some(&store), &[1.0]);
        assert_eq!(buf, original);
    }

    /// AnchorFormat 3 with both x and y device offsets pointing at a
    /// VariationIndex: both static fields must absorb the delta and
    /// both device slots must zero.
    #[test]
    fn fold_anchor_format3_folds_x_and_y() {
        // Build: 4 bytes of pad, anchor at byte 4 (10 B), then the
        // shared VariationIndex. The pad keeps anchor_off != 0 so the
        // helper doesn't treat the anchor as "absent".
        let mut buf = vec![0u8; 4];
        buf.extend_from_slice(&3u16.to_be_bytes()); // format
        buf.extend_from_slice(&100i16.to_be_bytes()); // xCoord
        buf.extend_from_slice(&(-50i16).to_be_bytes()); // yCoord
        let x_dev_pos = buf.len();
        buf.extend_from_slice(&0u16.to_be_bytes()); // xDevice (filled)
        let y_dev_pos = buf.len();
        buf.extend_from_slice(&0u16.to_be_bytes()); // yDevice (filled)
        let vi_pos = buf.len();
        buf.extend_from_slice(&0u16.to_be_bytes()); // outer
        buf.extend_from_slice(&0u16.to_be_bytes()); // inner
        buf.extend_from_slice(&0x8000u16.to_be_bytes()); // deltaFormat
                                                         // Device offsets are relative to the anchor at byte 4.
        buf[x_dev_pos..x_dev_pos + 2].copy_from_slice(&((vi_pos - 4) as u16).to_be_bytes());
        buf[y_dev_pos..y_dev_pos + 2].copy_from_slice(&((vi_pos - 4) as u16).to_be_bytes());

        let ivs_bytes = build_ivs_one_region_one_item(25);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        fold_anchor_variations(&mut buf, 4, Some(&store), &[1.0]);
        let x = i16::from_be_bytes([buf[6], buf[7]]);
        let y = i16::from_be_bytes([buf[8], buf[9]]);
        assert_eq!(x, 125);
        assert_eq!(y, -25);
        assert_eq!(u16::from_be_bytes([buf[x_dev_pos], buf[x_dev_pos + 1]]), 0);
        assert_eq!(u16::from_be_bytes([buf[y_dev_pos], buf[y_dev_pos + 1]]), 0);
    }

    /// Zero anchor offset (the spec's "absent" sentinel): the fold
    /// must early-return rather than walk into byte 0 of the subtable.
    #[test]
    fn fold_anchor_zero_offset_is_noop() {
        let mut buf = vec![0xAAu8; 16];
        let original = buf.clone();
        fold_anchor_variations(&mut buf, 0, None, &[1.0]);
        assert_eq!(buf, original);
    }

    /// CursivePos round-trip: one EntryExitRecord with both anchors
    /// in format 3, each pointing at a VariationIndex into a non-zero
    /// IVS delta. Bake at coord 1.0 and assert the static fields
    /// absorb the delta and every device offset is zeroed.
    #[test]
    fn cursive_pos_anchor_variation_folds() {
        let mut gpos = Vec::new();
        // Header.
        gpos.extend_from_slice(&1u16.to_be_bytes()); // major
        gpos.extend_from_slice(&0u16.to_be_bytes()); // minor
        gpos.extend_from_slice(&100u16.to_be_bytes()); // scriptListOff (unused)
        gpos.extend_from_slice(&100u16.to_be_bytes()); // featureListOff (unused)
        gpos.extend_from_slice(&10u16.to_be_bytes()); // lookupListOff
                                                      // LookupList at 10:
        gpos.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
        gpos.extend_from_slice(&4u16.to_be_bytes()); // lookupOffset[0] (rel to LookupList)
                                                     // Lookup at 14:
        gpos.extend_from_slice(&3u16.to_be_bytes()); // lookupType = CursivePos
        gpos.extend_from_slice(&0u16.to_be_bytes()); // flag
        gpos.extend_from_slice(&1u16.to_be_bytes()); // subtableCount
        gpos.extend_from_slice(&8u16.to_be_bytes()); // subtableOffset[0] (rel to Lookup)
                                                     // CursivePos subtable at byte 22.
        let sub_off = gpos.len();
        gpos.extend_from_slice(&1u16.to_be_bytes()); // posFormat=1
        gpos.extend_from_slice(&0u16.to_be_bytes()); // coverageOff (filled below)
        gpos.extend_from_slice(&1u16.to_be_bytes()); // entryExitCount=1
                                                     // EntryExitRecord[0]: entryAnchorOffset, exitAnchorOffset.
        let ee_pos = gpos.len();
        gpos.extend_from_slice(&0u16.to_be_bytes()); // entryAnchorOffset (filled)
        gpos.extend_from_slice(&0u16.to_be_bytes()); // exitAnchorOffset (filled)
                                                     // Entry anchor (format 3) at end-of-records.
        let entry_anchor_off = gpos.len() - sub_off;
        let entry_x_pos = gpos.len() + 2;
        let entry_y_pos = gpos.len() + 4;
        let entry_x_dev_pos = gpos.len() + 6;
        let entry_y_dev_pos = gpos.len() + 8;
        gpos.extend_from_slice(&3u16.to_be_bytes()); // format=3
        gpos.extend_from_slice(&500i16.to_be_bytes()); // xCoord
        gpos.extend_from_slice(&100i16.to_be_bytes()); // yCoord
        gpos.extend_from_slice(&0u16.to_be_bytes()); // xDevice (filled below)
        gpos.extend_from_slice(&0u16.to_be_bytes()); // yDevice (filled below)
                                                     // Exit anchor (format 3).
        let exit_anchor_off = gpos.len() - sub_off;
        let exit_x_pos = gpos.len() + 2;
        let exit_y_pos = gpos.len() + 4;
        let exit_x_dev_pos = gpos.len() + 6;
        let exit_y_dev_pos = gpos.len() + 8;
        gpos.extend_from_slice(&3u16.to_be_bytes());
        gpos.extend_from_slice(&(-200i16).to_be_bytes());
        gpos.extend_from_slice(&50i16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());

        // Coverage at end.
        let coverage_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&77u16.to_be_bytes());

        // Shared VariationIndex at end of subtable.
        let vi_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0x8000u16.to_be_bytes());

        // Patch slots.
        gpos[sub_off + 2..sub_off + 4].copy_from_slice(&coverage_rel.to_be_bytes());
        gpos[ee_pos..ee_pos + 2].copy_from_slice(&(entry_anchor_off as u16).to_be_bytes());
        gpos[ee_pos + 2..ee_pos + 4].copy_from_slice(&(exit_anchor_off as u16).to_be_bytes());
        // Point both anchors' x/y devices at the shared VariationIndex.
        set_x_device(&mut gpos, entry_x_dev_pos, sub_off + vi_rel as usize);
        set_y_device(&mut gpos, entry_y_dev_pos, sub_off + vi_rel as usize);
        set_x_device(&mut gpos, exit_x_dev_pos, sub_off + vi_rel as usize);
        set_y_device(&mut gpos, exit_y_dev_pos, sub_off + vi_rel as usize);

        let ivs_bytes = build_ivs_one_region_one_item(60);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let baked = bake_gpos_at_coords(&gpos, Some(&store), &[1.0]).unwrap();

        // Entry: x=500+60=560, y=100+60=160.
        assert_eq!(
            i16::from_be_bytes([baked[entry_x_pos], baked[entry_x_pos + 1]]),
            560
        );
        assert_eq!(
            i16::from_be_bytes([baked[entry_y_pos], baked[entry_y_pos + 1]]),
            160
        );
        // Exit: x=-200+60=-140, y=50+60=110.
        assert_eq!(
            i16::from_be_bytes([baked[exit_x_pos], baked[exit_x_pos + 1]]),
            -140
        );
        assert_eq!(
            i16::from_be_bytes([baked[exit_y_pos], baked[exit_y_pos + 1]]),
            110
        );
        // All four device offset slots zeroed.
        for off in [
            entry_x_dev_pos,
            entry_y_dev_pos,
            exit_x_dev_pos,
            exit_y_dev_pos,
        ] {
            assert_eq!(u16::from_be_bytes([baked[off], baked[off + 1]]), 0);
        }
    }

    /// MarkBasePos round-trip: one mark, one base, single mark class.
    /// Both anchors are AnchorFormat 3 with x/yDevice -> VariationIndex.
    /// Assert both anchors' static fields absorb the delta and every
    /// device slot zeros.
    #[test]
    fn mark_base_pos_anchor_variation_folds() {
        // GPOS header / lookup list / lookup identical to the cursive
        // test, but with lookupType = 4.
        let mut gpos = Vec::new();
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&100u16.to_be_bytes());
        gpos.extend_from_slice(&100u16.to_be_bytes());
        gpos.extend_from_slice(&10u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&4u16.to_be_bytes());
        gpos.extend_from_slice(&4u16.to_be_bytes()); // lookupType=4
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&8u16.to_be_bytes());
        let sub_off = gpos.len();
        // MarkBasePos: u16 format=1, u16 markCovOff, u16 baseCovOff,
        // u16 markClassCount, o16 markArrayOff, o16 baseArrayOff.
        gpos.extend_from_slice(&1u16.to_be_bytes()); // posFormat
        gpos.extend_from_slice(&0u16.to_be_bytes()); // markCovOff (fill)
        gpos.extend_from_slice(&0u16.to_be_bytes()); // baseCovOff (fill)
        gpos.extend_from_slice(&1u16.to_be_bytes()); // markClassCount=1
        gpos.extend_from_slice(&0u16.to_be_bytes()); // markArrayOff (fill)
        gpos.extend_from_slice(&0u16.to_be_bytes()); // baseArrayOff (fill)

        // MarkArray.
        let mark_array_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&1u16.to_be_bytes()); // markCount
        gpos.extend_from_slice(&0u16.to_be_bytes()); // markRecord.class=0
                                                     // markAnchorOffset (rel to MarkArray), fill below.
        let mark_anchor_off_pos = gpos.len();
        gpos.extend_from_slice(&0u16.to_be_bytes());

        // Mark anchor (format 3).
        let mark_anchor_rel_to_marray = (gpos.len() - sub_off - mark_array_rel as usize) as u16;
        let mark_x_pos = gpos.len() + 2;
        let mark_y_pos = gpos.len() + 4;
        let mark_x_dev_pos = gpos.len() + 6;
        let mark_y_dev_pos = gpos.len() + 8;
        gpos.extend_from_slice(&3u16.to_be_bytes());
        gpos.extend_from_slice(&10i16.to_be_bytes());
        gpos.extend_from_slice(&20i16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());

        // BaseArray.
        let base_array_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&1u16.to_be_bytes()); // baseCount
                                                     // baseAnchorOffsets[markClassCount=1], rel to BaseArray.
        let base_anchor_off_pos = gpos.len();
        gpos.extend_from_slice(&0u16.to_be_bytes());

        // Base anchor (format 3).
        let base_anchor_rel_to_barray = (gpos.len() - sub_off - base_array_rel as usize) as u16;
        let base_x_pos = gpos.len() + 2;
        let base_y_pos = gpos.len() + 4;
        let base_x_dev_pos = gpos.len() + 6;
        let base_y_dev_pos = gpos.len() + 8;
        gpos.extend_from_slice(&3u16.to_be_bytes());
        gpos.extend_from_slice(&100i16.to_be_bytes());
        gpos.extend_from_slice(&200i16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());

        // Coverage records at the end (same shape, same content; we
        // don't actually consult them in the bake walk).
        let cov_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&50u16.to_be_bytes());

        // Shared VariationIndex.
        let vi_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0x8000u16.to_be_bytes());

        // Patch slots.
        gpos[sub_off + 2..sub_off + 4].copy_from_slice(&cov_rel.to_be_bytes());
        gpos[sub_off + 4..sub_off + 6].copy_from_slice(&cov_rel.to_be_bytes());
        gpos[sub_off + 8..sub_off + 10].copy_from_slice(&mark_array_rel.to_be_bytes());
        gpos[sub_off + 10..sub_off + 12].copy_from_slice(&base_array_rel.to_be_bytes());
        gpos[mark_anchor_off_pos..mark_anchor_off_pos + 2]
            .copy_from_slice(&mark_anchor_rel_to_marray.to_be_bytes());
        gpos[base_anchor_off_pos..base_anchor_off_pos + 2]
            .copy_from_slice(&base_anchor_rel_to_barray.to_be_bytes());
        set_x_device(&mut gpos, mark_x_dev_pos, sub_off + vi_rel as usize);
        set_y_device(&mut gpos, mark_y_dev_pos, sub_off + vi_rel as usize);
        set_x_device(&mut gpos, base_x_dev_pos, sub_off + vi_rel as usize);
        set_y_device(&mut gpos, base_y_dev_pos, sub_off + vi_rel as usize);

        let ivs_bytes = build_ivs_one_region_one_item(15);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let baked = bake_gpos_at_coords(&gpos, Some(&store), &[1.0]).unwrap();

        assert_eq!(
            i16::from_be_bytes([baked[mark_x_pos], baked[mark_x_pos + 1]]),
            25
        );
        assert_eq!(
            i16::from_be_bytes([baked[mark_y_pos], baked[mark_y_pos + 1]]),
            35
        );
        assert_eq!(
            i16::from_be_bytes([baked[base_x_pos], baked[base_x_pos + 1]]),
            115
        );
        assert_eq!(
            i16::from_be_bytes([baked[base_y_pos], baked[base_y_pos + 1]]),
            215
        );
        for off in [
            mark_x_dev_pos,
            mark_y_dev_pos,
            base_x_dev_pos,
            base_y_dev_pos,
        ] {
            assert_eq!(u16::from_be_bytes([baked[off], baked[off + 1]]), 0);
        }
    }

    /// MarkLigPos round-trip: one mark, one ligature with two
    /// components, single mark class. The component matrix is
    /// `componentCount * markClassCount`, 2 anchors per ligature.
    /// Assert both component anchors absorb the delta and zero their
    /// device slots.
    #[test]
    fn mark_lig_pos_anchor_variation_folds() {
        let mut gpos = Vec::new();
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&100u16.to_be_bytes());
        gpos.extend_from_slice(&100u16.to_be_bytes());
        gpos.extend_from_slice(&10u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&4u16.to_be_bytes());
        gpos.extend_from_slice(&5u16.to_be_bytes()); // lookupType=5
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&8u16.to_be_bytes());
        let sub_off = gpos.len();
        gpos.extend_from_slice(&1u16.to_be_bytes()); // posFormat
        gpos.extend_from_slice(&0u16.to_be_bytes()); // markCovOff (fill)
        gpos.extend_from_slice(&0u16.to_be_bytes()); // ligCovOff (fill)
        gpos.extend_from_slice(&1u16.to_be_bytes()); // markClassCount=1
        gpos.extend_from_slice(&0u16.to_be_bytes()); // markArrayOff (fill)
        gpos.extend_from_slice(&0u16.to_be_bytes()); // ligArrayOff (fill)

        // MarkArray.
        let mark_array_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&1u16.to_be_bytes()); // markCount=1
        gpos.extend_from_slice(&0u16.to_be_bytes()); // markRecord.class=0
        let mark_anchor_off_pos = gpos.len();
        gpos.extend_from_slice(&0u16.to_be_bytes()); // markAnchorOff (fill)

        // Mark anchor (format 3, single shared VariationIndex).
        let mark_anchor_rel_to_marray = (gpos.len() - sub_off - mark_array_rel as usize) as u16;
        let mark_x_pos = gpos.len() + 2;
        let mark_x_dev_pos = gpos.len() + 6;
        let mark_y_dev_pos = gpos.len() + 8;
        gpos.extend_from_slice(&3u16.to_be_bytes());
        gpos.extend_from_slice(&5i16.to_be_bytes());
        gpos.extend_from_slice(&5i16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());

        // LigatureArray.
        let lig_array_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&1u16.to_be_bytes()); // ligatureCount=1
        let lig_attach_off_pos = gpos.len();
        gpos.extend_from_slice(&0u16.to_be_bytes()); // ligAttachOff (fill)

        // LigatureAttach.
        let lig_attach_rel_to_larray = (gpos.len() - sub_off - lig_array_rel as usize) as u16;
        let lig_attach_abs_in_sub = gpos.len() - sub_off;
        gpos.extend_from_slice(&2u16.to_be_bytes()); // componentCount=2
                                                     // 2 components * 1 markClass = 2 anchor offsets.
        let comp0_anchor_off_pos = gpos.len();
        gpos.extend_from_slice(&0u16.to_be_bytes());
        let comp1_anchor_off_pos = gpos.len();
        gpos.extend_from_slice(&0u16.to_be_bytes());

        // Component 0 anchor (format 3).
        let comp0_anchor_rel_to_la = (gpos.len() - sub_off - lig_attach_abs_in_sub) as u16;
        let comp0_x_pos = gpos.len() + 2;
        let comp0_x_dev_pos = gpos.len() + 6;
        let comp0_y_dev_pos = gpos.len() + 8;
        gpos.extend_from_slice(&3u16.to_be_bytes());
        gpos.extend_from_slice(&100i16.to_be_bytes());
        gpos.extend_from_slice(&50i16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());

        // Component 1 anchor (format 3).
        let comp1_anchor_rel_to_la = (gpos.len() - sub_off - lig_attach_abs_in_sub) as u16;
        let comp1_x_pos = gpos.len() + 2;
        let comp1_x_dev_pos = gpos.len() + 6;
        let comp1_y_dev_pos = gpos.len() + 8;
        gpos.extend_from_slice(&3u16.to_be_bytes());
        gpos.extend_from_slice(&200i16.to_be_bytes());
        gpos.extend_from_slice(&75i16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());

        // Coverage filler.
        let cov_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&60u16.to_be_bytes());

        // Shared VariationIndex.
        let vi_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0x8000u16.to_be_bytes());

        // Patch slots.
        gpos[sub_off + 2..sub_off + 4].copy_from_slice(&cov_rel.to_be_bytes());
        gpos[sub_off + 4..sub_off + 6].copy_from_slice(&cov_rel.to_be_bytes());
        gpos[sub_off + 8..sub_off + 10].copy_from_slice(&mark_array_rel.to_be_bytes());
        gpos[sub_off + 10..sub_off + 12].copy_from_slice(&lig_array_rel.to_be_bytes());
        gpos[mark_anchor_off_pos..mark_anchor_off_pos + 2]
            .copy_from_slice(&mark_anchor_rel_to_marray.to_be_bytes());
        gpos[lig_attach_off_pos..lig_attach_off_pos + 2]
            .copy_from_slice(&lig_attach_rel_to_larray.to_be_bytes());
        gpos[comp0_anchor_off_pos..comp0_anchor_off_pos + 2]
            .copy_from_slice(&comp0_anchor_rel_to_la.to_be_bytes());
        gpos[comp1_anchor_off_pos..comp1_anchor_off_pos + 2]
            .copy_from_slice(&comp1_anchor_rel_to_la.to_be_bytes());
        set_x_device(&mut gpos, mark_x_dev_pos, sub_off + vi_rel as usize);
        set_y_device(&mut gpos, mark_y_dev_pos, sub_off + vi_rel as usize);
        set_x_device(&mut gpos, comp0_x_dev_pos, sub_off + vi_rel as usize);
        set_y_device(&mut gpos, comp0_y_dev_pos, sub_off + vi_rel as usize);
        set_x_device(&mut gpos, comp1_x_dev_pos, sub_off + vi_rel as usize);
        set_y_device(&mut gpos, comp1_y_dev_pos, sub_off + vi_rel as usize);

        let ivs_bytes = build_ivs_one_region_one_item(20);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let baked = bake_gpos_at_coords(&gpos, Some(&store), &[1.0]).unwrap();

        // Mark anchor: 5+20=25.
        assert_eq!(
            i16::from_be_bytes([baked[mark_x_pos], baked[mark_x_pos + 1]]),
            25
        );
        // Component 0 anchor: 100+20=120.
        assert_eq!(
            i16::from_be_bytes([baked[comp0_x_pos], baked[comp0_x_pos + 1]]),
            120
        );
        // Component 1 anchor: 200+20=220.
        assert_eq!(
            i16::from_be_bytes([baked[comp1_x_pos], baked[comp1_x_pos + 1]]),
            220
        );
        for off in [
            mark_x_dev_pos,
            mark_y_dev_pos,
            comp0_x_dev_pos,
            comp0_y_dev_pos,
            comp1_x_dev_pos,
            comp1_y_dev_pos,
        ] {
            assert_eq!(u16::from_be_bytes([baked[off], baked[off + 1]]), 0);
        }
    }

    /// MarkMarkPos shares MarkBasePos's shape (two MarkArrays). Single
    /// targeted check that MarkMark dispatches into the same anchor
    /// walk by routing one anchor through the type-6 branch.
    #[test]
    fn mark_mark_pos_anchor_variation_folds() {
        let mut gpos = Vec::new();
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&100u16.to_be_bytes());
        gpos.extend_from_slice(&100u16.to_be_bytes());
        gpos.extend_from_slice(&10u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&4u16.to_be_bytes());
        gpos.extend_from_slice(&6u16.to_be_bytes()); // lookupType=6
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&8u16.to_be_bytes());
        let sub_off = gpos.len();
        gpos.extend_from_slice(&1u16.to_be_bytes()); // posFormat
        gpos.extend_from_slice(&0u16.to_be_bytes()); // mark1CovOff (fill)
        gpos.extend_from_slice(&0u16.to_be_bytes()); // mark2CovOff (fill)
        gpos.extend_from_slice(&1u16.to_be_bytes()); // markClassCount=1
        gpos.extend_from_slice(&0u16.to_be_bytes()); // mark1ArrayOff (fill)
        gpos.extend_from_slice(&0u16.to_be_bytes()); // mark2ArrayOff (fill)

        // Mark1Array.
        let m1_array_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        let m1_anchor_off_pos = gpos.len();
        gpos.extend_from_slice(&0u16.to_be_bytes());
        let m1_anchor_rel_to_array = (gpos.len() - sub_off - m1_array_rel as usize) as u16;
        let m1_x_pos = gpos.len() + 2;
        let m1_x_dev_pos = gpos.len() + 6;
        let m1_y_dev_pos = gpos.len() + 8;
        gpos.extend_from_slice(&3u16.to_be_bytes());
        gpos.extend_from_slice(&7i16.to_be_bytes());
        gpos.extend_from_slice(&8i16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());

        // Mark2Array (same shape as BaseArray).
        let m2_array_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&1u16.to_be_bytes()); // mark2Count
        let m2_anchor_off_pos = gpos.len();
        gpos.extend_from_slice(&0u16.to_be_bytes());
        let m2_anchor_rel_to_array = (gpos.len() - sub_off - m2_array_rel as usize) as u16;
        let m2_x_pos = gpos.len() + 2;
        let m2_x_dev_pos = gpos.len() + 6;
        let m2_y_dev_pos = gpos.len() + 8;
        gpos.extend_from_slice(&3u16.to_be_bytes());
        gpos.extend_from_slice(&77i16.to_be_bytes());
        gpos.extend_from_slice(&88i16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());

        let cov_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&90u16.to_be_bytes());

        let vi_rel = (gpos.len() - sub_off) as u16;
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0x8000u16.to_be_bytes());

        gpos[sub_off + 2..sub_off + 4].copy_from_slice(&cov_rel.to_be_bytes());
        gpos[sub_off + 4..sub_off + 6].copy_from_slice(&cov_rel.to_be_bytes());
        gpos[sub_off + 8..sub_off + 10].copy_from_slice(&m1_array_rel.to_be_bytes());
        gpos[sub_off + 10..sub_off + 12].copy_from_slice(&m2_array_rel.to_be_bytes());
        gpos[m1_anchor_off_pos..m1_anchor_off_pos + 2]
            .copy_from_slice(&m1_anchor_rel_to_array.to_be_bytes());
        gpos[m2_anchor_off_pos..m2_anchor_off_pos + 2]
            .copy_from_slice(&m2_anchor_rel_to_array.to_be_bytes());
        set_x_device(&mut gpos, m1_x_dev_pos, sub_off + vi_rel as usize);
        set_y_device(&mut gpos, m1_y_dev_pos, sub_off + vi_rel as usize);
        set_x_device(&mut gpos, m2_x_dev_pos, sub_off + vi_rel as usize);
        set_y_device(&mut gpos, m2_y_dev_pos, sub_off + vi_rel as usize);

        let ivs_bytes = build_ivs_one_region_one_item(11);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let baked = bake_gpos_at_coords(&gpos, Some(&store), &[1.0]).unwrap();

        assert_eq!(
            i16::from_be_bytes([baked[m1_x_pos], baked[m1_x_pos + 1]]),
            18
        );
        assert_eq!(
            i16::from_be_bytes([baked[m2_x_pos], baked[m2_x_pos + 1]]),
            88
        );
        for off in [m1_x_dev_pos, m1_y_dev_pos, m2_x_dev_pos, m2_y_dev_pos] {
            assert_eq!(u16::from_be_bytes([baked[off], baked[off + 1]]), 0);
        }
    }
}
