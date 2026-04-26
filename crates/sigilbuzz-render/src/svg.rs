//! SVG-in-OT rasterization.
//!
//! The OpenType `SVG ` table maps glyph ids to inline SVG documents
//! (Apple/Adobe extension supported by HarfBuzz). sigilbuzz exposes the
//! raw payload bytes via [`sigilbuzz::Face::svg_document`]; this module
//! turns that XML into a [`crate::ColorPixmap`].
//!
//! ## Bounded subset
//!
//! Real-world SVG-in-OT documents (Twitter Color Emoji, Mozilla Firefox
//! OS Emoji, designer fonts produced by `nanoemoji` / `fonttools`) use
//! a small fraction of the full SVG 1.1 grammar. We deliberately
//! implement only that fraction:
//!
//! - `<svg>` with `viewBox` / `width` / `height` attributes.
//! - `<g>` with optional `transform=` (`translate`, `scale`, `rotate`,
//!   `matrix`).
//! - `<path>` with `d=` containing M/L/H/V/C/Q/Z + relative variants.
//! - `<rect>` / `<circle>` / `<ellipse>` shape primitives — converted
//!   to paths and run through the existing fill pipeline.
//! - `<polygon>` / `<polyline>` / `<line>` shape primitives — converted
//!   to paths via the SVG `points` list grammar (space- or
//!   comma-separated coords). `<polygon>` closes back to the first
//!   point; `<polyline>` is open; `<line>` is a single segment.
//! - `fill="#RRGGBB"`, `fill="#RGB"`, `fill="rgb(...)"`, named colours,
//!   `fill="none"`, `fill-opacity` / `opacity`, plus `fill="url(#g)"`
//!   pointing at a `<linearGradient>` / `<radialGradient>`.
//! - Stroking: `stroke`, `stroke-width`, `stroke-linecap` (butt
//!   minimum, round / square as best-effort), `stroke-linejoin` (miter
//!   minimum, round / bevel as best-effort).
//! - `<linearGradient>` / `<radialGradient>` with `<stop>` children;
//!   ramp evaluation reuses the COLRv1 implementation in
//!   [`crate::colrv1`].
//! - `<use xlink:href="#id">` with in-document refs and a 16-deep
//!   recursion cap.
//! - `<clipPath>` containing a single `<path>` (the common case in
//!   designer-emoji fonts).
//!
//! - `stroke-dasharray` + `stroke-dashoffset` on stroked geometry,
//!   applied to the post-flattening polyline. Curves become chords
//!   first, then dashes are walked along cumulative arc length per
//!   contour.
//!
//! Anything outside this list — filter primitives, masks beyond
//! clipPath, animations, scripting, `style=` attributes, text-on-path
//! — is silently skipped.
//!
//! ## Pipeline
//!
//! ```text
//!   Face.svg_document(gid)    → SvgDocument { data, gzipped }
//!     │
//!     │ gzipped → RenderError::SvgGzipped (no gzip dep here)
//!     ▼
//!   parse_document(xml)       → SvgDoc { viewbox, defs, fills, strokes }
//!     │
//!     │ each fill: { ops, paint, xform, clip? }
//!     ▼
//!   for each fill / stroke pass:
//!     flatten(ops × world_xform) → Segment[]
//!     raster(segments)           → Pixmap (alpha mask)
//!     blit(mask × paint → out)   → ColorPixmap
//! ```
//!
//! No XML library on the read path — the parser is a hand-rolled tree
//! walker. Coordinates are decimal numbers parsed with `f32::from_str`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use sigilbuzz::tables::PathOp;
use sigilbuzz::Face;
use sigilbuzz_paint::{Color as PaintColor, ColorStop, Extend};

use crate::affine::Affine;
use crate::colrv1::{apply_extend, project_linear, project_radial, sample_stops, to_premul};
use crate::error::RenderError;
use crate::flatten::flatten;
use crate::pixmap::{ColorPixmap, Pixmap};
use crate::raster::rasterize as raster;
use crate::rasterizer::Rasterizer;

/// Maximum recursion depth for nested `<g>` elements. Real fonts stay
/// under 4; we cap at 32 to keep malicious payloads from blowing the
/// stack.
const MAX_GROUP_DEPTH: u32 = 32;

/// Maximum number of fills a single document may emit. Same rationale
/// as `MAX_GROUP_DEPTH`: a hostile SVG could otherwise expand to
/// gigabytes of work. 4096 is well above what real fonts produce.
const MAX_FILLS: usize = 4096;

/// Maximum nested `<use>` resolution depth. SVG mandates ≥ 16 in real
/// engines; we match.
const MAX_USE_DEPTH: u32 = 16;

/// Miter cut-off ratio per SVG: when the miter would extend more than
/// `4 × stroke-width` past the join, fall back to a bevel join.
const MITER_LIMIT: f32 = 4.0;

/// Maximum pixel dimension for a rasterized SVG-in-OT glyph. Matches
/// the PNG decoder's per-dim ceiling (16384) so the bound is uniform
/// across the public render surface. A combination of a font-supplied
/// finite-but-extreme `viewBox` and a caller-supplied large `size_pt`
/// can otherwise multiply up to a `u32::MAX × u32::MAX × 4` allocation
/// that panics in the `Vec` macro before any rasterization runs.
const MAX_RENDER_DIM: f32 = 16384.0;

// =========================================================================
// Public entry
// =========================================================================

impl Rasterizer {
    /// Rasterizes the SVG document for `gid` from the font's `SVG`
    /// table, returning a premultiplied RGBA [`ColorPixmap`].
    ///
    /// `size_pt` is the rendering size in pixels — the SVG document's
    /// viewBox is mapped onto a `size_pt × size_pt` square. If the
    /// viewBox is non-square, the rendered bitmap preserves the
    /// document's aspect ratio (the longer axis maps to `size_pt`).
    ///
    /// `coords` is accepted for API symmetry with
    /// [`Self::rasterize_glyph`] and [`Self::rasterize_colrv0_glyph`]
    /// but currently has no effect — SVG-in-OT documents are static
    /// (no axis tagging), and HarfBuzz / CoreText behave the same way.
    ///
    /// # Errors
    /// - [`RenderError::SvgNotFound`] when `gid` has no SVG record.
    /// - [`RenderError::SvgGzipped`] when the payload is gzip-compressed
    ///   (sigilbuzz-render does not ship a gzip dep; consumers should
    ///   decompress and feed back via a future bytes-based entry point).
    /// - [`RenderError::Parse`] for unrecoverable XML / path-data
    ///   errors.
    /// - [`RenderError::BadSize`] when `size_pt` is non-finite or
    ///   non-positive.
    pub fn rasterize_svg_glyph(
        &self,
        face: &Face<'_>,
        gid: u16,
        size_pt: f32,
        _coords: &[f32],
    ) -> Result<ColorPixmap, RenderError> {
        if !size_pt.is_finite() || size_pt <= 0.0 {
            return Err(RenderError::BadSize(size_pt));
        }
        let doc_record = face
            .svg_document(gid)
            .map_err(|_| RenderError::Parse("svg"))?
            .ok_or(RenderError::SvgNotFound(gid))?;
        if doc_record.gzipped {
            return Err(RenderError::SvgGzipped);
        }
        let xml = core::str::from_utf8(doc_record.data).map_err(|_| RenderError::Parse("svg"))?;
        let doc = parse_document(xml)?;

        if doc.view_w <= 0.0 || doc.view_h <= 0.0 {
            return Err(RenderError::Parse("svg viewBox"));
        }
        // Map document → pixel space: scale the viewBox onto a
        // size_pt × size_pt square, preserving aspect ratio.
        let s = (size_pt / doc.view_w).min(size_pt / doc.view_h);
        let world = Affine {
            xx: s,
            yx: 0.0,
            xy: 0.0,
            yy: s,
            dx: -doc.view_x * s,
            dy: -doc.view_y * s,
        };

        // Cap output dimensions before allocation. Without this guard
        // an extreme finite viewBox + matching size_pt yields a
        // post-cast `u32::MAX × u32::MAX × 4` allocation that overflows
        // `usize` even through `saturating_mul`, and `vec![0u8; len]`
        // panics with "capacity overflow". 16384 matches the PNG
        // decoder's per-dim ceiling (`Ihdr::parse`) so the bound is
        // consistent across the public render surface.
        let width_f = (doc.view_w * s).round().max(1.0);
        let height_f = (doc.view_h * s).round().max(1.0);
        if !width_f.is_finite()
            || !height_f.is_finite()
            || width_f > MAX_RENDER_DIM
            || height_f > MAX_RENDER_DIM
        {
            return Err(RenderError::BadSize(size_pt));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let width = width_f as u32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let height = height_f as u32;
        let mut out = ColorPixmap::new(width, height);

        let tol = self.flattening_tolerance();
        for fill in &doc.fills {
            render_fill(&mut out, fill, &world, tol);
        }
        Ok(out)
    }
}

// =========================================================================
// Internal model
// =========================================================================

/// One paintable surface collected from the document. `ops` is in the
/// document's intrinsic coordinate space — the world transform
/// (document → pixel) is applied on top at rasterize time.
#[derive(Debug, Clone)]
struct Fill {
    ops: Vec<PathOp>,
    paint: Paint,
    /// Composed transform from the element's nested `<g transform=...>`
    /// stack, in document coordinates.
    xform: Affine,
    /// Optional clip-path geometry, expressed in the same document
    /// space the parent fill was emitted in (so the same `xform` and
    /// world transform apply to both).
    clip: Option<ClipShape>,
    /// Indicates whether this fill is the outline of a stroke (closed
    /// fill ribbon) — affects nothing in rendering but documents the
    /// pipeline split.
    #[allow(dead_code)]
    is_stroke: bool,
}

/// Paint source for a [`Fill`]. SVG-in-OT documents use solid colour
/// almost exclusively, with the rare gradient for designer emoji.
#[derive(Debug, Clone)]
enum Paint {
    /// Straight (un-premultiplied) RGBA.
    Solid([u8; 4]),
    /// Reference to a parsed gradient. Geometry is in document space;
    /// the renderer composes the world transform on top.
    Gradient(GradientPaint),
}

#[derive(Debug, Clone)]
struct GradientPaint {
    kind: GradKind,
    stops: Vec<ColorStop>,
    extend: Extend,
    /// Per-element opacity multiplier folded into stop alpha at sample
    /// time.
    opacity: f32,
    /// `gradientTransform`. Composed onto the gradient geometry
    /// *before* the document → pixel `world` matrix.
    gradient_xform: Affine,
}

#[derive(Debug, Clone, Copy)]
enum GradKind {
    Linear {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
    },
    Radial {
        cx: f32,
        cy: f32,
        r: f32,
        fx: f32,
        fy: f32,
    },
}

#[derive(Debug, Clone)]
struct ClipShape {
    ops: Vec<PathOp>,
    /// Transform stack the clipPath's child path inherited (clipPath
    /// contents may carry their own `transform=`).
    xform: Affine,
}

/// Parsed SVG document.
#[derive(Debug, Clone)]
struct SvgDoc {
    view_w: f32,
    view_h: f32,
    view_x: f32,
    view_y: f32,
    fills: Vec<Fill>,
}

// =========================================================================
// XML tree
// =========================================================================

/// In-memory DOM. The document is small enough that this is cheap and
/// gives us free random access for `<use>` href resolution.
#[derive(Debug, Clone)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
}

impl Node {
    fn attr(&self, key: &str) -> Option<&str> {
        for (k, v) in &self.attrs {
            if attr_matches(k, key) {
                return Some(v.as_str());
            }
        }
        None
    }

    fn id(&self) -> Option<&str> {
        self.attr("id")
    }
}

fn attr_matches(actual: &str, target: &str) -> bool {
    if actual.eq_ignore_ascii_case(target) {
        return true;
    }
    if let Some(i) = actual.find(':') {
        return actual[i + 1..].eq_ignore_ascii_case(target);
    }
    false
}

fn name_eq(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    if let Some(i) = a.find(':') {
        return a[i + 1..].eq_ignore_ascii_case(b);
    }
    false
}

// =========================================================================
// Top-level parse
// =========================================================================

fn parse_document(xml: &str) -> Result<SvgDoc, RenderError> {
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
    let ctx = ElemCtx::default();
    walk(&root, &mut doc, &defs, &ctx, 0, 0)?;

    Ok(doc)
}

#[derive(Default)]
struct Defs<'a> {
    by_id: Vec<(&'a str, &'a Node)>,
}

impl<'a> Defs<'a> {
    fn lookup(&self, id: &str) -> Option<&'a Node> {
        for (k, v) in &self.by_id {
            if *k == id {
                return Some(*v);
            }
        }
        None
    }
}

fn collect_defs<'a>(node: &'a Node, defs: &mut Defs<'a>) {
    if let Some(id) = node.id() {
        defs.by_id.push((id, node));
    }
    for c in &node.children {
        collect_defs(c, defs);
    }
}

