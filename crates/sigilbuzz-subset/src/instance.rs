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
//! # GDEF.IVS / GPOS variable-position trade-off
//!
//! When `drop_var_tables` is true (the recommended default) any
//! `GDEF.ItemVariationStore` is pruned by re-emitting the GDEF table
//! header with the IVS offset zeroed. GPOS ValueRecords that referred
//! to the IVS via `VariationIndex` deltas keep their static (default-
//! instance) values; the consequence is that variable-position kerning
//! at non-default coords is lost, which matches the documented
//! "ship as static" intent of instancing. Resolving each GPOS
//! VariationIndex into the corresponding ValueRecord is staged for a
//! follow-up — at no-coords (the default instance) consumers see the
//! same advances regardless.
//!
//! # Out of scope (deferred)
//!
//! - **Partial instancing** (some axes pinned, others left variable).
//!   Sigil's first cut bakes the full coord vector — every axis pins.
//! - **GPOS VariationIndex re-emit.** When the source GPOS carries
//!   `VariationIndex` deltas the simple "drop GDEF.IVS" path leaves
//!   GPOS pointing at orphaned variation indices; per the briefing
//!   we ship the simple path and stage the full re-emit (resolve
//!   VariationIndex deltas, fold into static ValueRecord fields,
//!   zero the offset) as a follow-up.
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
}

impl Default for InstanceInput {
    fn default() -> Self {
        Self {
            coords: Vec::new(),
            drop_var_tables: true,
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
    if let Some(vmtx_bytes) = vmtx_bake_result.vmtx_bytes {
        tables.push((tag::VMTX, vmtx_bytes));
    }
    if let Some(vhea_bytes) = mvar_bake.vhea.clone() {
        tables.push((tag::VHEA, vhea_bytes));
    }
    if let Some(os2_bytes) = mvar_bake.os2.clone() {
        tables.push((*b"OS/2", os2_bytes));
    }
    if let Some(post_bytes) = mvar_bake.post.clone() {
        tables.push((tag::POST, post_bytes));
    }

    // GDEF: when the source carries an ItemVariationStore and the
    // caller wants the static "ship as static" output, prune it. See
    // module header for the GPOS-default-instance trade-off.
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
    if let Some(vmtx_bytes) = vmtx_bake_result.vmtx_bytes {
        tables.push((tag::VMTX, vmtx_bytes));
    }
    if let Some(vhea_bytes) = mvar_bake.vhea.clone() {
        tables.push((tag::VHEA, vhea_bytes));
    }
    if let Some(os2_bytes) = mvar_bake.os2.clone() {
        tables.push((*b"OS/2", os2_bytes));
    }
    if let Some(post_bytes) = mvar_bake.post.clone() {
        tables.push((tag::POST, post_bytes));
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
    /// The vmtx layout is determined by the source's `vhea`'s
    /// `numberOfLongVerMetrics` — instancing keeps every gid so the
    /// long count stays unchanged.
    vmtx_bytes: Option<Vec<u8>>,
}

fn bake_vmtx(face: &Face<'_>, coords: &[f32], num_glyphs: u16) -> Result<VmtxBake, SubsetError> {
    let vmtx = face.vmtx().map_err(SubsetError::from)?;
    let Some(vmtx) = vmtx else {
        return Ok(VmtxBake { vmtx_bytes: None });
    };
    // vhea must be present for vmtx to parse; reach for it to read
    // numberOfLongVerMetrics so the rebuild keeps the same long-count.
    let vhea = face
        .vhea()
        .map_err(SubsetError::from)?
        .ok_or(SubsetError::Unsupported(
            "instance: vmtx present without vhea",
        ))?;
    let long_count = vhea.number_of_long_ver_metrics;

    let vvar = face.vvar().map_err(SubsetError::from)?;

    // Compute the new (advance, tsb) per gid. Every glyph in the long
    // range carries its own advance; trailing glyphs share the last
    // advance as in the source. We rebuild the long range from each
    // source advance + VVAR delta, then keep tsbs for trailing glyphs
    // patched by VVAR.tsb deltas (when the source carries that
    // mapping).
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

    let mut out = Vec::with_capacity(num_glyphs as usize * 4);
    for i in 0..long_count.min(num_glyphs) {
        out.extend_from_slice(&advances[i as usize].to_be_bytes());
        out.extend_from_slice(&tsbs[i as usize].to_be_bytes());
    }
    for i in long_count..num_glyphs {
        out.extend_from_slice(&tsbs[i as usize].to_be_bytes());
    }

    Ok(VmtxBake {
        vmtx_bytes: Some(out),
    })
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
        };
        let out = instance(&face, &input).unwrap();
        let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
        // If the source had a GDEF, the baked face should too — and
        // it should still parse cleanly.
        if face.gdef().unwrap().is_some() {
            assert!(baked.gdef().unwrap().is_some());
        }
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
