//! `<textPath>` resolution: finds `<textPath>` nodes that match a
//! consumer-supplied run and places the glyph outlines along the
//! referenced path's arc length.

use alloc::vec::Vec;

use sigilbuzz::tables::PathOp;
use sigilbuzz::Face;

use crate::affine::Affine;
use crate::flatten::flatten;

use super::clip_mask::{resolve_clip_shape, resolve_mask_shape};
use super::document::{doc_full, push_fill, visit_cost, Defs, ElemCtx};
use super::filter::resolve_filter;
use super::model::{Fill, Paint, SvgDoc};
use super::paint_server::resolve_fill_paint;
use super::path::parse_path_d;
use super::style::inherit_attrs;
use super::xml::{name_eq, Node};
use super::{TextPathInput, MAX_GROUP_DEPTH};

// =========================================================================
// textPath resolution
// =========================================================================

/// Walks the parsed XML tree, locates `<textPath>` nodes whose
/// `xlink:href` matches an entry in `text_paths`, resolves the
/// referenced `<path>` definition, and emits one [`Fill`] per
/// pre-shaped glyph translated onto the path's cumulative-advance
/// position.
///
/// Glyphs are placed axis-aligned only, no tangent rotation. Glyph
/// outlines come back in font design units (y-up); we flip y while
/// scaling by `font_size / upem` so the result lives in the SVG
/// document's user-space (y-down) alongside the rest of the parsed
/// fills.
///
/// All failures (missing href, missing def, non-`<path>` def,
/// un-parseable `d`, glyph outline lookup error, advance past path
/// length) silently drop the offending glyph or run, matching the
/// rest of the SVG-subset policy.
#[allow(clippy::too_many_arguments)]
pub(super) fn append_text_path_fills(
    doc: &mut SvgDoc,
    root: &Node,
    defs: &Defs<'_>,
    face: &Face<'_>,
    coords: &[f32],
    upem: f32,
    text_paths: &[TextPathInput<'_>],
    foreground: [u8; 4],
) {
    let ctx = ElemCtx {
        current_color: foreground,
        ..ElemCtx::default()
    };
    walk_for_text_paths(root, doc, defs, &ctx, face, coords, upem, text_paths, 0);
}

#[allow(clippy::too_many_arguments)]
fn walk_for_text_paths(
    node: &Node,
    doc: &mut SvgDoc,
    defs: &Defs<'_>,
    parent: &ElemCtx,
    face: &Face<'_>,
    coords: &[f32],
    upem: f32,
    text_paths: &[TextPathInput<'_>],
    depth: u32,
) {
    if depth > MAX_GROUP_DEPTH {
        return;
    }
    if doc_full(doc, defs) || !defs.charge_work(visit_cost(node, parent)) {
        return;
    }
    let ctx = inherit_attrs(parent, node);

    if name_eq(&node.name, "textPath") {
        let href = node
            .attr("href")
            .or_else(|| node.attr("xlink:href"))
            .unwrap_or("");
        let id = href.strip_prefix('#').unwrap_or("");
        if !id.is_empty() {
            if let Some(input) = text_paths.iter().find(|t| t.text_path_id == id) {
                emit_text_path_fills(doc, defs, &ctx, face, coords, upem, id, input);
            }
        }
        // `<textPath>` doesn't recurse into structural children for
        // text-content extraction. The consumer-shaped runs are the
        // sole source. Stop here.
        return;
    }

    for child in &node.children {
        walk_for_text_paths(
            child,
            doc,
            defs,
            &ctx,
            face,
            coords,
            upem,
            text_paths,
            depth + 1,
        );
        if doc_full(doc, defs) {
            break;
        }
    }
}

/// Emits one [`Fill`] per pre-shaped glyph in `input`, placed along
/// the `<path id=path_id>` defined elsewhere in `defs`.
#[allow(clippy::too_many_arguments)]
fn emit_text_path_fills(
    doc: &mut SvgDoc,
    defs: &Defs<'_>,
    ctx: &ElemCtx,
    face: &Face<'_>,
    coords: &[f32],
    upem: f32,
    path_id: &str,
    input: &TextPathInput<'_>,
) {
    let Some(target) = defs.lookup(path_id) else {
        return;
    };
    if !name_eq(&target.name, "path") {
        return;
    }
    let Some(d_attr) = target.attr("d") else {
        return;
    };
    // Each matching `<textPath>` re-parses and re-flattens the
    // referenced path, so both costs come out of the work budget.
    if !defs.charge_work(d_attr.len()) {
        return;
    }
    let Ok(path_ops) = parse_path_d(d_attr) else {
        return;
    };
    if path_ops.is_empty() {
        return;
    }

    // Path lives in document user-space; flatten in identity so chord
    // coordinates land on the same space the rest of the SvgDoc fills
    // already use. The world transform (doc -> pixel) is applied per
    // Fill at raster time, so we don't double-apply it here.
    let polyline = build_arc_length_polyline(&path_ops);
    if polyline.is_empty() || !defs.charge_work(polyline.len()) {
        return;
    }
    let total = polyline.last().map_or(0.0, |p| p.cum);
    if total <= 0.0 {
        return;
    }

    // Scale design units to user-space units. Y is flipped because
    // OT outlines are y-up and SVG document space is y-down.
    let scale = input.font_size / upem;

    let fill_paint = resolve_fill_paint(defs, ctx).unwrap_or(Paint::Solid([0, 0, 0, 255]));

    let mut cum = 0.0_f32;
    for g in &input.glyph_runs {
        if doc_full(doc, defs) {
            break;
        }
        if cum > total {
            break;
        }
        // Locating the glyph scans the polyline.
        if !defs.charge_work(polyline.len()) {
            break;
        }
        let Some(pos) = sample_polyline_position(&polyline, cum) else {
            break;
        };
        if let Ok(Some(outline)) = face.glyph_outline_at_coords(g.gid, coords) {
            if !outline.is_empty() {
                let translated = transform_outline_ops(outline.ops(), scale, pos.0, pos.1);
                if !translated.is_empty() {
                    push_fill(
                        doc,
                        defs,
                        Fill {
                            ops: translated,
                            paint: fill_paint.clone(),
                            xform: ctx.xform,
                            clip: ctx
                                .clip_href
                                .as_deref()
                                .and_then(|id| resolve_clip_shape(defs, id)),
                            #[cfg(test)]
                            is_stroke: false,
                            filter: ctx
                                .filter_href
                                .as_deref()
                                .and_then(|id| resolve_filter(defs, id)),
                            mask: ctx
                                .mask_href
                                .as_deref()
                                .and_then(|id| resolve_mask_shape(defs, id)),
                        },
                    );
                }
            }
        }
        cum += g.x_advance;
    }
}

/// One sample along the cumulative-arc-length polyline of a flattened
/// path. `cum` is the arc-length distance from the path start; `(x,y)`
/// are the user-space coordinates at that distance.
#[derive(Debug, Clone, Copy)]
pub(super) struct PolyPoint {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) cum: f32,
}

/// Flattens `ops` and converts the resulting [`Segment`] list into a
/// cumulative-arc-length polyline. The first point sits at `cum = 0`
/// at the path's first MoveTo; each subsequent point appends one
/// chord's length onto the running total.
///
/// Multi-contour paths concatenate their per-contour polylines back to
/// back. The cumulative-advance walk treats them as one continuous
/// stroke for placement, matching the contract documented on
/// [`TextPathInput`]. Tangent rotation and per-contour breaks are not
/// supported.
///
/// The path is walked as drawn: a subpath without `Close` stays open,
/// so an open reference path gets no return leg added to its length.
pub(super) fn build_arc_length_polyline(ops: &[PathOp]) -> Vec<PolyPoint> {
    let segs = flatten(
        ops.iter().copied(),
        &Affine::identity(),
        DEFAULT_TOLERANCE_LOCAL,
    );
    let mut out = Vec::with_capacity(segs.len() + 1);
    let mut cum = 0.0_f32;
    for (i, s) in segs.iter().enumerate() {
        if i == 0 {
            out.push(PolyPoint {
                x: s.x0,
                y: s.y0,
                cum,
            });
        }
        let dx = s.x1 - s.x0;
        let dy = s.y1 - s.y0;
        let len = (dx * dx + dy * dy).sqrt();
        cum += len;
        out.push(PolyPoint {
            x: s.x1,
            y: s.y1,
            cum,
        });
    }
    out
}

/// Local copy of [`crate::flatten::DEFAULT_TOLERANCE`] held here so
/// the textPath flattener keeps a stable subdivision policy
/// independent of the top-level rasterizer's runtime tolerance:
/// arc-length walks want consistent chord lengths across calls.
const DEFAULT_TOLERANCE_LOCAL: f32 = crate::flatten::DEFAULT_TOLERANCE;

/// Linear-interpolates a position on the polyline at cumulative
/// arc-length `target`. Returns `None` if `target` is past the
/// polyline's total length.
pub(super) fn sample_polyline_position(poly: &[PolyPoint], target: f32) -> Option<(f32, f32)> {
    if poly.is_empty() {
        return None;
    }
    if target <= 0.0 {
        return Some((poly[0].x, poly[0].y));
    }
    for w in poly.windows(2) {
        let a = w[0];
        let b = w[1];
        if target <= b.cum {
            let span = b.cum - a.cum;
            if span <= 0.0 {
                return Some((b.x, b.y));
            }
            let t = (target - a.cum) / span;
            return Some((a.x + t * (b.x - a.x), a.y + t * (b.y - a.y)));
        }
    }
    None
}

/// Translates and scales an outline's `PathOp`s so the design-unit
/// origin lands at `(ox, oy)` in user-space. The y-axis is flipped
/// because OT outlines are y-up and SVG document space is y-down.
pub(super) fn transform_outline_ops(ops: &[PathOp], scale: f32, ox: f32, oy: f32) -> Vec<PathOp> {
    let map = |x: f32, y: f32| -> (f32, f32) { (ox + x * scale, oy - y * scale) };
    let mut out = Vec::with_capacity(ops.len());
    for op in ops {
        match *op {
            PathOp::MoveTo { x, y } => {
                let (nx, ny) = map(x, y);
                out.push(PathOp::MoveTo { x: nx, y: ny });
            }
            PathOp::LineTo { x, y } => {
                let (nx, ny) = map(x, y);
                out.push(PathOp::LineTo { x: nx, y: ny });
            }
            PathOp::QuadTo { cx, cy, x, y } => {
                let (cx2, cy2) = map(cx, cy);
                let (nx, ny) = map(x, y);
                out.push(PathOp::QuadTo {
                    cx: cx2,
                    cy: cy2,
                    x: nx,
                    y: ny,
                });
            }
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => {
                let (a1, b1) = map(c1x, c1y);
                let (a2, b2) = map(c2x, c2y);
                let (nx, ny) = map(x, y);
                out.push(PathOp::CubicTo {
                    c1x: a1,
                    c1y: b1,
                    c2x: a2,
                    c2y: b2,
                    x: nx,
                    y: ny,
                });
            }
            PathOp::Close => out.push(PathOp::Close),
        }
    }
    out
}
