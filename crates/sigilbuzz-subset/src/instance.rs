//! Variable-font instancing: bake a coord vector into a static font. (#163)
//!
//! Given a [`Face`] and a per-axis normalized coord vector, this module
//! produces a new font where the variable-font deltas have been folded
//! into the underlying glyph outlines and metrics. The result is a
//! static font that consumers without VF awareness (older PDF renderers,
//! legacy print pipelines, test feeds that expect static fonts) can use as
//! though the source had been designed at the chosen instance.
//!
//! # Coordinates
//!
//! The normalized coordinates go through `avar` and land on the
//! F2DOT14 grid, as HarfBuzz and fontTools place them: the instance is
//! baked where a renderer draws the variable font.
//!
//! # What lands on the static side
//!
//! The `glyf` bake follows HarfBuzz's instancer (see the `glyf`
//! submodule):
//!
//! - A simple glyph's points move by their gvar deltas at the
//!   coordinates, the points a tuple skips inferred from those it lists
//!   (IUP). The coordinate streams are re-encoded with the spec's
//!   flag-driven SHORT / SAME compression; hinting instructions are left
//!   out.
//! - A composite glyph's components placed by offset move by their
//!   deltas, and their arguments widen to words when the moved offset
//!   no longer fits a byte. Components placed by matching points keep
//!   their records.
//! - The bounding box in each glyph header is recomputed.
//! - `hmtx` (and `vmtx` when the source has one) takes each glyph's
//!   advance and side bearing from its four phantom points moved by
//!   their deltas, and `head`'s bounding box and the extremes in `hhea`
//!   (and `vhea`) follow from the baked glyphs. A font without `gvar`
//!   keeps its outlines and folds the `HVAR` (and `VVAR`) deltas into
//!   the advances instead.
//!
//! Every value rounds to the nearest unit, halves up, as HarfBuzz and
//! fontTools round.
//!
//! # What gets dropped (or kept verbatim)
//!
//! When [`InstanceInput::drop_var_tables`] is true (the recommended
//! default for the "ship as static" workflow):
//!
//! - `fvar`, `avar`, `gvar`, `HVAR`, `VVAR`, `MVAR` are dropped from
//!   the directory.
//! - `GDEF` keeps every subtable but its `ItemVariationStore`, which is
//!   pruned once the GPOS bake (see below) and the LigCaretList caret
//!   fold have resolved every `VariationIndex` that pointed into it.
//!
//! When `drop_var_tables` is false the variable-font tables ride
//! through verbatim next to the baked outline and metric tables. A
//! consumer that ignores the variable-font tables sees the baked
//! instance. A consumer that also applies them adds the deltas a second
//! time unless `coords` is the default instance.
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
//! # VVAR-aware vmtx and VORG
//!
//! Symmetric to the HVAR/hmtx bake. When the source carries `vmtx` +
//! `VVAR` the per-glyph advance height + tsb deltas resolve at
//! `coords` and fold into the rewritten `vmtx`; `VVAR` is then
//! dropped. Sources without `VVAR` pass `vmtx` through unchanged.
//! When `VVAR` also maps vertical origin deltas, they fold into `VORG`
//! the same way, entries added for glyphs whose origin moves off the
//! default.
//!
//! As in a subset, a malformed vertical table does not fail the
//! instance. A `vhea` or `vmtx` that cannot be read is left out with
//! its partner, a `VVAR` that cannot be read is left out and its
//! deltas are not applied, and a malformed `VORG` is left out; each is
//! reported in [`InstancedOutput::warnings`].
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
//! # Partial instancing
//!
//! When [`InstanceInput::axis_pins`] keeps some axes variable, the
//! bake emits a reduced-axis variable font instead. As in HarfBuzz's
//! instancer, the default of a `glyf` font moves to the pinned location
//! (the kept axes at their defaults): `glyf`, `hmtx`, `vmtx` and `VORG`
//! are baked there, and the gvar tuples and `HVAR` / `VVAR` regions
//! left on the pinned axes only go. `gvar` goes through
//! [`crate::gvar_partial::bake_gvar_partial_with`], which merges tuples
//! that land on the same region, and the CFF2 VarStore through
//! [`crate::cff2::bake_cff2_partial`].
//!
//! # Determinism
//!
//! Output is byte-deterministic for a given input face + coord vector.
//! No `HashMap` iteration touches the output; gids walk in order, the
//! SFNT directory is sorted by tag at emission, and every floating-
//! point value rounds through a fixed rule, so the same inputs always
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
use crate::{SubsetError, SubsetWarning};
use gdef_store::{prune_gdef_store, GdefBake};
use glyf::{bake_glyf_loca, GlyfLocaBake, GlyphMetrics};
use metrics::{
    bake_hmtx, bake_mvar_metrics, bake_vmtx, bake_vorg, hmtx_from_metrics, patch_head_bounds,
    patch_line_extremes, MvarBake, VmtxBake, VorgBake,
};
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
    /// If false, leave them in place next to the baked outlines and
    /// metrics. The file is larger and shapers will still treat the
    /// font as variable. A consumer that applies the variation tables
    /// on top of the baked values adds the deltas a second time unless
    /// `coords` is the default instance.
    ///
    /// Has no effect when [`InstanceInput::axis_pins`] keeps any axis
    /// variable. The output is then still a variable font, so its
    /// trimmed variation tables always stay.
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
    /// zero are dropped. A CFF2 source has its VarStore projected the
    /// same way, and every `blend` is rewritten to the surviving
    /// regions.
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
    /// formed font. At most 65,536 are kept. See [`SubsetWarning`].
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
    let coords = post_avar(face, &input.coords)?;

    if face.record(tag::CFF2).is_some() {
        return cff2_bake(face, input, &coords);
    }

    let warnings = Warnings::default();

    // MVAR-aware bake of OS/2, hhea, post, vhea (when MVAR is present).
    let mvar_bake = bake_mvar_metrics(face, &coords)?;

    // glyf, loca, and the metrics their phantom points give: hmtx,
    // vmtx, and the head, hhea and vhea fields that follow from them.
    // maxp passes through verbatim (instancing keeps every gid).
    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    let glyf_bake = push_glyf_tables(face, &coords, &mvar_bake, &warnings, &mut tables)?;
    let vmtx_bake_result = glyf_bake.vmtx;

    // The VVAR vertical origin deltas fold into VORG.
    let vorg_bake = bake_vorg(face, &coords, glyf_bake.num_glyphs, &warnings);
    if let VorgBake::Rebuilt(b) = &vorg_bake {
        tables.push((tag::VORG, b.clone()));
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
        // A VORG the bake could not read is left out, and so are the
        // vertical metrics and VVAR the vmtx bake could not read.
        if (rec.tag == tag::VORG && vorg_bake == VorgBake::Dropped)
            || vmtx_bake_result.left_out.contains(&rec.tag)
        {
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

/// The post-avar coordinates of the normalized `coords`: mapped through
/// `avar`, then put on the F2DOT14 grid, as HarfBuzz and fontTools
/// place them. A variable font is drawn at those coordinates, so its
/// instance is baked at them.
fn post_avar(face: &Face<'_>, coords: &[f32]) -> Result<Vec<f32>, SubsetError> {
    let mapped = match face.avar().map_err(SubsetError::from)? {
        Some(av) => av.remap_all(coords),
        None => coords.to_vec(),
    };
    Ok(mapped.into_iter().map(snap_f2dot14).collect())
}

/// `v` on the F2DOT14 grid, rounded to nearest (halves up) and clamped to
/// the normalized range. A non-finite value becomes zero.
fn snap_f2dot14(v: f32) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    f32::from(glyf::clamp_i16(util::round_half_up(
        v.clamp(-1.0, 1.0) * 16384.0,
    ))) / 16384.0
}

/// What [`push_glyf_tables`] baked besides the tables it pushed.
struct GlyfTablesBake {
    /// The glyf bake, for the partial instance's `gvar` rewrite.
    glyf: GlyfLocaBake,
    /// The vertical metrics bake, naming the tables it left out.
    vmtx: VmtxBake,
    /// The font's glyph count.
    num_glyphs: u16,
}

/// Bakes the glyphs of a `glyf` font at the post-avar `coords` and
/// pushes `head`, `hhea`, `maxp`, `hmtx`, `loca`, `glyf`, and the
/// vertical metrics onto `tables`.
///
/// With `gvar`, the advances and side bearings come from the baked
/// glyphs' phantom points and bounds, and so do the `head` bounding box
/// and the `hhea` and `vhea` extremes. Without it, the outlines stay,
/// `HVAR` and `VVAR` deltas fold into the advances, and `head` keeps
/// its box. `hhea` and `vhea` start from `mvar_bake`'s copies when
/// `MVAR` varies them.
fn push_glyf_tables(
    face: &Face<'_>,
    coords: &[f32],
    mvar_bake: &MvarBake,
    warnings: &Warnings,
    tables: &mut Vec<([u8; 4], Vec<u8>)>,
) -> Result<GlyfTablesBake, SubsetError> {
    let num_glyphs = face.maxp()?.num_glyphs;
    let glyf_loca = bake_glyf_loca(face, coords, num_glyphs, warnings)?;
    let baked = glyf_loca.metrics.as_deref();
    let hmtx_out = match baked {
        Some(m) => hmtx_from_metrics(m),
        None => bake_hmtx(face, coords, num_glyphs)?,
    };
    let vmtx = bake_vmtx(face, coords, num_glyphs, baked, warnings);

    // head: the loca format the bake chose, and the new bounding box.
    let mut head_out = face
        .table_bytes(tag::HEAD)
        .map_err(SubsetError::from)?
        .to_vec();
    util::write_index_to_loc_format(&mut head_out, glyf_loca.long_loca);
    // hhea: the long metrics count the new hmtx needs, and its extremes.
    let mut hhea_out = match mvar_bake.hhea.clone() {
        Some(bytes) => bytes,
        None => face
            .table_bytes(tag::HHEA)
            .map_err(SubsetError::from)?
            .to_vec(),
    };
    util::write_hhea_metrics_count(&mut hhea_out, hmtx_out.number_of_h_metrics)?;
    if let Some(m) = baked {
        patch_head_bounds(&mut head_out, m);
        patch_line_extremes(&mut hhea_out, m, false);
    }
    let maxp_out = face
        .table_bytes(tag::MAXP)
        .map_err(SubsetError::from)?
        .to_vec();
    tables.extend([
        (tag::HEAD, head_out),
        (tag::HHEA, hhea_out),
        (tag::MAXP, maxp_out),
        (tag::HMTX, hmtx_out.bytes),
        (tag::LOCA, glyf_loca.loca.clone()),
        (tag::GLYF, glyf_loca.glyf.clone()),
    ]);
    push_vertical_metrics(face, &vmtx, mvar_bake, baked, tables)?;
    Ok(GlyfTablesBake {
        glyf: glyf_loca,
        vmtx,
        num_glyphs,
    })
}

/// Appends the rebuilt `vmtx` with `vhea` (MVAR-baked when `MVAR`
/// varies it) patched to its `numberOfLongVerMetrics`, which may
/// extend the long range to cover VVAR-induced trailing-advance
/// differences, and, when `baked` holds the baked glyphs' metrics, to
/// their extremes. Without a rebuilt `vmtx`, appends an MVAR-baked
/// `vhea` unless the bake left `vhea` out; an unbaked one rides through
/// with the other tables.
fn push_vertical_metrics(
    face: &Face<'_>,
    vmtx_bake: &VmtxBake,
    mvar_bake: &MvarBake,
    baked: Option<&[GlyphMetrics]>,
    tables: &mut Vec<([u8; 4], Vec<u8>)>,
) -> Result<(), SubsetError> {
    if let Some(vmtx_bytes) = vmtx_bake.vmtx_bytes.clone() {
        tables.push((tag::VMTX, vmtx_bytes));
        let mut vhea_out = match mvar_bake.vhea.clone() {
            Some(bytes) => bytes,
            None => face
                .table_bytes(tag::VHEA)
                .map_err(SubsetError::from)?
                .to_vec(),
        };
        util::write_vhea_metrics_count(&mut vhea_out, vmtx_bake.number_of_long_ver_metrics)?;
        if let Some(m) = baked {
            patch_line_extremes(&mut vhea_out, m, true);
        }
        tables.push((tag::VHEA, vhea_out));
    } else if !vmtx_bake.left_out.contains(&tag::VHEA) {
        if let Some(vhea_bytes) = mvar_bake.vhea.clone() {
            tables.push((tag::VHEA, vhea_bytes));
        }
    }
    Ok(())
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
    let vmtx_bake_result = bake_vmtx(face, coords, num_glyphs, None, &warnings);
    let vorg_bake = bake_vorg(face, coords, num_glyphs, &warnings);
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
    push_vertical_metrics(face, &vmtx_bake_result, &mvar_bake, None, &mut tables)?;
    if let VorgBake::Rebuilt(b) = &vorg_bake {
        tables.push((tag::VORG, b.clone()));
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
        if (rec.tag == tag::VORG && vorg_bake == VorgBake::Dropped)
            || vmtx_bake_result.left_out.contains(&rec.tag)
        {
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

#[cfg(test)]
mod tests;

#[cfg(test)]
mod vvar_synthetic_tests;

#[cfg(test)]
mod partial_instancing_tests;

#[cfg(test)]
mod robustness_tests;
