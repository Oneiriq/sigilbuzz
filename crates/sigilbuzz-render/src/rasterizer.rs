//! Public `Rasterizer` entry points.
//!
//! Two methods at the moment:
//!
//! - [`Rasterizer::rasterize_glyph`]: outline rasterization for any
//!   glyph reachable through `Face::glyph_outline_at_coords` (glyf,
//!   CFF, CFF2, VARC).
//! - [`Rasterizer::rasterize_colrv0_glyph`]: COLRv0 layered color
//!   composition. Each layer is rasterized as a sub-glyph, multiplied
//!   by its CPAL palette color, and `over`-composited onto the
//!   running RGBA pixmap.
//!
//! Every entry point has a `_placed` sibling that also returns the
//! image's [`Placement`], its offset from the glyph origin.

use alloc::vec::Vec;

use sigilbuzz::Face;

use crate::affine::Affine;
use crate::bitmaps;
use crate::colrv1::rasterize_colrv1;
use crate::error::RenderError;
use crate::flatten::{flatten_fill, flatten_fill_limited, Segment, MAX_SEGMENTS};
use crate::pixmap::{ColorPixmap, Pixmap, Placement};
use crate::raster::{raster_bounds, rasterize as raster, RasterBounds, MAX_RASTER_DIM};

/// Configuration for the rasterizer.
///
/// Construction is cheap. The default config picks a `0.25`-pixel
/// curve flattening tolerance, which matches FreeType's smooth
/// rasterizer perceptually, and an opaque black foreground color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rasterizer {
    /// Curve flattening tolerance in pixels.
    tolerance: f32,
    /// Straight-alpha RGBA used for COLR palette entry `0xFFFF`.
    foreground: [u8; 4],
}

impl Default for Rasterizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Rasterizer {
    /// Foreground color used unless [`Rasterizer::with_foreground`]
    /// picks another: opaque black, the default ink of a text renderer.
    pub const DEFAULT_FOREGROUND: [u8; 4] = [0, 0, 0, 255];

