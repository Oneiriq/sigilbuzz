//! Render-time blit: rasterizes each fill and composites its solid or
//! gradient paint into the output pixmap.

use crate::affine::Affine;
use crate::colrv1::{apply_extend, project_linear, project_radial, sample_stops, to_premul};
use crate::flatten::flatten;
use crate::pixmap::{ColorPixmap, Pixmap};
use crate::raster::rasterize as raster;

use super::clip_mask::apply_mask;
use super::filter::{apply_filter, composite_over};
use super::model::{Fill, GradKind, GradientPaint, Paint};

// =========================================================================
// Render-time blit
// =========================================================================

pub(super) fn render_fill(out: &mut ColorPixmap, fill: &Fill, world: &Affine, tol: f32) {
    let xf = world.compose(&fill.xform);
    let segs = flatten(fill.ops.iter().copied(), &xf, tol);
    if segs.is_empty() {
        return;
    }
    let mask = raster(&segs);
    if mask.pixmap.is_empty() {
        return;
    }
    // If a clip-path is set, rasterize it once, then multiply mask
    // alpha by the clip alpha at sample time. The clip lives in
    // document space; compose the world transform on top.
    let clip_mask = fill.clip.as_ref().map(|cs| {
        let cxf = world.compose(&cs.xform);
        let csegs = flatten(cs.ops.iter().copied(), &cxf, tol);
        raster(&csegs)
    });

    // Filtered or masked shapes route through a same-size scratch
    // ColorPixmap (the SourceGraphic) instead of writing to `out`
    // directly. The filter / mask pipeline then produces a final
    // pixmap which is composited under the canvas via Porter-Duff
    // source-over. This keeps the per-pixel ops (filter primitives,
    // mask alpha multiplication) operating on canvas-aligned buffers
    // and avoids tracking per-shape filter / mask regions.
    if fill.filter.is_some() || fill.mask.is_some() {
        let mut src = ColorPixmap::new(out.width, out.height);
        paint_into(&mut src, fill, &mask, clip_mask.as_ref(), world);
        let mut result = if let Some(filter) = &fill.filter {
            apply_filter(filter, &src)
        } else {
            src
        };
        if let Some(m) = &fill.mask {
            apply_mask(&mut result, m, world, tol);
        }
        composite_over(out, &result);
        return;
    }

    paint_into(out, fill, &mask, clip_mask.as_ref(), world);
}

/// Paints `fill` into `dst` at the canvas-aligned position implied by
/// `mask.origin_x/y`. Shared by the unfiltered fast path and the
/// filtered SourceGraphic materialization.
fn paint_into(
    dst: &mut ColorPixmap,
    fill: &Fill,
    mask: &crate::raster::Render,
    clip_mask: Option<&crate::raster::Render>,
    world: &Affine,
) {
    match &fill.paint {
        Paint::Solid(color) => {
            blit_solid(
                dst,
                &mask.pixmap,
                mask.origin_x,
                mask.origin_y,
                *color,
                clip_mask,
            );
        }
        Paint::Gradient(g) => {
            let g_xf = world.compose(&fill.xform).compose(&g.gradient_xform);
            blit_gradient(
                dst,
                &mask.pixmap,
                mask.origin_x,
                mask.origin_y,
                g,
                &g_xf,
                clip_mask,
            );
        }
    }
}

/// Blits `mask * color` into `dst`, where `(ox, oy)` is the
/// device-space origin of the mask. Clipping is applied per-pixel
/// against `clip` if provided.
fn blit_solid(
    dst: &mut ColorPixmap,
    mask: &Pixmap,
    ox: i32,
    oy: i32,
    color: [u8; 4],
    clip: Option<&crate::raster::Render>,
) {
    if dst.is_empty() || mask.is_empty() {
        return;
    }
    let dw = dst.width as i32;
    let dh = dst.height as i32;
    let cr = color[0] as u32;
    let cg = color[1] as u32;
    let cb = color[2] as u32;
    let ca = color[3] as u32;
    for my in 0..mask.height as i32 {
        let py = oy + my;
        if py < 0 || py >= dh {
            continue;
        }
        for mx in 0..mask.width as i32 {
            let px = ox + mx;
            if px < 0 || px >= dw {
                continue;
            }
            let mut m = mask.get(mx as u32, my as u32) as u32;
            if m == 0 {
                continue;
            }
            if let Some(cm) = clip {
                let cm_x = px - cm.origin_x;
                let cm_y = py - cm.origin_y;
                if cm_x < 0
                    || cm_y < 0
                    || cm_x >= cm.pixmap.width as i32
                    || cm_y >= cm.pixmap.height as i32
                {
                    continue;
                }
                let cv = cm.pixmap.get(cm_x as u32, cm_y as u32) as u32;
                if cv == 0 {
                    continue;
                }
                m = (m * cv + 127) / 255;
                if m == 0 {
                    continue;
                }
            }
            let sa = (ca * m + 127) / 255;
            if sa == 0 {
                continue;
            }
            let sr = (cr * sa + 127) / 255;
            let sg = (cg * sa + 127) / 255;
            let sb = (cb * sa + 127) / 255;
            let idx = (py as usize * dst.width as usize + px as usize) * 4;
            let dr = dst.data[idx] as u32;
            let dg = dst.data[idx + 1] as u32;
            let db = dst.data[idx + 2] as u32;
            let da = dst.data[idx + 3] as u32;
            let inv = 255 - sa;
            dst.data[idx] = (sr + (dr * inv + 127) / 255) as u8;
            dst.data[idx + 1] = (sg + (dg * inv + 127) / 255) as u8;
            dst.data[idx + 2] = (sb + (db * inv + 127) / 255) as u8;
            dst.data[idx + 3] = (sa + (da * inv + 127) / 255) as u8;
        }
    }
}

