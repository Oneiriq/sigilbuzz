//! Keeping VariationIndex tables in step with a partially instanced
//! GDEF ItemVariationStore.
//!
//! [`super::ivs::project_ivs`] projects the store onto the kept axes and
//! elides every ItemVariationData subtable left with no region (all of
//! its regions sat outside the pinned coordinates) or no rows. The
//! subtables after an elided one move down, so an `(outer, inner)` row
//! reference changes its `outer`, and a reference into an elided
//! subtable has nothing left to name: its delta is zero at every kept
//! coordinate. [`super::ivs::RegionRemap`] records both.
//!
//! GPOS ValueRecords and anchors and GDEF ligature carets reach the
//! store through VariationIndex tables, so each of them goes through
//! the remap: a moved row is renumbered in place, and a row that is
//! gone has its Device offset cleared, which reads as no variation.
//! First, each value takes its row's deltas from the regions on the
//! pinned axes only, which the projection drops
//! ([`super::ivs::RegionRemap::folded`]): they apply at the new default
//! too, where a shaper applies no variations, so HarfBuzz's instancer
//! moves them into the static values the same way.
//! GPOS is patched in place (every structure stays where it was); GDEF
//! is rebuilt around the new store by the GDEF writer, the same way
//! full instancing rebuilds it (see [`super::gdef_store`]).

use alloc::collections::{BTreeMap, BTreeSet};

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use super::gdef_store::{has_store, identity_map, GdefBake};
use super::ivs::{project_ivs_with, shifted, Projection, RegionRemap};
use super::AxisPin;
use crate::gdef::StorePlan;
use crate::gpos_var::{add_to_field, walk_gpos_device_slots, VARIATION_INDEX_DELTA_FORMAT};
use crate::layout::GidMap;
use crate::read;
use crate::util::round_half_up;
use crate::warnings::Warnings;
use crate::SubsetError;

/// How the source store's rows map into the partially instanced one.
pub(super) enum StoreRemap {
    /// Rows follow the projected store's numbering.
    Rows(RegionRemap),
    /// The store could not be projected (an unknown format, or data
    /// the projection rejects) and was dropped: no row survives.
    Cleared,
}

impl StoreRemap {
    /// The new `(outer, inner)` of a source row, or `None` when the row
    /// has no variation left.
    pub(super) fn lookup(&self, outer: u16, inner: u16) -> Option<(u16, u16)> {
        match self {
            Self::Rows(remap) => remap.lookup(outer, inner),
            Self::Cleared => None,
        }
    }

    /// What a value varied by source row `(outer, inner)` takes into its
    /// default, rounded halves up as HarfBuzz's instancer rounds it: the
    /// row's deltas from regions on the pinned axes only.
    pub(super) fn folded(&self, outer: u16, inner: u16) -> i32 {
        match self {
            Self::Rows(remap) => round_half_up(remap.folded(outer, inner)),
            Self::Cleared => 0,
        }
    }
}

/// Partially instances the face's GDEF ItemVariationStore at `coords`
/// over the `Pin` axes of `pins` and rebuilds the GDEF around it, with
/// its caret VariationIndex rows renumbered. Also returns the remap
/// the GPOS VariationIndex tables need; `None` when there is no store.
/// Malformed pieces left out are reported to `warnings`.
pub(super) fn bake_gdef_store_partial(
    face: &Face<'_>,
    coords: &[f32],
    pins: &[AxisPin],
    warnings: &Warnings,
) -> Result<(GdefBake, Option<StoreRemap>), SubsetError> {
    let Ok(bytes) = face.table_bytes(tag::GDEF) else {
        return Ok((GdefBake::Unchanged, None));
    };
    bake_gdef_bytes_partial(bytes, &identity_map(face)?, coords, pins, warnings)
}

