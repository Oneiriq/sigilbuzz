//! Variation-delta lookup for `PaintVar*` fields and `VarColorStop`s.
//!
//! The flattening evaluator ([`crate::evaluate_with`]) resolves every
//! delta through one [`Deltas`] value, so units and index-map handling
//! live in one place.

use sigilbuzz::tables::colr::{Colr, VarIndexBase};
use sigilbuzz::tables::variation_store::ItemVariationStore;
use sigilbuzz::Face;

use crate::eval::{resolve_gdef_var_store, resolve_index_map, resolve_var_store, DeltaSetIndexMap};

/// Variation deltas for one evaluation: the font's item variation
/// store, its optional index map, and the normalized coordinates.
/// Shared by every paint-tree walk in this crate.
pub(crate) struct Deltas<'a, 'c> {
    var_store: Option<ItemVariationStore<'a>>,
    /// Optional DeltaSetIndexMap that redirects a paint's
    /// `var_index_base + field_index` through an indirection table
    /// before it hits the IVS. Spec-compliant variable color fonts
    /// use this to share IVS rows across many paint records.
    index_map: Option<DeltaSetIndexMap<'a>>,
    coords: &'c [f32],
}

impl<'a, 'c> Deltas<'a, 'c> {
    /// Finds the variation store (COLR's own, else GDEF's) and index
    /// map for `face`.
    pub(crate) fn new(face: &Face<'a>, colr: &Colr<'a>, coords: &'c [f32]) -> Self {
        Self {
            var_store: resolve_var_store(colr).or_else(|| resolve_gdef_var_store(face)),
            index_map: resolve_index_map(face),
            coords,
        }
    }

    /// Looks up the raw delta for the `field_index`-th variable field
    /// of a paint that records `var_index_base` as its anchor.
    ///
    /// The COLRv1 spec resolves variable fields through one of two
    /// paths:
    ///
    /// 1. **No indirection (default).** `var_index_base + field_index`
    ///    is the on-disk `(outer, inner)` pair; the IVS row at that
    ///    pair is the delta source.
    /// 2. **DeltaSetIndexMap indirection.** `var_index_base +
    ///    field_index` is a *flat index* into the map; the map yields
    ///    the actual `(outer, inner)` pair.
    ///
    /// The delta is in the field's raw units (design units, or F2DOT14
    /// / Fixed ticks). Returns `0.0` when the var store is absent,
    /// `var_index_base` is the no-deltas sentinel, or `coords` is empty.
    pub(crate) fn raw(&self, var_index_base: VarIndexBase, field_index: u16) -> f32 {
        if var_index_base == VarIndexBase::MAX || self.coords.is_empty() {
            return 0.0;
        }
        let Some(store) = self.var_store.as_ref() else {
            return 0.0;
        };
        let flat_index = var_index_base.wrapping_add(u32::from(field_index));
        let (outer, inner) = if let Some(map) = self.index_map.as_ref() {
            match map.lookup(flat_index) {
                Some(pair) => pair,
                None => return 0.0,
            }
        } else {
            ((flat_index >> 16) as u16, flat_index as u16)
        };
        store.delta(outer, inner, self.coords)
    }

    /// Delta for an F2DOT14 field, as a fraction: a raw delta of 8192
    /// ticks is 0.5.
    pub(crate) fn f2dot14(&self, var_index_base: VarIndexBase, field_index: u16) -> f32 {
        self.raw(var_index_base, field_index) / 16384.0
    }

    /// Delta for a 16.16 Fixed field, as a fraction.
    pub(crate) fn fixed(&self, var_index_base: VarIndexBase, field_index: u16) -> f32 {
        self.raw(var_index_base, field_index) / 65536.0
    }

    /// Offset and alpha deltas (both F2DOT14 fractions) for a
    /// `VarColorStop` whose `varIndexBase` is `stop_var`. Stops route
    /// through the same index map as paint fields. Returns `None` when
    /// the map cannot resolve the offset entry.
    pub(crate) fn stop(&self, stop_var: u32) -> Option<(f32, f32)> {
        let Some(store) = self.var_store.as_ref() else {
            return Some((0.0, 0.0));
        };
        if stop_var == u32::MAX || self.coords.is_empty() {
            return Some((0.0, 0.0));
        }
        let (off_outer, off_inner) = match self.index_map.as_ref() {
            Some(map) => map.lookup(stop_var)?,
            None => ((stop_var >> 16) as u16, stop_var as u16),
        };
        let (a_outer, a_inner) = self
            .index_map
            .as_ref()
            .and_then(|map| map.lookup(stop_var.wrapping_add(1)))
            .unwrap_or((off_outer, off_inner.wrapping_add(1)));
        Some((
            store.delta(off_outer, off_inner, self.coords) / 16384.0,
            store.delta(a_outer, a_inner, self.coords) / 16384.0,
        ))
    }
}