    /// New rasterizer with default settings.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            tolerance: 0.25,
            foreground: Self::DEFAULT_FOREGROUND,
        }
    }

    /// Override the curve flattening tolerance (pixel units). Smaller
    /// is more accurate and slower.
    #[must_use]
    pub const fn with_tolerance(mut self, tol: f32) -> Self {
        self.tolerance = tol;
        self
    }

    /// Paints color-glyph layers that use COLR palette entry `0xFFFF`
    /// (the text color) in `rgba`, straight (not premultiplied) alpha.
    /// Applies to [`Rasterizer::rasterize_colrv0_glyph`] and
    /// [`Rasterizer::rasterize_colrv1_glyph`], where the paint's own
    /// alpha multiplies `rgba[3]`, and to `currentColor` in the documents
    /// [`Rasterizer::rasterize_svg_glyph`] draws.
    ///
    /// ```
    /// use sigilbuzz_render::Rasterizer;
    ///
    /// assert_eq!(Rasterizer::new().foreground(), [0, 0, 0, 255]);
    /// let white = Rasterizer::new().with_foreground([255, 255, 255, 255]);
    /// assert_eq!(white.foreground(), [255, 255, 255, 255]);
    /// ```
    #[must_use]
    pub const fn with_foreground(mut self, rgba: [u8; 4]) -> Self {
        self.foreground = rgba;
        self
    }

    /// The foreground color, straight-alpha RGBA.
    ///
    /// ```
    /// use sigilbuzz_render::Rasterizer;
    ///
    /// assert_eq!(Rasterizer::new().foreground(), Rasterizer::DEFAULT_FOREGROUND);
    /// ```
    #[must_use]
    pub const fn foreground(&self) -> [u8; 4] {
        self.foreground
    }

    /// Returns the configured curve flattening tolerance. Used by
    /// sibling modules (`svg.rs`) that re-use the flatten/raster
    /// pipeline directly.
    #[must_use]
    pub(crate) fn flattening_tolerance(&self) -> f32 {
        self.tolerance
    }

    /// Rasterizes a single glyph outline at `size_pt` pixels with the
    /// given variable-font normalized coords. The returned [`Pixmap`]
    /// is sized to the glyph's bounding box plus a one-pixel margin so
    /// anti-aliased edges don't clip.
    ///
    /// `size_pt` is the rendering size in pixels (the renderer's "em
    /// size"); the function maps font design-units to pixels via
    /// `size_pt / units_per_em`. Y flips so that increasing pixel rows
    /// move down (the conventional bitmap orientation).
    ///
    /// The pixmap does not say where it sits relative to the glyph
    /// origin. [`Rasterizer::rasterize_glyph_placed`] returns the same
    /// pixmap together with that offset.
    ///
    /// # Errors
    /// Returns [`RenderError::NoOutline`] when the glyph is invisible
    /// (whitespace) or out of range; [`RenderError::BadSize`] when
    /// `size_pt` is non-finite or non-positive, or when the rendered
    /// glyph would exceed 16384 pixels on a side; [`RenderError::BadUpem`]
    /// when the font has zero units-per-em; [`RenderError::Parse`]
    /// when the underlying parser refuses the glyph data.
    pub fn rasterize_glyph(
        &self,
        face: &Face<'_>,
        gid: u16,
        size_pt: f32,
        coords: &[f32],
    ) -> Result<Pixmap, RenderError> {
        self.rasterize_glyph_placed(face, gid, size_pt, coords)
            .map(|(pixmap, _)| pixmap)
    }

    /// Rasterizes a glyph outline like [`Rasterizer::rasterize_glyph`]
    /// and also returns where the pixmap sits relative to the glyph
    /// origin (see [`Placement`]).
    ///
    /// The pixmap covers the outline's bounding box in pixels, rounded
    /// out to whole pixels plus a one-pixel margin, so `left` is
    /// `floor(x_min * scale) - 1` and `top` is `floor(-y_max * scale) - 1`,
    /// where `scale` is `size_pt / units_per_em` and the box is the
    /// flattened outline's. A glyph whose outline flattens to nothing
    /// returns an empty pixmap at `Placement::default()`.
    ///
    /// ```
    /// use sigilbuzz::Face;
    /// use sigilbuzz_render::Rasterizer;
    ///
    /// let data = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
    /// let face = Face::parse_bytes(data, 0).unwrap();
    /// let gid = face.cmap().unwrap().glyph_id('g').unwrap();
    /// let (pix, at) = Rasterizer::new()
    ///     .rasterize_glyph_placed(&face, gid, 40.0, &[])
    ///     .unwrap();
    /// // 'g' has a descender: its image starts above the baseline and
    /// // ends below it.
    /// assert!(at.top < 0);
    /// assert!(at.top + pix.height as i32 > 1);
    /// ```
    ///
    /// # Errors
    /// The same as [`Rasterizer::rasterize_glyph`].
    pub fn rasterize_glyph_placed(
        &self,
        face: &Face<'_>,
        gid: u16,
        size_pt: f32,
        coords: &[f32],
    ) -> Result<(Pixmap, Placement), RenderError> {
        if !size_pt.is_finite() || size_pt <= 0.0 {
            return Err(RenderError::BadSize(size_pt));
        }
        let head = face.head().map_err(|_| RenderError::Parse("head"))?;
        let upem = head.units_per_em as f32;
        if upem <= 0.0 {
            return Err(RenderError::BadUpem);
        }
        let outline = face
            .glyph_outline_at_coords(gid, coords)
            .map_err(|_| RenderError::Parse("glyph_outline"))?;
        let outline = outline.ok_or(RenderError::NoOutline(gid))?;
        if outline.is_empty() {
            return Err(RenderError::NoOutline(gid));
        }

        // Design-units -> pixels. Y flips because OpenType's y axis
        // points up while bitmap rows go down.
        let s = size_pt / upem;
        let xform = Affine {
            xx: s,
            yx: 0.0,
            xy: 0.0,
            yy: -s,
            dx: 0.0,
            dy: 0.0,
        };

        let segs = flatten_fill(outline.ops().iter().copied(), &xform, self.tolerance);
        if let Some(b) = raster_bounds(&segs) {
            if b.width > MAX_RASTER_DIM || b.height > MAX_RASTER_DIM {
                return Err(RenderError::BadSize(size_pt));
            }
        }
        // The device transform puts the glyph origin at pixel (0, 0),
        // so the raster's origin is the placement. An empty raster
        // reports (0, 0).
        let r = raster(&segs);
        Ok((r.pixmap, Placement::new(r.origin_x, r.origin_y)))
    }

    /// Rasterizes a COLRv0 layered color glyph and composes the
    /// layers into an RGBA premultiplied [`ColorPixmap`].
    ///
    /// Layers are drawn in COLR order (bottom-up). Each layer's outline
    /// is rasterized at the same size as the base glyph; the resulting
    /// alpha mask is multiplied by the palette color for that layer
    /// and then `over`-composited on top of the running pixmap. The
    /// special palette index `0xFFFF` paints in the foreground color
    /// (see [`Rasterizer::with_foreground`]), opaque black by default.
    ///
    /// Palette entries resolve the way
    /// [`Rasterizer::rasterize_colrv1_glyph`] resolves them, as in
    /// HarfBuzz: a palette index the font does not have, an entry past
    /// the end of the palette, and a font without `CPAL` all paint in
    /// the foreground color instead of failing.
    ///
    /// [`Rasterizer::rasterize_colrv0_glyph_placed`] returns the same
    /// pixmap together with its offset from the glyph origin.
    ///
    /// # Errors
    /// - [`RenderError::NoColrV0`] when the glyph has no v0 layer record.
    /// - [`RenderError::BadSize`] when `size_pt` is non-finite or
    ///   non-positive, or when the composed glyph would exceed 16384
    ///   pixels on a side.
    /// - [`RenderError::Parse`] for any underlying parser failure.
    pub fn rasterize_colrv0_glyph(
        &self,
        face: &Face<'_>,
        gid: u16,
        palette_index: u16,
        size_pt: f32,
        coords: &[f32],
    ) -> Result<ColorPixmap, RenderError> {
        self.rasterize_colrv0_glyph_placed(face, gid, palette_index, size_pt, coords)
            .map(|(pixmap, _)| pixmap)
    }

    /// Rasterizes a COLRv0 glyph like
    /// [`Rasterizer::rasterize_colrv0_glyph`] and also returns where the
    /// pixmap sits relative to the glyph origin (see [`Placement`]).
    ///
    /// The pixmap covers the union of the layer outlines' pixel boxes,
    /// each rounded out to whole pixels plus a one-pixel margin, the box
    /// [`Rasterizer::rasterize_glyph_placed`] gives a single outline. A
    /// glyph whose layers draw nothing returns an empty pixmap at
    /// `Placement::default()`.
    ///
    /// A font without color layers for the glyph returns
    /// [`RenderError::NoColrV0`], and a renderer falls back to the
    /// outline:
    ///
    /// ```
    /// use sigilbuzz::Face;
    /// use sigilbuzz_render::{Rasterizer, RenderError};
    ///
    /// let data = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
    /// let face = Face::parse_bytes(data, 0).unwrap();
    /// let gid = face.cmap().unwrap().glyph_id('A').unwrap();
    /// let rast = Rasterizer::new();
    /// let at = match rast.rasterize_colrv0_glyph_placed(&face, gid, 0, 24.0, &[]) {
    ///     Ok((_color, at)) => at,
    ///     Err(RenderError::NoColrV0(_)) => {
    ///         rast.rasterize_glyph_placed(&face, gid, 24.0, &[]).unwrap().1
    ///     }
    ///     Err(e) => panic!("{e}"),
    /// };
    /// assert!(at.top < 0, "'A' rises above the baseline");
    /// ```
    ///
    /// # Errors
    /// The same as [`Rasterizer::rasterize_colrv0_glyph`].
    pub fn rasterize_colrv0_glyph_placed(
        &self,
        face: &Face<'_>,
        gid: u16,
        palette_index: u16,
        size_pt: f32,
        coords: &[f32],
    ) -> Result<(ColorPixmap, Placement), RenderError> {
        if !size_pt.is_finite() || size_pt <= 0.0 {
            return Err(RenderError::BadSize(size_pt));
        }
        let head = face.head().map_err(|_| RenderError::Parse("head"))?;
        let upem = head.units_per_em as f32;
        if upem <= 0.0 {
            return Err(RenderError::BadUpem);
        }
        let colr = face
            .colr()
            .map_err(|_| RenderError::Parse("colr"))?
            .ok_or(RenderError::NoColrV0(gid))?;
        let layers = colr.v0_layers(gid).ok_or(RenderError::NoColrV0(gid))?;
        let cpal = face.cpal().map_err(|_| RenderError::Parse("cpal"))?;

        let s = size_pt / upem;
        let xform = Affine {
            xx: s,
            yx: 0.0,
            xy: 0.0,
            yy: -s,
            dx: 0.0,
            dy: 0.0,
        };

        // First pass: flatten every layer and find where its mask will
        // land, so we can establish the union bounding box before
        // allocating the destination. Masks are rasterized one at a
        // time in the second pass, which keeps memory at one mask no
        // matter how many layers the record lists.
        struct LayerEdges {
            segs: Vec<Segment>,
            bounds: Option<RasterBounds>,
            color: [u8; 4],
        }
        let mut edges: Vec<LayerEdges> = Vec::new();
        // All layers share one segment budget, so a record that lists
        // the same heavy outline thousands of times stays bounded.
        let mut budget = MAX_SEGMENTS;
        for layer in layers.iter() {
            if budget == 0 {
                break;
            }
            let outline = face
                .glyph_outline_at_coords(layer.glyph_id, coords)
                .map_err(|_| RenderError::Parse("glyph_outline"))?;
            let Some(outline) = outline else {
                continue;
            };
            if outline.is_empty() {
                continue;
            }
            let segs = flatten_fill_limited(
                outline.ops().iter().copied(),
                &xform,
                self.tolerance,
                budget,
            );
            budget = budget.saturating_sub(segs.len());
            if segs.is_empty() {
                continue;
            }
            let bounds = raster_bounds(&segs);

            // HarfBuzz's paint context: entry 0xFFFF is the foreground,
            // and so is any entry the font cannot supply.
            let color = cpal
                .as_ref()
                .filter(|_| layer.palette_index != 0xFFFF)
                .and_then(|cpal| cpal.color(palette_index, layer.palette_index))
                .map_or(self.foreground, |c| [c.r, c.g, c.b, c.a]);
            edges.push(LayerEdges {
                segs,
                bounds,
                color,
            });
        }

        if edges.is_empty() {
            return Ok((ColorPixmap::new(0, 0), Placement::default()));
        }

        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        for b in edges.iter().filter_map(|e| e.bounds) {
            min_x = min_x.min(b.origin_x);
            min_y = min_y.min(b.origin_y);
            max_x = max_x.max(b.origin_x + b.width as i32);
            max_y = max_y.max(b.origin_y + b.height as i32);
        }
        if max_x <= min_x || max_y <= min_y {
            return Ok((ColorPixmap::new(0, 0), Placement::default()));
        }
        let width = (max_x - min_x) as u32;
        let height = (max_y - min_y) as u32;
        // The union contains every layer, so this also covers a single
        // layer that is too large to rasterize.
        if width > MAX_RASTER_DIM || height > MAX_RASTER_DIM {
            return Err(RenderError::BadSize(size_pt));
        }
        let mut out = ColorPixmap::new(width, height);

        for e in &edges {
            if e.bounds.is_none() {
                continue;
            }
            let r = raster(&e.segs);
            if r.pixmap.is_empty() {
                continue;
            }
            let dx = (r.origin_x - min_x) as u32;
            let dy = (r.origin_y - min_y) as u32;
            blit_layer(&mut out, &r.pixmap, dx, dy, e.color);
        }
        // The device transform puts the glyph origin at pixel (0, 0),
        // so the union's corner is the placement.
        Ok((out, Placement::new(min_x, min_y)))
    }

    /// Rasterizes a COLRv1 paint-tree color glyph into a premultiplied
    /// RGBA [`ColorPixmap`].
    ///
    /// Walks the paint tree the way HarfBuzz's `hb_font_paint_glyph`
    /// does and draws every fill (solid / linear / radial / sweep
    /// gradient) inside its enclosing clips, with each `PaintComposite`
    /// blending isolated source and backdrop layers. A transform below a
    /// `PaintGlyph` moves the fill, not the outline that clips it, and
    /// gradients stay exact under any transform.
    ///
    /// The surface is the glyph's clip box, as in HarfBuzz: its ClipList
    /// box when it has one, else the bounds of its paint tree, rounded
    /// out to whole pixels plus a one-pixel transparent margin. Paint
    /// outside the box is clipped. A glyph whose paint is not bounded by
    /// any clip renders as an empty pixmap.
    ///
    /// `palette_index` selects the CPAL palette that solid fills and
    /// gradient stops resolve against. An index the font does not have
    /// is not an error: as in HarfBuzz, every palette entry then paints
    /// in the foreground color, as does an entry the palette lacks.
    /// Foreground (`0xFFFF`) entries paint in the foreground color (see
    /// [`Rasterizer::with_foreground`]), opaque black by default.
    ///
    /// [`Rasterizer::rasterize_colrv1_glyph_placed`] returns the same
    /// pixmap together with its offset from the glyph origin.
    ///
    /// # Errors
    /// - [`RenderError::ColrV1NotFound`] when the font has no v1
    ///   paint record for `gid`.
    /// - [`RenderError::BadSize`] when `size_pt` is non-finite or
    ///   non-positive, when the clip box would exceed 16384 pixels on a
    ///   side, when nested clips and composite layers would need more
    ///   than 4 GiB of storage at once, or when drawing the paint graph
    ///   would take more than 2^34 pixel updates.
    /// - [`RenderError::BadUpem`] when the font has zero `units_per_em`.
    /// - [`RenderError::Parse`] when the underlying parser refuses
    ///   one of the tables we need.
    pub fn rasterize_colrv1_glyph(
        &self,
        face: &Face<'_>,
        gid: u16,
        palette_index: u16,
        size_pt: f32,
        coords: &[f32],
    ) -> Result<ColorPixmap, RenderError> {
        self.rasterize_colrv1_glyph_placed(face, gid, palette_index, size_pt, coords)
            .map(|(pixmap, _)| pixmap)
    }

    /// Rasterizes a COLRv1 glyph like
    /// [`Rasterizer::rasterize_colrv1_glyph`] and also returns where the
    /// pixmap sits relative to the glyph origin (see [`Placement`]).
    ///
    /// The pixmap is the glyph's clip box in pixels: its ClipList box,
    /// or the bounds of its paint tree when it has none, scaled by
    /// `size_pt / units_per_em`, rounded out to whole pixels, plus a
    /// one-pixel transparent margin. For a box `(x_min, y_min, x_max,
    /// y_max)` in design units that makes `left` equal to
    /// `floor(x_min * scale) - 1` and `top` equal to
    /// `floor(-y_max * scale) - 1`. An unbounded glyph, which renders
    /// as an empty pixmap, returns `Placement::default()`.
    ///
    /// A font without a COLRv1 paint for the glyph returns
    /// [`RenderError::ColrV1NotFound`], and a renderer falls back to
    /// the next format:
    ///
    /// ```
    /// use sigilbuzz::Face;
    /// use sigilbuzz_render::{Rasterizer, RenderError};
    ///
    /// let data = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
    /// let face = Face::parse_bytes(data, 0).unwrap();
    /// let gid = face.cmap().unwrap().glyph_id('Q').unwrap();
    /// let rast = Rasterizer::new();
    /// let (width, at) = match rast.rasterize_colrv1_glyph_placed(&face, gid, 0, 24.0, &[]) {
    ///     Ok((color, at)) => (color.width, at),
    ///     Err(RenderError::ColrV1NotFound(_)) => {
    ///         let (alpha, at) = rast.rasterize_glyph_placed(&face, gid, 24.0, &[]).unwrap();
    ///         (alpha.width, at)
    ///     }
    ///     Err(e) => panic!("{e}"),
    /// };
    /// assert!(width > 0);
    /// // 'Q' rises above the baseline.
    /// assert!(at.top < 0);
    /// ```
    ///
    /// # Errors
    /// The same as [`Rasterizer::rasterize_colrv1_glyph`].
    pub fn rasterize_colrv1_glyph_placed(
        &self,
        face: &Face<'_>,
        gid: u16,
        palette_index: u16,
        size_pt: f32,
        coords: &[f32],
    ) -> Result<(ColorPixmap, Placement), RenderError> {
        rasterize_colrv1(
            face,
            gid,
            palette_index,
            size_pt,
            coords,
            self.tolerance,
            self.foreground,
        )
    }

    /// Rasterizes an embedded bitmap glyph (CBDT/CBLC or sbix PNG)
    /// into a [`ColorPixmap`]. Strike selection picks the closest
    /// match, and the result is bilinearly rescaled when the strike
    /// ppem doesn't equal the requested `size_pt`.
    ///
    /// `coords` is reserved for future variable-axis bitmap variants
    /// and currently unused. The canonical bitmap embed tables don't
    /// vary per axis.
    ///
    /// [`Rasterizer::rasterize_bitmap_glyph_placed`] returns the same
    /// pixmap together with its offset from the glyph origin.
    ///
    /// # Errors
    /// See [`crate::rasterize_bitmap_glyph`].
    pub fn rasterize_bitmap_glyph(
        &self,
        face: &Face<'_>,
        gid: u16,
        size_pt: f32,
        coords: &[f32],
    ) -> Result<ColorPixmap, RenderError> {
        bitmaps::rasterize_bitmap_glyph(self, face, gid, size_pt, coords)
    }

    /// Rasterizes an embedded bitmap glyph like
    /// [`Rasterizer::rasterize_bitmap_glyph`] and also returns where the
    /// pixmap sits relative to the glyph origin (see [`Placement`]).
    ///
    /// The offset comes from the strike: the CBDT or EBDT horizontal
    /// bearings, or the sbix origin offset, scaled with the bitmap when
    /// it is resampled. See [`crate::rasterize_bitmap_glyph_placed`].
    ///
    /// ```
    /// use sigilbuzz::Face;
    /// use sigilbuzz_render::Rasterizer;
    ///
    /// // One 32 ppem CBDT strike holding gid 1.
    /// let data = include_bytes!("../../../tests/fixtures/cbdt_synthetic.ttf");
    /// let face = Face::parse_bytes(data, 0).unwrap();
    /// let rast = Rasterizer::new();
    /// let (pix, at) = rast.rasterize_bitmap_glyph_placed(&face, 1, 32.0, &[]).unwrap();
    /// assert_eq!(pix, rast.rasterize_bitmap_glyph(&face, 1, 32.0, &[]).unwrap());
    /// // Twice the size doubles the bitmap and its offsets.
    /// let (_, at2) = rast.rasterize_bitmap_glyph_placed(&face, 1, 64.0, &[]).unwrap();
    /// assert_eq!((at2.left, at2.top), (2 * at.left, 2 * at.top));
    /// ```
    ///
    /// # Errors
    /// See [`crate::rasterize_bitmap_glyph`].
    pub fn rasterize_bitmap_glyph_placed(
        &self,
        face: &Face<'_>,
        gid: u16,
        size_pt: f32,
        coords: &[f32],
    ) -> Result<(ColorPixmap, Placement), RenderError> {
        bitmaps::rasterize_bitmap_glyph_placed(self, face, gid, size_pt, coords)
    }
}

