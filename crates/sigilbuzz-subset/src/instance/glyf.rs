//! The glyf and loca bake of an instance: every glyph moved by its
//! gvar deltas at the instance coordinates, and the metrics its
//! phantom points give, the way HarfBuzz's instancer bakes them.
//!
//! - A simple glyph's points move by their deltas, with the points a
//!   tuple skips inferred from the points it lists (IUP, see
//!   [`sigilbuzz::tables::Gvar::glyph_point_deltas`]). The points are
//!   rounded to whole units and the coordinate streams re-encoded.
//! - A composite glyph's gvar points are its components, one each: a
//!   component placed by offset gets its delta added to the offset,
//!   rounded, and widened to 16-bit arguments when it no longer fits a
//!   byte. A component placed by matching points keeps its record: the
//!   points it matches have already moved.
//! - Every glyph's four phantom points (left side bearing origin,
//!   advance origin, top origin, bottom origin) start from `hmtx`,
//!   `vmtx`, and the source glyph header, and move by their deltas.
//!   The advance is the distance between the first two, the left side
//!   bearing the distance from the first to the baked `xMin`, and the
//!   vertical metrics follow the same way from the last two. A
//!   composite takes its own phantom points, as HarfBuzz's instancer
//!   does, not those of a `USE_MY_METRICS` component.
//! - The bounding box of a simple glyph comes from its baked points,
//!   that of a composite from its outline drawn at the instance
//!   coordinates.
//!
//! Every value rounds to the nearest unit, halves up, as HarfBuzz and
//! fontTools round.
//!
//! A glyph whose variation data cannot be read keeps its default
//! outline and metrics, and the instance reports it in its warnings. A
//! composite whose outline cannot be drawn keeps its source bounding
//! box the same way.

use alloc::vec::Vec;

use sigilbuzz::tables::{tag, Glyf, Gvar, Hmtx, Loca, Reader, Vmtx};
use sigilbuzz::Face;

use crate::gvar_partial::GlyphPoints;
use crate::util::round_half_up;
use crate::warnings::Warnings;
use crate::SubsetError;
use extents::{Extents, NoExtent};

// ---------------------------------------------------------------------------
// glyf + loca bake
// ---------------------------------------------------------------------------

/// The metrics of one baked glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GlyphMetrics {
    /// Advance width: phantom point 2 x minus phantom point 1 x.
    pub(super) advance: u16,
    /// Left side bearing: `xMin` minus phantom point 1 x.
    pub(super) lsb: i16,
    /// Advance height: phantom point 3 y minus phantom point 4 y.
    pub(super) v_advance: u16,
    /// Top side bearing: phantom point 3 y minus `yMax`.
    pub(super) tsb: i16,
    /// `(xMin, yMin, xMax, yMax)`, or `None` for a glyph with no
    /// outline.
    pub(super) bounds: Option<[i16; 4]>,
}

pub(super) struct GlyfLocaBake {
    pub(super) glyf: Vec<u8>,
    pub(super) loca: Vec<u8>,
    pub(super) long_loca: bool,
    /// Every glyph's metrics, in glyph order, from its varied phantom
    /// points. `None` when the source has no `gvar`: nothing moves the
    /// outlines, and the advances come from `HVAR` and `VVAR` instead.
    pub(super) metrics: Option<Vec<GlyphMetrics>>,
    /// Extents worked out and composites drawn through the core walk,
    /// for the tests that bound the bake's work.
    #[cfg(test)]
    pub(super) extent_work: (u64, u64),
}

/// The tables one glyph bake reads.
struct BakeCtx<'a, 'f> {
    loca: &'a Loca<'f>,
    glyf_bytes: &'f [u8],
    glyf: Glyf<'f>,
    gvar: Option<&'a Gvar<'f>>,
    coords: &'a [f32],
    hmtx: &'a Hmtx<'f>,
    vmtx: Option<&'a Vmtx<'f>>,
    warnings: &'a Warnings,
    /// The extents of the glyphs composites draw, each worked out once.
    extents: Extents,
}

