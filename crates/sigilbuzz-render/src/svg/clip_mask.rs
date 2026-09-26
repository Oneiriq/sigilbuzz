//! `<clipPath>` and `<mask>`: resolving the referenced definitions and
//! applying a mask to a rendered fill.

use alloc::vec::Vec;

use crate::affine::Affine;
use crate::pixmap::ColorPixmap;

use super::document::{doc_full, node_cost, walk, Defs, ElemCtx};
use super::model::{ClipShape, MaskShape, MaskType, MaskUnits, SvgDoc};
use super::path::{
    circle_to_path, ellipse_to_path, line_to_path, parse_path_d, polygon_to_path, polyline_to_path,
    rect_to_path,
};
use super::render::{render_fill, RenderBudget};
use super::style::{parse_length, parse_transform};
use super::xml::name_eq;

pub(super) fn resolve_clip_shape(defs: &Defs<'_>, id: &str) -> Option<ClipShape> {
    let cp = defs.lookup(id)?;
    if !name_eq(&cp.name, "clipPath") || !defs.charge_work(node_cost(cp)) {
        return None;
    }
    // Walk children. We support exactly one shape (path / rect /
    // circle / ellipse). Multiple shapes inside a clipPath are still
    // accepted but only the first is used.
    let mut local_xform = Affine::identity();
    if let Some(t) = cp.attr("transform").and_then(parse_transform) {
        local_xform = local_xform.compose(&t);
    }
    for c in &cp.children {
        if !defs.charge_work(node_cost(c)) {
            return None;
        }
        let child_ops = if name_eq(&c.name, "path") {
            c.attr("d").and_then(|d| parse_path_d(d).ok())
        } else if name_eq(&c.name, "rect") {
            Some(rect_to_path(c))
        } else if name_eq(&c.name, "circle") {
            Some(circle_to_path(c))
        } else if name_eq(&c.name, "ellipse") {
            Some(ellipse_to_path(c))
        } else if name_eq(&c.name, "polygon") {
            Some(polygon_to_path(c))
        } else if name_eq(&c.name, "polyline") {
            Some(polyline_to_path(c))
        } else if name_eq(&c.name, "line") {
            Some(line_to_path(c))
        } else {
            None
        };
        if let Some(ops) = child_ops {
            if ops.is_empty() {
                continue;
            }
            let mut xform = local_xform;
            if let Some(t) = c.attr("transform").and_then(parse_transform) {
                xform = xform.compose(&t);
            }
            return Some(ClipShape { ops, xform });
        }
    }
    None
}

