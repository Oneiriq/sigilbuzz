//! GPOS variation-bake — folds `VariationIndex` deltas into static
//! `ValueRecord` fields at a chosen coord vector. (#175)
//!
//! # Why
//!
//! Variable-font GPOS subtables carry per-field `Device` /
//! `VariationIndex` sub-offsets. When `deltaFormat == 0x8000` the
//! offset names an `(outer, inner)` row in `GDEF.ItemVariationStore`
//! whose region-weighted delta scales the static `ValueRecord` field
//! at the user's axis coords. Instancing — the operation that
//! collapses a variable font to a static one at a chosen instance —
//! cannot leave those references intact: the `ItemVariationStore`
//! they point at is dropped along with the rest of the variable-font
//! surface.
//!
//! The first cut (#173) shipped the simple escape-hatch: drop GDEF.IVS
//! and leave the GPOS subtables pointing at orphan offsets. Consumers
//! that resolve a Device/VariationIndex offset off a `ValueRecord` got
//! garbage; consumers that ignore them (the common case for static
//! pipelines) saw the default-instance value.
//!
//! This module replaces that with the proper bake. For every supported
//! lookup type we walk the subtable's `ValueRecord` byte ranges, look
//! up each `VariationIndex` offset in the source `ItemVariationStore`,
//! resolve the delta at the bake's `coords`, fold the rounded result
//! into the static field (saturating-add), and zero the offset slot so
//! a downstream consumer cannot follow it.
//!
//! # Coverage
//!
//! - **PairPos format 1** — explicit pair entries: 2 ValueRecords per
//!   `PairValueRecord`.
//! - **PairPos format 2** — class-pair matrix: 2 ValueRecords per
//!   `Class1Record × Class2Record` cell.
//! - **SinglePos format 1** — uniform `ValueRecord` shared across the
//!   coverage.
//! - **SinglePos format 2** — per-coverage-entry `ValueRecord` array.
//!
//! `MarkBasePos` / `MarkLigPos` / `MarkMarkPos` / `CursivePos` carry
//! `Anchor` records with their own Device/VariationIndex slots. Those
//! are deferred to a follow-up — instance() drops `GDEF.IVS` after the
//! supported lookups are baked, so any `VariationIndex` in a deferred
//! lookup type is left orphan, matching the #173 trade-off for that
//! subset of GPOS.
//!
//! Lookup types we do not bake (`Cursive`, `Mark*`, `Context`,
//! `ChainContext`, `Extension`) ride through verbatim — only their
//! parent table bytes are copied; nested subtables we do not understand
//! are not touched.
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

/// Defined ValueRecord format bits — bits 0x0001..=0x0080. Mirrors the
/// `DEFINED_BITS` constant in `sigilbuzz::tables::gpos::value_record`.
const VALUE_FORMAT_DEFINED: u16 = 0x00FF;

/// Bit offsets within a ValueRecord, in the order the spec lays them
/// out. The first four (`x_placement` … `y_advance`) are the static
/// i16 fields; the next four (`x_placement_device` … `y_advance_device`)
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
/// — repeated here so the bake does not pull a runtime parser dep on
/// the layout module.
const VARIATION_INDEX_DELTA_FORMAT: u16 = 0x8000;

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

