//! The ItemVariationStore projection of a partial instance: regions
//! trimmed to the kept axes, deltas scaled by the pinned axes, and the
//! row remap every table that names the store follows.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::region::project_region_onto_kept_axes;
use super::AxisPin;
use crate::read;
use crate::util::round_half_up;
use crate::SubsetError;

// ---------------------------------------------------------------------------
// Partial-instancing ItemVariationStore rewrite
// ---------------------------------------------------------------------------

/// Maps `(old_outer, old_inner)` source IVS rows to their new
/// `(new_outer, new_inner)` indexes after a partial-instance rewrite,
/// and reports what each row loses with the regions on the pinned axes
/// only.
///
/// A region the pinned axes leave with no peak on any kept axis applies
/// alike at every kept coordinate, the new default included, where
/// renderers apply no variations. The projection drops it, and the
/// caller adds its scaled deltas ([`RegionRemap::folded`]) to the
/// default values the store varies, as HarfBuzz's and fontTools'
/// instancers do.
#[derive(Debug, Clone, Default)]
pub(crate) struct RegionRemap {
    /// Per-source-outer entries. Each entry is either:
    /// - `Some(new_outer)`: the subtable survives at this index, with
    ///   the same item rows in the same order. The new subtable's
    ///   `region_indexes.len()` may differ (regions are dropped /
    ///   trimmed), but `inner` indices are preserved verbatim because
    ///   the partial-instance pass never reorders or drops rows.
    /// - `None`: no region the subtable referenced has a peak left on a
    ///   kept axis (or it has no rows), so the subtable was elided.
    ///   Consumers reading via `(outer, inner)` resolve to a delta of
    ///   zero, after adding the row's folded delta to their default.
    new_outer_for_old: Vec<Option<u16>>,
    /// Per-source-outer layout; `None` for a null subtable offset.
    layouts: Vec<Option<SubtableLayout>>,
}

/// What became of one source subtable's slots.
#[derive(Debug, Clone, Default)]
pub(crate) struct SubtableLayout {
    /// Each kept column's source slots with their pin scalars, in column
    /// order. Without merging, one slot per column.
    pub(crate) columns: Vec<Vec<(u16, f32)>>,
    /// The source slots whose regions lie on the pinned axes only, with
    /// their pin scalars: their scaled deltas fold into the defaults.
    pub(crate) folded: Vec<(u16, f32)>,
    /// Each row's folded delta: the sum of its folded slots' deltas,
    /// scaled. Empty when no slot folds.
    rows: Vec<f32>,
}

impl RegionRemap {
    /// Returns the new (outer, inner) for an old row, or `None` when
    /// the surrounding subtable collapsed.
    pub(crate) fn lookup(&self, old_outer: u16, old_inner: u16) -> Option<(u16, u16)> {
        let new_outer = (*self.new_outer_for_old.get(old_outer as usize)?)?;
        Some((new_outer, old_inner))
    }

    /// The number of source subtables.
    pub(crate) fn subtable_count(&self) -> u16 {
        self.layouts.len() as u16
    }

    /// The new index of source subtable `old_outer`, or `None` when it
    /// collapsed.
    pub(crate) fn new_outer(&self, old_outer: u16) -> Option<u16> {
        self.new_outer_for_old
            .get(usize::from(old_outer))
            .copied()
            .flatten()
    }

    /// What became of source subtable `old_outer`'s slots; `None` for
    /// a null subtable offset or an index past the store.
    pub(crate) fn layout(&self, old_outer: u16) -> Option<&SubtableLayout> {
        self.layouts.get(usize::from(old_outer))?.as_ref()
    }

    /// The delta row `(old_outer, old_inner)` adds at the new default
    /// through its regions on the pinned axes only, which the projected
    /// store no longer holds: the caller adds it to the default value.
    /// Zero for a row the store does not hold.
    pub(crate) fn folded(&self, old_outer: u16, old_inner: u16) -> f32 {
        self.layout(old_outer)
            .and_then(|l| l.rows.get(usize::from(old_inner)))
            .copied()
            .unwrap_or(0.0)
    }
}

/// Converts a raw F2DOT14 to its value.
fn f2dot14(raw: [u8; 2]) -> f32 {
    f32::from(i16::from_be_bytes(raw)) / 16384.0
}

/// Writes an F2DOT14 to a byte vector.
fn write_f2dot14_bytes(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&f2dot14_bits(v).to_be_bytes());
}

