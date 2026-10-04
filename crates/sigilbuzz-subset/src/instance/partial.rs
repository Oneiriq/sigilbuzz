//! Partial instancing: pins some axes, keeps the rest, and rebuilds
//! every variation table for the kept axes; plus the axis and
//! FeatureVariations helpers the full instancer shares.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use super::axes::{bake_avar_partial, bake_fvar_partial};
use super::cvar;
use super::gdef_store::GdefBake;
use super::metrics::{bake_vorg, mvar_defaults, VorgBake};
use super::metrics_var::{bake_hvar_partial_with, bake_mvar_partial, bake_vvar_partial_with};
use super::store_remap::{bake_gdef_store_partial, remap_gpos_variation_indices};
use super::style::AxisLocations;
use super::{
    plan_coords, post_avar, push_base, push_glyf_tables, push_metric_tables, push_style_tables,
    AxisPin, InstanceInput, InstancedOutput,
};
use crate::base::BaseBake;
use crate::sfnt;
use crate::warnings::Warnings;
use crate::SubsetError;

/// Partial-instance bake: produces a reduced-axis variable font.
///
/// This path runs when `input.axis_pins` carries at least one
/// `AxisPin::Keep`.
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
/// - moves every default value to the new default (the pinned axes at
///   their pins, the kept axes at their defaults), where renderers apply
///   no variations, as HarfBuzz's instancer does: for a `glyf` font with
///   `gvar`, `glyf` and the `head` / `hhea` / `vhea` fields that follow
///   from it; for a CFF2 font, the charstrings' and Private DICTs'
///   blend defaults; `hmtx`, `vmtx` and `VORG`; the `OS/2`, `hhea`,
///   `vhea` and `post` fields `MVAR` varies; the GPOS values and
///   anchors and the GDEF carets. The regions left on the pinned axes
///   only are in those defaults now, so every variation table drops
///   them,
/// - moves the `BASE` coordinates varied through its store to the new
///   default and projects the store,
/// - adds the `cvar` tuples left on the pinned axes only to `cvt ` and
///   rebuilds `cvar` for the kept axes from the rest (see
///   [`cvar`](super::cvar)),
/// - rides `maxp` and the rest of the layout and other tables through
///   verbatim. The Keep-axis variations stay live; the Pin-axis
///   dimensions fold into the trimmed deltas so a shaper at
///   `(Keep coords)` produces what the source produced at
///   `(Keep coords, Pin coords)`, within rounding.
///
/// An `avar`, `cvar`, `HVAR`, `VVAR` or `MVAR` that cannot be rebuilt
/// for the kept axes (malformed, or an `avar` other than version 1) is
/// left out, never carried through with regions that still count the
/// pinned axes, and reported in [`InstancedOutput::warnings`]. The
/// bakes check every Offset32 and every count-times-size product, so a
/// crafted table cannot wrap a 32-bit `usize`; a rebuilt table that
/// outgrows its own offsets is an error.
///
/// `input.drop_var_tables` is not read here. The output keeps live
/// axes, so the trimmed variation tables always stay: they drive
/// those axes.
///
/// `locations` holds the user value of each pinned axis, which sets the
/// `OS/2` and `post` fields that follow from `wght`, `wdth` and `slnt`.
pub(super) fn partial_instance(
    face: &Face<'_>,
    input: &InstanceInput,
    locations: &AxisLocations,
) -> Result<InstancedOutput, SubsetError> {
    let pins = &input.axis_pins;
    let coords = &input.coords;

    // The coordinates after avar, the variation regions' space, in
    // HarfBuzz's two forms: the new default outlines and glyph metrics
    // are baked at the outline coordinates, as a HarfBuzz font set to
    // the pinned location draws them, and every variation table is
    // projected at the plan coordinates, as HarfBuzz's instancer pins
    // its axes (see `plan_coords`).
    let plan = plan_coords(face, coords)?;
    let outline = post_avar(face, coords)?;

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

    // The new default (the pinned axes at their pins, the kept axes at
    // their defaults) is where a renderer applies no variations, so
    // every default value moves there, as HarfBuzz's instancer moves
    // them: a glyf font's outlines and glyph metrics through `gvar`, a
    // CFF2 font's charstrings and Private DICTs through their blends,
    // the advances and origins through `HVAR`, `VVAR` and `VORG`, the
    // `OS/2`, `hhea`, `vhea` and `post` fields through `MVAR`, the GPOS
    // values, anchors and carets through `GDEF`'s store, `BASE`, and
    // `cvt`. Each variation table drops the regions on the pinned axes
    // only, whose deltas are now in those defaults.
    let fold_glyphs = face.record(tag::GLYF).is_some() && matches!(face.gvar(), Ok(Some(_)));
    let default_coords: Vec<f32> = outline
        .iter()
        .zip(pins)
        .map(|(&c, pin)| if *pin == AxisPin::Keep { 0.0 } else { c })
        .collect();
    let mvar_bake = mvar_defaults(face, &plan, pins)?;
    // CFF2 VarStore + blend-operator rewrite (optional). VarStore
    // region trim via `bake_ivs_partial`; charstrings re-emit blend
    // ops with the surviving regions and pre-scaled deltas. It runs
    // before the metrics, which draw the outlines within the work it
    // allowed.
    if let Ok(cff2_bytes) = face.table_bytes(tag::CFF2) {
        let new_cff2 = crate::cff2::bake_cff2_partial(cff2_bytes, &plan, pins)?;
        tables.push((tag::CFF2, new_cff2));
    }
    let glyf_bake = if fold_glyphs {
        Some(push_glyf_tables(
            face,
            &default_coords,
            &mvar_bake,
            &warnings,
            &mut tables,
        )?)
    } else {
        None
    };
    let metrics_bake = if fold_glyphs {
        None
    } else {
        Some(push_metric_tables(
            face,
            &default_coords,
            &mvar_bake,
            &warnings,
            &mut tables,
        )?)
    };
    let vorg_bake = bake_vorg(face, &default_coords, face.maxp()?.num_glyphs, &warnings);
    if let VorgBake::Rebuilt(b) = &vorg_bake {
        tables.push((tag::VORG, b.clone()));
    }
    let avg_char_width = match (&glyf_bake, &metrics_bake) {
        (Some(b), _) => b.avg_char_width,
        (None, Some(m)) => m.avg_char_width,
        (None, None) => 0,
    };
    push_style_tables(face, &mvar_bake, avg_char_width, locations, &mut tables);

    // HVAR / VVAR / MVAR rewrites (optional). A malformed table is
    // dropped (its metrics stop varying) and reported.
    let hvar = |b: &[u8]| bake_hvar_partial_with(b, &plan, pins);
    let vvar = |b: &[u8]| bake_vvar_partial_with(b, &plan, pins);
    let mvar = |b: &[u8]| bake_mvar_partial(b, &plan, pins);
    type MetricsBake<'b> = &'b dyn Fn(&[u8]) -> Result<Vec<u8>, SubsetError>;
    let metrics_bakes: [([u8; 4], MetricsBake<'_>); 3] =
        [(tag::HVAR, &hvar), (tag::VVAR, &vvar), (tag::MVAR, &mvar)];
    for (table, bake) in metrics_bakes {
        let Ok(bytes) = face.table_bytes(table) else {
            continue;
        };
        match bake(bytes) {
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
    let (gdef_bake, store_remap) = bake_gdef_store_partial(face, &plan, pins, &warnings)?;
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
    let pinned = pinned_axes(&plan, pins);
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

    // gvar tuple-projection rewrite (optional). Pin-axis support
    // scalars fold into per-point deltas; Pin-axis dimensions drop
    // from every tuple region; tuples whose Pin-axis support is zero
    // disappear, and so do those left on the pinned axes only, whose
    // deltas the glyf bake moved into the default outlines. Output
    // gvar's axisCount = Keep-axis count.
    if let Ok(gvar_bytes) = face.table_bytes(tag::GVAR) {
        let new_axis_count = pins.iter().filter(|p| matches!(p, AxisPin::Keep)).count() as u16;
        let src_glyf = face.table_bytes(tag::GLYF).ok();
        let src_loca = face.loca().ok();
        let points = |gid: u16| {
            let (bake, glyf, loca) = (glyf_bake.as_ref()?, src_glyf?, src_loca.as_ref()?);
            let (start, end) = loca.range(gid)?;
            let body = glyf.get(start as usize..end as usize)?;
            bake.glyf.glyph_points(body, gid)
        };
        let new_gvar = crate::gvar_partial::bake_gvar_partial_with(
            gvar_bytes,
            &plan,
            pins,
            new_axis_count,
            &points,
            &warnings,
        )?;
        tables.push((tag::GVAR, new_gvar));
    }

    // BASE: the coordinates its store varies move to the new default,
    // and the store keeps the kept axes.
    let base_bake = push_base(face, &plan, pins, &warnings, &mut tables);

    // cvt takes the cvar tuples left on the pinned axes only; cvar
    // keeps the rest, for the kept axes.
    if let Some(bake) = cvar::bake_cvt(face, &plan, pins, &warnings) {
        if let Some(cvt) = bake.cvt {
            tables.push((cvar::CVT, cvt));
        }
        match bake.cvar {
            Some(new) => tables.push((cvar::CVAR, new)),
            None => dropped.push(cvar::CVAR),
        }
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
        if dropped.contains(&rec.tag) || (rec.tag == tag::BASE && base_bake == BaseBake::Dropped) {
            continue;
        }
        // Vertical metrics the glyph bake could not read, and a VORG
        // it could not fold the pinned axes into, are left out.
        let left_out = glyf_bake
            .as_ref()
            .map(|b| &b.vmtx)
            .or(metrics_bake.as_ref().map(|m| &m.vmtx))
            .is_some_and(|v| v.left_out.contains(&rec.tag));
        if left_out || (rec.tag == tag::VORG && vorg_bake == VorgBake::Dropped) {
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
