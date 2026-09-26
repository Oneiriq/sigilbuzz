//! GPOS variation-bake: folds `VariationIndex` deltas into static
//! `ValueRecord` fields at a chosen coord vector. (#175)
//!
//! # Why
//!
//! Variable-font GPOS subtables carry per-field `Device` /
//! `VariationIndex` sub-offsets. When `deltaFormat == 0x8000` the
//! offset names an `(outer, inner)` row in `GDEF.ItemVariationStore`
//! whose region-weighted delta scales the static `ValueRecord` field
//! at the user's axis coords. Instancing (the operation that
//! collapses a variable font to a static one at a chosen instance)
//! cannot leave those references intact: the `ItemVariationStore`
//! they point at is dropped along with the rest of the variable-font
//! surface.
//!
//! Dropping GDEF.IVS alone would leave the GPOS subtables pointing at
//! orphan offsets. Consumers that resolve a Device/VariationIndex
//! offset off a `ValueRecord` would get garbage; consumers that ignore
//! them (the common case for static pipelines) would see the
//! default-instance value.
//!
//! This module performs the bake instead. For every supported lookup
//! type we walk the subtable's `ValueRecord` byte ranges, look up each
//! `VariationIndex` offset in the source `ItemVariationStore`, resolve
//! the delta at the bake's `coords`, fold the rounded result into the
//! static field (saturating-add), and zero the offset slot so a
//! downstream consumer cannot follow it.
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
//!
//! # Work limit
//!
//! Folding zeroes each offset it follows, so visiting a subtable twice
//! changes nothing. The driver therefore visits each subtable once even
//! when many lookups share it, and charges a [`WorkBudget`] for every
//! record it walks. A GPOS that exhausts the budget is left unbaked.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use sigilbuzz::tables::variation_store::ItemVariationStore;

use crate::util::{WorkBudget, WORK_LIMIT};

/// Defined ValueRecord format bits: bits 0x0001..=0x0080. Mirrors the
/// `DEFINED_BITS` constant in `sigilbuzz::tables::gpos::value_record`.
const VALUE_FORMAT_DEFINED: u16 = 0x00FF;

/// Bit offsets within a ValueRecord, in the order the spec lays them
/// out. The first four (`x_placement` ... `y_advance`) are the static
/// i16 fields; the next four (`x_placement_device` ... `y_advance_device`)
/// are the Offset16 sub-offsets that point at Device / VariationIndex
/// tables relative to the enclosing subtable.
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
const VARIATION_INDEX_DELTA_FORMAT: u16 = 0x8000;

/// Number of bytes a `ValueRecord` with the given format word
/// occupies. Each set defined bit is one i16 (or Offset16, same size).
#[inline]
const fn value_record_size(format: u16) -> usize {
    (format & VALUE_FORMAT_DEFINED).count_ones() as usize * 2
}

/// Reads a big-endian `u16` at `off` as a `usize`, or `None` past the
/// end.
fn read_u16(buf: &[u8], off: usize) -> Option<usize> {
    crate::layout::read_u16(buf, off).map(usize::from)
}

/// Rounds the variation store's float delta to the nearest design-unit
/// integer. Matches the rule the HVAR/MVAR/value_record pipelines use
/// so the four stay in byte-for-byte lockstep. The `as` cast saturates
/// on huge values and maps NaN to 0.
#[must_use]
#[inline]
fn round_delta(delta: f32) -> i32 {
    if delta >= 0.0 {
        (delta + 0.5) as i32
    } else {
        (delta - 0.5) as i32
    }
}

