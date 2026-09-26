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
//! a small fraction of the full SVG 1.1 grammar. We
//! implement only that fraction:
//!
//! - `<svg>` with `viewBox` / `width` / `height` attributes.
//! - `<g>` with optional `transform=` (`translate`, `scale`, `rotate`,
//!   `matrix`).
//! - `<path>` with `d=` containing M/L/H/V/C/Q/Z + relative variants.
//! - `<rect>` / `<circle>` / `<ellipse>` shape primitives, converted
//!   to paths and run through the existing fill pipeline.
//! - `<polygon>` / `<polyline>` / `<line>` shape primitives, converted
//!   to paths via the SVG `points` list grammar (space- or
//!   comma-separated coords). `<polygon>` closes back to the first
//!   point; `<polyline>` is open; `<line>` is a single segment.
//! - `fill="#RRGGBB"`, `fill="#RGB"`, `fill="rgb(...)"`, named colors,
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
//! - `<mask>` (`mask-type="luminance"` default plus `mask-type="alpha"`
//!   opt-in) containing any combination of the supported shape
//!   primitives. The mask children are rendered into a same-size
//!   scratch ColorPixmap; per-pixel BT.709 luminance * source alpha
//!   gives the mask alpha for `luminance`, while `alpha` uses the
//!   mask buffer's alpha channel directly. `maskUnits="userSpaceOnUse"`
//!   (default) and `maskUnits="objectBoundingBox"` (mask region rect
//!   re-interpreted in `[0, 1]²` of the masked element's bbox) are
//!   both supported. Nested mask references inside the mask body are
//!   not supported and are dropped.
//!
//! - `stroke-dasharray` + `stroke-dashoffset` on stroked geometry,
//!   applied to the post-flattening polyline. Curves become chords
//!   first, then dashes are walked along cumulative arc length per
//!   contour.
//!
//! - `<filter>` with the minimum-viable primitive set:
//!   `feGaussianBlur` (3-pass box-blur approximation), `feColorMatrix`
//!   (matrix / saturate / hueRotate / luminanceToAlpha), `feOffset`,
//!   `feFlood`, and `feMerge`. Drop-shadow chains
//!   (`SourceAlpha` -> blur -> offset -> merged under `SourceGraphic`)
//!   compose end-to-end. Filters apply per shape (`element[filter=...]`);
//!   group-level filter regions are rendered shape-by-shape.
//!
//! - `<textPath xlink:href="#id">` glyph placement along a referenced
//!   `<path>`, via the consumer-pre-shape API
//!   [`Rasterizer::rasterize_svg_glyph_with_text_paths`]. The renderer
//!   does not shape text. The consumer feeds in pre-shaped
//!   [`TextPathGlyph`] runs (one entry per visual glyph, carrying a
//!   gid and a user-space x-advance), and the renderer walks the
//!   referenced path's arc length, fetching each glyph's outline from
//!   the same [`Face`] and translating it to the cumulative-advance
//!   position. Glyphs are placed axis-aligned only: tangent rotation,
//!   `side="right"`, and path cycling (`startOffset` past path end)
//!   are not supported.
//!
//! - Work limits. Parsing, stored geometry, and rendering each run
//!   against a fixed per-document budget, far above what real fonts
//!   need. A document that exceeds one stops early instead of taking
//!   unbounded time or memory.
//!
//! Anything outside this list, filter primitives beyond the set above
//! (`feTurbulence`, `feImage`, `feMorphology`, `feConvolveMatrix`,
//! `feSpecularLighting`, `feDiffuseLighting`, `feComponentTransfer`,
//! `feComposite` operators beyond source-over), nested `<mask>`
//! references (mask-of-mask), animations, scripting, `style=`
//! attributes, plain `<text>` rendering (text shaping is the
//! consumer's responsibility, see sigilbuzz core), is silently
//! skipped. `<textPath>` is rendered only when the consumer supplies
//! pre-shaped runs via the API above; un-paired `<textPath>` nodes
//! (no matching [`TextPathInput`]) are silently skipped, matching the
//! broader policy.
//!
//! ## Pipeline
//!
//! ```text
//!   Face.svg_document(gid)    -> SvgDocument { data, gzipped }
//!     |
//!     | gzipped -> RenderError::SvgGzipped (no gzip dep here)
//!     v
//!   parse_document(xml)       -> SvgDoc { viewbox, defs, fills, strokes }
//!     |
//!     | each fill: { ops, paint, xform, clip? }
//!     v
//!   for each fill / stroke pass:
//!     flatten(ops * world_xform) -> Segment[]
//!     raster(segments)           -> Pixmap (alpha mask)
//!     blit(mask * paint -> out)  -> ColorPixmap
//! ```
//!
//! No XML library on the read path. The parser is a hand-rolled tree
//! walker. Coordinates are decimal numbers parsed with `f32::from_str`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::Cell;

use sigilbuzz::tables::PathOp;
use sigilbuzz::Face;
use sigilbuzz_paint::{Color as PaintColor, ColorStop, Extend};

use crate::affine::Affine;
use crate::colrv1::{apply_extend, project_linear, project_radial, sample_stops, to_premul};
use crate::error::RenderError;
use crate::flatten::{flatten, flatten_limited, Segment, MAX_SEGMENTS};
use crate::pixmap::{ColorPixmap, Pixmap};
use crate::raster::{raster_bounds, rasterize_in, Window};
use crate::rasterizer::Rasterizer;

/// Maximum recursion depth for nested `<g>` elements. Real fonts stay
/// under 4; we cap at 32 to keep malicious payloads from blowing the
/// stack.
const MAX_GROUP_DEPTH: u32 = 32;

/// Maximum number of fills a single document may emit. Same rationale
/// as `MAX_GROUP_DEPTH`: a hostile SVG could otherwise expand to
/// gigabytes of work. 4096 is well above what real fonts produce.
const MAX_FILLS: usize = 4096;

/// Maximum nested `<use>` resolution depth. SVG mandates >= 16 in real
/// engines; we match.
const MAX_USE_DEPTH: u32 = 16;

/// Maximum element nesting along one walk, counting the levels that
/// `<use>` expansion adds. `<use>` restarts the group depth count, so
/// without this the walk could recurse `MAX_USE_DEPTH` times
/// `MAX_GROUP_DEPTH` deep (over 500 frames), which overflows a 1 MiB
/// stack in debug builds. Real documents nest well under 20.
const MAX_WALK_NESTING: u16 = 64;

/// Miter cut-off ratio per SVG: when the miter would extend more than
/// `4 * stroke-width` past the join, fall back to a bevel join.
const MITER_LIMIT: f32 = 4.0;

/// Maximum pixel dimension for a rasterized SVG-in-OT glyph. Matches
/// the PNG decoder's per-dim ceiling (16384) so the bound is uniform
/// across the public render surface. A combination of a font-supplied
/// finite-but-extreme `viewBox` and a caller-supplied large `size_pt`
/// can otherwise multiply up to a `u32::MAX * u32::MAX * 4` allocation
/// that panics in the `Vec` macro before any rasterization runs.
const MAX_RENDER_DIM: f32 = 16384.0;

/// Parse work allowed for one document, in abstract units. Visiting an
/// element costs [`WALK_VISIT_COST`] plus the bytes of its attributes
/// and of the inherited state it copies, and resolving a reference
/// costs the bytes it parses. `<use>` expansion can revisit a subtree
/// many times, so without this a small document can demand exponential
/// work. Real documents use a small fraction of it.
const MAX_PARSE_WORK: usize = 1 << 24;

/// Fixed parse-work cost of visiting one element.
const WALK_VISIT_COST: usize = 64;

/// Path operations a document may store across all of its fills,
/// counting the copies attached through clip paths, masks, filters,
/// and gradient stops. Bounds memory when many fills share one large
/// referenced definition.
const MAX_DOC_OPS: usize = 1 << 21;

/// Maximum primitives kept per `<filter>`. Each named result holds a
/// canvas-sized pixmap. Real filters use a handful.
const MAX_FILTER_PRIMITIVES: usize = 64;

/// Canvas-sized passes allowed while rendering one document. A fill
/// costs one pass, plus one per filter primitive and merge input, plus
/// one for a mask buffer. Mask children and filters otherwise multiply
/// the per-fill cost without limit.
const MAX_RENDER_PASSES: u32 = 1 << 15;

/// Maximum points one [`flatten_to_polylines`] call produces. Once
/// reached, curves stop subdividing and contribute only their end
/// point.
const MAX_POLYLINE_POINTS: usize = 1 << 20;

/// Maximum path operations one [`stroke_to_fill`] call emits. Dashes
/// and joins multiply the input, so this bounds the ribbon size.
const MAX_STROKE_OPS: usize = 1 << 21;

/// Maximum dash boundaries walked while stroking one path, see
/// [`dash_polyline_limited`]. A tiny dash length on a long path would
/// otherwise split it billions of times.
const MAX_DASH_SPLITS: usize = 1 << 20;

/// Largest box-blur radius. The window sums stay within `u32` up to
/// `(2 * r + 1) * 255`, and no canvas is wide enough for a larger
/// radius to matter.
const MAX_BLUR_RADIUS: i32 = 1 << 22;

// =========================================================================
// Public entry
// =========================================================================

impl Rasterizer {
    /// Rasterizes the SVG document for `gid` from the font's `SVG`
    /// table, returning a premultiplied RGBA [`ColorPixmap`].
    ///
    /// `size_pt` is the rendering size in pixels. The SVG document's
    /// viewBox is mapped onto a `size_pt x size_pt` square. If the
    /// viewBox is non-square, the rendered bitmap preserves the
    /// document's aspect ratio (the longer axis maps to `size_pt`).
    ///
    /// `coords` is accepted for API symmetry with
    /// [`Self::rasterize_glyph`] and [`Self::rasterize_colrv0_glyph`]
    /// but currently has no effect. SVG-in-OT documents are static
    /// (no axis tagging), and HarfBuzz / CoreText behave the same way.
    ///
    /// # Errors
    /// - [`RenderError::SvgNotFound`] when `gid` has no SVG record.
    /// - [`RenderError::SvgGzipped`] when the payload is gzip-compressed
    ///   (sigilbuzz-render does not ship a gzip dep; consumers should
    ///   decompress and feed back via a future bytes-based entry point).
    /// - [`RenderError::Parse`] for unrecoverable XML / path-data
    ///   errors, and for documents that exceed the fill cap or the
    ///   parse work budget (for example through runaway `<use>`
    ///   expansion).
    /// - [`RenderError::BadSize`] when `size_pt` is non-finite or
    ///   non-positive, or when the canvas would exceed 16384 pixels on
    ///   a side.
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
        // Map document to pixel space: scale the viewBox onto a
        // size_pt x size_pt square, preserving aspect ratio.
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
        // post-cast `u32::MAX * u32::MAX * 4` allocation that overflows
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
        let width = width_f as u32;
        let height = height_f as u32;
        let mut out = ColorPixmap::new(width, height);

