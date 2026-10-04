//! The GDEF ItemVariationStore of an instanced font.
//!
//! Full instancing drops the store once everything that named it has
//! been folded: GPOS ValueRecords and anchors by [`crate::gpos_var`],
//! ligature carets by [`crate::gdef::fold_caret_variations`].
//!
//! The GDEF is then rebuilt through the subsetter's GDEF writer (see
//! [`crate::gdef`]) under an identity glyph map rather than cut short
//! at the store. The spec puts no order on the tables inside GDEF: any
//! subtable, or a table nested in one (a Coverage, a mark glyph set, a
//! caret's Device table), may sit after the store, and truncating the
//! table there would cut it off. The writer lays out every surviving
//! table afresh, so nothing depends on where the store sat.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use crate::layout::GidMap;
use crate::util::StoreDeltas;
use crate::warnings::Warnings;
use crate::SubsetError;

/// What becomes of the source GDEF in an instanced font.
pub(super) enum GdefBake {
    /// No store to take out: the source table rides through verbatim.
    Unchanged,
    /// The table rebuilt without its store.
    Rebuilt(Vec<u8>),
    /// The store was all the table held; the table goes.
    Dropped,
}

/// True when `gdef` is version 1.3 or later with a non-null
/// `itemVarStoreOffset`.
pub(super) fn has_store(gdef: &[u8]) -> bool {
    let minor = gdef
        .get(2..4)
        .map_or(0, |b| u16::from_be_bytes([b[0], b[1]]));
    minor >= 3 && gdef.get(14..18).is_some_and(|off| off != [0; 4])
}

/// The bytes of the store of `gdef`, from its first byte to the end of
/// the table, where the core GDEF parser reads it; `None` without one.
pub(super) fn store_bytes(gdef: &[u8]) -> Option<&[u8]> {
    if !has_store(gdef) {
        return None;
    }
    let off = gdef.get(14..).and_then(<[u8]>::first_chunk::<4>)?;
    gdef.get(u32::from_be_bytes(*off) as usize..)
}

/// The deltas of the face's GDEF store at `coords`, reporting a spent
/// budget against `table`; `None` when the face has no store.
pub(super) fn gdef_deltas<'s, 'a>(
    face: &Face<'a>,
    coords: &'s [f32],
    warnings: &'s Warnings,
    table: [u8; 4],
) -> Result<Option<StoreDeltas<'s, 'a>>, SubsetError> {
    // The core parse reports a malformed GDEF, and says whether it has
    // a store at all.
    let gdef = face.gdef()?;
    if gdef
        .as_ref()
        .and_then(|g| g.item_variation_store())
        .is_none()
    {
        return Ok(None);
    }
    Ok(face
        .table_bytes(tag::GDEF)
        .ok()
        .and_then(store_bytes)
        .and_then(|s| StoreDeltas::new(s, coords))
        .map(|d| d.reporting(warnings, table)))
}

/// Every glyph of the face under its own id.
pub(super) fn identity_map(face: &Face<'_>) -> Result<GidMap, SubsetError> {
    let num_glyphs = face.maxp()?.num_glyphs;
    Ok(GidMap::from_kept(&(0..num_glyphs).collect::<Vec<u16>>()))
}

/// Folds the ligature caret variations at `coords` into the carets,
/// then rebuilds the face's GDEF without its ItemVariationStore.
/// Malformed pieces the rebuild leaves out are reported to `warnings`.
pub(super) fn prune_gdef_store(
    face: &Face<'_>,
    coords: &[f32],
    warnings: &Warnings,
) -> Result<GdefBake, SubsetError> {
    let Ok(bytes) = face.table_bytes(tag::GDEF) else {
        return Ok(GdefBake::Unchanged);
    };
    if !has_store(bytes) {
        return Ok(GdefBake::Unchanged);
    }
    let mut folded = bytes.to_vec();
    let deltas = gdef_deltas(face, coords, warnings, tag::GDEF)?;
    crate::gdef::fold_caret_variations(&mut folded, deltas.as_ref());
    let rebuilt = crate::gdef::rewrite_gdef_bytes(&folded, &identity_map(face)?, false, warnings)?;
    Ok(match rebuilt {
        Some(bytes) => GdefBake::Rebuilt(bytes),
        None => GdefBake::Dropped,
    })
}

#[cfg(test)]
mod tests;
