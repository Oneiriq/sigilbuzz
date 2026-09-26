//! Partial instancing: pins some axes, keeps the rest, and rebuilds
//! every variation table for the kept axes; plus the axis and
//! FeatureVariations helpers the full instancer shares.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use super::axes::{bake_avar_partial, bake_fvar_partial};
use super::gdef_store::GdefBake;
use super::metrics_var::{bake_hvar_partial, bake_mvar_partial, bake_vvar_partial};
use super::store_remap::{bake_gdef_store_partial, remap_gpos_variation_indices};
use super::{AxisPin, InstanceInput, InstancedOutput};
use crate::sfnt;
use crate::warnings::Warnings;
use crate::SubsetError;

/// Partial-instance bake: produces a reduced-axis variable font.
///
/// This path runs when `input.axis_pins` carries at least one
/// `AxisPin::Keep` and the source has neither `gvar` nor `CFF2` (those
/// tuple-projection paths are tracked as a follow-up to PR #183: the
/// public surface there errors with `Unsupported` for now).
///
/// The bake:
/// - re-emits `fvar` with only the surviving axes (and instances whose
///   surviving coord vector doesn't collapse to default),
/// - re-emits `avar` with only the surviving segment maps,
/// - re-emits `HVAR` / `VVAR` / `MVAR` / `GDEF.IVS` with their embedded
///   `ItemVariationStore` partial-projected through `pins` / `coords`,
///   each DeltaSetIndexMap rewritten to point at the new subtable
///   indexes,
/// - rebuilds `GDEF` around its projected store and renumbers every
///   VariationIndex in it and in `GPOS` to match (see
///   [`store_remap`](super::store_remap)),
/// - rides `glyf` / `hmtx` / `vmtx` / `head` / `hhea` / `maxp` /
///   the rest of the layout and other tables through verbatim. The Keep-axis
///   variations stay live; the Pin-axis dimensions fold into the
///   trimmed deltas so a shaper at `(Keep coords)` produces exactly
///   what the source produced at `(Keep coords, Pin coords)`.
///
/// An `avar`, `HVAR`, `VVAR` or `MVAR` that cannot be rebuilt for the
/// kept axes (malformed, or an `avar` other than version 1) is left
/// out, never carried through with regions that still count the pinned
/// axes, and reported in [`InstancedOutput::warnings`]. The bakes check
/// every Offset32 and every count-times-size product, so a crafted
/// table cannot wrap a 32-bit `usize`; a rebuilt table that outgrows
/// its own offsets is an error.
///
/// `drop_var_tables = false` is honored. The trimmed variation
/// tables ride out either way; the field controls whether tables like
/// `MVAR` get folded down into static metric fields. For the partial
/// path we always keep the (trimmed) variation tables: they still
/// drive the live axes.
pub(super) fn partial_instance(
    face: &Face<'_>,
    input: &InstanceInput,
) -> Result<InstancedOutput, SubsetError> {
    let pins = &input.axis_pins;
    let coords = &input.coords;

    // Apply avar's piecewise-linear remap if the source ships one. The
    // Pin-axis support-scalar evaluation must use post-avar coords
    // (the IVS regions are defined in the post-avar space).
    let post_avar_coords: Vec<f32> = match face.avar().map_err(SubsetError::from)? {
        Some(av) => av.remap_all(coords),
        None => coords.clone(),
    };

    let warnings = Warnings::default();
    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();

    // fvar trim.
    let fvar_bytes = face.table_bytes(tag::FVAR).map_err(SubsetError::from)?;
    let new_fvar = bake_fvar_partial(fvar_bytes, pins).ok_or(SubsetError::Unsupported(
        "instance: fvar partial trim failed",
    ))?;
    tables.push((tag::FVAR, new_fvar));

    // Variation tables that could not be rebuilt for the kept axes.
    // They are left out rather than carried through: their regions
    // still count the pinned axes.
    let mut dropped: Vec<[u8; 4]> = Vec::new();

    // avar trim (optional).
    if let Ok(avar_bytes) = face.table_bytes(tag::AVAR) {
        match bake_avar_partial(avar_bytes, pins) {
            Some(new_avar) => tables.push((tag::AVAR, new_avar)),
            None => {
                warnings.push(
                    tag::AVAR,
                    0,
                    "avar is not version 1, or its segment maps are truncated",
                    "the whole table",
                );
                dropped.push(tag::AVAR);
            }
        }
    }

    // HVAR / VVAR / MVAR rewrites (optional). A malformed table is
    // dropped (its metrics stop varying) and reported.
    type MetricsBake = fn(&[u8], &[f32], &[AxisPin]) -> Result<Vec<u8>, SubsetError>;
    let metrics_bakes: [([u8; 4], MetricsBake); 3] = [
        (tag::HVAR, bake_hvar_partial),
        (tag::VVAR, bake_vvar_partial),
        (tag::MVAR, bake_mvar_partial),
    ];
    for (table, bake) in metrics_bakes {
        let Ok(bytes) = face.table_bytes(table) else {
            continue;
        };
        match bake(bytes, &post_avar_coords, pins) {
            Ok(new) => tables.push((table, new)),
            Err(SubsetError::Parse(e)) => {
                warnings.parse_error(table, 0, &e, "the whole table");
                dropped.push(table);
            }
            Err(other) => return Err(other),
        }
    }
    // GDEF.IVS rewrite (optional). The projection can renumber the
    // store's rows, so the GDEF carets follow the new numbering and so
    // do the GPOS VariationIndex tables below.
    let (gdef_bake, store_remap) =
        bake_gdef_store_partial(face, &post_avar_coords, pins, &warnings)?;
    if let GdefBake::Rebuilt(b) = &gdef_bake {
        tables.push((tag::GDEF, b.clone()));
    }
    let remapped_gpos = match (&store_remap, face.table_bytes(tag::GPOS)) {
        (Some(remap), Ok(gpos_bytes)) => {
            let mut gpos = gpos_bytes.to_vec();
            remap_gpos_variation_indices(&mut gpos, remap);
            Some(gpos)
        }
        _ => None,
    };

    // FeatureVariations: conditions on the pinned axes are settled and
    // the kept axes renumbered.
    let pinned = pinned_axes(&post_avar_coords, pins);
    let new_axis = kept_axis_indices(pins);
    if let Some(b) = layout_variations(face, tag::GSUB, None, &pinned, &new_axis, &warnings)? {
        tables.push((tag::GSUB, b));
    }
    if let Some(b) = layout_variations(
        face,
        tag::GPOS,
        remapped_gpos,
        &pinned,
        &new_axis,
        &warnings,
    )? {
        tables.push((tag::GPOS, b));
    }

    // CFF2 VarStore + blend-operator rewrite (optional). VarStore
    // region trim via `bake_ivs_partial`; charstrings re-emit blend
    // ops with the surviving regions and pre-scaled deltas.
    if let Ok(cff2_bytes) = face.table_bytes(tag::CFF2) {
        let new_cff2 = crate::cff2::bake_cff2_partial(cff2_bytes, &post_avar_coords, pins)?;
        tables.push((tag::CFF2, new_cff2));
    }

    // gvar tuple-projection rewrite (optional). Pin-axis support
    // scalars fold into per-point deltas; Pin-axis dimensions drop
    // from every tuple region; tuples whose Pin-axis support is zero
    // disappear. Output gvar's axisCount = Keep-axis count.
    if let Ok(gvar_bytes) = face.table_bytes(tag::GVAR) {
        let new_axis_count = pins.iter().filter(|p| matches!(p, AxisPin::Keep)).count() as u16;
        let new_gvar = crate::gvar_partial::bake_gvar_partial(
            gvar_bytes,
            &post_avar_coords,
            pins,
            new_axis_count,
        )?;
        tables.push((tag::GVAR, new_gvar));
    }

    // Carry every other table through verbatim.
    for rec in face.records() {
        if tables.iter().any(|(t, _)| *t == rec.tag) {
            continue;
        }
        // Variable-font tables we handled above are excluded; the
        // gvar / CFF2 paths run when their host tables are present. A
        // GDEF that held only its store is gone, and so is a variation
        // table that could not be rebuilt.
        if rec.tag == tag::GDEF && matches!(gdef_bake, GdefBake::Dropped) {
            continue;
        }
        if dropped.contains(&rec.tag) {
            continue;
        }
        let bytes = face.table_bytes(rec.tag).map_err(SubsetError::from)?;
        tables.push((rec.tag, bytes.to_vec()));
    }

    let bytes = sfnt::build(face.sfnt_version(), &tables);
    Ok(InstancedOutput {
        bytes,
        warnings: warnings.into_sorted(),
    })
}