/// Blits `mask * gradient` into `dst` using the COLRv1 ramp evaluator.
fn blit_gradient(
    dst: &mut ColorPixmap,
    mask: &Pixmap,
    ox: i32,
    oy: i32,
    g: &GradientPaint,
    g_xf: &Affine,
    clip: Option<&crate::raster::Render>,
) {
    if dst.is_empty() || mask.is_empty() {
        return;
    }
    let dw = dst.width as i32;
    let dh = dst.height as i32;
    for my in 0..mask.height as i32 {
        let py = oy + my;
        if py < 0 || py >= dh {
            continue;
        }
        for mx in 0..mask.width as i32 {
            let px = ox + mx;
            if px < 0 || px >= dw {
                continue;
            }
            let mut m = mask.get(mx as u32, my as u32) as u32;
            if m == 0 {
                continue;
            }
            if let Some(cm) = clip {
                let cm_x = px - cm.origin_x;
                let cm_y = py - cm.origin_y;
                if cm_x < 0
                    || cm_y < 0
                    || cm_x >= cm.pixmap.width as i32
                    || cm_y >= cm.pixmap.height as i32
                {
                    continue;
                }
                let cv = cm.pixmap.get(cm_x as u32, cm_y as u32) as u32;
                if cv == 0 {
                    continue;
                }
                m = (m * cv + 127) / 255;
                if m == 0 {
                    continue;
                }
            }
            // Pixel center in pixel space.
            let abs_x = px as f32 + 0.5;
            let abs_y = py as f32 + 0.5;
            let sample = sample_svg_gradient(g, g_xf, abs_x, abs_y);
            // Apply per-element opacity by scaling alpha.
            let mut sample = sample;
            if g.opacity < 1.0 {
                let factor = g.opacity.clamp(0.0, 1.0);
                sample[0] = ((sample[0] as f32 * factor).round()) as u8;
                sample[1] = ((sample[1] as f32 * factor).round()) as u8;
                sample[2] = ((sample[2] as f32 * factor).round()) as u8;
                sample[3] = ((sample[3] as f32 * factor).round()) as u8;
            }
            // Multiply by mask coverage `m`.
            let sr = (sample[0] as u32 * m + 127) / 255;
            let sg = (sample[1] as u32 * m + 127) / 255;
            let sb = (sample[2] as u32 * m + 127) / 255;
            let sa = (sample[3] as u32 * m + 127) / 255;
            if sa == 0 {
                continue;
            }
            let idx = (py as usize * dst.width as usize + px as usize) * 4;
            let dr = dst.data[idx] as u32;
            let dg = dst.data[idx + 1] as u32;
            let db = dst.data[idx + 2] as u32;
            let da = dst.data[idx + 3] as u32;
            let inv = 255 - sa;
            dst.data[idx] = (sr + (dr * inv + 127) / 255) as u8;
            dst.data[idx + 1] = (sg + (dg * inv + 127) / 255) as u8;
            dst.data[idx + 2] = (sb + (db * inv + 127) / 255) as u8;
            dst.data[idx + 3] = (sa + (da * inv + 127) / 255) as u8;
        }
    }
}

/// Evaluates a parsed SVG gradient at pixel-space `(x, y)`. Routes the
/// gradient geometry through `g_xf` (document -> pixel + any
/// `gradientTransform`) before calling the COLRv1 projection
/// primitives: same shape, same `Pad` / `Repeat` / `Reflect` semantics.
fn sample_svg_gradient(g: &GradientPaint, g_xf: &Affine, x: f32, y: f32) -> [u8; 4] {
    let t_opt = match g.kind {
        GradKind::Linear { x1, y1, x2, y2 } => {
            let a = g_xf.apply(x1, y1);
            let b = g_xf.apply(x2, y2);
            project_linear(a, b, (x, y))
        }
        GradKind::Radial { cx, cy, r, fx, fy } => {
            let centre = g_xf.apply(cx, cy);
            let focus = g_xf.apply(fx, fy);
            // Approximate radius scale by the matrix's geometric mean.
            let lx = (g_xf.xx * g_xf.xx + g_xf.yx * g_xf.yx).sqrt();
            let ly = (g_xf.xy * g_xf.xy + g_xf.yy * g_xf.yy).sqrt();
            let s = (lx * ly).sqrt();
            project_radial(focus, 0.0, centre, r * s, (x, y))
        }
    };
    let Some(t) = t_opt else {
        return [0, 0, 0, 0];
    };
    let t = apply_extend(t, g.extend);
    let c = sample_stops(&g.stops, t);
    to_premul(c)
}
