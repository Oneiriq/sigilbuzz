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
//!   both supported. Nested mask references inside the mask body
//!   remain deferred.
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
//!   position. Glyphs are placed axis-aligned only. Tangent rotation
//!   is deferred to a follow-up. `side="right"` and path cycling
//!   (`startOffset` past path end) are also deferred.
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

mod clip_mask;
mod dash;
mod document;
mod filter;
mod model;
mod paint_server;
mod path;
mod render;
mod stroke;
mod style;
mod text_path;
mod xml;

use alloc::vec::Vec;

use sigilbuzz::Face;

use crate::affine::Affine;
use crate::error::RenderError;
use crate::pixmap::ColorPixmap;
use crate::rasterizer::Rasterizer;

use document::{collect_defs, parse_document_with, Defs};
use render::render_fill;
use text_path::append_text_path_fills;
use xml::parse_xml;

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
    /// `currentColor` in `fill`, `stroke`, `stop-color`, and `color` is
    /// the rasterizer's foreground color (see
    /// [`Rasterizer::with_foreground`]), the text color the OpenType SVG
    /// spec hands a glyph document, until a `color` attribute changes
    /// it for a subtree.
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
        let doc = parse_document_with(xml, self.foreground())?;

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
    /// tangent; this is a known PoC limitation flagged by PR #236's
    /// defer-note and tracked for the next minor. `side="right"` and
    /// path cycling beyond a single cumulative-advance walk are also
    /// deferred. Extra glyphs whose advance overruns the path's total
    /// length are silently dropped.
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
        let mut doc = parse_document_with(xml, self.foreground())?;

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
            let mut defs = Defs::default();
            collect_defs(&root, &mut defs);
            append_text_path_fills(
                &mut doc,
                &root,
                &defs,
                face,
                coords,
                upem,
                text_paths,
                self.foreground(),
            );
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
/// arc length are silently dropped (path cycling is deferred, see the
/// module-level docs).
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
// Tests (parser + geometry primitives)
// =========================================================================

#[cfg(test)]
mod tests;