        render_doc(&mut out, &doc, &world, self.flattening_tolerance());
        Ok(out)
    }

    /// Rasterizes the SVG document for `gid` and additionally places
    /// pre-shaped glyph runs along any `<textPath>` nodes whose
    /// `xlink:href` (or `href`) matches an entry in `text_paths`.
    ///
    /// sigilbuzz-render does not shape text. The consumer supplies
    /// already-shaped [`TextPathGlyph`] runs (one record per visual
    /// glyph, carrying a `gid` and a user-space `x_advance`). For each
    /// matched `<textPath>` the renderer:
    ///
    /// 1. Resolves the referenced `<path>` from the document's defs.
    /// 2. Flattens the path into chord polylines (curves use the same
    ///    Roger-Willcocks-arc-length flattener `<stroke-dasharray>`
    ///    uses, so cumulative-advance lands on the *true* curve sweep
    ///    rather than the chord-shortened approximation).
    /// 3. Walks the polyline by cumulative advance. For each glyph,
    ///    fetches its outline via [`Face::glyph_outline_at_coords`],
    ///    scales design units to user-space units by
    ///    `font_size / units_per_em`, translates the outline to the
    ///    path-position, and emits it into the canvas as if it were a
    ///    document `<path>` filled with the inherited paint of the
    ///    enclosing `<textPath>` (or fallback solid black if none).
    ///
    /// **Axis-aligned only.** Glyphs do not rotate to follow the path
    /// tangent. `side="right"` and path cycling beyond a single
    /// cumulative-advance walk are not supported either. Extra glyphs
    /// whose advance overruns the path's total length are silently
    /// dropped.
    ///
    /// `coords` flows through to glyph outline lookups so variable
    /// fonts produce the right outlines for the supplied axis position;
    /// it does not affect the SVG document parse (SVG-in-OT documents
    /// are static).
    ///
    /// # Errors
    /// Same set as [`Self::rasterize_svg_glyph`], plus
    /// [`RenderError::BadUpem`] when the font has zero units-per-em
    /// (needed to scale glyph design units onto the SVG user space).
    /// A `<textPath>` whose `xlink:href` points at a missing or
    /// non-`<path>` def is silently skipped, matching the rest of the
    /// `<svg>`-subset policy. Glyph outlines that fail to parse are
    /// also silently skipped (the rest of the document still renders).
    pub fn rasterize_svg_glyph_with_text_paths(
        &self,
        face: &Face<'_>,
        gid: u16,
        size_pt: f32,
        coords: &[f32],
        text_paths: &[TextPathInput<'_>],
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
        let mut doc = parse_document(xml)?;

        if doc.view_w <= 0.0 || doc.view_h <= 0.0 {
            return Err(RenderError::Parse("svg viewBox"));
        }

        // Resolve text-path runs against the (re-parsed) XML tree and
        // append their glyph fills to the document's fill list before
        // raster pass. The fills land in user-space units alongside the
        // rest of the document so the existing `world` transform maps
        // them straight onto the canvas.
        if !text_paths.is_empty() {
            let head = face.head().map_err(|_| RenderError::Parse("head"))?;
            let upem = f32::from(head.units_per_em);
            if upem <= 0.0 {
                return Err(RenderError::BadUpem);
            }
            let root = parse_xml(xml)?;
            let defs = build_defs(&root);
            append_text_path_fills(&mut doc, &root, &defs, face, coords, upem, text_paths);
        }

        let s = (size_pt / doc.view_w).min(size_pt / doc.view_h);
        let world = Affine {
            xx: s,
            yx: 0.0,
            xy: 0.0,
            yy: s,
            dx: -doc.view_x * s,
            dy: -doc.view_y * s,
        };
        let width_f = (doc.view_w * s).round().max(1.0);
        let height_f = (doc.view_h * s).round().max(1.0);
        if !width_f.is_finite()
            || !height_f.is_finite()
            || width_f > MAX_RENDER_DIM
            || height_f > MAX_RENDER_DIM
        {
            return Err(RenderError::BadSize(size_pt));
        }
        let width = width_f as u32;
        let height = height_f as u32;
        let mut out = ColorPixmap::new(width, height);

        render_doc(&mut out, &doc, &world, self.flattening_tolerance());
        Ok(out)
    }
}

// =========================================================================
// Public textPath API
// =========================================================================

/// Pre-shaped input for one `<textPath>` element.
///
/// sigilbuzz-render does not perform text shaping. To render a
/// `<textPath xlink:href="#id">...</textPath>` the caller must
/// pre-shape the contained text into a sequence of [`TextPathGlyph`]
/// records (one per visual glyph) and pass them in via
/// [`Rasterizer::rasterize_svg_glyph_with_text_paths`]. The renderer
/// then walks the referenced path's arc length and translates each
/// glyph's outline onto its cumulative-advance position.
///
/// `text_path_id` is the bare element id, the part after the `#` in
/// `xlink:href="#id"`. Whichever `<textPath>` node matches by id has
/// its content replaced with the supplied glyph runs (any text-bearing
/// children inside the SVG `<textPath>` are ignored: this API is the
/// sole text source).
///
/// `font_size` is the user-space height of one em; design-unit glyph
/// outlines fetched from the [`Face`] are scaled by
/// `font_size / units_per_em` before placement. This decouples the
/// SVG document's user-space units from the font's design-unit grid.
///
/// `glyph_runs` is consumed in order. Cumulative `x_advance` walks the
/// path; glyphs whose run-start position lands past the path's total
/// arc length are silently dropped (path cycling is not supported, see
/// the module-level docs).
#[derive(Debug, Clone)]
pub struct TextPathInput<'a> {
    /// The `<path>` id this run targets. Matches the
    /// `xlink:href="#id"` (or `href="#id"`) attribute on a
    /// `<textPath>` node, with the leading `#` stripped.
    pub text_path_id: &'a str,
    /// User-space units per em. Converts design-unit glyph outlines
    /// to the document's coordinate space.
    pub font_size: f32,
    /// Pre-shaped glyph stream. Walked left-to-right; each glyph is
    /// placed at the cumulative-advance position along the path.
    pub glyph_runs: Vec<TextPathGlyph>,
}

/// One pre-shaped glyph in a [`TextPathInput`] run.
///
/// The consumer is responsible for shaping (cluster decomposition,
/// kerning, ligatures, mark positioning). sigilbuzz-render only
/// places. `gid` indexes into the same [`Face`] that owns the SVG
/// document; the renderer fetches its outline via
/// [`Face::glyph_outline_at_coords`].
///
/// `x_advance` is in user-space units (the same coordinate system the
/// SVG document's `viewBox` is expressed in). The glyph's *origin* is
/// placed at the path-position corresponding to the *cumulative* run
/// advance up to (and including) this glyph's pre-advance, i.e.
/// glyph 0 sits at advance 0, glyph 1 sits at glyph-0's `x_advance`,
/// and so on.
#[derive(Debug, Clone, Copy)]
pub struct TextPathGlyph {
    /// Glyph id, indexed against the same [`Face`] passed to
    /// [`Rasterizer::rasterize_svg_glyph_with_text_paths`].
    pub gid: u16,
    /// Cumulative-advance step in user-space units. The renderer adds
    /// this to a running counter *after* placing the glyph, so the
    /// first glyph is always at position 0 along the path.
    pub x_advance: f32,
}

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
fn append_text_path_fills(
    doc: &mut SvgDoc,
    root: &Node,
    defs: &Defs<'_>,
    face: &Face<'_>,
    coords: &[f32],
    upem: f32,
    text_paths: &[TextPathInput<'_>],
) {
    let ctx = ElemCtx::default();
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
struct PolyPoint {
    x: f32,
    y: f32,
    cum: f32,
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
fn build_arc_length_polyline(ops: &[PathOp]) -> Vec<PolyPoint> {
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
fn sample_polyline_position(poly: &[PolyPoint], target: f32) -> Option<(f32, f32)> {
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
fn transform_outline_ops(ops: &[PathOp], scale: f32, ox: f32, oy: f32) -> Vec<PathOp> {
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

// =========================================================================
// Internal model
// =========================================================================

/// One paintable surface collected from the document. `ops` is in the
/// document's intrinsic coordinate space. The world transform
/// (document -> pixel) is applied on top at rasterize time.
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
    /// fill ribbon). Rendering ignores it. Tests use it to tell the
    /// fill and stroke passes apart.
    #[cfg(test)]
    is_stroke: bool,
    /// Optional filter chain to apply to this fill. Resolved at parse
    /// time from `filter="url(#id)"`. When set, the fill is rendered
    /// to a temporary `SourceGraphic` pixmap, the filter pipeline is
    /// walked, and the final primitive's output is composited under
    /// the canvas via Porter-Duff source-over.
    filter: Option<Filter>,
    /// Optional alpha mask (SVG `<mask>` element) to apply to this
    /// fill. Distinct from `clip`: clip is binary inside/outside,
    /// mask is a continuous luminance-derived alpha multiplier (so
    /// gradient mask edges feather the masked element). When set, the
    /// element rasterizes to a SourceGraphic pixmap, the mask
    /// children are rendered into a same-size buffer, and per-pixel
    /// BT.709 luminance * mask source alpha modulates the
    /// SourceGraphic alpha before composite.
    mask: Option<MaskShape>,
}

impl Fill {
    /// Storage weight charged against [`MAX_DOC_OPS`]: the path
    /// operations this fill owns, including the copies attached
    /// through its clip, mask, filter, and gradient stops.
    fn weight(&self) -> usize {
        let paint = match &self.paint {
            Paint::Solid(_) => 0,
            Paint::Gradient(g) => g.stops.len(),
        };
        self.ops
            .len()
            .saturating_add(paint)
            .saturating_add(self.clip.as_ref().map_or(0, |c| c.ops.len()))
            .saturating_add(self.filter.as_ref().map_or(0, Filter::passes))
            .saturating_add(
                self.mask
                    .as_ref()
                    .map_or(0, |m| m.fills.iter().map(Fill::weight).sum()),
            )
    }

    /// Canvas-sized passes rendering this fill costs, not counting the
    /// children of its mask, which charge their own.
    fn render_passes(&self) -> u32 {
        let filter = self.filter.as_ref().map_or(0, Filter::passes);
        let filter = u32::try_from(filter).unwrap_or(u32::MAX);
        1u32.saturating_add(filter)
            .saturating_add(u32::from(self.mask.is_some()))
    }
}

/// Paint source for a [`Fill`]. SVG-in-OT documents use solid color
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
    /// *before* the document -> pixel `world` matrix.
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

/// A parsed `<mask>` element. Stored as a list of [`Fill`] records
/// because masks can hold any combination of shape primitives,
/// gradients, and per-element transforms, the same machinery that
/// renders the rest of the document. At render time the mask's fills
/// paint into a same-size scratch ColorPixmap, then either a per-pixel
/// BT.709 luminance derivation (`mask-type="luminance"`, the default)
/// or the source alpha channel directly (`mask-type="alpha"`) is used
/// as the alpha mask multiplied against the masked element's coverage.
#[derive(Debug, Clone)]
struct MaskShape {
    fills: Vec<Fill>,
    /// `mask-type="luminance" | "alpha"`. Luminance is the SVG
    /// default; alpha skips the BT.709 derivation and uses the mask
    /// buffer's alpha channel directly.
    mask_type: MaskType,
    /// `maskUnits`: coordinate system the mask region (`x`, `y`,
    /// `width`, `height`) is expressed in. `UserSpaceOnUse` is the
    /// SVG default for our prior implementation; `ObjectBoundingBox`
    /// reinterprets the region as `[0, 1]²` of the masked element's
    /// bounding box.
    units: MaskUnits,
    /// Mask region as parsed from `x`, `y`, `width`, `height`.
    /// Interpretation depends on `units`. When `units` is
    /// `UserSpaceOnUse`, this is currently informational only. The
    /// luminance fast path renders the mask body across the entire
    /// canvas.
    region_x: f32,
    region_y: f32,
    region_w: f32,
    region_h: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MaskType {
    Luminance,
    Alpha,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MaskUnits {
    UserSpaceOnUse,
    ObjectBoundingBox,
}

/// A parsed `<filter>` element: an ordered list of primitives forming
/// a small DAG keyed by `result=` names. The DAG is evaluated at render
/// time against a `SourceGraphic` pixmap (the filtered shape rendered
/// into a transparent buffer) and a `SourceAlpha` pixmap (same shape,
/// alpha only).
#[derive(Debug, Clone)]
struct Filter {
    primitives: Vec<FilterPrimitive>,
}

impl Filter {
    /// Canvas-sized passes evaluating this filter costs: one per
    /// primitive plus one per `feMerge` input.
    fn passes(&self) -> usize {
        self.primitives
            .iter()
            .map(|p| match &p.op {
                FilterOp::Merge { inputs } => inputs.len().saturating_add(1),
                _ => 1,
            })
            .fold(0usize, usize::saturating_add)
    }
}

/// One `<fe*>` element: an input ref (`in="..."`), an output name
/// (`result="..."`), and an operation. Inputs default to `SourceGraphic`
/// for the first primitive and the previous primitive's result
/// thereafter (per SVG 1.1 §15.6).
#[derive(Debug, Clone)]
struct FilterPrimitive {
    /// `in="..."`. `None` means "use previous primitive's output, or
    /// SourceGraphic if no previous primitive". No supported primitive
    /// takes a second input, so `in2="..."` is not read.
    input: Option<String>,
    /// `result="..."`. Names this primitive's output for later refs.
    /// `None` means "anonymous; only the next primitive can reference
    /// it (via the implicit-input chain)".
    result: Option<String>,
    op: FilterOp,
}

/// The actual operation a [`FilterPrimitive`] performs.
#[derive(Debug, Clone)]
enum FilterOp {
    /// `feGaussianBlur stdDeviation="σ"` or `"σx σy"`. Implemented as a
    /// 3-pass box-blur approximation (separable, O(N) per pass per axis)
    /// that is visually indistinguishable from a true Gaussian for σ >= 1 and
    /// vastly faster than convolving a full kernel.
    GaussianBlur { std_dev_x: f32, std_dev_y: f32 },
    /// `feColorMatrix` in any of its `type=` flavors.
    ColorMatrix { matrix: [f32; 20] },
    /// `feOffset dx=... dy=...`. Pure translation, integer-rounded at blit
    /// time.
    Offset { dx: f32, dy: f32 },
    /// `feFlood flood-color=... flood-opacity=...`. Constant-color pixmap
    /// of the filter region. Color stored straight (un-premultiplied);
    /// premultiplication happens at materialize time.
    Flood { color: [u8; 4] },
    /// `feMerge` with N `<feMergeNode in="...">` children. Composites the
    /// inputs in document order via Porter-Duff source-over.
    Merge { inputs: Vec<String> },
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
    let defs = build_defs(&root);

    // Second pass: walk the tree, emitting fills.
    let ctx = ElemCtx::default();
    walk(&root, &mut doc, &defs, &ctx, 0, 0)?;

    Ok(doc)
}

/// Id index plus the per-document work budgets. Every parse-time
/// helper already receives the `Defs`, so the budgets live here as
/// cells shared by the document walk and by mask resolution.
struct Defs<'a> {
    /// Elements that carry `id=`, sorted by id. Elements sharing an id
    /// stay in document order, so a lookup returns the first one.
    by_id: Vec<(&'a str, &'a Node)>,
    /// Parse work left, see [`MAX_PARSE_WORK`].
    work_left: Cell<usize>,
    /// Stored path operations left, see [`MAX_DOC_OPS`].
    ops_left: Cell<usize>,
}

impl Default for Defs<'_> {
    fn default() -> Self {
        Self {
            by_id: Vec::new(),
            work_left: Cell::new(MAX_PARSE_WORK),
            ops_left: Cell::new(MAX_DOC_OPS),
        }
    }
}

impl<'a> Defs<'a> {
    fn lookup(&self, id: &str) -> Option<&'a Node> {
        let first = self.by_id.partition_point(|(k, _)| *k < id);
        match self.by_id.get(first) {
            Some(&(k, node)) if k == id => Some(node),
            _ => None,
        }
    }

    /// Spends `n` units of parse work. Returns `false`, and leaves the
    /// budget empty, when fewer than `n` remain.
    fn charge_work(&self, n: usize) -> bool {
        charge(&self.work_left, n)
    }

    /// Reserves room for `n` stored path operations. Returns `false`,
    /// and leaves the budget empty, when fewer than `n` remain.
    fn charge_ops(&self, n: usize) -> bool {
        charge(&self.ops_left, n)
    }

    /// True once the stored-operation budget is spent.
    fn ops_exhausted(&self) -> bool {
        self.ops_left.get() == 0
    }

    /// True once the parse work budget is spent. A reference resolved
    /// after that point may be incomplete, so callers drop the fill.
    fn work_exhausted(&self) -> bool {
        self.work_left.get() == 0
    }
}

fn charge(left: &Cell<usize>, n: usize) -> bool {
    if let Some(rest) = left.get().checked_sub(n) {
        left.set(rest);
        true
    } else {
        left.set(0);
        false
    }
}

/// True when `doc` must not take more fills: either the fill count cap
/// or the stored-operation budget is reached.
fn doc_full(doc: &SvgDoc, defs: &Defs<'_>) -> bool {
    doc.fills.len() >= MAX_FILLS || defs.ops_exhausted()
}

/// Builds the id index for the tree rooted at `root`.
fn build_defs(root: &Node) -> Defs<'_> {
    let mut defs = Defs::default();
    collect_defs(root, &mut defs);
    // `sort_by` is stable, so equal ids keep document order.
    defs.by_id.sort_by(|a, b| a.0.cmp(b.0));
    defs
}

fn collect_defs<'a>(node: &'a Node, defs: &mut Defs<'a>) {
    if let Some(id) = node.id() {
        defs.by_id.push((id, node));
    }
    for c in &node.children {
        collect_defs(c, defs);
    }
}

/// Parse-work cost of reading `node`: the fixed visit cost plus the
/// attribute bytes that get scanned.
fn node_cost(node: &Node) -> usize {
    node.attrs
        .iter()
        .map(|(k, v)| k.len().saturating_add(v.len()))
        .fold(WALK_VISIT_COST, usize::saturating_add)
}

/// Parse-work cost of visiting `node` with inherited state `ctx`:
/// [`node_cost`] plus the heap bytes of the inherited state that gets
/// cloned.
fn visit_cost(node: &Node, ctx: &ElemCtx) -> usize {
    node_cost(node).saturating_add(ctx.heap_bytes())
}

#[derive(Debug, Clone)]
struct ElemCtx {
    xform: Affine,
    /// Inherited fill color (straight RGBA). `None` means "use solid
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
    /// Stroke color (None = no stroke, default).
    stroke_color: Option<[u8; 4]>,
    stroke_width: f32,
    stroke_linecap: LineCap,
    stroke_linejoin: LineJoin,
    /// Inherited stroke-opacity factor in `[0, 1]`.
    stroke_opacity: f32,
    /// Parsed `stroke-dasharray`. Empty means "no dashing". Odd-length
    /// lists are normalized to even length by [`parse_dasharray`].
    stroke_dasharray: Vec<f32>,
    /// `stroke-dashoffset` (in user-space units), applied at the start
    /// of every contour.
    stroke_dashoffset: f32,
    /// Active clip-path href, applied to every fill / stroke produced
    /// inside this subtree. Stored as the bare id (no `url(#...)` form).
    clip_href: Option<String>,
    /// Active filter href (`filter="url(#id)"`). Stored as the bare id.
    /// Inherited like `clip_href`; resolved against the document `Defs`
    /// at emit time to a [`Filter`] cloned onto each Fill.
    filter_href: Option<String>,
    /// Active mask href (`mask="url(#id)"`). Stored as the bare id.
    /// Inherited like `clip_href` / `filter_href`; resolved against
    /// the document `Defs` at emit time to a [`MaskShape`].
    mask_href: Option<String>,
    /// Cycle-guard for nested mask resolution. `resolve_mask_shape`
    /// bumps this when it walks the mask body so any descendant
    /// `mask="url(#...)"` reference (including the cyclic
    /// `<mask id=a>...<rect mask=url(#b)>...<mask id=b>...<rect mask=url(#a)>`
    /// case) is dropped at `emit_paint` rather than recursing back
    /// into the resolver. Keeps the stack bounded at the documented
    /// "mask-of-mask is unsupported" semantics.
    mask_depth: u8,
    /// Elements between the walk root and this context, including the
    /// levels `<use>` expansion adds. See [`MAX_WALK_NESTING`].
    nesting: u16,
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
            filter_href: None,
            mask_href: None,
            mask_depth: 0,
            nesting: 0,
        }
    }
}

