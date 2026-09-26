//! Top-level document parse: the definitions table, the inherited
//! element context, and the tree walk that emits fills.

use alloc::string::String;
use alloc::vec::Vec;

use sigilbuzz::tables::PathOp;

use crate::affine::Affine;
use crate::error::RenderError;
use crate::rasterizer::Rasterizer;

use super::clip_mask::{resolve_clip_shape, resolve_mask_shape};
use super::filter::resolve_filter;
use super::model::{Fill, Paint, SvgDoc};
use super::paint_server::{is_fully_transparent, resolve_fill_paint};
use super::path::{
    circle_to_path, ellipse_to_path, line_to_path, parse_path_d, polygon_to_path, polyline_to_path,
    rect_to_path,
};
use super::stroke::stroke_to_fill;
use super::style::{inherit_attrs, parse_length, parse_viewbox};
use super::xml::{attr_matches, name_eq, parse_xml, Node};
use super::{MAX_FILLS, MAX_GROUP_DEPTH, MAX_USE_DEPTH};

// =========================================================================
// Top-level parse
// =========================================================================

#[cfg(test)]
pub(super) fn parse_document(xml: &str) -> Result<SvgDoc, RenderError> {
    parse_document_with(xml, Rasterizer::DEFAULT_FOREGROUND)
}

/// Parses `xml` with `currentColor` starting as `foreground`, the text
/// color the OpenType SVG spec hands a glyph document.
pub(super) fn parse_document_with(xml: &str, foreground: [u8; 4]) -> Result<SvgDoc, RenderError> {
    let root = parse_xml(xml)?;
    if !name_eq(&root.name, "svg") {
        return Err(RenderError::Parse("svg root"));
    }

    let mut doc = SvgDoc {
        view_w: 1000.0,
        view_h: 1000.0,
        view_x: 0.0,
        view_y: 0.0,
        fills: Vec::new(),
    };

    let mut vb_seen = false;
    for (k, v) in &root.attrs {
        if attr_matches(k, "viewBox") {
            if let Some((x, y, w, h)) = parse_viewbox(v) {
                doc.view_x = x;
                doc.view_y = y;
                doc.view_w = w;
                doc.view_h = h;
                vb_seen = true;
            }
        } else if !vb_seen && attr_matches(k, "width") {
            if let Some(w) = parse_length(v) {
                doc.view_w = w;
            }
        } else if !vb_seen && attr_matches(k, "height") {
            if let Some(h) = parse_length(v) {
                doc.view_h = h;
            }
        }
    }

    // First pass: collect every element that carries `id=` so `<use>`
    // and `fill="url(#...)"` can resolve forward references. We just
    // index by id; the renderer walks the tree itself.
    let mut defs = Defs::default();
    collect_defs(&root, &mut defs);

    // Second pass: walk the tree, emitting fills.
    let ctx = ElemCtx {
        current_color: foreground,
        ..ElemCtx::default()
    };
    walk(&root, &mut doc, &defs, &ctx, 0, 0)?;

    Ok(doc)
}

#[derive(Default)]
pub(super) struct Defs<'a> {
    by_id: Vec<(&'a str, &'a Node)>,
}

impl<'a> Defs<'a> {
    pub(super) fn lookup(&self, id: &str) -> Option<&'a Node> {
        for (k, v) in &self.by_id {
            if *k == id {
                return Some(*v);
            }
        }
        None
    }
}

pub(super) fn collect_defs<'a>(node: &'a Node, defs: &mut Defs<'a>) {
    if let Some(id) = node.id() {
        defs.by_id.push((id, node));
    }
    for c in &node.children {
        collect_defs(c, defs);
    }
}