/// Folds the `VariationIndex` referenced by `device_off` into the
/// static i16 field at `static_field_pos` and zeros the offset slot at
/// `device_off_pos`.
///
/// - `subtable_buf` is a mutable slice covering exactly the subtable
///   bytes (same start that `Device/VariationIndex` offsets are
///   relative to).
/// - `static_field_pos` is the byte offset of the static i16 field
///   (`x_placement` / `x_advance` / ...) within `subtable_buf`.
/// - `device_off_pos` is the byte offset of the Offset16 slot within
///   `subtable_buf` (i.e., the position of `x_placement_device` / ...).
///
/// Behavior:
///
/// - If the offset slot is zero (the spec's "absent" sentinel): no-op.
/// - If the offset points past the subtable, the header is malformed,
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
    subtable_buf: &mut [u8],
    static_field_pos: usize,
    device_off_pos: usize,
    store: Option<&ItemVariationStore<'_>>,
    coords: &[f32],
) {
    let Some(slot) = subtable_buf.get_mut(device_off_pos..device_off_pos.saturating_add(2)) else {
        return;
    };
    let device_off = usize::from(u16::from_be_bytes([slot[0], slot[1]]));
    if device_off == 0 {
        return;
    }
    // Always zero the offset, even when we can't resolve the delta:
    // the GDEF.IVS prune that follows would leave it pointing at an
    // orphan otherwise.
    slot.copy_from_slice(&[0, 0]);

    let (Some(outer), Some(inner), Some(delta_format)) = (
        read_u16(subtable_buf, device_off),
        read_u16(subtable_buf, device_off + 2),
        read_u16(subtable_buf, device_off + 4),
    ) else {
        return;
    };
    if delta_format != usize::from(VARIATION_INDEX_DELTA_FORMAT) {
        // Plain Device: leave the static field alone.
        return;
    }

    let Some(store) = store else {
        return;
    };
    let scaled = round_delta(store.delta(outer as u16, inner as u16, coords));
    if scaled == 0 {
        return;
    }
    let Some(field) = subtable_buf.get_mut(static_field_pos..static_field_pos.saturating_add(2))
    else {
        return;
    };
    let cur = i16::from_be_bytes([field[0], field[1]]);
    let new = i32::from(cur)
        .saturating_add(scaled)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX));
    // Clamped to the i16 range just above.
    field.copy_from_slice(&(new as i16).to_be_bytes());
}

/// Folds every defined `VariationIndex` slot in the ValueRecord at
/// `vr_pos` (relative to `subtable_buf`) for the given `format` word.
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
/// set but the paired static field bit is *not*, the spec doesn't
/// define a fold target: we zero the offset slot and skip the
/// static-field write.
pub(crate) fn fold_value_record(
    subtable_buf: &mut [u8],
    vr_pos: usize,
    format: u16,
    store: Option<&ItemVariationStore<'_>>,
    coords: &[f32],
) {
    let mut cursor = vr_pos;
    // Pre-compute static field positions for each of the four pairs.
    // A static field is present at `cursor` only if its bit is set;
    // when absent we record `usize::MAX` and `fold_one_field` falls
    // through to the offset-zero path.
    let mut static_pos = [usize::MAX; 4];
    for (pos, bit) in
        static_pos
            .iter_mut()
            .zip([VR_X_PLACEMENT, VR_Y_PLACEMENT, VR_X_ADVANCE, VR_Y_ADVANCE])
    {
        if format & bit != 0 {
            *pos = cursor;
            cursor += 2;
        }
    }
    for (pos, bit) in static_pos.into_iter().zip([
        VR_X_PLACEMENT_DEVICE,
        VR_Y_PLACEMENT_DEVICE,
        VR_X_ADVANCE_DEVICE,
        VR_Y_ADVANCE_DEVICE,
    ]) {
        if format & bit != 0 {
            fold_one_field(subtable_buf, pos, cursor, store, coords);
            cursor += 2;
        }
    }
}

// ---------------------------------------------------------------------------
// Anchor fold (Mark*/Cursive)
// ---------------------------------------------------------------------------

