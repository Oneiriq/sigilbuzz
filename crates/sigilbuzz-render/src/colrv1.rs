//! COLRv1 paint-tree rasterization.
//!
//! [`rasterize_colrv1`] consumes a flat [`DrawCmd`] stream emitted by
//! [`sigilbuzz_paint::evaluate_with`] and turns it into a single
//! premultiplied RGBA [`ColorPixmap`]. The flow is:
//!
//! 1. Walk the `DrawCmd` stream linearly. Maintain a *layer stack* of
//!    `ColorPixmap`s. The bottom of the stack is the final surface.
//! 2. `FillGlyph` rasterizes the inner outline glyph as an alpha mask,
//!    converts the [`PaintSource`] (solid or gradient) into a premul
//!    RGBA pixel grid, masks it by the glyph alpha, and `over`-composes
//!    the result onto the top of the layer stack.
//! 3. `PushLayer { mode }` allocates a fresh transparent pixmap of the
//!    same size as the surface and pushes it; subsequent fills land on
//!    that fresh pixmap.
//! 4. `PopLayer` blends the top pixmap into the one below it using the
//!    composite mode recorded at push time, per Porter-Duff.
//!
//! The implementation avoids any external math crates:
//! gradients, transforms, and Porter-Duff are all inline. The crate's
//! single dep is `sigilbuzz-paint`, which itself only depends on
//! `sigilbuzz`.
//!
//! ## Coordinate space
//!
//! `sigilbuzz-paint` ships transforms in *design-unit* space. The
//! caller (`Rasterizer::rasterize_colrv1_glyph`) bakes the
//! design-units-to-pixels matrix into the outermost transform before
//! every leaf is hit, so by the time we get a `Transform2D` it already
//! maps design units straight to (Y-flipped) pixel space.
//!
//! ## Composite modes
//!
//! The five Porter-Duff modes the driver implements pixel-perfectly:
//! `SrcOver`, `DestIn`, `DestOut`, `SrcIn`, `SrcOut`. Other modes
//! (`Plus`, the HSL family, etc.) fall back to `SrcOver` so color
//! glyphs that use them at least show *something*. The caller can
//! upgrade individual modes later without breaking the API.

use alloc::vec::Vec;

use sigilbuzz::Face;
use sigilbuzz_paint::{
    evaluate_with, Color, CompositeMode, DrawCmd, EvalOptions, Extend, Gradient, GradientKind,
    PaintSource, Transform2D,
};

use crate::affine::Affine;
use crate::error::RenderError;
use crate::flatten::flatten;
use crate::pixmap::{ColorPixmap, Pixmap};
use crate::raster::rasterize as raster;