/// Bakes every glyph of `face` at the post-avar `coords`.
pub(super) fn bake_glyf_loca(
    face: &Face<'_>,
    coords: &[f32],
    num_glyphs: u16,
    warnings: &Warnings,
) -> Result<GlyfLocaBake, SubsetError> {
    let loca = face.loca().map_err(SubsetError::from)?;
    let glyf_bytes = face.table_bytes(tag::GLYF).map_err(SubsetError::from)?;
    let glyf = face.glyf().map_err(SubsetError::from)?;
    let gvar = face.gvar().map_err(SubsetError::from)?;
    let hmtx = face.hmtx().map_err(SubsetError::from)?;
    // A `vmtx` that cannot be read only loses the vertical phantom
    // points; the vertical metrics bake reports it.
    let vmtx = face.vmtx().ok().flatten();
    let cx = BakeCtx {
        loca: &loca,
        glyf_bytes,
        glyf,
        gvar: gvar.as_ref(),
        coords,
        hmtx: &hmtx,
        vmtx: vmtx.as_ref(),
        warnings,
        extents: Extents::new(num_glyphs),
    };

    let mut new_bodies: Vec<Vec<u8>> = Vec::with_capacity(num_glyphs as usize);
    let mut metrics: Vec<GlyphMetrics> = Vec::with_capacity(num_glyphs as usize);
    for gid in 0..num_glyphs {
        let (body, m) = bake_glyph(&cx, gid)?;
        new_bodies.push(body);
        metrics.push(m);
    }

    let (glyf_out, loca_out, long_loca) = assemble_glyf_loca(new_bodies)?;
    Ok(GlyfLocaBake {
        glyf: glyf_out,
        loca: loca_out,
        long_loca,
        metrics: gvar.is_some().then_some(metrics),
        #[cfg(test)]
        extent_work: (cx.extents.computed.get(), cx.extents.drawn.get()),
    })
}

impl GlyfLocaBake {
    /// The baked `glyf` body of glyph `gid`, empty for a glyph with no
    /// outline or past the end.
    fn body(&self, gid: u16) -> &[u8] {
        let i = usize::from(gid);
        let offset = |k: usize| -> Option<usize> {
            if self.long_loca {
                let b = self.loca.get(k * 4..k * 4 + 4)?;
                Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
            } else {
                let b = self.loca.get(k * 2..k * 2 + 2)?;
                Some(usize::from(u16::from_be_bytes([b[0], b[1]])) * 2)
            }
        };
        offset(i)
            .zip(offset(i + 1))
            .and_then(|(s, e)| self.glyf.get(s..e))
            .unwrap_or_default()
    }

    /// The gvar points of glyph `gid`, whose source body is `src_body`:
    /// its contour points or its components' offsets in the source, and
    /// for a simple glyph whose points this bake moved, the moved
    /// points. `None` when the glyph cannot be read.
    pub(super) fn glyph_points(&self, src_body: &[u8], gid: u16) -> Option<GlyphPoints> {
        let nc = src_body
            .first_chunk::<10>()
            .map_or(0, |h| i16::from_be_bytes([h[0], h[1]]));
        if nc == 0 {
            // No outline (see `bake_glyph`): the phantom points only.
            return Some(GlyphPoints {
                end_pts: Vec::new(),
                before: Vec::new(),
                after: None,
            });
        }
        if nc < 0 {
            let components = read_component_records(src_body).ok()?;
            return Some(GlyphPoints {
                end_pts: Vec::new(),
                before: components.iter().map(CompRecord::gvar_point).collect(),
                after: None,
            });
        }
        let before = SimpleGlyph::decode(src_body).ok()?;
        let after = SimpleGlyph::decode(self.body(gid)).ok()?;
        let moved = before.xs != after.xs || before.ys != after.ys;
        Some(GlyphPoints {
            after: moved.then(|| after.points()),
            before: before.points(),
            end_pts: before.end_pts,
        })
    }
}

