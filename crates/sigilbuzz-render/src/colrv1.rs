//! COLRv1 paint-tree rasterization.
//!
//! [`rasterize_colrv1`] walks the glyph with `sigilbuzz_paint::walk`,
//! which reports it in HarfBuzz's callback order, and draws it with the
//! [`RasterSink`](crate::canvas::RasterSink): clips as coverage masks,
//! composites as isolated layers, and fills sampled in paint space.
//! This module holds the entry point and the pixel math shared with the
//! SVG-in-OT renderer: gradient sampling, extend modes, premultiplied
//! color, and Porter-Duff compositing.
//!
//! The implementation avoids any external math crates: gradients,
//! transforms, and Porter-Duff are all inline.
//!
//! ## Coordinate space
//!
//! The walk reports design units. The sink maps them to pixels with
//! `size_pt / units_per_em`, flipping y so rows run down, and places
//! the glyph's clip box (its ClipList box or its computed bounds, as in
//! HarfBuzz) plus a one-pixel margin at the top-left of the output.
//!
//! ## Composite modes
//!
//! Every Porter-Duff mode is exact: `Clear`, `Src`, `Dest`, `SrcOver`,
//! `DestOver`, `SrcIn`, `DestIn`, `SrcOut`, `DestOut`, `SrcAtop`,
//! `DestAtop`, `Xor`, and `Plus`. The separable and HSL blend modes
//! (`Screen`, `Multiply`, `HslHue`, and so on) fall back to `SrcOver`
//! so glyphs that use them at least show *something*.

use sigilbuzz::Face;
use sigilbuzz_paint::walk::{paint_glyph, Resolver};
use sigilbuzz_paint::{Color, ColorStop, CompositeMode, EvalOptions, Extend, GradientKind};

use crate::canvas::RasterSink;
use crate::error::RenderError;
use crate::pixmap::ColorPixmap;

/// Public entry: walks `gid`'s COLRv1 paint tree and renders it to a
/// premultiplied RGBA pixmap.
///
/// `palette_index` selects the CPAL palette palette entries resolve
/// against. Foreground (`0xFFFF`) entries, and entries the font cannot
/// supply, render in `foreground` (straight-alpha RGBA).
///
/// `tolerance` is the per-glyph curve flattening tolerance in pixel
/// units (same semantics as [`crate::Rasterizer`]'s field).
pub(crate) fn rasterize_colrv1(
    face: &Face<'_>,
    gid: u16,
    palette_index: u16,
    size_pt: f32,
    coords: &[f32],
    tolerance: f32,
    foreground: [u8; 4],
) -> Result<ColorPixmap, RenderError> {
    if !size_pt.is_finite() || size_pt <= 0.0 {
        return Err(RenderError::BadSize(size_pt));
    }
    let head = face.head().map_err(|_| RenderError::Parse("head"))?;
    let upem = head.units_per_em as f32;
    if upem <= 0.0 {
        return Err(RenderError::BadUpem);
    }

    // Bail early when the font has no v1 paint for this gid. Returning
    // the dedicated error lets callers fall back to v0 / outline.
    let Some(colr) = face.colr().map_err(|_| RenderError::Parse("colr"))? else {
        return Err(RenderError::ColrV1NotFound(gid));
    };
    if colr.paint(gid).is_none() {
        return Err(RenderError::ColrV1NotFound(gid));
    }

    let [r, g, b, a] = foreground.map(|c| f32::from(c) / 255.0);
    let options = EvalOptions::new()
        .with_coords(coords)
        .with_palette_index(palette_index)
        .with_foreground(Color::new(r, g, b, a));
    let cpal = face.cpal().ok().flatten();
    let resolver = Resolver::new(cpal.as_ref(), &options);
    let mut sink = RasterSink::new(face, coords, resolver, size_pt / upem, tolerance);
    paint_glyph(face, gid, coords, &mut sink);
    sink.finish().ok_or(RenderError::BadSize(size_pt))
}

/// Multiplies a premul RGBA pixel by an extra mask coverage `m`
/// (0..=255). All channels, including alpha, scale together so the
/// result stays premultiplied.
pub(crate) fn mul_alpha(rgba: [u8; 4], m: u8) -> [u8; 4] {
    let m = m as u32;
    rgba.map(|c| ((c as u32 * m + 127) / 255) as u8)
}

