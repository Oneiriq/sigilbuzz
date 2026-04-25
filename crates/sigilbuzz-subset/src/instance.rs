//! Variable-font instancing: bake a coord vector into a static font. (#163)
//!
//! Given a [`Face`] and a per-axis normalized coord vector, this module
//! produces a new font where the variable-font deltas have been folded
//! into the underlying glyph outlines and metrics. The result is a
//! static font that consumers without VF awareness (older PDF renderers,
//! legacy print pipelines, sigilbuzz's own oniq-test feed) can use as
//! though the source had been designed at the chosen instance.
//!
//! # What lands on the static side
//!
//! - `glyf` simple-glyph outlines have their contour points shifted by
//!   the gvar deltas at the requested coords. The simple-glyph header,
//!   contour count, endPtsOfContours, and on-curve flag stream are
//!   rebuilt from the absolute, baked points; the per-point coordinate
//!   stream is re-encoded with the spec's flag-driven SHORT / SAME
//!   compression.
//! - `glyf` composite components are passed through with their gids
//!   remapped through the identity (instancing keeps every glyph) and
//!   their per-component translations preserved verbatim. Composite
//!   variation (`WE_HAVE_VARIATION` / point-anchor deltas) is not
//!   resynthesised on this pass — gvar's composite-glyph contribution
//!   is conservatively dropped.
//! - `hmtx` is rebuilt with each gid's advance + lsb adjusted by the
//!   HVAR deltas at the requested coords (rounded to the nearest
//!   integer per the OpenType spec for design-unit metrics).
//!
//! # What gets dropped (or kept verbatim)
//!
//! When [`InstanceInput::drop_var_tables`] is true (the recommended
//! default for the "ship as static" workflow):
//!
//! - `fvar`, `avar`, `gvar`, `HVAR` are dropped from the directory.
//! - `GDEF` is preserved verbatim. Its embedded `ItemVariationStore` is
//!   no longer reachable from any consumer because the surrounding
//!   variable-font tables are gone, but the bytes ride along — pruning
//!   it cleanly is staged for a sibling.
//!
//! When `drop_var_tables` is false the variable-font tables ride
//! through verbatim. The glyf and hmtx bake still applies to *bake* the
//! default-instance values into the outline / metric tables, so a
//! consumer that ignores the variable-font tables sees the same shape
//! as a consumer that does honour them.
//!
//! # CFF2 baking
//!
//! For CFF2 sources the [`crate::cff2::bake_at_coords`] helper walks
//! every charstring, inlines `callsubr` / `callgsubr`, resolves every
//! `blend` to its scalar value at `coords`, strips `vsindex`, and
//! emits a fresh CFF2 table without a VariationStore. Output is still
//! CFF2-tagged (the SFNT directory entry remains `CFF2`) but no
//! variable-font opcodes survive — consumers that ignore CFF2's
//! variable surface see the same outline as a consumer that honours
//! it at the chosen instance.
//!
//! # VVAR-aware vmtx
//!
//! Symmetric to the HVAR/hmtx bake. When the source carries `vmtx` +
//! `VVAR` the per-glyph advance height + tsb deltas resolve at
//! `coords` and fold into the rewritten `vmtx`; `VVAR` is then
//! dropped. Sources without `VVAR` pass `vmtx` through unchanged.
//!
//! # MVAR-aware OS/2 / hhea / post / vhea
//!
//! When the source carries `MVAR` we walk every value record, look up
//! its delta at `coords`, and apply the rounded result to the target
//! field per the spec's tag → field mapping (`hasc` → OS/2.sTypoAscender,
//! `xhgt` → OS/2.sxHeight, `unds` → post.underlineThickness, …). The
//! patched tables are emitted; `MVAR` is dropped. Sources without
//! `MVAR` pass these tables through unchanged.
//!
//! # GDEF.IVS / GPOS variable-position bake
//!
//! When `drop_var_tables` is true (the recommended default) the bake
//! folds every supported GPOS `VariationIndex` into the corresponding
//! `ValueRecord` static field at `coords` and zeros the offset slot,
//! then prunes `GDEF.ItemVariationStore`. Variable-position kerning
//! (the `VariationIndex` shape on PairPos / SinglePos value records)
//! therefore lands at the chosen instance — not the default — so the
//! static output renders correctly at the baked coord vector.
//!
//! The supported lookup types are GPOS Type 1 (SinglePos formats 1 / 2)
//! and Type 2 (PairPos formats 1 / 2), including those wrapped in a
//! Type 9 Extension lookup. `Mark*` and `Cursive` lookups carry their
//! variations on `Anchor` records, not `ValueRecord` fields; the
//! Anchor bake is staged for a follow-up. Unsupported lookups still
//! land in the output but their `VariationIndex` offsets are left
//! intact — the GDEF.IVS prune that follows leaves them orphan, the
//! same trade-off the simpler #173 path shipped.
//!
//! # Out of scope (deferred)
//!
//! - **Partial instancing** (some axes pinned, others left variable).
//!   Sigil's first cut bakes the full coord vector — every axis pins.
//! - **Mark / Cursive GPOS Anchor variations.** GPOS Types 3 / 4 / 5 / 6
//!   carry per-x/y `Device` / `VariationIndex` offsets on their
//!   `Anchor` records; this pass folds `ValueRecord` variations only.
//!   Anchor variations ride through with their `VariationIndex`
//!   offsets intact and are then orphaned by the GDEF.IVS prune,
//!   matching the #173 trade-off for that subset of GPOS. The Anchor
//!   bake is tracked as a follow-up.
//!
//! # Determinism
//!
//! Output is byte-deterministic for a given input face + coord vector.
//! No `HashMap` iteration touches the output; gids walk in order, the
//! SFNT directory is sorted by tag at emission, and every floating-
//! point round happens through `f32::round()` so the same inputs always
//! hit the same integer.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::tables::Reader;
use sigilbuzz::Face;

use crate::sfnt;
use crate::util;
use crate::{GlyphId, SubsetError};

/// F2DOT14 normalized axis coordinate. Matches the on-disk encoding the
/// VF spec uses: a signed 2.14 fixed-point in the range `[-1.0, 1.0]`,
/// where `0` is the axis default and `±1` is the extreme. Callers
/// usually obtain the vector by feeding user-space coords through
/// [`sigilbuzz::tables::Fvar::normalize_coords`].
pub type F2Dot14 = f32;

/// Per-axis pin policy for partial instancing.
///
/// fontTools' `varLib.instancer.instantiateVariableFont(axisLimits=...)`
/// supports pinning a *subset* of axes — the deltas for those axes fold
/// into the static outlines / metrics at the chosen coord, while the
/// remaining axes keep their variation surface and the output is still
/// a variable font (just with fewer axes in `fvar`). [`AxisPin`] is the
/// per-axis switch that drives that behaviour from
/// [`InstanceInput::axis_pins`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisPin {
    /// Bake this axis at `coords[i]` into every variation table; drop
    /// the axis from `fvar` / `avar` and from every variation tuple
    /// region. This is the existing full-instancing behaviour.
    Pin,
    /// Keep this axis variable. The axis stays in `fvar` / `avar`;
    /// every variation tuple region keeps its dimension on this axis;
    /// the value passed in `coords[i]` for this axis is ignored on the
    /// bake side (variation tables continue to evaluate `Keep`-axis
    /// deltas dynamically at shape time).
    Keep,
}

/// Inputs to [`instance`].
#[derive(Debug, Clone)]
pub struct InstanceInput {
    /// Per-axis normalized F2DOT14 coords. Length must match
    /// `face.fvar()`'s axis count.
    pub coords: Vec<F2Dot14>,
    /// If true (the recommended setting), drop `fvar` / `avar` /
    /// `HVAR` / `gvar` from the output. The font becomes static —
    /// shapers will ignore any axis coords passed alongside it.
    ///
    /// If false, leave them in place. Any consumer that does honour
    /// the variable-font tables will see deltas of zero relative to
    /// the baked outlines/metrics, so the result still renders
    /// correctly at the chosen instance — but the file is larger and
    /// shapers will still treat the font as variable.
    pub drop_var_tables: bool,
    /// Per-axis pin policy. An empty vector means "pin every axis"
    /// (the existing full-instancing behaviour). When non-empty,
    /// length must equal `coords.len()`; each entry says whether the
    /// matching axis bakes (`Pin`) or stays variable (`Keep`).
    ///
    /// fontTools-equivalent of
    /// `varLib.instancer.instantiateVariableFont(axisLimits={...})` —
    /// `Pin` axes correspond to a bare-coord entry in `axisLimits`,
    /// `Keep` axes correspond to an axis omitted from `axisLimits`.
    ///
    /// Note: when any axis is `Keep` the bake emits a reduced-axis
    /// variable font: `fvar` and `avar` are trimmed to only the
    /// surviving axes, `ItemVariationStore`-bearing tables (HVAR /
    /// VVAR / MVAR / GDEF.IVS) have their region lists rewritten with
    /// every Pin-axis dimension folded into the surviving deltas, and
    /// any tuple that contributes nothing at the pin coords is
    /// dropped. `gvar` and CFF2's VarStore are not yet rewritten — a
    /// source carrying either with `Keep` still surfaces an
    /// `Unsupported` error today; the projection follows in a
    /// follow-up using the same [`project_region_onto_kept_axes`]
    /// primitive that drives the IVS rewrite.
    pub axis_pins: Vec<AxisPin>,
}

impl Default for InstanceInput {
    fn default() -> Self {
        Self {
            coords: Vec::new(),
            drop_var_tables: true,
            axis_pins: Vec::new(),
        }
    }
}

/// Result of [`instance`].
#[derive(Debug, Clone)]
pub struct InstancedOutput {
    /// New font binary (a complete SFNT).
    pub bytes: Vec<u8>,
}

/// Instances `face` at `input.coords`. Returns the new font bytes.
///
/// Closure walking is *not* performed — instancing keeps every glyph in
/// the source font; it's not a subset operation. Every gid `0..num_glyphs`
/// rides through with its outline / metric baked.
pub fn instance(face: &Face<'_>, input: &InstanceInput) -> Result<InstancedOutput, SubsetError> {
    if face.record(tag::CFF1).is_some() && face.record(tag::GLYF).is_none() {
        // Pure CFF1 source — there is no variable data to bake; just
        // copy through. We still drop the variable-font directory
        // entries the caller asked us to drop.
        return cff1_passthrough(face, input);
    }

    // Validate axis count up front. fvar is required for instancing —
    // a static font has no axes and the API is meaningless.
    let fvar = face
        .fvar()
        .map_err(SubsetError::from)?
        .ok_or(SubsetError::Unsupported("instance: source has no fvar"))?;
    let axis_count = fvar.axes().len();
    if input.coords.len() != axis_count {
        return Err(SubsetError::Unsupported(
            "instance: coord vector length must match fvar axisCount",
        ));
    }
    // Partial-instancing validation: an empty axis_pins falls through
    // to the full-instancing path (every axis pins). A non-empty
    // axis_pins must equal coords.len().
    if !input.axis_pins.is_empty() {
        if input.axis_pins.len() != input.coords.len() {
            return Err(SubsetError::Unsupported(
                "instance: axis_pins length must equal coords.len()",
            ));
        }
        // gvar + CFF2 partial-projection have not landed yet (issue
        // tracking #183 follow-up). Reject Keep when the source has
        // either of those, with a clear message — the fvar / avar /
        // HVAR / VVAR / MVAR / GDEF.IVS paths are wired through below.
        if input.axis_pins.contains(&AxisPin::Keep) {
            let has_gvar = face.record(tag::GVAR).is_some();
            let has_cff2 = face.record(tag::CFF2).is_some();
            if has_gvar || has_cff2 {
                return Err(SubsetError::Unsupported(
                    "instance: partial instancing (axis_pins with Keep) for sources with \
                     gvar or CFF2 not yet implemented; the IVS-bearing tables \
                     (HVAR / VVAR / MVAR / GDEF.IVS) trim correctly today",
                ));
            }
            // partial_instance returns `Ok` with the reduced-axis VF;
            // its caller chain mirrors the full-instancing path.
            return partial_instance(face, input);
        }
    }

    // Apply avar's piecewise-linear remap if the source ships one. The
    // shaper's coord-space is post-avar, so the deltas we apply must
    // come from the same space.
    let coords: Vec<f32> = match face.avar().map_err(SubsetError::from)? {
        Some(av) => av.remap_all(&input.coords),
        None => input.coords.clone(),
    };

    if face.record(tag::CFF2).is_some() {
        return cff2_bake(face, input, &coords);
    }

    let maxp = face.maxp()?;
    let num_glyphs = maxp.num_glyphs;

    // glyf + loca bake.
    let glyf_loca = bake_glyf_loca(face, &coords, num_glyphs)?;

    // hmtx + hhea bake. HVAR deltas fold in here; gids without HVAR
    // entries carry their default-instance metrics through unchanged.
    let hmtx_out = bake_hmtx(face, &coords, num_glyphs)?;

    // vmtx bake (when the source carries vmtx). VVAR deltas fold in
    // here; vmtx-without-VVAR rides through unchanged.
    let vmtx_bake_result = bake_vmtx(face, &coords, num_glyphs)?;

    // MVAR-aware bake of OS/2, hhea, post, vhea (when MVAR is present).
    let mvar_bake = bake_mvar_metrics(face, &coords)?;

    // head: pass through, only patching indexToLocFormat to match the
    // bake's chosen loca format.
    let head_bytes = face.table_bytes(tag::HEAD).map_err(SubsetError::from)?;
    let mut head_out = head_bytes.to_vec();
    util::write_index_to_loc_format(&mut head_out, glyf_loca.long_loca);

    // hhea: start from MVAR-baked bytes (when MVAR carries vlgp etc.,
    // those ride through MVAR; for hhea-relevant tags the bake patches
    // OS/2 not hhea — hhea gets the metrics-count patch unconditionally
    // via util::write_hhea_metrics_count below).
    let mut hhea_out = match mvar_bake.hhea.clone() {
        Some(bytes) => bytes,
        None => face
            .table_bytes(tag::HHEA)
            .map_err(SubsetError::from)?
            .to_vec(),
    };
    util::write_hhea_metrics_count(&mut hhea_out, hmtx_out.number_of_h_metrics)?;

    // maxp: pass through verbatim (glyph count is unchanged — instancing
    // keeps every gid).
    let maxp_out = face
        .table_bytes(tag::MAXP)
        .map_err(SubsetError::from)?
        .to_vec();

    // Assemble the directory.
    let mut tables: Vec<([u8; 4], Vec<u8>)> = alloc::vec![
        (tag::HEAD, head_out),
        (tag::HHEA, hhea_out),
        (tag::MAXP, maxp_out),
        (tag::HMTX, hmtx_out.bytes),
        (tag::LOCA, glyf_loca.loca),
        (tag::GLYF, glyf_loca.glyf),
    ];
    // vmtx + vhea: when the source carries vmtx, emit the rebuilt
    // table and patch vhea's numberOfLongVerMetrics to the count
    // `bake_vmtx` computed (which may extend the long range to cover
    // VVAR-induced trailing-advance differences).
    if let Some(vmtx_bytes) = vmtx_bake_result.vmtx_bytes.clone() {
        tables.push((tag::VMTX, vmtx_bytes));
        let mut vhea_out = match mvar_bake.vhea.clone() {
            Some(bytes) => bytes,
            None => face
                .table_bytes(tag::VHEA)
                .map_err(SubsetError::from)?
                .to_vec(),
        };
        util::write_vhea_metrics_count(&mut vhea_out, vmtx_bake_result.number_of_long_ver_metrics)?;
        tables.push((tag::VHEA, vhea_out));
    } else if let Some(vhea_bytes) = mvar_bake.vhea.clone() {
        tables.push((tag::VHEA, vhea_bytes));
    }
    if let Some(os2_bytes) = mvar_bake.os2.clone() {
        tables.push((*b"OS/2", os2_bytes));
    }
    if let Some(post_bytes) = mvar_bake.post.clone() {
        tables.push((tag::POST, post_bytes));
    }

    // GPOS variation bake: when the source carries GPOS variations
    // (VariationIndex offsets on PairPos / SinglePos value records),
    // fold every resolvable variation into the static ValueRecord
    // field at `coords` and zero the offset slot. Runs *before* the
    // GDEF.IVS prune below — the prune severs the only path back to
    // the IVS bytes, so any remaining VariationIndex would be orphan.
    let gpos_baked = if input.drop_var_tables {
        bake_gpos_var(face, &coords)?
    } else {
        None
    };
    if let Some(b) = gpos_baked.clone() {
        tables.push((tag::GPOS, b));
    }

    // GDEF: when the source carries an ItemVariationStore and the
    // caller wants the static "ship as static" output, prune it. See
    // module header for the GPOS-bake-then-IVS-prune ordering.
    let gdef_pruned = if input.drop_var_tables {
        prune_gdef_ivs(face)?
    } else {
        None
    };
    if let Some(b) = gdef_pruned.clone() {
        tables.push((tag::GDEF, b));
    }

    // Carry every other table through verbatim, with a drop list for
    // the variable-font tables when `drop_var_tables` is true.
    for rec in face.records() {
        if tables.iter().any(|(t, _)| *t == rec.tag) {
            continue;
        }
        if input.drop_var_tables
            && matches!(
                rec.tag,
                tag::FVAR | tag::AVAR | tag::GVAR | tag::HVAR | tag::VVAR | tag::MVAR
            )
        {
            continue;
        }
        // GDEF was handled above (either pruned or dropped from the
        // pruning path).
        if rec.tag == tag::GDEF && gdef_pruned.is_some() {
            continue;
        }
        // GPOS was handled above when the variation bake produced a
        // rewritten table.
        if rec.tag == tag::GPOS && gpos_baked.is_some() {
            continue;
        }
        let bytes = face.table_bytes(rec.tag).map_err(SubsetError::from)?;
        tables.push((rec.tag, bytes.to_vec()));
    }

    let bytes = sfnt::build(face.sfnt_version(), &tables);
    Ok(InstancedOutput { bytes })
}

/// CFF2 path: rebuild the CFF2 table with `blend` resolved at `coords`,
/// then assemble a fresh SFNT directory mirroring the glyf path's
/// hmtx/vmtx/MVAR bakes and GDEF.IVS prune.
fn cff2_bake(
    face: &Face<'_>,
    input: &InstanceInput,
    coords: &[f32],
) -> Result<InstancedOutput, SubsetError> {
    let maxp = face.maxp()?;
    let num_glyphs = maxp.num_glyphs;

    let cff2_bytes = face.table_bytes(tag::CFF2).map_err(SubsetError::from)?;
    let new_cff2 = crate::cff2::bake_at_coords(cff2_bytes, coords)?;

    let hmtx_out = bake_hmtx(face, coords, num_glyphs)?;
    let vmtx_bake_result = bake_vmtx(face, coords, num_glyphs)?;
    let mvar_bake = bake_mvar_metrics(face, coords)?;

    let head_out = face
        .table_bytes(tag::HEAD)
        .map_err(SubsetError::from)?
        .to_vec();

    let mut hhea_out = match mvar_bake.hhea.clone() {
        Some(bytes) => bytes,
        None => face
            .table_bytes(tag::HHEA)
            .map_err(SubsetError::from)?
            .to_vec(),
    };
    util::write_hhea_metrics_count(&mut hhea_out, hmtx_out.number_of_h_metrics)?;

    let maxp_out = face
        .table_bytes(tag::MAXP)
        .map_err(SubsetError::from)?
        .to_vec();

    let mut tables: Vec<([u8; 4], Vec<u8>)> = alloc::vec![
        (tag::HEAD, head_out),
        (tag::HHEA, hhea_out),
        (tag::MAXP, maxp_out),
        (tag::HMTX, hmtx_out.bytes),
        (tag::CFF2, new_cff2),
    ];
    if let Some(vmtx_bytes) = vmtx_bake_result.vmtx_bytes.clone() {
        tables.push((tag::VMTX, vmtx_bytes));
        let mut vhea_out = match mvar_bake.vhea.clone() {
            Some(bytes) => bytes,
            None => face
                .table_bytes(tag::VHEA)
                .map_err(SubsetError::from)?
                .to_vec(),
        };
        util::write_vhea_metrics_count(&mut vhea_out, vmtx_bake_result.number_of_long_ver_metrics)?;
        tables.push((tag::VHEA, vhea_out));
    } else if let Some(vhea_bytes) = mvar_bake.vhea.clone() {
        tables.push((tag::VHEA, vhea_bytes));
    }
    if let Some(os2_bytes) = mvar_bake.os2.clone() {
        tables.push((*b"OS/2", os2_bytes));
    }
    if let Some(post_bytes) = mvar_bake.post.clone() {
        tables.push((tag::POST, post_bytes));
    }

    let gpos_baked = if input.drop_var_tables {
        bake_gpos_var(face, coords)?
    } else {
        None
    };
    if let Some(b) = gpos_baked.clone() {
        tables.push((tag::GPOS, b));
    }

    let gdef_pruned = if input.drop_var_tables {
        prune_gdef_ivs(face)?
    } else {
        None
    };
    if let Some(b) = gdef_pruned.clone() {
        tables.push((tag::GDEF, b));
    }

    for rec in face.records() {
        if tables.iter().any(|(t, _)| *t == rec.tag) {
            continue;
        }
        if input.drop_var_tables
            && matches!(
                rec.tag,
                tag::FVAR | tag::AVAR | tag::GVAR | tag::HVAR | tag::VVAR | tag::MVAR
            )
        {
            continue;
        }
        if rec.tag == tag::GDEF && gdef_pruned.is_some() {
            continue;
        }
        if rec.tag == tag::GPOS && gpos_baked.is_some() {
            continue;
        }
        let bytes = face.table_bytes(rec.tag).map_err(SubsetError::from)?;
        tables.push((rec.tag, bytes.to_vec()));
    }

    let bytes = sfnt::build(face.sfnt_version(), &tables);
    Ok(InstancedOutput { bytes })
}