/// Lays the glyph bodies out into `glyf`, each padded to two bytes,
/// and writes the `loca` that indexes them, short when the offsets
/// allow. Returns `(glyf, loca, long_loca)`.
fn assemble_glyf_loca(mut bodies: Vec<Vec<u8>>) -> Result<(Vec<u8>, Vec<u8>, bool), SubsetError> {
    // Pad each body to 2-byte alignment so short-loca offsets divide
    // cleanly.
    for body in &mut bodies {
        if body.len() % 2 != 0 {
            body.push(0);
        }
    }

    // Compute offsets and decide loca format.
    let mut offsets: Vec<u32> = Vec::with_capacity(bodies.len() + 1);
    let mut cursor: u32 = 0;
    offsets.push(0);
    for body in &bodies {
        cursor = u32::try_from(body.len())
            .ok()
            .and_then(|len| cursor.checked_add(len))
            .ok_or(SubsetError::Unsupported("instance: glyf overflow"))?;
        offsets.push(cursor);
    }
    let long_loca = *offsets.last().unwrap_or(&0) > 0x1_FFFE;

    let mut glyf_out = Vec::with_capacity(cursor as usize);
    for body in &bodies {
        glyf_out.extend_from_slice(body);
    }
    while glyf_out.len() % 4 != 0 {
        glyf_out.push(0);
    }

    let loca_out = if long_loca {
        offsets.iter().flat_map(|o| o.to_be_bytes()).collect()
    } else {
        offsets
            .iter()
            .flat_map(|o| ((*o / 2) as u16).to_be_bytes())
            .collect()
    };
    Ok((glyf_out, loca_out, long_loca))
}

/// Bakes glyph `gid`: its new `glyf` body and its metrics.
fn bake_glyph(cx: &BakeCtx<'_, '_>, gid: u16) -> Result<(Vec<u8>, GlyphMetrics), SubsetError> {
    let body = match cx.loca.range(gid) {
        Some((s, e)) if s != e => {
            cx.glyf_bytes
                .get(s as usize..e as usize)
                .ok_or(SubsetError::Unsupported(
                    "instance: glyf range falls outside table",
                ))?
        }
        _ => &[][..],
    };
    // A body too short for a header is no outline, and HarfBuzz reads
    // a zero header for it; a header with no contours is no outline
    // either. Only the phantom points move, from the header's box.
    let (nc, header_x_min, header_y_max) = match body.first_chunk::<10>() {
        Some(h) => (
            i16::from_be_bytes([h[0], h[1]]),
            i16::from_be_bytes([h[2], h[3]]),
            i16::from_be_bytes([h[8], h[9]]),
        ),
        None => (0, 0, 0),
    };
    if nc == 0 {
        let deltas = cx.deltas(gid, &[], &[]);
        let pp = cx.phantoms(gid, header_x_min, header_y_max, &deltas);
        return Ok((Vec::new(), metrics_from(&pp, None)));
    }
    if nc > 0 {
        let glyph = SimpleGlyph::decode(body)?;
        let points = glyph.points();
        let deltas = cx.deltas(gid, &points, &glyph.end_pts);
        let (baked, bounds) = encode_baked_simple(&glyph, &deltas);
        let pp = cx.phantoms(gid, header_x_min, header_y_max, &deltas[points.len()..]);
        Ok((baked, metrics_from(&pp, bounds)))
    } else {
        let components = read_component_records(body)?;
        let points: Vec<(i32, i32)> = components.iter().map(CompRecord::gvar_point).collect();
        let deltas = cx.deltas(gid, &points, &[]);
        let mut out = rewrite_components(body, &components, &deltas);
        let start = cx.loca.range(gid).map_or(0, |(s, _)| s as usize);
        let bounds = cx.composite_bounds(gid, body, start);
        // Header: the source contour count, then the new bbox.
        if let Some(b) = bounds {
            for (i, v) in b.iter().enumerate() {
                out[2 + 2 * i..4 + 2 * i].copy_from_slice(&v.to_be_bytes());
            }
        } else {
            out[2..10].fill(0);
        }
        let pp = cx.phantoms(gid, header_x_min, header_y_max, &deltas[points.len()..]);
        Ok((out, metrics_from(&pp, bounds)))
    }
}

impl BakeCtx<'_, '_> {
    /// The deltas of every point of `gid` at the instance coordinates,
    /// inferred points included, then its four phantom points: `points`
    /// plus four entries. All zero when the font has no `gvar`, and,
    /// reported in the warnings, when the glyph's data cannot be read.
    fn deltas(&self, gid: u16, points: &[(i32, i32)], end_pts: &[u16]) -> Vec<(f32, f32)> {
        let zeros = || alloc::vec![(0.0, 0.0); points.len() + 4];
        let Some(gvar) = self.gvar else {
            return zeros();
        };
        if self.coords.iter().all(|&c| c == 0.0) {
            return zeros();
        }
        match gvar.glyph_point_deltas(gid, self.coords, points, end_pts) {
            Ok(d) if d.len() == points.len() + 4 => d,
            Ok(_) => zeros(),
            Err(e) => {
                self.warnings
                    .parse_error(tag::GVAR, 0, &e, "the glyph's variations");
                zeros()
            }
        }
    }

