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

use alloc::vec::Vec;

use sigilbuzz::Face;

use crate::affine::Affine;
use crate::bitmaps;
use crate::colrv1::rasterize_colrv1;
use crate::error::RenderError;
use crate::flatten::{flatten, flatten_limited, Segment, MAX_SEGMENTS};
use crate::pixmap::{ColorPixmap, Pixmap};
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

        let segs = flatten(outline.ops().iter().copied(), &xform, self.tolerance);
        if let Some(b) = raster_bounds(&segs) {
            if b.width > MAX_RASTER_DIM || b.height > MAX_RASTER_DIM {
                return Err(RenderError::BadSize(size_pt));
            }
        }
        let r = raster(&segs);
        Ok(r.pixmap)
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
            let segs = flatten_limited(
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
            return Ok(ColorPixmap::new(0, 0));
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
            return Ok(ColorPixmap::new(0, 0));
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
        Ok(out)
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