/// [`bake_gdef_store_partial`] on raw GDEF bytes, glyphs kept per `map`.
pub(super) fn bake_gdef_bytes_partial(
    bytes: &[u8],
    map: &GidMap,
    coords: &[f32],
    pins: &[AxisPin],
    warnings: &Warnings,
) -> Result<(GdefBake, Option<StoreRemap>), SubsetError> {
    if !has_store(bytes) {
        return Ok((GdefBake::Unchanged, None));
    }
    // A store that cannot be projected is dropped and reported; its
    // rows then read as no variation. Running out of 32-bit offsets
    // is still an error.
    let projected =
        match read::offset32_at(bytes, 14, 0, "GDEF ItemVariationStore offset past the end")
            .map_err(SubsetError::from)
            .and_then(|at| {
                project_ivs_with(&bytes[at..], coords, pins, Projection::MERGED)
                    .map_err(|e| shifted(e, at))
            }) {
            Ok(projected) => Some(projected),
            Err(SubsetError::Parse(e)) => {
                warnings.parse_error(tag::GDEF, 0, &e, "the ItemVariationStore");
                None
            }
            Err(other) => return Err(other),
        };
    let (rebuilt, remap) = match projected {
        Some((store, rows)) => {
            let remap = StoreRemap::Rows(rows);
            // The carets take their pinned-only deltas first.
            let mut folded = bytes.to_vec();
            crate::gdef::fold_caret_defaults(&mut folded, &|outer, inner| {
                remap.folded(outer, inner)
            });
            let rows = |outer, inner| remap.lookup(outer, inner);
            let plan = StorePlan::Replace {
                store: &store,
                remap: &rows,
            };
            (
                crate::gdef::rebuild_gdef(&folded, map, plan, warnings)?,
                remap,
            )
        }
        None => (
            crate::gdef::rebuild_gdef(bytes, map, StorePlan::Drop, warnings)?,
            StoreRemap::Cleared,
        ),
    };
    let bake = match rebuilt {
        Some(bytes) => GdefBake::Rebuilt(bytes),
        None => GdefBake::Dropped,
    };
    Ok((bake, Some(remap)))
}

/// Sends every VariationIndex table the GPOS device slots reach through
/// `remap`: each slot's value first takes its row's folded delta (once,
/// however often the walk reaches it), then a moved row is renumbered
/// in the table itself and a row that is gone has its slot cleared. A
/// table several slots share is renumbered once, and every slot naming
/// a gone row is cleared.
///
/// Compilers share identical VariationIndex tables, even between
/// subtables of a lookup. The walk hands each subtable its own slice of
/// the GPOS, so a table is identified by its position in the whole
/// table, not in the slice, or it could be renumbered twice.
pub(super) fn remap_gpos_variation_indices(gpos: &mut [u8], remap: &StoreRemap) {
    // Absolute table position -> the row it now names and the delta its
    // values take, decided on first visit.
    type Decided = (Option<(u16, u16)>, i32);
    let mut decided: BTreeMap<usize, Decided> = BTreeMap::new();
    // Absolute positions of the fields that took their delta.
    let mut moved: BTreeSet<usize> = BTreeSet::new();
    let origin = gpos.as_ptr() as usize;
    walk_gpos_device_slots(gpos, &mut |buf, slot| {
        if slot.delta_format(buf) != Some(VARIATION_INDEX_DELTA_FORMAT) {
            return;
        }
        let Some(target) = slot.target(buf) else {
            return;
        };
        // `buf` is a subslice of `gpos`: its address minus the start
        // of `gpos` is where it begins in the table.
        let base = buf.as_ptr() as usize - origin;
        let absolute = base + target;
        let (row, fold) = *decided.entry(absolute).or_insert_with(|| {
            let read = |pos: usize| u16::from_be_bytes([buf[pos], buf[pos + 1]]);
            let (outer, inner) = (read(target), read(target + 2));
            let row = remap.lookup(outer, inner);
            if let Some((outer, inner)) = row {
                buf[target..target + 2].copy_from_slice(&outer.to_be_bytes());
                buf[target + 2..target + 4].copy_from_slice(&inner.to_be_bytes());
            }
            (row, remap.folded(outer, inner))
        });
        if let Some(field) = slot.field.filter(|&f| fold != 0 && moved.insert(base + f)) {
            add_to_field(buf, field, fold);
        }
        if row.is_none() {
            slot.clear(buf);
        }
    });
}

#[cfg(test)]
mod tests;