    /// The four phantom points of `gid` moved by `deltas` (at least
    /// four entries; the first four are used). The defaults follow
    /// HarfBuzz: `xMin` minus the left side bearing, plus the advance,
    /// then `yMax` plus the top side bearing, minus the advance height.
    fn phantoms(&self, gid: u16, x_min: i16, y_max: i16, deltas: &[(f32, f32)]) -> [(f32, f32); 4] {
        let lsb = self.hmtx.lsb(gid).unwrap_or(x_min);
        let advance = self.hmtx.advance(gid).unwrap_or(0);
        let h_origin = i32::from(x_min) - i32::from(lsb);
        let (top, bottom) = match self.vmtx {
            Some(vmtx) => {
                let tsb = vmtx.tsb(gid).unwrap_or(0);
                let top = i32::from(y_max) + i32::from(tsb);
                (top, top - i32::from(vmtx.advance(gid).unwrap_or(0)))
            }
            None => (0, 0),
        };
        let mut pp = [
            (h_origin as f32, 0.0),
            ((h_origin + i32::from(advance)) as f32, 0.0),
            (0.0, top as f32),
            (0.0, bottom as f32),
        ];
        for (p, d) in pp.iter_mut().zip(deltas) {
            p.0 += d.0;
            p.1 += d.1;
        }
        pp
    }

    /// The bounding box of composite `gid` drawn at the instance
    /// coordinates, rounded (see [`extents`]). A composite that cannot
    /// be drawn keeps the box in its source header `body`, which starts
    /// `start` bytes into `glyf`, and is reported.
    fn composite_bounds(&self, gid: u16, body: &[u8], start: usize) -> Option<[i16; 4]> {
        match self.extents.of(self, gid) {
            Ok(extent) => extents::rounded(extent.bounds),
            Err(no) => {
                let e = match no {
                    NoExtent::Broken(e) => e,
                    NoExtent::TooDeep => sigilbuzz::Error::Malformed {
                        offset: 0,
                        context: "glyf composite recursion exceeded cap",
                    },
                };
                self.warnings.parse_error(
                    tag::GLYF,
                    start,
                    &e,
                    "the composite glyph's new bounding box",
                );
                let at = |i: usize| i16::from_be_bytes([body[i], body[i + 1]]);
                Some([at(2), at(4), at(6), at(8)])
            }
        }
    }
}

/// The metrics phantom points `pp` give a glyph with `bounds`, as
/// HarfBuzz's instancer computes them: an empty glyph measures its
/// bearings from zero.
fn metrics_from(pp: &[(f32, f32); 4], bounds: Option<[i16; 4]>) -> GlyphMetrics {
    let [x_min, _, _, y_max] = bounds.unwrap_or([0; 4]);
    let advance = round_half_up(pp[1].0 - pp[0].0).clamp(0, i32::from(u16::MAX)) as u16;
    let v_advance = round_half_up(pp[2].1 - pp[3].1).clamp(0, i32::from(u16::MAX)) as u16;
    GlyphMetrics {
        advance,
        lsb: clamp_i16(round_half_up(f32::from(x_min) - pp[0].0)),
        v_advance,
        tsb: clamp_i16(round_half_up(pp[2].1 - f32::from(y_max))),
        bounds,
    }
}

// ---------------------------------------------------------------------------
// Simple glyphs
// ---------------------------------------------------------------------------

// Simple-glyph flag bits.
pub(super) const FLAG_ON_CURVE: u8 = 0x01;
const FLAG_X_SHORT: u8 = 0x02;
const FLAG_Y_SHORT: u8 = 0x04;
pub(super) const FLAG_REPEAT: u8 = 0x08;
pub(super) const FLAG_X_SAME_OR_POS: u8 = 0x10;
pub(super) const FLAG_Y_SAME_OR_POS: u8 = 0x20;
/// `OVERLAP_SIMPLE` and the cubic bit: kept on every point, as
/// HarfBuzz keeps them, with the on-curve bit.
const FLAG_OVERLAP_AND_CUBIC: u8 = 0xC0;