/// Public entry: walks the `DrawCmd` stream `sigilbuzz-paint` would
/// produce for `gid` and renders it to a premultiplied RGBA pixmap.
///
/// `palette_index` selects the CPAL palette the evaluator resolves
/// palette entries against. A font without that palette falls back to
/// palette 0. Foreground (`0xFFFF`) entries render in the evaluator's
/// default foreground, opaque white.
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
    {
        let Some(colr) = face.colr().map_err(|_| RenderError::Parse("colr"))? else {
            return Err(RenderError::ColrV1NotFound(gid));
        };
        if colr.paint(gid).is_none() {
            return Err(RenderError::ColrV1NotFound(gid));
        }
    }

    let options = EvalOptions::new()
        .with_coords(coords)
        .with_palette_index(palette_index);
    let cmds = evaluate_with(face, gid, &options);
    if cmds.is_empty() {
        return Ok(ColorPixmap::new(0, 0));
    }

    // Design units to pixel space. Y flips so output rows go down.
    let s = size_pt / upem;
    let to_pixels = Transform2D {
        xx: s,
        yx: 0.0,
        xy: 0.0,
        yy: -s,
        dx: 0.0,
        dy: 0.0,
    };

    // First pass: pre-rasterize every FillGlyph leaf to discover the
    // overall bounding box. This matches the strategy used by the
    // COLRv0 path: no surface allocation until we know the union.
    let mut leaves: Vec<Leaf> = Vec::new();
    for cmd in &cmds {
        if let DrawCmd::FillGlyph {
            gid: fill_gid,
            transform,
            paint,
        } = cmd
        {
            let xform = transform.then(to_pixels);
            let affine = transform_to_affine(xform);
            let outline = face
                .glyph_outline_at_coords(*fill_gid, coords)
                .map_err(|_| RenderError::Parse("glyph_outline"))?;
            let Some(outline) = outline else {
                leaves.push(Leaf::Empty);
                continue;
            };
            if outline.is_empty() {
                leaves.push(Leaf::Empty);
                continue;
            }
            let segs = flatten(outline.ops().iter().copied(), &affine, tolerance);
            if segs.is_empty() {
                leaves.push(Leaf::Empty);
                continue;
            }
            let r = raster(&segs);
            leaves.push(Leaf::Glyph {
                mask: r.pixmap,
                origin_x: r.origin_x,
                origin_y: r.origin_y,
                paint: paint.clone(),
                paint_xform: xform,
            });
        }
    }

    // Compute the union bbox of every non-empty leaf so the driver can
    // allocate a single canvas big enough for every push/pop layer.
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    let mut any = false;
    for leaf in &leaves {
        if let Leaf::Glyph {
            mask,
            origin_x,
            origin_y,
            ..
        } = leaf
        {
            if mask.is_empty() {
                continue;
            }
            any = true;
            min_x = min_x.min(*origin_x);
            min_y = min_y.min(*origin_y);
            max_x = max_x.max(*origin_x + mask.width as i32);
            max_y = max_y.max(*origin_y + mask.height as i32);
        }
    }
    if !any || max_x <= min_x || max_y <= min_y {
        return Ok(ColorPixmap::new(0, 0));
    }
    let width = (max_x - min_x) as u32;
    let height = (max_y - min_y) as u32;

    // Layer stack. The bottom is the destination; PushLayer adds a
    // fresh transparent surface on top.
    let mut stack: Vec<Layer> = Vec::with_capacity(4);
    stack.push(Layer {
        pixmap: ColorPixmap::new(width, height),
        mode: CompositeMode::SrcOver,
    });

    let mut leaf_iter = leaves.into_iter();
    for cmd in cmds {
        match cmd {
            DrawCmd::FillGlyph { .. } => {
                let leaf = leaf_iter
                    .next()
                    .expect("leaf vec built from cmd stream stays in lock-step");
                let Leaf::Glyph {
                    mask,
                    origin_x,
                    origin_y,
                    paint,
                    paint_xform,
                } = leaf
                else {
                    continue;
                };
                let dx = origin_x - min_x;
                let dy = origin_y - min_y;
                // Translate paint coords from union-bbox space into
                // mask-local space when sampling the gradient.
                let bbox_origin = (min_x, min_y);
                let top = stack.last_mut().expect("layer stack never empty");
                paint_glyph_into_layer(
                    &mut top.pixmap,
                    &mask,
                    dx,
                    dy,
                    &paint,
                    paint_xform,
                    bbox_origin,
                );
            }
            DrawCmd::PushLayer { composite_mode } => {
                stack.push(Layer {
                    pixmap: ColorPixmap::new(width, height),
                    mode: composite_mode,
                });
            }
            DrawCmd::PopLayer => {
                if stack.len() < 2 {
                    // Mismatched pop: evaluator guarantees pairing
                    // but we stay defensive against future cmd-stream
                    // changes.
                    continue;
                }
                let top = stack.pop().expect("len >= 2 above");
                let parent = stack.last_mut().expect("len >= 1 above");
                composite_layer(&mut parent.pixmap, &top.pixmap, top.mode);
            }
        }
    }

    Ok(stack.remove(0).pixmap)
}

