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
//! Every `VariationIndex` resolution rounds the delta halves up (see
//! [`round_delta`]), as HarfBuzz's and fontTools' instancers and the
//! HVAR, MVAR and BASE bakes do; the instancer passes the coordinates
//! HarfBuzz's instancer resolves the store at. Saturating addition
//! guards against ValueRecord field overflow on extreme coords.
//!
//! # Work limit
//!
//! Every visitor of the walk is idempotent: folding or clearing an
//! offset zeroes it, and renumbering a shared table is decided once.
//! So the walk visits each subtable once even when many lookups share
//! it, and charges a [`WorkBudget`] for every record it walks. A GPOS
//! that exhausts the budget stops the walk, and a bake of it is left
//! undone.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use crate::util::{StoreDeltas, WorkBudget, WORK_LIMIT};

mod anchors;
mod value_records;

use anchors::{walk_cursive_pos, walk_mark_base_or_mark_pos, walk_mark_lig_pos};
use value_records::{walk_pair_pos, walk_single_pos};

/// `deltaFormat` sentinel that turns a Device-shaped offset into a
/// `VariationIndex`. Mirrors
/// `sigilbuzz::tables::layout::device::VARIATION_INDEX_DELTA_FORMAT`
/// and is repeated here so the bake does not pull a runtime parser dep on
/// the layout module.
pub(crate) const VARIATION_INDEX_DELTA_FORMAT: u16 = 0x8000;

/// Rounds the variation store's float delta to the nearest design-unit
/// integer, halves up, as HarfBuzz's instancer (`roundf`, which it
/// defines as `floor (x + 0.5)`) and fontTools' (`otRound`) round the
/// deltas they fold into GPOS values, anchors and GDEF carets.
#[must_use]
#[inline]
fn round_delta(delta: f32) -> i32 {
    crate::util::round_half_up(delta)
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
pub(crate) fn fold_one_field(
    buf: &mut [u8],
    slot: DeviceSlot,
    store: Option<&StoreDeltas<'_, '_>>,
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
    let scaled = round_delta(store.get(outer, inner));
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

// ---------------------------------------------------------------------------
// Top-level driver
// ---------------------------------------------------------------------------

/// Walks one subtable of the given (non-extension) lookup type.
fn walk_subtable(
    buf: &mut [u8],
    lookup_type: u16,
    sub_abs: usize,
    visit: &mut SlotVisitor<'_>,
    budget: &WorkBudget,
) {
    match lookup_type {
        1 => walk_single_pos(buf, sub_abs, visit, budget),
        2 => walk_pair_pos(buf, sub_abs, visit, budget),
        3 => walk_cursive_pos(buf, sub_abs, visit, budget),
        4 | 6 => walk_mark_base_or_mark_pos(buf, sub_abs, visit, budget),
        5 => walk_mark_lig_pos(buf, sub_abs, visit, budget),
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
/// identical anchors) is reported once per referencing record, but a
/// subtable shared by several lookups is walked once (see the module
/// docs).
///
/// Returns `false` when the GPOS header or LookupList is malformed and
/// nothing was walked, or when the walk ran out of its work budget and
/// stopped part way.
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
    let budget = WorkBudget::new(WORK_LIMIT);
    // `(subtable position, lookup type)` pairs already walked.
    let mut visited: BTreeSet<(usize, u16)> = BTreeSet::new();
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
        if !budget.spend(1 + usize::from(subtable_count)) {
            return false;
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
                if visited.insert((sub_abs, lookup_type)) {
                    walk_subtable(gpos, lookup_type, sub_abs, visit, &budget);
                }
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
            if inner_abs < gpos.len() && ext_type != 9 && visited.insert((inner_abs, ext_type)) {
                walk_subtable(gpos, ext_type, inner_abs, visit, &budget);
            }
        }
    }
    !budget.is_spent()
}

/// Folds every supported `VariationIndex` in the source GPOS into the
/// static field it adjusts, by the store `deltas` (at the instance's
/// coordinates), and zeros the offset slot. Lookup types we do not
/// understand ride through verbatim.
///
/// Returns `Some(new_gpos_bytes)` when the source carries a parseable
/// GPOS header, else `None` (caller passes through). The returned
/// table is byte-for-byte identical to the source for every byte we
/// did not touch. Only the fields we folded into and the offset slots
/// we zeroed change.
pub(crate) fn bake_gpos_at_coords(
    gpos_bytes: &[u8],
    deltas: Option<&StoreDeltas<'_, '_>>,
) -> Option<Vec<u8>> {
    let mut buf = gpos_bytes.to_vec();
    let walked = walk_gpos_device_slots(&mut buf, &mut |b, slot| {
        fold_one_field(b, slot, deltas);
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
mod tests;