/// A decoded simple glyph: its contours and absolute points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SimpleGlyph {
    /// `numberOfContours`.
    pub(crate) num_contours: i16,
    /// `endPtsOfContours`.
    pub(crate) end_pts: Vec<u16>,
    /// One flag byte per point, REPEAT runs expanded.
    pub(crate) flags: Vec<u8>,
    /// Absolute x of every point.
    pub(crate) xs: Vec<i32>,
    /// Absolute y of every point.
    pub(crate) ys: Vec<i32>,
    /// The hinting instructions.
    pub(crate) instructions: Vec<u8>,
}

impl SimpleGlyph {
    /// Decodes the simple glyph `body`, header included.
    pub(crate) fn decode(body: &[u8]) -> Result<Self, SubsetError> {
        let mut r = Reader::new(body);
        let nc = r
            .read_i16()
            .map_err(|_| SubsetError::Unsupported("instance: simple header"))?;
        // Skip the source bbox.
        r.skip(8)
            .map_err(|_| SubsetError::Unsupported("instance: simple bbox"))?;

        let count = usize::try_from(nc).unwrap_or(0);
        let mut end_pts = Vec::with_capacity(count);
        for _ in 0..count {
            end_pts.push(
                r.read_u16()
                    .map_err(|_| SubsetError::Unsupported("instance: endPtsOfContours"))?,
            );
        }
        let total_points = end_pts
            .last()
            .copied()
            .map(|e| usize::from(e) + 1)
            .unwrap_or(0);

        // Instructions: kept, as HarfBuzz's instancer keeps them unless
        // asked to drop hinting, with the `cvt`, `fpgm` and `prep` they
        // run against.
        let instr_len = r
            .read_u16()
            .map_err(|_| SubsetError::Unsupported("instance: instructionLength"))?
            as usize;
        let instructions = r
            .read_bytes(instr_len)
            .map_err(|_| SubsetError::Unsupported("instance: instructions"))?
            .to_vec();

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
                    if flags.len() >= total_points {
                        break;
                    }
                    flags.push(f);
                }
            }
        }

        let xs = read_coords(
            &mut r,
            &flags,
            FLAG_X_SHORT,
            FLAG_X_SAME_OR_POS,
            "instance: x",
        )?;
        let ys = read_coords(
            &mut r,
            &flags,
            FLAG_Y_SHORT,
            FLAG_Y_SAME_OR_POS,
            "instance: y",
        )?;
        Ok(Self {
            num_contours: nc,
            end_pts,
            flags,
            xs,
            ys,
            instructions,
        })
    }

    /// The glyph's points as `(x, y)` pairs.
    pub(crate) fn points(&self) -> Vec<(i32, i32)> {
        self.xs
            .iter()
            .copied()
            .zip(self.ys.iter().copied())
            .collect()
    }
}

/// Reads one delta-encoded coordinate stream and returns the absolute
/// coordinates.
fn read_coords(
    r: &mut Reader<'_>,
    flags: &[u8],
    short_bit: u8,
    same_bit: u8,
    ctx: &'static str,
) -> Result<Vec<i32>, SubsetError> {
    let mut out = Vec::with_capacity(flags.len());
    let mut cur: i32 = 0;
    for &f in flags {
        let delta: i32 = if f & short_bit != 0 {
            let v = i32::from(r.read_u8().map_err(|_| SubsetError::Unsupported(ctx))?);
            if f & same_bit != 0 {
                v
            } else {
                -v
            }
        } else if f & same_bit != 0 {
            0
        } else {
            i32::from(r.read_i16().map_err(|_| SubsetError::Unsupported(ctx))?)
        };
        cur = cur.wrapping_add(delta);
        out.push(cur);
    }
    Ok(out)
}