/// The F2DOT14 bits [`write_f2dot14_bytes`] writes for `v`.
fn f2dot14_bits(v: f32) -> i16 {
    (v * 16384.0)
        .round()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

/// A projected region as written: the F2DOT14 bits of each axis's
/// start, peak and end.
fn region_bits(region: &[(f32, f32, f32)]) -> Vec<[i16; 3]> {
    region
        .iter()
        .map(|&(s, p, e)| [f2dot14_bits(s), f2dot14_bits(p), f2dot14_bits(e)])
        .collect()
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

/// How [`project_ivs_with`] projects a store. Regions on the pinned axes
/// only are always dropped and their deltas folded (see
/// [`RegionRemap::folded`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Projection {
    /// Merge regions that project to the same kept-axis region, as
    /// HarfBuzz's instancer does: one region in the new list, and in
    /// each subtable one column whose deltas are the sum of the merged
    /// columns' scaled deltas, rounded once (halves up). Without it
    /// every surviving column stays, rounded on its own (halves away
    /// from zero); the CFF2 bake relies on that layout.
    pub(crate) merge: bool,
    /// Keep the first subtable at outer index 0 even when it collapses,
    /// as an empty subtable, so a lookup that names outer 0 without a
    /// map (an HVAR or VVAR advance read by glyph id) still reads the
    /// first subtable's rows, not the next surviving subtable's.
    pub(crate) keep_outer_zero: bool,
    /// Keep a subtable with no rows when some of its regions survive. A
    /// CFF2 store's subtables hold no rows (the charstrings carry the
    /// deltas), yet `vsindex` and `blend` count on them and their region
    /// lists.
    pub(crate) keep_itemless: bool,
}

impl Projection {
    /// Every region that survives keeps its own column.
    pub(crate) const SEPARATE: Self = Self {
        merge: false,
        keep_outer_zero: false,
        keep_itemless: false,
    };

    /// The projection of a CFF2 store: every surviving region keeps its
    /// own column, in order, and subtables stay without rows.
    pub(crate) const CFF2: Self = Self {
        keep_itemless: true,
        ..Self::SEPARATE
    };

    /// Regions that project alike merge, as in HarfBuzz's instancer
    /// (MVAR's fields, GDEF's carets and GPOS's values).
    pub(crate) const MERGED: Self = Self {
        merge: true,
        keep_outer_zero: false,
        keep_itemless: false,
    };
}

/// [`project_ivs`] as an `Option`, for callers that treat any failure
/// alike (the CFF2 VarStore bake reports its own error).
pub(crate) fn bake_ivs_partial(
    ivs_bytes: &[u8],
    coords: &[f32],
    pins: &[AxisPin],
) -> Option<(Vec<u8>, RegionRemap)> {
    project_ivs_with(ivs_bytes, coords, pins, Projection::CFF2).ok()
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
#[cfg(test)]
pub(crate) fn project_ivs(
    ivs_bytes: &[u8],
    coords: &[f32],
    pins: &[AxisPin],
) -> Result<(Vec<u8>, RegionRemap), SubsetError> {
    project_ivs_with(ivs_bytes, coords, pins, Projection::SEPARATE)
}

/// [`project_ivs`], projecting as `how` says.
pub(crate) fn project_ivs_with(
    ivs_bytes: &[u8],
    coords: &[f32],
    pins: &[AxisPin],
    how: Projection,
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

    // Project each region. A region keeps a peak on some kept axis
    // (written, possibly merged), lies on the pinned axes only (dropped,
    // its scaled deltas folded into the defaults), or is zero at the pin
    // coordinates (dropped).
    #[derive(Clone, Copy)]
    enum Fate {
        Dropped,
        Folded(f32),
        Kept(u16, f32),
    }
    let mut region_fate: Vec<Fate> = Vec::with_capacity(region_count);
    let mut new_regions: Vec<Vec<(f32, f32, f32)>> = Vec::new();
    // The new index of each region written, by its bits, for merging.
    let mut region_of: BTreeMap<Vec<[i16; 3]>, u16> = BTreeMap::new();
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
        let fate = match project_region_onto_kept_axes(&region, pins, coords) {
            None => Fate::Dropped,
            Some(p) if !p.kept_axes.iter().any(|&(_, peak, _)| peak != 0.0) => {
                Fate::Folded(p.pin_scalar)
            }
            Some(p) => {
                // A merging projection reuses a region it already wrote
                // when the two read the same once written.
                let existing = if how.merge {
                    let next = new_regions.len() as u16;
                    let idx = *region_of.entry(region_bits(&p.kept_axes)).or_insert(next);
                    (idx != next).then_some(idx)
                } else {
                    None
                };
                let new_idx = match existing {
                    Some(i) => i,
                    None => {
                        new_regions.push(p.kept_axes);
                        (new_regions.len() - 1) as u16
                    }
                };
                Fate::Kept(new_idx, p.pin_scalar)
            }
        };
        region_fate.push(fate);
    }

    // Walk every subtable, project its regionIndexes through
    // region_fate, scale every delta by pin_scalar, and re-emit. We
    // emit each surviving subtable with a simple all-i16 or all-i32
    // delta encoding: pick the smallest that fits every value.
    let mut new_outer_for_old: Vec<Option<u16>> = Vec::with_capacity(subtable_count);
    let mut layouts: Vec<Option<SubtableLayout>> = Vec::with_capacity(subtable_count);
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
            layouts.push(None);
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

        // The new columns: each a new region index and the source slots
        // (with their scalars) that sum into it. Without merging, one
        // column per surviving slot. The slots on the pinned axes only
        // fold.
        let mut columns: Vec<(u16, Vec<(usize, f32)>)> = Vec::new();
        let mut folded: Vec<(usize, f32)> = Vec::new();
        // The column of each new region index, for merging.
        let mut column_of: BTreeMap<u16, usize> = BTreeMap::new();
        for (slot, &old_ri) in region_indexes.iter().enumerate() {
            match region_fate.get(old_ri as usize) {
                Some(&Fate::Kept(new_ri, scalar)) => {
                    let merged = if how.merge {
                        column_of.get(&new_ri).and_then(|&c| columns.get_mut(c))
                    } else {
                        None
                    };
                    match merged {
                        Some((_, slots)) => slots.push((slot, scalar)),
                        None => {
                            column_of.insert(new_ri, columns.len());
                            columns.push((new_ri, alloc::vec![(slot, scalar)]));
                        }
                    }
                }
                Some(&Fate::Folded(scalar)) => folded.push((slot, scalar)),
                Some(Fate::Dropped) | None => {}
            }
        }

        // Subtable collapses entirely if either no items or no
        // surviving regions; a CFF2 store's subtables have no items.
        let keep = !((item_count == 0 && !how.keep_itemless) || columns.is_empty());
        let mut layout = SubtableLayout {
            columns: columns
                .iter()
                .map(|(_, slots)| slots.iter().map(|&(s, k)| (s as u16, k)).collect())
                .collect(),
            folded: folded.iter().map(|&(s, k)| (s as u16, k)).collect(),
            rows: Vec::new(),
        };
        if !keep && (folded.is_empty() || item_count == 0) {
            new_outer_for_old.push(None);
            layouts.push(Some(layout));
            continue;
        }
        // Every kept column is written wide, so wordDeltaCount equals
        // the kept region count, and its top bit is the LONG_WORDS flag.
        let surviving_count = u16::try_from(columns.len())
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
        // (post-pin-scalar) and its folded delta. `row_size` is at
        // least 1 here because a surviving or folded slot implies at
        // least one region index.
        let mut item_rows: Vec<Vec<i32>> = Vec::with_capacity(if keep { item_count } else { 0 });
        if !folded.is_empty() {
            layout.rows.reserve(item_count);
        }
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
            if !folded.is_empty() {
                let sum: f32 = folded
                    .iter()
                    .map(|&(slot, scalar)| {
                        src_deltas.get(slot).copied().unwrap_or(0) as f32 * scalar
                    })
                    .sum();
                layout.rows.push(sum);
            }
            if !keep {
                continue;
            }
            // Apply scalar to each surviving slot and sum each column,
            // building the new row in column order.
            let new_row: Vec<i32> = columns
                .iter()
                .map(|(_, slots)| {
                    let scaled: f32 = slots
                        .iter()
                        .map(|&(slot, scalar)| {
                            src_deltas.get(slot).copied().unwrap_or(0) as f32 * scalar
                        })
                        .sum();
                    if how.merge {
                        round_half_up(scaled)
                    } else {
                        scaled.round() as i32
                    }
                })
                .collect();
            item_rows.push(new_row);
        }
        layouts.push(Some(layout));
        if !keep {
            new_outer_for_old.push(None);
            continue;
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
        for (new_ri, _) in &columns {
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

    // A collapsed first subtable whose rows a lookup reaches without a
    // map stays at outer 0, empty, so every row there reads as zero
    // instead of as the next surviving subtable's.
    if how.keep_outer_zero && new_outer_for_old.first() == Some(&None) {
        new_subtables.insert(0, alloc::vec![0; 6]);
        for outer in new_outer_for_old.iter_mut().flatten() {
            *outer += 1;
        }
        new_outer_for_old[0] = Some(0);
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

    Ok((
        out,
        RegionRemap {
            new_outer_for_old,
            layouts,
        },
    ))
}