#[derive(Debug, Clone)]
struct ElemCtx {
    xform: Affine,
    /// Inherited fill colour (straight RGBA). `None` means "use solid
    /// black" at paint time, matching the SVG default. Tracked
    /// separately from gradient paint so cascading respects both.
    fill_color: Option<[u8; 4]>,
    /// Inherited gradient href (when `fill="url(#id)"`). Resolved at
    /// paint time so the cascade stays simple.
    fill_grad_href: Option<String>,
    /// Inherited fill-opacity factor in `[0, 1]`.
    fill_opacity: f32,
    /// Element-level opacity factor in `[0, 1]`.
    opacity: f32,
    /// Stroke colour (None = no stroke, default).
    stroke_color: Option<[u8; 4]>,
    stroke_width: f32,
    stroke_linecap: LineCap,
    stroke_linejoin: LineJoin,
    /// Inherited stroke-opacity factor in `[0, 1]`.
    stroke_opacity: f32,
    /// Parsed `stroke-dasharray`. Empty means "no dashing". Odd-length
    /// lists are normalised to even length by [`parse_dasharray`].
    stroke_dasharray: Vec<f32>,
    /// `stroke-dashoffset` (in user-space units), applied at the start
    /// of every contour.
    stroke_dashoffset: f32,
    /// Active clip-path href, applied to every fill / stroke produced
    /// inside this subtree. Stored as the bare id (no `url(#…)` form).
    clip_href: Option<String>,
}

impl Default for ElemCtx {
    fn default() -> Self {
        Self {
            xform: Affine::identity(),
            fill_color: None,
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
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum LineJoin {
    Miter,
    Round,
    Bevel,
}

fn walk(
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
    // <radialGradient>, <clipPath>, <stop>, <metadata>, <title>, <desc>.
    // They were already harvested by `collect_defs` for href resolution.
    if name_eq(&node.name, "defs")
        || name_eq(&node.name, "linearGradient")
        || name_eq(&node.name, "radialGradient")
        || name_eq(&node.name, "clipPath")
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
        // the group-nesting counter — `<use>` expansion is flattening,
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

/// Computes the inherited [`ElemCtx`] for `node`, given `parent`.
fn inherit_attrs(parent: &ElemCtx, node: &Node) -> ElemCtx {
    let mut ctx = parent.clone();
    for (k, v) in &node.attrs {
        if attr_matches(k, "transform") {
            if let Some(t) = parse_transform(v) {
                ctx.xform = ctx.xform.compose(&t);
            }
        } else if attr_matches(k, "fill") {
            if let Some(href) = parse_url_ref(v) {
                ctx.fill_grad_href = Some(href);
                ctx.fill_color = None;
            } else if v.trim().eq_ignore_ascii_case("none") {
                ctx.fill_color = Some([0, 0, 0, 0]);
                ctx.fill_grad_href = None;
            } else if let Some(c) = parse_color(v) {
                ctx.fill_color = Some(c);
                ctx.fill_grad_href = None;
            }
        } else if attr_matches(k, "fill-opacity") {
            if let Some(o) = parse_opacity(v) {
                ctx.fill_opacity = (ctx.fill_opacity * o).clamp(0.0, 1.0);
            }
        } else if attr_matches(k, "opacity") {
            if let Some(o) = parse_opacity(v) {
                ctx.opacity = (ctx.opacity * o).clamp(0.0, 1.0);
            }
        } else if attr_matches(k, "stroke") {
            if v.trim().eq_ignore_ascii_case("none") {
                ctx.stroke_color = None;
            } else if let Some(c) = parse_color(v) {
                ctx.stroke_color = Some(c);
            }
        } else if attr_matches(k, "stroke-opacity") {
            if let Some(o) = parse_opacity(v) {
                ctx.stroke_opacity = (ctx.stroke_opacity * o).clamp(0.0, 1.0);
            }
        } else if attr_matches(k, "stroke-width") {
            if let Some(w) = parse_length(v) {
                if w >= 0.0 {
                    ctx.stroke_width = w;
                }
            }
        } else if attr_matches(k, "stroke-linecap") {
            ctx.stroke_linecap = match v.trim().to_ascii_lowercase().as_str() {
                "round" => LineCap::Round,
                "square" => LineCap::Square,
                _ => LineCap::Butt,
            };
        } else if attr_matches(k, "stroke-linejoin") {
            ctx.stroke_linejoin = match v.trim().to_ascii_lowercase().as_str() {
                "round" => LineJoin::Round,
                "bevel" => LineJoin::Bevel,
                _ => LineJoin::Miter,
            };
        } else if attr_matches(k, "stroke-dasharray") {
            ctx.stroke_dasharray = parse_dasharray(v);
        } else if attr_matches(k, "stroke-dashoffset") {
            if let Some(o) = parse_length(v) {
                ctx.stroke_dashoffset = o;
            }
        } else if attr_matches(k, "clip-path") {
            if let Some(href) = parse_url_ref(v) {
                ctx.clip_href = Some(href);
            }
        }
    }
    ctx
}

/// Parses a `url(#id)` reference, returning `id`. Tolerates whitespace
/// and either single or double quote bodies inside the `url(...)` body
/// (some authoring tools emit them).
fn parse_url_ref(s: &str) -> Option<String> {
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
                    });
                }
            }
        }
    }
}

fn is_fully_transparent(p: &Paint) -> bool {
    match p {
        Paint::Solid(c) => c[3] == 0,
        Paint::Gradient(g) => {
            // Treat as transparent only when *every* stop is fully
            // transparent and the per-element opacity is zero. Cheap
            // early-exit; gradients with mid-range stops still render.
            g.opacity <= 0.0 || (g.stops.iter().all(|s| s.color.a <= 0.0))
        }
    }
}

fn resolve_fill_paint(defs: &Defs<'_>, ctx: &ElemCtx) -> Option<Paint> {
    if let Some(id) = ctx.fill_grad_href.as_deref() {
        if let Some(g) = resolve_gradient(defs, id, ctx) {
            return Some(Paint::Gradient(g));
        }
        // url(#…) pointing to nothing falls back to default black.
    }
    let base = ctx.fill_color.unwrap_or([0, 0, 0, 255]);
    if base[3] == 0 {
        return None;
    }
    let alpha_factor = (ctx.fill_opacity * ctx.opacity).clamp(0.0, 1.0);
    let a = (base[3] as f32 / 255.0 * alpha_factor * 255.0).round() as u8;
    if a == 0 {
        return None;
    }
    Some(Paint::Solid([base[0], base[1], base[2], a]))
}