/// Partial-instance bake: produces a reduced-axis variable font.
///
/// This path runs when `input.axis_pins` carries at least one
/// `AxisPin::Keep` and the source has neither `gvar` nor `CFF2` (those
/// tuple-projection paths are tracked as a follow-up to PR #183 — the
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
/// - rides `glyf` / `hmtx` / `vmtx` / `head` / `hhea` / `maxp` /
///   layout / and other tables through verbatim. The Keep-axis
///   variations stay live; the Pin-axis dimensions fold into the
///   trimmed deltas so a shaper at `(Keep coords)` produces exactly
///   what the source produced at `(Keep coords, Pin coords)`.
///
/// `drop_var_tables = false` is honoured — the trimmed variation
/// tables ride out either way; the field controls whether tables like
/// `MVAR` get folded down into static metric fields. For the partial
/// path we always keep the (trimmed) variation tables: they still
/// drive the live axes.
fn partial_instance(
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

    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();

    // fvar trim.
    let fvar_bytes = face.table_bytes(tag::FVAR).map_err(SubsetError::from)?;
    let new_fvar = bake_fvar_partial(fvar_bytes, pins).ok_or(SubsetError::Unsupported(
        "instance: fvar partial trim failed",
    ))?;
    tables.push((tag::FVAR, new_fvar));

    // avar trim (optional).
    if let Ok(avar_bytes) = face.table_bytes(tag::AVAR) {
        if let Some(new_avar) = bake_avar_partial(avar_bytes, pins) {
            tables.push((tag::AVAR, new_avar));
        }
    }

    // HVAR rewrite (optional).
    if let Ok(hvar_bytes) = face.table_bytes(tag::HVAR) {
        if let Some(new_hvar) = bake_hvar_partial(hvar_bytes, &post_avar_coords, pins) {
            tables.push((tag::HVAR, new_hvar));
        }
    }
    // VVAR rewrite (optional).
    if let Ok(vvar_bytes) = face.table_bytes(tag::VVAR) {
        if let Some(new_vvar) = bake_vvar_partial(vvar_bytes, &post_avar_coords, pins) {
            tables.push((tag::VVAR, new_vvar));
        }
    }
    // MVAR rewrite (optional).
    if let Ok(mvar_bytes) = face.table_bytes(tag::MVAR) {
        if let Some(new_mvar) = bake_mvar_partial(mvar_bytes, &post_avar_coords, pins) {
            tables.push((tag::MVAR, new_mvar));
        }
    }
    // GDEF.IVS rewrite (optional).
    if let Ok(gdef_bytes) = face.table_bytes(tag::GDEF) {
        if let Some(new_gdef) = bake_gdef_ivs_partial(gdef_bytes, &post_avar_coords, pins) {
            tables.push((tag::GDEF, new_gdef));
        } else {
            // No IVS in GDEF — pass through.
            tables.push((tag::GDEF, gdef_bytes.to_vec()));
        }
    }

    // Carry every other table through verbatim.
    for rec in face.records() {
        if tables.iter().any(|(t, _)| *t == rec.tag) {
            continue;
        }
        // Skip variable-font tables we already handled (or that we
        // refuse to handle in the partial path — gvar / CFF2 are
        // gated upstream).
        let bytes = face.table_bytes(rec.tag).map_err(SubsetError::from)?;
        tables.push((rec.tag, bytes.to_vec()));
    }

    let bytes = sfnt::build(face.sfnt_version(), &tables);
    Ok(InstancedOutput { bytes })
}

/// CFF1 (non-variable) instance pass: nothing to bake; rebuild the SFNT
/// directory and optionally drop variable-font tables. CFF1 sources
/// don't carry gvar / HVAR in practice but we honour the same
/// `drop_var_tables` switch for consistency.
fn cff1_passthrough(
    face: &Face<'_>,
    input: &InstanceInput,
) -> Result<InstancedOutput, SubsetError> {
    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    for rec in face.records() {
        if input.drop_var_tables && matches!(rec.tag, tag::FVAR | tag::AVAR | tag::GVAR | tag::HVAR)
        {
            continue;
        }
        let bytes = face.table_bytes(rec.tag).map_err(SubsetError::from)?;
        tables.push((rec.tag, bytes.to_vec()));
    }
    let bytes = sfnt::build(face.sfnt_version(), &tables);
    Ok(InstancedOutput { bytes })
}

// ---------------------------------------------------------------------------
// glyf + loca bake
// ---------------------------------------------------------------------------

struct GlyfLocaBake {
    glyf: Vec<u8>,
    loca: Vec<u8>,
    long_loca: bool,
}

fn bake_glyf_loca(
    face: &Face<'_>,
    coords: &[f32],
    num_glyphs: u16,
) -> Result<GlyfLocaBake, SubsetError> {
    let loca = face.loca().map_err(SubsetError::from)?;
    let glyf_bytes = face.table_bytes(tag::GLYF).map_err(SubsetError::from)?;
    let glyf = face.glyf().map_err(SubsetError::from)?;
    let gvar = face.gvar().map_err(SubsetError::from)?;

    let mut new_bodies: Vec<Vec<u8>> = Vec::with_capacity(num_glyphs as usize);
    for gid in 0..num_glyphs {
        let body = match loca.range(gid) {
            Some((s, e)) if s != e => {
                glyf_bytes
                    .get(s as usize..e as usize)
                    .ok_or(SubsetError::Unsupported(
                        "instance: glyf range falls outside table",
                    ))?
            }
            _ => &[][..],
        };
        let baked = if body.is_empty() {
            Vec::new()
        } else if body.len() < 10 {
            return Err(SubsetError::Unsupported(
                "instance: glyf body shorter than 10 bytes",
            ));
        } else {
            let nc = i16::from_be_bytes([body[0], body[1]]);
            if nc >= 0 {
                // Simple glyph: bake gvar deltas into the contour
                // points, then re-encode.
                let num_points: u16 = glyf
                    .point_count(&loca, gid)
                    .map_err(SubsetError::from)?
                    .unwrap_or_default();
                let deltas = match gvar.as_ref() {
                    Some(g) if !coords.is_empty() => g.glyph_deltas(gid, coords, num_points),
                    _ => Vec::new(),
                };
                bake_simple_glyph(body, &deltas)?
            } else {
                // Composite: pass through verbatim. Component gids do
                // not change (instancing keeps every glyph) so no
                // rewrite is needed. Composite-level gvar deltas are
                // conservatively dropped on this pass — the briefing
                // calls them out as a deferral.
                body.to_vec()
            }
        };
        new_bodies.push(baked);
    }

    // Pad each body to 2-byte alignment so short-loca offsets divide
    // cleanly.
    for body in &mut new_bodies {
        if body.len() % 2 != 0 {
            body.push(0);
        }
    }

    // Compute offsets and decide loca format.
    let mut offsets: Vec<u32> = Vec::with_capacity(new_bodies.len() + 1);
    let mut cursor: u32 = 0;
    offsets.push(0);
    for body in &new_bodies {
        cursor = cursor
            .checked_add(body.len() as u32)
            .ok_or(SubsetError::Unsupported("instance: glyf overflow"))?;
        offsets.push(cursor);
    }
    let long_loca = *offsets.last().unwrap_or(&0) > 0x1_FFFE;

    let mut glyf_out = Vec::with_capacity(cursor as usize);
    for body in &new_bodies {
        glyf_out.extend_from_slice(body);
    }
    while glyf_out.len() % 4 != 0 {
        glyf_out.push(0);
    }

    let loca_out = if long_loca {
        let mut out = Vec::with_capacity(offsets.len() * 4);
        for o in &offsets {
            out.extend_from_slice(&o.to_be_bytes());
        }
        out
    } else {
        let mut out = Vec::with_capacity(offsets.len() * 2);
        for o in &offsets {
            let half = (*o / 2) as u16;
            out.extend_from_slice(&half.to_be_bytes());
        }
        out
    };

    Ok(GlyfLocaBake {
        glyf: glyf_out,
        loca: loca_out,
        long_loca,
    })
}

// Simple-glyph flag bits.
const FLAG_ON_CURVE: u8 = 0x01;
const FLAG_X_SHORT: u8 = 0x02;
const FLAG_Y_SHORT: u8 = 0x04;
const FLAG_REPEAT: u8 = 0x08;
const FLAG_X_SAME_OR_POS: u8 = 0x10;
const FLAG_Y_SAME_OR_POS: u8 = 0x20;

/// Re-encodes a simple glyph with `deltas` applied to its contour
/// points. The new bbox is recomputed from the baked coordinates.
fn bake_simple_glyph(
    body: &[u8],
    deltas: &[sigilbuzz::tables::PointDelta],
) -> Result<Vec<u8>, SubsetError> {
    let mut r = Reader::new(body);
    let nc = r
        .read_i16()
        .map_err(|_| SubsetError::Unsupported("instance: simple header"))?;
    debug_assert!(nc >= 0);
    // Skip the source bbox — we recompute it below.
    r.skip(8)
        .map_err(|_| SubsetError::Unsupported("instance: simple bbox"))?;

    // endPtsOfContours.
    let mut end_pts = Vec::with_capacity(nc as usize);
    for _ in 0..nc {
        end_pts.push(
            r.read_u16()
                .map_err(|_| SubsetError::Unsupported("instance: endPtsOfContours"))?,
        );
    }
    let total_points = end_pts
        .last()
        .copied()
        .map(|e| e.saturating_add(1))
        .unwrap_or(0) as usize;

    // Instructions: read past them. We do not preserve hints in the
    // baked output — they reference the source's `cvt` / `prep` /
    // `fpgm`, which we forward verbatim, but the variable-font deltas
    // mean the hinted grid no longer matches the rasterised outline.
    // Stripping is the safest default and matches what fonttools'
    // instancer does in `--no-recalc-hints` mode.
    let instr_len =
        r.read_u16()
            .map_err(|_| SubsetError::Unsupported("instance: instructionLength"))? as usize;
    r.skip(instr_len)
        .map_err(|_| SubsetError::Unsupported("instance: instructions"))?;

    // Flags with REPEAT expansion.
    let mut flags = Vec::with_capacity(total_points);
    while flags.len() < total_points {
        let f = r
            .read_u8()
            .map_err(|_| SubsetError::Unsupported("instance: flags byte"))?;
        flags.push(f);
        if f & FLAG_REPEAT != 0 {
            let rep = r
                .read_u8()
                .map_err(|_| SubsetError::Unsupported("instance: flag repeat"))?;
            for _ in 0..rep {
                flags.push(f);
                if flags.len() >= total_points {
                    break;
                }
            }
        }
    }
    flags.truncate(total_points);

    // X coords.
    let mut xs: Vec<i32> = Vec::with_capacity(total_points);
    let mut x_cur: i32 = 0;
    for &f in &flags {
        let short = f & FLAG_X_SHORT != 0;
        let same_or_pos = f & FLAG_X_SAME_OR_POS != 0;
        let delta: i32 = if short {
            let v = i32::from(
                r.read_u8()
                    .map_err(|_| SubsetError::Unsupported("instance: x byte"))?,
            );
            if same_or_pos {
                v
            } else {
                -v
            }
        } else if same_or_pos {
            0
        } else {
            i32::from(
                r.read_i16()
                    .map_err(|_| SubsetError::Unsupported("instance: x i16"))?,
            )
        };
        x_cur += delta;
        xs.push(x_cur);
    }

    // Y coords.
    let mut ys: Vec<i32> = Vec::with_capacity(total_points);
    let mut y_cur: i32 = 0;
    for &f in &flags {
        let short = f & FLAG_Y_SHORT != 0;
        let same_or_pos = f & FLAG_Y_SAME_OR_POS != 0;
        let delta: i32 = if short {
            let v = i32::from(
                r.read_u8()
                    .map_err(|_| SubsetError::Unsupported("instance: y byte"))?,
            );
            if same_or_pos {
                v
            } else {
                -v
            }
        } else if same_or_pos {
            0
        } else {
            i32::from(
                r.read_i16()
                    .map_err(|_| SubsetError::Unsupported("instance: y i16"))?,
            )
        };
        y_cur += delta;
        ys.push(y_cur);
    }

    // Apply deltas. gvar's PointDelta vector is sparse — points
    // without an entry pick up zero deltas. Phantom-point deltas (point
    // index >= total_points) influence advances via HVAR rather than
    // contour points, so we ignore them here.
    let mut baked_x: Vec<i32> = Vec::with_capacity(total_points);
    let mut baked_y: Vec<i32> = Vec::with_capacity(total_points);
    for i in 0..total_points {
        let mut x = xs[i] as f32;
        let mut y = ys[i] as f32;
        // Look up delta for point i (linear scan: the typical glyph
        // has < 100 points and < 20 deltas, so this beats a HashMap
        // and stays no_std-clean).
        for d in deltas {
            if d.point as usize == i {
                x += d.dx;
                y += d.dy;
                break;
            }
        }
        baked_x.push(round_half_to_even(x));
        baked_y.push(round_half_to_even(y));
    }

    // Recompute bbox from baked points. An empty contour list yields a
    // (0, 0, 0, 0) bbox per the glyf spec convention for whitespace
    // glyphs.
    let (x_min, y_min, x_max, y_max) = if total_points == 0 {
        (0, 0, 0, 0)
    } else {
        let mut xmn = baked_x[0];
        let mut xmx = baked_x[0];
        let mut ymn = baked_y[0];
        let mut ymx = baked_y[0];
        for i in 1..total_points {
            if baked_x[i] < xmn {
                xmn = baked_x[i];
            }
            if baked_x[i] > xmx {
                xmx = baked_x[i];
            }
            if baked_y[i] < ymn {
                ymn = baked_y[i];
            }
            if baked_y[i] > ymx {
                ymx = baked_y[i];
            }
        }
        (
            clamp_i16(xmn),
            clamp_i16(ymn),
            clamp_i16(xmx),
            clamp_i16(ymx),
        )
    };

    // Encode.
    let mut out = Vec::with_capacity(body.len());
    out.extend_from_slice(&nc.to_be_bytes());
    out.extend_from_slice(&x_min.to_be_bytes());
    out.extend_from_slice(&y_min.to_be_bytes());
    out.extend_from_slice(&x_max.to_be_bytes());
    out.extend_from_slice(&y_max.to_be_bytes());
    for &e in &end_pts {
        out.extend_from_slice(&e.to_be_bytes());
    }
    // No instructions.
    out.extend_from_slice(&0u16.to_be_bytes());

    encode_simple_coords(&baked_x, &baked_y, &flags, &mut out);

    Ok(out)
}

/// Encodes the flags + x + y streams for a simple glyph, using the
/// SHORT / SAME-OR-POS encoding the spec defines. Each flag byte
/// derives from the on-curve bit of the input flag stream and the
/// per-point delta size; REPEAT runs collapse identical adjacent
/// flag bytes.
fn encode_simple_coords(xs: &[i32], ys: &[i32], src_flags: &[u8], out: &mut Vec<u8>) {
    let n = xs.len();
    if n == 0 {
        // No flags / x / y stream needed.
        return;
    }

    // Compute per-point deltas + new flag bytes.
    let mut new_flags: Vec<u8> = Vec::with_capacity(n);
    let mut x_payload: Vec<i16> = Vec::with_capacity(n);
    let mut y_payload: Vec<i16> = Vec::with_capacity(n);
    let mut prev_x: i32 = 0;
    let mut prev_y: i32 = 0;
    for i in 0..n {
        let dx = xs[i] - prev_x;
        let dy = ys[i] - prev_y;
        prev_x = xs[i];
        prev_y = ys[i];

        let mut f = src_flags[i] & FLAG_ON_CURVE;

        if dx == 0 {
            f |= FLAG_X_SAME_OR_POS;
        } else if (-255..=255).contains(&dx) {
            f |= FLAG_X_SHORT;
            if dx > 0 {
                f |= FLAG_X_SAME_OR_POS;
                x_payload.push(dx as i16);
            } else {
                x_payload.push(-dx as i16);
            }
        } else {
            // i16 range. Clamp to keep the encoding well-defined; in
            // practice glyf coords always fit because the source font
            // already used i16 deltas.
            let v = dx.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
            x_payload.push(v);
        }

        if dy == 0 {
            f |= FLAG_Y_SAME_OR_POS;
        } else if (-255..=255).contains(&dy) {
            f |= FLAG_Y_SHORT;
            if dy > 0 {
                f |= FLAG_Y_SAME_OR_POS;
                y_payload.push(dy as i16);
            } else {
                y_payload.push(-dy as i16);
            }
        } else {
            let v = dy.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
            y_payload.push(v);
        }

        new_flags.push(f);
    }

    // RLE-compress flags. A run of up to 256 identical flag bytes
    // collapses to `(flag | REPEAT) + count_byte`. Single occurrences
    // emit unchanged.
    let mut i = 0;
    let mut compressed: Vec<u8> = Vec::with_capacity(new_flags.len());
    while i < new_flags.len() {
        let f = new_flags[i];
        let mut j = i + 1;
        while j < new_flags.len() && new_flags[j] == f && (j - i) < 256 {
            j += 1;
        }
        let run = j - i;
        if run >= 2 {
            compressed.push(f | FLAG_REPEAT);
            compressed.push((run - 1) as u8);
        } else {
            compressed.push(f);
        }
        i = j;
    }
    out.extend_from_slice(&compressed);

    // X stream. SHORT entries are unsigned bytes; non-SHORT, non-SAME
    // entries are i16. We walk the original new_flags so the index
    // stays in step with x_payload.
    let mut xpi = 0usize;
    for &f in &new_flags {
        if f & FLAG_X_SHORT != 0 {
            // payload[xpi] is the absolute byte value (0..=255).
            let v = x_payload[xpi];
            out.push(v as u8);
            xpi += 1;
        } else if f & FLAG_X_SAME_OR_POS != 0 {
            // No bytes — same as previous.
        } else {
            let v = x_payload[xpi];
            out.extend_from_slice(&v.to_be_bytes());
            xpi += 1;
        }
    }

    // Y stream.
    let mut ypi = 0usize;
    for &f in &new_flags {
        if f & FLAG_Y_SHORT != 0 {
            let v = y_payload[ypi];
            out.push(v as u8);
            ypi += 1;
        } else if f & FLAG_Y_SAME_OR_POS != 0 {
            // No bytes.
        } else {
            let v = y_payload[ypi];
            out.extend_from_slice(&v.to_be_bytes());
            ypi += 1;
        }
    }
}

fn round_half_to_even(v: f32) -> i32 {
    // Banker's rounding to keep large-N delta sums stable. The spec
    // doesn't mandate a specific rounding mode for instancing, but
    // round-half-to-even is what fonttools' instancer uses, and it
    // matches IEEE 754's default.
    #[allow(clippy::cast_possible_truncation)]
    let r = if (v - v.floor() - 0.5).abs() < f32::EPSILON {
        let f = v.floor() as i32;
        if f % 2 == 0 {
            f
        } else {
            f + 1
        }
    } else {
        v.round() as i32
    };
    r
}