/// Folds the `VariationIndex` referenced by `device_off` into the
/// static i16 field at `static_field_pos` and zeros the offset slot at
/// `device_off_pos`.
///
/// - `subtable_buf` is a mutable slice covering exactly the subtable
///   bytes (same start that `Device/VariationIndex` offsets are
///   relative to).
/// - `static_field_pos` is the byte offset of the static i16 field
///   (`x_placement` / `x_advance` / …) within `subtable_buf`.
/// - `device_off_pos` is the byte offset of the Offset16 slot within
///   `subtable_buf` (i.e., the position of `x_placement_device` / …).
///
/// Behaviour:
///
/// - If the offset slot is zero (the spec's "absent" sentinel) — no-op.
/// - If the offset points past the subtable, the header is malformed,
///   or the referenced table is a `Device` (per-ppem hinting, not a
///   `VariationIndex`) — zero the offset slot and leave the static
///   field alone. Folding a Device into the static field would
///   silently warp design-unit metrics; the "ship as static" intent is
///   to preserve them.
/// - If the referenced table is a `VariationIndex` and `store` is
///   `Some`, look up `(outer, inner)`, resolve at `coords`, round, and
///   saturating-add to the static field. Then zero the offset slot.
/// - If the referenced table is a `VariationIndex` but `store` is
///   `None` (font has GPOS variations but no GDEF.IVS — malformed) —
///   zero the offset slot, leave the static field alone.
fn fold_one_field(
    subtable_buf: &mut [u8],
    static_field_pos: usize,
    device_off_pos: usize,
    store: Option<&ItemVariationStore<'_>>,
    coords: &[f32],
) {
    if device_off_pos + 2 > subtable_buf.len() {
        return;
    }
    let device_off =
        u16::from_be_bytes([subtable_buf[device_off_pos], subtable_buf[device_off_pos + 1]])
            as usize;
    if device_off == 0 {
        return;
    }
    // Always zero the offset, even when we can't resolve the delta —
    // the GDEF.IVS prune that follows would leave it pointing at an
    // orphan otherwise.
    subtable_buf[device_off_pos] = 0;
    subtable_buf[device_off_pos + 1] = 0;

    if device_off + 6 > subtable_buf.len() {
        return;
    }
    let first = u16::from_be_bytes([subtable_buf[device_off], subtable_buf[device_off + 1]]);
    let second =
        u16::from_be_bytes([subtable_buf[device_off + 2], subtable_buf[device_off + 3]]);
    let delta_format =
        u16::from_be_bytes([subtable_buf[device_off + 4], subtable_buf[device_off + 5]]);
    if delta_format != VARIATION_INDEX_DELTA_FORMAT {
        // Plain Device — leave the static field alone.
        return;
    }
    let outer = first;
    let inner = second;

    let Some(store) = store else {
        return;
    };
    let scaled = round_delta(store.delta(outer, inner, coords));
    if scaled == 0 || static_field_pos + 2 > subtable_buf.len() {
        return;
    }
    let cur = i16::from_be_bytes([subtable_buf[static_field_pos], subtable_buf[static_field_pos + 1]]);
    let new = i32::from(cur).saturating_add(scaled).clamp(
        i32::from(i16::MIN),
        i32::from(i16::MAX),
    );
    #[allow(clippy::cast_possible_truncation)]
    let new_i16 = new as i16;
    subtable_buf[static_field_pos..static_field_pos + 2].copy_from_slice(&new_i16.to_be_bytes());
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
/// `0x0010 ↔ 0x0001`, `0x0020 ↔ 0x0002`, `0x0040 ↔ 0x0004`,
/// `0x0080 ↔ 0x0008`. When a device-offset bit is set but the paired
/// static field bit is *not*, the spec doesn't define a fold target —
/// we zero the offset slot and skip the static-field write.
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
    let x_pl_pos = if format & VR_X_PLACEMENT != 0 {
        let p = cursor;
        cursor += 2;
        p
    } else {
        usize::MAX
    };
    let y_pl_pos = if format & VR_Y_PLACEMENT != 0 {
        let p = cursor;
        cursor += 2;
        p
    } else {
        usize::MAX
    };
    let x_ad_pos = if format & VR_X_ADVANCE != 0 {
        let p = cursor;
        cursor += 2;
        p
    } else {
        usize::MAX
    };
    let y_ad_pos = if format & VR_Y_ADVANCE != 0 {
        let p = cursor;
        cursor += 2;
        p
    } else {
        usize::MAX
    };

    if format & VR_X_PLACEMENT_DEVICE != 0 {
        fold_one_field(subtable_buf, x_pl_pos, cursor, store, coords);
        cursor += 2;
    }
    if format & VR_Y_PLACEMENT_DEVICE != 0 {
        fold_one_field(subtable_buf, y_pl_pos, cursor, store, coords);
        cursor += 2;
    }
    if format & VR_X_ADVANCE_DEVICE != 0 {
        fold_one_field(subtable_buf, x_ad_pos, cursor, store, coords);
        cursor += 2;
    }
    if format & VR_Y_ADVANCE_DEVICE != 0 {
        fold_one_field(subtable_buf, y_ad_pos, cursor, store, coords);
        // No more after y_advance_device; cursor unused.
        let _ = cursor;
    }
}

// ---------------------------------------------------------------------------
// Per-lookup-type fold
// ---------------------------------------------------------------------------

