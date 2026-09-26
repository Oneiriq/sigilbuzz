//! The anchor side of the GPOS device-slot walk: CursivePos entry and
//! exit anchors and the MarkArray, BaseArray, Mark2Array and
//! LigatureArray anchors of the mark attachment lookups. AnchorFormat3
//! measures its device offsets from the Anchor itself.

use alloc::vec::Vec;

use super::{read_u16, DeviceSlot, SlotVisitor};
use crate::util::WorkBudget;

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
pub(super) fn visit_anchor(buf: &mut [u8], anchor_off: usize, visit: &mut SlotVisitor<'_>) {
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
pub(super) fn walk_cursive_pos(
    gpos_buf: &mut [u8],
    sub_off: usize,
    visit: &mut SlotVisitor<'_>,
    budget: &WorkBudget,
) {
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
    if sub.len() < records_off + entry_exit_count * 4 || !budget.spend(entry_exit_count) {
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
fn walk_mark_array(
    sub: &mut [u8],
    mark_array_off: usize,
    visit: &mut SlotVisitor<'_>,
    budget: &WorkBudget,
) {
    let Some(mark_count) = read_u16(sub, mark_array_off).map(usize::from) else {
        return;
    };
    let records_off = mark_array_off + 2;
    if records_off + mark_count * 4 > sub.len() || !budget.spend(mark_count) {
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
    budget: &WorkBudget,
) {
    let Some(base_count) = read_u16(sub, base_array_off).map(usize::from) else {
        return;
    };
    let records_off = base_array_off + 2;
    // Both counts are 16-bit, so the product needs checked math on
    // 32-bit targets.
    let Some(total) = base_count.checked_mul(mark_class_count) else {
        return;
    };
    let fits = total
        .checked_mul(2)
        .and_then(|len| records_off.checked_add(len))
        .is_some_and(|end| end <= sub.len());
    if !fits || !budget.spend(total) {
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
pub(super) fn walk_mark_base_or_mark_pos(
    gpos_buf: &mut [u8],
    sub_off: usize,
    visit: &mut SlotVisitor<'_>,
    budget: &WorkBudget,
) {
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
    walk_mark_array(sub, mark_array_off as usize, visit, budget);
    walk_base_or_mark2_array(sub, base_array_off as usize, mcc as usize, visit, budget);
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
pub(super) fn walk_mark_lig_pos(
    gpos_buf: &mut [u8],
    sub_off: usize,
    visit: &mut SlotVisitor<'_>,
    budget: &WorkBudget,
) {
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
    walk_mark_array(sub, mark_array_off as usize, visit, budget);

    // LigatureArray: collect every ComponentRecord's anchor offsets,
    // then visit them in one pass to keep the borrows simple.
    let Some(lig_count) = read_u16(sub, lig_array_off).map(usize::from) else {
        return;
    };
    let lig_attach_offs_start = lig_array_off + 2;
    if lig_attach_offs_start + lig_count * 2 > sub.len() || !budget.spend(lig_count) {
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
        // Checked for 32-bit targets: both factors come from 16-bit
        // counts.
        let Some(anchors) = comp_count.checked_mul(mark_class_count) else {
            continue;
        };
        let fits = anchors
            .checked_mul(2)
            .and_then(|len| comps_off.checked_add(len))
            .is_some_and(|end| end <= sub.len());
        if !fits {
            continue;
        }
        if !budget.spend(anchors) {
            return;
        }
        for k in 0..anchors {
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