/// Re-encodes `glyph` with `deltas` (one per point, in point order;
/// extra entries are ignored, missing ones count as zero) added and
/// rounded. Returns the body, instructions included, and its bounding
/// box, `None` when it has no points.
fn encode_baked_simple(glyph: &SimpleGlyph, deltas: &[(f32, f32)]) -> (Vec<u8>, Option<[i16; 4]>) {
    let mut xs: Vec<i32> = Vec::with_capacity(glyph.xs.len());
    let mut ys: Vec<i32> = Vec::with_capacity(glyph.ys.len());
    for (i, (&x, &y)) in glyph.xs.iter().zip(&glyph.ys).enumerate() {
        let (dx, dy) = deltas.get(i).copied().unwrap_or((0.0, 0.0));
        xs.push(round_half_up(x as f32 + dx));
        ys.push(round_half_up(y as f32 + dy));
    }
    let bounds = points_bounds(&xs, &ys);
    let body = encode_simple(glyph, &xs, &ys, bounds);
    (body, bounds)
}

/// The rounded-coordinate bounding box of a point set, `None` when it
/// is empty.
fn points_bounds(xs: &[i32], ys: &[i32]) -> Option<[i16; 4]> {
    let x_min = xs.iter().copied().min()?;
    let x_max = xs.iter().copied().max()?;
    let y_min = ys.iter().copied().min()?;
    let y_max = ys.iter().copied().max()?;
    Some([
        clamp_i16(x_min),
        clamp_i16(y_min),
        clamp_i16(x_max),
        clamp_i16(y_max),
    ])
}

/// Encodes a simple glyph with `glyph`'s contours, instructions and
/// flags at the points `xs` / `ys`, with header bounds `bounds` (zero
/// when `None`).
pub(crate) fn encode_simple(
    glyph: &SimpleGlyph,
    xs: &[i32],
    ys: &[i32],
    bounds: Option<[i16; 4]>,
) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(12 + glyph.end_pts.len() * 2 + glyph.instructions.len() + xs.len() * 5);
    out.extend_from_slice(&glyph.num_contours.to_be_bytes());
    for v in bounds.unwrap_or([0; 4]) {
        out.extend_from_slice(&v.to_be_bytes());
    }
    for &e in &glyph.end_pts {
        out.extend_from_slice(&e.to_be_bytes());
    }
    // Instructions longer than 65,535 bytes cannot have been read.
    out.extend_from_slice(&(glyph.instructions.len() as u16).to_be_bytes());
    out.extend_from_slice(&glyph.instructions);
    encode_simple_coords(xs, ys, &glyph.flags, &mut out);
    out
}

/// Re-encodes a simple glyph `body` with `deltas` (one per point, in
/// point order) applied to its contour points. The new bbox is
/// recomputed from the baked coordinates.
#[cfg(test)]
pub(super) fn bake_simple_glyph(
    body: &[u8],
    deltas: &[(f32, f32)],
) -> Result<Vec<u8>, SubsetError> {
    let glyph = SimpleGlyph::decode(body)?;
    Ok(encode_baked_simple(&glyph, deltas).0)
}

/// Encodes the flags + x + y streams for a simple glyph, using the
/// SHORT / SAME-OR-POS encoding the spec defines. Each flag byte
/// keeps the on-curve, overlap and cubic bits of the input flag
/// stream and takes the rest from the per-point delta size; REPEAT
/// runs collapse identical adjacent flag bytes.
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
        let dx = xs[i].wrapping_sub(prev_x);
        let dy = ys[i].wrapping_sub(prev_y);
        prev_x = xs[i];
        prev_y = ys[i];

        let mut f = src_flags.get(i).copied().unwrap_or(FLAG_ON_CURVE)
            & (FLAG_ON_CURVE | FLAG_OVERLAP_AND_CUBIC);
        f |= encode_coord(dx, FLAG_X_SHORT, FLAG_X_SAME_OR_POS, &mut x_payload);
        f |= encode_coord(dy, FLAG_Y_SHORT, FLAG_Y_SAME_OR_POS, &mut y_payload);
        new_flags.push(f);
    }

    // RLE-compress flags. A run of up to 256 identical flag bytes
    // collapses to `(flag | REPEAT) + count_byte`. Single occurrences
    // emit unchanged.
    let mut i = 0;
    while i < new_flags.len() {
        let f = new_flags[i];
        let mut j = i + 1;
        while j < new_flags.len() && new_flags[j] == f && (j - i) < 256 {
            j += 1;
        }
        let run = j - i;
        if run >= 2 {
            out.push(f | FLAG_REPEAT);
            out.push((run - 1) as u8);
        } else {
            out.push(f);
        }
        i = j;
    }

    write_coord_stream(
        &new_flags,
        &x_payload,
        FLAG_X_SHORT,
        FLAG_X_SAME_OR_POS,
        out,
    );
    write_coord_stream(
        &new_flags,
        &y_payload,
        FLAG_Y_SHORT,
        FLAG_Y_SAME_OR_POS,
        out,
    );
}