#[derive(Debug, Clone)]
pub(super) struct ElemCtx {
    pub(super) xform: Affine,
    /// Inherited fill color (straight RGBA). `None` means "use solid
    /// black" at paint time, matching the SVG default. Tracked
    /// separately from gradient paint so cascading respects both.
    pub(super) fill_color: Option<[u8; 4]>,
    /// `currentColor`: the text foreground color, or the nearest
    /// `color` attribute.
    pub(super) current_color: [u8; 4],
    /// Inherited gradient href (when `fill="url(#id)"`). Resolved at
    /// paint time so the cascade stays simple.
    pub(super) fill_grad_href: Option<String>,
    /// Inherited fill-opacity factor in `[0, 1]`.
    pub(super) fill_opacity: f32,
    /// Element-level opacity factor in `[0, 1]`.
    pub(super) opacity: f32,
    /// Stroke color (None = no stroke, default).
    pub(super) stroke_color: Option<[u8; 4]>,
    pub(super) stroke_width: f32,
    pub(super) stroke_linecap: LineCap,
    pub(super) stroke_linejoin: LineJoin,
    /// Inherited stroke-opacity factor in `[0, 1]`.
    pub(super) stroke_opacity: f32,
    /// Parsed `stroke-dasharray`. Empty means "no dashing". Odd-length
    /// lists are normalized to even length by [`parse_dasharray`](super::dash::parse_dasharray).
    pub(super) stroke_dasharray: Vec<f32>,
    /// `stroke-dashoffset` (in user-space units), applied at the start
    /// of every contour.
    pub(super) stroke_dashoffset: f32,
    /// Active clip-path href, applied to every fill / stroke produced
    /// inside this subtree. Stored as the bare id (no `url(#...)` form).
    pub(super) clip_href: Option<String>,
    /// Active filter href (`filter="url(#id)"`). Stored as the bare id.
    /// Inherited like `clip_href`; resolved against the document `Defs`
    /// at emit time to a [`Filter`](super::model::Filter) cloned onto each Fill.
    pub(super) filter_href: Option<String>,
    /// Active mask href (`mask="url(#id)"`). Stored as the bare id.
    /// Inherited like `clip_href` / `filter_href`; resolved against
    /// the document `Defs` at emit time to a [`MaskShape`](super::model::MaskShape).
    pub(super) mask_href: Option<String>,
    /// Cycle-guard for nested mask resolution. `resolve_mask_shape`
    /// bumps this when it walks the mask body so any descendant
    /// `mask="url(#...)"` reference (including the cyclic
    /// `<mask id=a>...<rect mask=url(#b)>...<mask id=b>...<rect mask=url(#a)>`
    /// case) is dropped at `emit_paint` rather than recursing back
    /// into the resolver. Keeps the stack bounded at the documented
    /// "mask-of-mask is unsupported" semantics.
    pub(super) mask_depth: u8,
}

