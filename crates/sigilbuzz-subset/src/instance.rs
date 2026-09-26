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
//!   resynthesized on this pass: gvar's composite-glyph contribution
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
//! - `GDEF` keeps every subtable but its `ItemVariationStore`, which is
//!   pruned once the GPOS bake (see below) and the LigCaretList caret
//!   fold have resolved every `VariationIndex` that pointed into it.
//!
//! When `drop_var_tables` is false the variable-font tables ride
//! through verbatim. The glyf and hmtx bake still applies to *bake* the
//! default-instance values into the outline / metric tables, so a
//! consumer that ignores the variable-font tables sees the same shape
//! as a consumer that does honor them.
//!
//! # CFF2 baking
//!
//! For CFF2 sources the [`crate::cff2::bake_at_coords`] helper walks
//! every charstring, inlines `callsubr` / `callgsubr`, resolves every
//! `blend` to its scalar value at `coords`, strips `vsindex`, and
//! emits a fresh CFF2 table without a VariationStore. Output is still
//! CFF2-tagged (the SFNT directory entry remains `CFF2`) but no
//! variable-font opcodes survive. Consumers that ignore CFF2's
//! variable surface see the same outline as a consumer that honors
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
//! field per the spec's tag -> field mapping (`hasc` -> OS/2.sTypoAscender,
//! `xhgt` -> OS/2.sxHeight, `unds` -> post.underlineThickness, ...). The
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
//! therefore lands at the chosen instance, not the default, so the
//! static output renders correctly at the baked coord vector.
//!
//! The supported lookup types are GPOS Type 1 (SinglePos formats 1 / 2),
//! Type 2 (PairPos formats 1 / 2), Type 3 (CursivePos), and Types 4 / 5
//! / 6 (mark attachment), including those wrapped in a Type 9 Extension
//! lookup. `Mark*` and `Cursive` lookups carry their variations on
//! AnchorFormat3 records, whose device offsets are measured from the
//! Anchor itself; PairPos format 1 measures its from the PairSet. See
//! [`crate::gpos_var`] for the per-structure offset bases.
//!
//! # GSUB / GPOS FeatureVariations
//!
//! A 1.1 GSUB or GPOS swaps feature lookups by region of the design
//! space. A full instance applies the record that matches at the
//! instance's coordinates: its substitutions become the default
//! features, and the table becomes version 1.0. A partial instance
//! settles every condition on a pinned axis (a record that can no
//! longer match goes, a condition that always holds goes) and
//! renumbers the axes that stay, as HarfBuzz's instancer does. See
//! [`crate::feature_variations`].
//!
//! # Out of scope (deferred)
//!
//! - **CFF2 partial instancing** (some axes pinned, others left
//!   variable on a CFF2 source). The gvar / TrueType partial path is
//!   wired through [`crate::gvar_partial::bake_gvar_partial`]; the
//!   CFF2 VarStore equivalent lands separately.
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
use sigilbuzz::Face;

mod axes;
mod gdef_store;
mod glyf;
mod ivs;
mod metrics;
mod metrics_var;
mod partial;
mod region;
mod store_remap;

use crate::sfnt;
use crate::util;
use crate::warnings::Warnings;
use crate::{GlyphId, SubsetError, SubsetWarning};
use gdef_store::{prune_gdef_store, GdefBake};
use glyf::bake_glyf_loca;
use metrics::{bake_hmtx, bake_mvar_metrics, bake_vmtx};
use partial::{layout_variations, partial_instance, pinned_axes};

pub(crate) use ivs::bake_ivs_partial;
pub(crate) use region::project_region_onto_kept_axes;

/// F2DOT14 normalized axis coordinate. Matches the on-disk encoding the
/// VF spec uses: a signed 2.14 fixed-point in the range `[-1.0, 1.0]`,
/// where `0` is the axis default and `±1` is the extreme. Callers
/// usually obtain the vector by feeding user-space coords through
/// [`sigilbuzz::tables::Fvar::normalize_coords`].
pub type F2Dot14 = f32;