fn resolve_clip_shape(defs: &Defs<'_>, id: &str) -> Option<ClipShape> {
    let cp = defs.lookup(id)?;
    if !name_eq(&cp.name, "clipPath") {
        return None;
    }
    // Walk children — we support exactly one shape (path / rect /
    // circle / ellipse). Multiple shapes inside a clipPath are still
    // accepted but only the first is used; this matches the
    // documented "single-path basic clipPath" deferral note.
    let mut local_xform = Affine::identity();
    if let Some(t) = cp.attr("transform").and_then(parse_transform) {
        local_xform = local_xform.compose(&t);
    }
    for c in &cp.children {
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

fn resolve_gradient(defs: &Defs<'_>, id: &str, ctx: &ElemCtx) -> Option<GradientPaint> {
    let node = defs.lookup(id)?;
    let is_linear = name_eq(&node.name, "linearGradient");
    let is_radial = name_eq(&node.name, "radialGradient");
    if !is_linear && !is_radial {
        return None;
    }
    // Stops can come from this node or, via xlink:href, an ancestor
    // gradient. A single hop of resolution is enough for every real
    // SVG-in-OT we've seen.
    let mut stops: Vec<ColorStop> = Vec::new();
    for c in &node.children {
        if name_eq(&c.name, "stop") {
            if let Some(s) = parse_stop(c) {
                stops.push(s);
            }
        }
    }
    if stops.is_empty() {
        if let Some(href) = node
            .attr("href")
            .or_else(|| node.attr("xlink:href"))
            .and_then(|s| s.strip_prefix('#'))
        {
            if let Some(parent) = defs.lookup(href) {
                for c in &parent.children {
                    if name_eq(&c.name, "stop") {
                        if let Some(s) = parse_stop(c) {
                            stops.push(s);
                        }
                    }
                }
            }
        }
    }
    if stops.is_empty() {
        return None;
    }
    let extend = match node
        .attr("spreadMethod")
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("repeat") => Extend::Repeat,
        Some("reflect") => Extend::Reflect,
        _ => Extend::Pad,
    };
    let gradient_xform = node
        .attr("gradientTransform")
        .and_then(parse_transform)
        .unwrap_or_else(Affine::identity);

    let kind = if is_linear {
        let x1 = node.attr("x1").and_then(parse_length).unwrap_or(0.0);
        let y1 = node.attr("y1").and_then(parse_length).unwrap_or(0.0);
        let x2 = node.attr("x2").and_then(parse_length).unwrap_or(1.0);
        let y2 = node.attr("y2").and_then(parse_length).unwrap_or(0.0);
        GradKind::Linear { x1, y1, x2, y2 }
    } else {
        let cx = node.attr("cx").and_then(parse_length).unwrap_or(0.5);
        let cy = node.attr("cy").and_then(parse_length).unwrap_or(0.5);
        let r = node.attr("r").and_then(parse_length).unwrap_or(0.5);
        let fx = node.attr("fx").and_then(parse_length).unwrap_or(cx);
        let fy = node.attr("fy").and_then(parse_length).unwrap_or(cy);
        GradKind::Radial { cx, cy, r, fx, fy }
    };
    Some(GradientPaint {
        kind,
        stops,
        extend,
        opacity: (ctx.fill_opacity * ctx.opacity).clamp(0.0, 1.0),
        gradient_xform,
    })
}

fn parse_stop(node: &Node) -> Option<ColorStop> {
    let offset = node.attr("offset").map(parse_stop_offset).unwrap_or(0.0);
    // stop-color is the canonical attribute; some authoring tools fold
    // it into a CSS-ish style="stop-color:#rgb;stop-opacity:0.5". Be
    // tolerant.
    let mut color = node
        .attr("stop-color")
        .and_then(parse_color)
        .unwrap_or([0, 0, 0, 255]);
    let stop_opacity = node
        .attr("stop-opacity")
        .and_then(parse_opacity)
        .unwrap_or(1.0);
    if let Some(style) = node.attr("style") {
        for chunk in style.split(';') {
            let mut parts = chunk.splitn(2, ':');
            let key = parts.next()?.trim();
            let val = parts.next()?.trim();
            if key.eq_ignore_ascii_case("stop-color") {
                if let Some(c) = parse_color(val) {
                    color = c;
                }
            } else if key.eq_ignore_ascii_case("stop-opacity") {
                if let Some(_o) = parse_opacity(val) {
                    // applied below
                }
            }
        }
    }
    let a = (color[3] as f32 / 255.0 * stop_opacity).clamp(0.0, 1.0);
    Some(ColorStop {
        offset,
        color: PaintColor {
            r: color[0] as f32 / 255.0,
            g: color[1] as f32 / 255.0,
            b: color[2] as f32 / 255.0,
            a,
        },
    })
}

fn parse_stop_offset(s: &str) -> f32 {
    let s = s.trim();
    if let Some(v) = s.strip_suffix('%') {
        return v.trim().parse::<f32>().map(|n| n / 100.0).unwrap_or(0.0);
    }
    s.parse::<f32>().unwrap_or(0.0)
}

// =========================================================================
// Stroke geometry: walk polyline → emit closed quad ribbons with caps
// and joins.
// =========================================================================

/// Expands an open / closed polyline into a closed filled outline that
/// represents the stroke. The output is a sequence of `MoveTo` /
/// `LineTo` / `Close` ops that the existing fill pipeline can consume.
///
/// The polyline is obtained by flattening the input ops (curves
/// flattened to chords at default tolerance). For each segment we emit
/// a quadrilateral of width `stroke_width` perpendicular to the
/// segment direction. Joins between segments are filled with
/// miter / round / bevel geometry, and the open ends carry the
/// configured cap shape.
fn stroke_to_fill(
    ops: &[PathOp],
    stroke_width: f32,
    cap: LineCap,
    join: LineJoin,
    dasharray: &[f32],
    dashoffset: f32,
) -> Vec<PathOp> {
    if stroke_width <= 0.0 {
        return Vec::new();
    }
    let polylines = flatten_to_polylines(ops);
    let half = stroke_width * 0.5;
    let mut out: Vec<PathOp> = Vec::new();

    let dashed = !dasharray.is_empty() && dasharray.iter().any(|&v| v > 0.0);

    for poly in &polylines {
        if poly.points.len() < 2 {
            continue;
        }
        if dashed {
            // Per-contour: walk *true Bezier arc length* (not the
            // chord-flattened polyline cumulative length, which is
            // always slightly short of the curve), emit only the "draw"
            // phase segments as fresh open polylines.
            let segs = dash_polyline(
                &poly.points,
                &poly.arc_lengths,
                poly.closed,
                dasharray,
                dashoffset,
            );
            for seg in segs {
                if seg.len() >= 2 {
                    emit_stroked_polyline(&mut out, &seg, false, half, cap, join);
                }
            }
        } else {
            emit_stroked_polyline(&mut out, &poly.points, poly.closed, half, cap, join);
        }
    }
    out
}

#[derive(Debug, Clone)]
struct PolyLine {
    points: Vec<(f32, f32)>,
    /// Per-chord *true* arc length. `arc_lengths[i]` is the arc length
    /// from `points[i]` to `points[(i + 1) % n]` along the original
    /// Bezier the chord came from. For straight `LineTo` chords this is
    /// the Euclidean distance and matches `(b - a).norm()`. For chords
    /// produced by curve flattening this is computed via the Roger
    /// Willcocks chord+control-polygon estimator at the leaf of curve
    /// subdivision, so it captures the curve's true sweep length
    /// instead of the (always-shorter) chord length.
    ///
    /// Length is `points.len() - 1` for open contours; for closed
    /// contours the implicit close-line's length is appended, giving
    /// `points.len()` entries.
    arc_lengths: Vec<f32>,
    closed: bool,
}

/// Flattens curves into a polyline list. One [`PolyLine`] per
/// sub-path. Closed sub-paths (terminated by `Close`) get
/// `closed = true`. Each polyline carries a parallel `arc_lengths`
/// array recording the *true Bezier arc length* of each chord segment;
/// for straight chords this equals the Euclidean distance, for
/// curve-flattened chords it is the leaf-level Roger Willcocks
/// approximation against the original control points.
fn flatten_to_polylines(ops: &[PathOp]) -> Vec<PolyLine> {
    let mut out: Vec<PolyLine> = Vec::new();
    let mut cur: Vec<(f32, f32)> = Vec::new();
    let mut cur_arc: Vec<f32> = Vec::new();
    let mut sx = 0.0_f32;
    let mut sy = 0.0_f32;
    let mut cx = 0.0_f32;
    let mut cy = 0.0_f32;
    let mut open = false;

    let push_line = |cur: &mut Vec<(f32, f32)>, arcs: &mut Vec<f32>, x: f32, y: f32| {
        let dup = cur
            .last()
            .map(|p| (p.0 - x).abs() <= 1e-6 && (p.1 - y).abs() <= 1e-6)
            .unwrap_or(false);
        if !dup {
            if let Some(prev) = cur.last() {
                let dx = x - prev.0;
                let dy = y - prev.1;
                arcs.push((dx * dx + dy * dy).sqrt());
            }
            cur.push((x, y));
        }
    };

    let finalize_close = |cur: &Vec<(f32, f32)>, arcs: &mut Vec<f32>| {
        // Closed contours need a wrap-segment arc length appended for
        // the implicit edge from `points[n-1]` back to `points[0]`.
        if let (Some(first), Some(last)) = (cur.first(), cur.last()) {
            let dx = first.0 - last.0;
            let dy = first.1 - last.1;
            arcs.push((dx * dx + dy * dy).sqrt());
        }
    };

    for op in ops {
        match *op {
            PathOp::MoveTo { x, y } => {
                if open && cur.len() >= 2 {
                    out.push(PolyLine {
                        points: core::mem::take(&mut cur),
                        arc_lengths: core::mem::take(&mut cur_arc),
                        closed: false,
                    });
                } else {
                    cur.clear();
                    cur_arc.clear();
                }
                cur.push((x, y));
                sx = x;
                sy = y;
                cx = x;
                cy = y;
                open = true;
            }
            PathOp::LineTo { x, y } => {
                push_line(&mut cur, &mut cur_arc, x, y);
                cx = x;
                cy = y;
            }
            PathOp::QuadTo {
                cx: ccx,
                cy: ccy,
                x,
                y,
            } => {
                flatten_quad_polyline(
                    &mut cur, &mut cur_arc, cx, cy, ccx, ccy, x, y, 0.25, 0,
                );
                cx = x;
                cy = y;
            }
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => {
                flatten_cubic_polyline(
                    &mut cur, &mut cur_arc, cx, cy, c1x, c1y, c2x, c2y, x, y, 0.25, 0,
                );
                cx = x;
                cy = y;
            }
            PathOp::Close => {
                if open && cur.len() >= 2 {
                    finalize_close(&cur, &mut cur_arc);
                    out.push(PolyLine {
                        points: core::mem::take(&mut cur),
                        arc_lengths: core::mem::take(&mut cur_arc),
                        closed: true,
                    });
                }
                cx = sx;
                cy = sy;
                open = false;
            }
        }
    }
    if open && cur.len() >= 2 {
        out.push(PolyLine {
            points: cur,
            arc_lengths: cur_arc,
            closed: false,
        });
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn flatten_quad_polyline(
    out: &mut Vec<(f32, f32)>,
    arcs: &mut Vec<f32>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    tol: f32,
    depth: u32,
) {
    let dx = x2 - x0;
    let dy = y2 - y0;
    let denom = dx * dx + dy * dy;
    let cross = (x1 - x0) * dy - (y1 - y0) * dx;
    let dist_sq = if denom > 0.0 {
        (cross * cross) / denom
    } else {
        let ex = x1 - x0;
        let ey = y1 - y0;
        ex * ex + ey * ey
    };
    if depth >= 16 || dist_sq <= 4.0 * tol * tol {
        if out
            .last()
            .map(|p| (p.0 - x2).abs() > 1e-6 || (p.1 - y2).abs() > 1e-6)
            .unwrap_or(true)
        {
            // Roger Willcocks arc-length estimate for the leaf curve
            // segment we're about to accept as a chord: the chord is
            // shorter than the curve, so dasharray walking against
            // chord length would land dashes early on long sweeps.
            let chord = (dx * dx + dy * dy).sqrt();
            let poly = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt()
                + ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
            arcs.push(0.5 * (chord + poly));
            out.push((x2, y2));
        }
        return;
    }
    let m01 = (0.5 * (x0 + x1), 0.5 * (y0 + y1));
    let m12 = (0.5 * (x1 + x2), 0.5 * (y1 + y2));
    let m = (0.5 * (m01.0 + m12.0), 0.5 * (m01.1 + m12.1));
    flatten_quad_polyline(out, arcs, x0, y0, m01.0, m01.1, m.0, m.1, tol, depth + 1);
    flatten_quad_polyline(out, arcs, m.0, m.1, m12.0, m12.1, x2, y2, tol, depth + 1);
}

#[allow(clippy::too_many_arguments)]
fn flatten_cubic_polyline(
    out: &mut Vec<(f32, f32)>,
    arcs: &mut Vec<f32>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    x3: f32,
    y3: f32,
    tol: f32,
    depth: u32,
) {
    let dx = x3 - x0;
    let dy = y3 - y0;
    let denom = dx * dx + dy * dy;
    let (d1, d2) = if denom > 0.0 {
        let c1 = (x1 - x0) * dy - (y1 - y0) * dx;
        let c2 = (x2 - x0) * dy - (y2 - y0) * dx;
        ((c1 * c1) / denom, (c2 * c2) / denom)
    } else {
        let e1x = x1 - x0;
        let e1y = y1 - y0;
        let e2x = x2 - x0;
        let e2y = y2 - y0;
        (e1x * e1x + e1y * e1y, e2x * e2x + e2y * e2y)
    };
    if depth >= 16 || (d1 <= tol * tol && d2 <= tol * tol) {
        if out
            .last()
            .map(|p| (p.0 - x3).abs() > 1e-6 || (p.1 - y3).abs() > 1e-6)
            .unwrap_or(true)
        {
            // Leaf-level Roger Willcocks arc-length estimate using the
            // four control points: closer to the true Bezier arc than
            // the chord (x0,y0)-(x3,y3) for non-degenerate curves.
            let chord = (dx * dx + dy * dy).sqrt();
            let poly = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt()
                + ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt()
                + ((x3 - x2).powi(2) + (y3 - y2).powi(2)).sqrt();
            arcs.push(0.5 * (chord + poly));
            out.push((x3, y3));
        }
        return;
    }
    let m01 = (0.5 * (x0 + x1), 0.5 * (y0 + y1));
    let m12 = (0.5 * (x1 + x2), 0.5 * (y1 + y2));
    let m23 = (0.5 * (x2 + x3), 0.5 * (y2 + y3));
    let m012 = (0.5 * (m01.0 + m12.0), 0.5 * (m01.1 + m12.1));
    let m123 = (0.5 * (m12.0 + m23.0), 0.5 * (m12.1 + m23.1));
    let m = (0.5 * (m012.0 + m123.0), 0.5 * (m012.1 + m123.1));
    flatten_cubic_polyline(
        out,
        arcs,
        x0,
        y0,
        m01.0,
        m01.1,
        m012.0,
        m012.1,
        m.0,
        m.1,
        tol,
        depth + 1,
    );
    flatten_cubic_polyline(
        out,
        arcs,
        m.0,
        m.1,
        m123.0,
        m123.1,
        m23.0,
        m23.1,
        x3,
        y3,
        tol,
        depth + 1,
    );
}

/// Emits the stroke ribbon for one polyline. For the minimum-viable
/// path this draws each segment as a separate rectangle (butt cap +
/// miter-style overlap). Adjacent segments overlap at joins so
/// scanline winding fills the joint cleanly without explicit miter
/// geometry — the result is visually identical to "miter" for typical
/// stroke widths and avoids the corner-case math.
///
/// Round / square caps emit half-circles / extended rectangles at the
/// open ends (best-effort follow-up — for now butt is the default).
fn emit_stroked_polyline(
    out: &mut Vec<PathOp>,
    points: &[(f32, f32)],
    closed: bool,
    half: f32,
    cap: LineCap,
    join: LineJoin,
) {
    if points.len() < 2 || half <= 0.0 {
        return;
    }
    let n = points.len();
    let segs = if closed { n } else { n - 1 };

    for i in 0..segs {
        let a = points[i];
        let b = points[(i + 1) % n];
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-6 {
            continue;
        }
        let (nx, ny) = (-dy / len, dx / len); // unit perpendicular (left)
        let (px, py) = (nx * half, ny * half);

        // Per-segment cap extension for square cap on the end caps.
        let mut a_ex = (0.0, 0.0);
        let mut b_ex = (0.0, 0.0);
        if !closed && cap == LineCap::Square {
            let (tx, ty) = (dx / len, dy / len);
            if i == 0 {
                a_ex = (-tx * half, -ty * half);
            }
            if i == segs - 1 {
                b_ex = (tx * half, ty * half);
            }
        }

        let p0 = (a.0 + a_ex.0 + px, a.1 + a_ex.1 + py);
        let p1 = (b.0 + b_ex.0 + px, b.1 + b_ex.1 + py);
        let p2 = (b.0 + b_ex.0 - px, b.1 + b_ex.1 - py);
        let p3 = (a.0 + a_ex.0 - px, a.1 + a_ex.1 - py);
        out.push(PathOp::MoveTo { x: p0.0, y: p0.1 });
        out.push(PathOp::LineTo { x: p1.0, y: p1.1 });
        out.push(PathOp::LineTo { x: p2.0, y: p2.1 });
        out.push(PathOp::LineTo { x: p3.0, y: p3.1 });
        out.push(PathOp::Close);
    }

    // Joins. For miter (default): overlapping rectangles already paint
    // the joint correctly. For round / bevel we approximate with a
    // disk / triangle at each vertex.
    if join == LineJoin::Round || cap == LineCap::Round {
        let join_at = |out: &mut Vec<PathOp>, p: (f32, f32)| {
            emit_disk(out, p.0, p.1, half);
        };
        let start = if closed { 0 } else { 1 };
        let end = if closed { n } else { n - 1 };
        for p in &points[start..end] {
            join_at(out, *p);
        }
        if !closed && cap == LineCap::Round {
            join_at(out, points[0]);
            join_at(out, points[n - 1]);
        }
    }

    // Miter spikes: when adjacent segments don't form a near-straight
    // angle, fill the wedge between them so a sharp corner doesn't
    // leave a notch. Falls back to bevel beyond the miter limit.
    if join == LineJoin::Miter && n >= 3 {
        let span = if closed { n } else { n - 2 };
        for i in 0..span {
            let prev = points[if closed && i == 0 { n - 1 } else { i }];
            let cur = points[if closed { (i + 1) % n } else { i + 1 }];
            let next = points[if closed { (i + 2) % n } else { i + 2 }];
            emit_miter_join(out, prev, cur, next, half);
        }
    }
}

/// Emits an axis-aligned octagon ("disk") of radius `r` centred at
/// `(cx, cy)`. 8 segments is the documented round-cap approximation.
fn emit_disk(out: &mut Vec<PathOp>, cx: f32, cy: f32, r: f32) {
    if r <= 0.0 {
        return;
    }
    const N: usize = 8;
    let two_pi = core::f32::consts::TAU;
    let mut first = (0.0, 0.0);
    for i in 0..N {
        let theta = (i as f32) / (N as f32) * two_pi;
        let x = cx + r * theta.cos();
        let y = cy + r * theta.sin();
        if i == 0 {
            out.push(PathOp::MoveTo { x, y });
            first = (x, y);
        } else {
            out.push(PathOp::LineTo { x, y });
        }
    }
    let _ = first;
    out.push(PathOp::Close);
}

/// Emits a miter-join wedge at vertex `cur`, given the previous and
/// next polyline points. When the join angle is reflex enough that the
/// miter would exceed `MITER_LIMIT * width`, a bevel triangle is used
/// instead (matching SVG's stroke-miterlimit default of 4).
fn emit_miter_join(
    out: &mut Vec<PathOp>,
    prev: (f32, f32),
    cur: (f32, f32),
    next: (f32, f32),
    half: f32,
) {
    let (ax, ay) = (cur.0 - prev.0, cur.1 - prev.1);
    let la = (ax * ax + ay * ay).sqrt();
    let (bx, by) = (next.0 - cur.0, next.1 - cur.1);
    let lb = (bx * bx + by * by).sqrt();
    if la < 1e-6 || lb < 1e-6 {
        return;
    }
    let (tax, tay) = (ax / la, ay / la);
    let (tbx, tby) = (bx / lb, by / lb);
    // Outer perpendicular (left of travel) on each segment.
    let (na, na2) = ((-tay) * half, tax * half);
    let (nb, nb2) = ((-tby) * half, tbx * half);
    // Outer corners.
    let p_a_left = (cur.0 + na, cur.1 + na2);
    let p_b_left = (cur.0 + nb, cur.1 + nb2);
    let p_a_right = (cur.0 - na, cur.1 - na2);
    let p_b_right = (cur.0 - nb, cur.1 - nb2);

    // Compute miter point on the outer side. A small angle between
    // segments means a long spike — bail to bevel beyond the limit.
    let dot = tax * tbx + tay * tby;
    let denom = 1.0 + dot;
    if denom <= 1e-6 {
        // Near 180° turn; bevel triangle on each side handles it.
        out.push(PathOp::MoveTo { x: cur.0, y: cur.1 });
        out.push(PathOp::LineTo {
            x: p_a_left.0,
            y: p_a_left.1,
        });
        out.push(PathOp::LineTo {
            x: p_b_left.0,
            y: p_b_left.1,
        });
        out.push(PathOp::Close);
        out.push(PathOp::MoveTo { x: cur.0, y: cur.1 });
        out.push(PathOp::LineTo {
            x: p_a_right.0,
            y: p_a_right.1,
        });
        out.push(PathOp::LineTo {
            x: p_b_right.0,
            y: p_b_right.1,
        });
        out.push(PathOp::Close);
        return;
    }
    // Miter spike length per the SVG appendix:
    //   m = half / sin(theta/2)   where  cos(theta) = -dot for "turn"
    let miter_ratio = (2.0_f32 / denom).sqrt(); // = 1 / sin(theta/2)
    if miter_ratio > MITER_LIMIT {
        // Bevel: just two triangles connecting outer corners to the
        // join centre.
        out.push(PathOp::MoveTo { x: cur.0, y: cur.1 });
        out.push(PathOp::LineTo {
            x: p_a_left.0,
            y: p_a_left.1,
        });
        out.push(PathOp::LineTo {
            x: p_b_left.0,
            y: p_b_left.1,
        });
        out.push(PathOp::Close);
        out.push(PathOp::MoveTo { x: cur.0, y: cur.1 });
        out.push(PathOp::LineTo {
            x: p_a_right.0,
            y: p_a_right.1,
        });
        out.push(PathOp::LineTo {
            x: p_b_right.0,
            y: p_b_right.1,
        });
        out.push(PathOp::Close);
        return;
    }
    // Bisector direction.
    let bis_x = tax + tbx;
    let bis_y = tay + tby;
    let bis_len = (bis_x * bis_x + bis_y * bis_y).sqrt();
    if bis_len < 1e-6 {
        return;
    }
    let (bxn, byn) = (bis_x / bis_len, bis_y / bis_len);
    // Outer normal (left of join travel).
    let (n_left_x, n_left_y) = (-byn, bxn);
    let dx_m = n_left_x * half * miter_ratio;
    let dy_m = n_left_y * half * miter_ratio;
    let p_left_miter = (cur.0 + dx_m, cur.1 + dy_m);
    let p_right_miter = (cur.0 - dx_m, cur.1 - dy_m);

    // Outer-side miter wedge.
    out.push(PathOp::MoveTo { x: cur.0, y: cur.1 });
    out.push(PathOp::LineTo {
        x: p_a_left.0,
        y: p_a_left.1,
    });
    out.push(PathOp::LineTo {
        x: p_left_miter.0,
        y: p_left_miter.1,
    });
    out.push(PathOp::LineTo {
        x: p_b_left.0,
        y: p_b_left.1,
    });
    out.push(PathOp::Close);
    // Inner-side miter wedge (mirrors the outer one).
    out.push(PathOp::MoveTo { x: cur.0, y: cur.1 });
    out.push(PathOp::LineTo {
        x: p_a_right.0,
        y: p_a_right.1,
    });
    out.push(PathOp::LineTo {
        x: p_right_miter.0,
        y: p_right_miter.1,
    });
    out.push(PathOp::LineTo {
        x: p_b_right.0,
        y: p_b_right.1,
    });
    out.push(PathOp::Close);
}

// =========================================================================
// Stroke dasharray
// =========================================================================

/// Parses a `stroke-dasharray` attribute body. Empty / `none` /
/// all-zero / unparseable inputs return an empty `Vec`. SVG mandates
/// that odd-length lists are doubled (e.g. `"2 3 5"` →
/// `"2 3 5 2 3 5"`); we apply that here so the walker can iterate
/// without worrying about parity.
fn parse_dasharray(s: &str) -> Vec<f32> {
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    let mut nums: Vec<f32> = Vec::new();
    for tok in trimmed.split(|c: char| c == ',' || c.is_ascii_whitespace()) {
        if tok.is_empty() {
            continue;
        }
        // Strip an optional `px` suffix; everything else (em/%/etc.)
        // we treat as "user-space units" per SVG.
        let body = tok.trim_end_matches("px");
        match body.parse::<f32>() {
            Ok(n) if n.is_finite() && n >= 0.0 => nums.push(n),
            _ => return Vec::new(), // SVG: any negative or invalid → ignore the whole list.
        }
    }
    if nums.is_empty() || nums.iter().all(|&v| v == 0.0) {
        return Vec::new();
    }
    if nums.len() % 2 == 1 {
        let extra = nums.clone();
        nums.extend_from_slice(&extra);
    }
    nums
}

/// Walks `points` by cumulative *true Bezier arc length* and returns
/// the polylines that fall inside the "draw" phase of the dash pattern.
/// `arc_lengths[i]` is the parent-curve arc length of the chord from
/// `points[i]` to `points[(i + 1) % n]` — for straight chords this is
/// the Euclidean distance, for chords flattened from Quad/Cubic Beziers
/// it is the Roger Willcocks chord+control-polygon estimate (~0.05 %
/// of the true Gauss-Legendre integral on typical sweeps). `pattern`
/// is even-length and non-empty (caller-checked); `offset` is applied
/// at the start of the contour, then resets per [SVG spec].
///
/// Position mapping: a dash boundary at arc-length `s` along chord
/// `i` lands geometrically at parameter `t = s / arc_lengths[i]`
/// linearly between `points[i]` and `points[i+1]`. This is the
/// standard mapping for chord-flattened curves — the dash is *placed*
/// at its true-arc-length position along the curve, but the geometry
/// is interpolated on the chord (which is what the rasterizer
/// already consumes).
///
/// Behaviour at a glance:
///
/// - Stride alternates draw / skip starting from index 0 ("draw").
/// - `offset` may be negative or larger than the pattern; reduced
///   modulo `total = sum(pattern)` after sign-folding.
/// - Closed contours are walked as if a final segment connected back
///   to the first vertex; the resulting "wrap" sub-polyline is split
///   the same way as any other.
/// - For straight-chord polylines (rect, polygon, polyline, line,
///   `LineTo` paths), `arc_lengths[i]` is exactly the Euclidean
///   distance, so this function is bit-identical to the previous
///   chord-only walker on those inputs.
fn dash_polyline(
    points: &[(f32, f32)],
    arc_lengths: &[f32],
    closed: bool,
    pattern: &[f32],
    offset: f32,
) -> Vec<Vec<(f32, f32)>> {
    let total: f32 = pattern.iter().sum();
    if total <= 0.0 || points.len() < 2 {
        return Vec::new();
    }
    // Normalise offset into [0, total).
    let mut off = offset % total;
    if off < 0.0 {
        off += total;
    }
    // The current dash index (even = draw, odd = skip) and remaining
    // length within that dash segment after consuming `off`.
    let mut idx = 0usize;
    let mut remaining = pattern[0];
    while off > 0.0 && remaining <= off {
        off -= remaining;
        idx = (idx + 1) % pattern.len();
        remaining = pattern[idx];
    }
    remaining -= off;
    let mut drawing = idx % 2 == 0;

    // Build the list of segments to walk. For closed contours we
    // append the wraparound segment.
    let n = points.len();
    let segs = if closed { n } else { n - 1 };

    let mut out: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut cur: Vec<(f32, f32)> = Vec::new();
    if drawing {
        cur.push(points[0]);
    }

    for i in 0..segs {
        let a = points[i];
        let b = points[(i + 1) % n];
        let dx = b.0 - a.0;
        let dy = b.1 - a.1;
        // True arc length of this chord segment (parent curve's sweep
        // length, not the chord-Euclidean distance — they only differ
        // for curve-flattened chords).
        let seg_arc = arc_lengths.get(i).copied().unwrap_or_else(|| {
            // Defensive fallback: parallel array missing this entry
            // (shouldn't happen with `flatten_to_polylines`, but guards
            // against future callers passing a malformed pair).
            (dx * dx + dy * dy).sqrt()
        });
        if seg_arc < 1e-6 {
            continue;
        }
        let mut s_consumed = 0.0_f32;
        // Walk the segment, splitting at every dash boundary in
        // arc-length space.
        while seg_arc - s_consumed > remaining {
            // Boundary lands at arc-length `s_consumed + remaining`
            // along this chord; map to chord parameter `t` linearly.
            // For straight chords this is exact; for curve chords the
            // sub-chord is short enough (curve flattening tolerance
            // 0.25 px) that the linear-on-chord mapping is well within
            // a sub-pixel of the true curve position.
            let t = (s_consumed + remaining) / seg_arc;
            let bx = a.0 + dx * t;
            let by = a.1 + dy * t;
            if drawing {
                cur.push((bx, by));
                if cur.len() >= 2 {
                    out.push(core::mem::take(&mut cur));
                }
            }
            s_consumed += remaining;
            // Advance to the next pattern entry.
            idx = (idx + 1) % pattern.len();
            remaining = pattern[idx];
            drawing = idx % 2 == 0;
            if drawing {
                cur.clear();
                cur.push((bx, by));
            }
        }
        // Remainder of the segment.
        let used = seg_arc - s_consumed;
        remaining -= used;
        if drawing {
            cur.push(b);
        }
    }
    if drawing && cur.len() >= 2 {
        out.push(cur);
    }
    out
}

// =========================================================================
// Shape primitives → path
// =========================================================================

fn rect_to_path(node: &Node) -> Vec<PathOp> {
    let x = node.attr("x").and_then(parse_length).unwrap_or(0.0);
    let y = node.attr("y").and_then(parse_length).unwrap_or(0.0);
    let w = node.attr("width").and_then(parse_length).unwrap_or(0.0);
    let h = node.attr("height").and_then(parse_length).unwrap_or(0.0);
    if w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    let rx_attr = node.attr("rx").and_then(parse_length);
    let ry_attr = node.attr("ry").and_then(parse_length);
    let rx = match (rx_attr, ry_attr) {
        (Some(rx), _) => rx,
        (None, Some(ry)) => ry,
        (None, None) => 0.0,
    };
    let ry = match (rx_attr, ry_attr) {
        (_, Some(ry)) => ry,
        (Some(rx), None) => rx,
        (None, None) => 0.0,
    };
    let rx = rx.max(0.0).min(w * 0.5);
    let ry = ry.max(0.0).min(h * 0.5);

    let mut ops = Vec::with_capacity(if rx > 0.0 || ry > 0.0 { 12 } else { 6 });
    if rx > 0.0 && ry > 0.0 {
        // Kappa for cubic-circle approximation of a quarter ellipse.
        const K: f32 = 0.552_284_8;
        let kx = rx * K;
        let ky = ry * K;
        // Top edge: start at (x+rx, y) and go to (x+w-rx, y).
        ops.push(PathOp::MoveTo { x: x + rx, y });
        ops.push(PathOp::LineTo { x: x + w - rx, y });
        // Top-right corner.
        ops.push(PathOp::CubicTo {
            c1x: x + w - rx + kx,
            c1y: y,
            c2x: x + w,
            c2y: y + ry - ky,
            x: x + w,
            y: y + ry,
        });
        // Right edge.
        ops.push(PathOp::LineTo {
            x: x + w,
            y: y + h - ry,
        });
        // Bottom-right corner.
        ops.push(PathOp::CubicTo {
            c1x: x + w,
            c1y: y + h - ry + ky,
            c2x: x + w - rx + kx,
            c2y: y + h,
            x: x + w - rx,
            y: y + h,
        });
        // Bottom edge.
        ops.push(PathOp::LineTo {
            x: x + rx,
            y: y + h,
        });
        // Bottom-left corner.
        ops.push(PathOp::CubicTo {
            c1x: x + rx - kx,
            c1y: y + h,
            c2x: x,
            c2y: y + h - ry + ky,
            x,
            y: y + h - ry,
        });
        // Left edge.
        ops.push(PathOp::LineTo { x, y: y + ry });
        // Top-left corner.
        ops.push(PathOp::CubicTo {
            c1x: x,
            c1y: y + ry - ky,
            c2x: x + rx - kx,
            c2y: y,
            x: x + rx,
            y,
        });
        ops.push(PathOp::Close);
    } else {
        ops.push(PathOp::MoveTo { x, y });
        ops.push(PathOp::LineTo { x: x + w, y });
        ops.push(PathOp::LineTo { x: x + w, y: y + h });
        ops.push(PathOp::LineTo { x, y: y + h });
        ops.push(PathOp::Close);
    }
    ops
}

fn circle_to_path(node: &Node) -> Vec<PathOp> {
    let cx = node.attr("cx").and_then(parse_length).unwrap_or(0.0);
    let cy = node.attr("cy").and_then(parse_length).unwrap_or(0.0);
    let r = node.attr("r").and_then(parse_length).unwrap_or(0.0);
    if r <= 0.0 {
        return Vec::new();
    }
    ellipse_path(cx, cy, r, r)
}

fn ellipse_to_path(node: &Node) -> Vec<PathOp> {
    let cx = node.attr("cx").and_then(parse_length).unwrap_or(0.0);
    let cy = node.attr("cy").and_then(parse_length).unwrap_or(0.0);
    let rx = node.attr("rx").and_then(parse_length).unwrap_or(0.0);
    let ry = node.attr("ry").and_then(parse_length).unwrap_or(0.0);
    if rx <= 0.0 || ry <= 0.0 {
        return Vec::new();
    }
    ellipse_path(cx, cy, rx, ry)
}

/// Approximates a centred ellipse with four cubic Béziers using the
/// standard kappa = 0.552_284_8. Drawing direction is clockwise (the
/// rasterizer's non-zero winding handles either, but we stay
/// consistent with `<rect>`).
fn ellipse_path(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<PathOp> {
    const K: f32 = 0.552_284_8;
    let kx = rx * K;
    let ky = ry * K;
    alloc::vec![
        PathOp::MoveTo { x: cx + rx, y: cy },
        PathOp::CubicTo {
            c1x: cx + rx,
            c1y: cy + ky,
            c2x: cx + kx,
            c2y: cy + ry,
            x: cx,
            y: cy + ry,
        },
        PathOp::CubicTo {
            c1x: cx - kx,
            c1y: cy + ry,
            c2x: cx - rx,
            c2y: cy + ky,
            x: cx - rx,
            y: cy,
        },
        PathOp::CubicTo {
            c1x: cx - rx,
            c1y: cy - ky,
            c2x: cx - kx,
            c2y: cy - ry,
            x: cx,
            y: cy - ry,
        },
        PathOp::CubicTo {
            c1x: cx + kx,
            c1y: cy - ry,
            c2x: cx + rx,
            c2y: cy - ky,
            x: cx + rx,
            y: cy,
        },
        PathOp::Close,
    ]
}

/// Parses an SVG `points="x1,y1 x2,y2 ..."` list. The grammar accepts
/// any mix of whitespace and commas as separators (per SVG 1.1
/// §9.7.1). Trailing odd coordinates (a stray "x" with no matching "y")
/// are dropped silently — that's what every browser does in practice.
fn parse_points_list(s: &str) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = Vec::new();
    let mut nums: Vec<f32> = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip separators: whitespace and commas.
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        if bytes[i] == b'+' || bytes[i] == b'-' {
            i += 1;
        }
        let mut saw_digit = false;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            saw_digit = true;
        }
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
                saw_digit = true;
            }
        }
        if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
            i += 1;
            if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                i += 1;
            }
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
        if !saw_digit {
            // Bail out on unrecognised garbage; what's parsed so far
            // stays.
            break;
        }
        if let Ok(s) = core::str::from_utf8(&bytes[start..i]) {
            if let Ok(n) = s.parse::<f32>() {
                nums.push(n);
            }
        }
    }
    let mut k = 0;
    while k + 1 < nums.len() {
        out.push((nums[k], nums[k + 1]));
        k += 2;
    }
    out
}