/// Per-leaf data captured during the first pass.
enum Leaf {
    /// Outline rasterized to alpha + the paint that fills it.
    Glyph {
        mask: Pixmap,
        origin_x: i32,
        origin_y: i32,
        paint: PaintSource,
        /// Design-units-to-pixel transform that was used to flatten
        /// the outline. The same transform takes paint coordinates
        /// (also given in design units) into pixel space.
        paint_xform: Transform2D,
    },
    /// Skipped leaf (no outline / empty / out-of-range).
    Empty,
}

/// Layer-stack entry.
struct Layer {
    pixmap: ColorPixmap,
    /// Composite mode to use when this layer is `pop`ped against the
    /// surface below.
    mode: CompositeMode,
}

/// Converts a paint-crate `Transform2D` to a render-crate `Affine`.
/// The two structs share a layout (xx, yx, xy, yy, dx, dy) but live in
/// different crates so we keep this micro-conversion explicit.
const fn transform_to_affine(t: Transform2D) -> Affine {
    Affine {
        xx: t.xx,
        yx: t.yx,
        xy: t.xy,
        yy: t.yy,
        dx: t.dx,
        dy: t.dy,
    }
}

// =========================================================================
// Per-leaf compositing
// =========================================================================

/// Composites a single FillGlyph onto the destination layer.
///
/// `mask` is the alpha pixmap for the outline. `(dx, dy)` is the
/// mask's offset within `dst`'s space. `paint` is the color source.
/// `paint_xform` is the transform that mapped paint design-unit
/// coordinates into pixel space (we need it to sample gradients in
/// pixel space). `bbox_origin` is the destination layer's `(min_x,
/// min_y)` in pixel space, i.e. the offset that turns a pixel index
/// `(px, py)` inside `dst` into absolute pixel-space coordinates.
fn paint_glyph_into_layer(
    dst: &mut ColorPixmap,
    mask: &Pixmap,
    dx: i32,
    dy: i32,
    paint: &PaintSource,
    paint_xform: Transform2D,
    bbox_origin: (i32, i32),
) {
    if dst.is_empty() || mask.is_empty() {
        return;
    }
    let dw = dst.width as i32;
    let dh = dst.height as i32;
    for my in 0..mask.height {
        let py = dy + my as i32;
        if py < 0 || py >= dh {
            continue;
        }
        for mx in 0..mask.width {
            let px = dx + mx as i32;
            if px < 0 || px >= dw {
                continue;
            }
            let m = mask.get(mx, my);
            if m == 0 {
                continue;
            }
            // The pixel's center in pixel space (= the gradient
            // domain after `paint_xform` was already folded in by the
            // caller via `transform.then(to_pixels)`).
            let abs_x = (bbox_origin.0 + px) as f32 + 0.5;
            let abs_y = (bbox_origin.1 + py) as f32 + 0.5;
            let rgba = evaluate_paint(paint, paint_xform, abs_x, abs_y);
            let src = mul_alpha(rgba, m);
            blend_src_over(dst, px as u32, py as u32, src);
        }
    }
}

/// Resolves the color at pixel `(x, y)` for a paint source.
fn evaluate_paint(paint: &PaintSource, paint_xform: Transform2D, x: f32, y: f32) -> [u8; 4] {
    match paint {
        // Foreground fills arrive already resolved to the evaluator's
        // default foreground (opaque white), so `is_foreground` needs no
        // special handling here.
        PaintSource::Solid { color, .. } => to_premul(*color),
        PaintSource::Gradient(g) => sample_gradient(g, paint_xform, x, y),
    }
}

