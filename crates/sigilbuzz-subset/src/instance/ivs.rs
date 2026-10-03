//! The ItemVariationStore projection of a partial instance: regions
//! trimmed to the kept axes, deltas scaled by the pinned axes, and the
//! row remap every table that names the store follows.

use alloc::vec::Vec;

use super::region::project_region_onto_kept_axes;
use super::AxisPin;
use crate::read;
use crate::SubsetError;

// ---------------------------------------------------------------------------
// Partial-instancing ItemVariationStore rewrite
// ---------------------------------------------------------------------------

/// Maps `(old_outer, old_inner)` source IVS rows to their new
/// `(new_outer, new_inner)` indexes after a partial-instance rewrite.
/// `None` means the source row exists but its surrounding subtable
/// collapsed to nothing (every region dropped). Callers must treat
/// the row as "no variation" and leave the consumer field at its
/// static value.
#[derive(Debug, Clone, Default)]
pub(crate) struct RegionRemap {
    /// Per-source-outer entries. Each entry is either:
    /// - `Some(new_outer)`: the subtable survives at this index, with
    ///   the same item rows in the same order. The new subtable's
    ///   `region_indexes.len()` may differ (regions are dropped /
    ///   trimmed), but `inner` indices are preserved verbatim because
    ///   the partial-instance pass never reorders or drops rows.
    /// - `None`: every region the subtable referenced was dropped, so
    ///   the subtable was elided. Consumers reading via `(outer, inner)`
    ///   resolve to a delta of zero.
    new_outer_for_old: Vec<Option<u16>>,
}

impl RegionRemap {
    /// Returns the new (outer, inner) for an old row, or `None` when
    /// the surrounding subtable collapsed.
    pub(crate) fn lookup(&self, old_outer: u16, old_inner: u16) -> Option<(u16, u16)> {
        let new_outer = (*self.new_outer_for_old.get(old_outer as usize)?)?;
        Some((new_outer, old_inner))
    }
}

/// Converts a raw F2DOT14 to its value.
fn f2dot14(raw: [u8; 2]) -> f32 {
    f32::from(i16::from_be_bytes(raw)) / 16384.0
}

/// Writes an F2DOT14 to a byte vector.
fn write_f2dot14_bytes(out: &mut Vec<u8>, v: f32) {
    let raw = (v * 16384.0)
        .round()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
    out.extend_from_slice(&raw.to_be_bytes());
}

/// Narrows the position of a table written into a rebuilt parent to
/// the Offset32 that points at it, or reports that the parent outgrew
/// 32-bit offsets.
pub(super) fn offset32(pos: usize, what: &'static str) -> Result<u32, SubsetError> {
    u32::try_from(pos).map_err(|_| SubsetError::Unsupported(what))
}

/// Moves the byte offset of a parse error found in a sub-table that
/// starts `by` bytes into its host table, so it counts from the host.
pub(super) fn shifted(err: SubsetError, by: usize) -> SubsetError {
    match err {
        SubsetError::Parse(sigilbuzz::Error::Truncated { offset, context }) => {
            SubsetError::Parse(sigilbuzz::Error::Truncated {
                offset: offset.saturating_add(by),
                context,
            })
        }
        SubsetError::Parse(sigilbuzz::Error::Malformed { offset, context }) => {
            SubsetError::Parse(sigilbuzz::Error::Malformed {
                offset: offset.saturating_add(by),
                context,
            })
        }
        other => other,
    }
}

/// [`project_ivs`] as an `Option`, for callers that treat any failure
/// alike (the CFF2 VarStore bake reports its own error).
pub(crate) fn bake_ivs_partial(
    ivs_bytes: &[u8],
    coords: &[f32],
    pins: &[AxisPin],
) -> Option<(Vec<u8>, RegionRemap)> {
    project_ivs(ivs_bytes, coords, pins).ok()
}