/// Converts a paint-crate float `Color` to a premultiplied 8-bit RGBA
/// pixel.
///
/// `pub(crate)` so the SVG path can reuse the same conversion when it
/// resolves a gradient sample to an output pixel.
pub(crate) fn to_premul(c: Color) -> [u8; 4] {
    let a = c.a.clamp(0.0, 1.0);
    let r = c.r.clamp(0.0, 1.0) * a;
    let g = c.g.clamp(0.0, 1.0) * a;
    let b = c.b.clamp(0.0, 1.0) * a;
    [
        (r * 255.0).round().clamp(0.0, 255.0) as u8,
        (g * 255.0).round().clamp(0.0, 255.0) as u8,
        (b * 255.0).round().clamp(0.0, 255.0) as u8,
        (a * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

// =========================================================================
// Gradient sampling
// =========================================================================

/// Samples a gradient at the paint-space point `p`, returning a
/// premultiplied 8-bit RGBA. Sampling in paint space keeps every
/// gradient exact under any transform: a radial gradient under a
/// non-uniform scale is an exact ellipse, and a sweep keeps its angles
/// under a skew.
pub(crate) fn sample_gradient(
    kind: GradientKind,
    stops: &[ColorStop],
    extend: Extend,
    p: (f32, f32),
) -> [u8; 4] {
    if stops.is_empty() {
        return [0, 0, 0, 0];
    }
    let t = match kind {
        GradientKind::Linear { p0, p1, p2 } => {
            let (a, b) = reduce_linear_anchors(p0, p1, p2);
            project_linear(a, b, p)
        }
        GradientKind::Radial { c0, r0, c1, r1 } => project_radial(c0, r0, c1, r1, p),
        GradientKind::Sweep {
            center,
            start_angle,
            end_angle,
        } => project_sweep(center, start_angle, end_angle, p),
    };
    match t {
        Some(t) => to_premul(sample_stops(stops, apply_extend(t, extend))),
        None => [0, 0, 0, 0],
    }
}

/// Folds a COLRv1 linear gradient's rotation point `p2` into its end
/// point: the gradient runs from `p0` toward `p1` projected onto the
/// normal of the line from `p0` to `p2`, so its color lines run
/// parallel to `p0 p2`. With `p2` on `p0` the gradient is plain
/// `p0 -> p1`. This is the reduction HarfBuzz's renderers apply
/// (`hb_paint_reduce_linear_anchors`).
pub(crate) fn reduce_linear_anchors(
    p0: (f32, f32),
    p1: (f32, f32),
    p2: (f32, f32),
) -> ((f32, f32), (f32, f32)) {
    let (q1x, q1y) = (p1.0 - p0.0, p1.1 - p0.1);
    let (q2x, q2y) = (p2.0 - p0.0, p2.1 - p0.1);
    let s = q2x * q2x + q2y * q2y;
    if s < 0.000_001 {
        return (p0, p1);
    }
    let k = (q2x * q1x + q2y * q1y) / s;
    (p0, (p1.0 - k * q2x, p1.1 - k * q2y))
}

/// Projects `p` onto the line from `a` to `b`, returning the
/// normalized parameter `t` such that `a + t * (b - a)` is the
/// closest point on the line. Returns `None` when `a == b`.
///
/// `pub(crate)` so the SVG path can reuse the same projection logic
/// for `<linearGradient>`.
pub(crate) fn project_linear(a: (f32, f32), b: (f32, f32), p: (f32, f32)) -> Option<f32> {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let len_sq = dx * dx + dy * dy;
    if len_sq <= f32::EPSILON {
        return None;
    }
    Some(((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len_sq)
}

/// Two-circle radial gradient projection. Solves the standard
/// quadratic that COLRv1 / SVG share. See the spec's appendix.
/// Returns the larger valid root in `[0, +inf)` (the "outer" branch).
///
/// `pub(crate)` so the SVG path can reuse the same code for
/// `<radialGradient>`.
pub(crate) fn project_radial(
    c0: (f32, f32),
    r0: f32,
    c1: (f32, f32),
    r1: f32,
    p: (f32, f32),
) -> Option<f32> {
    // The cone equation for an animated circle interpolating between
    // (c0, r0) at t=0 and (c1, r1) at t=1:
    //
    //   |p - (c0 + t*(c1-c0))|^2 = (r0 + t*(r1-r0))^2
    //
    // Expanding into At^2 + Bt + C = 0 with the standard substitutions.
    let dx = c1.0 - c0.0;
    let dy = c1.1 - c0.1;
    let dr = r1 - r0;
    let px = p.0 - c0.0;
    let py = p.1 - c0.1;
    let aa = dx * dx + dy * dy - dr * dr;
    let bb = -2.0 * (px * dx + py * dy + r0 * dr);
    let cc = px * px + py * py - r0 * r0;
    if aa.abs() <= f32::EPSILON {
        // Degenerate: both circles same size or coincident centers.
        if bb.abs() <= f32::EPSILON {
            return None;
        }
        let t = -cc / bb;
        // Reject negative radii on the interpolated circle.
        if r0 + t * dr < 0.0 {
            return None;
        }
        return Some(t);
    }
    let disc = bb * bb - 4.0 * aa * cc;
    if disc < 0.0 {
        return None;
    }
    let sq = disc.sqrt();
    let t0 = (-bb + sq) / (2.0 * aa);
    let t1 = (-bb - sq) / (2.0 * aa);
    // Prefer the larger t whose interpolated radius is non-negative.
    let valid = |t: f32| (r0 + t * dr) >= 0.0;
    if valid(t0) {
        Some(t0)
    } else if valid(t1) {
        Some(t1)
    } else {
        None
    }
}

/// Sweep (conic) projection. Returns `t = (angle - start) / (end - start)`,
/// where `angle` is the polar angle of `(p - center)` measured from the
/// +x axis CCW.
fn project_sweep(
    center: (f32, f32),
    start_angle: f32,
    end_angle: f32,
    p: (f32, f32),
) -> Option<f32> {
    let dx = p.0 - center.0;
    let dy = p.1 - center.1;
    if dx == 0.0 && dy == 0.0 {
        return Some(0.0);
    }
    let mut angle = dy.atan2(dx);
    let two_pi = core::f32::consts::TAU;
    // Normalize into [0, 2pi) so it can compare against any
    // start/end the spec might pass.
    if angle < 0.0 {
        angle += two_pi;
    }
    let span = end_angle - start_angle;
    if span.abs() <= f32::EPSILON {
        return None;
    }
    Some((angle - start_angle) / span)
}

/// Applies the extend mode to a gradient parameter `t`, returning the
/// in-`[0, 1]` value used to sample the stops.
///
/// `pub(crate)` so the SVG path can reuse the COLRv1 ramp behavior.
pub(crate) fn apply_extend(t: f32, extend: Extend) -> f32 {
    match extend {
        Extend::Pad => t.clamp(0.0, 1.0),
        Extend::Repeat => {
            let f = t - t.floor();
            if f < 0.0 {
                f + 1.0
            } else {
                f
            }
        }
        Extend::Reflect => {
            let two = 2.0_f32;
            let m = ((t % two) + two) % two;
            if m > 1.0 {
                two - m
            } else {
                m
            }
        }
    }
}

/// Samples the stop list at `t`. Stops are not assumed sorted, but the
/// spec says they should be. We walk them in order and clamp.
///
/// `pub(crate)` so the SVG path can reuse the same ramp interpolation
/// for `<linearGradient>` / `<radialGradient>` stops.
pub(crate) fn sample_stops(stops: &[ColorStop], t: f32) -> Color {
    let (Some(&first), Some(&last)) = (stops.first(), stops.last()) else {
        return Color::TRANSPARENT;
    };
    if stops.len() == 1 {
        return first.color;
    }
    if t <= first.offset {
        return first.color;
    }
    if t >= last.offset {
        return last.color;
    }
    for w in stops.windows(2) {
        let a = w[0];
        let b = w[1];
        if t >= a.offset && t <= b.offset {
            let span = b.offset - a.offset;
            if span <= f32::EPSILON {
                return b.color;
            }
            let u = (t - a.offset) / span;
            return Color::new(
                a.color.r + (b.color.r - a.color.r) * u,
                a.color.g + (b.color.g - a.color.g) * u,
                a.color.b + (b.color.b - a.color.b) * u,
                a.color.a + (b.color.a - a.color.a) * u,
            );
        }
    }
    last.color
}

// =========================================================================
// Layer composition (Porter-Duff)
// =========================================================================

/// Composites layer `top` into `parent` per `mode`. Both are assumed
/// to be the same width / height; the COLRv1 sink maintains that
/// invariant by allocating every layer at the canvas size.
pub(crate) fn composite_layer(parent: &mut ColorPixmap, top: &ColorPixmap, mode: CompositeMode) {
    debug_assert_eq!(parent.width, top.width);
    debug_assert_eq!(parent.height, top.height);
    for (d, s) in parent
        .data
        .chunks_exact_mut(4)
        .zip(top.data.chunks_exact(4))
    {
        let src = [s[0], s[1], s[2], s[3]];
        let dst = [d[0], d[1], d[2], d[3]];
        d.copy_from_slice(&porter_duff(mode, src, dst));
    }
}

/// Porter-Duff composition of premultiplied `src` onto premultiplied
/// `dst`: every channel is `src * Fa + dst * Fb` with the mode's
/// factors. Blend modes outside the Porter-Duff set fall through to
/// `SrcOver` so a color glyph at least renders something instead of
/// vanishing.
fn porter_duff(mode: CompositeMode, src: [u8; 4], dst: [u8; 4]) -> [u8; 4] {
    let (sa, da) = (u32::from(src[3]), u32::from(dst[3]));
    let (fa, fb) = match mode {
        CompositeMode::Clear => (0, 0),
        CompositeMode::Src => (255, 0),
        CompositeMode::Dest => (0, 255),
        CompositeMode::DestOver => (255 - da, 255),
        CompositeMode::SrcIn => (da, 0),
        CompositeMode::DestIn => (0, sa),
        CompositeMode::SrcOut => (255 - da, 0),
        CompositeMode::DestOut => (0, 255 - sa),
        CompositeMode::SrcAtop => (da, 255 - sa),
        CompositeMode::DestAtop => (255 - da, sa),
        CompositeMode::Xor => (255 - da, 255 - sa),
        CompositeMode::Plus => (255, 255),
        _ => (255, 255 - sa),
    };
    let mix = |s: u8, d: u8| {
        let v = (u32::from(s) * fa + u32::from(d) * fb + 127) / 255;
        v.min(255) as u8
    };
    [
        mix(src[0], dst[0]),
        mix(src[1], dst[1]),
        mix(src[2], dst[2]),
        mix(src[3], dst[3]),
    ]
}

#[cfg(test)]
mod tests;