/// Per-axis pin policy for partial instancing.
///
/// fontTools' `varLib.instancer.instantiateVariableFont(axisLimits=...)`
/// supports pinning a *subset* of axes. The deltas for those axes fold
/// into the static outlines / metrics at the chosen coord, while the
/// remaining axes keep their variation surface and the output is still
/// a variable font (just with fewer axes in `fvar`). [`AxisPin`] is the
/// per-axis switch that drives that behavior from
/// [`InstanceInput::axis_pins`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisPin {
    /// Bake this axis at `coords[i]` into every variation table; drop
    /// the axis from `fvar` / `avar` and from every variation tuple
    /// region. This is the existing full-instancing behavior.
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
    /// `HVAR` / `gvar` from the output. The font becomes static:
    /// shapers will ignore any axis coords passed alongside it.
    ///
    /// If false, leave them in place. Any consumer that does honor
    /// the variable-font tables will see deltas of zero relative to
    /// the baked outlines/metrics, so the result still renders
    /// correctly at the chosen instance, but the file is larger and
    /// shapers will still treat the font as variable.
    pub drop_var_tables: bool,
    /// Per-axis pin policy. An empty vector means "pin every axis"
    /// (the existing full-instancing behavior). When non-empty,
    /// length must equal `coords.len()`; each entry says whether the
    /// matching axis bakes (`Pin`) or stays variable (`Keep`).
    ///
    /// fontTools-equivalent of
    /// `varLib.instancer.instantiateVariableFont(axisLimits={...})`:
    /// `Pin` axes correspond to a bare-coord entry in `axisLimits`,
    /// `Keep` axes correspond to an axis omitted from `axisLimits`.
    ///
    /// Note: when any axis is `Keep` the bake emits a reduced-axis
    /// variable font: `fvar` and `avar` are trimmed to only the
    /// surviving axes, `ItemVariationStore`-bearing tables (HVAR /
    /// VVAR / MVAR / GDEF.IVS) have their region lists rewritten with
    /// every Pin-axis dimension folded into the surviving deltas, and
    /// any tuple that contributes nothing at the pin coords is
    /// dropped. `gvar` is rewritten through the same
    /// `project_region_onto_kept_axes` primitive: every per-tuple
    /// peak / intermediate region keeps only its `Keep`-axis
    /// dimensions, every per-point delta scales by the Pin-axis
    /// support-scalar product, and tuples whose Pin support drops to
    /// zero are dropped. CFF2's VarStore partial-projection is still
    /// staged; a CFF2 source with `Keep` still surfaces an
    /// `Unsupported` error today.
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
    /// Pieces of the source font left out of the instance because they
    /// could not be read, sorted by table and offset. Empty for a well
    /// formed font. See [`SubsetWarning`].
    pub warnings: Vec<SubsetWarning>,
}