/// Chooses the encoding of one coordinate delta: returns the flag bits
/// it needs and queues its payload, the magnitude for a SHORT entry or
/// the value for a 16-bit one.
fn encode_coord(d: i32, short_bit: u8, same_bit: u8, payload: &mut Vec<i16>) -> u8 {
    if d == 0 {
        same_bit
    } else if (-255..=255).contains(&d) {
        payload.push(d.unsigned_abs() as i16);
        if d > 0 {
            short_bit | same_bit
        } else {
            short_bit
        }
    } else {
        // i16 range. Clamp to keep the encoding well-defined; baked
        // coordinates stay within a few units of the source's, which
        // already fit.
        payload.push(clamp_i16(d));
        0
    }
}

/// Writes one coordinate stream: a byte per SHORT entry, nothing per
/// SAME entry, and an `i16` per other entry.
fn write_coord_stream(
    flags: &[u8],
    payload: &[i16],
    short_bit: u8,
    same_bit: u8,
    out: &mut Vec<u8>,
) {
    let mut values = payload.iter();
    for &f in flags {
        if f & short_bit != 0 {
            if let Some(&v) = values.next() {
                out.push(v as u8);
            }
        } else if f & same_bit == 0 {
            if let Some(&v) = values.next() {
                out.extend_from_slice(&v.to_be_bytes());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Composite glyphs
// ---------------------------------------------------------------------------

// Composite-glyph flag bits.
const COMP_ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
const COMP_ARGS_ARE_XY_VALUES: u16 = 0x0002;
const COMP_WE_HAVE_A_SCALE: u16 = 0x0008;
const COMP_MORE_COMPONENTS: u16 = 0x0020;
const COMP_WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
const COMP_WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
const COMP_SCALED_COMPONENT_OFFSET: u16 = 0x0800;
const COMP_UNSCALED_COMPONENT_OFFSET: u16 = 0x1000;

/// One component record of a composite glyph body.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CompRecord {
    /// Offset of the record's flags in the body.
    start: usize,
    /// Offset just past the record.
    end: usize,
    /// The record's flags.
    flags: u16,
    /// The glyph the component draws.
    glyph: u16,
    /// `arg1`: the x offset, or in anchor mode the parent's point.
    arg1: i32,
    /// `arg2`: the y offset, or in anchor mode the component's point.
    arg2: i32,
    /// The 2x2 as the core walk reads it: `[xx, yx, xy, yy]`, where
    /// `x' = xx * x + xy * y` and `y' = yx * x + yy * y`.
    matrix: [f32; 4],
}

impl CompRecord {
    /// True when the arguments are point numbers to match rather than
    /// an offset.
    const fn is_anchored(&self) -> bool {
        self.flags & COMP_ARGS_ARE_XY_VALUES == 0
    }

    /// The component's gvar point: its offset, or the origin for an
    /// anchored component.
    pub(crate) const fn gvar_point(&self) -> (i32, i32) {
        if self.is_anchored() {
            (0, 0)
        } else {
            (self.arg1, self.arg2)
        }
    }

    /// True when the offset goes through the 2x2
    /// (`SCALED_COMPONENT_OFFSET` set and `UNSCALED_COMPONENT_OFFSET`
    /// clear), as the core walk reads it.
    const fn scales_offset(&self) -> bool {
        self.flags & (COMP_SCALED_COMPONENT_OFFSET | COMP_UNSCALED_COMPONENT_OFFSET)
            == COMP_SCALED_COMPONENT_OFFSET
    }
}

/// Reads every component record of the composite glyph `body`.
pub(crate) fn read_component_records(body: &[u8]) -> Result<Vec<CompRecord>, SubsetError> {
    const CTX: SubsetError = SubsetError::Unsupported("instance: composite component truncated");
    let mut r = Reader::new(body);
    r.skip(10).map_err(|_| CTX)?;
    let mut out = Vec::new();
    loop {
        let start = r.position();
        let flags = r.read_u16().map_err(|_| CTX)?;
        let glyph = r.read_u16().map_err(|_| CTX)?;
        let xy = flags & COMP_ARGS_ARE_XY_VALUES != 0;
        let (arg1, arg2) = if flags & COMP_ARG_1_AND_2_ARE_WORDS != 0 {
            if xy {
                let a = r.read_i16().map_err(|_| CTX)?;
                let b = r.read_i16().map_err(|_| CTX)?;
                (i32::from(a), i32::from(b))
            } else {
                let a = r.read_u16().map_err(|_| CTX)?;
                let b = r.read_u16().map_err(|_| CTX)?;
                (i32::from(a), i32::from(b))
            }
        } else if xy {
            let a = r.read_i8().map_err(|_| CTX)?;
            let b = r.read_i8().map_err(|_| CTX)?;
            (i32::from(a), i32::from(b))
        } else {
            let a = r.read_u8().map_err(|_| CTX)?;
            let b = r.read_u8().map_err(|_| CTX)?;
            (i32::from(a), i32::from(b))
        };
        let mut f2dot14 = || r.read_f2dot14().map_err(|_| CTX);
        let matrix = if flags & COMP_WE_HAVE_A_SCALE != 0 {
            let s = f2dot14()?;
            [s, 0.0, 0.0, s]
        } else if flags & COMP_WE_HAVE_AN_X_AND_Y_SCALE != 0 {
            let (xx, yy) = (f2dot14()?, f2dot14()?);
            [xx, 0.0, 0.0, yy]
        } else if flags & COMP_WE_HAVE_A_TWO_BY_TWO != 0 {
            [f2dot14()?, f2dot14()?, f2dot14()?, f2dot14()?]
        } else {
            [1.0, 0.0, 0.0, 1.0]
        };
        out.push(CompRecord {
            start,
            end: r.position(),
            flags,
            glyph,
            arg1,
            arg2,
            matrix,
        });
        if flags & COMP_MORE_COMPONENTS == 0 {
            break;
        }
    }
    Ok(out)
}

/// Rewrites the composite glyph `body` with every offset-placed
/// component moved by its delta (`deltas[i]` for component `i`; a
/// missing delta counts as zero), as HarfBuzz's instancer does: the
/// offset rounds, and arguments that no longer fit a byte become
/// words. Anchored components, the header, and whatever follows the
/// last component (its instructions) are copied unchanged.
fn rewrite_components(body: &[u8], components: &[CompRecord], deltas: &[(f32, f32)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 2 * components.len());
    out.extend_from_slice(&body[..10]);
    let mut tail = 10;
    for (i, c) in components.iter().enumerate() {
        tail = c.end;
        let record = &body[c.start..c.end];
        if c.is_anchored() {
            out.extend_from_slice(record);
            continue;
        }
        let (dx, dy) = deltas.get(i).copied().unwrap_or((0.0, 0.0));
        let x = round_half_up(c.arg1 as f32 + dx);
        let y = round_half_up(c.arg2 as f32 + dy);
        let words = c.flags & COMP_ARG_1_AND_2_ARE_WORDS != 0;
        let fits_bytes = (-128..=127).contains(&x) && (-128..=127).contains(&y);
        // Flags and glyph id, then the arguments, then the transform.
        let (args_len, flags) = if words || !fits_bytes {
            (4, c.flags | COMP_ARG_1_AND_2_ARE_WORDS)
        } else {
            (2, c.flags)
        };
        out.extend_from_slice(&flags.to_be_bytes());
        out.extend_from_slice(&record[2..4]);
        if args_len == 4 {
            out.extend_from_slice(&clamp_i16(x).to_be_bytes());
            out.extend_from_slice(&clamp_i16(y).to_be_bytes());
        } else {
            out.push(x as i8 as u8);
            out.push(y as i8 as u8);
        }
        let src_args_len = if words { 4 } else { 2 };
        out.extend_from_slice(&record[4 + src_args_len..]);
    }
    out.extend_from_slice(&body[tail..]);
    out
}

// ---------------------------------------------------------------------------
// Rounding
// ---------------------------------------------------------------------------

pub(crate) fn clamp_i16(v: i32) -> i16 {
    v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

mod extents;

#[cfg(test)]
mod tests;