/// Re-emits an `ItemVariationStore` with every Pin-axis dimension
/// folded into the surviving deltas, returning the new store and the
/// row remap.
///
/// The output IVS uses the same format-1 layout: a region list with
/// only the Keep-axis dimensions, plus one `ItemVariationData` per
/// surviving source subtable. Subtables whose region list collapses
/// entirely (every region drops at the pin coords, or there are no
/// rows) are elided (see `RegionRemap`); when all of them do, the
/// store is empty and every row reads as zero delta.
///
/// # Errors
///
/// A parse error, measured from the start of `ivs_bytes`, when the
/// store is malformed, not format 1, has an axis count that differs
/// from `pins`, has subtables that overlap so heavily that projecting
/// them would read far more bytes than the store holds, or has a
/// subtable that keeps more than 32,767 regions, more than its
/// all-wide rewrite can count.
/// [`SubsetError::Unsupported`] when the projected store
/// outgrows its Offset32s. Offsets and sizes are checked, so a crafted
/// Offset32 cannot wrap a 32-bit `usize`.
pub(crate) fn project_ivs(
    ivs_bytes: &[u8],
    coords: &[f32],
    pins: &[AxisPin],
) -> Result<(Vec<u8>, RegionRemap), SubsetError> {
    const CTX: &str = "ItemVariationStore truncated";
    const OFFSET: &str = "ItemVariationStore offset past the end";
    // Subtable offsets may alias one large subtable, and each offset is
    // projected on its own, so the output could grow without bound. The
    // walk reads at most a few times the store's size. The subtables of
    // a well-formed store occupy disjoint spans and never reach that.
    const ALIASED: sigilbuzz::Error = sigilbuzz::Error::Malformed {
        offset: 6,
        context: "ItemVariationStore subtables overlap too much to project",
    };
    if read::u16_at(ivs_bytes, 0, CTX)? != 1 {
        return Err(sigilbuzz::Error::Malformed {
            offset: 0,
            context: "unsupported ItemVariationStore format",
        }
        .into());
    }
    let region_list_off = read::offset32_at(ivs_bytes, 2, 0, OFFSET)?;
    let subtable_count = usize::from(read::u16_at(ivs_bytes, 6, CTX)?);
    let mut subtable_offsets: Vec<Option<usize>> = Vec::with_capacity(subtable_count);
    for i in 0..subtable_count {
        let slot = 8 + i * 4;
        // A null ItemVariationData offset names an empty subtable.
        subtable_offsets.push(if read::u32_at(ivs_bytes, slot, CTX)? == 0 {
            None
        } else {
            Some(read::offset32_at(ivs_bytes, slot, 0, OFFSET)?)
        });
    }

    let axis_count = usize::from(read::u16_at(ivs_bytes, region_list_off, CTX)?);
    let region_count = usize::from(read::u16_at(ivs_bytes, region_list_off + 2, CTX)?);
    if pins.len() != axis_count || coords.len() != axis_count {
        return Err(sigilbuzz::Error::Malformed {
            offset: region_list_off,
            context: "ItemVariationStore axisCount differs from the fvar axis count",
        }
        .into());
    }
    let region_size = axis_count * 6;
    let regions = read::array_at(
        ivs_bytes,
        region_list_off + 4,
        region_count,
        region_size,
        CTX,
    )?;

    // Project each region. None -> dropped; Some((new_index, scalar)).
    let mut region_remap: Vec<Option<(u16, f32)>> = Vec::with_capacity(region_count);
    let mut new_regions: Vec<Vec<(f32, f32, f32)>> = Vec::new();
    for ri in 0..region_count {
        let record = &regions[ri * region_size..(ri + 1) * region_size];
        let region: Vec<(f32, f32, f32)> = record
            .chunks_exact(6)
            .map(|axis| {
                (
                    f2dot14([axis[0], axis[1]]),
                    f2dot14([axis[2], axis[3]]),
                    f2dot14([axis[4], axis[5]]),
                )
            })
            .collect();
        match project_region_onto_kept_axes(&region, pins, coords) {
            Some(p) => {
                let new_idx = new_regions.len() as u16;
                new_regions.push(p.kept_axes);
                region_remap.push(Some((new_idx, p.pin_scalar)));
            }
            None => region_remap.push(None),
        }
    }

    // Walk every subtable, project its regionIndexes through
    // region_remap, scale every delta by pin_scalar, and re-emit. We
    // emit each surviving subtable with a simple all-i16 or all-i32
    // delta encoding: pick the smallest that fits every value.
    let mut new_outer_for_old: Vec<Option<u16>> = Vec::with_capacity(subtable_count);
    // Pre-encoded subtable bodies (everything past the subtable's own
    // header bytes are written below; we serialize them in order so
    // offsets land deterministically).
    let mut new_subtables: Vec<Vec<u8>> = Vec::new();
    // Source bytes the subtable walk may still read. See the doc
    // comment for why overlapping subtables need a cap.
    let mut read_budget = ivs_bytes.len().saturating_mul(4).saturating_add(1 << 16);

    for sub_off in &subtable_offsets {
        let Some(sub_off) = *sub_off else {
            new_outer_for_old.push(None);
            continue;
        };
        // Subtable header: itemCount, wordDeltaCount, regionIndexCount,
        // then regionIndexCount x u16 indexes, then itemCount delta
        // rows.
        let item_count = usize::from(read::u16_at(ivs_bytes, sub_off, CTX)?);
        let wdc_raw = read::u16_at(ivs_bytes, sub_off + 2, CTX)?;
        let long_words = wdc_raw & 0x8000 != 0;
        let word_delta_count = (wdc_raw & 0x7FFF) as usize;
        let region_index_count = usize::from(read::u16_at(ivs_bytes, sub_off + 4, CTX)?);
        if word_delta_count > region_index_count {
            return Err(sigilbuzz::Error::Malformed {
                offset: sub_off + 2,
                context: "ItemVariationData has more word deltas than regions",
            }
            .into());
        }
        // The reads above put `sub_off + 6` inside the data.
        let ri_start = sub_off + 6;
        let region_index_bytes = read::array_at(ivs_bytes, ri_start, region_index_count, 2, CTX)?;
        read_budget = read_budget
            .checked_sub(6 + region_index_bytes.len())
            .ok_or(ALIASED)?;
        let region_indexes: Vec<u16> = region_index_bytes
            .chunks_exact(2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .collect();

        // Per-source-slot survival list: index into source slot,
        // produces (new_region_index, scalar).
        let mut surviving_slots: Vec<(usize, u16, f32)> = Vec::new();
        for (slot, &old_ri) in region_indexes.iter().enumerate() {
            if let Some(Some((new_ri, scalar))) = region_remap.get(old_ri as usize) {
                surviving_slots.push((slot, *new_ri, *scalar));
            }
        }

        // Subtable collapses entirely if either no items or no
        // surviving regions.
        if item_count == 0 || surviving_slots.is_empty() {
            new_outer_for_old.push(None);
            continue;
        }
        // Every kept column is written wide, so wordDeltaCount equals
        // the kept region count, and its top bit is the LONG_WORDS flag.
        let surviving_count = u16::try_from(surviving_slots.len())
            .ok()
            .filter(|&count| count <= 0x7FFF)
            .ok_or(sigilbuzz::Error::Malformed {
                offset: sub_off + 4,
                context: "ItemVariationData keeps more than 32,767 regions",
            })?;

        // Read every delta row's source slots. Each slot's source
        // encoding depends on (slot < word_delta_count, long_words).
        let (src_wide, src_narrow) = if long_words {
            (4usize, 2usize)
        } else {
            (2usize, 1usize)
        };
        let row_size =
            word_delta_count * src_wide + (region_index_count - word_delta_count) * src_narrow;
        let rows_start = ri_start + region_index_count * 2;
        let rows = read::array_at(ivs_bytes, rows_start, item_count, row_size, CTX)?;
        read_budget = read_budget.checked_sub(rows.len()).ok_or(ALIASED)?;

        // For each item, build its surviving row of i32 deltas
        // (post-pin-scalar). `row_size` is at least 1 here because a
        // surviving slot implies at least one region index.
        let mut item_rows: Vec<Vec<i32>> = Vec::with_capacity(item_count);
        for it in 0..item_count {
            let row = &rows[it * row_size..(it + 1) * row_size];
            // Walk source slots, decoding each.
            let mut src_deltas: Vec<i32> = Vec::with_capacity(region_index_count);
            let mut cursor = 0;
            for slot in 0..region_index_count {
                let is_wide = slot < word_delta_count;
                let value: i32 = match (is_wide, long_words) {
                    (true, true) => {
                        let v = i32::from_be_bytes([
                            row[cursor],
                            row[cursor + 1],
                            row[cursor + 2],
                            row[cursor + 3],
                        ]);
                        cursor += 4;
                        v
                    }
                    (true, false) | (false, true) => {
                        let v = i32::from(i16::from_be_bytes([row[cursor], row[cursor + 1]]));
                        cursor += 2;
                        v
                    }
                    (false, false) => {
                        #[allow(clippy::cast_possible_wrap)]
                        let v = row[cursor] as i8;
                        cursor += 1;
                        i32::from(v)
                    }
                };
                src_deltas.push(value);
            }
            // Apply scalar to each surviving slot, build the new row in
            // surviving-slot order.
            let new_row: Vec<i32> = surviving_slots
                .iter()
                .map(|&(slot, _new_ri, scalar)| {
                    let scaled = src_deltas.get(slot).copied().unwrap_or(0) as f32 * scalar;
                    scaled.round() as i32
                })
                .collect();
            item_rows.push(new_row);
        }

        // Decide encoding: pick all-i16 if every value fits, else
        // all-i32 (set LONG_WORDS bit, wordDeltaCount =
        // surviving_slot_count). Simple and conservative: the partial
        // output is not run through another IVS dedup pass.
        let all_fit_i16 = item_rows
            .iter()
            .flat_map(|r| r.iter())
            .all(|v| (i32::from(i16::MIN)..=i32::from(i16::MAX)).contains(v));

        // Emit the subtable body.
        let mut sub_bytes: Vec<u8> = Vec::new();
        sub_bytes.extend_from_slice(&(item_count as u16).to_be_bytes());
        let wdc_word: u16 = if all_fit_i16 {
            // wordDeltaCount = surviving_count (all wide as i16),
            // long_words bit clear.
            surviving_count
        } else {
            // long_words bit set, wordDeltaCount = surviving_count
            // (all wide as i32).
            surviving_count | 0x8000
        };
        sub_bytes.extend_from_slice(&wdc_word.to_be_bytes());
        sub_bytes.extend_from_slice(&surviving_count.to_be_bytes());
        for &(_slot, new_ri, _scalar) in &surviving_slots {
            sub_bytes.extend_from_slice(&new_ri.to_be_bytes());
        }
        for row in &item_rows {
            for &v in row {
                if all_fit_i16 {
                    let v16 = v as i16;
                    sub_bytes.extend_from_slice(&v16.to_be_bytes());
                } else {
                    sub_bytes.extend_from_slice(&v.to_be_bytes());
                }
            }
        }
        let new_outer = new_subtables.len() as u16;
        new_subtables.push(sub_bytes);
        new_outer_for_old.push(Some(new_outer));
    }

    // Note: when every subtable collapses we still emit a valid (but
    // empty) IVS. The caller decides whether to drop the host table
    // entirely, but the RegionRemap stays meaningful (every lookup
    // returns None). A zero-region zero-subtable IVS is a 16-byte
    // skeleton: 8-byte header + 4-byte region list + 0 subtable
    // offsets. Real consumers (HVAR / VVAR / MVAR / GDEF) read deltas
    // by (outer, inner) and resolve out-of-range to zero.

    // Emit the new IVS.
    let new_axis_count = pins.iter().filter(|p| matches!(p, AxisPin::Keep)).count() as u16;
    let new_subtable_count = new_subtables.len();
    let header_size = 8 + new_subtable_count * 4;
    const TOO_BIG: &str = "partial instancing: an ItemVariationStore exceeds 4 GiB";

    let mut out: Vec<u8> = Vec::with_capacity(header_size);
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&offset32(header_size, TOO_BIG)?.to_be_bytes());
    out.extend_from_slice(&(new_subtable_count as u16).to_be_bytes());
    // Subtable offsets (filled in below).
    let subtable_off_slot = out.len();
    for _ in 0..new_subtable_count {
        out.extend_from_slice(&0u32.to_be_bytes());
    }

    // Region list.
    out.extend_from_slice(&new_axis_count.to_be_bytes());
    out.extend_from_slice(&(new_regions.len() as u16).to_be_bytes());
    for region in &new_regions {
        // Each region must have exactly new_axis_count entries; the
        // projection guarantees this.
        for &(s, p, e) in region {
            write_f2dot14_bytes(&mut out, s);
            write_f2dot14_bytes(&mut out, p);
            write_f2dot14_bytes(&mut out, e);
        }
    }

    // Subtables.
    for (i, sub) in new_subtables.iter().enumerate() {
        let off_u32 = offset32(out.len(), TOO_BIG)?;
        let slot = subtable_off_slot + i * 4;
        out[slot..slot + 4].copy_from_slice(&off_u32.to_be_bytes());
        out.extend_from_slice(sub);
    }

    Ok((out, RegionRemap { new_outer_for_old }))
}