impl Default for ElemCtx {
    fn default() -> Self {
        Self {
            xform: Affine::identity(),
            fill_color: None,
            current_color: Rasterizer::DEFAULT_FOREGROUND,
            fill_grad_href: None,
            fill_opacity: 1.0,
            opacity: 1.0,
            stroke_color: None,
            stroke_width: 1.0,
            stroke_linecap: LineCap::Butt,
            stroke_linejoin: LineJoin::Miter,
            stroke_opacity: 1.0,
            stroke_dasharray: Vec::new(),
            stroke_dashoffset: 0.0,
            clip_href: None,
            filter_href: None,
            mask_href: None,
            mask_depth: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum LineJoin {
    Miter,
    Round,
    Bevel,
}

pub(super) fn walk(
    node: &Node,
    doc: &mut SvgDoc,
    defs: &Defs<'_>,
    parent: &ElemCtx,
    depth: u32,
    use_depth: u32,
) -> Result<(), RenderError> {
    if depth > MAX_GROUP_DEPTH {
        return Err(RenderError::Parse("svg nesting"));
    }
    if doc.fills.len() >= MAX_FILLS {
        return Err(RenderError::Parse("svg fill cap"));
    }

    // Skip elements that contribute no rendering: <defs>, <linearGradient>,
    // <radialGradient>, <clipPath>, <mask>, <filter>, <stop>, <metadata>,
    // <title>, <desc>. They were already harvested by `collect_defs` for
    // href resolution; <mask> is materialized lazily by `resolve_mask_shape`.
    if name_eq(&node.name, "defs")
        || name_eq(&node.name, "linearGradient")
        || name_eq(&node.name, "radialGradient")
        || name_eq(&node.name, "clipPath")
        || name_eq(&node.name, "mask")
        || name_eq(&node.name, "filter")
        || name_eq(&node.name, "metadata")
        || name_eq(&node.name, "title")
        || name_eq(&node.name, "desc")
        || name_eq(&node.name, "stop")
    {
        return Ok(());
    }

    let ctx = inherit_attrs(parent, node);

    if name_eq(&node.name, "svg") || name_eq(&node.name, "g") {
        for child in &node.children {
            walk(child, doc, defs, &ctx, depth + 1, use_depth)?;
            if doc.fills.len() >= MAX_FILLS {
                break;
            }
        }
        return Ok(());
    }

    if name_eq(&node.name, "use") {
        if use_depth >= MAX_USE_DEPTH {
            // Recursion guard: silently drop deeper expansions.
            return Ok(());
        }
        // xlink:href / href = "#id"
        let href = node
            .attr("href")
            .or_else(|| node.attr("xlink:href"))
            .unwrap_or("");
        let id = href.strip_prefix('#').unwrap_or("");
        if id.is_empty() {
            return Ok(());
        }
        let Some(target) = defs.lookup(id) else {
            return Ok(());
        };
        // Apply the use's local x/y as a pre-translate, on top of any
        // transform inherited from the use itself (already folded into
        // ctx.xform by `inherit_attrs`).
        let ux = node.attr("x").and_then(parse_length).unwrap_or(0.0);
        let uy = node.attr("y").and_then(parse_length).unwrap_or(0.0);
        let mut child_ctx = ctx.clone();
        if ux != 0.0 || uy != 0.0 {
            child_ctx.xform = child_ctx.xform.compose(&Affine::translate(ux, uy));
        }
        // Walk the referenced element with the use's context. Reset
        // the group-nesting counter: `<use>` expansion is flattening,
        // not source-level nesting, so the only relevant cap is
        // `MAX_USE_DEPTH`.
        walk(target, doc, defs, &child_ctx, 0, use_depth + 1)?;
        return Ok(());
    }

    // Shape-bearing elements.
    let path_ops: Option<Vec<PathOp>> = if name_eq(&node.name, "path") {
        node.attr("d").map(parse_path_d).transpose()?
    } else if name_eq(&node.name, "rect") {
        Some(rect_to_path(node))
    } else if name_eq(&node.name, "circle") {
        Some(circle_to_path(node))
    } else if name_eq(&node.name, "ellipse") {
        Some(ellipse_to_path(node))
    } else if name_eq(&node.name, "polygon") {
        Some(polygon_to_path(node))
    } else if name_eq(&node.name, "polyline") {
        Some(polyline_to_path(node))
    } else if name_eq(&node.name, "line") {
        Some(line_to_path(node))
    } else {
        None
    };

    if let Some(ops) = path_ops {
        if !ops.is_empty() {
            emit_paint(doc, defs, &ctx, &ops);
        }
        // path elements don't normally have render-bearing children.
    } else {
        // Walk children of any unknown element so wrapping <text> / <a>
        // / <symbol> don't swallow visible content.
        for child in &node.children {
            walk(child, doc, defs, &ctx, depth + 1, use_depth)?;
            if doc.fills.len() >= MAX_FILLS {
                break;
            }
        }
    }
    Ok(())
}

/// Parses a `url(#id)` reference, returning `id`. Tolerates whitespace
/// and either single or double quote bodies inside the `url(...)` body
/// (some authoring tools emit them).
pub(super) fn parse_url_ref(s: &str) -> Option<String> {
    let trimmed = s.trim();
    let inner = trimmed
        .strip_prefix("url(")
        .or_else(|| trimmed.strip_prefix("URL("))?
        .strip_suffix(')')?
        .trim();
    let inner = inner.trim_matches(|c| c == '"' || c == '\'');
    let stripped = inner.strip_prefix('#')?;
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.into())
    }
}

/// Pushes one or more fills (and stroke fills) for `ops` under `ctx`.
fn emit_paint(doc: &mut SvgDoc, defs: &Defs<'_>, ctx: &ElemCtx, ops: &[PathOp]) {
    // Resolve the clip shape once per emission.
    let clip = ctx
        .clip_href
        .as_deref()
        .and_then(|id| resolve_clip_shape(defs, id));

    // Resolve the filter chain once per emission. Unrecognized /
    // missing filter ids degrade to "no filter", which matches browser
    // behavior and keeps a typo from blanking the glyph.
    let filter = ctx
        .filter_href
        .as_deref()
        .and_then(|id| resolve_filter(defs, id));

    // Resolve the mask once per emission. Same fallback policy as
    // filter / clipPath: unknown id silently drops the mask rather
    // than blanking the glyph.
    //
    // `mask_depth` is the cycle-guard: once we are inside a
    // `resolve_mask_shape` walk (depth > 0), drop any nested mask
    // reference so a `<mask id=a>...<rect mask=url(#b)>...<mask
    // id=b>...<rect mask=url(#a)>` document can't recurse the
    // resolver into a stack overflow. mask-of-mask is documented as
    // deferred; this enforces it.
    let mask = if ctx.mask_depth == 0 {
        ctx.mask_href
            .as_deref()
            .and_then(|id| resolve_mask_shape(defs, id))
    } else {
        None
    };

    // Fill pass.
    let fill_paint = resolve_fill_paint(defs, ctx);
    if let Some(p) = fill_paint {
        if !is_fully_transparent(&p) {
            doc.fills.push(Fill {
                ops: ops.to_vec(),
                paint: p,
                xform: ctx.xform,
                clip: clip.clone(),
                is_stroke: false,
                filter: filter.clone(),
                mask: mask.clone(),
            });
        }
    }

    // Stroke pass.
    if let Some(scol) = ctx.stroke_color {
        if ctx.stroke_width > 0.0 {
            let alpha = (scol[3] as f32 / 255.0) * ctx.stroke_opacity * ctx.opacity;
            let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
            if a > 0 {
                let rgba = [scol[0], scol[1], scol[2], a];
                let stroke_ops = stroke_to_fill(
                    ops,
                    ctx.stroke_width,
                    ctx.stroke_linecap,
                    ctx.stroke_linejoin,
                    &ctx.stroke_dasharray,
                    ctx.stroke_dashoffset,
                );
                if !stroke_ops.is_empty() && doc.fills.len() < MAX_FILLS {
                    doc.fills.push(Fill {
                        ops: stroke_ops,
                        paint: Paint::Solid(rgba),
                        xform: ctx.xform,
                        clip: clip.clone(),
                        is_stroke: true,
                        filter: filter.clone(),
                        mask: mask.clone(),
                    });
                }
            }
        }
    }
}