/// Resolves a `<mask id="...">` definition into a [`MaskShape`].
///
/// Walks the mask's children with a fresh root [`ElemCtx`] (mask
/// contents inherit nothing from the masked element) and reuses the
/// document walk machinery to collect each child shape into a
/// [`Fill`](super::model::Fill). The mask's own `transform=` attribute pre-composes onto
/// the inherited identity. Parses `mask-type` (`luminance` default,
/// `alpha` opt-in) and `maskUnits` (`userSpaceOnUse` default,
/// `objectBoundingBox` opt-in) plus the mask region rect (`x`, `y`,
/// `width`, `height`).
///
/// Returns `None` when the id doesn't point at a `<mask>` element or
/// the mask has no renderable children. A self-referential mask
/// (mask-of-mask) is not supported. Nested mask references inside
/// the mask body are dropped at walk time so the caller never sees a
/// recursive composite.
pub(super) fn resolve_mask_shape(defs: &Defs<'_>, id: &str) -> Option<MaskShape> {
    let mn = defs.lookup(id)?;
    if !name_eq(&mn.name, "mask") || !defs.charge_work(node_cost(mn)) {
        return None;
    }
    // Build a tiny scratch SvgDoc so we can re-use `walk` end-to-end.
    // The dimensions don't matter: render-time uses the masked
    // element's pixmap size, not the mask's viewBox.
    let mut scratch = SvgDoc {
        view_w: 1.0,
        view_h: 1.0,
        view_x: 0.0,
        view_y: 0.0,
        fills: Vec::new(),
    };
    let mut ctx = ElemCtx::default();
    if let Some(t) = mn.attr("transform").and_then(parse_transform) {
        ctx.xform = ctx.xform.compose(&t);
    }
    // Drop any nested mask reference on the mask root itself:
    // mask-of-mask isn't supported.
    ctx.mask_href = None;
    // Cycle-guard: bump `mask_depth` so any descendant `<rect
    // mask="url(#...)">` inside the mask body falls out at
    // `emit_paint` rather than recursing back into
    // `resolve_mask_shape`. Caps the call stack at one level of mask
    // resolution.
    ctx.mask_depth = ctx.mask_depth.saturating_add(1);
    for child in &mn.children {
        // Sanity: cap mask-internal fill count at the same MAX_FILLS
        // ceiling as the document. The work and storage budgets are
        // the document's own.
        if doc_full(&scratch, defs) {
            break;
        }
        let _ = walk(child, &mut scratch, defs, &ctx, 0, 0);
    }
    if scratch.fills.is_empty() {
        return None;
    }
    // Strip any nested mask references that survived from grand-
    // children. Mask-of-mask is documented as unsupported.
    for f in &mut scratch.fills {
        f.mask = None;
    }

    let mask_type = match mn.attr("mask-type").map(str::trim) {
        Some(s) if s.eq_ignore_ascii_case("alpha") => MaskType::Alpha,
        _ => MaskType::Luminance,
    };
    let units = match mn.attr("maskUnits").map(str::trim) {
        Some(s) if s.eq_ignore_ascii_case("objectBoundingBox") => MaskUnits::ObjectBoundingBox,
        _ => MaskUnits::UserSpaceOnUse,
    };
    // Per SVG spec the mask region defaults to the full bounding-box
    // window when `objectBoundingBox` (-10%, -10%, 120%, 120% in the
    // spec, but consumer-side OT-SVG fonts almost always use the
    // simpler 0/0/1/1 window, which is the default we use). For
    // `userSpaceOnUse` the region is ignored, so we keep the parse but
    // only consult it in the bbox path.
    let region_x = mn.attr("x").and_then(parse_length).unwrap_or(0.0);
    let region_y = mn.attr("y").and_then(parse_length).unwrap_or(0.0);
    let region_w = mn.attr("width").and_then(parse_length).unwrap_or(1.0);
    let region_h = mn.attr("height").and_then(parse_length).unwrap_or(1.0);

    Some(MaskShape {
        fills: scratch.fills,
        mask_type,
        units,
        region_x,
        region_y,
        region_w,
        region_h,
    })
}