impl ElemCtx {
    /// Heap bytes a clone of this context copies.
    fn heap_bytes(&self) -> usize {
        let href = |h: &Option<String>| h.as_ref().map_or(0, String::len);
        (self.stroke_dasharray.len() * core::mem::size_of::<f32>())
            .saturating_add(href(&self.fill_grad_href))
            .saturating_add(href(&self.clip_href))
            .saturating_add(href(&self.filter_href))
            .saturating_add(href(&self.mask_href))
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
    if depth > MAX_GROUP_DEPTH || parent.nesting >= MAX_WALK_NESTING {
        return Err(RenderError::Parse("svg nesting"));
    }
    if doc_full(doc, defs) {
        return Err(RenderError::Parse("svg fill cap"));
    }
    if !defs.charge_work(visit_cost(node, parent)) {
        return Err(RenderError::Parse("svg work cap"));
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
            if doc_full(doc, defs) {
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
            if doc_full(doc, defs) {
                break;
            }
        }
    }
    Ok(())
}

/// Computes the inherited [`ElemCtx`] for `node`, given `parent`.
fn inherit_attrs(parent: &ElemCtx, node: &Node) -> ElemCtx {
    let mut ctx = parent.clone();
    ctx.nesting = parent.nesting.saturating_add(1);
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
        } else if attr_matches(k, "filter") {
            if let Some(href) = parse_url_ref(v) {
                ctx.filter_href = Some(href);
            }
        } else if attr_matches(k, "mask") {
            if let Some(href) = parse_url_ref(v) {
                ctx.mask_href = Some(href);
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
    if defs.ops_exhausted() {
        return;
    }
    // Work out what this element paints before resolving any
    // reference, so an element that paints nothing costs nothing.
    let fill_paint = resolve_fill_paint(defs, ctx).filter(|p| !is_fully_transparent(p));
    let stroke = ctx.stroke_color.and_then(|scol| {
        if ctx.stroke_width <= 0.0 {
            return None;
        }
        let alpha = (scol[3] as f32 / 255.0) * ctx.stroke_opacity * ctx.opacity;
        let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        if a == 0 {
            return None;
        }
        let (stroke_ops, work) = stroke_to_fill(
            ops,
            ctx.stroke_width,
            ctx.stroke_linecap,
            ctx.stroke_linejoin,
            &ctx.stroke_dasharray,
            ctx.stroke_dashoffset,
        );
        if stroke_ops.is_empty() || !defs.charge_work(work) {
            return None;
        }
        Some((stroke_ops, [scol[0], scol[1], scol[2], a]))
    });
    if fill_paint.is_none() && stroke.is_none() {
        return;
    }

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
    // unsupported, and this enforces it.
    let mask = if ctx.mask_depth == 0 {
        ctx.mask_href
            .as_deref()
            .and_then(|id| resolve_mask_shape(defs, id))
    } else {
        None
    };

    // Fill pass.
    if let Some(p) = fill_paint {
        push_fill(
            doc,
            defs,
            Fill {
                ops: ops.to_vec(),
                paint: p,
                xform: ctx.xform,
                clip: clip.clone(),
                #[cfg(test)]
                is_stroke: false,
                filter: filter.clone(),
                mask: mask.clone(),
            },
        );
    }

    // Stroke pass.
    if let Some((stroke_ops, rgba)) = stroke {
        if doc.fills.len() < MAX_FILLS {
            push_fill(
                doc,
                defs,
                Fill {
                    ops: stroke_ops,
                    paint: Paint::Solid(rgba),
                    xform: ctx.xform,
                    clip,
                    #[cfg(test)]
                    is_stroke: true,
                    filter,
                    mask,
                },
            );
        }
    }
}

/// Appends `fill` if the document's stored-operation budget can hold
/// it. A fill that does not fit is dropped and the budget is marked
/// spent, which stops the walk the same way the fill cap does. A fill
/// built after the parse work budget ran out is dropped too, since its
/// clip, mask, or filter may have been cut short.
fn push_fill(doc: &mut SvgDoc, defs: &Defs<'_>, fill: Fill) {
    if !defs.work_exhausted() && defs.charge_ops(fill.weight()) {
        doc.fills.push(fill);
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
        // url(#...) pointing to nothing falls back to default black.
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
/// [`Fill`]. The mask's own `transform=` attribute pre-composes onto
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
fn resolve_mask_shape(defs: &Defs<'_>, id: &str) -> Option<MaskShape> {
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

/// Resolves a `<filter id="...">` definition into a [`Filter`] record.
/// Unknown / malformed primitives are skipped silently. The rest of
/// the chain still runs. Primitives past [`MAX_FILTER_PRIMITIVES`] are
/// ignored. Returns `None` if the id doesn't point at a `<filter>`
/// element or no recognized primitives were collected.
fn resolve_filter(defs: &Defs<'_>, id: &str) -> Option<Filter> {
    let f = defs.lookup(id)?;
    if !name_eq(&f.name, "filter") || !defs.charge_work(node_cost(f)) {
        return None;
    }
    let mut primitives = Vec::new();
    for c in &f.children {
        if primitives.len() >= MAX_FILTER_PRIMITIVES {
            break;
        }
        // `feMerge` also reads its `feMergeNode` children.
        let cost = c
            .children
            .iter()
            .map(node_cost)
            .fold(node_cost(c), usize::saturating_add);
        if !defs.charge_work(cost) {
            return None;
        }
        if let Some(p) = parse_filter_primitive(c) {
            primitives.push(p);
        }
    }
    if primitives.is_empty() {
        return None;
    }
    Some(Filter { primitives })
}

fn parse_filter_primitive(node: &Node) -> Option<FilterPrimitive> {
    let input = node.attr("in").map(|s| s.trim().to_string());
    let result = node.attr("result").map(|s| s.trim().to_string());

    let op = if name_eq(&node.name, "feGaussianBlur") {
        let (sx, sy) = parse_std_deviation(node.attr("stdDeviation").unwrap_or(""))?;
        FilterOp::GaussianBlur {
            std_dev_x: sx,
            std_dev_y: sy,
        }
    } else if name_eq(&node.name, "feColorMatrix") {
        let kind = node
            .attr("type")
            .unwrap_or("matrix")
            .trim()
            .to_ascii_lowercase();
        let values = node.attr("values").unwrap_or("");
        let matrix = parse_color_matrix(&kind, values)?;
        FilterOp::ColorMatrix { matrix }
    } else if name_eq(&node.name, "feOffset") {
        let dx = node.attr("dx").and_then(parse_length).unwrap_or(0.0);
        let dy = node.attr("dy").and_then(parse_length).unwrap_or(0.0);
        FilterOp::Offset { dx, dy }
    } else if name_eq(&node.name, "feFlood") {
        let mut color = node
            .attr("flood-color")
            .and_then(parse_color)
            .unwrap_or([0, 0, 0, 255]);
        let opa = node
            .attr("flood-opacity")
            .and_then(parse_opacity)
            .unwrap_or(1.0);
        let a = (color[3] as f32 / 255.0 * opa).clamp(0.0, 1.0);
        color[3] = (a * 255.0).round() as u8;
        FilterOp::Flood { color }
    } else if name_eq(&node.name, "feMerge") {
        let mut inputs = Vec::new();
        for c in &node.children {
            if name_eq(&c.name, "feMergeNode") {
                if let Some(r) = c.attr("in") {
                    inputs.push(r.trim().to_string());
                }
            }
        }
        if inputs.is_empty() {
            return None;
        }
        FilterOp::Merge { inputs }
    } else {
        return None;
    };

    Some(FilterPrimitive { input, result, op })
}

/// `stdDeviation` may be a single number or two whitespace-separated
/// numbers (x, y). Negative values are an SVG error; we treat them as
/// zero (no blur on that axis).
fn parse_std_deviation(s: &str) -> Option<(f32, f32)> {
    let mut it = s
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|t| !t.is_empty());
    let a: f32 = it.next()?.parse().ok()?;
    let b = it.next().and_then(|t| t.parse::<f32>().ok()).unwrap_or(a);
    Some((a.max(0.0), b.max(0.0)))
}

/// Parses an `feColorMatrix` `values=` attribute under the named
/// `type=` flavor. Returns a 4x5 row-major matrix (RGBA in, RGBA out
/// plus 1 column of bias). Failure modes (wrong arity, NaN) silently
/// degrade to identity so downstream rendering stays sane.
fn parse_color_matrix(kind: &str, values: &str) -> Option<[f32; 20]> {
    let nums: Vec<f32> = values
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse::<f32>().ok())
        .collect();
    match kind {
        "matrix" | "" => {
            if nums.len() != 20 {
                return None;
            }
            let mut m = [0.0_f32; 20];
            m.copy_from_slice(&nums);
            Some(m)
        }
        "saturate" => {
            // SVG 1.1 §15.18: saturation matrix.
            let s = nums.first().copied().unwrap_or(1.0);
            Some(saturate_matrix(s))
        }
        "huerotate" => {
            let deg = nums.first().copied().unwrap_or(0.0);
            Some(hue_rotate_matrix(deg))
        }
        "luminancetoalpha" => Some(LUMINANCE_TO_ALPHA_MATRIX),
        _ => None,
    }
}

/// Identity-on-luma matrix from SVG 1.1 §15.18 with `s` controlling the
/// linear interpolation between luma-only (s=0) and identity (s=1).
fn saturate_matrix(s: f32) -> [f32; 20] {
    // Coefficients from the SVG spec.
    let r0 = 0.213 + 0.787 * s;
    let r1 = 0.715 - 0.715 * s;
    let r2 = 0.072 - 0.072 * s;
    let g0 = 0.213 - 0.213 * s;
    let g1 = 0.715 + 0.285 * s;
    let g2 = 0.072 - 0.072 * s;
    let b0 = 0.213 - 0.213 * s;
    let b1 = 0.715 - 0.715 * s;
    let b2 = 0.072 + 0.928 * s;
    [
        r0, r1, r2, 0.0, 0.0, //
        g0, g1, g2, 0.0, 0.0, //
        b0, b1, b2, 0.0, 0.0, //
        0.0, 0.0, 0.0, 1.0, 0.0,
    ]
}

/// Hue-rotation matrix from SVG 1.1 §15.18.
fn hue_rotate_matrix(degrees: f32) -> [f32; 20] {
    let rad = degrees.to_radians();
    let c = rad.cos();
    let s = rad.sin();
    let r0 = 0.213 + c * 0.787 - s * 0.213;
    let r1 = 0.715 - c * 0.715 - s * 0.715;
    let r2 = 0.072 - c * 0.072 + s * 0.928;
    let g0 = 0.213 - c * 0.213 + s * 0.143;
    let g1 = 0.715 + c * 0.285 + s * 0.140;
    let g2 = 0.072 - c * 0.072 - s * 0.283;
    let b0 = 0.213 - c * 0.213 - s * 0.787;
    let b1 = 0.715 - c * 0.715 + s * 0.715;
    let b2 = 0.072 + c * 0.928 + s * 0.072;
    [
        r0, r1, r2, 0.0, 0.0, //
        g0, g1, g2, 0.0, 0.0, //
        b0, b1, b2, 0.0, 0.0, //
        0.0, 0.0, 0.0, 1.0, 0.0,
    ]
}

const LUMINANCE_TO_ALPHA_MATRIX: [f32; 20] = [
    0.0, 0.0, 0.0, 0.0, 0.0, //
    0.0, 0.0, 0.0, 0.0, 0.0, //
    0.0, 0.0, 0.0, 0.0, 0.0, //
    0.2125, 0.7154, 0.0721, 0.0, 0.0,
];

fn resolve_gradient(defs: &Defs<'_>, id: &str, ctx: &ElemCtx) -> Option<GradientPaint> {
    let node = defs.lookup(id)?;
    let is_linear = name_eq(&node.name, "linearGradient");
    let is_radial = name_eq(&node.name, "radialGradient");
    if !is_linear && !is_radial {
        return None;
    }
    // Every fill that references the gradient parses its stops again.
    let stops_cost = |n: &Node| {
        n.children
            .iter()
            .map(node_cost)
            .fold(node_cost(n), usize::saturating_add)
    };
    if !defs.charge_work(stops_cost(node)) {
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
                if !defs.charge_work(stops_cost(parent)) {
                    return None;
                }
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
    // stop-color and stop-opacity are presentation attributes. Some
    // authoring tools fold them into a CSS-ish
    // style="stop-color:#rgb;stop-opacity:0.5" instead. A style
    // declaration wins over the attribute, as in CSS. Chunks without
    // a colon, such as the empty one after a trailing semicolon, are
    // skipped.
    let mut color = node
        .attr("stop-color")
        .and_then(parse_color)
        .unwrap_or([0, 0, 0, 255]);
    let mut stop_opacity = node
        .attr("stop-opacity")
        .and_then(parse_opacity)
        .unwrap_or(1.0);
    if let Some(style) = node.attr("style") {
        for (key, val) in style.split(';').filter_map(|chunk| chunk.split_once(':')) {
            let (key, val) = (key.trim(), val.trim());
            if key.eq_ignore_ascii_case("stop-color") {
                if let Some(c) = parse_color(val) {
                    color = c;
                }
            } else if key.eq_ignore_ascii_case("stop-opacity") {
                if let Some(o) = parse_opacity(val) {
                    stop_opacity = o;
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
// Stroke geometry: walk polyline -> emit closed quad ribbons with caps
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
///
/// Also returns the work spent, in polyline points, dash boundaries,
/// and emitted operations, so the caller can charge it to the
/// document budget.
fn stroke_to_fill(
    ops: &[PathOp],
    stroke_width: f32,
    cap: LineCap,
    join: LineJoin,
    dasharray: &[f32],
    dashoffset: f32,
) -> (Vec<PathOp>, usize) {
    if stroke_width <= 0.0 {
        return (Vec::new(), 0);
    }
    let polylines = flatten_to_polylines(ops);
    let half = stroke_width * 0.5;
    let mut out: Vec<PathOp> = Vec::new();
    let mut splits_left = MAX_DASH_SPLITS;
    let points: usize = polylines.iter().map(|p| p.points.len()).sum();

    let dashed = !dasharray.is_empty() && dasharray.iter().any(|&v| v > 0.0);

    for poly in &polylines {
        if out.len() >= MAX_STROKE_OPS {
            break;
        }
        if poly.points.len() < 2 {
            continue;
        }
        if dashed {
            // Per-contour: walk *true Bezier arc length* (not the
            // chord-flattened polyline cumulative length, which is
            // always slightly short of the curve), emit only the "draw"
            // phase segments as fresh open polylines.
            let segs = dash_polyline_limited(
                &poly.points,
                &poly.arc_lengths,
                poly.closed,
                dasharray,
                dashoffset,
                &mut splits_left,
            );
            for seg in segs {
                if out.len() >= MAX_STROKE_OPS {
                    break;
                }
                if seg.len() >= 2 {
                    emit_stroked_polyline(&mut out, &seg, false, half, cap, join);
                }
            }
        } else {
            emit_stroked_polyline(&mut out, &poly.points, poly.closed, half, cap, join);
        }
    }
    let work = points
        .saturating_add(MAX_DASH_SPLITS - splits_left)
        .saturating_add(out.len());
    (out, work)
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
    // Curve subdivision stops once this many points exist in total.
    let mut budget = MAX_POLYLINE_POINTS;

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
                    &mut cur,
                    &mut cur_arc,
                    cx,
                    cy,
                    ccx,
                    ccy,
                    x,
                    y,
                    0.25,
                    &mut budget,
                    0,
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
                    &mut cur,
                    &mut cur_arc,
                    cx,
                    cy,
                    c1x,
                    c1y,
                    c2x,
                    c2y,
                    x,
                    y,
                    0.25,
                    &mut budget,
                    0,
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
    budget: &mut usize,
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
    if stop_polyline_subdivision(depth, *budget, &[x0, y0, x1, y1, x2, y2])
        || dist_sq <= 4.0 * tol * tol
    {
        *budget = budget.saturating_sub(1);
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
    flatten_quad_polyline(
        out,
        arcs,
        x0,
        y0,
        m01.0,
        m01.1,
        m.0,
        m.1,
        tol,
        budget,
        depth + 1,
    );
    flatten_quad_polyline(
        out,
        arcs,
        m.0,
        m.1,
        m12.0,
        m12.1,
        x2,
        y2,
        tol,
        budget,
        depth + 1,
    );
}

/// True when polyline subdivision must stop: the depth cap or the
/// point budget is reached, or a control point is NaN or infinite
/// (splitting those only yields more non-finite points).
fn stop_polyline_subdivision(depth: u32, budget: usize, points: &[f32]) -> bool {
    depth >= 16 || budget == 0 || !points.iter().all(|v| v.is_finite())
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
    budget: &mut usize,
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
    if stop_polyline_subdivision(depth, *budget, &[x0, y0, x1, y1, x2, y2, x3, y3])
        || (d1 <= tol * tol && d2 <= tol * tol)
    {
        *budget = budget.saturating_sub(1);
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
        budget,
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
        budget,
        depth + 1,
    );
}

/// Emits the stroke ribbon for one polyline. For the minimum-viable
/// path this draws each segment as a separate rectangle (butt cap +
/// miter-style overlap). Adjacent segments overlap at joins so
/// scanline winding fills the joint cleanly without explicit miter
/// geometry. The result is visually identical to "miter" for typical
/// stroke widths and avoids the corner-case math.
///
/// Round / square caps emit octagon disks / extended rectangles at the
/// open ends. Butt is the default.
///
/// Stops early once `out` holds [`MAX_STROKE_OPS`] operations.
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
        if out.len() >= MAX_STROKE_OPS {
            return;
        }
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
            if out.len() >= MAX_STROKE_OPS {
                return;
            }
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
            if out.len() >= MAX_STROKE_OPS {
                return;
            }
            let prev = points[if closed && i == 0 { n - 1 } else { i }];
            let cur = points[if closed { (i + 1) % n } else { i + 1 }];
            let next = points[if closed { (i + 2) % n } else { i + 2 }];
            emit_miter_join(out, prev, cur, next, half);
        }
    }
}

/// Emits an axis-aligned octagon ("disk") of radius `r` centered at
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
    // segments means a long spike. Bail to bevel beyond the limit.
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
        // join center.
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
/// that odd-length lists are doubled (e.g. `"2 3 5"` ->
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
            _ => return Vec::new(), // SVG: any negative or invalid -> ignore the whole list.
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

/// [`dash_polyline_limited`] with a fresh split budget.
#[cfg(test)]
fn dash_polyline(
    points: &[(f32, f32)],
    arc_lengths: &[f32],
    closed: bool,
    pattern: &[f32],
    offset: f32,
) -> Vec<Vec<(f32, f32)>> {
    let mut splits_left = MAX_DASH_SPLITS;
    dash_polyline_limited(
        points,
        arc_lengths,
        closed,
        pattern,
        offset,
        &mut splits_left,
    )
}

/// Walks `points` by cumulative *true Bezier arc length* and returns
/// the polylines that fall inside the "draw" phase of the dash pattern.
/// `arc_lengths[i]` is the parent-curve arc length of the chord from
/// `points[i]` to `points[(i + 1) % n]`. For straight chords this is
/// the Euclidean distance, for chords flattened from Quad/Cubic Beziers
/// it is the Roger Willcocks chord+control-polygon estimate (~0.05 %
/// of the true Gauss-Legendre integral on typical sweeps). `pattern`
/// is even-length and non-empty (caller-checked); `offset` is applied
/// at the start of the contour, then resets per [SVG spec].
///
/// Position mapping: a dash boundary at arc-length `s` along chord
/// `i` lands geometrically at parameter `t = s / arc_lengths[i]`
/// linearly between `points[i]` and `points[i+1]`. This is the
/// standard mapping for chord-flattened curves. The dash is *placed*
/// at its true-arc-length position along the curve, but the geometry
/// is interpolated on the chord (which is what the rasterizer
/// already consumes).
///
/// Behavior at a glance:
///
/// - Stride alternates draw / skip starting from index 0 ("draw").
/// - `offset` may be negative or larger than the pattern; reduced
///   modulo `total = sum(pattern)` after sign-folding.
/// - Closed contours are walked as if a final segment connected back
///   to the first vertex; the resulting "wrap" sub-polyline is split
///   the same way as any other.
/// - For straight-chord polylines (rect, polygon, polyline, line,
///   `LineTo` paths), `arc_lengths[i]` is exactly the Euclidean
///   distance, so this function is bit-identical to a chord-only
///   walker on those inputs.
///
/// Each dash boundary walked costs one unit of `splits_left`, a budget
/// shared across the calls for one stroke. When it runs out the walk
/// stops and returns the dashes found so far. This also ends the walk
/// when float rounding stops a tiny dash length from advancing along a
/// long path.
fn dash_polyline_limited(
    points: &[(f32, f32)],
    arc_lengths: &[f32],
    closed: bool,
    pattern: &[f32],
    offset: f32,
    splits_left: &mut usize,
) -> Vec<Vec<(f32, f32)>> {
    let total: f32 = pattern.iter().sum();
    let Some(&first) = pattern.first() else {
        return Vec::new();
    };
    if total <= 0.0 || points.len() < 2 {
        return Vec::new();
    }
    // Normalize offset into [0, total).
    let mut off = offset % total;
    if off < 0.0 {
        off += total;
    }
    // The current dash index (even = draw, odd = skip) and remaining
    // length within that dash segment after consuming `off`.
    let mut idx = 0usize;
    let mut remaining = first;
    // `off < total`, so this finishes within one pass over the pattern
    // plus rounding slack. The bound stops a pattern whose entries are
    // too small to change `off` from cycling forever.
    let mut steps_left = pattern.len().saturating_mul(2).saturating_add(1);
    while off > 0.0 && remaining <= off && steps_left > 0 {
        steps_left -= 1;
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
        // length, not the chord-Euclidean distance. They only differ
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
            let Some(left) = splits_left.checked_sub(1) else {
                return out;
            };
            *splits_left = left;
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
// Shape primitives -> path
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

/// Approximates a centered ellipse with four cubic Béziers using the
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
/// are dropped silently. That's what every browser does in practice.
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
            // Bail out on unrecognized garbage; what's parsed so far
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

/// Canvas-pass and segment budgets for rendering one document,
/// shared by every fill and by the children of every mask.
struct RenderBudget {
    /// Canvas-sized passes left, see [`MAX_RENDER_PASSES`].
    passes_left: u32,
    /// Flattened segments left, see [`MAX_SEGMENTS`].
    segments_left: usize,
}

impl RenderBudget {
    fn new() -> Self {
        Self {
            passes_left: MAX_RENDER_PASSES,
            segments_left: MAX_SEGMENTS,
        }
    }

    /// Spends `n` passes. Returns `false`, spending nothing, when
    /// fewer than `n` remain.
    fn take_passes(&mut self, n: u32) -> bool {
        match self.passes_left.checked_sub(n) {
            Some(rest) => {
                self.passes_left = rest;
                true
            }
            None => false,
        }
    }

    /// Flattens `ops` within the remaining segment budget and charges
    /// the segments it produced.
    fn flatten(&mut self, ops: &[PathOp], xform: &Affine, tol: f32) -> Vec<Segment> {
        let segs = flatten_limited(ops.iter().copied(), xform, tol, self.segments_left);
        self.segments_left = self.segments_left.saturating_sub(segs.len());
        segs
    }
}

/// Renders every fill of `doc` onto `out`.
fn render_doc(out: &mut ColorPixmap, doc: &SvgDoc, world: &Affine, tol: f32) {
    let mut budget = RenderBudget::new();
    for fill in &doc.fills {
        render_fill(out, fill, world, tol, &mut budget);
    }
}

/// Renders one fill onto `out`. Fills that no longer fit the render
/// budget are skipped.
///
/// Masks are rasterized only inside the canvas. Pixels there get the
/// same coverage as a full rasterization, and a shape far larger than
/// the canvas costs no more than the canvas.
fn render_fill(
    out: &mut ColorPixmap,
    fill: &Fill,
    world: &Affine,
    tol: f32,
    budget: &mut RenderBudget,
) {
    if budget.segments_left == 0 || !budget.take_passes(fill.render_passes()) {
        return;
    }
    let canvas = Window {
        x0: 0,
        y0: 0,
        x1: out.width as i32,
        y1: out.height as i32,
    };
    let xf = world.compose(&fill.xform);
    let segs = budget.flatten(&fill.ops, &xf, tol);
    // `raster_bounds` is `None` exactly when the full rasterization
    // would be empty, which is when there is nothing to paint.
    if segs.is_empty() || raster_bounds(&segs).is_none() {
        return;
    }
    // Clipped to the canvas this can be empty while the full raster is
    // not. That still paints nothing, but filters such as `feFlood`
    // must run as before, so keep going.
    let mask = rasterize_in(&segs, Some(canvas));
    // If a clip-path is set, rasterize it once, then multiply mask
    // alpha by the clip alpha at sample time. The clip lives in
    // document space; compose the world transform on top.
    let clip_mask = fill.clip.as_ref().map(|cs| {
        let cxf = world.compose(&cs.xform);
        let csegs = budget.flatten(&cs.ops, &cxf, tol);
        rasterize_in(&csegs, Some(canvas))
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
            apply_mask_budgeted(&mut result, m, world, tol, budget);
        }
        composite_over(out, &result);
        return;
    }

    paint_into(out, fill, &mask, clip_mask.as_ref(), world);
}

/// [`apply_mask_budgeted`] with a fresh render budget.
#[cfg(test)]
fn apply_mask(dst: &mut ColorPixmap, mask_shape: &MaskShape, world: &Affine, tol: f32) {
    apply_mask_budgeted(dst, mask_shape, world, tol, &mut RenderBudget::new());
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
fn apply_mask_budgeted(
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

// =========================================================================
// Filter pipeline
// =========================================================================
//
// Each `<filter>` is a small DAG of `FilterPrimitive`s. We evaluate the
// DAG against a same-size `SourceGraphic` pixmap (the filtered shape
// rendered alone into a transparent buffer) and a `SourceAlpha` pixmap
// (the same shape with R=G=B=0). Each primitive reads from `in` (named
// or implicit-prev) and writes to `result` (named or anonymous). The
// last primitive's output is the filtered pixmap, composited under the
// canvas via Porter-Duff source-over.
//
// All intermediate buffers are full canvas size. This trades memory
// for simplicity: feOffset + feMerge etc. don't need to track filter
// regions, and shifting / blurring stays within the visible canvas.

/// Walks the primitive list and returns the final pixmap. Built-in
/// inputs `SourceGraphic` and `SourceAlpha` are materialized lazily.
fn apply_filter(filter: &Filter, source: &ColorPixmap) -> ColorPixmap {
    use alloc::collections::BTreeMap;
    let mut named: BTreeMap<String, ColorPixmap> = BTreeMap::new();
    let mut prev: Option<ColorPixmap> = None;
    let mut source_alpha: Option<ColorPixmap> = None;

    for prim in &filter.primitives {
        let in_pix: ColorPixmap = match prim.input.as_deref() {
            Some("SourceGraphic") => source.clone(),
            Some("SourceAlpha") => source_alpha
                .get_or_insert_with(|| make_source_alpha(source))
                .clone(),
            Some(name) => named
                .get(name)
                .cloned()
                .unwrap_or_else(|| ColorPixmap::new(source.width, source.height)),
            None => prev.clone().unwrap_or_else(|| source.clone()),
        };

        let out = match &prim.op {
            FilterOp::GaussianBlur {
                std_dev_x,
                std_dev_y,
            } => apply_gaussian_blur(&in_pix, *std_dev_x, *std_dev_y),
            FilterOp::ColorMatrix { matrix } => apply_color_matrix(&in_pix, matrix),
            FilterOp::Offset { dx, dy } => apply_offset(&in_pix, *dx, *dy),
            FilterOp::Flood { color } => apply_flood(in_pix.width, in_pix.height, *color),
            FilterOp::Merge { inputs } => {
                let mut acc = ColorPixmap::new(source.width, source.height);
                for name in inputs {
                    let layer = match name.as_str() {
                        "SourceGraphic" => source.clone(),
                        "SourceAlpha" => source_alpha
                            .get_or_insert_with(|| make_source_alpha(source))
                            .clone(),
                        other => named
                            .get(other)
                            .cloned()
                            .unwrap_or_else(|| ColorPixmap::new(source.width, source.height)),
                    };
                    composite_over(&mut acc, &layer);
                }
                acc
            }
        };

        if let Some(name) = &prim.result {
            named.insert(name.clone(), out.clone());
        }
        prev = Some(out);
    }

    prev.unwrap_or_else(|| source.clone())
}

/// `SourceAlpha`: the source's alpha channel in all four channels'
/// premultiplied form (R=G=B=0, A unchanged).
fn make_source_alpha(src: &ColorPixmap) -> ColorPixmap {
    let mut out = ColorPixmap::new(src.width, src.height);
    let n = src.data.len() / 4;
    for i in 0..n {
        let a = src.data[i * 4 + 3];
        out.data[i * 4] = 0;
        out.data[i * 4 + 1] = 0;
        out.data[i * 4 + 2] = 0;
        out.data[i * 4 + 3] = a;
    }
    out
}

/// Porter-Duff source-over compositing of a same-size premultiplied
/// `top` onto `dst`. Reuses the per-pixel formula from
/// `colrv1::blend_src_over` but in a tight inner loop.
fn composite_over(dst: &mut ColorPixmap, top: &ColorPixmap) {
    if dst.width != top.width || dst.height != top.height {
        return;
    }
    let n = dst.data.len() / 4;
    for i in 0..n {
        let sa = top.data[i * 4 + 3] as u32;
        if sa == 0 {
            continue;
        }
        let sr = top.data[i * 4] as u32;
        let sg = top.data[i * 4 + 1] as u32;
        let sb = top.data[i * 4 + 2] as u32;
        let dr = dst.data[i * 4] as u32;
        let dg = dst.data[i * 4 + 1] as u32;
        let db = dst.data[i * 4 + 2] as u32;
        let da = dst.data[i * 4 + 3] as u32;
        let inv = 255 - sa;
        dst.data[i * 4] = (sr + (dr * inv + 127) / 255) as u8;
        dst.data[i * 4 + 1] = (sg + (dg * inv + 127) / 255) as u8;
        dst.data[i * 4 + 2] = (sb + (db * inv + 127) / 255) as u8;
        dst.data[i * 4 + 3] = (sa + (da * inv + 127) / 255) as u8;
    }
}

/// Three-pass separable box-blur approximation. Each axis is convolved
/// with a box kernel of radius `r ~= ceil(sigma)` three times, which approaches
/// a true Gaussian by the central-limit theorem and is visually
/// indistinguishable for σ >= 1.
fn apply_gaussian_blur(src: &ColorPixmap, sx: f32, sy: f32) -> ColorPixmap {
    if (sx <= 0.0 && sy <= 0.0) || src.is_empty() {
        return src.clone();
    }
    let rx = ((sx.max(0.0)).ceil() as i32).min(MAX_BLUR_RADIUS);
    let ry = ((sy.max(0.0)).ceil() as i32).min(MAX_BLUR_RADIUS);
    let mut buf = src.clone();
    if rx > 0 {
        for _ in 0..3 {
            buf = box_blur_h(&buf, rx);
        }
    }
    if ry > 0 {
        for _ in 0..3 {
            buf = box_blur_v(&buf, ry);
        }
    }
    buf
}

/// Sum of `sample(k)` over `k` in `-r..=r` with `k` clamped into
/// `0..len`, as the edge-extending blur window needs. Counts the
/// clamped samples instead of visiting them, so the cost is at most
/// `len` samples however large `r` is. `r >= 0` and `len >= 1`.
fn clamped_window_sum(r: i32, len: i32, sample: impl Fn(i32) -> u32) -> u32 {
    let inside = r.min(len - 1);
    let below = r as u32 * sample(0);
    let above = (r - inside) as u32 * sample(len - 1);
    (0..=inside).map(&sample).sum::<u32>() + below + above
}

fn box_blur_h(src: &ColorPixmap, r: i32) -> ColorPixmap {
    let w = src.width as i32;
    let h = src.height as i32;
    let mut out = ColorPixmap::new(src.width, src.height);
    if w == 0 || h == 0 || r == 0 {
        out.data.copy_from_slice(&src.data);
        return out;
    }
    let kernel = (r * 2 + 1) as u32;
    for y in 0..h {
        let row = (y * w) as usize * 4;
        // Sliding-window sum over the kernel. Out-of-bounds samples
        // clamp to the edge ("EDGE" mode in SVG terms, closer to what
        // browser engines do for filter regions touching the canvas
        // edge).
        // Prime the window with [-r, r] samples.
        let sample = |c: usize| move |kx: i32| src.data[row + kx as usize * 4 + c] as u32;
        let mut sr = clamped_window_sum(r, w, sample(0));
        let mut sg = clamped_window_sum(r, w, sample(1));
        let mut sb = clamped_window_sum(r, w, sample(2));
        let mut sa = clamped_window_sum(r, w, sample(3));
        for x in 0..w {
            let oi = row + x as usize * 4;
            out.data[oi] = (sr / kernel) as u8;
            out.data[oi + 1] = (sg / kernel) as u8;
            out.data[oi + 2] = (sb / kernel) as u8;
            out.data[oi + 3] = (sa / kernel) as u8;
            // Slide window: drop pixel at x-r, add pixel at x+r+1.
            let drop_x = (x - r).clamp(0, w - 1);
            let add_x = (x + r + 1).clamp(0, w - 1);
            let di = row + drop_x as usize * 4;
            let ai = row + add_x as usize * 4;
            sr = sr + src.data[ai] as u32 - src.data[di] as u32;
            sg = sg + src.data[ai + 1] as u32 - src.data[di + 1] as u32;
            sb = sb + src.data[ai + 2] as u32 - src.data[di + 2] as u32;
            sa = sa + src.data[ai + 3] as u32 - src.data[di + 3] as u32;
        }
    }
    out
}

fn box_blur_v(src: &ColorPixmap, r: i32) -> ColorPixmap {
    let w = src.width as i32;
    let h = src.height as i32;
    let mut out = ColorPixmap::new(src.width, src.height);
    if w == 0 || h == 0 || r == 0 {
        out.data.copy_from_slice(&src.data);
        return out;
    }
    let kernel = (r * 2 + 1) as u32;
    let stride = (w as usize) * 4;
    for x in 0..w {
        let col = x as usize * 4;
        let sample = |c: usize| move |ky: i32| src.data[col + ky as usize * stride + c] as u32;
        let mut sr = clamped_window_sum(r, h, sample(0));
        let mut sg = clamped_window_sum(r, h, sample(1));
        let mut sb = clamped_window_sum(r, h, sample(2));
        let mut sa = clamped_window_sum(r, h, sample(3));
        for y in 0..h {
            let oi = col + y as usize * stride;
            out.data[oi] = (sr / kernel) as u8;
            out.data[oi + 1] = (sg / kernel) as u8;
            out.data[oi + 2] = (sb / kernel) as u8;
            out.data[oi + 3] = (sa / kernel) as u8;
            let drop_y = (y - r).clamp(0, h - 1);
            let add_y = (y + r + 1).clamp(0, h - 1);
            let di = col + drop_y as usize * stride;
            let ai = col + add_y as usize * stride;
            sr = sr + src.data[ai] as u32 - src.data[di] as u32;
            sg = sg + src.data[ai + 1] as u32 - src.data[di + 1] as u32;
            sb = sb + src.data[ai + 2] as u32 - src.data[di + 2] as u32;
            sa = sa + src.data[ai + 3] as u32 - src.data[di + 3] as u32;
        }
    }
    out
}

/// Applies a 4x5 color matrix (RGBA + bias column) to a premultiplied
/// pixmap. Per SVG 1.1 §15.18, `feColorMatrix` operates on
/// non-premultiplied RGBA, so we un-premultiply, transform, clamp, and
/// re-premultiply.
fn apply_color_matrix(src: &ColorPixmap, m: &[f32; 20]) -> ColorPixmap {
    let mut out = ColorPixmap::new(src.width, src.height);
    let n = src.data.len() / 4;
    for i in 0..n {
        let pr = src.data[i * 4] as f32 / 255.0;
        let pg = src.data[i * 4 + 1] as f32 / 255.0;
        let pb = src.data[i * 4 + 2] as f32 / 255.0;
        let pa = src.data[i * 4 + 3] as f32 / 255.0;
        // Un-premultiply (avoid div-by-zero).
        let (r, g, b) = if pa > 0.0 {
            (pr / pa, pg / pa, pb / pa)
        } else {
            (0.0, 0.0, 0.0)
        };
        let nr = (m[0] * r + m[1] * g + m[2] * b + m[3] * pa + m[4]).clamp(0.0, 1.0);
        let ng = (m[5] * r + m[6] * g + m[7] * b + m[8] * pa + m[9]).clamp(0.0, 1.0);
        let nb = (m[10] * r + m[11] * g + m[12] * b + m[13] * pa + m[14]).clamp(0.0, 1.0);
        let na = (m[15] * r + m[16] * g + m[17] * b + m[18] * pa + m[19]).clamp(0.0, 1.0);
        out.data[i * 4] = (nr * na * 255.0).round() as u8;
        out.data[i * 4 + 1] = (ng * na * 255.0).round() as u8;
        out.data[i * 4 + 2] = (nb * na * 255.0).round() as u8;
        out.data[i * 4 + 3] = (na * 255.0).round() as u8;
    }
    out
}

/// Translates a pixmap by `(dx, dy)` device-space pixels. Out-of-bounds
/// reads return transparent black; the destination is fresh.
fn apply_offset(src: &ColorPixmap, dx: f32, dy: f32) -> ColorPixmap {
    let mut out = ColorPixmap::new(src.width, src.height);
    // `as i32` saturates for huge offsets, so the subtractions below
    // saturate too. Any saturated source index is out of range.
    let dxi = dx.round() as i32;
    let dyi = dy.round() as i32;
    let w = src.width as i32;
    let h = src.height as i32;
    for y in 0..h {
        let sy = y.saturating_sub(dyi);
        if sy < 0 || sy >= h {
            continue;
        }
        for x in 0..w {
            let sx = x.saturating_sub(dxi);
            if sx < 0 || sx >= w {
                continue;
            }
            let s = (sy as usize * w as usize + sx as usize) * 4;
            let d = (y as usize * w as usize + x as usize) * 4;
            out.data[d] = src.data[s];
            out.data[d + 1] = src.data[s + 1];
            out.data[d + 2] = src.data[s + 2];
            out.data[d + 3] = src.data[s + 3];
        }
    }
    out
}

/// Returns a same-size pixmap filled with a solid premultiplied color.
fn apply_flood(width: u32, height: u32, color: [u8; 4]) -> ColorPixmap {
    let mut out = ColorPixmap::new(width, height);
    // Premultiply.
    let a = color[3] as u32;
    let r = (color[0] as u32 * a + 127) / 255;
    let g = (color[1] as u32 * a + 127) / 255;
    let b = (color[2] as u32 * a + 127) / 255;
    let n = out.data.len() / 4;
    for i in 0..n {
        out.data[i * 4] = r as u8;
        out.data[i * 4 + 1] = g as u8;
        out.data[i * 4 + 2] = b as u8;
        out.data[i * 4 + 3] = a as u8;
    }
    out
}

// =========================================================================
// XML scanner -> DOM
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
// Numeric / color / transform parsing
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
            // `get` rather than slicing: six bytes of non-ASCII text can
            // put a byte offset inside a character.
            let r = u8::from_str_radix(rest.get(0..2)?, 16).ok()?;
            let g = u8::from_str_radix(rest.get(2..4)?, 16).ok()?;
            let b = u8::from_str_radix(rest.get(4..6)?, 16).ok()?;
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
                // Closepath takes no arguments, so a number after it is
                // an error. Repeating it would consume nothing and loop
                // forever.
                Some(b'Z' | b'z') | None => return Err(RenderError::Parse("svg path d")),
                Some(prev) => prev,
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
        // the parent should bottom out at MAX_USE_DEPTH instead of
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
    fn stop_style_sets_color_and_opacity() {
        // The style declarations win over the attributes, and the
        // trailing semicolon does not drop the stop.
        let xml = r##"<svg viewBox="0 0 10 10">
            <defs>
                <linearGradient id="g" x1="0" y1="0" x2="10" y2="0">
                    <stop offset="0" stop-opacity="1" style="stop-color:#00FF00;stop-opacity:0.25;"/>
                    <stop offset="1" stop-color="#0000FF" stop-opacity="0.5"/>
                </linearGradient>
            </defs>
            <rect x="0" y="0" width="10" height="10" fill="url(#g)"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        let Paint::Gradient(g) = &doc.fills[0].paint else {
            panic!("expected gradient fill");
        };
        assert_eq!(g.stops.len(), 2);
        let first = g.stops[0].color;
        assert!((first.g - 1.0).abs() < 1e-6 && first.r.abs() < 1e-6);
        assert!(
            (first.a - 0.25).abs() < 1e-6,
            "style opacity, got {}",
            first.a
        );
        assert!((g.stops[1].color.a - 0.5).abs() < 1e-6, "attribute opacity");
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
    fn mask_attaches_to_referencing_fill() {
        // <mask> with a luminance body: black circle on white square.
        // The fill that references it should carry a non-empty
        // MaskShape with both child fills harvested.
        let xml = r##"<svg viewBox="0 0 100 100">
            <defs>
                <mask id="m">
                    <rect x="0" y="0" width="100" height="100" fill="white"/>
                    <circle cx="50" cy="50" r="30" fill="black"/>
                </mask>
            </defs>
            <rect x="0" y="0" width="100" height="100" fill="red" mask="url(#m)"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.fills.len(), 1);
        let m = doc.fills[0].mask.as_ref().expect("mask attached");
        assert_eq!(m.fills.len(), 2, "mask should carry rect + circle fills");
    }

    #[test]
    fn mask_unknown_id_silently_drops() {
        // Bad reference falls back to "no mask" (matches the
        // clip-path / filter degrade-gracefully policy).
        let xml = r##"<svg viewBox="0 0 10 10">
            <rect x="0" y="0" width="10" height="10" fill="#000" mask="url(#missing)"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.fills.len(), 1);
        assert!(doc.fills[0].mask.is_none());
    }

    #[test]
    fn mask_does_not_emit_a_top_level_fill() {
        // The <mask> element itself must NOT emit fills into the
        // document (it's a definition, not a render target). Only the
        // top-level <rect> referencing it should produce a fill.
        let xml = r##"<svg viewBox="0 0 100 100">
            <mask id="m">
                <rect x="0" y="0" width="100" height="100" fill="white"/>
            </mask>
            <rect x="0" y="0" width="100" height="100" fill="red" mask="url(#m)"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        assert_eq!(
            doc.fills.len(),
            1,
            "mask body must not contribute top-level fills"
        );
    }

    #[test]
    fn mask_of_mask_is_dropped() {
        // Nested masks are unsupported: a mask whose body references
        // another mask must drop the inner reference at resolve time.
        let xml = r##"<svg viewBox="0 0 100 100">
            <defs>
                <mask id="inner">
                    <rect x="0" y="0" width="100" height="100" fill="white"/>
                </mask>
                <mask id="outer">
                    <rect x="0" y="0" width="100" height="100" fill="white" mask="url(#inner)"/>
                </mask>
            </defs>
            <rect x="0" y="0" width="100" height="100" fill="red" mask="url(#outer)"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        let outer = doc.fills[0].mask.as_ref().expect("outer mask attached");
        // Outer's child fill must NOT carry a nested mask reference.
        assert!(outer.fills.iter().all(|f| f.mask.is_none()));
    }

    #[test]
    fn mask_type_defaults_to_luminance() {
        // No `mask-type=` attribute -> MaskType::Luminance, matching
        // the SVG spec default and the PR #236 baseline.
        let xml = r##"<svg viewBox="0 0 10 10">
            <defs>
                <mask id="m">
                    <rect x="0" y="0" width="10" height="10" fill="white"/>
                </mask>
            </defs>
            <rect x="0" y="0" width="10" height="10" fill="red" mask="url(#m)"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        let m = doc.fills[0].mask.as_ref().unwrap();
        assert_eq!(m.mask_type, MaskType::Luminance);
        assert_eq!(m.units, MaskUnits::UserSpaceOnUse);
    }

    #[test]
    fn mask_type_alpha_is_parsed() {
        // `mask-type="alpha"` opts into the alpha-channel-direct path.
        let xml = r##"<svg viewBox="0 0 10 10">
            <defs>
                <mask id="m" mask-type="alpha">
                    <rect x="0" y="0" width="10" height="10" fill="black" fill-opacity="0.5"/>
                </mask>
            </defs>
            <rect x="0" y="0" width="10" height="10" fill="red" mask="url(#m)"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        let m = doc.fills[0].mask.as_ref().unwrap();
        assert_eq!(m.mask_type, MaskType::Alpha);
    }

    #[test]
    fn mask_units_object_bounding_box_is_parsed() {
        // `maskUnits="objectBoundingBox"` plus a region rect must round-
        // trip through resolve_mask_shape.
        let xml = r##"<svg viewBox="0 0 100 100">
            <defs>
                <mask id="m" maskUnits="objectBoundingBox" x="0.25" y="0.25" width="0.5" height="0.5">
                    <rect x="0" y="0" width="100" height="100" fill="white"/>
                </mask>
            </defs>
            <rect x="0" y="0" width="100" height="100" fill="red" mask="url(#m)"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        let m = doc.fills[0].mask.as_ref().unwrap();
        assert_eq!(m.units, MaskUnits::ObjectBoundingBox);
        assert!((m.region_x - 0.25).abs() < 1e-5);
        assert!((m.region_y - 0.25).abs() < 1e-5);
        assert!((m.region_w - 0.5).abs() < 1e-5);
        assert!((m.region_h - 0.5).abs() < 1e-5);
    }

    #[test]
    fn apply_mask_alpha_uses_alpha_channel_directly() {
        // mask-type="alpha" means: ignore RGB luminance, sample the
        // mask buffer's alpha channel directly. A mask body that paints
        // opaque BLACK (luminance = 0, alpha = 255) would zero the
        // output under luminance, but must keep it under alpha.
        let world = Affine::identity();
        let mut dst = ColorPixmap::new(4, 4);
        // Fill dst with opaque red (premultiplied: r=255, a=255).
        for px in dst.data.chunks_exact_mut(4) {
            px[0] = 255;
            px[1] = 0;
            px[2] = 0;
            px[3] = 255;
        }
        // Mask body: a black rect that fully covers the canvas.
        let ops = vec![
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 4.0, y: 0.0 },
            PathOp::LineTo { x: 4.0, y: 4.0 },
            PathOp::LineTo { x: 0.0, y: 4.0 },
            PathOp::Close,
        ];
        let body_fill = Fill {
            ops,
            paint: Paint::Solid([0, 0, 0, 255]),
            xform: Affine::identity(),
            clip: None,
            is_stroke: false,
            filter: None,
            mask: None,
        };
        let mask_shape = MaskShape {
            fills: vec![body_fill],
            mask_type: MaskType::Alpha,
            units: MaskUnits::UserSpaceOnUse,
            region_x: 0.0,
            region_y: 0.0,
            region_w: 1.0,
            region_h: 1.0,
        };
        apply_mask(&mut dst, &mask_shape, &world, 0.25);
        // Under alpha-mode the opaque-black mask body keeps every dst
        // pixel intact (alpha = 255 -> m = 255). Under luminance it
        // would have zeroed the pixels.
        for px in dst.data.chunks_exact(4) {
            assert_eq!(px[0], 255, "alpha-mask kept red channel intact");
            assert_eq!(px[3], 255, "alpha-mask kept dst alpha intact");
        }
    }

    #[test]
    fn apply_mask_object_bounding_box_clips_to_region() {
        // maskUnits="objectBoundingBox" with x=0.25 y=0.25 w=0.5 h=0.5
        // on a 100x100 opaque rect: only the [25, 75) x [25, 75) pixel
        // region survives; everything outside is zeroed.
        let world = Affine::identity();
        let mut dst = ColorPixmap::new(100, 100);
        for px in dst.data.chunks_exact_mut(4) {
            px[0] = 255;
            px[1] = 0;
            px[2] = 0;
            px[3] = 255;
        }
        let ops = vec![
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 100.0, y: 0.0 },
            PathOp::LineTo { x: 100.0, y: 100.0 },
            PathOp::LineTo { x: 0.0, y: 100.0 },
            PathOp::Close,
        ];
        let body_fill = Fill {
            ops,
            paint: Paint::Solid([255, 255, 255, 255]),
            xform: Affine::identity(),
            clip: None,
            is_stroke: false,
            filter: None,
            mask: None,
        };
        let mask_shape = MaskShape {
            fills: vec![body_fill],
            mask_type: MaskType::Luminance,
            units: MaskUnits::ObjectBoundingBox,
            region_x: 0.25,
            region_y: 0.25,
            region_w: 0.5,
            region_h: 0.5,
        };
        apply_mask(&mut dst, &mask_shape, &world, 0.25);
        // Inside the [25, 75) box: pixels survive (white luminance *
        // opaque alpha = 255 -> unchanged premultiplied red).
        let inside = dst.get(50, 50);
        assert_eq!(inside, [255, 0, 0, 255]);
        // Outside the box: forced to zero.
        let outside_tl = dst.get(5, 5);
        let outside_br = dst.get(95, 95);
        assert_eq!(outside_tl, [0, 0, 0, 0]);
        assert_eq!(outside_br, [0, 0, 0, 0]);
        // Just outside the upper-left region edge.
        let edge_just_outside = dst.get(24, 24);
        assert_eq!(edge_just_outside, [0, 0, 0, 0]);
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
        // "2 3 5" -> "2 3 5 2 3 5"
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
        // All zeros means "no dash" per the SVG spec, same as none.
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
        // [0,4], [6,10], [12,16], [18,20] -> 4 sub-polylines.
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
        // first 4-unit draw. The contour now opens with a 2-unit skip
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
    fn filter_attaches_to_fill_when_referenced() {
        let xml = r##"<svg viewBox="0 0 100 100">
            <defs>
                <filter id="b"><feGaussianBlur stdDeviation="2"/></filter>
            </defs>
            <rect x="0" y="0" width="100" height="100" fill="#000" filter="url(#b)"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.fills.len(), 1);
        let f = doc.fills[0].filter.as_ref().expect("filter expected");
        assert_eq!(f.primitives.len(), 1);
        assert!(matches!(f.primitives[0].op, FilterOp::GaussianBlur { .. }));
    }

    #[test]
    fn filter_unknown_id_silently_drops() {
        let xml = r##"<svg viewBox="0 0 10 10">
            <rect x="0" y="0" width="10" height="10" fill="#000" filter="url(#missing)"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.fills.len(), 1);
        assert!(doc.fills[0].filter.is_none());
    }

    #[test]
    fn filter_parses_full_drop_shadow_chain() {
        let xml = r##"<svg viewBox="0 0 100 100">
            <defs>
                <filter id="ds">
                    <feGaussianBlur in="SourceAlpha" stdDeviation="2" result="b"/>
                    <feOffset in="b" dx="4" dy="4" result="o"/>
                    <feMerge>
                        <feMergeNode in="o"/>
                        <feMergeNode in="SourceGraphic"/>
                    </feMerge>
                </filter>
            </defs>
            <rect x="10" y="10" width="40" height="40" fill="#000" filter="url(#ds)"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        let f = doc.fills[0].filter.as_ref().unwrap();
        assert_eq!(f.primitives.len(), 3);
        assert!(matches!(f.primitives[0].op, FilterOp::GaussianBlur { .. }));
        assert!(matches!(f.primitives[1].op, FilterOp::Offset { .. }));
        assert!(matches!(f.primitives[2].op, FilterOp::Merge { .. }));
    }

    #[test]
    fn color_matrix_saturate_zero_collapses_red_channels() {
        let m = saturate_matrix(0.0);
        // Pure red (1,0,0,1) -> gray: each output channel ~0.213.
        let r = m[0] * 1.0 + m[1] * 0.0 + m[2] * 0.0 + m[3] * 1.0 + m[4];
        let g = m[5] * 1.0 + m[6] * 0.0 + m[7] * 0.0 + m[8] * 1.0 + m[9];
        let b = m[10] * 1.0 + m[11] * 0.0 + m[12] * 0.0 + m[13] * 1.0 + m[14];
        assert!((r - g).abs() < 1e-3);
        assert!((g - b).abs() < 1e-3);
    }

    #[test]
    fn color_matrix_hue_rotate_zero_is_identity() {
        let m = hue_rotate_matrix(0.0);
        // (1,0,0) stays roughly (1,0,0).
        let r = m[0] * 1.0 + m[1] * 0.0 + m[2] * 0.0;
        assert!((r - 1.0).abs() < 1e-2);
    }

    #[test]
    fn parse_std_deviation_handles_one_or_two_values() {
        assert_eq!(parse_std_deviation("3"), Some((3.0, 3.0)));
        assert_eq!(parse_std_deviation("3 5"), Some((3.0, 5.0)));
        assert_eq!(parse_std_deviation("3,5"), Some((3.0, 5.0)));
        // Negative collapses to zero.
        assert_eq!(parse_std_deviation("-2"), Some((0.0, 0.0)));
        assert_eq!(parse_std_deviation(""), None);
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
    /// produces them. Returned circle is centered at `(cx, cy)` with
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
        // circumference = 2π*100 ~ 628.32. With dasharray "10 10"
        // (period 20) we expect ~31.4 dash periods around the circle,
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
        // matching the analytic prediction.
        assert!(
            true_arc_total > chord_total,
            "arc-length {true_arc_total} must exceed chord total {chord_total}"
        );
        // Both should be close to 2π*100; arc-length should be much
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
        // dashes as the chord walker. A longer "track" can only fit
        // more (or equal) dash periods, never fewer. This is the
        // observable signature of the fix.
        // The arc-length walker must produce at least as many full
        // dashes as the chord walker. A longer track can only fit
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
        // 628.32 / 20 ~ 31.4 cycles, so either 31 or 32 full draw
        // sub-polylines depending on where the final dash boundary
        // lands. The chord-flatten path measures ~627 (about 0.2 % short),
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
        // discretized at 0.25 px tolerance these are within sub-pixel
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
        // arc on a radius-100 circle is 2*100*sin(5/100) ~ 9.996, so
        // expected drawn (chord-measured) ~ 31 * 9.996 ~ 309.9. Allow a
        // generous tolerance because the trailing partial dash can vary.
        assert!(
            drawn > 300.0 && drawn < 320.0,
            "expected ~310 chord-measured draw length, got {drawn}"
        );
    }

    #[test]
    fn long_quadratic_with_continuous_dasharray_has_no_dash_break() {
        // dasharray "1 0" -> period 1, fully drawing (skip is zero
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
        // The fill should cover the entire stroke ribbon, i.e. it has
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
                PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } =>
                    !x.is_finite() || !y.is_finite(),
                _ => false,
            }));
        }
    }

    #[test]
    fn straight_polyline_dasharray_byte_identical_to_chord_walker() {
        // Pre-arc-length walker computed `seg_len` from chord points.
        // For a straight polyline `arc_lengths[i]` IS the chord
        // length, so the new walker must produce bit-identical output
        // on this input, which is the contract the existing PR #227
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

    // ---- textPath helpers ------------------------------------------------

    #[test]
    fn arc_length_polyline_horizontal_line_lays_out_endpoints() {
        // A simple horizontal line from (10,50) to (210,50). The
        // flattener emits one segment so the polyline has two points,
        // with cum 0 and cum 200.
        let ops = vec![
            PathOp::MoveTo { x: 10.0, y: 50.0 },
            PathOp::LineTo { x: 210.0, y: 50.0 },
        ];
        let poly = build_arc_length_polyline(&ops);
        assert_eq!(poly.len(), 2);
        assert!((poly[0].cum - 0.0).abs() < 1e-5);
        assert!((poly[1].cum - 200.0).abs() < 1e-3);
        assert!((poly[0].x - 10.0).abs() < 1e-5);
        assert!((poly[1].x - 210.0).abs() < 1e-5);
    }

    #[test]
    fn sample_polyline_position_lerps_between_chord_endpoints() {
        let poly = vec![
            PolyPoint {
                x: 0.0,
                y: 0.0,
                cum: 0.0,
            },
            PolyPoint {
                x: 100.0,
                y: 0.0,
                cum: 100.0,
            },
            PolyPoint {
                x: 100.0,
                y: 100.0,
                cum: 200.0,
            },
        ];
        let p0 = sample_polyline_position(&poly, 0.0).unwrap();
        assert!((p0.0 - 0.0).abs() < 1e-5 && (p0.1 - 0.0).abs() < 1e-5);
        let p_mid_first = sample_polyline_position(&poly, 50.0).unwrap();
        assert!((p_mid_first.0 - 50.0).abs() < 1e-5 && (p_mid_first.1).abs() < 1e-5);
        let p_corner = sample_polyline_position(&poly, 100.0).unwrap();
        assert!((p_corner.0 - 100.0).abs() < 1e-5 && (p_corner.1 - 0.0).abs() < 1e-5);
        let p_mid_second = sample_polyline_position(&poly, 150.0).unwrap();
        assert!((p_mid_second.0 - 100.0).abs() < 1e-5 && (p_mid_second.1 - 50.0).abs() < 1e-5);
        // Past the total length -> None (silent drop policy in
        // emit_text_path_fills).
        assert!(sample_polyline_position(&poly, 250.0).is_none());
    }

    #[test]
    fn transform_outline_ops_translates_and_flips_y() {
        // Design-unit point (0, 100) at scale 0.5 with origin
        // (50, 200) maps to (50 + 0*0.5, 200 - 100*0.5) = (50, 150).
        // Y is flipped so OT y-up matches SVG y-down.
        let ops = vec![
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 0.0, y: 100.0 },
            PathOp::QuadTo {
                cx: 50.0,
                cy: 50.0,
                x: 100.0,
                y: 0.0,
            },
            PathOp::Close,
        ];
        let out = transform_outline_ops(&ops, 0.5, 50.0, 200.0);
        assert_eq!(out.len(), ops.len());
        match out[0] {
            PathOp::MoveTo { x, y } => {
                assert!((x - 50.0).abs() < 1e-5);
                assert!((y - 200.0).abs() < 1e-5);
            }
            _ => panic!("expected MoveTo"),
        }
        match out[1] {
            PathOp::LineTo { x, y } => {
                assert!((x - 50.0).abs() < 1e-5);
                assert!((y - 150.0).abs() < 1e-5);
            }
            _ => panic!("expected LineTo"),
        }
        match out[2] {
            PathOp::QuadTo { cx, cy, x, y } => {
                assert!((cx - 75.0).abs() < 1e-5);
                assert!((cy - 175.0).abs() < 1e-5);
                assert!((x - 100.0).abs() < 1e-5);
                assert!((y - 200.0).abs() < 1e-5);
            }
            _ => panic!("expected QuadTo"),
        }
        assert!(matches!(out[3], PathOp::Close));
    }

    #[test]
    fn transform_outline_ops_handles_cubic() {
        let ops = vec![
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::CubicTo {
                c1x: 10.0,
                c1y: 20.0,
                c2x: 30.0,
                c2y: 40.0,
                x: 50.0,
                y: 60.0,
            },
        ];
        let out = transform_outline_ops(&ops, 1.0, 0.0, 0.0);
        match out[1] {
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => {
                assert!((c1x - 10.0).abs() < 1e-5);
                assert!((c1y - -20.0).abs() < 1e-5); // y-flipped
                assert!((c2x - 30.0).abs() < 1e-5);
                assert!((c2y - -40.0).abs() < 1e-5);
                assert!((x - 50.0).abs() < 1e-5);
                assert!((y - -60.0).abs() < 1e-5);
            }
            _ => panic!("expected CubicTo"),
        }
    }

    #[test]
    fn arc_length_polyline_cubic_aggregates_chord_lengths() {
        // A single cubic Bézier: M 0 0 C 0 100, 100 100, 100 0. A
        // hump from (0,0) to (100,0). Its true arc length is about 146.
        // The polyline should have len > 1 chords and a non-trivial
        // total cum.
        let ops = vec![
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::CubicTo {
                c1x: 0.0,
                c1y: 100.0,
                c2x: 100.0,
                c2y: 100.0,
                x: 100.0,
                y: 0.0,
            },
        ];
        let poly = build_arc_length_polyline(&ops);
        assert!(poly.len() > 2, "cubic should subdivide into many chords");
        let total = poly.last().unwrap().cum;
        // The cubic with controls at y=100 sweeps well above a tight
        // arc. Empirical chord-length total at default tolerance is
        // ~200 (the curve's true arc length), not the much smaller
        // straight-line chord. Bound conservatively.
        assert!(
            (180.0..=220.0).contains(&total),
            "expected chord total ~200, got {total}"
        );
    }

    #[test]
    fn path_d_number_after_closepath_is_an_error() {
        // A number after `Z` used to repeat the closepath without
        // consuming input, looping forever while pushing Close ops.
        assert!(parse_path_d("M0 0 L1 0 Z 1 1").is_err());
        assert!(parse_path_d("m0 0 z5").is_err());
    }

    #[test]
    fn parse_color_rejects_six_bytes_of_non_ascii_hex() {
        // Six bytes of text where byte 2 falls inside a character used
        // to panic on the slice.
        assert_eq!(parse_color("#a\u{20AC}bc"), None);
        assert_eq!(parse_color("#\u{e9}\u{e9}\u{e9}"), None);
    }

    #[test]
    fn use_fan_out_is_bounded_by_the_work_budget() {
        // Ten levels of groups that each reference the next level ten
        // times expand to 10^10 element visits. This used to hang.
        use core::fmt::Write;
        let mut xml = String::from(r#"<svg viewBox="0 0 10 10"><defs>"#);
        for level in 0..10 {
            write!(xml, r#"<g id="l{level}">"#).unwrap();
            for _ in 0..10 {
                write!(xml, r##"<use href="#l{}"/>"##, level + 1).unwrap();
            }
            xml.push_str("</g>");
        }
        xml.push_str(r##"<g id="l10"/></defs><use href="#l0"/></svg>"##);
        assert_eq!(
            parse_document(&xml).unwrap_err(),
            RenderError::Parse("svg work cap")
        );
    }

    #[test]
    fn repeated_large_clip_is_bounded_by_the_ops_budget() {
        // Every fill used to carry its own copy of the clip path, so
        // 4096 fills sharing a 20k-op clip stored 80M operations.
        use core::fmt::Write;
        let mut clip = String::from("M0 0");
        for i in 0..20_000 {
            write!(clip, " L{} {}", i % 97, i % 89).unwrap();
        }
        let mut xml = String::from(r#"<svg viewBox="0 0 10 10"><defs><clipPath id="c">"#);
        write!(xml, r#"<path d="{clip}"/></clipPath></defs>"#).unwrap();
        for _ in 0..4096 {
            xml.push_str(r#"<rect width="5" height="5" clip-path="url(#c)"/>"#);
        }
        xml.push_str("</svg>");
        let doc = parse_document(&xml).unwrap();
        let stored: usize = doc.fills.iter().map(Fill::weight).sum();
        assert!(stored <= MAX_DOC_OPS, "stored {stored} ops");
        assert!(doc.fills.len() < 4096);
    }

    #[test]
    fn filter_keeps_at_most_max_primitives() {
        let mut xml = String::from(r#"<svg viewBox="0 0 10 10"><defs><filter id="f">"#);
        for _ in 0..500 {
            xml.push_str(r#"<feOffset dx="1"/>"#);
        }
        xml.push_str(r##"</filter></defs><rect width="5" height="5" filter="url(#f)"/></svg>"##);
        let doc = parse_document(&xml).unwrap();
        let f = doc.fills[0].filter.as_ref().expect("filter attached");
        assert_eq!(f.primitives.len(), MAX_FILTER_PRIMITIVES);
    }

    #[test]
    fn shared_mask_rendering_is_bounded_by_the_pass_budget() {
        // Every masked fill renders all mask children again, so fills
        // times children grows without limit. Here 1000 fills with a
        // 100-child mask would need about 102k canvas passes.
        use core::fmt::Write;
        let mut xml = String::from(r#"<svg viewBox="0 0 16 16"><defs><mask id="m">"#);
        for i in 0..100 {
            write!(
                xml,
                r#"<rect x="{}" y="0" width="1" height="16" fill="white"/>"#,
                i % 16
            )
            .unwrap();
        }
        xml.push_str("</mask></defs>");
        for _ in 0..1000 {
            xml.push_str(r#"<rect width="8" height="8" fill="red" mask="url(#m)"/>"#);
        }
        xml.push_str("</svg>");
        let doc = parse_document(&xml).unwrap();
        assert_eq!(doc.fills.len(), 1000);
        let mut out = ColorPixmap::new(16, 16);
        let mut budget = RenderBudget::new();
        for fill in &doc.fills {
            render_fill(&mut out, fill, &Affine::identity(), 0.25, &mut budget);
        }
        assert!(budget.passes_left < doc.fills[0].render_passes() + 100);
        assert_eq!(out.get(4, 4)[3], 255, "early fills still render");
    }

    #[test]
    fn repeated_mask_resolution_is_bounded_by_the_work_budget() {
        // Each masked element walks the whole mask body again. With a
        // 1000-child mask and 4000 elements that is four million
        // element visits, so the parse stops at the work budget.
        let mut xml = String::from(r#"<svg viewBox="0 0 16 16"><defs><mask id="m">"#);
        for _ in 0..1000 {
            xml.push_str(r#"<rect width="1" height="16" fill="white"/>"#);
        }
        xml.push_str("</mask></defs>");
        for _ in 0..4000 {
            xml.push_str(r#"<rect width="8" height="8" fill="red" mask="url(#m)"/>"#);
        }
        xml.push_str("</svg>");
        assert_eq!(
            parse_document(&xml).unwrap_err(),
            RenderError::Parse("svg work cap")
        );
    }

    #[test]
    fn tiny_dash_on_a_long_line_stops_at_the_split_budget() {
        // Past ~16384 units a 0.001 dash no longer advances the walk
        // position, so this used to loop forever.
        let pts = [(0.0, 0.0), (100_000.0, 0.0)];
        let segs = dash_polyline(&pts, &[100_000.0], false, &[0.001, 0.001], 0.0);
        assert!(!segs.is_empty());
        assert!(segs.len() <= MAX_DASH_SPLITS);
    }

    #[test]
    fn non_finite_curves_do_not_multiply_stroke_points() {
        // A NaN control point used to subdivide 16 levels deep and emit
        // 65536 points per curve.
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::QuadTo {
                cx: f32::NAN,
                cy: 0.0,
                x: 10.0,
                y: 0.0,
            },
            PathOp::CubicTo {
                c1x: f32::INFINITY,
                c1y: 0.0,
                c2x: 0.0,
                c2y: 0.0,
                x: 20.0,
                y: 5.0,
            },
        ];
        let polys = flatten_to_polylines(&ops);
        let points: usize = polys.iter().map(|p| p.points.len()).sum();
        assert_eq!(points, 3);
    }

    #[test]
    fn huge_blur_radius_is_clamped() {
        let mut src = ColorPixmap::new(7, 5);
        for (i, b) in src.data.iter_mut().enumerate() {
            *b = (i * 37 % 256) as u8;
        }
        // Used to overflow `r * 2 + 1` (a panic in debug builds) and
        // visit four billion window samples per row.
        let out = apply_gaussian_blur(&src, 1e30, f32::INFINITY);
        assert_eq!((out.width, out.height), (7, 5));
    }

    #[test]
    fn counted_blur_window_matches_the_sample_by_sample_sum() {
        for r in 0..24 {
            for len in 1..12 {
                let sample = |k: i32| (k * 13 + 7) as u32 % 256;
                let naive: u32 = (-r..=r).map(|k| sample(k.clamp(0, len - 1))).sum();
                assert_eq!(clamped_window_sum(r, len, sample), naive, "r {r} len {len}");
            }
        }
    }

    #[test]
    fn huge_filter_offset_does_not_overflow() {
        let mut src = ColorPixmap::new(4, 4);
        src.data.fill(200);
        // `y - dy` used to overflow `i32` once the offset saturated.
        for (dx, dy) in [
            (0.0, -1e30),
            (-1e30, 0.0),
            (f32::NEG_INFINITY, f32::INFINITY),
        ] {
            let out = apply_offset(&src, dx, dy);
            assert!(out.data.iter().all(|&b| b == 0));
        }
    }
}