/// Folds every ValueRecord variation slot in a SinglePos subtable
/// starting at `sub_off` within `gpos_buf`.
fn fold_single_pos(
    gpos_buf: &mut [u8],
    sub_off: usize,
    store: Option<&ItemVariationStore<'_>>,
    coords: &[f32],
) {
    let Some(sub) = gpos_buf.get(sub_off..) else {
        return;
    };
    if sub.len() < 6 {
        return;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    let value_format = u16::from_be_bytes([sub[4], sub[5]]);
    if value_format & 0x00F0 == 0 {
        // No device-offset fields — no variation work to do.
        return;
    }
    let stride = value_record_size(value_format);
    match format {
        1 => {
            // One shared ValueRecord at offset 6 from subtable start.
            if sub.len() < 6 + stride {
                return;
            }
            fold_value_record(
                &mut gpos_buf[sub_off..],
                6,
                value_format,
                store,
                coords,
            );
        }
        2 => {
            // Per-glyph array at offset 8.
            if sub.len() < 8 {
                return;
            }
            let value_count =
                u16::from_be_bytes([sub[6], sub[7]]) as usize;
            let need = 8 + value_count * stride;
            if sub.len() < need {
                return;
            }
            for i in 0..value_count {
                let vr = 8 + i * stride;
                fold_value_record(
                    &mut gpos_buf[sub_off..],
                    vr,
                    value_format,
                    store,
                    coords,
                );
            }
        }
        _ => {}
    }
}

/// Folds every ValueRecord variation slot in a PairPos subtable
/// starting at `sub_off` within `gpos_buf`.
fn fold_pair_pos(
    gpos_buf: &mut [u8],
    sub_off: usize,
    store: Option<&ItemVariationStore<'_>>,
    coords: &[f32],
) {
    let Some(sub) = gpos_buf.get(sub_off..) else {
        return;
    };
    if sub.len() < 2 {
        return;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    match format {
        1 => fold_pair_pos_format1(gpos_buf, sub_off, store, coords),
        2 => fold_pair_pos_format2(gpos_buf, sub_off, store, coords),
        _ => {}
    }
}

fn fold_pair_pos_format1(
    gpos_buf: &mut [u8],
    sub_off: usize,
    store: Option<&ItemVariationStore<'_>>,
    coords: &[f32],
) {
    let sub_len = gpos_buf.len() - sub_off;
    let sub = &gpos_buf[sub_off..];
    if sub.len() < 10 {
        return;
    }
    let value_format1 = u16::from_be_bytes([sub[4], sub[5]]);
    let value_format2 = u16::from_be_bytes([sub[6], sub[7]]);
    let pair_set_count = u16::from_be_bytes([sub[8], sub[9]]) as usize;
    if (value_format1 | value_format2) & 0x00F0 == 0 {
        return;
    }
    let v1_size = value_record_size(value_format1);
    let v2_size = value_record_size(value_format2);
    let pvr_size = 2 + v1_size + v2_size;
    let set_offsets_off = 10usize;
    if sub.len() < set_offsets_off + pair_set_count * 2 {
        return;
    }
    // Collect set offsets first, then fold each set in turn — the
    // borrow of `sub` ends here.
    let mut set_offs: Vec<usize> = Vec::with_capacity(pair_set_count);
    for i in 0..pair_set_count {
        let off_off = set_offsets_off + i * 2;
        let set_off = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        set_offs.push(set_off);
    }

    for set_off in set_offs {
        if set_off >= sub_len || set_off + 2 > sub_len {
            continue;
        }
        let pair_value_count =
            u16::from_be_bytes([gpos_buf[sub_off + set_off], gpos_buf[sub_off + set_off + 1]])
                as usize;
        let need = 2 + pair_value_count * pvr_size;
        if set_off + need > sub_len {
            continue;
        }
        for j in 0..pair_value_count {
            let pvr_off = set_off + 2 + j * pvr_size;
            // ValueRecord1 starts after the 2-byte secondGlyph.
            let vr1_pos = pvr_off + 2;
            let vr2_pos = vr1_pos + v1_size;
            fold_value_record(
                &mut gpos_buf[sub_off..],
                vr1_pos,
                value_format1,
                store,
                coords,
            );
            fold_value_record(
                &mut gpos_buf[sub_off..],
                vr2_pos,
                value_format2,
                store,
                coords,
            );
        }
    }
}

fn fold_pair_pos_format2(
    gpos_buf: &mut [u8],
    sub_off: usize,
    store: Option<&ItemVariationStore<'_>>,
    coords: &[f32],
) {
    let sub = &gpos_buf[sub_off..];
    if sub.len() < 16 {
        return;
    }
    let value_format1 = u16::from_be_bytes([sub[4], sub[5]]);
    let value_format2 = u16::from_be_bytes([sub[6], sub[7]]);
    let class1_count = u16::from_be_bytes([sub[12], sub[13]]) as usize;
    let class2_count = u16::from_be_bytes([sub[14], sub[15]]) as usize;
    if (value_format1 | value_format2) & 0x00F0 == 0 {
        return;
    }
    let v1_size = value_record_size(value_format1);
    let v2_size = value_record_size(value_format2);
    let cell_size = v1_size + v2_size;
    let row_size = class2_count * cell_size;
    let records_off = 16usize;
    let need = records_off + class1_count * row_size;
    if sub.len() < need {
        return;
    }
    for i in 0..class1_count {
        for j in 0..class2_count {
            let cell_off = records_off + i * row_size + j * cell_size;
            let vr1_pos = cell_off;
            let vr2_pos = cell_off + v1_size;
            fold_value_record(
                &mut gpos_buf[sub_off..],
                vr1_pos,
                value_format1,
                store,
                coords,
            );
            fold_value_record(
                &mut gpos_buf[sub_off..],
                vr2_pos,
                value_format2,
                store,
                coords,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level driver
// ---------------------------------------------------------------------------

/// Walks every lookup in the source GPOS table; for the supported
/// lookup types (Type 1 SinglePos, Type 2 PairPos) folds every
/// `VariationIndex`-bearing ValueRecord field into the static field
/// at `coords` and zeros the offset slot. Lookup types we do not
/// understand ride through verbatim.
///
/// Returns `Some(new_gpos_bytes)` when the source carries GPOS, else
/// `None` (caller passes through). The returned table is byte-for-byte
/// identical to the source for every byte we did not touch — only the
/// fields we folded into and the offset slots we zeroed change.
pub(crate) fn bake_gpos_at_coords(
    gpos_bytes: &[u8],
    store: Option<&ItemVariationStore<'_>>,
    coords: &[f32],
) -> Option<Vec<u8>> {
    if gpos_bytes.len() < 10 {
        return None;
    }
    let major = u16::from_be_bytes([gpos_bytes[0], gpos_bytes[1]]);
    if major != 1 {
        return None;
    }
    let lookup_list_off =
        u16::from_be_bytes([gpos_bytes[8], gpos_bytes[9]]) as usize;
    if lookup_list_off + 2 > gpos_bytes.len() {
        return None;
    }
    let lookup_count = u16::from_be_bytes([
        gpos_bytes[lookup_list_off],
        gpos_bytes[lookup_list_off + 1],
    ]) as usize;
    let offsets_start = lookup_list_off + 2;
    if offsets_start + lookup_count * 2 > gpos_bytes.len() {
        return None;
    }

    // Collect (lookup_type, lookup_base, [subtable_abs_off, …]) for
    // every lookup. We do not patch context/chain/extension lookups —
    // those are passed through.
    let mut buf = gpos_bytes.to_vec();

    // Re-read everything from the immutable copy so we don't accidentally
    // mutate while we are walking the headers.
    for li in 0..lookup_count {
        let off_pos = offsets_start + li * 2;
        let lookup_off =
            u16::from_be_bytes([gpos_bytes[off_pos], gpos_bytes[off_pos + 1]]) as usize;
        let lookup_base = lookup_list_off + lookup_off;
        if lookup_base + 6 > gpos_bytes.len() {
            continue;
        }
        let lookup_type =
            u16::from_be_bytes([gpos_bytes[lookup_base], gpos_bytes[lookup_base + 1]]);
        let subtable_count = u16::from_be_bytes([
            gpos_bytes[lookup_base + 4],
            gpos_bytes[lookup_base + 5],
        ]) as usize;
        let subtable_offsets_off = lookup_base + 6;
        if subtable_offsets_off + subtable_count * 2 > gpos_bytes.len() {
            continue;
        }
        for si in 0..subtable_count {
            let so_pos = subtable_offsets_off + si * 2;
            let sub_rel = u16::from_be_bytes([gpos_bytes[so_pos], gpos_bytes[so_pos + 1]])
                as usize;
            let sub_abs = lookup_base + sub_rel;
            if sub_abs >= gpos_bytes.len() {
                continue;
            }
            match lookup_type {
                // Type 1 — SinglePos.
                1 => fold_single_pos(&mut buf, sub_abs, store, coords),
                // Type 2 — PairPos.
                2 => fold_pair_pos(&mut buf, sub_abs, store, coords),
                // Type 9 — Extension. The extension subtable is a
                // 2-byte format + 2-byte extensionLookupType + 4-byte
                // extensionOffset (relative to the extension subtable
                // start). Recurse into the inner subtable so we cover
                // PairPos/SinglePos that font compilers wrap in
                // Extension lookups (common in large GPOS tables).
                9 => {
                    if sub_abs + 8 > gpos_bytes.len() {
                        continue;
                    }
                    let ext_type = u16::from_be_bytes([
                        gpos_bytes[sub_abs + 2],
                        gpos_bytes[sub_abs + 3],
                    ]);
                    let ext_off = u32::from_be_bytes([
                        gpos_bytes[sub_abs + 4],
                        gpos_bytes[sub_abs + 5],
                        gpos_bytes[sub_abs + 6],
                        gpos_bytes[sub_abs + 7],
                    ]) as usize;
                    let inner_abs = sub_abs + ext_off;
                    if inner_abs >= gpos_bytes.len() {
                        continue;
                    }
                    match ext_type {
                        1 => fold_single_pos(&mut buf, inner_abs, store, coords),
                        2 => fold_pair_pos(&mut buf, inner_abs, store, coords),
                        _ => {}
                    }
                }
                // Mark*/Cursive/Context/ChainContext — deferred. The
                // anchors and nested lookups in these types may carry
                // VariationIndex offsets, but the GDEF.IVS prune that
                // follows leaves them orphan. Tracked in the #175
                // follow-up.
                _ => {}
            }
        }
    }

    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        fold_one_field(&mut buf, 0, 4, Some(&store), &[1.0]);
        let cur = i16::from_be_bytes([buf[0], buf[1]]);
        assert_eq!(cur, 100);
    }

    #[test]
    fn fold_one_field_resolves_variation_index_and_zeros_offset() {
        // Static field at 0..2 = 50; offset slot at 4..6 = 8 (points
        // at the VariationIndex header at byte 8). At coord 1.0 the
        // delta is 80 → 50 + 80 = 130. After fold the offset slot is
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
        // Device-shape (deltaFormat = 3) — must zero the offset but
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
        // delta = 30000 → 30000 + 30000 saturates at i16::MAX (32767).
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
        // PairPos at 22 — sub_off = 22.
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
        // Will fill device_off below — points at the VariationIndex
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
        gpos[device_off_pos..device_off_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());

        // Build IVS and run the bake at coord 1.0.
        let ivs_bytes = build_ivs_one_region_one_item(75);
        let store = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let baked = bake_gpos_at_coords(&gpos, Some(&store), &[1.0]).unwrap();

        // x_advance: -50 + 75 = 25.
        let baked_x =
            i16::from_be_bytes([baked[x_advance_pos], baked[x_advance_pos + 1]]);
        assert_eq!(baked_x, 25);
        // device offset slot zeroed.
        let baked_off =
            u16::from_be_bytes([baked[device_off_pos], baked[device_off_pos + 1]]);
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
    /// "GDEF.IVS will be pruned next" path — leaving the offsets
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
        gpos[device_off_pos..device_off_pos + 2].copy_from_slice(&vi_rel.to_be_bytes());

        let baked = bake_gpos_at_coords(&gpos, None, &[1.0]).unwrap();
        // Static field unchanged.
        let baked_x =
            i16::from_be_bytes([baked[x_advance_pos], baked[x_advance_pos + 1]]);
        assert_eq!(baked_x, -50);
        // Offset zeroed.
        let baked_off =
            u16::from_be_bytes([baked[device_off_pos], baked[device_off_pos + 1]]);
        assert_eq!(baked_off, 0);
    }
}