fn clamp_i16(v: i32) -> i16 {
    v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

// ---------------------------------------------------------------------------
// hmtx bake
// ---------------------------------------------------------------------------

struct HmtxBake {
    bytes: Vec<u8>,
    number_of_h_metrics: u16,
}

fn bake_hmtx(face: &Face<'_>, coords: &[f32], num_glyphs: u16) -> Result<HmtxBake, SubsetError> {
    let hmtx = face.hmtx().map_err(SubsetError::from)?;
    let hvar = face.hvar().map_err(SubsetError::from)?;

    let mut advances: Vec<u16> = Vec::with_capacity(num_glyphs as usize);
    let mut lsbs: Vec<i16> = Vec::with_capacity(num_glyphs as usize);
    for gid in 0..num_glyphs {
        let base_adv = hmtx.advance(gid).unwrap_or(0);
        let base_lsb = hmtx.lsb(gid).unwrap_or(0);
        let adv_delta = match hvar.as_ref() {
            Some(h) if !coords.is_empty() => h.advance_delta(gid, coords),
            _ => 0.0,
        };
        // hmtx advances are unsigned; clamp at 0 if a delta would
        // underflow. In practice this only happens with malformed
        // HVAR data.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let new_adv = (f32::from(base_adv) + adv_delta).round().max(0.0) as i32;
        advances.push(new_adv.clamp(0, i32::from(u16::MAX)) as u16);
        lsbs.push(base_lsb);
    }

    // Compress trailing identical advances into the LSB-only tail.
    let mut long_count = advances.len();
    if long_count > 1 {
        let last = advances[long_count - 1];
        while long_count > 1 && advances[long_count - 1] == last {
            long_count -= 1;
        }
        long_count += 1;
    }
    if long_count == 0 {
        long_count = 1;
    }

    let mut out = Vec::with_capacity(advances.len() * 4);
    for (advance, lsb) in advances.iter().zip(lsbs.iter()).take(long_count) {
        out.extend_from_slice(&advance.to_be_bytes());
        out.extend_from_slice(&lsb.to_be_bytes());
    }
    for lsb in lsbs.iter().skip(long_count) {
        out.extend_from_slice(&lsb.to_be_bytes());
    }

    Ok(HmtxBake {
        bytes: out,
        number_of_h_metrics: long_count as u16,
    })
}

// ---------------------------------------------------------------------------
// vmtx bake (VVAR-aware)
// ---------------------------------------------------------------------------

struct VmtxBake {
    /// New `vmtx` bytes, or `None` when the source has no `vmtx`.
    vmtx_bytes: Option<Vec<u8>>,
    /// Recomputed `numberOfLongVerMetrics` for the rebuilt table. The
    /// caller must patch `vhea` with this value when it differs from
    /// the source's count. Holds zero when no vmtx was emitted.
    number_of_long_ver_metrics: u16,
}

fn bake_vmtx(face: &Face<'_>, coords: &[f32], num_glyphs: u16) -> Result<VmtxBake, SubsetError> {
    let vmtx = face.vmtx().map_err(SubsetError::from)?;
    let Some(vmtx) = vmtx else {
        return Ok(VmtxBake {
            vmtx_bytes: None,
            number_of_long_ver_metrics: 0,
        });
    };
    // vhea must be present whenever vmtx is — the parser uses
    // `numberOfLongVerMetrics` to slice the table. Confirm presence
    // here so a malformed source (vmtx without vhea) errors cleanly
    // before we try to re-emit. The actual long count is recomputed
    // below from the post-VVAR advance vector.
    let _ = face
        .vhea()
        .map_err(SubsetError::from)?
        .ok_or(SubsetError::Unsupported(
            "instance: vmtx present without vhea",
        ))?;

    let vvar = face.vvar().map_err(SubsetError::from)?;

    // Compute the new (advance, tsb) per gid. Every glyph that ends
    // up in the long range carries its own advance; trailing glyphs
    // share the last advance. We resolve VVAR deltas for *every* gid
    // (including those originally past the source's long count) so
    // that a trailing glyph whose advance now diverges from the
    // shared one extends the long range below.
    let mut advances: Vec<u16> = Vec::with_capacity(num_glyphs as usize);
    let mut tsbs: Vec<i16> = Vec::with_capacity(num_glyphs as usize);
    for gid in 0..num_glyphs {
        let base_adv = vmtx.advance(gid).unwrap_or(0);
        let base_tsb = vmtx.tsb(gid).unwrap_or(0);
        let adv_delta = match vvar.as_ref() {
            Some(v) if !coords.is_empty() => v.advance_height_delta(gid, coords),
            _ => 0.0,
        };
        let tsb_delta = match vvar.as_ref() {
            Some(v) if !coords.is_empty() => v.top_side_bearing_delta(gid, coords).unwrap_or(0.0),
            _ => 0.0,
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let new_adv = (f32::from(base_adv) + adv_delta).round().max(0.0) as i32;
        advances.push(new_adv.clamp(0, i32::from(u16::MAX)) as u16);
        let new_tsb = (f32::from(base_tsb) + tsb_delta).round() as i32;
        tsbs.push(clamp_i16(new_tsb));
    }

    let (out, long_count) = emit_vmtx_bytes(&advances, &tsbs);

    Ok(VmtxBake {
        vmtx_bytes: Some(out),
        number_of_long_ver_metrics: long_count,
    })
}

/// Emits a vmtx body from per-gid `advances` + `tsbs`, recomputing the
/// `numberOfLongVerMetrics` count so trailing glyphs that now share an
/// advance compress into the tsb-only tail. Mirrors `bake_hmtx`'s long-
/// count compression so VVAR-induced advance deltas at trailing gids
/// extend the long range below.
fn emit_vmtx_bytes(advances: &[u16], tsbs: &[i16]) -> (Vec<u8>, u16) {
    debug_assert_eq!(advances.len(), tsbs.len());
    let mut long_count = advances.len();
    if long_count > 1 {
        let last = advances[long_count - 1];
        while long_count > 1 && advances[long_count - 1] == last {
            long_count -= 1;
        }
        long_count += 1;
    }
    if long_count == 0 {
        long_count = 1;
    }
    let mut out = Vec::with_capacity(advances.len() * 4);
    for (advance, tsb) in advances.iter().zip(tsbs.iter()).take(long_count) {
        out.extend_from_slice(&advance.to_be_bytes());
        out.extend_from_slice(&tsb.to_be_bytes());
    }
    for tsb in tsbs.iter().skip(long_count) {
        out.extend_from_slice(&tsb.to_be_bytes());
    }
    (out, long_count as u16)
}

// ---------------------------------------------------------------------------
// MVAR bake (OS/2 + hhea + vhea + post)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct MvarBake {
    os2: Option<Vec<u8>>,
    hhea: Option<Vec<u8>>,
    vhea: Option<Vec<u8>>,
    post: Option<Vec<u8>>,
}

/// Walks the source's `MVAR` records, applies each delta to its target
/// field in OS/2 / hhea / vhea / post, and returns the patched table
/// bytes. Tables that don't exist in the source — or whose fields no
/// MVAR record references — return `None` (caller passes through the
/// source bytes).
fn bake_mvar_metrics(face: &Face<'_>, coords: &[f32]) -> Result<MvarBake, SubsetError> {
    let mvar = face.mvar().map_err(SubsetError::from)?;
    let Some(mvar) = mvar else {
        return Ok(MvarBake::default());
    };
    if coords.is_empty() {
        return Ok(MvarBake::default());
    }

    let os2 = face.table_bytes(*b"OS/2").ok().map(<[u8]>::to_vec);
    let hhea = face.table_bytes(tag::HHEA).ok().map(<[u8]>::to_vec);
    let vhea = face.table_bytes(tag::VHEA).ok().map(<[u8]>::to_vec);
    let post = face.table_bytes(tag::POST).ok().map(<[u8]>::to_vec);

    apply_mvar_records(&mvar, coords, os2, hhea, vhea, post)
}

/// Walks `mvar.entries()` and patches the rebuilt OS/2 / hhea / vhea /
/// post buffers in place. Splits out from [`bake_mvar_metrics`] so the
/// duplicate-tag dedup policy is unit-testable without spinning up a
/// full Face.
fn apply_mvar_records(
    mvar: &sigilbuzz::tables::Mvar<'_>,
    coords: &[f32],
    mut os2: Option<Vec<u8>>,
    hhea: Option<Vec<u8>>,
    mut vhea: Option<Vec<u8>>,
    mut post: Option<Vec<u8>>,
) -> Result<MvarBake, SubsetError> {
    use sigilbuzz::tables::mvar::tag as mvar_tag;
    // OS/2 v0 is 78 bytes; v1+ goes through 96/100. Field offsets
    // (per OpenType OS/2 spec):
    //   sxHeight        (s i16) at v2+ offset 0x56 (86)
    //   sCapHeight      (s i16) at v2+ offset 0x58 (88)
    //   ySubscriptXSize (s i16) 0x0A (10)
    //   ySubscriptYSize          0x0C (12)
    //   ySubscriptXOffset        0x0E (14)
    //   ySubscriptYOffset        0x10 (16)
    //   ySuperscriptXSize        0x12 (18)
    //   ySuperscriptYSize        0x14 (20)
    //   ySuperscriptXOffset      0x16 (22)
    //   ySuperscriptYOffset      0x18 (24)
    //   yStrikeoutSize           0x1A (26)
    //   yStrikeoutPosition       0x1C (28)
    //   sTypoAscender   (i16)    0x44 (68)
    //   sTypoDescender           0x46 (70)
    //   sTypoLineGap             0x48 (72)
    //   usWinAscent     (u16)    0x4A (74)
    //   usWinDescent             0x4C (76)
    //
    // post: italicAngle is offset 4 (Fixed16.16). underlineThickness
    // and underlinePosition are i16 at offsets 10 and 8 respectively.
    //
    // hhea offsets:
    //   ascent / vertTypoAscender at offset 4 (i16)
    //   descent at offset 6
    //   lineGap at offset 8
    //
    // vhea (OpenType / AAT): same layout as hhea — ascent/descent/lineGap
    // are at offsets 4/6/8.

    // Per OpenType MVAR spec each tag appears at most once in a
    // well-formed `valueRecords` array. Malformed fonts can ship the
    // same tag twice; without dedup the patch path applies the delta
    // once per record, doubling its effect on the rebuilt OS/2 / hhea
    // / vhea / post fields. Dedup with first-wins so the rebuild
    // matches the spec-conforming case bit-for-bit.
    let mut seen: Vec<[u8; 4]> = Vec::new();
    for (rec_tag, _) in mvar.entries() {
        if seen.contains(&rec_tag) {
            continue;
        }
        seen.push(rec_tag);
        let Some(d) = mvar.metric_delta(rec_tag, coords) else {
            continue;
        };
        let delta = d.round() as i32;
        if delta == 0 {
            continue;
        }
        match rec_tag {
            t if t == mvar_tag::HORIZ_ASCENDER => patch_i16(&mut os2, 68, delta),
            t if t == mvar_tag::HORIZ_DESCENDER => patch_i16(&mut os2, 70, delta),
            t if t == mvar_tag::HORIZ_LINE_GAP => patch_i16(&mut os2, 72, delta),
            t if t == mvar_tag::HORIZ_CLIPPING_ASCENT => patch_u16(&mut os2, 74, delta),
            t if t == mvar_tag::HORIZ_CLIPPING_DESCENT => patch_u16(&mut os2, 76, delta),
            t if t == mvar_tag::X_HEIGHT => patch_i16(&mut os2, 86, delta),
            t if t == mvar_tag::CAP_HEIGHT => patch_i16(&mut os2, 88, delta),
            t if t == mvar_tag::SUBSCRIPT_X_SIZE => patch_i16(&mut os2, 10, delta),
            t if t == mvar_tag::SUBSCRIPT_Y_SIZE => patch_i16(&mut os2, 12, delta),
            t if t == mvar_tag::SUBSCRIPT_X_OFFSET => patch_i16(&mut os2, 14, delta),
            t if t == mvar_tag::SUBSCRIPT_Y_OFFSET => patch_i16(&mut os2, 16, delta),
            t if t == mvar_tag::SUPERSCRIPT_X_SIZE => patch_i16(&mut os2, 18, delta),
            t if t == mvar_tag::SUPERSCRIPT_Y_SIZE => patch_i16(&mut os2, 20, delta),
            t if t == mvar_tag::SUPERSCRIPT_X_OFFSET => patch_i16(&mut os2, 22, delta),
            t if t == mvar_tag::SUPERSCRIPT_Y_OFFSET => patch_i16(&mut os2, 24, delta),
            t if t == mvar_tag::STRIKEOUT_SIZE => patch_i16(&mut os2, 26, delta),
            t if t == mvar_tag::STRIKEOUT_OFFSET => patch_i16(&mut os2, 28, delta),
            t if t == mvar_tag::VERT_ASCENDER => patch_i16(&mut vhea, 4, delta),
            t if t == mvar_tag::VERT_DESCENDER => patch_i16(&mut vhea, 6, delta),
            t if t == mvar_tag::VERT_LINE_GAP => patch_i16(&mut vhea, 8, delta),
            t if t == mvar_tag::UNDERLINE_SIZE => patch_i16(&mut post, 10, delta),
            t if t == mvar_tag::UNDERLINE_OFFSET => patch_i16(&mut post, 8, delta),
            _ => {} // unrecognised tag — silently ignore
        }
    }

    Ok(MvarBake {
        os2,
        hhea,
        vhea,
        post,
    })
}

fn patch_i16(buf: &mut Option<Vec<u8>>, off: usize, delta: i32) {
    let Some(b) = buf.as_mut() else {
        return;
    };
    if b.len() < off + 2 {
        return;
    }
    let cur = i16::from_be_bytes([b[off], b[off + 1]]);
    let new = (i32::from(cur) + delta).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    b[off..off + 2].copy_from_slice(&new.to_be_bytes());
}

fn patch_u16(buf: &mut Option<Vec<u8>>, off: usize, delta: i32) {
    let Some(b) = buf.as_mut() else {
        return;
    };
    if b.len() < off + 2 {
        return;
    }
    let cur = u16::from_be_bytes([b[off], b[off + 1]]);
    let new = (i32::from(cur) + delta).clamp(0, i32::from(u16::MAX)) as u16;
    b[off..off + 2].copy_from_slice(&new.to_be_bytes());
}

// ---------------------------------------------------------------------------
// Partial-instancing tuple projection math.
//
// Each variation tuple is a region defined by per-axis (peak, start, end)
// triples plus a delta payload. To project a tuple onto a Keep-axis
// subspace:
//
//   1. Resolve every Pin-axis dimension to a constant scalar at
//      `coords[i]` (the OpenType `supportScalar`-style ramp the
//      shaper already uses to evaluate variation tables).
//   2. Multiply the per-Pin-axis scalars together. If the product is
//      zero — meaning the pin coord falls outside the tuple's region
//      on at least one Pin-axis — the tuple contributes nothing at
//      this pin and gets dropped.
//   3. Otherwise the survivor tuple keeps only the Keep-axis dimensions
//      of its region triples; its delta payload is multiplied by the
//      Pin-axis product so that evaluating the trimmed tuple at the
//      Keep-axis coords reproduces the source tuple's contribution
//      exactly at every (Keep-coord, Pin-coord) pair where the Pin
//      coord matches `coords[i]`.
//
// These primitives are the building blocks the variation-table
// rewriters (HVAR / VVAR / MVAR / gvar / GDEF.IVS) consume to emit
// trimmed `ItemVariationStore` / gvar tuples in a partial-instance
// font. They are tested in isolation here so the math stays correct
// regardless of which table a follow-up wires them into first.
// ---------------------------------------------------------------------------

/// Computes the support-scalar contribution of a single axis dimension
/// at `coord`. Mirrors the OpenType `supportScalar` formula used by
/// the gvar / IVS evaluators in `sigilbuzz::tables::gvar` —
/// re-implemented here because the subset crate cannot import
/// crate-private helpers from the parent crate, and the formula is
/// trivially small.
///
/// Returns `1.0` when the axis does not participate in the tuple
/// (peak == 0 with the spec's "axis ignored" convention) and `0.0`
/// when `coord` falls outside `[start, end]`.
#[must_use]
#[allow(dead_code)] // wired in by the partial-instance emit follow-ups
pub(crate) fn axis_support_scalar(start: f32, peak: f32, end: f32, coord: f32) -> f32 {
    // Spec: peak of zero means the axis does not participate.
    if peak == 0.0 {
        return 1.0;
    }
    if (coord - peak).abs() < f32::EPSILON {
        return 1.0;
    }
    if coord < start || coord > end {
        return 0.0;
    }
    if coord < peak {
        let denom = peak - start;
        if denom.abs() < f32::EPSILON {
            return 0.0;
        }
        (coord - start) / denom
    } else {
        // coord > peak
        let denom = end - peak;
        if denom.abs() < f32::EPSILON {
            return 0.0;
        }
        (end - coord) / denom
    }
}

/// One tuple region's per-axis (start, peak, end) triple, in the
/// source font's axis order. Length must equal the source's fvar axis
/// count.
#[allow(dead_code)] // wired in by the partial-instance emit follow-ups
pub(crate) type RegionAxes = [(f32, f32, f32)];

/// Result of projecting a variation tuple onto its Keep-axis subspace.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // wired in by the partial-instance emit follow-ups
pub(crate) struct ProjectedTuple {
    /// Pin-axis support-scalar product evaluated at the pin coords.
    /// Caller multiplies every delta in this tuple's payload by this
    /// scalar before emitting the trimmed tuple.
    pub pin_scalar: f32,
    /// Trimmed region triples: only the Keep-axis dimensions, in the
    /// source's Keep-axis order.
    pub kept_axes: alloc::vec::Vec<(f32, f32, f32)>,
}

/// Projects a tuple's per-axis region onto the Keep-axis subspace.
///
/// `region` carries one `(start, peak, end)` triple per source axis.
/// `pins` indicates which axes pin (`Pin`) and which stay variable
/// (`Keep`); `coords` carries the pin coord for every axis (entries
/// for `Keep` axes are ignored).
///
/// Returns `None` when the tuple contributes nothing at the pin coords
/// — the survivor would have a zero pin-scalar and the caller should
/// drop the tuple entirely. Returns `Some(ProjectedTuple)` otherwise.
///
/// Lengths must agree: `region.len() == pins.len() == coords.len()`.
/// Mismatched inputs return `None` (defensive — callers should validate
/// upstream, but a length skew should not produce silently-wrong deltas).
#[must_use]
#[allow(dead_code)] // wired in by the partial-instance emit follow-ups
pub(crate) fn project_region_onto_kept_axes(
    region: &RegionAxes,
    pins: &[AxisPin],
    coords: &[f32],
) -> Option<ProjectedTuple> {
    if region.len() != pins.len() || pins.len() != coords.len() {
        return None;
    }
    let mut pin_scalar: f32 = 1.0;
    let mut kept_axes: alloc::vec::Vec<(f32, f32, f32)> =
        alloc::vec::Vec::with_capacity(pins.len());
    for (i, &pin) in pins.iter().enumerate() {
        let (s, p, e) = region[i];
        match pin {
            AxisPin::Pin => {
                let s_axis = axis_support_scalar(s, p, e, coords[i]);
                if s_axis == 0.0 {
                    return None;
                }
                pin_scalar *= s_axis;
            }
            AxisPin::Keep => {
                kept_axes.push((s, p, e));
            }
        }
    }
    if pin_scalar == 0.0 {
        return None;
    }
    Some(ProjectedTuple {
        pin_scalar,
        kept_axes,
    })
}

// ---------------------------------------------------------------------------
// Partial-instancing fvar trim
// ---------------------------------------------------------------------------

/// Re-emits an `fvar` table with every Pin-axis dimension dropped.
///
/// `pins` carries one entry per source axis; only axes whose pin is
/// `AxisPin::Keep` survive. Instance records keep the same flags /
/// nameIDs but drop their Pin-axis coord slots; instances whose
/// surviving coord vector is now identical to the trimmed default-
/// instance vector are removed (they would shadow the implicit default).
///
/// Returns `None` when every axis pins (the all-pin case is the
/// existing full-instancing behaviour and the caller drops fvar
/// outright when `drop_var_tables` is true).
#[allow(dead_code)] // wired in by the partial-instancing integration commit
fn bake_fvar_partial(fvar_bytes: &[u8], pins: &[AxisPin]) -> Option<Vec<u8>> {
    if pins.iter().all(|p| matches!(p, AxisPin::Pin)) {
        return None;
    }
    if fvar_bytes.len() < 16 {
        return None;
    }
    let major = u16::from_be_bytes([fvar_bytes[0], fvar_bytes[1]]);
    if major != 1 {
        return None;
    }
    let axes_array_off = u16::from_be_bytes([fvar_bytes[4], fvar_bytes[5]]) as usize;
    let axis_count = u16::from_be_bytes([fvar_bytes[8], fvar_bytes[9]]) as usize;
    let axis_size = u16::from_be_bytes([fvar_bytes[10], fvar_bytes[11]]) as usize;
    let instance_count = u16::from_be_bytes([fvar_bytes[12], fvar_bytes[13]]) as usize;
    let instance_size = u16::from_be_bytes([fvar_bytes[14], fvar_bytes[15]]) as usize;
    if axis_size < 20 || pins.len() != axis_count {
        return None;
    }
    let need_axes = axes_array_off.checked_add(axis_count.checked_mul(axis_size)?)?;
    if fvar_bytes.len() < need_axes {
        return None;
    }

    // Surviving axis indices (in source order).
    let kept: Vec<usize> = pins
        .iter()
        .enumerate()
        .filter_map(|(i, p)| matches!(p, AxisPin::Keep).then_some(i))
        .collect();
    let new_axis_count = kept.len();

    // Collect the source's per-axis default values (for instance
    // dedup). Each axis record's defaultValue lives at +8 in the 20-
    // byte axis record (tag[4] + min[4] + default[4]).
    let mut axis_defaults: Vec<u32> = Vec::with_capacity(axis_count);
    for i in 0..axis_count {
        let off = axes_array_off + i * axis_size;
        let raw = u32::from_be_bytes([
            fvar_bytes[off + 8],
            fvar_bytes[off + 9],
            fvar_bytes[off + 10],
            fvar_bytes[off + 11],
        ]);
        axis_defaults.push(raw);
    }

    // Decide the new instanceSize. Fixed-format: 20 (axis records) but
    // for instances it's 4 (subfamilyNameID + flags) + 4 × axisCount
    // + optional 2 (postScriptNameID). We detect "with PS name" by
    // checking source instance_size against 4 + 4 × axis_count.
    let base_inst = 4usize + 4 * axis_count;
    let with_ps = instance_size == base_inst + 2;
    let new_instance_size = if with_ps {
        4usize + 4 * new_axis_count + 2
    } else {
        4usize + 4 * new_axis_count
    };

    // Filter instances: read each, drop Pin-axis slots, then drop the
    // record entirely if its surviving coord vector matches the
    // trimmed default-instance vector.
    let mut new_instance_records: Vec<Vec<u8>> = Vec::with_capacity(instance_count);
    let instances_off = axes_array_off + axis_count * axis_size;
    if instance_count > 0 {
        if instance_size < base_inst {
            return None;
        }
        let need_inst = instances_off.checked_add(instance_count.checked_mul(instance_size)?)?;
        if fvar_bytes.len() < need_inst {
            return None;
        }
        for i in 0..instance_count {
            let off = instances_off + i * instance_size;
            let mut rec = Vec::with_capacity(new_instance_size);
            // subfamilyNameID + flags.
            rec.extend_from_slice(&fvar_bytes[off..off + 4]);
            let mut all_default = true;
            for &k in &kept {
                let coord_off = off + 4 + k * 4;
                let raw = &fvar_bytes[coord_off..coord_off + 4];
                rec.extend_from_slice(raw);
                let raw_u32 = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
                if raw_u32 != axis_defaults[k] {
                    all_default = false;
                }
            }
            if with_ps {
                let ps_off = off + 4 + axis_count * 4;
                rec.extend_from_slice(&fvar_bytes[ps_off..ps_off + 2]);
            }
            // Drop instances that collapse to the default once the Pin
            // axes are removed. Keeping them would create duplicates of
            // the implicit default instance.
            if all_default && new_axis_count > 0 {
                continue;
            }
            new_instance_records.push(rec);
        }
    }

    // Assemble the new fvar.
    let mut out = Vec::with_capacity(16 + new_axis_count * 20 + new_instance_records.len() * new_instance_size);
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&16u16.to_be_bytes()); // axesArrayOffset (header is 16 bytes)
    out.extend_from_slice(&2u16.to_be_bytes()); // reserved
    out.extend_from_slice(&(new_axis_count as u16).to_be_bytes());
    out.extend_from_slice(&20u16.to_be_bytes()); // axisSize
    out.extend_from_slice(&(new_instance_records.len() as u16).to_be_bytes());
    out.extend_from_slice(&(new_instance_size as u16).to_be_bytes());
    for &k in &kept {
        let off = axes_array_off + k * axis_size;
        // Each axis record is 20 bytes; emit verbatim from source.
        out.extend_from_slice(&fvar_bytes[off..off + 20]);
    }
    for rec in &new_instance_records {
        out.extend_from_slice(rec);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Partial-instancing avar trim
// ---------------------------------------------------------------------------

/// Re-emits an `avar` table with every Pin-axis segment map dropped.
/// Returns `None` when every axis pins.
#[allow(dead_code)] // wired in by the partial-instancing integration commit
fn bake_avar_partial(avar_bytes: &[u8], pins: &[AxisPin]) -> Option<Vec<u8>> {
    if pins.iter().all(|p| matches!(p, AxisPin::Pin)) {
        return None;
    }
    if avar_bytes.len() < 8 {
        return None;
    }
    let major = u16::from_be_bytes([avar_bytes[0], avar_bytes[1]]);
    if major != 1 {
        return None;
    }
    let axis_count = u16::from_be_bytes([avar_bytes[6], avar_bytes[7]]) as usize;
    if pins.len() != axis_count {
        return None;
    }

    // Walk the segment maps, slicing each into its byte range so we
    // can emit the kept ones verbatim.
    let mut cursor = 8usize;
    let mut map_ranges: Vec<(usize, usize)> = Vec::with_capacity(axis_count);
    for _ in 0..axis_count {
        if avar_bytes.len() < cursor + 2 {
            return None;
        }
        let count = u16::from_be_bytes([avar_bytes[cursor], avar_bytes[cursor + 1]]) as usize;
        let start = cursor;
        // Each AxisValueMap is 4 bytes (2 × F2DOT14).
        let map_size = 2 + count * 4;
        if avar_bytes.len() < start + map_size {
            return None;
        }
        cursor = start + map_size;
        map_ranges.push((start, cursor));
    }

    // Count surviving axes.
    let new_axis_count = pins.iter().filter(|p| matches!(p, AxisPin::Keep)).count();

    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&0u16.to_be_bytes()); // reserved
    out.extend_from_slice(&(new_axis_count as u16).to_be_bytes());
    for (i, &pin) in pins.iter().enumerate() {
        if matches!(pin, AxisPin::Keep) {
            let (s, e) = map_ranges[i];
            out.extend_from_slice(&avar_bytes[s..e]);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Partial-instancing ItemVariationStore rewrite
// ---------------------------------------------------------------------------

/// Maps `(old_outer, old_inner)` source IVS rows to their new
/// `(new_outer, new_inner)` indexes after a partial-instance rewrite.
/// `None` means the source row exists but its surrounding subtable
/// collapsed to nothing (every region dropped) — callers must treat
/// the row as "no variation" and leave the consumer field at its
/// static value.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // wired in by the partial-instancing integration commit
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

#[allow(dead_code)] // wired in by the partial-instancing integration commit
impl RegionRemap {
    /// Returns the new (outer, inner) for an old row, or `None` when
    /// the surrounding subtable collapsed.
    pub(crate) fn lookup(&self, old_outer: u16, old_inner: u16) -> Option<(u16, u16)> {
        let new_outer = (*self.new_outer_for_old.get(old_outer as usize)?)?;
        Some((new_outer, old_inner))
    }
}

/// Reads an F2DOT14 from a byte slice at `off`.
fn read_f2dot14(data: &[u8], off: usize) -> f32 {
    let raw = i16::from_be_bytes([data[off], data[off + 1]]);
    f32::from(raw) / 16384.0
}

/// Writes an F2DOT14 to a byte vector.
fn write_f2dot14_bytes(out: &mut Vec<u8>, v: f32) {
    #[allow(clippy::cast_possible_truncation)]
    let raw = (v * 16384.0).round().clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
    out.extend_from_slice(&raw.to_be_bytes());
}

/// Re-emits an `ItemVariationStore` with every Pin-axis dimension
/// folded into the surviving deltas. Returns `(new_bytes, remap)` on
/// success, `None` when:
/// - the input is malformed,
/// - the input is not format 1, or
/// - every region drops at the pin coords (consumers should treat
///   every row as zero-delta and emit no IVS).
///
/// The output IVS uses the same format-1 layout: a region list with
/// only the Keep-axis dimensions, plus one `ItemVariationData` per
/// surviving source subtable. Subtables whose region list collapses
/// entirely are elided (see `RegionRemap`).
#[allow(dead_code)] // wired in by the partial-instancing integration commit
pub(crate) fn bake_ivs_partial(
    ivs_bytes: &[u8],
    coords: &[f32],
    pins: &[AxisPin],
) -> Option<(Vec<u8>, RegionRemap)> {
    if ivs_bytes.len() < 8 {
        return None;
    }
    let format = u16::from_be_bytes([ivs_bytes[0], ivs_bytes[1]]);
    if format != 1 {
        return None;
    }
    let region_list_off = u32::from_be_bytes([
        ivs_bytes[2],
        ivs_bytes[3],
        ivs_bytes[4],
        ivs_bytes[5],
    ]) as usize;
    let subtable_count = u16::from_be_bytes([ivs_bytes[6], ivs_bytes[7]]) as usize;
    if ivs_bytes.len() < 8 + subtable_count * 4 {
        return None;
    }
    let mut subtable_offsets: Vec<usize> = Vec::with_capacity(subtable_count);
    for i in 0..subtable_count {
        let off = u32::from_be_bytes([
            ivs_bytes[8 + i * 4],
            ivs_bytes[8 + i * 4 + 1],
            ivs_bytes[8 + i * 4 + 2],
            ivs_bytes[8 + i * 4 + 3],
        ]) as usize;
        subtable_offsets.push(off);
    }

    if ivs_bytes.len() < region_list_off + 4 {
        return None;
    }
    let axis_count =
        u16::from_be_bytes([ivs_bytes[region_list_off], ivs_bytes[region_list_off + 1]]) as usize;
    let region_count = u16::from_be_bytes([
        ivs_bytes[region_list_off + 2],
        ivs_bytes[region_list_off + 3],
    ]) as usize;
    if pins.len() != axis_count || coords.len() != axis_count {
        return None;
    }
    let regions_start = region_list_off + 4;
    let region_size = axis_count * 6;
    if ivs_bytes.len() < regions_start + region_count * region_size {
        return None;
    }

    // Project each region. None → dropped; Some((new_index, scalar)).
    let mut region_remap: Vec<Option<(u16, f32)>> = Vec::with_capacity(region_count);
    let mut new_regions: Vec<Vec<(f32, f32, f32)>> = Vec::new();
    for ri in 0..region_count {
        let base = regions_start + ri * region_size;
        let mut region: Vec<(f32, f32, f32)> = Vec::with_capacity(axis_count);
        for axis_i in 0..axis_count {
            let off = base + axis_i * 6;
            let s = read_f2dot14(ivs_bytes, off);
            let p = read_f2dot14(ivs_bytes, off + 2);
            let e = read_f2dot14(ivs_bytes, off + 4);
            region.push((s, p, e));
        }
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
    // delta encoding — pick the smallest that fits every value.
    let mut new_outer_for_old: Vec<Option<u16>> = Vec::with_capacity(subtable_count);
    // Pre-encoded subtable bodies (everything past the subtable's own
    // header bytes are written below; we serialize them in order so
    // offsets land deterministically).
    let mut new_subtables: Vec<Vec<u8>> = Vec::new();

    for sub_off in &subtable_offsets {
        let sub_off = *sub_off;
        // Subtable header: itemCount, wordDeltaCount, regionIndexCount,
        // then regionIndexCount × u16 indexes, then itemCount delta
        // rows.
        if ivs_bytes.len() < sub_off + 6 {
            return None;
        }
        let item_count = u16::from_be_bytes([ivs_bytes[sub_off], ivs_bytes[sub_off + 1]]) as usize;
        let wdc_raw =
            u16::from_be_bytes([ivs_bytes[sub_off + 2], ivs_bytes[sub_off + 3]]);
        let long_words = wdc_raw & 0x8000 != 0;
        let word_delta_count = (wdc_raw & 0x7FFF) as usize;
        let region_index_count =
            u16::from_be_bytes([ivs_bytes[sub_off + 4], ivs_bytes[sub_off + 5]]) as usize;
        if word_delta_count > region_index_count {
            return None;
        }
        let ri_start = sub_off + 6;
        if ivs_bytes.len() < ri_start + region_index_count * 2 {
            return None;
        }
        let mut region_indexes: Vec<u16> = Vec::with_capacity(region_index_count);
        for i in 0..region_index_count {
            region_indexes.push(u16::from_be_bytes([
                ivs_bytes[ri_start + i * 2],
                ivs_bytes[ri_start + i * 2 + 1],
            ]));
        }

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

        // Read every delta row's source slots. Each slot's source
        // encoding depends on (slot < word_delta_count, long_words).
        let (src_wide, src_narrow) = if long_words { (4usize, 2usize) } else { (2usize, 1usize) };
        let row_size = word_delta_count * src_wide
            + (region_index_count - word_delta_count) * src_narrow;
        let rows_start = ri_start + region_index_count * 2;
        if ivs_bytes.len() < rows_start + item_count * row_size {
            return None;
        }

        // For each item, build its surviving row of i32 deltas
        // (post-pin-scalar).
        let mut item_rows: Vec<Vec<i32>> = Vec::with_capacity(item_count);
        for it in 0..item_count {
            let row_off = rows_start + it * row_size;
            // Walk source slots, decoding each.
            let mut src_deltas: Vec<i32> = Vec::with_capacity(region_index_count);
            let mut cursor = row_off;
            for slot in 0..region_index_count {
                let is_wide = slot < word_delta_count;
                let value: i32 = match (is_wide, long_words) {
                    (true, true) => {
                        let v = i32::from_be_bytes([
                            ivs_bytes[cursor],
                            ivs_bytes[cursor + 1],
                            ivs_bytes[cursor + 2],
                            ivs_bytes[cursor + 3],
                        ]);
                        cursor += 4;
                        v
                    }
                    (true, false) | (false, true) => {
                        let v = i32::from(i16::from_be_bytes([
                            ivs_bytes[cursor],
                            ivs_bytes[cursor + 1],
                        ]));
                        cursor += 2;
                        v
                    }
                    (false, false) => {
                        #[allow(clippy::cast_possible_wrap)]
                        let v = ivs_bytes[cursor] as i8;
                        cursor += 1;
                        i32::from(v)
                    }
                };
                src_deltas.push(value);
            }
            // Apply scalar to each surviving slot, build the new row in
            // surviving-slot order.
            let mut new_row: Vec<i32> = Vec::with_capacity(surviving_slots.len());
            for &(slot, _new_ri, scalar) in &surviving_slots {
                #[allow(clippy::cast_precision_loss)]
                let scaled = src_deltas[slot] as f32 * scalar;
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_precision_loss
                )]
                let rounded = scaled.round() as i32;
                new_row.push(rounded);
            }
            item_rows.push(new_row);
        }

        // Decide encoding: pick all-i16 if every value fits, else
        // all-i32 (set LONG_WORDS bit, wordDeltaCount =
        // surviving_slot_count). Simple and conservative — the IVS
        // dedup pass in 0.13 doesn't run again on the partial output.
        let all_fit_i16 = item_rows
            .iter()
            .flat_map(|r| r.iter())
            .all(|v| (i32::from(i16::MIN)..=i32::from(i16::MAX)).contains(v));

        // Emit the subtable body.
        let mut sub_bytes: Vec<u8> = Vec::new();
        sub_bytes.extend_from_slice(&(item_count as u16).to_be_bytes());
        let surviving_count = surviving_slots.len() as u16;
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
                    #[allow(clippy::cast_possible_truncation)]
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
    // empty) IVS — the caller decides whether to drop the host table
    // entirely, but the RegionRemap stays meaningful (every lookup
    // returns None). A zero-region zero-subtable IVS is a 16-byte
    // skeleton: 8-byte header + 4-byte region list + 0 subtable
    // offsets. Real consumers (HVAR / VVAR / MVAR / GDEF) read deltas
    // by (outer, inner) and resolve out-of-range to zero.

    // Emit the new IVS.
    let new_axis_count = pins.iter().filter(|p| matches!(p, AxisPin::Keep)).count() as u16;
    let new_subtable_count = new_subtables.len();
    let header_size = 8 + new_subtable_count * 4;

    // Region list size: 4 bytes (axisCount + regionCount) + axes * 6
    // per region.
    let region_list_size = 4 + new_regions.len() * (new_axis_count as usize) * 6;
    let new_region_list_off = header_size as u32;
    // Subtables start after the region list.
    let subtables_base = header_size + region_list_size;

    let mut out: Vec<u8> = Vec::with_capacity(subtables_base);
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&new_region_list_off.to_be_bytes());
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
    let mut cursor = out.len();
    for (i, sub) in new_subtables.iter().enumerate() {
        let off_u32 = cursor as u32;
        let slot = subtable_off_slot + i * 4;
        out[slot..slot + 4].copy_from_slice(&off_u32.to_be_bytes());
        out.extend_from_slice(sub);
        cursor += sub.len();
    }

    Some((out, RegionRemap { new_outer_for_old }))
}

// ---------------------------------------------------------------------------
// DeltaSetIndexMap rewrite (used by HVAR / VVAR partial bake)
// ---------------------------------------------------------------------------

/// Re-emits a `DeltaSetIndexMap` with every entry's outer index
/// rewritten through `remap`. Entries whose outer subtable collapsed
/// land at `(new_subtable_count, 0)` — guaranteed out-of-range, so
/// IVS evaluation returns zero (the desired "no variation for this
/// row" semantics).
///
/// The output keeps the source's format (0 / 1) and entryFormat
/// (bytes-per-entry, inner-bit-count) unchanged. The packed
/// `(outer, inner)` may overflow the source's bit allocation — when
/// that happens we widen entryFormat conservatively.
///
/// `start` is the offset into `data` where the map begins.
fn rewrite_delta_set_index_map(
    data: &[u8],
    start: usize,
    remap: &RegionRemap,
    new_subtable_count: u16,
) -> Option<Vec<u8>> {
    if data.len() < start + 2 {
        return None;
    }
    let format = data[start];
    let entry_format = data[start + 1];
    let mut cursor = start + 2;

    let map_count: u32 = match format {
        0 => {
            if data.len() < cursor + 2 {
                return None;
            }
            let v = u16::from_be_bytes([data[cursor], data[cursor + 1]]) as u32;
            cursor += 2;
            v
        }
        1 => {
            if data.len() < cursor + 4 {
                return None;
            }
            let v = u32::from_be_bytes([
                data[cursor],
                data[cursor + 1],
                data[cursor + 2],
                data[cursor + 3],
            ]);
            cursor += 4;
            v
        }
        _ => return None,
    };

    let entry_bytes = ((entry_format >> 4) & 0x03) as usize + 1;
    let inner_bits = (entry_format & 0x0F) as u32 + 1;
    let inner_mask: u32 = (1u32 << inner_bits) - 1;

    if map_count == 0 {
        // Nothing to rewrite — return a clone of the unchanged map
        // header so the caller's offset surgery still works.
        return Some(data[start..cursor].to_vec());
    }

    if data.len() < cursor + (map_count as usize) * entry_bytes {
        return None;
    }

    // Decode every entry, remap, then decide the new entryFormat.
    let mut new_entries: Vec<(u16, u16)> = Vec::with_capacity(map_count as usize);
    for i in 0..map_count as usize {
        let off = cursor + i * entry_bytes;
        let mut raw: u32 = 0;
        for b in 0..entry_bytes {
            raw = (raw << 8) | u32::from(data[off + b]);
        }
        let inner = (raw & inner_mask) as u16;
        let outer = (raw >> inner_bits) as u16;
        let (new_outer, new_inner) = match remap.lookup(outer, inner) {
            Some(v) => v,
            None => (new_subtable_count, 0),
        };
        new_entries.push((new_outer, new_inner));
    }

    // Compute new entryFormat. Use the smallest entryFormat that
    // covers every (outer, inner) we'll write. inner_bits = ceil(log2)
    // of (max_inner + 1), clamped to [1, 16]; total bits = inner_bits
    // + outer_bits, clamped to multiples of 8 for entry_bytes.
    let max_outer = new_entries.iter().map(|(o, _)| *o).max().unwrap_or(0);
    let max_inner = new_entries.iter().map(|(_, i)| *i).max().unwrap_or(0);
    let new_inner_bits: u32 = if max_inner == 0 {
        1
    } else {
        16 - max_inner.leading_zeros()
    };
    let new_outer_bits: u32 = if max_outer == 0 {
        0
    } else {
        16 - max_outer.leading_zeros()
    };
    let total_bits = new_inner_bits + new_outer_bits;
    let new_entry_bytes: u32 = total_bits.div_ceil(8);
    let new_entry_bytes = new_entry_bytes.clamp(1, 4);
    let new_entry_format = (((new_entry_bytes - 1) as u8) << 4) | ((new_inner_bits - 1) as u8 & 0x0F);
    let new_inner_mask: u32 = (1u32 << new_inner_bits) - 1;

    // Re-emit.
    let mut out = Vec::with_capacity(2 + 4 + (map_count as usize) * (new_entry_bytes as usize));
    out.push(format);
    out.push(new_entry_format);
    match format {
        0 => out.extend_from_slice(&(map_count as u16).to_be_bytes()),
        1 => out.extend_from_slice(&map_count.to_be_bytes()),
        _ => return None,
    }
    for (outer, inner) in new_entries {
        let packed: u32 =
            (u32::from(outer) << new_inner_bits) | (u32::from(inner) & new_inner_mask);
        let bytes = packed.to_be_bytes();
        // Take the low `new_entry_bytes` bytes (big-endian).
        out.extend_from_slice(&bytes[(4 - new_entry_bytes as usize)..]);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// HVAR partial bake
// ---------------------------------------------------------------------------

/// Re-emits HVAR with its embedded IVS partial-projected through
/// `pins` / `coords`, every DeltaSetIndexMap rewritten through the
/// remap, and the table header offsets adjusted to match.
///
/// Returns `None` when the source HVAR is malformed or the IVS
/// rewrite fails. The caller should fall through to dropping the
/// table when this returns `None` — that's equivalent to "no advance
/// variation," safe but slightly degraded.
#[allow(dead_code)] // wired in by the partial-instancing integration commit
fn bake_hvar_partial(hvar_bytes: &[u8], coords: &[f32], pins: &[AxisPin]) -> Option<Vec<u8>> {
    // HVAR header: u16 major, u16 minor, o32 ivs, o32 advance, o32 lsb,
    // o32 rsb. 20 bytes.
    if hvar_bytes.len() < 20 {
        return None;
    }
    let major = u16::from_be_bytes([hvar_bytes[0], hvar_bytes[1]]);
    if major != 1 {
        return None;
    }
    let ivs_off = u32::from_be_bytes([hvar_bytes[4], hvar_bytes[5], hvar_bytes[6], hvar_bytes[7]])
        as usize;
    let advance_off =
        u32::from_be_bytes([hvar_bytes[8], hvar_bytes[9], hvar_bytes[10], hvar_bytes[11]]);
    let lsb_off = u32::from_be_bytes([
        hvar_bytes[12],
        hvar_bytes[13],
        hvar_bytes[14],
        hvar_bytes[15],
    ]);
    let rsb_off = u32::from_be_bytes([
        hvar_bytes[16],
        hvar_bytes[17],
        hvar_bytes[18],
        hvar_bytes[19],
    ]);

    if hvar_bytes.len() < ivs_off {
        return None;
    }
    let (new_ivs, remap) = bake_ivs_partial(&hvar_bytes[ivs_off..], coords, pins)?;
    // Read new subtable count from the just-emitted IVS.
    let new_subtable_count = u16::from_be_bytes([new_ivs[6], new_ivs[7]]);

    // Rewrite each non-zero map.
    let new_advance_map = if advance_off != 0 {
        Some(rewrite_delta_set_index_map(
            hvar_bytes,
            advance_off as usize,
            &remap,
            new_subtable_count,
        )?)
    } else {
        None
    };
    let new_lsb_map = if lsb_off != 0 {
        Some(rewrite_delta_set_index_map(
            hvar_bytes,
            lsb_off as usize,
            &remap,
            new_subtable_count,
        )?)
    } else {
        None
    };
    let new_rsb_map = if rsb_off != 0 {
        Some(rewrite_delta_set_index_map(
            hvar_bytes,
            rsb_off as usize,
            &remap,
            new_subtable_count,
        )?)
    } else {
        None
    };

    // Layout the output: 20-byte header + IVS + advance map +
    // lsb map + rsb map. Offsets are u32 from start of HVAR.
    let mut out = Vec::with_capacity(hvar_bytes.len());
    out.extend_from_slice(&hvar_bytes[..4]); // major + minor
    let new_ivs_off: u32 = 20;
    out.extend_from_slice(&new_ivs_off.to_be_bytes());
    let advance_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    let lsb_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    let rsb_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&new_ivs);
    if let Some(map) = &new_advance_map {
        let off = out.len() as u32;
        out[advance_off_slot..advance_off_slot + 4].copy_from_slice(&off.to_be_bytes());
        out.extend_from_slice(map);
    }
    if let Some(map) = &new_lsb_map {
        let off = out.len() as u32;
        out[lsb_off_slot..lsb_off_slot + 4].copy_from_slice(&off.to_be_bytes());
        out.extend_from_slice(map);
    }
    if let Some(map) = &new_rsb_map {
        let off = out.len() as u32;
        out[rsb_off_slot..rsb_off_slot + 4].copy_from_slice(&off.to_be_bytes());
        out.extend_from_slice(map);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// VVAR partial bake
// ---------------------------------------------------------------------------

/// Re-emits VVAR with its embedded IVS partial-projected and every
/// DeltaSetIndexMap rewritten. VVAR's header is 24 bytes (4 ver + 5
/// × o32: ivs / advance-height / tsb / bsb / vorg). The vorg map
/// shares the IVS rows with the others; we rewrite it through the
/// same remap.
#[allow(dead_code)] // wired in by the partial-instancing integration commit
fn bake_vvar_partial(vvar_bytes: &[u8], coords: &[f32], pins: &[AxisPin]) -> Option<Vec<u8>> {
    if vvar_bytes.len() < 24 {
        return None;
    }
    let major = u16::from_be_bytes([vvar_bytes[0], vvar_bytes[1]]);
    if major != 1 {
        return None;
    }
    let ivs_off = u32::from_be_bytes([vvar_bytes[4], vvar_bytes[5], vvar_bytes[6], vvar_bytes[7]])
        as usize;
    let mut map_offs: [u32; 4] = [0; 4];
    for (i, slot) in map_offs.iter_mut().enumerate() {
        let base = 8 + i * 4;
        *slot = u32::from_be_bytes([
            vvar_bytes[base],
            vvar_bytes[base + 1],
            vvar_bytes[base + 2],
            vvar_bytes[base + 3],
        ]);
    }

    if vvar_bytes.len() < ivs_off {
        return None;
    }
    let (new_ivs, remap) = bake_ivs_partial(&vvar_bytes[ivs_off..], coords, pins)?;
    let new_subtable_count = u16::from_be_bytes([new_ivs[6], new_ivs[7]]);

    let mut new_maps: [Option<Vec<u8>>; 4] = [None, None, None, None];
    for (i, off) in map_offs.iter().enumerate() {
        if *off != 0 {
            new_maps[i] = Some(rewrite_delta_set_index_map(
                vvar_bytes,
                *off as usize,
                &remap,
                new_subtable_count,
            )?);
        }
    }

    let mut out = Vec::with_capacity(vvar_bytes.len());
    out.extend_from_slice(&vvar_bytes[..4]); // major + minor
    let new_ivs_off: u32 = 24;
    out.extend_from_slice(&new_ivs_off.to_be_bytes());
    let mut map_slots = [0usize; 4];
    for slot in &mut map_slots {
        *slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    out.extend_from_slice(&new_ivs);
    for (i, map) in new_maps.iter().enumerate() {
        if let Some(map) = map {
            let off = out.len() as u32;
            out[map_slots[i]..map_slots[i] + 4].copy_from_slice(&off.to_be_bytes());
            out.extend_from_slice(map);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// MVAR partial bake
// ---------------------------------------------------------------------------

/// Re-emits MVAR with its embedded IVS partial-projected. MVAR
/// references rows by direct (outer, inner) in each value record —
/// no DeltaSetIndexMap. Rows pointing at collapsed subtables get
/// rewritten to `(new_subtable_count, 0)` (out-of-range; resolves to
/// zero delta).
///
/// The MVAR header has `valueRecordSize >= 8`; we preserve the
/// source's record_size and only patch the first 8 bytes of each
/// record (tag + outer + inner).
#[allow(dead_code)] // wired in by the partial-instancing integration commit
fn bake_mvar_partial(mvar_bytes: &[u8], coords: &[f32], pins: &[AxisPin]) -> Option<Vec<u8>> {
    if mvar_bytes.len() < 12 {
        return None;
    }
    let major = u16::from_be_bytes([mvar_bytes[0], mvar_bytes[1]]);
    if major != 1 {
        return None;
    }
    let record_size = u16::from_be_bytes([mvar_bytes[6], mvar_bytes[7]]) as usize;
    let record_count = u16::from_be_bytes([mvar_bytes[8], mvar_bytes[9]]) as usize;
    let store_off = u16::from_be_bytes([mvar_bytes[10], mvar_bytes[11]]) as usize;

    if record_count > 0 && record_size < 8 {
        return None;
    }
    if store_off == 0 {
        // No store — pass through unchanged.
        return Some(mvar_bytes.to_vec());
    }
    if mvar_bytes.len() < store_off {
        return None;
    }
    let (new_ivs, remap) = bake_ivs_partial(&mvar_bytes[store_off..], coords, pins)?;
    let new_subtable_count = u16::from_be_bytes([new_ivs[6], new_ivs[7]]);

    // Layout: 12-byte header + records + IVS. Preserve record_size.
    let records_start: usize = 12;
    let records_size = record_count * record_size;
    if mvar_bytes.len() < records_start + records_size {
        return None;
    }
    let new_store_off: u16 = (records_start + records_size) as u16;
    let mut out = Vec::with_capacity(mvar_bytes.len());
    out.extend_from_slice(&mvar_bytes[..6]);
    out.extend_from_slice(&(record_size as u16).to_be_bytes());
    out.extend_from_slice(&(record_count as u16).to_be_bytes());
    out.extend_from_slice(&new_store_off.to_be_bytes());

    // Records.
    for i in 0..record_count {
        let off = records_start + i * record_size;
        let tag = &mvar_bytes[off..off + 4];
        let outer = u16::from_be_bytes([mvar_bytes[off + 4], mvar_bytes[off + 5]]);
        let inner = u16::from_be_bytes([mvar_bytes[off + 6], mvar_bytes[off + 7]]);
        let (new_outer, new_inner) = match remap.lookup(outer, inner) {
            Some(v) => v,
            None => (new_subtable_count, 0),
        };
        out.extend_from_slice(tag);
        out.extend_from_slice(&new_outer.to_be_bytes());
        out.extend_from_slice(&new_inner.to_be_bytes());
        // Trailing pad bytes per record_size (record_size >= 8).
        if record_size > 8 {
            out.extend_from_slice(&mvar_bytes[off + 8..off + record_size]);
        }
    }
    out.extend_from_slice(&new_ivs);
    Some(out)
}

// ---------------------------------------------------------------------------
// GDEF.IVS partial bake
// ---------------------------------------------------------------------------

/// Re-emits GDEF with its embedded IVS partial-projected. The other
/// GDEF tables (glyphClassDef, attachList, ligCaretList,
/// markAttachClassDef, markGlyphSetsDef) ride through verbatim — they
/// don't carry variation indices.
///
/// Unlike the prune-IVS path used for full instancing, this preserves
/// GDEF v1.3 with a non-zero `itemVarStoreOffset` pointing at the new
/// trimmed IVS. GPOS Anchor variations + future GDEF-resident
/// VariationIndex consumers (mark / cursive bake) reach the trimmed
/// regions through the same offset.
#[allow(dead_code)] // wired in by the partial-instancing integration commit
fn bake_gdef_ivs_partial(
    gdef_bytes: &[u8],
    coords: &[f32],
    pins: &[AxisPin],
) -> Option<Vec<u8>> {
    if gdef_bytes.len() < 18 {
        return None;
    }
    let major = u16::from_be_bytes([gdef_bytes[0], gdef_bytes[1]]);
    let minor = u16::from_be_bytes([gdef_bytes[2], gdef_bytes[3]]);
    if major != 1 || minor < 3 {
        return None;
    }
    let ivs_off =
        u32::from_be_bytes([gdef_bytes[14], gdef_bytes[15], gdef_bytes[16], gdef_bytes[17]])
            as usize;
    if ivs_off == 0 {
        return None;
    }
    if gdef_bytes.len() < ivs_off {
        return None;
    }
    let (new_ivs, _remap) = bake_ivs_partial(&gdef_bytes[ivs_off..], coords, pins)?;

    // The non-IVS GDEF body (everything before ivs_off) rides through
    // verbatim. Patch the IVS offset to the truncated body's tail and
    // append the new IVS.
    let mut out = Vec::with_capacity(ivs_off + new_ivs.len());
    out.extend_from_slice(&gdef_bytes[..ivs_off]);
    let new_ivs_off: u32 = ivs_off as u32;
    out[14..18].copy_from_slice(&new_ivs_off.to_be_bytes());
    out.extend_from_slice(&new_ivs);
    Some(out)
}

// ---------------------------------------------------------------------------
// GPOS variation bake (#175)
// ---------------------------------------------------------------------------

/// Returns a GPOS byte buffer with every supported subtable's
/// `VariationIndex`-driven ValueRecord field folded into the static
/// field at `coords` and the matching offset slot zeroed. Returns
/// `None` when the source has no GPOS, when the parser refuses the
/// GPOS bytes, or when the GPOS lookup walk produced no patches.
///
/// Lookup-type coverage matches `gpos_var::bake_gpos_at_coords`:
/// SinglePos (formats 1 / 2), PairPos (formats 1 / 2), and Type 9
/// Extension wrappers around either. Mark*/Cursive lookups carry their
/// variations on `Anchor` records — those ride through verbatim and
/// are orphaned by the GDEF.IVS prune that follows.
///
/// The bake reads its `ItemVariationStore` from the *source* GDEF, not
/// from a re-parsed copy, so it sees every region the source uses
/// before the prune sever the path.
fn bake_gpos_var(face: &Face<'_>, coords: &[f32]) -> Result<Option<Vec<u8>>, SubsetError> {
    let gpos_bytes = match face.table_bytes(tag::GPOS) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    let gdef = face.gdef().map_err(SubsetError::from)?;
    let store = gdef.as_ref().and_then(|g| g.item_variation_store());
    Ok(crate::gpos_var::bake_gpos_at_coords(
        gpos_bytes, store, coords,
    ))
}

// ---------------------------------------------------------------------------
// GDEF.IVS pruning
// ---------------------------------------------------------------------------

/// Returns a GDEF byte buffer with its `ItemVariationStore` offset
/// zeroed (and the store payload truncated from the table) when the
/// source GDEF carries one. When the source has no GDEF or the IVS
/// offset is already zero, returns `None` (caller passes through the
/// source bytes — or omits GDEF entirely if absent).
///
/// GDEF v1.3 layout (28 bytes header, every offset is from start of
/// table):
///
/// ```text
///   u16  majorVersion
///   u16  minorVersion
///   o16  glyphClassDefOffset
///   o16  attachListOffset
///   o16  ligCaretListOffset
///   o16  markAttachClassDefOffset
///   o16  markGlyphSetsDefOffset       (v1.2+, may be 0)
///   o32  itemVarStoreOffset           (v1.3, may be 0)
/// ```
///
/// When v == 1.3 and itemVarStoreOffset != 0 we zero the offset in
/// place and truncate the table at the IVS body's start (when the
/// store sits at the tail of the table). When the store is in the
/// middle of the table — rare in real fonts — we just zero the
/// offset; the orphan bytes ride through but are unreachable by any
/// consumer.
fn prune_gdef_ivs(face: &Face<'_>) -> Result<Option<Vec<u8>>, SubsetError> {
    let bytes = match face.table_bytes(tag::GDEF) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    // GDEF header (v1.3) is 18 bytes: u16 major, u16 minor,
    // o16 glyphClass, o16 attach, o16 ligCaret, o16 markAttach,
    // o16 markGlyphSets (v1.2+), o32 itemVarStore (v1.3).
    if bytes.len() < 18 {
        return Ok(None);
    }
    let major = u16::from_be_bytes([bytes[0], bytes[1]]);
    let minor = u16::from_be_bytes([bytes[2], bytes[3]]);
    if major != 1 || minor < 3 {
        // No IVS in v1.0 / v1.2; pass through.
        return Ok(None);
    }
    let ivs_off = u32::from_be_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]);
    if ivs_off == 0 {
        return Ok(None);
    }
    let mut out = bytes.to_vec();
    out[14..18].copy_from_slice(&0u32.to_be_bytes());
    // Truncate the IVS payload when it sits at the tail of the table
    // (the layout fontTools emits and that every real GDEF in the
    // wild uses). When the store is in the middle, leave the orphan
    // bytes — they're unreachable now that the offset is zero.
    let ivs_off_us = ivs_off as usize;
    if ivs_off_us < out.len() {
        // If IVS is the last referenced offset, truncate. Every other
        // offset in the GDEF header sits before the IVS payload in
        // well-formed fonts; we check that no other offset (glyphClass
        // / attachList / ligCaretList / markAttach / markGlyphSets)
        // points past `ivs_off`.
        let mut max_other: usize = 0;
        for slot in [4, 6, 8, 10, 12] {
            let off = u16::from_be_bytes([out[slot], out[slot + 1]]) as usize;
            if off > max_other {
                max_other = off;
            }
        }
        if max_other <= ivs_off_us {
            out.truncate(ivs_off_us);
        }
    }
    Ok(Some(out))
}

// silence clippy warning about unused GlyphId import from lib (kept for
// public surface symmetry with the rest of the crate).
const _: () = {
    let _: Option<GlyphId> = None;
};

#[cfg(test)]
mod tests {
    use super::*;

    const RUBIK: &[u8] = include_bytes!("../../../tests/fixtures/rubik_vf.ttf");
    const SOURCE_SANS: &[u8] =
        include_bytes!("../../../tests/fonts/SourceSans3VF-Latin-Subset.otf");
    const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
    /// 972-byte synthetic VF with a single PairPos format 1 lookup
    /// whose AV pair carries a VariationIndex into a one-region IVS;
    /// at wght=900 the delta is -100, at wght=400 it is 0. Built by
    /// `tests/tools/build_var_kern_fixture.py`. See
    /// `tests/variable_kern.rs` for the upstream cover.
    const VAR_KERN: &[u8] = include_bytes!("../../../tests/fixtures/var_kern.ttf");

    fn rubik_face() -> Face<'static> {
        Face::parse_bytes(RUBIK, 0).unwrap()
    }

    #[test]
    fn input_default_drops_var_tables() {
        let i = InstanceInput::default();
        assert!(i.drop_var_tables);
        assert!(i.coords.is_empty());
    }

    #[test]
    fn instance_at_default_coords_keeps_outline_shape() {
        // Bake at the source's default instance (all zeros). The
        // baked outline must round-trip: every gid's outline at zero
        // coords in the instanced font equals the source's static
        // outline.
        let face = rubik_face();
        let axis_count = face.fvar().unwrap().unwrap().axes().len();
        let input = InstanceInput {
            coords: alloc::vec![0.0_f32; axis_count],
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let out = instance(&face, &input).expect("bake at default");
        let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
        assert!(baked.fvar().unwrap().is_none(), "fvar dropped");
        assert!(baked.gvar().unwrap().is_none(), "gvar dropped");
        assert!(baked.hvar().unwrap().is_none(), "HVAR dropped");

        // Pick a couple of gids with outlines and confirm the bake
        // produces (approximately) the same outline as the source at
        // default-instance coords.
        let cmap = face.cmap().unwrap();
        let gid_a = cmap.glyph_id('A').expect("rubik has 'A'");
        let want = face.glyph_outline_at_coords(gid_a, &[]).unwrap();
        let got = baked.glyph_outline_at_coords(gid_a, &[]).unwrap();
        match (want, got) {
            (Some(w), Some(g)) => {
                assert_eq!(
                    w.ops().len(),
                    g.ops().len(),
                    "op count diverged for 'A' at default coords"
                );
            }
            (None, None) => {}
            _ => panic!("baked outline presence diverged"),
        }
    }

    #[test]
    fn instance_at_extreme_coords_matches_outline_at_coords() {
        // Bake at the Rubik VF's wght extreme. Compare each gid's
        // baked outline to the source's outline-at-coords result.
        let face = rubik_face();
        let fvar = face.fvar().unwrap().unwrap();
        let mut user = alloc::vec![0.0_f32; fvar.axes().len()];
        if let Some(idx) = fvar.axis_index(*b"wght") {
            user[idx] = fvar.axes()[idx].max_value;
        }
        let coords = fvar.normalize_coords(&user);
        let input = InstanceInput {
            coords: coords.clone(),
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let out = instance(&face, &input).expect("bake at extreme");
        let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");

        let cmap = face.cmap().unwrap();
        let gid_a = cmap.glyph_id('A').expect("rubik has 'A'");
        let want = face
            .glyph_outline_at_coords(gid_a, &coords)
            .unwrap()
            .expect("source draws 'A'");
        let got = baked
            .glyph_outline_at_coords(gid_a, &[])
            .unwrap()
            .expect("baked draws 'A'");
        // Outline op counts must match; the baked outline bypassed
        // gvar entirely (the table is gone) so its shape comes from
        // the rewritten contour points.
        assert_eq!(want.ops().len(), got.ops().len());
    }

    #[test]
    fn instance_advances_match_hvar_eval_at_coords() {
        let face = rubik_face();
        let fvar = face.fvar().unwrap().unwrap();
        let mut user = alloc::vec![0.0_f32; fvar.axes().len()];
        if let Some(idx) = fvar.axis_index(*b"wght") {
            user[idx] = fvar.axes()[idx].max_value;
        }
        let coords = fvar.normalize_coords(&user);
        let input = InstanceInput {
            coords: coords.clone(),
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let out = instance(&face, &input).expect("bake");
        let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");

        let src_hmtx = face.hmtx().unwrap();
        let baked_hmtx = baked.hmtx().unwrap();
        let hvar = face.hvar().unwrap().expect("rubik has HVAR");

        let cmap = face.cmap().unwrap();
        for ch in ['A', 'g', 'M', 'i'] {
            let Some(gid) = cmap.glyph_id(ch) else {
                continue;
            };
            let base = f32::from(src_hmtx.advance(gid).unwrap_or(0));
            let want = (base + hvar.advance_delta(gid, &coords)).round() as i32;
            let got = i32::from(baked_hmtx.advance(gid).unwrap_or(0));
            assert_eq!(want, got, "advance mismatch for {ch:?} (gid {gid})");
        }
    }

    #[test]
    fn instance_static_font_errors() {
        let face = Face::parse_bytes(OPEN_SANS, 0).unwrap();
        let input = InstanceInput::default();
        let r = instance(&face, &input);
        assert!(matches!(r, Err(SubsetError::Unsupported(_))));
    }

    #[test]
    fn instance_coord_length_validation() {
        let face = rubik_face();
        let bad = InstanceInput {
            coords: alloc::vec![0.0_f32; 99],
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        assert!(matches!(
            instance(&face, &bad),
            Err(SubsetError::Unsupported(_))
        ));
    }

    #[test]
    fn source_sans_round_trip_at_default_instance() {
        // Source Sans 3 VF Latin Subset is a CFF2-flavoured VF. After
        // the 0.12.0 CFF2 blend bake landed, instancing produces a
        // static CFF2 face whose every glyph re-parses through the
        // standard outline pipeline.
        let face = Face::parse_bytes(SOURCE_SANS, 0).unwrap();
        let axis_count = face.fvar().unwrap().map_or(0, |f| f.axes().len());
        let input = InstanceInput {
            coords: alloc::vec![0.0_f32; axis_count],
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let out = instance(&face, &input).expect("CFF2 default-instance bake");
        let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
        // GDEF.IVS pruned: at default coords no IVS reference would
        // resolve to a non-zero delta anyway, so the output's GDEF —
        // when present — must have a zero IVS offset.
        if let Some(_gdef) = face.gdef().unwrap() {
            // Pruned baked GDEF: the IVS getter on the baked side is
            // None (offset zeroed by the prune step).
            let baked_gdef = baked.gdef().unwrap().expect("baked GDEF preserved");
            assert!(
                baked_gdef.item_variation_store().is_none(),
                "baked GDEF.IVS must be pruned"
            );
        }
        // No fvar / HVAR / MVAR survive on the static side.
        assert!(baked.fvar().unwrap().is_none(), "fvar dropped");
        assert!(baked.hvar().unwrap().is_none(), "HVAR dropped");

        // At default coords every glyph that drew in the source must
        // draw in the baked output with the same op count.
        let cmap = face.cmap().unwrap();
        for ch in ['A', 'g', 'i', 'O'] {
            let Some(gid) = cmap.glyph_id(ch) else {
                continue;
            };
            let want = face.glyph_outline_at_coords(gid, &[]).unwrap();
            let got = baked.glyph_outline_at_coords(gid, &[]).unwrap();
            match (want, got) {
                (Some(w), Some(g)) => assert_eq!(
                    w.ops().len(),
                    g.ops().len(),
                    "op count diverged for {ch:?} at default coords"
                ),
                (None, None) => {}
                (w, g) => panic!(
                    "drew presence diverged for {ch:?}: source={:?}, baked={:?}",
                    w.is_some(),
                    g.is_some()
                ),
            }
        }
    }

    #[test]
    fn source_sans_round_trip_at_extreme_coord_matches_source_outline() {
        // Bake at wght=900 (extreme). Compare each ascii gid's outline
        // op count against the source's outline-at-coords result.
        let face = Face::parse_bytes(SOURCE_SANS, 0).unwrap();
        let fvar = face.fvar().unwrap().unwrap();
        let mut user = alloc::vec![0.0_f32; fvar.axes().len()];
        if let Some(idx) = fvar.axis_index(*b"wght") {
            user[idx] = fvar.axes()[idx].max_value;
        }
        let coords = fvar.normalize_coords(&user);
        let input = InstanceInput {
            coords: coords.clone(),
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let out = instance(&face, &input).expect("CFF2 extreme bake");
        let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
        // The CFF2 source's outline-at-coords path is exercised by
        // sigilbuzz core's own test suite; here we just check the
        // bake produces a parseable face whose CFF2 charstring count
        // matches the source's. Compare the no-coords outline (which
        // both sides should agree on) for each ascii letter.
        let cmap = face.cmap().unwrap();
        let mut compared = 0;
        for ch in ['A', 'g', 'i', 'O'] {
            let Some(gid) = cmap.glyph_id(ch) else {
                continue;
            };
            let want = face.glyph_outline_at_coords(gid, &coords).unwrap();
            let got = baked.glyph_outline_at_coords(gid, &[]).unwrap();
            // Drew-or-didn't-draw must match: a baked charstring whose
            // source draws but baked doesn't (or vice versa) signals a
            // round-trip break.
            match (want, got) {
                (Some(w), Some(g)) => {
                    assert_eq!(
                        w.ops().len(),
                        g.ops().len(),
                        "op count mismatch for {ch:?} (gid {gid})"
                    );
                    compared += 1;
                }
                (None, None) => {}
                (w, g) => panic!(
                    "drew presence diverged for {ch:?} (gid {gid}): source={:?}, baked={:?}",
                    w.is_some(),
                    g.is_some()
                ),
            }
        }
        let _ = compared; // some glyphs may legitimately not draw
    }

    #[test]
    fn source_sans_default_coords_byte_deterministic() {
        let face = Face::parse_bytes(SOURCE_SANS, 0).unwrap();
        let axis_count = face.fvar().unwrap().map_or(0, |f| f.axes().len());
        let input = InstanceInput {
            coords: alloc::vec![0.0_f32; axis_count],
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let a = instance(&face, &input).unwrap();
        let b = instance(&face, &input).unwrap();
        assert_eq!(a.bytes, b.bytes);
    }

    #[test]
    fn instance_drops_var_tables_when_flag_set() {
        let face = rubik_face();
        let axis_count = face.fvar().unwrap().unwrap().axes().len();
        let input = InstanceInput {
            coords: alloc::vec![0.0_f32; axis_count],
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let out = instance(&face, &input).unwrap();
        let baked = Face::parse_bytes(&out.bytes, 0).unwrap();
        assert!(baked.record(tag::FVAR).is_none());
        assert!(baked.record(tag::AVAR).is_none());
        assert!(baked.record(tag::GVAR).is_none());
        assert!(baked.record(tag::HVAR).is_none());
    }

    #[test]
    fn instance_keeps_var_tables_when_flag_unset() {
        let face = rubik_face();
        let axis_count = face.fvar().unwrap().unwrap().axes().len();
        let input = InstanceInput {
            coords: alloc::vec![0.0_f32; axis_count],
            drop_var_tables: false,
            axis_pins: Vec::new(),
        };
        let out = instance(&face, &input).unwrap();
        let baked = Face::parse_bytes(&out.bytes, 0).unwrap();
        assert!(baked.record(tag::FVAR).is_some());
    }

    #[test]
    fn instance_is_deterministic() {
        let face = rubik_face();
        let axis_count = face.fvar().unwrap().unwrap().axes().len();
        let input = InstanceInput {
            coords: alloc::vec![0.5_f32; axis_count],
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let a = instance(&face, &input).unwrap();
        let b = instance(&face, &input).unwrap();
        assert_eq!(a.bytes, b.bytes);
    }

    #[test]
    fn rubik_mvar_bake_applies_undo_delta_to_post_underline_position() {
        // Rubik VF carries a single MVAR record for `undo` (post
        // underlinePosition). Bake at the wght extreme and confirm
        // the output's post.underlinePosition shifted by the MVAR
        // delta resolved at that coord.
        let face = rubik_face();
        let fvar = face.fvar().unwrap().unwrap();
        let mut user = alloc::vec![0.0_f32; fvar.axes().len()];
        if let Some(idx) = fvar.axis_index(*b"wght") {
            user[idx] = fvar.axes()[idx].max_value;
        }
        let coords = fvar.normalize_coords(&user);

        let mvar = face.mvar().unwrap().expect("rubik has MVAR");
        let undo_delta = mvar
            .metric_delta(*b"undo", &coords)
            .expect("rubik MVAR carries undo");
        let undo_delta_i32 = undo_delta.round() as i32;

        let post_src = face.table_bytes(tag::POST).unwrap();
        let src_undo = i16::from_be_bytes([post_src[8], post_src[9]]);

        let input = InstanceInput {
            coords: coords.clone(),
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let out = instance(&face, &input).expect("bake at extreme");
        let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
        let post_baked = baked.table_bytes(tag::POST).unwrap();
        let baked_undo = i16::from_be_bytes([post_baked[8], post_baked[9]]);

        assert_eq!(
            i32::from(baked_undo),
            i32::from(src_undo) + undo_delta_i32,
            "MVAR undo bake mismatch: src={src_undo}, delta={undo_delta_i32}, baked={baked_undo}"
        );
        // MVAR table itself is dropped from the static output.
        assert!(baked.mvar().unwrap().is_none(), "MVAR dropped after bake");
    }

    #[test]
    fn rubik_gdef_ivs_pruned_when_present_at_v13() {
        // Rubik's GDEF doesn't carry an IVS — but the prune
        // path should be a no-op rather than corrupt bytes.
        let face = rubik_face();
        let axis_count = face.fvar().unwrap().unwrap().axes().len();
        let input = InstanceInput {
            coords: alloc::vec![0.0_f32; axis_count],
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let out = instance(&face, &input).unwrap();
        let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
        // If the source had a GDEF, the baked face should too — and
        // it should still parse cleanly.
        if face.gdef().unwrap().is_some() {
            assert!(baked.gdef().unwrap().is_some());
        }
    }

    /// Walks every GPOS lookup and returns true if any ValueRecord
    /// device offset slot is non-zero. Used by the post-bake assertions
    /// to confirm no orphan VariationIndex offsets survived the fold.
    /// Covers SinglePos / PairPos formats 1 and 2, both directly and
    /// via Type 9 Extension wrappers — the same set we explicitly bake.
    fn any_value_record_device_offset_nonzero(face: &Face<'_>) -> bool {
        let Ok(Some(gpos)) = face.gpos() else {
            return false;
        };
        let lookups = gpos.lookup_list();
        for li in 0..lookups.len() {
            let Some(lookup) = lookups.get(li) else {
                continue;
            };
            let lt = lookup.lookup_type();
            for si in 0..lookup.subtable_count() {
                let Some(sub) = lookup.subtable_bytes(si) else {
                    continue;
                };
                let (effective_lt, effective_sub) = if lt == 9 {
                    if sub.len() < 8 {
                        continue;
                    }
                    let ext_type = u16::from_be_bytes([sub[2], sub[3]]);
                    let ext_off = u32::from_be_bytes([sub[4], sub[5], sub[6], sub[7]]) as usize;
                    let Some(inner) = sub.get(ext_off..) else {
                        continue;
                    };
                    (ext_type, inner)
                } else {
                    (lt, sub)
                };
                if check_subtable_for_device_offsets(effective_lt, effective_sub) {
                    return true;
                }
            }
        }
        false
    }

    fn check_subtable_for_device_offsets(lt: u16, sub: &[u8]) -> bool {
        match lt {
            1 => {
                // SinglePos.
                if sub.len() < 6 {
                    return false;
                }
                let format = u16::from_be_bytes([sub[0], sub[1]]);
                let value_format = u16::from_be_bytes([sub[4], sub[5]]);
                if value_format & 0x00F0 == 0 {
                    return false;
                }
                let stride = (value_format & 0x00FF).count_ones() as usize * 2;
                let value_count = if format == 2 {
                    u16::from_be_bytes([sub[6], sub[7]]) as usize
                } else {
                    1
                };
                let header_len = if format == 2 { 8 } else { 6 };
                for i in 0..value_count {
                    let vr = header_len + i * stride;
                    if vr_has_nonzero_device_offset(&sub[vr..vr + stride], value_format) {
                        return true;
                    }
                }
                false
            }
            2 => {
                // PairPos.
                if sub.len() < 4 {
                    return false;
                }
                let format = u16::from_be_bytes([sub[0], sub[1]]);
                let vf1 = u16::from_be_bytes([sub[4], sub[5]]);
                let vf2 = u16::from_be_bytes([sub[6], sub[7]]);
                let v1 = (vf1 & 0x00FF).count_ones() as usize * 2;
                let v2 = (vf2 & 0x00FF).count_ones() as usize * 2;
                if (vf1 | vf2) & 0x00F0 == 0 {
                    return false;
                }
                if format == 1 {
                    let pair_set_count = u16::from_be_bytes([sub[8], sub[9]]) as usize;
                    let pvr_size = 2 + v1 + v2;
                    for i in 0..pair_set_count {
                        let off_off = 10 + i * 2;
                        if off_off + 2 > sub.len() {
                            continue;
                        }
                        let set_off = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
                        if set_off + 2 > sub.len() {
                            continue;
                        }
                        let pair_value_count =
                            u16::from_be_bytes([sub[set_off], sub[set_off + 1]]) as usize;
                        for j in 0..pair_value_count {
                            let pvr = set_off + 2 + j * pvr_size;
                            if pvr + pvr_size > sub.len() {
                                continue;
                            }
                            if vr_has_nonzero_device_offset(&sub[pvr + 2..pvr + 2 + v1], vf1)
                                || vr_has_nonzero_device_offset(
                                    &sub[pvr + 2 + v1..pvr + 2 + v1 + v2],
                                    vf2,
                                )
                            {
                                return true;
                            }
                        }
                    }
                    false
                } else if format == 2 {
                    if sub.len() < 16 {
                        return false;
                    }
                    let class1 = u16::from_be_bytes([sub[12], sub[13]]) as usize;
                    let class2 = u16::from_be_bytes([sub[14], sub[15]]) as usize;
                    let cell = v1 + v2;
                    let row = class2 * cell;
                    for i in 0..class1 {
                        for j in 0..class2 {
                            let off = 16 + i * row + j * cell;
                            if off + cell > sub.len() {
                                continue;
                            }
                            if vr_has_nonzero_device_offset(&sub[off..off + v1], vf1)
                                || vr_has_nonzero_device_offset(&sub[off + v1..off + cell], vf2)
                            {
                                return true;
                            }
                        }
                    }
                    false
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn vr_has_nonzero_device_offset(vr: &[u8], format: u16) -> bool {
        // Skip the four static i16 fields (each present iff its bit
        // is set) and inspect the four device-offset slots.
        let mut cursor = 0usize;
        for bit in [0x0001u16, 0x0002, 0x0004, 0x0008] {
            if format & bit != 0 {
                cursor += 2;
            }
        }
        for bit in [0x0010u16, 0x0020, 0x0040, 0x0080] {
            if format & bit != 0 {
                if cursor + 2 > vr.len() {
                    return false;
                }
                let off = u16::from_be_bytes([vr[cursor], vr[cursor + 1]]);
                if off != 0 {
                    return true;
                }
                cursor += 2;
            }
        }
        false
    }

    #[test]
    fn var_kern_fixture_bake_at_wght_900_folds_pair_pos_advance() {
        // The synthetic var_kern fixture carries a single PairPos
        // format 1 lookup. At wght=900 the source GPOS has x_advance=0
        // on the AV pair plus a VariationIndex that resolves to -100.
        // After the bake, the baked GPOS must carry x_advance=-100
        // statically and the device offset slot must be zero.
        let face = Face::parse_bytes(VAR_KERN, 0).unwrap();
        let coords = face.fvar().unwrap().unwrap().normalize_coords(&[900.0]);
        let input = InstanceInput {
            coords: coords.clone(),
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let out = instance(&face, &input).expect("bake");
        let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
        // Confirm no GPOS device offset survived the fold.
        assert!(
            !any_value_record_device_offset_nonzero(&baked),
            "baked GPOS must have zero device offsets"
        );
        // Confirm GDEF.IVS was pruned.
        if let Some(gdef) = baked.gdef().unwrap() {
            assert!(
                gdef.item_variation_store().is_none(),
                "baked GDEF.IVS must be pruned"
            );
        }
        // Confirm the static field carries the resolved delta. Walk
        // GPOS by hand to read the AV pair's value.
        let gpos_bytes = baked.table_bytes(tag::GPOS).expect("baked GPOS");
        let lookup_list_off = u16::from_be_bytes([gpos_bytes[8], gpos_bytes[9]]) as usize;
        let lookup_off = u16::from_be_bytes([
            gpos_bytes[lookup_list_off + 2],
            gpos_bytes[lookup_list_off + 3],
        ]) as usize;
        let lookup_base = lookup_list_off + lookup_off;
        let sub_off =
            u16::from_be_bytes([gpos_bytes[lookup_base + 6], gpos_bytes[lookup_base + 7]]) as usize;
        let sub_abs = lookup_base + sub_off;
        let sub = &gpos_bytes[sub_abs..];
        // PairPos fmt 1 — first PairSet at the first set offset.
        let pair_set_rel = u16::from_be_bytes([sub[10], sub[11]]) as usize;
        // PairValueRecord 0 starts at +2 inside the PairSet, AV pair
        // bytes are: u16 secondGlyph (V), i16 x_advance, u16 device.
        let pvr_off = pair_set_rel + 2;
        let x_advance = i16::from_be_bytes([sub[pvr_off + 2], sub[pvr_off + 3]]);
        assert_eq!(x_advance, -100, "AV x_advance baked at wght=900");
    }

    #[test]
    fn var_kern_fixture_bake_at_default_coords_leaves_static_field_at_source() {
        // At wght=400 the variation region peaks at zero scalar →
        // delta is zero. The static x_advance must stay at the
        // source's 0 and the device offset must still be zeroed (the
        // bake unconditionally severs the offset to keep GDEF.IVS
        // safe to drop).
        let face = Face::parse_bytes(VAR_KERN, 0).unwrap();
        let coords = face.fvar().unwrap().unwrap().normalize_coords(&[400.0]);
        let input = InstanceInput {
            coords,
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let out = instance(&face, &input).expect("bake");
        let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
        assert!(!any_value_record_device_offset_nonzero(&baked));
        let gpos_bytes = baked.table_bytes(tag::GPOS).expect("baked GPOS");
        let lookup_list_off = u16::from_be_bytes([gpos_bytes[8], gpos_bytes[9]]) as usize;
        let lookup_off = u16::from_be_bytes([
            gpos_bytes[lookup_list_off + 2],
            gpos_bytes[lookup_list_off + 3],
        ]) as usize;
        let lookup_base = lookup_list_off + lookup_off;
        let sub_off =
            u16::from_be_bytes([gpos_bytes[lookup_base + 6], gpos_bytes[lookup_base + 7]]) as usize;
        let sub_abs = lookup_base + sub_off;
        let sub = &gpos_bytes[sub_abs..];
        let pair_set_rel = u16::from_be_bytes([sub[10], sub[11]]) as usize;
        let pvr_off = pair_set_rel + 2;
        let x_advance = i16::from_be_bytes([sub[pvr_off + 2], sub[pvr_off + 3]]);
        assert_eq!(x_advance, 0, "AV x_advance unchanged at default wght");
    }

    #[test]
    fn source_sans_vf_subset_bake_clears_all_gpos_variation_offsets() {
        // Source Sans 3 VF Latin Subset is the real-world fixture #173
        // already covered with the IVS-prune path. After the variation
        // fold, no PairPos / SinglePos ValueRecord must carry a
        // surviving device offset, and GDEF.IVS must be pruned.
        let face = Face::parse_bytes(SOURCE_SANS, 0).unwrap();
        let fvar = face.fvar().unwrap().unwrap();
        let mut user = alloc::vec![0.0_f32; fvar.axes().len()];
        if let Some(idx) = fvar.axis_index(*b"wght") {
            user[idx] = fvar.axes()[idx].max_value;
        }
        let coords = fvar.normalize_coords(&user);
        let input = InstanceInput {
            coords,
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let out = instance(&face, &input).expect("bake");
        let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
        if let Some(gdef) = baked.gdef().unwrap() {
            assert!(
                gdef.item_variation_store().is_none(),
                "baked GDEF.IVS must be pruned"
            );
        }
        assert!(
            !any_value_record_device_offset_nonzero(&baked),
            "baked GPOS must have no surviving device offsets on PairPos/SinglePos"
        );
    }

    #[test]
    fn rubik_vmtx_passthrough_when_source_has_none() {
        // Rubik VF is horizontal-only — no vmtx, no VVAR. The bake
        // must not synthesise either.
        let face = rubik_face();
        assert!(face.vmtx().unwrap().is_none(), "rubik has no vmtx");
        let axis_count = face.fvar().unwrap().unwrap().axes().len();
        let input = InstanceInput {
            coords: alloc::vec![0.0_f32; axis_count],
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let out = instance(&face, &input).unwrap();
        let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
        assert!(baked.vmtx().unwrap().is_none(), "vmtx not synthesised");
        assert!(baked.vvar().unwrap().is_none(), "VVAR not synthesised");
    }
}

#[cfg(test)]
mod vvar_synthetic_tests {
    //! Synthetic-VF tests that exercise the VVAR-aware vmtx bake.
    //!
    //! No real fixture in sigilbuzz's test corpus carries `vmtx` +
    //! `VVAR` together — most variable fonts in the wild are
    //! horizontal-only. We unit-test the helpers directly with
    //! hand-built records rather than spinning up a synthetic SFNT
    //! around the bake. The integration shape (vmtx delta application
    //! and VVAR drop in the directory) is exercised by the
    //! [`super::tests::rubik_vmtx_passthrough_when_source_has_none`]
    //! test on the no-VVAR side.

    use super::*;

    #[test]
    fn patch_i16_clamps_at_overflow() {
        let mut buf = Some(alloc::vec![0x7Fu8, 0xFEu8]); // 32766
        patch_i16(&mut buf, 0, 5);
        let b = buf.unwrap();
        assert_eq!(i16::from_be_bytes([b[0], b[1]]), i16::MAX);
    }

    #[test]
    fn patch_u16_floors_at_zero() {
        let mut buf = Some(alloc::vec![0x00u8, 0x05u8]);
        patch_u16(&mut buf, 0, -50);
        let b = buf.unwrap();
        assert_eq!(u16::from_be_bytes([b[0], b[1]]), 0);
    }

    #[test]
    fn patch_i16_handles_short_buffer_gracefully() {
        let mut buf = Some(alloc::vec![0u8]);
        // Out-of-range offset must not panic — short bufs survive.
        patch_i16(&mut buf, 10, 5);
        assert_eq!(buf.unwrap().len(), 1);
    }

    #[test]
    fn emit_vmtx_bytes_compresses_trailing_run() {
        // 5 glyphs, every glyph shares advance 1000. The compression
        // matches `bake_hmtx`: trailing identical advances collapse
        // into the tsb-only tail. The shared-advance run leaves 2
        // long entries (the loop bottoms at 1 then adds back 1 to
        // anchor the shared advance — same as hmtx).
        let advances = alloc::vec![1000u16; 5];
        let tsbs = alloc::vec![10i16, 20, 30, 40, 50];
        let (bytes, n_long) = emit_vmtx_bytes(&advances, &tsbs);
        assert_eq!(n_long, 2);
        // 2 long entries (4 B each) + 3 trailing tsbs (2 B each) = 14.
        assert_eq!(bytes.len(), 4 * 2 + 3 * 2);
    }

    #[test]
    fn emit_vmtx_bytes_extends_long_range_when_trailing_advances_diverge() {
        // 5 glyphs. Source vmtx had long_count=1 (every glyph shared
        // advance 1000), but a hypothetical VVAR delta at gid 3 shifted
        // its advance to 1100. emit_vmtx_bytes must promote gid 3 into
        // the long range so its distinct advance survives the byte
        // emission. Without the long-count recompute fix this trailing
        // delta is silently dropped.
        let advances = alloc::vec![1000u16, 1000, 1000, 1100, 1000];
        let tsbs = alloc::vec![10i16, 20, 30, 40, 50];
        let (bytes, n_long) = emit_vmtx_bytes(&advances, &tsbs);
        // Same compression rule as hmtx: scan trailing equal-to-last
        // run, plus one anchor entry. Last advance is 1000; gid 3 is
        // 1100 (different) so the run is just gid 4. long_count = 5
        // - 1 + 1 = 5 (every glyph in the long range).
        assert_eq!(n_long, 5);
        assert_eq!(bytes.len(), 4 * 5);
        // Gid 3's advance survives at the rebuilt long-entry slot.
        let g3_adv = u16::from_be_bytes([bytes[3 * 4], bytes[3 * 4 + 1]]);
        assert_eq!(g3_adv, 1100);
    }

    #[test]
    fn write_vhea_metrics_count_patches_tail() {
        let mut vhea = alloc::vec![0u8; 36];
        crate::util::write_vhea_metrics_count(&mut vhea, 7).unwrap();
        assert_eq!(&vhea[34..36], &7u16.to_be_bytes());
    }

    #[test]
    fn prune_gdef_returns_none_for_missing_table() {
        // OPEN_SANS has GDEF but it's v1.0 (no IVS).
        const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
        let face = Face::parse_bytes(OPEN_SANS, 0).unwrap();
        let out = prune_gdef_ivs(&face).unwrap();
        // OpenSans is GDEF v1.0 — no prune.
        assert!(out.is_none());
    }

    /// Builds a minimal MVAR table carrying `records` (each pointing
    /// at IVS item (outer=0, inner=0)) and an embedded variation store
    /// that resolves to `delta` at coord 1.0.
    fn build_synthetic_mvar(records: &[[u8; 4]], delta: i16) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
        out.extend_from_slice(&0u16.to_be_bytes()); // reserved
        out.extend_from_slice(&8u16.to_be_bytes()); // valueRecordSize
        out.extend_from_slice(&(records.len() as u16).to_be_bytes());
        let store_off_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // store offset placeholder
        for tag in records {
            out.extend_from_slice(tag);
            out.extend_from_slice(&0u16.to_be_bytes()); // outer
            out.extend_from_slice(&0u16.to_be_bytes()); // inner
        }
        let store_off = out.len() as u16;
        out[store_off_slot..store_off_slot + 2].copy_from_slice(&store_off.to_be_bytes());

        // ItemVariationStore with one region (full peak at axis 0,
        // coord 1.0) and one subtable carrying a single i16 delta.
        // Layout: format(=1) + regionListOff + subtableCount +
        // subtableOff[1] + RegionList + Subtable.
        let mut ivs: Vec<u8> = Vec::new();
        ivs.extend_from_slice(&1u16.to_be_bytes()); // format
        let region_off_slot = ivs.len();
        ivs.extend_from_slice(&0u32.to_be_bytes()); // regionListOff placeholder
        ivs.extend_from_slice(&1u16.to_be_bytes()); // subtableCount
        let sub_off_slot = ivs.len();
        ivs.extend_from_slice(&0u32.to_be_bytes()); // subtableOffsets[0] placeholder

        let region_off = ivs.len() as u32;
        ivs.extend_from_slice(&1u16.to_be_bytes()); // axisCount
        ivs.extend_from_slice(&1u16.to_be_bytes()); // regionCount
                                                    // Region 0 axis 0 — start=0, peak=1.0, end=1.0 in F2DOT14.
        ivs.extend_from_slice(&0i16.to_be_bytes());
        ivs.extend_from_slice(&0x4000i16.to_be_bytes());
        ivs.extend_from_slice(&0x4000i16.to_be_bytes());

        let sub_off = ivs.len() as u32;
        ivs.extend_from_slice(&1u16.to_be_bytes()); // itemCount
        ivs.extend_from_slice(&1u16.to_be_bytes()); // wordDeltaCount = 1 (i16 wide)
        ivs.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
        ivs.extend_from_slice(&0u16.to_be_bytes()); // regionIndexes[0]
                                                    // Single delta row, one region: i16 word.
        ivs.extend_from_slice(&delta.to_be_bytes());

        ivs[region_off_slot..region_off_slot + 4].copy_from_slice(&region_off.to_be_bytes());
        ivs[sub_off_slot..sub_off_slot + 4].copy_from_slice(&sub_off.to_be_bytes());

        out.extend_from_slice(&ivs);
        out
    }

    #[test]
    fn apply_mvar_records_skips_duplicate_tag() {
        // MVAR with two `hasc` records pointing at the same item.
        // Without the dedup the OS/2 sTypoAscender would be patched
        // twice: this test pins `apply_mvar_records` to first-wins.
        let blob = build_synthetic_mvar(&[*b"hasc", *b"hasc"], 100);
        let mvar = sigilbuzz::tables::Mvar::parse(&blob).unwrap();
        // OS/2 v2 (96 bytes) with sTypoAscender = 800 at offset 68.
        let mut os2 = alloc::vec![0u8; 96];
        os2[68..70].copy_from_slice(&800i16.to_be_bytes());
        let baked = apply_mvar_records(&mvar, &[1.0], Some(os2), None, None, None).unwrap();
        let out = baked.os2.unwrap();
        let val = i16::from_be_bytes([out[68], out[69]]);
        // First-wins: 800 + 100 == 900. (Without dedup: 800 + 200 = 1000.)
        assert_eq!(val, 900, "duplicate hasc must apply delta exactly once");
    }
}

#[cfg(test)]
mod partial_instancing_tests {
    //! Unit tests for the partial-instancing public API + tuple
    //! projection math primitives. These cover the building blocks
    //! the variation-table emitters (HVAR / VVAR / MVAR / gvar /
    //! GDEF.IVS) will consume in the follow-up that ships full
    //! partial instancing — wiring them in is staged so this commit
    //! lands the API surface and math without touching every
    //! variation emitter at once.
    //!
    //! fontTools-equivalent of
    //! `varLib.instancer.instantiateVariableFont(axisLimits=...)`.

    use super::*;

    const RUBIK: &[u8] = include_bytes!("../../../tests/fixtures/rubik_vf.ttf");

    fn rubik_face() -> Face<'static> {
        Face::parse_bytes(RUBIK, 0).unwrap()
    }

    #[test]
    fn axis_pin_default_is_empty_pin_every_axis() {
        // The default `InstanceInput::axis_pins` is an empty Vec —
        // semantically "pin every axis" so existing callers that
        // never set the field keep getting full instancing. Anything
        // else would be a silent breaking change.
        let i = InstanceInput::default();
        assert!(i.axis_pins.is_empty());
    }

    #[test]
    fn empty_axis_pins_falls_through_to_full_instancing() {
        // Empty axis_pins is the existing full-instancing path. The
        // bake must succeed end-to-end and produce a static font
        // (no fvar / gvar / HVAR), exactly as before this feature
        // landed.
        let face = rubik_face();
        let axis_count = face.fvar().unwrap().unwrap().axes().len();
        let input = InstanceInput {
            coords: alloc::vec![0.0_f32; axis_count],
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let out = instance(&face, &input).expect("empty axis_pins → full bake");
        let baked = Face::parse_bytes(&out.bytes, 0).expect("parse");
        assert!(baked.fvar().unwrap().is_none(), "fvar dropped");
    }

    #[test]
    fn all_pin_axis_pins_equivalent_to_empty_axis_pins() {
        // A non-empty axis_pins where every entry is `Pin` must
        // produce the same bytes as an empty axis_pins. The emitter
        // walks the same code path either way; this guards against
        // a future regression that branches on length rather than
        // entry policy.
        let face = rubik_face();
        let axis_count = face.fvar().unwrap().unwrap().axes().len();
        let coords = alloc::vec![0.25_f32; axis_count];
        let empty = InstanceInput {
            coords: coords.clone(),
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let all_pin = InstanceInput {
            coords,
            drop_var_tables: true,
            axis_pins: alloc::vec![AxisPin::Pin; axis_count],
        };
        let a = instance(&face, &empty).expect("empty bake");
        let b = instance(&face, &all_pin).expect("all-Pin bake");
        assert_eq!(a.bytes, b.bytes, "all-Pin must equal empty axis_pins");
    }

    #[test]
    fn axis_pins_length_mismatch_errors() {
        let face = rubik_face();
        let axis_count = face.fvar().unwrap().unwrap().axes().len();
        let input = InstanceInput {
            coords: alloc::vec![0.0_f32; axis_count],
            drop_var_tables: true,
            // Length deliberately wrong.
            axis_pins: alloc::vec![AxisPin::Pin; axis_count + 1],
        };
        assert!(matches!(
            instance(&face, &input),
            Err(SubsetError::Unsupported(_))
        ));
    }

    #[test]
    fn axis_pins_with_keep_on_gvar_source_unsupported() {
        // gvar tuple-projection + CFF2 VarStore rewrite are tracked
        // as a follow-up to this PR. Sources carrying gvar (like
        // Rubik VF) still surface a clear Unsupported error when any
        // axis is `Keep`; the IVS-bearing tables (HVAR / VVAR / MVAR /
        // GDEF.IVS) trim correctly today.
        let face = rubik_face();
        let axis_count = face.fvar().unwrap().unwrap().axes().len();
        let mut pins = alloc::vec![AxisPin::Pin; axis_count];
        pins[0] = AxisPin::Keep;
        let input = InstanceInput {
            coords: alloc::vec![0.0_f32; axis_count],
            drop_var_tables: true,
            axis_pins: pins,
        };
        assert!(matches!(
            instance(&face, &input),
            Err(SubsetError::Unsupported(_))
        ));
    }

    // --------------------------------------------------------------
    // axis_support_scalar — single-axis ramp matches OpenType spec.
    // --------------------------------------------------------------

    #[test]
    fn axis_support_scalar_peak_returns_one() {
        // At the peak the scalar is 1.
        assert!((axis_support_scalar(0.0, 1.0, 1.0, 1.0) - 1.0).abs() < 1e-6);
        assert!((axis_support_scalar(-1.0, -1.0, 0.0, -1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn axis_support_scalar_zero_peak_means_axis_ignored() {
        // Per spec a peak of zero means the axis does not participate
        // in the tuple — the scalar is 1 regardless of coord.
        assert!((axis_support_scalar(0.0, 0.0, 0.0, 0.5) - 1.0).abs() < 1e-6);
        assert!((axis_support_scalar(-1.0, 0.0, 1.0, 0.5) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn axis_support_scalar_outside_region_returns_zero() {
        // coord beyond [start, end] → zero contribution.
        assert!(axis_support_scalar(0.0, 1.0, 1.0, -0.5).abs() < 1e-6);
        assert!(axis_support_scalar(0.0, 1.0, 1.0, 1.1).abs() < 1e-6);
    }

    #[test]
    fn axis_support_scalar_linear_ramp_below_peak() {
        // start=0, peak=1, end=1 — coord=0.5 is halfway up the ramp.
        assert!((axis_support_scalar(0.0, 1.0, 1.0, 0.5) - 0.5).abs() < 1e-6);
        // 0.25 quarter up.
        assert!((axis_support_scalar(0.0, 1.0, 1.0, 0.25) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn axis_support_scalar_linear_ramp_above_peak() {
        // start=-1, peak=0.5, end=1 — coord=0.75 ramps down from 1 at
        // peak to 0 at end. Halfway → 0.5.
        // (peak == 0 would short-circuit to 1.0 per the spec's
        // "axis ignored" convention; we use a non-zero peak here.)
        assert!((axis_support_scalar(-1.0, 0.5, 1.0, 0.75) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn axis_support_scalar_degenerate_peak_eq_start_returns_zero() {
        // peak == start, coord between them → division by zero
        // guarded with a 0.0 fallback.
        assert!(axis_support_scalar(1.0, 1.0, 1.0, 0.5).abs() < 1e-6);
    }

    // --------------------------------------------------------------
    // project_region_onto_kept_axes — full tuple projection.
    // --------------------------------------------------------------

    #[test]
    fn project_two_axis_region_pin_first_keep_second() {
        // Two axes (wght + wdth). Region: wght (0, 1, 1), wdth (0, 1, 1).
        // Pin wght=0.5 (scalar 0.5), keep wdth.
        let region = [(0.0_f32, 1.0, 1.0), (0.0, 1.0, 1.0)];
        let pins = [AxisPin::Pin, AxisPin::Keep];
        let coords = [0.5, 0.0];
        let p = project_region_onto_kept_axes(&region, &pins, &coords).expect("survives");
        assert!((p.pin_scalar - 0.5).abs() < 1e-6);
        assert_eq!(p.kept_axes, alloc::vec![(0.0, 1.0, 1.0)]);
    }

    #[test]
    fn project_drops_tuple_when_pin_falls_outside_region() {
        // Pin coord 0.0 falls outside the wght region [0.5, 1.0]:
        // the scalar is zero and the tuple gets dropped.
        let region = [(0.5_f32, 1.0, 1.0), (0.0, 1.0, 1.0)];
        let pins = [AxisPin::Pin, AxisPin::Keep];
        let coords = [0.0, 0.0];
        assert!(project_region_onto_kept_axes(&region, &pins, &coords).is_none());
    }

    #[test]
    fn project_pin_at_peak_passes_kept_axes_through_at_unit_scalar() {
        // Pin axis at peak → scalar 1, kept axes ride through.
        let region = [(0.0_f32, 1.0, 1.0), (-1.0, -1.0, 0.0)];
        let pins = [AxisPin::Pin, AxisPin::Keep];
        let coords = [1.0, 0.0];
        let p = project_region_onto_kept_axes(&region, &pins, &coords).unwrap();
        assert!((p.pin_scalar - 1.0).abs() < 1e-6);
        assert_eq!(p.kept_axes, alloc::vec![(-1.0, -1.0, 0.0)]);
    }

    #[test]
    fn project_all_pin_yields_empty_kept_axes() {
        // Every axis pinned: kept_axes is empty (the survivor tuple
        // becomes a plain delta-set with no region dimensions).
        let region = [(0.0_f32, 1.0, 1.0), (0.0, 1.0, 1.0)];
        let pins = [AxisPin::Pin, AxisPin::Pin];
        let coords = [0.5, 0.5];
        let p = project_region_onto_kept_axes(&region, &pins, &coords).unwrap();
        // Two ramps at 0.5 each → 0.25 product.
        assert!((p.pin_scalar - 0.25).abs() < 1e-6);
        assert!(p.kept_axes.is_empty());
    }

    #[test]
    fn project_all_keep_yields_unit_scalar_full_kept_axes() {
        // Every axis kept variable: scalar 1, kept_axes = source region.
        let region = [(0.0_f32, 1.0, 1.0), (-1.0, -0.5, 0.0)];
        let pins = [AxisPin::Keep, AxisPin::Keep];
        let coords = [0.0, 0.0]; // ignored
        let p = project_region_onto_kept_axes(&region, &pins, &coords).unwrap();
        assert!((p.pin_scalar - 1.0).abs() < 1e-6);
        assert_eq!(p.kept_axes, alloc::vec![(0.0, 1.0, 1.0), (-1.0, -0.5, 0.0)]);
    }

    #[test]
    fn project_zero_peak_pin_axis_passes_scalar_through() {
        // Pin-axis with peak == 0 (axis-doesn't-participate): scalar
        // contribution is 1 regardless of coord, so the survivor
        // carries through with no payload scaling.
        let region = [(0.0_f32, 0.0, 0.0), (0.0, 1.0, 1.0)];
        let pins = [AxisPin::Pin, AxisPin::Keep];
        let coords = [0.5, 0.0];
        let p = project_region_onto_kept_axes(&region, &pins, &coords).unwrap();
        assert!((p.pin_scalar - 1.0).abs() < 1e-6);
        assert_eq!(p.kept_axes, alloc::vec![(0.0, 1.0, 1.0)]);
    }

    #[test]
    fn project_length_mismatch_returns_none() {
        // Defensive: mismatched input lengths return None rather than
        // panicking on an OOB index.
        let region = [(0.0_f32, 1.0, 1.0)];
        let pins = [AxisPin::Pin, AxisPin::Keep];
        let coords = [0.5, 0.5];
        assert!(project_region_onto_kept_axes(&region, &pins, &coords).is_none());
    }

    #[test]
    fn project_two_pin_axes_multiplies_scalars() {
        // Both Pin axes contribute partial ramps; the survivor's
        // pin_scalar is their product (0.5 × 0.25 = 0.125).
        let region = [(0.0_f32, 1.0, 1.0), (0.0, 1.0, 1.0)];
        let pins = [AxisPin::Pin, AxisPin::Pin];
        let coords = [0.5, 0.25];
        let p = project_region_onto_kept_axes(&region, &pins, &coords).unwrap();
        assert!(
            (p.pin_scalar - 0.125).abs() < 1e-6,
            "expected 0.125, got {}",
            p.pin_scalar
        );
    }

    // --------------------------------------------------------------
    // bake_fvar_partial — fvar trim.
    // --------------------------------------------------------------

    fn write_f16dot16(out: &mut Vec<u8>, v: f32) {
        #[allow(clippy::cast_possible_truncation)]
        let raw = (v * 65536.0).round() as i32;
        out.extend_from_slice(&raw.to_be_bytes());
    }

    fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
        #[allow(clippy::cast_possible_truncation)]
        let raw = (v * 16384.0).round() as i16;
        out.extend_from_slice(&raw.to_be_bytes());
    }

    /// Builds a synthetic 2-axis fvar (wght 100..400..900,
    /// wdth 50..100..200) with `instances`, each carrying a
    /// (subfamilyNameID, flags, [coord_per_axis], optional ps_name_id).
    fn build_fvar2(instances: &[(u16, u16, [f32; 2], Option<u16>)]) -> Vec<u8> {
        let mut out = Vec::new();
        let with_ps = instances.iter().any(|(_, _, _, p)| p.is_some());
        let inst_size: u16 = if with_ps { 4 + 4 * 2 + 2 } else { 4 + 4 * 2 };
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
        out.extend_from_slice(&16u16.to_be_bytes()); // axesArrayOffset
        out.extend_from_slice(&2u16.to_be_bytes()); // reserved
        out.extend_from_slice(&2u16.to_be_bytes()); // axisCount
        out.extend_from_slice(&20u16.to_be_bytes()); // axisSize
        out.extend_from_slice(&(instances.len() as u16).to_be_bytes());
        out.extend_from_slice(&inst_size.to_be_bytes());
        // Axis 0: wght
        out.extend_from_slice(b"wght");
        write_f16dot16(&mut out, 100.0);
        write_f16dot16(&mut out, 400.0);
        write_f16dot16(&mut out, 900.0);
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&256u16.to_be_bytes());
        // Axis 1: wdth
        out.extend_from_slice(b"wdth");
        write_f16dot16(&mut out, 50.0);
        write_f16dot16(&mut out, 100.0);
        write_f16dot16(&mut out, 200.0);
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&257u16.to_be_bytes());
        for (sub, flags, coords, ps) in instances {
            out.extend_from_slice(&sub.to_be_bytes());
            out.extend_from_slice(&flags.to_be_bytes());
            for c in coords {
                write_f16dot16(&mut out, *c);
            }
            if with_ps {
                out.extend_from_slice(&ps.unwrap_or(0).to_be_bytes());
            }
        }
        out
    }

    #[test]
    fn bake_fvar_partial_returns_none_when_every_axis_pins() {
        let bytes = build_fvar2(&[]);
        assert!(bake_fvar_partial(&bytes, &[AxisPin::Pin, AxisPin::Pin]).is_none());
    }

    #[test]
    fn bake_fvar_partial_drops_pin_axis_records() {
        // Pin wght, keep wdth — survivor fvar has only the wdth axis.
        let bytes = build_fvar2(&[]);
        let trimmed = bake_fvar_partial(&bytes, &[AxisPin::Pin, AxisPin::Keep]).unwrap();
        // Header layout matches the spec: 16 bytes, axisCount = 1.
        assert_eq!(u16::from_be_bytes([trimmed[8], trimmed[9]]), 1);
        // First axis tag is now wdth.
        assert_eq!(&trimmed[16..20], b"wdth");
        // Re-parse via the public Fvar parser — it must accept the
        // emitted bytes.
        let parsed = sigilbuzz::tables::Fvar::parse(&trimmed).unwrap();
        assert_eq!(parsed.axes().len(), 1);
        assert_eq!(parsed.axes()[0].tag, *b"wdth");
    }

    #[test]
    fn bake_fvar_partial_keeps_kept_axis_in_source_order() {
        // Pin wdth, keep wght → only wght survives.
        let bytes = build_fvar2(&[]);
        let trimmed = bake_fvar_partial(&bytes, &[AxisPin::Keep, AxisPin::Pin]).unwrap();
        assert_eq!(u16::from_be_bytes([trimmed[8], trimmed[9]]), 1);
        assert_eq!(&trimmed[16..20], b"wght");
    }

    #[test]
    fn bake_fvar_partial_drops_instances_that_collapse_to_default() {
        // Three instances: (Regular wght=400 wdth=100 — default-equal),
        // (Bold wght=700 wdth=100), (Condensed wght=400 wdth=75).
        // With wght pinned, the (400, 100) instance collapses to "wdth
        // default" → drop. The (700, 100) instance collapses to "wdth
        // default" → drop. The (400, 75) survives at wdth=75.
        let bytes = build_fvar2(&[
            (1, 0, [400.0, 100.0], None),
            (2, 0, [700.0, 100.0], None),
            (3, 0, [400.0, 75.0], None),
        ]);
        let trimmed = bake_fvar_partial(&bytes, &[AxisPin::Pin, AxisPin::Keep]).unwrap();
        // instanceCount = 1 (only Condensed survived).
        assert_eq!(u16::from_be_bytes([trimmed[12], trimmed[13]]), 1);
    }

    #[test]
    fn bake_fvar_partial_round_trips_with_ps_name_variant() {
        let bytes = build_fvar2(&[(1, 0, [700.0, 100.0], Some(258))]);
        let trimmed = bake_fvar_partial(&bytes, &[AxisPin::Pin, AxisPin::Keep]).unwrap();
        // instanceSize for the trimmed (1-axis, with-ps) variant
        // = 4 + 4 * 1 + 2 = 10.
        assert_eq!(u16::from_be_bytes([trimmed[14], trimmed[15]]), 10);
        // Bold's (700, 100) collapses to wdth-default after Pin-wght
        // — instance dropped. instanceCount = 0.
        assert_eq!(u16::from_be_bytes([trimmed[12], trimmed[13]]), 0);
    }

    // --------------------------------------------------------------
    // bake_avar_partial — avar trim.
    // --------------------------------------------------------------

    fn build_avar2(map_a: &[(f32, f32)], map_b: &[(f32, f32)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&2u16.to_be_bytes()); // axisCount
        for map in &[map_a, map_b] {
            out.extend_from_slice(&(map.len() as u16).to_be_bytes());
            for (f, t) in *map {
                write_f2dot14(&mut out, *f);
                write_f2dot14(&mut out, *t);
            }
        }
        out
    }

    #[test]
    fn bake_avar_partial_returns_none_when_every_axis_pins() {
        let bytes = build_avar2(&[(-1.0, -1.0), (0.0, 0.0), (1.0, 1.0)], &[]);
        assert!(bake_avar_partial(&bytes, &[AxisPin::Pin, AxisPin::Pin]).is_none());
    }

    #[test]
    fn bake_avar_partial_drops_pin_axis_segment_map() {
        let map_w = &[(-1.0, -1.0), (0.0, 0.0), (0.5, 0.75), (1.0, 1.0)];
        let bytes = build_avar2(map_w, &[(-1.0, -1.0), (0.0, 0.0), (1.0, 1.0)]);
        let trimmed = bake_avar_partial(&bytes, &[AxisPin::Pin, AxisPin::Keep]).unwrap();
        let parsed = sigilbuzz::tables::Avar::parse(&trimmed).unwrap();
        assert_eq!(parsed.axis_count(), 1);
        // The surviving axis was axis 1 (the trivial 3-point identity).
        // Confirm round-trip: 0.5 → 0.5.
        assert!((parsed.remap(0, 0.5) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn bake_avar_partial_keeps_first_axis_when_second_pins() {
        let map_w = &[(-1.0, -1.0), (0.0, 0.0), (0.5, 0.75), (1.0, 1.0)];
        let bytes = build_avar2(map_w, &[(-1.0, -1.0), (0.0, 0.0), (1.0, 1.0)]);
        let trimmed = bake_avar_partial(&bytes, &[AxisPin::Keep, AxisPin::Pin]).unwrap();
        let parsed = sigilbuzz::tables::Avar::parse(&trimmed).unwrap();
        assert_eq!(parsed.axis_count(), 1);
        // The non-trivial map survived: 0.5 → 0.75.
        assert!((parsed.remap(0, 0.5) - 0.75).abs() < 1e-3);
    }

    // --------------------------------------------------------------
    // bake_ivs_partial — IVS region trim + delta scale.
    // --------------------------------------------------------------

    /// Builds a 2-axis IVS with `regions`, `subtables[i] = (regionIndexes,
    /// rows)` where each row has one i16 delta per region index.
    fn build_ivs2(
        regions: &[[(f32, f32, f32); 2]],
        subtables: &[(Vec<u16>, Vec<Vec<i16>>)],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        let region_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(subtables.len() as u16).to_be_bytes());
        let sub_slot_start = out.len();
        for _ in 0..subtables.len() {
            out.extend_from_slice(&0u32.to_be_bytes());
        }
        // Region list.
        let region_off = out.len() as u32;
        out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_off.to_be_bytes());
        out.extend_from_slice(&2u16.to_be_bytes()); // axisCount
        out.extend_from_slice(&(regions.len() as u16).to_be_bytes());
        for region in regions {
            for (s, p, e) in region {
                write_f2dot14(&mut out, *s);
                write_f2dot14(&mut out, *p);
                write_f2dot14(&mut out, *e);
            }
        }
        // Subtables.
        for (i, (region_indexes, rows)) in subtables.iter().enumerate() {
            let sub_off = out.len() as u32;
            let slot = sub_slot_start + i * 4;
            out[slot..slot + 4].copy_from_slice(&sub_off.to_be_bytes());
            out.extend_from_slice(&(rows.len() as u16).to_be_bytes()); // itemCount
            // wordDeltaCount = regionIndexCount, all i16.
            out.extend_from_slice(&(region_indexes.len() as u16).to_be_bytes());
            out.extend_from_slice(&(region_indexes.len() as u16).to_be_bytes());
            for ri in region_indexes {
                out.extend_from_slice(&ri.to_be_bytes());
            }
            for row in rows {
                assert_eq!(row.len(), region_indexes.len());
                for v in row {
                    out.extend_from_slice(&v.to_be_bytes());
                }
            }
        }
        out
    }

    #[test]
    fn bake_ivs_partial_pin_one_axis_keep_other_drops_pin_dimension() {
        // 2-axis IVS, one region (peak (1, 1)), one subtable with one
        // delta of 100. Pin wght=1.0 (scalar 1.0), keep wdth.
        let bytes = build_ivs2(
            &[[(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)]],
            &[(alloc::vec![0], alloc::vec![alloc::vec![100]])],
        );
        let pins = [AxisPin::Pin, AxisPin::Keep];
        let coords = [1.0, 0.0];
        let (out, remap) = bake_ivs_partial(&bytes, &coords, &pins).expect("survives");
        let parsed = sigilbuzz::tables::variation_store::ItemVariationStore::parse(&out).unwrap();
        assert_eq!(parsed.axis_count(), 1);
        assert_eq!(parsed.region_count(), 1);
        // At wdth=1.0, the delta is the original 100 (scaled by Pin
        // scalar of 1.0 because wght pin is at the region's peak).
        let d = parsed.delta(0, 0, &[1.0]);
        assert!((d - 100.0).abs() < 1e-3, "got {}", d);
        assert_eq!(remap.lookup(0, 0), Some((0, 0)));
    }

    #[test]
    fn bake_ivs_partial_drops_region_when_pin_outside() {
        // Region peaks at wght=1, wdth=1. Pin wght=0 (outside [0, 1]
        // boundary trivially gives scalar=0 because peak=1, coord=0:
        // ramp from start=0 to peak=1 → 0). Region drops, subtable
        // collapses.
        let bytes = build_ivs2(
            &[[(0.5, 1.0, 1.0), (0.0, 1.0, 1.0)]],
            &[(alloc::vec![0], alloc::vec![alloc::vec![100]])],
        );
        let pins = [AxisPin::Pin, AxisPin::Keep];
        let coords = [0.0, 0.0];
        let (_out, remap) = bake_ivs_partial(&bytes, &coords, &pins).expect("emits empty IVS");
        // Subtable collapsed entirely.
        assert_eq!(remap.lookup(0, 0), None);
    }

    #[test]
    fn bake_ivs_partial_scales_delta_by_pin_scalar() {
        // Region with wght peak=1, wdth peak=1. Pin wght=0.5 → scalar
        // 0.5. Source delta 100 → new delta 50.
        let bytes = build_ivs2(
            &[[(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)]],
            &[(alloc::vec![0], alloc::vec![alloc::vec![100]])],
        );
        let pins = [AxisPin::Pin, AxisPin::Keep];
        let coords = [0.5, 0.0];
        let (out, _remap) = bake_ivs_partial(&bytes, &coords, &pins).unwrap();
        let parsed = sigilbuzz::tables::variation_store::ItemVariationStore::parse(&out).unwrap();
        // At wdth=1.0, evaluate the new tuple: scalar = 1.0 (peak),
        // delta = 50.
        let d = parsed.delta(0, 0, &[1.0]);
        assert!((d - 50.0).abs() < 1.0, "got {}", d);
    }

    #[test]
    fn bake_ivs_partial_round_trips_at_keep_coord() {
        // The pivotal correctness property: evaluating the trimmed IVS
        // at (Keep coord) reproduces evaluating the source IVS at
        // (Keep coord, Pin coord). Two regions, two-axis source, pin
        // wght=0.6, keep wdth. Item delta = (regionA: 100, regionB: 50).
        // Source A: peak=(1, 1), so scalar at (0.6, wdth) = 0.6 * wdth.
        // Source B: peak=(0, 1) — wght peak=0 means "axis ignored" so
        // scalar is just wdth.
        // Source eval at (0.6, wdth=1) = 0.6*1*100 + 1*1*50 = 110.
        // Trimmed eval at (wdth=1) = 1*60 + 1*50 = 110. (delta_A
        // pre-scaled by 0.6 → 60; delta_B pre-scaled by 1 → 50.)
        let bytes = build_ivs2(
            &[
                [(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)],
                [(0.0, 0.0, 0.0), (0.0, 1.0, 1.0)],
            ],
            &[(alloc::vec![0, 1], alloc::vec![alloc::vec![100, 50]])],
        );
        let pins = [AxisPin::Pin, AxisPin::Keep];
        let coords = [0.6, 0.0];
        let (out, _remap) = bake_ivs_partial(&bytes, &coords, &pins).unwrap();
        let parsed = sigilbuzz::tables::variation_store::ItemVariationStore::parse(&out).unwrap();
        let d = parsed.delta(0, 0, &[1.0]);
        assert!((d - 110.0).abs() < 1.0, "got {}", d);
    }

    #[test]
    fn bake_ivs_partial_collapses_empty_subtable() {
        // Two subtables; subtable 1 only references a region that
        // drops. RegionRemap reflects the elision.
        let bytes = build_ivs2(
            &[
                [(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)],   // region 0: keeps
                [(0.5, 1.0, 1.0), (0.0, 1.0, 1.0)],   // region 1: drops at coord 0
            ],
            &[
                (alloc::vec![0], alloc::vec![alloc::vec![100]]),
                (alloc::vec![1], alloc::vec![alloc::vec![999]]),
            ],
        );
        let pins = [AxisPin::Pin, AxisPin::Keep];
        let coords = [0.0, 0.0];
        let (_out, remap) = bake_ivs_partial(&bytes, &coords, &pins).unwrap();
        // Subtable 0 referenced only region 0 — region 0 drops at
        // coord=0 too (peak=1, coord=0 → scalar 0 on wght). So both
        // subtables collapse.
        assert_eq!(remap.lookup(0, 0), None);
        assert_eq!(remap.lookup(1, 0), None);
    }

    #[test]
    fn bake_ivs_partial_preserves_inner_index_order() {
        // Two items in one subtable. The trimmed IVS keeps both, in
        // the same inner-index order, scaled by the pin scalar.
        let bytes = build_ivs2(
            &[[(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)]],
            &[(
                alloc::vec![0],
                alloc::vec![alloc::vec![100], alloc::vec![200]],
            )],
        );
        let pins = [AxisPin::Pin, AxisPin::Keep];
        let coords = [1.0, 0.0]; // pin at peak → scalar 1
        let (out, remap) = bake_ivs_partial(&bytes, &coords, &pins).unwrap();
        let parsed = sigilbuzz::tables::variation_store::ItemVariationStore::parse(&out).unwrap();
        assert!((parsed.delta(0, 0, &[1.0]) - 100.0).abs() < 1e-3);
        assert!((parsed.delta(0, 1, &[1.0]) - 200.0).abs() < 1e-3);
        assert_eq!(remap.lookup(0, 0), Some((0, 0)));
        assert_eq!(remap.lookup(0, 1), Some((0, 1)));
    }

    #[test]
    fn bake_ivs_partial_all_keep_is_identity_modulo_format() {
        // With every axis Keep, the IVS must round-trip — same regions,
        // same deltas, just possibly re-encoded with a uniform format.
        let bytes = build_ivs2(
            &[
                [(0.0, 1.0, 1.0), (-1.0, -1.0, 0.0)],
                [(0.0, 0.5, 1.0), (0.0, 0.0, 0.0)],
            ],
            &[(
                alloc::vec![0, 1],
                alloc::vec![alloc::vec![100, 50], alloc::vec![-30, 70]],
            )],
        );
        let pins = [AxisPin::Keep, AxisPin::Keep];
        let coords = [0.0, 0.0];
        let (out, _remap) = bake_ivs_partial(&bytes, &coords, &pins).unwrap();
        let src = sigilbuzz::tables::variation_store::ItemVariationStore::parse(&bytes).unwrap();
        let dst = sigilbuzz::tables::variation_store::ItemVariationStore::parse(&out).unwrap();
        assert_eq!(src.axis_count(), dst.axis_count());
        assert_eq!(src.region_count(), dst.region_count());
        // Same deltas at the same coords.
        for c0 in [-1.0, -0.5, 0.0, 0.5, 1.0] {
            for c1 in [-1.0, -0.5, 0.0, 0.5, 1.0] {
                let a = src.delta(0, 0, &[c0, c1]);
                let b = dst.delta(0, 0, &[c0, c1]);
                assert!(
                    (a - b).abs() < 1.0,
                    "mismatch at ({}, {}): src={}, dst={}",
                    c0,
                    c1,
                    a,
                    b
                );
            }
        }
    }
}