/// Instances `face` at `input.coords`. Returns the new font bytes.
///
/// Closure walking is *not* performed: instancing keeps every glyph in
/// the source font; it's not a subset operation. Every gid `0..num_glyphs`
/// rides through with its outline / metric baked.
pub fn instance(face: &Face<'_>, input: &InstanceInput) -> Result<InstancedOutput, SubsetError> {
    if face.record(tag::CFF1).is_some() && face.record(tag::GLYF).is_none() {
        // Pure CFF1 source: there is no variable data to bake; just
        // copy through. We still drop the variable-font directory
        // entries the caller asked us to drop.
        return cff1_passthrough(face, input);
    }

    // Validate axis count up front. fvar is required for instancing:
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
        // CFF2 VarStore partial-projection is wired through
        // [`crate::cff2::bake_cff2_partial`]; gvar tuple-projection
        // is wired through [`crate::gvar_partial::bake_gvar_partial`].
        // Both paths flow through `partial_instance` below.
        if input.axis_pins.contains(&AxisPin::Keep) {
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

    let warnings = Warnings::default();

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
    // OS/2 not hhea: hhea gets the metrics-count patch unconditionally
    // via util::write_hhea_metrics_count below).
    let mut hhea_out = match mvar_bake.hhea.clone() {
        Some(bytes) => bytes,
        None => face
            .table_bytes(tag::HHEA)
            .map_err(SubsetError::from)?
            .to_vec(),
    };
    util::write_hhea_metrics_count(&mut hhea_out, hmtx_out.number_of_h_metrics)?;

    // maxp: pass through verbatim (glyph count is unchanged: instancing
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
    // (VariationIndex offsets on value records and anchors), fold
    // every resolvable variation into the static field it adjusts at
    // `coords` and zero the offset slot. Runs *before* the
    // GDEF.IVS prune below. The prune severs the only path back to
    // the IVS bytes, so any remaining VariationIndex would be orphan.
    let gpos_baked = if input.drop_var_tables {
        bake_gpos_var(face, &coords)?
    } else {
        None
    };
    // FeatureVariations: the record that matches at `coords` becomes
    // the default features, and the table goes.
    let pinned = pinned_axes(&coords, &[]);
    let gpos_baked = layout_variations(face, tag::GPOS, gpos_baked, &pinned, &[], &warnings)?;
    if let Some(b) = layout_variations(face, tag::GSUB, None, &pinned, &[], &warnings)? {
        tables.push((tag::GSUB, b));
    }
    if let Some(b) = gpos_baked.clone() {
        tables.push((tag::GPOS, b));
    }

    // GDEF: when the source carries an ItemVariationStore and the
    // caller wants the static "ship as static" output, prune it. See
    // module header for the GPOS-bake-then-IVS-prune ordering.
    let gdef_bake = if input.drop_var_tables {
        prune_gdef_store(face, &coords, &warnings)?
    } else {
        GdefBake::Unchanged
    };
    if let GdefBake::Rebuilt(b) = &gdef_bake {
        tables.push((tag::GDEF, b.clone()));
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
        if rec.tag == tag::GDEF && !matches!(gdef_bake, GdefBake::Unchanged) {
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
    Ok(InstancedOutput {
        bytes,
        warnings: warnings.into_sorted(),
    })
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

    let warnings = Warnings::default();
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
    let pinned = pinned_axes(coords, &[]);
    let gpos_baked = layout_variations(face, tag::GPOS, gpos_baked, &pinned, &[], &warnings)?;
    if let Some(b) = layout_variations(face, tag::GSUB, None, &pinned, &[], &warnings)? {
        tables.push((tag::GSUB, b));
    }
    if let Some(b) = gpos_baked.clone() {
        tables.push((tag::GPOS, b));
    }

    let gdef_bake = if input.drop_var_tables {
        prune_gdef_store(face, coords, &warnings)?
    } else {
        GdefBake::Unchanged
    };
    if let GdefBake::Rebuilt(b) = &gdef_bake {
        tables.push((tag::GDEF, b.clone()));
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
        if rec.tag == tag::GDEF && !matches!(gdef_bake, GdefBake::Unchanged) {
            continue;
        }
        if rec.tag == tag::GPOS && gpos_baked.is_some() {
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

/// CFF1 (non-variable) instance pass: nothing to bake; rebuild the SFNT
/// directory and optionally drop variable-font tables. CFF1 sources
/// don't carry gvar / HVAR in practice but we honor the same
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
    Ok(InstancedOutput {
        bytes,
        warnings: Vec::new(),
    })
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
/// SinglePos (formats 1 / 2), PairPos (formats 1 / 2), CursivePos,
/// MarkBasePos / MarkLigPos / MarkMarkPos, and Type 9 Extension
/// wrappers around any of those. Mark*/Cursive lookups carry their
/// variations on `Anchor` records (xDevice / yDevice on AnchorFormat
/// 3); those slots resolve against the Anchor, through the same
/// VariationIndex path used for ValueRecord device offsets.
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

// silence clippy warning about unused GlyphId import from lib (kept for
// public surface symmetry with the rest of the crate).
const _: () = {
    let _: Option<GlyphId> = None;
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod vvar_synthetic_tests;

#[cfg(test)]
mod partial_instancing_tests;