/// The F2DOT14 coordinate of every pinned axis (`None` for a `Keep`
/// axis), from post-avar `coords`. An empty `pins` pins every axis.
/// FeatureVariations conditions compare these against their F2DOT14
/// ranges, as a shaper does with its normalized coordinates.
pub(super) fn pinned_axes(coords: &[f32], pins: &[AxisPin]) -> Vec<Option<i16>> {
    coords
        .iter()
        .enumerate()
        .map(|(i, &c)| match pins.get(i) {
            Some(AxisPin::Keep) => None,
            Some(AxisPin::Pin) | None => {
                #[allow(clippy::cast_possible_truncation)]
                let raw = (c * 16384.0).round().clamp(-16384.0, 16384.0) as i16;
                Some(raw)
            }
        })
        .collect()
}

/// The index each `Keep` axis takes in the instance's `fvar` (the kept
/// axes in source order), `None` for a pinned one.
fn kept_axis_indices(pins: &[AxisPin]) -> Vec<Option<u16>> {
    let mut next = 0u16;
    pins.iter()
        .map(|pin| match pin {
            AxisPin::Keep => {
                next += 1;
                Some(next - 1)
            }
            AxisPin::Pin => None,
        })
        .collect()
}

/// The GSUB or GPOS `tag` of the instance with its FeatureVariations
/// instanced (see [`crate::feature_variations::instance_table`]):
/// starting from `rebuilt` when an earlier step already rewrote the
/// table, else from the source. `None` when the table does not change.
pub(super) fn layout_variations(
    face: &Face<'_>,
    tag: [u8; 4],
    rebuilt: Option<Vec<u8>>,
    pinned: &[Option<i16>],
    new_axis: &[Option<u16>],
    warnings: &Warnings,
) -> Result<Option<Vec<u8>>, SubsetError> {
    let instanced = match &rebuilt {
        Some(bytes) => {
            crate::feature_variations::instance_table(bytes, tag, pinned, new_axis, warnings)?
        }
        None => match face.table_bytes(tag) {
            Ok(bytes) => {
                crate::feature_variations::instance_table(bytes, tag, pinned, new_axis, warnings)?
            }
            Err(_) => None,
        },
    };
    Ok(instanced.or(rebuilt))
}