/// Folds the `VariationIndex` deltas (if any) carried by the Anchor
/// record at `anchor_off` (relative to `subtable_buf`) into the
/// anchor's static `xCoord` / `yCoord` fields, then zeros the device
/// offset slots.
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
/// Only format 3 carries variations: `xDeviceOffset` / `yDeviceOffset`
/// can name a `VariationIndex` (`deltaFormat == 0x8000`) whose
/// region-weighted delta scales the static x/y at the bake's `coords`.
/// Formats 1 and 2 have no variation surface: early return.
///
/// The static-field fold reuses `fold_one_field` so the Anchor and
/// ValueRecord paths stay in byte-for-byte lockstep on the
/// `add-0.5/subtract-0.5` rounding rule and the saturating-add overflow
/// guard.
pub(crate) fn fold_anchor_variations(
    subtable_buf: &mut [u8],
    anchor_off: usize,
    store: Option<&ItemVariationStore<'_>>,
    coords: &[f32],
) {
    if anchor_off == 0 || anchor_off.saturating_add(10) > subtable_buf.len() {
        // Null anchor, or too short for format 3. Formats 1 and 2 have
        // no variation slots anyway.
        return;
    }
    if read_u16(subtable_buf, anchor_off) != Some(3) {
        // Format 1 / 2: no Device/VariationIndex slots. Format 0 or
        // anything > 3 is malformed; ride through.
        return;
    }
    let x_pos = anchor_off + 2;
    let y_pos = anchor_off + 4;
    let x_dev_pos = anchor_off + 6;
    let y_dev_pos = anchor_off + 8;
    fold_one_field(subtable_buf, x_pos, x_dev_pos, store, coords);
    fold_one_field(subtable_buf, y_pos, y_dev_pos, store, coords);
}

/// Shared inputs of one bake pass.
struct Bake<'s, 'c> {
    store: Option<&'s ItemVariationStore<'s>>,
    coords: &'c [f32],
    budget: WorkBudget,
}

impl Bake<'_, '_> {
    /// Charges `units` of work. Returns false once the budget is spent.
    fn spend(&self, units: usize) -> bool {
        self.budget.spend(units)
    }

    fn anchor(&self, buf: &mut [u8], off: usize) {
        fold_anchor_variations(buf, off, self.store, self.coords);
    }

    fn value_record(&self, buf: &mut [u8], pos: usize, format: u16) {
        fold_value_record(buf, pos, format, self.store, self.coords);
    }

    /// Folds one subtable of GPOS lookup type `lookup_type` starting at
    /// `sub_off` within `gpos_buf`.
    fn subtable(&self, gpos_buf: &mut [u8], lookup_type: u16, sub_off: usize) {
        let Some(sub) = gpos_buf.get_mut(sub_off..) else {
            return;
        };
        match lookup_type {
            1 => self.single_pos(sub),
            2 => self.pair_pos(sub),
            3 => self.cursive_pos(sub),
            4 | 6 => self.mark_base_or_mark_pos(sub),
            5 => self.mark_lig_pos(sub),
            _ => {}
        }
    }

    /// Folds every Anchor in a CursivePos subtable.
    ///
    /// Layout (CursivePos format 1):
    /// ```text
    ///   u16 posFormat = 1
    ///   u16 coverageOffset
    ///   u16 entryExitCount
    ///   EntryExitRecord[entryExitCount]:
    ///     u16 entryAnchorOffset
    ///     u16 exitAnchorOffset
    /// ```
    fn cursive_pos(&self, sub: &mut [u8]) {
        if read_u16(sub, 0) != Some(1) {
            return;
        }
        let Some(entry_exit_count) = read_u16(sub, 4) else {
            return;
        };
        let Some(records) = sub.get(6..6 + entry_exit_count * 4) else {
            return;
        };
        if !self.spend(entry_exit_count) {
            return;
        }
        // Collect anchor offsets first so we don't overlap mut/immut borrows.
        let anchor_offs: Vec<usize> = records
            .chunks_exact(2)
            .map(|c| usize::from(u16::from_be_bytes([c[0], c[1]])))
            .collect();
        for off in anchor_offs {
            self.anchor(sub, off);
        }
    }