/// Multiplies `dst`'s premultiplied alpha by the alpha derived from
/// `mask`. The mask's children are rendered into a same-size scratch
/// ColorPixmap; the per-pixel coverage factor is then either:
///
/// - `mask-type="luminance"` (SVG default): BT.709 luminance * source
///   alpha (SVG 1.1 §14.4).
/// - `mask-type="alpha"`: the mask buffer's alpha channel directly,
///   skipping the luminance derivation entirely.
///
/// When `maskUnits="objectBoundingBox"` the mask's `(x, y, width,
/// height)` rect is interpreted in `[0, 1]²` of the masked element's
/// bounding box (computed from `dst`'s non-zero alpha extent). Pixels
/// outside that rect are forced to `m = 0`. `userSpaceOnUse` leaves
/// the mask coverage unchanged across the whole canvas.
///
/// Mask children draw from `budget` like any other fill.
pub(super) fn apply_mask_budgeted(
    dst: &mut ColorPixmap,
    mask_shape: &MaskShape,
    world: &Affine,
    tol: f32,
    budget: &mut RenderBudget,
) {
    if dst.is_empty() {
        return;
    }
    // Render the mask's children into a same-size buffer using the
    // same world transform so mask geometry lines up with the masked
    // element in pixel space.
    let mut mask_buf = ColorPixmap::new(dst.width, dst.height);
    for f in &mask_shape.fills {
        render_fill(&mut mask_buf, f, world, tol, budget);
    }

    // For `objectBoundingBox`, derive the bbox from `dst`'s non-zero
    // alpha extent and pre-compute the pixel-space window the mask
    // region (`x`, `y`, `width`, `height`) maps onto.
    //
    // Storing the inclusive min / exclusive max keeps the inside
    // test branch-cheap in the per-pixel loop.
    let bbox = if mask_shape.units == MaskUnits::ObjectBoundingBox {
        compute_alpha_bbox(dst).map(|(min_x, min_y, max_x, max_y)| {
            let bw = (max_x - min_x) as f32;
            let bh = (max_y - min_y) as f32;
            let rx = (min_x as f32) + mask_shape.region_x * bw;
            let ry = (min_y as f32) + mask_shape.region_y * bh;
            let rw = mask_shape.region_w * bw;
            let rh = mask_shape.region_h * bh;
            // Floor / ceil to integer pixel rows; clamp to canvas.
            let lo_x = (rx.floor() as i32).max(0);
            let lo_y = (ry.floor() as i32).max(0);
            let hi_x = ((rx + rw).ceil() as i32).clamp(0, dst.width as i32);
            let hi_y = ((ry + rh).ceil() as i32).clamp(0, dst.height as i32);
            (lo_x, lo_y, hi_x, hi_y)
        })
    } else {
        None
    };
    // ObjectBoundingBox with no opaque pixels in `dst` collapses to a
    // fully transparent result: nothing to mask, nothing to keep.
    if mask_shape.units == MaskUnits::ObjectBoundingBox && bbox.is_none() {
        for px in dst.data.chunks_exact_mut(4) {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            px[3] = 0;
        }
        return;
    }

    // Per-pixel: derive a coverage factor m in [0, 1] from the mask
    // buffer (luminance or alpha), optionally zero it outside the
    // objectBoundingBox window, then scale every channel of dst by m.
    // dst is premultiplied, so scaling all four channels uniformly
    // preserves the invariant.
    //
    // The mask buffer is also premultiplied (it came out of the same
    // render pipeline). For luminance we use an integer-math shortcut:
    // luminance(premul_rgb) is already luminance * alpha
    // because premul_rgb = straight_rgb * alpha, so no un-premultiply
    // step is needed. Fixed-point: BT.709 weights * 1024 -> 218 / 732 /
    // 74 (sum 1024) for round-trip-stable integer math.
    let w = dst.width as i32;
    let h = dst.height as i32;
    for y in 0..h {
        let inside_y = bbox.map_or(true, |(_, lo_y, _, hi_y)| y >= lo_y && y < hi_y);
        for x in 0..w {
            let i = (y as usize) * (w as usize) + (x as usize);
            let inside = inside_y && bbox.map_or(true, |(lo_x, _, hi_x, _)| x >= lo_x && x < hi_x);
            let m = if inside {
                let mr = mask_buf.data[i * 4] as u32;
                let mg = mask_buf.data[i * 4 + 1] as u32;
                let mb = mask_buf.data[i * 4 + 2] as u32;
                let ma = mask_buf.data[i * 4 + 3] as u32;
                match mask_shape.mask_type {
                    MaskType::Luminance => {
                        if ma == 0 {
                            0
                        } else {
                            let lum = (218 * mr + 732 * mg + 74 * mb + 512) / 1024;
                            lum.min(255)
                        }
                    }
                    MaskType::Alpha => ma,
                }
            } else {
                0u32
            };
            if m == 255 {
                continue;
            }
            if m == 0 {
                dst.data[i * 4] = 0;
                dst.data[i * 4 + 1] = 0;
                dst.data[i * 4 + 2] = 0;
                dst.data[i * 4 + 3] = 0;
                continue;
            }
            let dr = dst.data[i * 4] as u32;
            let dg = dst.data[i * 4 + 1] as u32;
            let db = dst.data[i * 4 + 2] as u32;
            let da = dst.data[i * 4 + 3] as u32;
            dst.data[i * 4] = ((dr * m + 127) / 255) as u8;
            dst.data[i * 4 + 1] = ((dg * m + 127) / 255) as u8;
            dst.data[i * 4 + 2] = ((db * m + 127) / 255) as u8;
            dst.data[i * 4 + 3] = ((da * m + 127) / 255) as u8;
        }
    }
}

/// [`apply_mask_budgeted`] with a fresh render budget.
#[cfg(test)]
pub(super) fn apply_mask(dst: &mut ColorPixmap, mask_shape: &MaskShape, world: &Affine, tol: f32) {
    apply_mask_budgeted(dst, mask_shape, world, tol, &mut RenderBudget::new());
}

/// Returns the inclusive-min / exclusive-max pixel extent of the
/// non-zero-alpha pixels of `pm`, or `None` if every pixel is
/// transparent. Used to derive an `objectBoundingBox` window for the
/// `<mask>` apply step.
fn compute_alpha_bbox(pm: &ColorPixmap) -> Option<(i32, i32, i32, i32)> {
    if pm.is_empty() {
        return None;
    }
    let w = pm.width as i32;
    let h = pm.height as i32;
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for y in 0..h {
        for x in 0..w {
            let i = (y as usize) * (w as usize) + (x as usize);
            if pm.data[i * 4 + 3] != 0 {
                if x < min_x {
                    min_x = x;
                }
                if y < min_y {
                    min_y = y;
                }
                if x > max_x {
                    max_x = x;
                }
                if y > max_y {
                    max_y = y;
                }
            }
        }
    }
    if min_x == i32::MAX {
        return None;
    }
    Some((min_x, min_y, max_x + 1, max_y + 1))
}