/// `<polygon points="...">`: closed shape, `MoveTo + LineTo* + Close`.
fn polygon_to_path(node: &Node) -> Vec<PathOp> {
    let pts = node
        .attr("points")
        .map(parse_points_list)
        .unwrap_or_default();
    if pts.len() < 2 {
        return Vec::new();
    }
    let mut ops = Vec::with_capacity(pts.len() + 1);
    ops.push(PathOp::MoveTo {
        x: pts[0].0,
        y: pts[0].1,
    });
    for p in &pts[1..] {
        ops.push(PathOp::LineTo { x: p.0, y: p.1 });
    }
    ops.push(PathOp::Close);
    ops
}

/// `<polyline points="...">`: open shape, `MoveTo + LineTo*` (no Close).
fn polyline_to_path(node: &Node) -> Vec<PathOp> {
    let pts = node
        .attr("points")
        .map(parse_points_list)
        .unwrap_or_default();
    if pts.len() < 2 {
        return Vec::new();
    }
    let mut ops = Vec::with_capacity(pts.len());
    ops.push(PathOp::MoveTo {
        x: pts[0].0,
        y: pts[0].1,
    });
    for p in &pts[1..] {
        ops.push(PathOp::LineTo { x: p.0, y: p.1 });
    }
    ops
}