    /// Walks every Anchor in a `MarkArray` at `mark_array_off` (relative
    /// to `sub`) and folds its variation slots.
    ///
    /// MarkArray layout:
    /// ```text
    ///   u16 markCount
    ///   MarkRecord[markCount]:
    ///     u16 class
    ///     u16 markAnchorOffset (relative to MarkArray start)
    /// ```
    ///
    /// Note that `markAnchorOffset` is relative to the MarkArray, not to
    /// the enclosing subtable. We add `mark_array_off` to land in
    /// subtable-relative space before folding.
    fn mark_array(&self, sub: &mut [u8], mark_array_off: usize) {
        let Some(mark_count) = read_u16(sub, mark_array_off) else {
            return;
        };
        let records_off = mark_array_off + 2;
        let Some(records) = sub.get(records_off..records_off + mark_count * 4) else {
            return;
        };
        if !self.spend(mark_count) {
            return;
        }
        let anchor_offs: Vec<usize> = records
            .chunks_exact(4)
            .map(
                |rec| match usize::from(u16::from_be_bytes([rec[2], rec[3]])) {
                    0 => 0,
                    rel => mark_array_off + rel,
                },
            )
            .collect();
        for off in anchor_offs {
            self.anchor(sub, off);
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
    ///
    /// The flat anchor matrix is `baseCount * markClassCount` u16 offsets,
    /// each relative to `base_array_off`.
    fn base_or_mark2_array(&self, sub: &mut [u8], base_array_off: usize, mark_class_count: usize) {
        let Some(base_count) = read_u16(sub, base_array_off) else {
            return;
        };
        let records_off = base_array_off + 2;
        // Both counts are 16-bit, so the product needs checked math on
        // 32-bit targets.
        let Some(total) = base_count.checked_mul(mark_class_count) else {
            return;
        };
        let Some(records) = total
            .checked_mul(2)
            .and_then(|len| sub.get(records_off..records_off.checked_add(len)?))
        else {
            return;
        };
        if !self.spend(total) {
            return;
        }
        let anchor_offs: Vec<usize> = records
            .chunks_exact(2)
            .map(|c| match usize::from(u16::from_be_bytes([c[0], c[1]])) {
                0 => 0,
                rel => base_array_off + rel,
            })
            .collect();
        for off in anchor_offs {
            self.anchor(sub, off);
        }
    }

    /// Folds every Anchor in a MarkBasePos (type 4) or MarkMarkPos
    /// (type 6) subtable.
    ///
    /// Layout (MarkBasePos / MarkMarkPos format 1):
    /// ```text
    ///   u16 posFormat = 1
    ///   u16 markCoverageOffset     (mark1CoverageOffset)
    ///   u16 baseCoverageOffset     (mark2CoverageOffset)
    ///   u16 markClassCount
    ///   o16 markArrayOffset        (mark1ArrayOffset)
    ///   o16 baseArrayOffset        (mark2ArrayOffset)
    /// ```
    ///
    /// The `Mark2Array` matrix dimensions match `BaseArray`'s:
    /// `mark2Count * markClassCount`.
    fn mark_base_or_mark_pos(&self, sub: &mut [u8]) {
        if sub.len() < 12 || read_u16(sub, 0) != Some(1) {
            return;
        }
        let (Some(mark_class_count), Some(mark_array_off), Some(base_array_off)) =
            (read_u16(sub, 6), read_u16(sub, 8), read_u16(sub, 10))
        else {
            return;
        };
        self.mark_array(sub, mark_array_off);
        self.base_or_mark2_array(sub, base_array_off, mark_class_count);
    }

    /// Folds every Anchor in a MarkLigPos subtable.
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
    fn mark_lig_pos(&self, sub: &mut [u8]) {
        if sub.len() < 12 || read_u16(sub, 0) != Some(1) {
            return;
        }
        let (Some(mark_class_count), Some(mark_array_off), Some(lig_array_off)) =
            (read_u16(sub, 6), read_u16(sub, 8), read_u16(sub, 10))
        else {
            return;
        };

        // MarkArray walks like the other Mark* lookups.
        self.mark_array(sub, mark_array_off);

        // LigatureArray: collect every ligatureAttach offset and every
        // ComponentRecord's anchor offsets, then fold in one pass to keep
        // the borrows simple.
        let Some(lig_count) = read_u16(sub, lig_array_off) else {
            return;
        };
        let lig_attach_offs_start = lig_array_off + 2;
        let Some(attach_offsets) =
            sub.get(lig_attach_offs_start..lig_attach_offs_start + lig_count * 2)
        else {
            return;
        };
        if !self.spend(lig_count) {
            return;
        }
        let lig_attach_abs: Vec<usize> = attach_offsets
            .chunks_exact(2)
            .map(|c| usize::from(u16::from_be_bytes([c[0], c[1]])))
            .filter(|&rel| rel != 0)
            .map(|rel| lig_array_off + rel)
            .collect();

        let row_size = mark_class_count * 2;
        let mut anchor_abs: Vec<usize> = Vec::new();
        for la_off in lig_attach_abs {
            let Some(comp_count) = read_u16(sub, la_off) else {
                continue;
            };
            let comps_off = la_off + 2;
            // Checked for 32-bit targets: both factors come from 16-bit
            // counts.
            let Some(rows) = comp_count
                .checked_mul(row_size)
                .and_then(|len| sub.get(comps_off..comps_off.checked_add(len)?))
            else {
                continue;
            };
            if !self.spend(rows.len() / 2) {
                return;
            }
            // ligatureAnchorOffsets are relative to LigatureAttach.
            anchor_abs.extend(
                rows.chunks_exact(2)
                    .map(|c| usize::from(u16::from_be_bytes([c[0], c[1]])))
                    .filter(|&rel| rel != 0)
                    .map(|rel| la_off + rel),
            );
        }
        for off in anchor_abs {
            self.anchor(sub, off);
        }
    }

    /// Folds every ValueRecord variation slot in a SinglePos subtable.
    fn single_pos(&self, sub: &mut [u8]) {
        let (Some(format), Some(value_format)) = (read_u16(sub, 0), read_u16(sub, 4)) else {
            return;
        };
        let value_format = value_format as u16;
        if value_format & 0x00F0 == 0 {
            // No device-offset fields: no variation work to do.
            return;
        }
        let stride = value_record_size(value_format);
        match format {
            1 => {
                // One shared ValueRecord at offset 6 from subtable start.
                if sub.len() < 6 + stride {
                    return;
                }
                self.value_record(sub, 6, value_format);
            }
            2 => {
                // Per-glyph array at offset 8.
                let Some(value_count) = read_u16(sub, 6) else {
                    return;
                };
                if sub.len() < 8 + value_count * stride || !self.spend(value_count) {
                    return;
                }
                for i in 0..value_count {
                    self.value_record(sub, 8 + i * stride, value_format);
                }
            }
            _ => {}
        }
    }

    /// Folds every ValueRecord variation slot in a PairPos subtable.
    fn pair_pos(&self, sub: &mut [u8]) {
        match read_u16(sub, 0) {
            Some(1) => self.pair_pos_format1(sub),
            Some(2) => self.pair_pos_format2(sub),
            _ => {}
        }
    }

    fn pair_pos_format1(&self, sub: &mut [u8]) {
        let (Some(value_format1), Some(value_format2), Some(pair_set_count)) =
            (read_u16(sub, 4), read_u16(sub, 6), read_u16(sub, 8))
        else {
            return;
        };
        let (value_format1, value_format2) = (value_format1 as u16, value_format2 as u16);
        if (value_format1 | value_format2) & 0x00F0 == 0 {
            return;
        }
        let v1_size = value_record_size(value_format1);
        let v2_size = value_record_size(value_format2);
        let pvr_size = 2 + v1_size + v2_size;
        let set_offsets_off = 10usize;
        // Collect set offsets first, then fold each set in turn: the
        // borrow of `sub` ends here.
        let Some(set_offsets) = sub.get(set_offsets_off..set_offsets_off + pair_set_count * 2)
        else {
            return;
        };
        if !self.spend(pair_set_count) {
            return;
        }
        let set_offs: Vec<usize> = set_offsets
            .chunks_exact(2)
            .map(|c| usize::from(u16::from_be_bytes([c[0], c[1]])))
            .collect();

        for set_off in set_offs {
            let Some(pair_value_count) = read_u16(sub, set_off) else {
                continue;
            };
            if set_off + 2 + pair_value_count * pvr_size > sub.len() {
                continue;
            }
            if !self.spend(pair_value_count) {
                return;
            }
            // Device offsets in format 1 are measured from the
            // PairSet, so the fold works on the PairSet's bytes.
            let Some(set) = sub.get_mut(set_off..) else {
                continue;
            };
            for j in 0..pair_value_count {
                let pvr_off = 2 + j * pvr_size;
                // ValueRecord1 starts after the 2-byte secondGlyph.
                let vr1_pos = pvr_off + 2;
                let vr2_pos = vr1_pos + v1_size;
                self.value_record(set, vr1_pos, value_format1);
                self.value_record(set, vr2_pos, value_format2);
            }
        }
    }

    fn pair_pos_format2(&self, sub: &mut [u8]) {
        if sub.len() < 16 {
            return;
        }
        let (Some(value_format1), Some(value_format2), Some(class1_count), Some(class2_count)) = (
            read_u16(sub, 4),
            read_u16(sub, 6),
            read_u16(sub, 12),
            read_u16(sub, 14),
        ) else {
            return;
        };
        let (value_format1, value_format2) = (value_format1 as u16, value_format2 as u16);
        if (value_format1 | value_format2) & 0x00F0 == 0 {
            return;
        }
        let v1_size = value_record_size(value_format1);
        let v2_size = value_record_size(value_format2);
        let cell_size = v1_size + v2_size;
        let records_off = 16usize;
        // The matrix size can exceed a 32-bit `usize`, so it is checked.
        let Some(cells) = class1_count.checked_mul(class2_count) else {
            return;
        };
        let fits = cells
            .checked_mul(cell_size)
            .and_then(|len| records_off.checked_add(len))
            .is_some_and(|need| need <= sub.len());
        if !fits || !self.spend(cells) {
            return;
        }
        for cell in 0..cells {
            let vr1_pos = records_off + cell * cell_size;
            let vr2_pos = vr1_pos + v1_size;
            self.value_record(sub, vr1_pos, value_format1);
            self.value_record(sub, vr2_pos, value_format2);
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level driver
// ---------------------------------------------------------------------------

/// Walks every lookup in the source GPOS table; for the supported
/// lookup types (SinglePos, PairPos, CursivePos, MarkBasePos,
/// MarkLigPos, MarkMarkPos, and Extension lookups wrapping them) folds
/// every `VariationIndex`-bearing field into its static field at
/// `coords` and zeros the offset slot. Lookup types we do not
/// understand ride through verbatim.
///
/// Returns `Some(new_gpos_bytes)` when the source header parses, else
/// `None` (caller passes through). Also returns `None` when the walk
/// exhausts its work budget. The returned table is byte-for-byte
/// identical to the source for every byte we did not touch. Only the
/// fields we folded into and the offset slots we zeroed change.
pub(crate) fn bake_gpos_at_coords(
    gpos_bytes: &[u8],
    store: Option<&ItemVariationStore<'_>>,
    coords: &[f32],
) -> Option<Vec<u8>> {
    if gpos_bytes.len() < 10 {
        return None;
    }
    if read_u16(gpos_bytes, 0)? != 1 {
        return None;
    }
    let lookup_list_off = read_u16(gpos_bytes, 8)?;
    let lookup_count = read_u16(gpos_bytes, lookup_list_off)?;
    let offsets_start = lookup_list_off + 2;
    let lookup_offsets = gpos_bytes.get(offsets_start..offsets_start + lookup_count * 2)?;

    // Every header and offset is read from the immutable source; the
    // folds write into this copy.
    let mut buf = gpos_bytes.to_vec();
    let bake = Bake {
        store,
        coords,
        budget: WorkBudget::new(WORK_LIMIT),
    };
    // `(subtable offset, lookup type)` pairs already folded.
    let mut visited: BTreeSet<(usize, u16)> = BTreeSet::new();

    for lookup_off in lookup_offsets
        .chunks_exact(2)
        .map(|c| usize::from(u16::from_be_bytes([c[0], c[1]])))
    {
        let lookup_base = lookup_list_off + lookup_off;
        let (Some(lookup_type), Some(subtable_count)) = (
            crate::layout::read_u16(gpos_bytes, lookup_base),
            read_u16(gpos_bytes, lookup_base + 4),
        ) else {
            continue;
        };
        let subtable_offsets_off = lookup_base + 6;
        let Some(subtable_offsets) =
            gpos_bytes.get(subtable_offsets_off..subtable_offsets_off + subtable_count * 2)
        else {
            continue;
        };
        if !bake.spend(1 + subtable_count) {
            return None;
        }
        for sub_rel in subtable_offsets
            .chunks_exact(2)
            .map(|c| usize::from(u16::from_be_bytes([c[0], c[1]])))
        {
            let sub_abs = lookup_base + sub_rel;
            if sub_abs >= gpos_bytes.len() {
                continue;
            }
            // Type 9: Extension. The extension subtable is a 2-byte
            // format + 2-byte extensionLookupType + 4-byte
            // extensionOffset (relative to the extension subtable
            // start). Recurse into the inner subtable so we cover
            // every supported type that font compilers wrap in
            // Extension lookups (common in large GPOS tables).
            //
            // Context (7) / ChainContext (8) are nested rule
            // dispatchers; their nested lookups are reached via the
            // LookupList loop so any anchor variations ride through
            // that path too.
            let (inner_type, inner_abs) = if lookup_type == 9 {
                let Some(header) = gpos_bytes.get(sub_abs..sub_abs + 8) else {
                    continue;
                };
                let ext_type = u16::from_be_bytes([header[2], header[3]]);
                let ext_off =
                    u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
                let Some(inner_abs) = sub_abs.checked_add(ext_off) else {
                    continue;
                };
                (ext_type, inner_abs)
            } else {
                (lookup_type, sub_abs)
            };
            if inner_abs >= gpos_bytes.len() || !visited.insert((inner_abs, inner_type)) {
                continue;
            }
            bake.subtable(&mut buf, inner_type, inner_abs);
        }
    }

    if bake.budget.is_spent() {
        return None;
    }
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

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

        let region_start = out.len() as u32;
        out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_start.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
        out.extend_from_slice(&1u16.to_be_bytes()); // regionCount
                                                    // F2DOT14 (start, peak, end) = (0.0, 1.0, 1.0)
        out.extend_from_slice(&0i16.to_be_bytes());
        out.extend_from_slice(&16384i16.to_be_bytes());
        out.extend_from_slice(&16384i16.to_be_bytes());

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
        fold_one_field(&mut buf, 0, 4, Some(&store), &[1.0]);
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
        fold_one_field(&mut buf, 0, 4, Some(&store), &[1.0]);
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
        fold_one_field(&mut buf, 0, 4, None, &[]);
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
        fold_one_field(&mut buf, 0, 4, None, &[1.0]);
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
        fold_one_field(&mut buf, 0, 4, Some(&store), &[1.0]);
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
        // PairPos format 1 measures device offsets from the PairSet.
        let vi_rel = (gpos.len() - sub_off) as u16 - pair_set_rel;
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0x8000u16.to_be_bytes());

        // Patch slots.
        gpos[sub_off + 2..sub_off + 4].copy_from_slice(&coverage_rel.to_be_bytes());
        gpos[sub_off + 10..sub_off + 12].copy_from_slice(&pair_set_rel.to_be_bytes());
        gpos[device_off_pos..device_off_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());

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
        let vi_rel = (gpos.len() - sub_off) as u16 - pair_set_rel;
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0x8000u16.to_be_bytes());
        gpos[sub_off + 2..sub_off + 4].copy_from_slice(&coverage_rel.to_be_bytes());
        gpos[sub_off + 10..sub_off + 12].copy_from_slice(&pair_set_rel.to_be_bytes());
        gpos[device_off_pos..device_off_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());

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
        buf[x_dev_pos..x_dev_pos + 2].copy_from_slice(&(vi_pos as u16).to_be_bytes());
        buf[y_dev_pos..y_dev_pos + 2].copy_from_slice(&(vi_pos as u16).to_be_bytes());

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
        gpos[entry_x_dev_pos..entry_x_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());
        gpos[entry_y_dev_pos..entry_y_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());
        gpos[exit_x_dev_pos..exit_x_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());
        gpos[exit_y_dev_pos..exit_y_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());

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
        gpos[mark_x_dev_pos..mark_x_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());
        gpos[mark_y_dev_pos..mark_y_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());
        gpos[base_x_dev_pos..base_x_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());
        gpos[base_y_dev_pos..base_y_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());

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
        gpos[mark_x_dev_pos..mark_x_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());
        gpos[mark_y_dev_pos..mark_y_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());
        gpos[comp0_x_dev_pos..comp0_x_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());
        gpos[comp0_y_dev_pos..comp0_y_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());
        gpos[comp1_x_dev_pos..comp1_x_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());
        gpos[comp1_y_dev_pos..comp1_y_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());

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
        gpos[m1_x_dev_pos..m1_x_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());
        gpos[m1_y_dev_pos..m1_y_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());
        gpos[m2_x_dev_pos..m2_x_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());
        gpos[m2_y_dev_pos..m2_y_dev_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());

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

    #[test]
    fn fold_value_record_device_without_static_field_skips_write() {
        // ValueFormat 0x0010: xPlaDevice without xPlacement. The static
        // position is the `usize::MAX` sentinel, which used to overflow
        // (debug) or index out of bounds (release) once the delta was
        // non-zero.
        let mut buf = vec![0u8; 8];
        // ValueRecord at 0: one Offset16 pointing at byte 2.
        buf[0..2].copy_from_slice(&2u16.to_be_bytes());
        // VariationIndex at byte 2: outer=0, inner=0, deltaFormat=0x8000.
        buf[6..8].copy_from_slice(&0x8000u16.to_be_bytes());
        let ivs_bytes = build_ivs_one_region_one_item(80);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        fold_value_record(&mut buf, 0, 0x0010, Some(&store), &[1.0]);
        // The offset slot is zeroed and nothing else changes.
        assert_eq!(&buf[0..2], &[0, 0]);
        assert_eq!(&buf[6..8], &0x8000u16.to_be_bytes());
    }

    #[test]
    fn bake_visits_a_shared_subtable_once() {
        // Two lookup-list entries point at the same SinglePos lookup, so
        // its subtable is reached twice. Folding is idempotent, so the
        // second visit is skipped and the result matches one visit.
        let mut sub = Vec::new();
        sub.extend_from_slice(&1u16.to_be_bytes()); // posFormat 1
        sub.extend_from_slice(&0u16.to_be_bytes()); // coverage (unused)
        sub.extend_from_slice(&0x0011u16.to_be_bytes()); // xPlacement + device
        sub.extend_from_slice(&10i16.to_be_bytes()); // xPlacement
        sub.extend_from_slice(&10u16.to_be_bytes()); // device offset
        sub.extend_from_slice(&0u16.to_be_bytes()); // outer
        sub.extend_from_slice(&0u16.to_be_bytes()); // inner
        sub.extend_from_slice(&0x8000u16.to_be_bytes()); // VariationIndex

        let mut gpos = Vec::new();
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&[0u8; 4]); // script / feature lists unused
        gpos.extend_from_slice(&10u16.to_be_bytes()); // lookupListOffset
                                                      // LookupList at 10: two entries, both at offset 6.
        gpos.extend_from_slice(&2u16.to_be_bytes());
        gpos.extend_from_slice(&6u16.to_be_bytes());
        gpos.extend_from_slice(&6u16.to_be_bytes());
        // Lookup at 16: type 1, flag 0, one subtable at offset 8.
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&8u16.to_be_bytes());
        gpos.extend_from_slice(&sub);

        let ivs_bytes = build_ivs_one_region_one_item(80);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let baked = bake_gpos_at_coords(&gpos, Some(&store), &[1.0]).unwrap();
        let x = i16::from_be_bytes([baked[24 + 6], baked[24 + 7]]);
        assert_eq!(x, 90);
        assert_eq!(&baked[24 + 8..24 + 10], &[0, 0]);
    }
}