/// Composites `mask * color` onto `dst` at offset `(dx, dy)` using the
/// straight-alpha source-over operator. `dst` stores premultiplied
/// RGBA.
fn blit_layer(dst: &mut ColorPixmap, mask: &Pixmap, dx: u32, dy: u32, color: [u8; 4]) {
    let dw = dst.width;
    let dh = dst.height;
    if dw == 0 || dh == 0 {
        return;
    }
    let cr = color[0] as u32;
    let cg = color[1] as u32;
    let cb = color[2] as u32;
    let ca = color[3] as u32;
    for my in 0..mask.height {
        let py = dy + my;
        if py >= dh {
            break;
        }
        for mx in 0..mask.width {
            let px = dx + mx;
            if px >= dw {
                break;
            }
            let m = mask.get(mx, my) as u32;
            if m == 0 {
                continue;
            }
            // Source alpha: layer color alpha * coverage.
            let sa = (ca * m + 127) / 255;
            if sa == 0 {
                continue;
            }
            // Premultiplied source.
            let sr = (cr * sa + 127) / 255;
            let sg = (cg * sa + 127) / 255;
            let sb = (cb * sa + 127) / 255;
            let idx = (py as usize * dw as usize + px as usize) * 4;
            let dr = dst.data[idx] as u32;
            let dg = dst.data[idx + 1] as u32;
            let db = dst.data[idx + 2] as u32;
            let da = dst.data[idx + 3] as u32;
            let inv = 255 - sa;
            // out = src + dst * (1 - src.a). Source-over with
            // premultiplied dst.
            dst.data[idx] = (sr + (dr * inv + 127) / 255) as u8;
            dst.data[idx + 1] = (sg + (dg * inv + 127) / 255) as u8;
            dst.data[idx + 2] = (sb + (db * inv + 127) / 255) as u8;
            dst.data[idx + 3] = (sa + (da * inv + 127) / 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::flatten::flatten;
    use sigilbuzz::tables::PathOp;

    /// A CFF-style contour: two cubics and no `Close`. CFF draws the
    /// closing edge implicitly, here the line from (20, 200) back down to
    /// the start at (20, 0).
    fn open_cff_contour() -> Vec<PathOp> {
        alloc::vec![
            PathOp::MoveTo { x: 20.0, y: 0.0 },
            PathOp::CubicTo {
                c1x: 120.0,
                c1y: 0.0,
                c2x: 180.0,
                c2y: 50.0,
                x: 180.0,
                y: 100.0,
            },
            PathOp::CubicTo {
                c1x: 180.0,
                c1y: 150.0,
                c2x: 120.0,
                c2y: 200.0,
                x: 20.0,
                y: 200.0,
            },
        ]
    }

    #[test]
    fn open_cff_contour_fills_like_its_closed_form() {
        let xform = Affine::scale(0.1, -0.1);
        let mut closed = open_cff_contour();
        closed.push(PathOp::Close);
        let want = raster(&flatten_fill(closed, &xform, 0.25));
        let got = raster(&flatten_fill(open_cff_contour(), &xform, 0.25));
        assert_eq!(got.pixmap, want.pixmap);
        assert_eq!((got.origin_x, got.origin_y), (want.origin_x, want.origin_y));
        // The middle of the D is solid, and nothing leaks past its edges.
        let at = |x: i32, y: i32| {
            got.pixmap
                .get((x - got.origin_x) as u32, (y - got.origin_y) as u32)
        };
        assert_eq!(at(9, -10), 255);
        assert_eq!(at(0, -10), 0);
        assert_eq!(at(19, -10), 0);
        // The public flatten leaves the contour open, and filling those
        // edges as they are does not give the glyph.
        let unclosed = raster(&flatten(open_cff_contour(), &xform, 0.25));
        assert_ne!(unclosed.pixmap, want.pixmap);
    }

    #[test]
    fn rasterizer_default_and_with_tolerance() {
        let r = Rasterizer::default();
        assert!((r.tolerance - 0.25).abs() < 1e-6);
        let r = Rasterizer::new().with_tolerance(0.1);
        assert!((r.tolerance - 0.1).abs() < 1e-6);
    }

    #[test]
    fn blit_solid_red_over_transparent() {
        let mut dst = ColorPixmap::new(4, 4);
        let mut mask = Pixmap::new(2, 2);
        mask.set(0, 0, 255);
        mask.set(1, 1, 128);
        blit_layer(&mut dst, &mask, 1, 1, [255, 0, 0, 255]);
        assert_eq!(dst.get(1, 1), [255, 0, 0, 255]);
        let mid = dst.get(2, 2);
        assert!(mid[0] > 100 && mid[0] < 200, "premul red ~half: {}", mid[0]);
        assert_eq!(mid[1], 0);
        // Pixel outside mask stays transparent.
        assert_eq!(dst.get(0, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn blit_two_layers_over_compose() {
        // Bottom blue, top red half-alpha: result should be a mix.
        let mut dst = ColorPixmap::new(2, 2);
        let mut mask = Pixmap::new(2, 2);
        for i in 0..4 {
            mask.data[i] = 255;
        }
        blit_layer(&mut dst, &mask, 0, 0, [0, 0, 255, 255]);
        // Now half-opaque red on top.
        blit_layer(&mut dst, &mask, 0, 0, [255, 0, 0, 128]);
        let p = dst.get(0, 0);
        assert!(p[0] > 100, "red present, got {p:?}");
        assert!(p[2] > 100, "blue still present, got {p:?}");
        assert_eq!(p[3], 255);
    }
}