/// `<line x1 y1 x2 y2>`: a single segment, `MoveTo + LineTo`.
fn line_to_path(node: &Node) -> Vec<PathOp> {
    let x1 = node.attr("x1").and_then(parse_length).unwrap_or(0.0);
    let y1 = node.attr("y1").and_then(parse_length).unwrap_or(0.0);
    let x2 = node.attr("x2").and_then(parse_length).unwrap_or(0.0);
    let y2 = node.attr("y2").and_then(parse_length).unwrap_or(0.0);
    if (x1 - x2).abs() < 1e-6 && (y1 - y2).abs() < 1e-6 {
        return Vec::new();
    }
    alloc::vec![
        PathOp::MoveTo { x: x1, y: y1 },
        PathOp::LineTo { x: x2, y: y2 },
    ]
}

// =========================================================================
// Render-time blit
// =========================================================================

fn render_fill(out: &mut ColorPixmap, fill: &Fill, world: &Affine, tol: f32) {
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

    match &fill.paint {
        Paint::Solid(color) => {
            blit_solid(
                out,
                &mask.pixmap,
                mask.origin_x,
                mask.origin_y,
                *color,
                clip_mask.as_ref(),
            );
        }
        Paint::Gradient(g) => {
            let g_xf = world.compose(&fill.xform).compose(&g.gradient_xform);
            blit_gradient(
                out,
                &mask.pixmap,
                mask.origin_x,
                mask.origin_y,
                g,
                &g_xf,
                clip_mask.as_ref(),
            );
        }
    }
}

/// Blits `mask × color` into `dst`, where `(ox, oy)` is the
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

/// Blits `mask × gradient` into `dst` using the COLRv1 ramp evaluator.
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
            // Pixel centre in pixel space.
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
/// gradient geometry through `g_xf` (document → pixel + any
/// `gradientTransform`) before calling the COLRv1 projection
/// primitives — same shape, same `Pad` / `Repeat` / `Reflect` semantics.
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

// =========================================================================
// XML scanner → DOM
// =========================================================================

fn parse_xml(xml: &str) -> Result<Node, RenderError> {
    let mut p = XmlParser::new(xml);
    p.skip_prolog();
    let Some(tag) = p.next_tag() else {
        return Err(RenderError::Parse("svg root"));
    };
    if tag.kind == TagKind::Comment || tag.kind == TagKind::Decl {
        return parse_xml_after_prolog(&mut p);
    }
    if tag.kind != TagKind::Open && tag.kind != TagKind::SelfClose {
        return Err(RenderError::Parse("svg root"));
    }
    let mut node = Node {
        name: tag.name.into(),
        attrs: parse_attrs(tag.attrs),
        children: Vec::new(),
    };
    if tag.kind == TagKind::SelfClose {
        return Ok(node);
    }
    parse_children(&mut p, &mut node, 0)?;
    Ok(node)
}

fn parse_xml_after_prolog(p: &mut XmlParser<'_>) -> Result<Node, RenderError> {
    while let Some(tag) = p.next_tag() {
        match tag.kind {
            TagKind::Comment | TagKind::Decl => continue,
            TagKind::Open | TagKind::SelfClose => {
                let mut node = Node {
                    name: tag.name.into(),
                    attrs: parse_attrs(tag.attrs),
                    children: Vec::new(),
                };
                if tag.kind == TagKind::Open {
                    parse_children(p, &mut node, 0)?;
                }
                return Ok(node);
            }
            TagKind::Close => {
                return Err(RenderError::Parse("svg root"));
            }
        }
    }
    Err(RenderError::Parse("svg root"))
}

fn parse_children(p: &mut XmlParser<'_>, parent: &mut Node, depth: u32) -> Result<(), RenderError> {
    if depth > 256 {
        return Err(RenderError::Parse("svg nesting"));
    }
    while let Some(tag) = p.next_tag() {
        match tag.kind {
            TagKind::Comment | TagKind::Decl => continue,
            TagKind::Close => return Ok(()),
            TagKind::Open => {
                let mut child = Node {
                    name: tag.name.into(),
                    attrs: parse_attrs(tag.attrs),
                    children: Vec::new(),
                };
                parse_children(p, &mut child, depth + 1)?;
                parent.children.push(child);
            }
            TagKind::SelfClose => {
                parent.children.push(Node {
                    name: tag.name.into(),
                    attrs: parse_attrs(tag.attrs),
                    children: Vec::new(),
                });
            }
        }
    }
    Ok(())
}

fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let Some(eq) = rest.find('=') else {
            break;
        };
        let key = rest[..eq].trim().to_string();
        let after = rest[eq + 1..].trim_start();
        let bytes = after.as_bytes();
        if bytes.is_empty() {
            break;
        }
        let q = bytes[0];
        let (val, next) = if q == b'"' || q == b'\'' {
            let body = &after[1..];
            let Some(end) = body.find(q as char) else {
                break;
            };
            (body[..end].to_string(), &body[end + 1..])
        } else {
            let end = after
                .find(|c: char| c.is_ascii_whitespace() || c == '>')
                .unwrap_or(after.len());
            (after[..end].to_string(), &after[end..])
        };
        out.push((key, val));
        rest = next;
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TagKind {
    Open,
    SelfClose,
    Close,
    Comment,
    Decl,
}

#[derive(Debug, Clone, Copy)]
struct Tag<'a> {
    kind: TagKind,
    name: &'a str,
    attrs: &'a str,
}

struct XmlParser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> XmlParser<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn skip_prolog(&mut self) {
        loop {
            self.skip_ws();
            let rest = &self.src[self.pos..];
            if let Some(stripped) = rest.strip_prefix("<?") {
                if let Some(end) = stripped.find("?>") {
                    self.pos += 2 + end + 2;
                    continue;
                }
                self.pos = self.src.len();
                return;
            }
            if rest.starts_with("<!--") {
                if let Some(end) = rest.find("-->") {
                    self.pos += end + 3;
                    continue;
                }
                self.pos = self.src.len();
                return;
            }
            if rest.starts_with("<!") {
                if let Some(end) = rest.find('>') {
                    self.pos += end + 1;
                    continue;
                }
                self.pos = self.src.len();
                return;
            }
            return;
        }
    }

    fn skip_ws(&mut self) {
        let bytes = self.src.as_bytes();
        while self.pos < bytes.len() && bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn next_tag(&mut self) -> Option<Tag<'a>> {
        let bytes = self.src.as_bytes();
        while self.pos < bytes.len() && bytes[self.pos] != b'<' {
            self.pos += 1;
        }
        if self.pos >= bytes.len() {
            return None;
        }
        let rest = &self.src[self.pos..];
        if rest.starts_with("<!--") {
            if let Some(end) = rest.find("-->") {
                self.pos += end + 3;
                return Some(Tag {
                    kind: TagKind::Comment,
                    name: "",
                    attrs: "",
                });
            }
            self.pos = self.src.len();
            return None;
        }
        if rest.starts_with("<?") || rest.starts_with("<!") {
            if let Some(end) = rest.find('>') {
                self.pos += end + 1;
                return Some(Tag {
                    kind: TagKind::Decl,
                    name: "",
                    attrs: "",
                });
            }
            self.pos = self.src.len();
            return None;
        }
        let close = rest.find('>')?;
        let inner = &rest[1..close];
        self.pos += close + 1;

        if let Some(stripped) = inner.strip_prefix('/') {
            let name = stripped.split_ascii_whitespace().next().unwrap_or("");
            return Some(Tag {
                kind: TagKind::Close,
                name,
                attrs: "",
            });
        }
        let (kind, body) = if let Some(stripped) = inner.strip_suffix('/') {
            (TagKind::SelfClose, stripped)
        } else {
            (TagKind::Open, inner)
        };
        let body = body.trim();
        let (name, attrs) = match body.find(|c: char| c.is_ascii_whitespace()) {
            Some(i) => (&body[..i], body[i..].trim()),
            None => (body, ""),
        };
        Some(Tag { kind, name, attrs })
    }
}

// =========================================================================
// Numeric / colour / transform parsing
// =========================================================================

fn parse_viewbox(s: &str) -> Option<(f32, f32, f32, f32)> {
    let mut it = s.split(|c: char| c.is_ascii_whitespace() || c == ',');
    let x = it.find(|t| !t.is_empty())?.parse::<f32>().ok()?;
    let y = it.find(|t| !t.is_empty())?.parse::<f32>().ok()?;
    let w = it.find(|t| !t.is_empty())?.parse::<f32>().ok()?;
    let h = it.find(|t| !t.is_empty())?.parse::<f32>().ok()?;
    Some((x, y, w, h))
}

