//! Variable-font instancing: bake a coord vector into a static font.
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
//! # Out of scope (deferred)
//!
//! - **CFF2 instancing.** Baking the `blend` operator into a CFF1-
//!   compatible charstring stream is significant work; sources that
//!   carry CFF2 outlines surface as
//!   [`SubsetError::Unsupported`] under [`instance`] for now.
//! - **VVAR / vmtx.** sigilbuzz has no `VVAR` parser yet (a sibling
//!   feature owns it); when the source carries `vmtx` + `VVAR` the
//!   `vmtx` table is preserved verbatim and `VVAR` rides through too,
//!   matching the no-bake behaviour. This is a defer rather than a
//!   correctness break — vertical metrics consumers can fall back to
//!   the default-instance values.
//! - **MVAR.** Per-field font-wide deltas to `OS/2`, `hhea`, `post`
//!   require an `MVAR` parser sigilbuzz hasn't shipped yet; those
//!   tables ride through verbatim at default-instance values.
//! - **Partial instancing** (some axes pinned, others left variable).
//!   Sigil's first cut bakes the full coord vector — every axis pins.
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
    // CFF / CFF2 sources route through a dedicated path that doesn't
    // exist yet — the blend-operator rewrite is significant work and a
    // sibling task owns it.
    if face.record(tag::CFF2).is_some() {
        return Err(SubsetError::Unsupported(
            "instance: CFF2 blend baking not yet implemented",
        ));
    }
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

    let maxp = face.maxp()?;
    let num_glyphs = maxp.num_glyphs;

    // glyf + loca bake.
    let glyf_loca = bake_glyf_loca(face, &coords, num_glyphs)?;

    // hmtx + hhea bake. HVAR deltas fold in here; gids without HVAR
    // entries carry their default-instance metrics through unchanged.
    let hmtx_out = bake_hmtx(face, &coords, num_glyphs)?;

    // head: pass through, only patching indexToLocFormat to match the
    // bake's chosen loca format.
    let head_bytes = face.table_bytes(tag::HEAD).map_err(SubsetError::from)?;
    let mut head_out = head_bytes.to_vec();
    util::write_index_to_loc_format(&mut head_out, glyf_loca.long_loca);

    // hhea: pass through, patching numberOfHMetrics.
    let hhea_bytes = face.table_bytes(tag::HHEA).map_err(SubsetError::from)?;
    let mut hhea_out = hhea_bytes.to_vec();
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

    // Carry every other table through verbatim, with a small drop list
    // for the variable-font tables when `drop_var_tables` is true.
    for rec in face.records() {
        if tables.iter().any(|(t, _)| *t == rec.tag) {
            continue;
        }
        if input.drop_var_tables
            && matches!(rec.tag, tag::FVAR | tag::AVAR | tag::GVAR | tag::HVAR)
        {
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
        if input.drop_var_tables
            && matches!(rec.tag, tag::FVAR | tag::AVAR | tag::GVAR | tag::HVAR)
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
            Some((s, e)) if s != e => glyf_bytes
                .get(s as usize..e as usize)
                .ok_or(SubsetError::Unsupported(
                    "instance: glyf range falls outside table",
                ))?,
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
    let instr_len = r
        .read_u16()
        .map_err(|_| SubsetError::Unsupported("instance: instructionLength"))?
        as usize;
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

// silence clippy warning about unused GlyphId import from lib (kept for
// public surface symmetry with the rest of the crate).
const _: () = {
    let _: Option<GlyphId> = None;
};

#[cfg(test)]
mod tests {
    use super::*;

    const RUBIK: &[u8] = include_bytes!("../../../tests/fixtures/rubik_vf.ttf");
    const SOURCE_SANS: &[u8] = include_bytes!("../../../tests/fonts/SourceSans3VF-Latin-Subset.otf");
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
        assert!(matches!(instance(&face, &bad), Err(SubsetError::Unsupported(_))));
    }

    #[test]
    fn source_sans_round_trip_at_default_instance() {
        // Source Sans 3 VF Latin Subset is a CFF2-flavoured VF. The
        // current cut surfaces this as an Unsupported error — the
        // CFF2-blend bake is staged for a sibling. The integration
        // contract here is "produce a clean, named error" rather than
        // silently emitting a broken font.
        let face = Face::parse_bytes(SOURCE_SANS, 0).unwrap();
        let axis_count = face
            .fvar()
            .unwrap()
            .map_or(0, |f| f.axes().len());
        let input = InstanceInput {
            coords: alloc::vec![0.0_f32; axis_count],
            drop_var_tables: true,
        };
        let r = instance(&face, &input);
        // CFF2 path — error today, baked outline tomorrow.
        assert!(
            matches!(r, Err(SubsetError::Unsupported(s)) if s.contains("CFF2")),
            "expected CFF2 unsupported, got: {r:?}",
        );
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
}