/// Multiplies a premul RGBA pixel by an extra mask coverage `m`
/// (0..=255). All channels, including alpha, scale together so the
/// result stays premultiplied.
fn mul_alpha(rgba: [u8; 4], m: u8) -> [u8; 4] {
    let m = m as u32;
    [
        ((rgba[0] as u32 * m + 127) / 255) as u8,
        ((rgba[1] as u32 * m + 127) / 255) as u8,
        ((rgba[2] as u32 * m + 127) / 255) as u8,
        ((rgba[3] as u32 * m + 127) / 255) as u8,
    ]
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

/// `dst[px, py] = src OVER dst[px, py]` (Porter-Duff source-over with
/// premultiplied operands).
fn blend_src_over(dst: &mut ColorPixmap, px: u32, py: u32, src: [u8; 4]) {
    if src[3] == 0 {
        return;
    }
    let idx = (py as usize * dst.width as usize + px as usize) * 4;
    let dr = dst.data[idx] as u32;
    let dg = dst.data[idx + 1] as u32;
    let db = dst.data[idx + 2] as u32;
    let da = dst.data[idx + 3] as u32;
    let inv = 255 - src[3] as u32;
    dst.data[idx] = (src[0] as u32 + (dr * inv + 127) / 255) as u8;
    dst.data[idx + 1] = (src[1] as u32 + (dg * inv + 127) / 255) as u8;
    dst.data[idx + 2] = (src[2] as u32 + (db * inv + 127) / 255) as u8;
    dst.data[idx + 3] = (src[3] as u32 + (da * inv + 127) / 255) as u8;
}

// =========================================================================
// Gradient sampling
// =========================================================================

/// Samples a gradient at pixel-space `(x, y)`. Returns a premultiplied
/// 8-bit RGBA. Gradient geometry arrives in *design-unit* space; we
/// transform it through `paint_xform` so the sample point can stay in
/// pixel space.
fn sample_gradient(g: &Gradient, paint_xform: Transform2D, x: f32, y: f32) -> [u8; 4] {
    if g.stops.is_empty() {
        return [0, 0, 0, 0];
    }
    let t = match g.kind {
        GradientKind::Linear { p0, p1, .. } => {
            let (a, b) = transformed_pair(paint_xform, p0, p1);
            project_linear(a, b, (x, y))
        }
        GradientKind::Radial { c0, r0, c1, r1 } => {
            let (a, b) = transformed_pair(paint_xform, c0, c1);
            // Radii scale by the matrix's average linear scale, a
            // rough but robust approximation that handles uniform
            // scale exactly and stays sensible under skew.
            let sa = matrix_scale(paint_xform);
            project_radial(a, r0 * sa, b, r1 * sa, (x, y))
        }
        GradientKind::Sweep {
            center,
            start_angle,
            end_angle,
        } => {
            let (cx, cy) = paint_xform.apply(center.0, center.1);
            project_sweep((cx, cy), start_angle, end_angle, (x, y))
        }
    };
    let t = match t {
        Some(t) => apply_extend(t, g.extend),
        None => return [0, 0, 0, 0],
    };
    let c = sample_stops(&g.stops, t);
    to_premul(c)
}

fn transformed_pair(m: Transform2D, p0: (f32, f32), p1: (f32, f32)) -> ((f32, f32), (f32, f32)) {
    (m.apply(p0.0, p0.1), m.apply(p1.0, p1.1))
}

/// Approximate uniform-scale factor for a 2x3 affine: geometric mean
/// of the column lengths. Exact for uniform scale, reasonable under
/// rotation and skew.
fn matrix_scale(m: Transform2D) -> f32 {
    let lx = (m.xx * m.xx + m.yx * m.yx).sqrt();
    let ly = (m.xy * m.xy + m.yy * m.yy).sqrt();
    (lx * ly).sqrt()
}

/// Projects `p` onto the line from `a` to `b`, returning the
/// normalized parameter `t` such that `a + t * (b - a)` is the
/// closest point on the line. Returns `None` when `a == b`.
///
/// Re-exported through the crate so the SVG path can reuse the same
/// projection logic for `<linearGradient>` (PR #205 deferral).
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
/// `<radialGradient>` (PR #205 deferral).
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
/// `pub(crate)` so the SVG path can reuse the COLRv1 ramp behavior
/// (PR #205 deferral).
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
pub(crate) fn sample_stops(stops: &[sigilbuzz_paint::ColorStop], t: f32) -> Color {
    if stops.is_empty() {
        return Color::TRANSPARENT;
    }
    if stops.len() == 1 {
        return stops[0].color;
    }
    let first = stops[0];
    let last = stops[stops.len() - 1];
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
/// to be the same width / height; the COLRv1 driver maintains that
/// invariant by allocating every layer at the union-bbox size.
fn composite_layer(parent: &mut ColorPixmap, top: &ColorPixmap, mode: CompositeMode) {
    debug_assert_eq!(parent.width, top.width);
    debug_assert_eq!(parent.height, top.height);
    if parent.is_empty() {
        return;
    }
    let n = (parent.width as usize) * (parent.height as usize);
    for i in 0..n {
        let idx = i * 4;
        let dr = parent.data[idx] as u32;
        let dg = parent.data[idx + 1] as u32;
        let db = parent.data[idx + 2] as u32;
        let da = parent.data[idx + 3] as u32;
        let sr = top.data[idx] as u32;
        let sg = top.data[idx + 1] as u32;
        let sb = top.data[idx + 2] as u32;
        let sa = top.data[idx + 3] as u32;
        let (rr, rg, rb, ra) = porter_duff(mode, sr, sg, sb, sa, dr, dg, db, da);
        parent.data[idx] = rr;
        parent.data[idx + 1] = rg;
        parent.data[idx + 2] = rb;
        parent.data[idx + 3] = ra;
    }
}

/// Porter-Duff blend table. All operands are 8-bit premultiplied. The
/// formulas are the canonical ones: `Sa` and `Da` are the source /
/// destination alpha channels, `inv = 255 - alpha`. Modes outside the
/// supported set fall through to `SrcOver` so a color glyph at least
/// renders something instead of vanishing.
#[allow(clippy::too_many_arguments)]
fn porter_duff(
    mode: CompositeMode,
    sr: u32,
    sg: u32,
    sb: u32,
    sa: u32,
    dr: u32,
    dg: u32,
    db: u32,
    da: u32,
) -> (u8, u8, u8, u8) {
    match mode {
        CompositeMode::SrcOver => {
            let inv = 255 - sa;
            (
                (sr + (dr * inv + 127) / 255) as u8,
                (sg + (dg * inv + 127) / 255) as u8,
                (sb + (db * inv + 127) / 255) as u8,
                (sa + (da * inv + 127) / 255) as u8,
            )
        }
        CompositeMode::DestIn => {
            // dst stays where src has alpha. Multiply dst by src.a.
            (
                ((dr * sa + 127) / 255) as u8,
                ((dg * sa + 127) / 255) as u8,
                ((db * sa + 127) / 255) as u8,
                ((da * sa + 127) / 255) as u8,
            )
        }
        CompositeMode::DestOut => {
            // dst stays where src is transparent. Multiply dst by (1 - src.a).
            let inv = 255 - sa;
            (
                ((dr * inv + 127) / 255) as u8,
                ((dg * inv + 127) / 255) as u8,
                ((db * inv + 127) / 255) as u8,
                ((da * inv + 127) / 255) as u8,
            )
        }
        CompositeMode::SrcIn => {
            // src masked by dst.a.
            (
                ((sr * da + 127) / 255) as u8,
                ((sg * da + 127) / 255) as u8,
                ((sb * da + 127) / 255) as u8,
                ((sa * da + 127) / 255) as u8,
            )
        }
        CompositeMode::SrcOut => {
            // src masked by (1 - dst.a).
            let inv = 255 - da;
            (
                ((sr * inv + 127) / 255) as u8,
                ((sg * inv + 127) / 255) as u8,
                ((sb * inv + 127) / 255) as u8,
                ((sa * inv + 127) / 255) as u8,
            )
        }
        // Unsupported / future modes: fall back to source-over so
        // the glyph still appears. A more advanced renderer can grow
        // this table in place.
        _ => {
            let inv = 255 - sa;
            (
                (sr + (dr * inv + 127) / 255) as u8,
                (sg + (dg * inv + 127) / 255) as u8,
                (sb + (db * inv + 127) / 255) as u8,
                (sa + (da * inv + 127) / 255) as u8,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use sigilbuzz_paint::{ColorStop, GradientKind};

    fn col(r: f32, g: f32, b: f32, a: f32) -> Color {
        Color::new(r, g, b, a)
    }

    #[test]
    fn extend_pad_clamps() {
        assert!((apply_extend(-0.5, Extend::Pad) - 0.0).abs() < 1e-6);
        assert!((apply_extend(1.5, Extend::Pad) - 1.0).abs() < 1e-6);
        assert!((apply_extend(0.5, Extend::Pad) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn extend_repeat_wraps() {
        assert!((apply_extend(1.25, Extend::Repeat) - 0.25).abs() < 1e-6);
        assert!((apply_extend(-0.25, Extend::Repeat) - 0.75).abs() < 1e-6);
    }

    #[test]
    fn extend_reflect_bounces() {
        // 1.25 -> 0.75 (reflected past 1)
        assert!((apply_extend(1.25, Extend::Reflect) - 0.75).abs() < 1e-6);
        // 2.25 -> 0.25 (full cycle + a bit)
        assert!((apply_extend(2.25, Extend::Reflect) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn linear_projection_endpoints() {
        let a = (0.0, 0.0);
        let b = (10.0, 0.0);
        assert!((project_linear(a, b, (0.0, 0.0)).unwrap() - 0.0).abs() < 1e-6);
        assert!((project_linear(a, b, (10.0, 0.0)).unwrap() - 1.0).abs() < 1e-6);
        assert!((project_linear(a, b, (5.0, 0.0)).unwrap() - 0.5).abs() < 1e-6);
        // Off-axis: projects to the foot of the perpendicular.
        assert!((project_linear(a, b, (5.0, 50.0)).unwrap() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn linear_projection_zero_length_is_none() {
        assert!(project_linear((1.0, 1.0), (1.0, 1.0), (0.0, 0.0)).is_none());
    }

    #[test]
    fn sample_stops_interpolates_linearly() {
        let stops = [
            ColorStop::new(0.0, col(1.0, 0.0, 0.0, 1.0)),
            ColorStop::new(1.0, col(0.0, 0.0, 1.0, 1.0)),
        ];
        let mid = sample_stops(&stops, 0.5);
        assert!((mid.r - 0.5).abs() < 1e-6);
        assert!((mid.b - 0.5).abs() < 1e-6);
        // Below first stop clamps to first.
        let lo = sample_stops(&stops, -1.0);
        assert!((lo.r - 1.0).abs() < 1e-6);
        // Above last stop clamps to last.
        let hi = sample_stops(&stops, 2.0);
        assert!((hi.b - 1.0).abs() < 1e-6);
    }

    #[test]
    fn to_premul_multiplies_channels() {
        let p = to_premul(col(1.0, 1.0, 1.0, 0.5));
        // 1.0 * 0.5 ~ 128 (rounded from 127.5).
        assert!(p[0] >= 127 && p[0] <= 128);
        assert_eq!(p[3], 128);
    }

    #[test]
    fn blend_src_over_full_alpha_replaces() {
        let mut dst = ColorPixmap::new(1, 1);
        blend_src_over(&mut dst, 0, 0, [255, 0, 0, 255]);
        assert_eq!(dst.get(0, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn blend_src_over_zero_alpha_is_noop() {
        let mut dst = ColorPixmap::new(1, 1);
        // Pre-fill so we can detect any clobber.
        dst.data = vec![10, 20, 30, 40];
        blend_src_over(&mut dst, 0, 0, [0, 0, 0, 0]);
        assert_eq!(dst.data, vec![10, 20, 30, 40]);
    }

    #[test]
    fn porter_duff_src_over_matches_legacy() {
        // (255,0,0,255) over (0,0,255,255) = (255,0,0,255).
        let r = porter_duff(CompositeMode::SrcOver, 255, 0, 0, 255, 0, 0, 255, 255);
        assert_eq!(r, (255, 0, 0, 255));
    }

    #[test]
    fn porter_duff_dest_in_masks_dest_by_src_alpha() {
        // src.a=128 (~50%), dst opaque red: result should be ~50% red.
        let r = porter_duff(CompositeMode::DestIn, 0, 0, 0, 128, 255, 0, 0, 255);
        assert!(r.0 > 120 && r.0 < 132, "got {}", r.0);
        assert_eq!(r.1, 0);
        assert!(r.3 > 120 && r.3 < 132);
    }

    #[test]
    fn porter_duff_dest_out_clears_dest_where_src_opaque() {
        let r = porter_duff(CompositeMode::DestOut, 0, 0, 0, 255, 255, 255, 255, 255);
        assert_eq!(r, (0, 0, 0, 0));
    }

    #[test]
    fn porter_duff_src_in_masks_src_by_dest_alpha() {
        let r = porter_duff(CompositeMode::SrcIn, 255, 0, 0, 255, 0, 0, 0, 128);
        // src red * dst.a/255.
        assert!(r.0 > 120 && r.0 < 132);
        assert!(r.3 > 120 && r.3 < 132);
    }

    #[test]
    fn porter_duff_src_out_keeps_src_where_dest_transparent() {
        let r = porter_duff(CompositeMode::SrcOut, 255, 0, 0, 255, 0, 0, 0, 0);
        assert_eq!(r, (255, 0, 0, 255));
    }

    #[test]
    fn unsupported_mode_falls_back_to_src_over() {
        let r1 = porter_duff(CompositeMode::SrcOver, 255, 0, 0, 255, 0, 0, 0, 0);
        let r2 = porter_duff(CompositeMode::Multiply, 255, 0, 0, 255, 0, 0, 0, 0);
        assert_eq!(r1, r2);
    }

    #[test]
    fn radial_quadratic_two_circles() {
        // Two concentric circles, radii 0 and 10, centered on origin.
        // Sample at (5, 0). That's halfway between r=0 and r=10.
        let t = project_radial((0.0, 0.0), 0.0, (0.0, 0.0), 10.0, (5.0, 0.0));
        assert!(t.is_some());
        let t = t.unwrap();
        assert!((t - 0.5).abs() < 1e-3, "got {t}");
    }

    #[test]
    fn sweep_basic_quadrants() {
        let c = (0.0, 0.0);
        // Sweep from 0 to 2pi: angle 0 -> t=0, angle pi -> t=0.5.
        let t = project_sweep(c, 0.0, core::f32::consts::TAU, (1.0, 0.0));
        assert!((t.unwrap() - 0.0).abs() < 1e-3);
        let t = project_sweep(c, 0.0, core::f32::consts::TAU, (-1.0, 0.0));
        assert!((t.unwrap() - 0.5).abs() < 1e-3);
    }

    #[test]
    fn gradient_with_no_stops_is_transparent() {
        let g = Gradient {
            kind: GradientKind::Linear {
                p0: (0.0, 0.0),
                p1: (10.0, 0.0),
                p2: (0.0, 1.0),
            },
            stops: Vec::new(),
            extend: Extend::Pad,
        };
        let p = sample_gradient(&g, Transform2D::IDENTITY, 5.0, 0.0);
        assert_eq!(p, [0, 0, 0, 0]);
    }
}