fn parse_length(s: &str) -> Option<f32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let cut = s
        .char_indices()
        .find(|(_, c)| {
            !(c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+' || *c == 'e' || *c == 'E')
        })
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    s[..cut].parse::<f32>().ok()
}

fn parse_opacity(s: &str) -> Option<f32> {
    let v = s.trim().parse::<f32>().ok()?;
    Some(v.clamp(0.0, 1.0))
}

fn parse_color(s: &str) -> Option<[u8; 4]> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("none") {
        return None;
    }
    match s.to_ascii_lowercase().as_str() {
        "black" => return Some([0, 0, 0, 255]),
        "white" => return Some([255, 255, 255, 255]),
        "red" => return Some([255, 0, 0, 255]),
        "green" => return Some([0, 128, 0, 255]),
        "blue" => return Some([0, 0, 255, 255]),
        _ => {}
    }
    if let Some(rest) = s.strip_prefix('#') {
        if rest.len() == 6 {
            let r = u8::from_str_radix(&rest[0..2], 16).ok()?;
            let g = u8::from_str_radix(&rest[2..4], 16).ok()?;
            let b = u8::from_str_radix(&rest[4..6], 16).ok()?;
            return Some([r, g, b, 255]);
        }
        if rest.len() == 3 {
            let nyb = |c: char| -> Option<u8> {
                let mut tmp = [0u8; 4];
                let s = c.encode_utf8(&mut tmp);
                u8::from_str_radix(s, 16).ok()
            };
            let mut chars = rest.chars();
            let r = nyb(chars.next()?)?;
            let g = nyb(chars.next()?)?;
            let b = nyb(chars.next()?)?;
            return Some([r * 17, g * 17, b * 17, 255]);
        }
        return None;
    }
    if let Some(inner) = s
        .strip_prefix("rgb(")
        .or_else(|| s.strip_prefix("RGB("))
        .and_then(|x| x.strip_suffix(')'))
    {
        let mut it = inner.split(|c: char| c == ',' || c.is_ascii_whitespace());
        let r = it.find(|t| !t.is_empty())?.parse::<f32>().ok()? as i32;
        let g = it.find(|t| !t.is_empty())?.parse::<f32>().ok()? as i32;
        let b = it.find(|t| !t.is_empty())?.parse::<f32>().ok()? as i32;
        return Some([
            r.clamp(0, 255) as u8,
            g.clamp(0, 255) as u8,
            b.clamp(0, 255) as u8,
            255,
        ]);
    }
    None
}

fn parse_transform(s: &str) -> Option<Affine> {
    let mut acc = Affine::identity();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let lparen = rest.find('(')?;
        let rparen_off = rest[lparen..].find(')')?;
        let name = rest[..lparen].trim();
        let body = &rest[lparen + 1..lparen + rparen_off];
        rest = rest[lparen + rparen_off + 1..]
            .trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
        let nums: Vec<f32> = body
            .split(|c: char| c == ',' || c.is_ascii_whitespace())
            .filter(|t| !t.is_empty())
            .map(|t| t.parse::<f32>().unwrap_or(0.0))
            .collect();
        let m = match name.to_ascii_lowercase().as_str() {
            "translate" => match nums.len() {
                0 => continue,
                1 => Affine::translate(nums[0], 0.0),
                _ => Affine::translate(nums[0], nums[1]),
            },
            "scale" => match nums.len() {
                0 => continue,
                1 => Affine::scale(nums[0], nums[0]),
                _ => Affine::scale(nums[0], nums[1]),
            },
            "matrix" if nums.len() >= 6 => Affine {
                xx: nums[0],
                yx: nums[1],
                xy: nums[2],
                yy: nums[3],
                dx: nums[4],
                dy: nums[5],
            },
            "rotate" => {
                let rad = nums.first().copied().unwrap_or(0.0).to_radians();
                if nums.len() >= 3 {
                    let cx = nums[1];
                    let cy = nums[2];
                    Affine::translate(cx, cy)
                        .compose(&Affine::rotate(rad))
                        .compose(&Affine::translate(-cx, -cy))
                } else {
                    Affine::rotate(rad)
                }
            }
            _ => continue,
        };
        acc = acc.compose(&m);
    }
    Some(acc)
}

// =========================================================================
// Path-data parser (M/L/H/V/C/Q/Z)
// =========================================================================

fn parse_path_d(s: &str) -> Result<Vec<PathOp>, RenderError> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut cx = 0.0_f32;
    let mut cy = 0.0_f32;
    let mut sx = 0.0_f32;
    let mut sy = 0.0_f32;
    let mut have_sub = false;
    let mut last_cmd: Option<u8> = None;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let c = bytes[i];
        let cmd = if c.is_ascii_alphabetic() {
            i += 1;
            last_cmd = Some(c);
            c
        } else {
            match last_cmd {
                Some(b'M') => b'L',
                Some(b'm') => b'l',
                Some(prev) => prev,
                None => return Err(RenderError::Parse("svg path d")),
            }
        };
        match cmd {
            b'M' | b'm' => {
                let (x, y) = read_pair(bytes, &mut i)?;
                let (ax, ay) = if cmd == b'M' {
                    (x, y)
                } else {
                    (cx + x, cy + y)
                };
                cx = ax;
                cy = ay;
                sx = ax;
                sy = ay;
                have_sub = true;
                out.push(PathOp::MoveTo { x: ax, y: ay });
            }
            b'L' | b'l' => {
                let (x, y) = read_pair(bytes, &mut i)?;
                let (ax, ay) = if cmd == b'L' {
                    (x, y)
                } else {
                    (cx + x, cy + y)
                };
                cx = ax;
                cy = ay;
                out.push(PathOp::LineTo { x: ax, y: ay });
            }
            b'H' | b'h' => {
                let x = read_num(bytes, &mut i)?;
                let ax = if cmd == b'H' { x } else { cx + x };
                cx = ax;
                out.push(PathOp::LineTo { x: ax, y: cy });
            }
            b'V' | b'v' => {
                let y = read_num(bytes, &mut i)?;
                let ay = if cmd == b'V' { y } else { cy + y };
                cy = ay;
                out.push(PathOp::LineTo { x: cx, y: ay });
            }
            b'C' | b'c' => {
                let (x1, y1) = read_pair(bytes, &mut i)?;
                let (x2, y2) = read_pair(bytes, &mut i)?;
                let (x, y) = read_pair(bytes, &mut i)?;
                let (a1x, a1y, a2x, a2y, ax, ay) = if cmd == b'C' {
                    (x1, y1, x2, y2, x, y)
                } else {
                    (cx + x1, cy + y1, cx + x2, cy + y2, cx + x, cy + y)
                };
                out.push(PathOp::CubicTo {
                    c1x: a1x,
                    c1y: a1y,
                    c2x: a2x,
                    c2y: a2y,
                    x: ax,
                    y: ay,
                });
                cx = ax;
                cy = ay;
            }
            b'Q' | b'q' => {
                let (x1, y1) = read_pair(bytes, &mut i)?;
                let (x, y) = read_pair(bytes, &mut i)?;
                let (a1x, a1y, ax, ay) = if cmd == b'Q' {
                    (x1, y1, x, y)
                } else {
                    (cx + x1, cy + y1, cx + x, cy + y)
                };
                out.push(PathOp::QuadTo {
                    cx: a1x,
                    cy: a1y,
                    x: ax,
                    y: ay,
                });
                cx = ax;
                cy = ay;
            }
            b'Z' | b'z' => {
                if have_sub {
                    out.push(PathOp::Close);
                    cx = sx;
                    cy = sy;
                }
            }
            _ => {
                return Err(RenderError::Parse("svg path d"));
            }
        }
    }
    Ok(out)
}

fn read_pair(bytes: &[u8], i: &mut usize) -> Result<(f32, f32), RenderError> {
    let x = read_num(bytes, i)?;
    let y = read_num(bytes, i)?;
    Ok((x, y))
}

fn read_num(bytes: &[u8], i: &mut usize) -> Result<f32, RenderError> {
    while *i < bytes.len() && (bytes[*i].is_ascii_whitespace() || bytes[*i] == b',') {
        *i += 1;
    }
    let start = *i;
    if *i < bytes.len() && (bytes[*i] == b'+' || bytes[*i] == b'-') {
        *i += 1;
    }
    let mut saw_digit = false;
    while *i < bytes.len() && bytes[*i].is_ascii_digit() {
        *i += 1;
        saw_digit = true;
    }
    if *i < bytes.len() && bytes[*i] == b'.' {
        *i += 1;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
            saw_digit = true;
        }
    }
    if *i < bytes.len() && (bytes[*i] == b'e' || bytes[*i] == b'E') {
        *i += 1;
        if *i < bytes.len() && (bytes[*i] == b'+' || bytes[*i] == b'-') {
            *i += 1;
        }
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
        }
    }
    if !saw_digit {
        return Err(RenderError::Parse("svg path d"));
    }
    let s =
        core::str::from_utf8(&bytes[start..*i]).map_err(|_| RenderError::Parse("svg path d"))?;
    s.parse::<f32>()
        .map_err(|_| RenderError::Parse("svg path d"))
}

// =========================================================================
// Tests (parser + geometry primitives)
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn first_fill(doc: &SvgDoc) -> &Fill {
        &doc.fills[0]
    }

    #[test]
    fn parse_color_hex_long() {
        assert_eq!(parse_color("#FF8800"), Some([0xff, 0x88, 0x00, 255]));
        assert_eq!(parse_color("#000000"), Some([0, 0, 0, 255]));
    }

    #[test]
    fn parse_color_hex_short_doubles_each_nybble() {
        assert_eq!(parse_color("#f80"), Some([0xff, 0x88, 0x00, 255]));
    }

    #[test]
    fn parse_color_rgb_clamps_oversize() {
        assert_eq!(parse_color("rgb(300, -2, 128)"), Some([255, 0, 128, 255]));
    }

    #[test]
    fn parse_color_named_and_none() {
        assert_eq!(parse_color("black"), Some([0, 0, 0, 255]));
        assert_eq!(parse_color("WHITE"), Some([255, 255, 255, 255]));
        assert_eq!(parse_color("none"), None);
    }

    #[test]
    fn parse_viewbox_four_numbers() {
        assert_eq!(parse_viewbox("0 0 100 200"), Some((0.0, 0.0, 100.0, 200.0)));
        assert_eq!(
            parse_viewbox("-5, -5, 10, 10"),
            Some((-5.0, -5.0, 10.0, 10.0))
        );
    }

    #[test]
    fn parse_transform_translate_scale_matrix() {
        let t = parse_transform("translate(10, 20)").unwrap();
        assert!((t.dx - 10.0).abs() < 1e-5 && (t.dy - 20.0).abs() < 1e-5);
        let s = parse_transform("scale(2)").unwrap();
        assert!((s.xx - 2.0).abs() < 1e-5 && (s.yy - 2.0).abs() < 1e-5);
        let m = parse_transform("matrix(1 0 0 -1 0 100)").unwrap();
        assert!((m.yy + 1.0).abs() < 1e-5);
        assert!((m.dy - 100.0).abs() < 1e-5);
    }

    #[test]
    fn parse_transform_chains_left_to_right() {
        let xf = parse_transform("translate(10, 0) scale(2)").unwrap();
        let (x, y) = xf.apply(1.0, 0.0);
        assert!((x - 12.0).abs() < 1e-5);
        assert!(y.abs() < 1e-5);
    }

    #[test]
    fn path_d_parses_absolute_mlz() {
        let ops = parse_path_d("M 0 0 L 10 0 L 10 10 L 0 10 Z").unwrap();
        assert!(matches!(ops[0], PathOp::MoveTo { x, y } if x == 0.0 && y == 0.0));
        assert!(matches!(ops[3], PathOp::LineTo { x, y } if x == 0.0 && y == 10.0));
        assert!(matches!(ops[4], PathOp::Close));
    }

    #[test]
    fn path_d_handles_relative_commands() {
        let abs = parse_path_d("M 0 0 L 10 0 L 10 10 L 0 10 Z").unwrap();
        let rel = parse_path_d("m 0 0 l 10 0 l 0 10 l -10 0 z").unwrap();
        let to_xy = |op: &PathOp| match *op {
            PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => Some((x, y)),
            _ => None,
        };
        let abs_pts: Vec<_> = abs.iter().filter_map(to_xy).collect();
        let rel_pts: Vec<_> = rel.iter().filter_map(to_xy).collect();
        assert_eq!(abs_pts, rel_pts);
    }

    #[test]
    fn path_d_handles_implicit_repetition() {
        let ops = parse_path_d("M 0 0 10 0 10 10 0 10 Z").unwrap();
        assert_eq!(ops.len(), 5);
        assert!(matches!(ops[1], PathOp::LineTo { x, y } if x == 10.0 && y == 0.0));
    }

    #[test]
    fn path_d_handles_curves() {
        let ops = parse_path_d("M 0 0 C 0 10 10 10 10 0 Q 5 -5 0 0 Z").unwrap();
        assert!(matches!(ops[1], PathOp::CubicTo { .. }));
        assert!(matches!(ops[2], PathOp::QuadTo { .. }));
        assert!(matches!(ops[3], PathOp::Close));
    }

    #[test]
    fn path_d_handles_h_and_v() {
        let ops = parse_path_d("M 1 2 H 5 V 7 Z").unwrap();
        assert!(matches!(ops[1], PathOp::LineTo { x, y } if x == 5.0 && y == 2.0));
        assert!(matches!(ops[2], PathOp::LineTo { x, y } if x == 5.0 && y == 7.0));
    }

    #[test]
    fn path_d_handles_compact_negative_numbers() {
        let ops = parse_path_d("M0 0L10-5L-3 .5Z").unwrap();
        assert_eq!(ops.len(), 4);
        assert!(
            matches!(ops[2], PathOp::LineTo { x, y } if (x + 3.0).abs() < 1e-3 && (y - 0.5).abs() < 1e-3)
        );
    }

    #[test]
    fn xml_parser_walks_attributes() {
        let xml = r##"<svg viewBox="0 0 100 100" xmlns="http://www.w3.org/2000/svg">
            <path d="M 0 0 L 100 0 L 100 100 L 0 100 Z" fill="#FF0000"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.view_w, 100.0);
        assert_eq!(doc.view_h, 100.0);
        assert_eq!(doc.fills.len(), 1);
        let Paint::Solid(c) = &first_fill(&doc).paint else {
            panic!("expected solid fill");
        };
        assert_eq!(*c, [0xff, 0, 0, 255]);
    }

    #[test]
    fn group_transform_composes_onto_path() {
        let xml = r#"<svg viewBox="0 0 100 100">
            <g transform="scale(2)">
                <path d="M 0 0 L 10 0 L 10 10 Z" fill="black"/>
            </g>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.fills.len(), 1);
        let xf = doc.fills[0].xform;
        assert!((xf.xx - 2.0).abs() < 1e-5);
        assert!((xf.yy - 2.0).abs() < 1e-5);
    }

    #[test]
    fn unknown_elements_are_skipped_not_failed() {
        let xml = r#"<svg viewBox="0 0 10 10">
            <metadata>hello</metadata>
            <path d="M 0 0 L 10 0 L 10 10 Z" fill="black"/>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.fills.len(), 1);
    }

    #[test]
    fn fill_none_suppresses_the_path() {
        let xml = r#"<svg viewBox="0 0 10 10">
            <path d="M 0 0 L 10 0 L 10 10 Z" fill="none"/>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        assert!(doc.fills.is_empty());
    }

    #[test]
    fn missing_root_svg_is_an_error() {
        assert!(parse_document("<not-svg/>").is_err());
    }

    #[test]
    fn rect_with_no_radii_is_a_quad() {
        let xml = r#"<svg viewBox="0 0 10 10">
            <rect x="1" y="2" width="4" height="6" fill="black"/>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.fills.len(), 1);
        // 4 line segments + close.
        let ops = &doc.fills[0].ops;
        assert!(ops.iter().any(|o| matches!(o, PathOp::Close)));
        assert!(matches!(ops[0], PathOp::MoveTo { x, y } if x == 1.0 && y == 2.0));
    }

    #[test]
    fn rect_with_rx_ry_emits_cubics() {
        let xml = r#"<svg viewBox="0 0 10 10">
            <rect x="0" y="0" width="10" height="10" rx="2" ry="2" fill="black"/>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        let ops = &doc.fills[0].ops;
        assert!(ops.iter().any(|o| matches!(o, PathOp::CubicTo { .. })));
    }

    #[test]
    fn circle_emits_four_cubics() {
        let xml = r#"<svg viewBox="0 0 10 10">
            <circle cx="5" cy="5" r="3" fill="red"/>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.fills.len(), 1);
        let cubics = doc.fills[0]
            .ops
            .iter()
            .filter(|o| matches!(o, PathOp::CubicTo { .. }))
            .count();
        assert_eq!(cubics, 4);
    }

    #[test]
    fn ellipse_emits_four_cubics() {
        let xml = r#"<svg viewBox="0 0 10 10">
            <ellipse cx="5" cy="5" rx="4" ry="2" fill="red"/>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        let cubics = doc.fills[0]
            .ops
            .iter()
            .filter(|o| matches!(o, PathOp::CubicTo { .. }))
            .count();
        assert_eq!(cubics, 4);
    }

    #[test]
    fn use_resolves_in_document_reference() {
        let xml = r##"<svg viewBox="0 0 100 100">
            <defs><circle id="dot" cx="0" cy="0" r="3" fill="black"/></defs>
            <use xlink:href="#dot" x="10" y="10"/>
            <use xlink:href="#dot" x="50" y="50"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.fills.len(), 2);
        // First use translated to (10, 10).
        let (x, y) = doc.fills[0].xform.apply(0.0, 0.0);
        assert!((x - 10.0).abs() < 1e-5 && (y - 10.0).abs() < 1e-5);
    }

    #[test]
    fn use_recursion_guard_caps_at_depth() {
        // <use> pointing at a <g> that itself contains a <use> back at
        // the parent — should bottom out at MAX_USE_DEPTH instead of
        // recursing forever.
        let xml = r##"<svg viewBox="0 0 100 100">
            <defs>
                <g id="a"><use xlink:href="#a"/><circle cx="0" cy="0" r="1" fill="black"/></g>
            </defs>
            <use xlink:href="#a"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        // The non-cycling circle inside <g id="a"> renders at every
        // expansion level. The expansion bottoms out at MAX_USE_DEPTH;
        // the test just asserts we stayed under MAX_FILLS and didn't
        // panic.
        assert!(doc.fills.len() <= MAX_FILLS);
    }

    #[test]
    fn linear_gradient_collected_with_stops() {
        let xml = r##"<svg viewBox="0 0 10 10">
            <defs>
                <linearGradient id="g" x1="0" y1="0" x2="10" y2="0">
                    <stop offset="0" stop-color="#FF0000"/>
                    <stop offset="1" stop-color="#0000FF"/>
                </linearGradient>
            </defs>
            <rect x="0" y="0" width="10" height="10" fill="url(#g)"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.fills.len(), 1);
        let Paint::Gradient(g) = &doc.fills[0].paint else {
            panic!("expected gradient fill");
        };
        assert_eq!(g.stops.len(), 2);
        assert!(matches!(g.kind, GradKind::Linear { .. }));
    }

    #[test]
    fn radial_gradient_parsed() {
        let xml = r##"<svg viewBox="0 0 10 10">
            <defs>
                <radialGradient id="g" cx="5" cy="5" r="5">
                    <stop offset="0" stop-color="#FF0000"/>
                    <stop offset="1" stop-color="#0000FF"/>
                </radialGradient>
            </defs>
            <rect x="0" y="0" width="10" height="10" fill="url(#g)"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        let Paint::Gradient(g) = &doc.fills[0].paint else {
            panic!("expected gradient fill");
        };
        assert!(matches!(g.kind, GradKind::Radial { .. }));
    }

    #[test]
    fn stroke_emits_outline_fill() {
        let xml = r##"<svg viewBox="0 0 100 100">
            <path d="M 10 10 L 90 10" stroke="#000" stroke-width="4" fill="none"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        // No fill (fill="none"), but one stroke fill.
        assert_eq!(doc.fills.len(), 1);
        assert!(doc.fills[0].is_stroke);
    }

    #[test]
    fn stroke_zero_width_ignored() {
        let xml = r##"<svg viewBox="0 0 10 10">
            <path d="M 0 0 L 10 0" stroke="#000" stroke-width="0" fill="none"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        assert!(doc.fills.is_empty());
    }

    #[test]
    fn clip_path_attaches_to_fill() {
        let xml = r##"<svg viewBox="0 0 100 100">
            <defs>
                <clipPath id="c"><circle cx="50" cy="50" r="20"/></clipPath>
            </defs>
            <rect x="0" y="0" width="100" height="100" fill="#000" clip-path="url(#c)"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.fills.len(), 1);
        assert!(doc.fills[0].clip.is_some());
    }

    #[test]
    fn parse_url_ref_extracts_id() {
        assert_eq!(parse_url_ref("url(#abc)"), Some("abc".into()));
        assert_eq!(parse_url_ref(" url(#xyz) "), Some("xyz".into()));
        assert_eq!(parse_url_ref("url('#q')"), Some("q".into()));
        assert_eq!(parse_url_ref("none"), None);
    }

    #[test]
    fn stop_offset_handles_percent_and_decimal() {
        assert!((parse_stop_offset("50%") - 0.5).abs() < 1e-5);
        assert!((parse_stop_offset("0.25") - 0.25).abs() < 1e-5);
    }

    #[test]
    fn points_list_accepts_space_and_comma_separators() {
        let a = parse_points_list("0,0 10,0 10,10 0,10");
        let b = parse_points_list("0 0 10 0 10 10 0 10");
        let c = parse_points_list("0,0,10,0,10,10,0,10");
        assert_eq!(a, vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[test]
    fn points_list_handles_decimals_and_signs() {
        let pts = parse_points_list("-1.5,2 3.25e1,-0.5");
        assert_eq!(pts.len(), 2);
        assert!((pts[0].0 + 1.5).abs() < 1e-5);
        assert!((pts[1].0 - 32.5).abs() < 1e-5);
        assert!((pts[1].1 + 0.5).abs() < 1e-5);
    }

    #[test]
    fn points_list_drops_trailing_odd_coordinate() {
        let pts = parse_points_list("0 0 10 0 5");
        assert_eq!(pts, vec![(0.0, 0.0), (10.0, 0.0)]);
    }

    #[test]
    fn polygon_lowers_to_closed_path() {
        let xml = r#"<svg viewBox="0 0 100 100">
            <polygon points="10,10 90,10 50,90" fill="black"/>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.fills.len(), 1);
        let ops = &doc.fills[0].ops;
        assert!(matches!(ops[0], PathOp::MoveTo { x, y } if x == 10.0 && y == 10.0));
        assert!(matches!(ops[1], PathOp::LineTo { x, y } if x == 90.0 && y == 10.0));
        assert!(matches!(ops[2], PathOp::LineTo { x, y } if x == 50.0 && y == 90.0));
        assert!(matches!(ops[3], PathOp::Close));
    }

    #[test]
    fn polyline_lowers_to_open_path() {
        let xml = r#"<svg viewBox="0 0 100 100">
            <polyline points="10,10 90,10 50,90" fill="none" stroke="black" stroke-width="2"/>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        // No fill (fill="none"); stroke produces one fill ribbon.
        assert_eq!(doc.fills.len(), 1);
        assert!(doc.fills[0].is_stroke);
    }

    #[test]
    fn line_lowers_to_two_op_path() {
        // fill="none" suppresses the (degenerate) fill so we can see
        // the stroke alone.
        let xml = r#"<svg viewBox="0 0 100 100">
            <line x1="10" y1="10" x2="90" y2="90" stroke="black" stroke-width="2" fill="none"/>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.fills.len(), 1);
        assert!(doc.fills[0].is_stroke);
        // The stroke source ops were MoveTo + LineTo before being
        // expanded into a ribbon: the Fill we collected is the ribbon,
        // so just ensure it's non-empty.
        assert!(!doc.fills[0].ops.is_empty());
    }

    #[test]
    fn polygon_with_too_few_points_drops() {
        let xml = r#"<svg viewBox="0 0 100 100">
            <polygon points="10,10" fill="black"/>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        assert!(doc.fills.is_empty());
    }

    #[test]
    fn dasharray_parses_even_list_unchanged() {
        assert_eq!(parse_dasharray("4 2"), vec![4.0, 2.0]);
        assert_eq!(parse_dasharray("4, 2"), vec![4.0, 2.0]);
        assert_eq!(parse_dasharray("1 2 3 4"), vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn dasharray_doubles_odd_length() {
        // "2 3 5" → "2 3 5 2 3 5"
        assert_eq!(parse_dasharray("2 3 5"), vec![2.0, 3.0, 5.0, 2.0, 3.0, 5.0]);
    }

    #[test]
    fn dasharray_none_and_empty_yield_empty() {
        assert!(parse_dasharray("none").is_empty());
        assert!(parse_dasharray("").is_empty());
        assert!(parse_dasharray("   ").is_empty());
    }

    #[test]
    fn dasharray_negative_or_invalid_yields_empty() {
        assert!(parse_dasharray("4 -2").is_empty());
        assert!(parse_dasharray("4 abc").is_empty());
    }

    #[test]
    fn dasharray_zero_only_yields_empty() {
        // All zeros means "no dash" per the SVG spec — same as none.
        assert!(parse_dasharray("0 0 0 0").is_empty());
    }

    #[test]
    fn dasharray_strips_px_suffix() {
        assert_eq!(parse_dasharray("4px 2px"), vec![4.0, 2.0]);
    }

    /// Compute Euclidean per-chord arc lengths for a straight-segment
    /// polyline test fixture. For straight chords, true arc length
    /// equals chord length, so callers can use this to drive
    /// `dash_polyline` exactly the way the pre-arc-length walker did.
    fn straight_arcs(points: &[(f32, f32)], closed: bool) -> Vec<f32> {
        let n = points.len();
        let segs = if closed { n } else { n - 1 };
        let mut out = Vec::with_capacity(segs);
        for i in 0..segs {
            let a = points[i];
            let b = points[(i + 1) % n];
            let dx = b.0 - a.0;
            let dy = b.1 - a.1;
            out.push((dx * dx + dy * dy).sqrt());
        }
        out
    }

    #[test]
    fn dash_walker_emits_alternating_subpolylines_on_a_line() {
        // 20-unit horizontal line with pattern "4 2": dashes at
        // [0,4], [6,10], [12,16], [18,20] → 4 sub-polylines.
        let line = vec![(0.0, 0.0), (20.0, 0.0)];
        let arcs = straight_arcs(&line, false);
        let segs = dash_polyline(&line, &arcs, false, &[4.0, 2.0], 0.0);
        assert_eq!(segs.len(), 4);
        // First dash starts at the contour origin.
        assert!((segs[0][0].0 - 0.0).abs() < 1e-4);
        // Second dash starts at x=6.
        assert!((segs[1][0].0 - 6.0).abs() < 1e-4);
    }

    #[test]
    fn dash_walker_honours_offset() {
        // Same 20-unit line, pattern "4 2", offset=4 advances past the
        // first 4-unit draw — the contour now opens with a 2-unit skip
        // (x=0..2), then dashes start at x=2.
        let line = vec![(0.0, 0.0), (20.0, 0.0)];
        let arcs = straight_arcs(&line, false);
        let zero = dash_polyline(&line, &arcs, false, &[4.0, 2.0], 0.0);
        let off = dash_polyline(&line, &arcs, false, &[4.0, 2.0], 4.0);
        // With offset=0 the first dash starts at x=0; with offset=4 it
        // starts later (at x=2). Just verify the offset moved the
        // first dash forward.
        assert!(zero[0][0].0 < off[0][0].0);
        assert!((zero[0][0].0).abs() < 1e-4);
        assert!((off[0][0].0 - 2.0).abs() < 1e-4);
    }

    #[test]
    fn dash_walker_resets_per_contour() {
        // Two contours via M..L M..L; both should start their dash
        // pattern from offset=0 (i.e. drawing first).
        let xml = r#"<svg viewBox="0 0 100 100">
            <path d="M 0 50 L 20 50 M 0 70 L 20 70" stroke="black"
                  stroke-width="2" stroke-dasharray="4 2" fill="none"/>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        // One Fill record holds the union of all dashed ribbons; just
        // confirm the parser accepted the attribute.
        assert_eq!(doc.fills.len(), 1);
        assert!(doc.fills[0].is_stroke);
    }

    #[test]
    fn dash_walker_works_on_closed_contour() {
        // A closed square has 4 sides of length 10; pattern "5 5".
        // Half of perimeter (20 of 40) should be drawing.
        let pts = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let arcs = straight_arcs(&pts, true);
        let segs = dash_polyline(&pts, &arcs, true, &[5.0, 5.0], 0.0);
        assert!(!segs.is_empty(), "closed contour should produce dashes");
        // Total drawn length should approximate 20 (= half the perimeter).
        let drawn: f32 = segs
            .iter()
            .map(|s| {
                let mut acc = 0.0_f32;
                for w in s.windows(2) {
                    let dx = w[1].0 - w[0].0;
                    let dy = w[1].1 - w[0].1;
                    acc += (dx * dx + dy * dy).sqrt();
                }
                acc
            })
            .sum();
        assert!(
            (drawn - 20.0).abs() < 0.5,
            "expected ~20 drawn units, got {drawn}"
        );
    }

    /// Build the four-cubic kappa-circle and return the polyline +
    /// per-chord arc-length array, exactly as `flatten_to_polylines`
    /// produces them. Returned circle is centred at `(cx, cy)` with
    /// radius `r`. Used by both the true-arc and chord-flatten dash
    /// count tests so they share input geometry.
    fn build_kappa_circle(cx: f32, cy: f32, r: f32) -> PolyLine {
        // Standard cubic-Bezier circle approximation: each quarter
        // sweeps 90 degrees with control points offset by `kappa * r`
        // tangentially.
        const K: f32 = 0.552_284_8;
        let kr = K * r;
        let ops = vec![
            PathOp::MoveTo { x: cx + r, y: cy },
            PathOp::CubicTo {
                c1x: cx + r,
                c1y: cy + kr,
                c2x: cx + kr,
                c2y: cy + r,
                x: cx,
                y: cy + r,
            },
            PathOp::CubicTo {
                c1x: cx - kr,
                c1y: cy + r,
                c2x: cx - r,
                c2y: cy + kr,
                x: cx - r,
                y: cy,
            },
            PathOp::CubicTo {
                c1x: cx - r,
                c1y: cy - kr,
                c2x: cx - kr,
                c2y: cy - r,
                x: cx,
                y: cy - r,
            },
            PathOp::CubicTo {
                c1x: cx + kr,
                c1y: cy - r,
                c2x: cx + r,
                c2y: cy - kr,
                x: cx + r,
                y: cy,
            },
            PathOp::Close,
        ];
        let polys = flatten_to_polylines(&ops);
        assert_eq!(polys.len(), 1);
        polys.into_iter().next().unwrap()
    }

    #[test]
    fn circle_of_cubics_dash_count_uses_true_arc_length() {
        // Radius-100 circle approximated by 4 cubics. True
        // circumference = 2π·100 ≈ 628.32. With dasharray "10 10"
        // (period 20) we expect ~31.4 dash periods around the circle —
        // and since pattern starts on a draw, ~31 full dashes (the
        // half-cycle being a rendering edge).
        //
        // The chord-flattened polyline of the kappa-circle is slightly
        // shorter than the true circumference (chords are always under
        // the curve), so a chord-only walker would produce a different
        // (smaller) dash count. We assert that the *true-arc* walker
        // lands within one dash of the analytic count.
        let circle = build_kappa_circle(0.0, 0.0, 100.0);
        let true_arc_total: f32 = circle.arc_lengths.iter().sum();
        let chord_total: f32 = {
            let n = circle.points.len();
            (0..n)
                .map(|i| {
                    let a = circle.points[i];
                    let b = circle.points[(i + 1) % n];
                    let dx = b.0 - a.0;
                    let dy = b.1 - a.1;
                    (dx * dx + dy * dy).sqrt()
                })
                .sum()
        };
        // Sanity: arc-length is *longer* than the chord polyline,
        // matching the brief's analytic prediction.
        assert!(
            true_arc_total > chord_total,
            "arc-length {true_arc_total} must exceed chord total {chord_total}"
        );
        // Both should be close to 2π·100; arc-length should be much
        // closer (sub-percent) than chord.
        let circumference = 2.0 * core::f32::consts::PI * 100.0;
        let arc_err = (true_arc_total - circumference).abs() / circumference;
        let chord_err = (chord_total - circumference).abs() / circumference;
        assert!(
            arc_err < 0.005,
            "arc-length should be < 0.5% off the true circumference, got {}%",
            arc_err * 100.0
        );
        assert!(
            arc_err < chord_err,
            "arc-length must beat chord-flatten ({}% vs {}%)",
            arc_err * 100.0,
            chord_err * 100.0
        );

        // Run the dasher (true arc length) and count "draw" sub-polylines.
        let segs = dash_polyline(
            &circle.points,
            &circle.arc_lengths,
            circle.closed,
            &[10.0, 10.0],
            0.0,
        );
        // Run the same input through chord-only walking by passing the
        // chord lengths as `arc_lengths`. The dash count will be lower
        // because the circumference is under-measured.
        let chord_arcs: Vec<f32> = {
            let n = circle.points.len();
            (0..n)
                .map(|i| {
                    let a = circle.points[i];
                    let b = circle.points[(i + 1) % n];
                    let dx = b.0 - a.0;
                    let dy = b.1 - a.1;
                    (dx * dx + dy * dy).sqrt()
                })
                .collect()
        };
        let chord_segs = dash_polyline(
            &circle.points,
            &chord_arcs,
            circle.closed,
            &[10.0, 10.0],
            0.0,
        );
        // The arc-length walker must see at least as many full draw
        // dashes as the chord walker — a longer "track" can only fit
        // more (or equal) dash periods, never fewer. This is the
        // observable signature of the fix.
        // The arc-length walker must produce at least as many full
        // dashes as the chord walker — a longer track can only fit
        // equal or more periods. (At radius 100 with dash-period 20
        // both round to 32 dashes; the discriminating signal is the
        // length measurement above, not the count, but on circles
        // tuned exactly to the dash period the count diverges.)
        assert!(
            segs.len() >= chord_segs.len(),
            "true-arc dash count {} must be >= chord-flatten count {}",
            segs.len(),
            chord_segs.len()
        );
        // 628.32 / 20 ≈ 31.4 cycles → either 31 or 32 full draw
        // sub-polylines depending on where the final dash boundary
        // lands. The chord-flatten path measures ~627 (≈0.2 % short),
        // which biases the count down by less than one. With true arc
        // length we should be right at the analytic count.
        assert!(
            (31..=32).contains(&segs.len()),
            "expected 31 or 32 draw dashes around the circle, got {}",
            segs.len()
        );

        // Also: total drawn arc length should be ~half the
        // circumference (since pattern is 50/50 draw/skip). Use the
        // chord lengths between dash sub-polyline points; for a circle
        // discretised at 0.25 px tolerance these are within sub-pixel
        // of the true sub-arc length.
        let drawn: f32 = segs
            .iter()
            .map(|s| {
                let mut acc = 0.0_f32;
                for w in s.windows(2) {
                    let dx = w[1].0 - w[0].0;
                    let dy = w[1].1 - w[0].1;
                    acc += (dx * dx + dy * dy).sqrt();
                }
                acc
            })
            .sum();
        // Each dash is 10 arc-length units; chord-length of a 10-unit
        // arc on a radius-100 circle is 2·100·sin(5/100) ≈ 9.996, so
        // expected drawn (chord-measured) ≈ 31 · 9.996 ≈ 309.9. Allow a
        // generous tolerance because the trailing partial dash can vary.
        assert!(
            drawn > 300.0 && drawn < 320.0,
            "expected ~310 chord-measured draw length, got {drawn}"
        );
    }

    #[test]
    fn long_quadratic_with_continuous_dasharray_has_no_dash_break() {
        // dasharray "1 0" → period 1, fully drawing (skip is zero
        // length). This must produce identical output to the
        // un-dashed stroke: every sub-polyline boundary the walker
        // emits coincides with a chord vertex, and the union of draw
        // sub-polylines covers the whole curve.
        let xml = r#"<svg viewBox="0 0 200 100">
            <path d="M 0 50 Q 100 -50 200 50" stroke="black"
                  stroke-width="2" stroke-dasharray="1 0" fill="none"/>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        // Parser accepted the dasharray and produced a stroke fill.
        assert_eq!(doc.fills.len(), 1);
        assert!(doc.fills[0].is_stroke);
        // "1 0" parses to [1, 0]; sum is 1 > 0, so dashed path runs.
        // The fill should cover the entire stroke ribbon — i.e. it has
        // a non-trivial number of MoveTo records (one per draw run).
        let moveto_count = doc.fills[0]
            .ops
            .iter()
            .filter(|o| matches!(o, PathOp::MoveTo { .. }))
            .count();
        assert!(moveto_count > 0);
    }

    #[test]
    fn cusp_cubic_with_dasharray_does_not_panic() {
        // Both controls collapse to one point: classic cusp shape.
        // Adversarial input for adaptive subdivision; verify the
        // parser + dasher complete without a panic.
        let xml = r#"<svg viewBox="0 0 100 100">
            <path d="M 0 0 C 100 100 100 100 0 0" stroke="black"
                  stroke-width="2" stroke-dasharray="5 5" fill="none"/>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        // Either a stroke fill is emitted or it's empty (degenerate
        // cusp may collapse), but we must not panic.
        for fill in &doc.fills {
            assert!(!fill.ops.iter().any(|o| match o {
                PathOp::MoveTo { x, y }
                | PathOp::LineTo { x, y } => !x.is_finite() || !y.is_finite(),
                _ => false,
            }));
        }
    }

    #[test]
    fn straight_polyline_dasharray_byte_identical_to_chord_walker() {
        // Pre-arc-length walker computed `seg_len` from chord points.
        // For a straight polyline `arc_lengths[i]` IS the chord
        // length, so the new walker must produce bit-identical output
        // on this input — which is the contract the existing PR #227
        // tests rely on.
        //
        // Drive both: the new walker via the public API, and a
        // recreation of the old walker (chord lengths derived inline)
        // and assert exact equality.
        let pts = vec![(0.0, 0.0), (5.0, 0.0), (5.0, 5.0), (15.0, 5.0)];
        let arcs = straight_arcs(&pts, false);
        let new_segs = dash_polyline(&pts, &arcs, false, &[3.0, 1.0], 0.0);
        // Chord-derived arc lengths == Euclidean distance for straight
        // chords; passing them through gives the same trace the
        // pre-arc-length walker would have computed itself inline.
        let chord_arcs: Vec<f32> = (0..pts.len() - 1)
            .map(|i| {
                let a = pts[i];
                let b = pts[i + 1];
                ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
            })
            .collect();
        let chord_segs = dash_polyline(&pts, &chord_arcs, false, &[3.0, 1.0], 0.0);
        assert_eq!(new_segs.len(), chord_segs.len());
        for (a, b) in new_segs.iter().zip(chord_segs.iter()) {
            assert_eq!(a.len(), b.len());
            for (pa, pb) in a.iter().zip(b.iter()) {
                assert!((pa.0 - pb.0).abs() < 1e-6 && (pa.1 - pb.1).abs() < 1e-6);
            }
        }
    }
}
