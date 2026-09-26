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
use crate::flatten::flatten;
use crate::pixmap::{ColorPixmap, Pixmap};
use crate::raster::{rasterize as raster, Render};

/// Configuration for the rasterizer.
///
/// Construction is cheap. The default config picks a `0.25`-pixel
/// curve flattening tolerance, which matches FreeType's smooth
/// rasterizer perceptually.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rasterizer {
    /// Curve flattening tolerance in pixels.
    tolerance: f32,
}

impl Default for Rasterizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Rasterizer {
    /// New rasterizer with default settings.
    #[must_use]
    pub const fn new() -> Self {
        Self { tolerance: 0.25 }
    }

    /// Override the curve flattening tolerance (pixel units). Smaller
    /// is more accurate and slower.
    #[must_use]
    pub const fn with_tolerance(mut self, tol: f32) -> Self {
        self.tolerance = tol;
        self
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
    /// `size_pt` is non-finite or non-positive; [`RenderError::BadUpem`]
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
    /// special palette index `0xFFFF` falls back to opaque black,
    /// rasterizers in real apps would substitute the foreground text
    /// color here, but at this layer we have no app-level context.
    ///
    /// # Errors
    /// - [`RenderError::NoColrV0`] when the glyph has no v0 layer record.
    /// - [`RenderError::NoCpal`] when the font lacks `CPAL`.
    /// - [`RenderError::BadPaletteIndex`] when a layer's palette entry
    ///   is out of range.
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
        let cpal = face
            .cpal()
            .map_err(|_| RenderError::Parse("cpal"))?
            .ok_or(RenderError::NoCpal)?;

        // Validate the user-supplied palette index against the CPAL
        // up front. The per-layer `cpal.color()` lookup below would
        // also catch this, but only for layers whose palette entry
        // is not the foreground sentinel `0xFFFF`. A glyph composed
        // entirely of foreground layers would otherwise silently
        // accept an out-of-range palette. (issue #203)
        if palette_index >= cpal.num_palettes() {
            return Err(RenderError::BadPaletteIndex {
                palette: palette_index,
                entry: 0xFFFF,
            });
        }

        let s = size_pt / upem;
        let xform = Affine {
            xx: s,
            yx: 0.0,
            xy: 0.0,
            yy: -s,
            dx: 0.0,
            dy: 0.0,
        };

        // First pass: rasterize every layer into its own offset pixmap
        // so we can establish the union bounding box before allocating
        // the destination.
        struct LayerMask {
            r: Render,
            color: [u8; 4],
        }
        let mut masks: Vec<LayerMask> = Vec::new();
        for layer in layers.iter() {
            let outline = face
                .glyph_outline_at_coords(layer.glyph_id, coords)
                .map_err(|_| RenderError::Parse("glyph_outline"))?;
            let Some(outline) = outline else {
                continue;
            };
            if outline.is_empty() {
                continue;
            }
            let segs = flatten(outline.ops().iter().copied(), &xform, self.tolerance);
            if segs.is_empty() {
                continue;
            }
            let mask = raster(&segs);

            let color = if layer.palette_index == 0xFFFF {
                // Foreground fallback. The renderer has no app context
                // here, so emit opaque black; a downstream caller can
                // remap this layer if it cares.
                [0, 0, 0, 255]
            } else {
                let c = cpal.color(palette_index, layer.palette_index).ok_or(
                    RenderError::BadPaletteIndex {
                        palette: palette_index,
                        entry: layer.palette_index,
                    },
                )?;
                [c.r, c.g, c.b, c.a]
            };
            masks.push(LayerMask { r: mask, color });
        }

        if masks.is_empty() {
            return Ok(ColorPixmap::new(0, 0));
        }

        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        for m in &masks {
            if m.r.pixmap.is_empty() {
                continue;
            }
            min_x = min_x.min(m.r.origin_x);
            min_y = min_y.min(m.r.origin_y);
            max_x = max_x.max(m.r.origin_x + m.r.pixmap.width as i32);
            max_y = max_y.max(m.r.origin_y + m.r.pixmap.height as i32);
        }
        if max_x <= min_x || max_y <= min_y {
            return Ok(ColorPixmap::new(0, 0));
        }
        let width = (max_x - min_x) as u32;
        let height = (max_y - min_y) as u32;
        let mut out = ColorPixmap::new(width, height);

        for m in &masks {
            if m.r.pixmap.is_empty() {
                continue;
            }
            let dx = (m.r.origin_x - min_x) as u32;
            let dy = (m.r.origin_y - min_y) as u32;
            blit_layer(&mut out, &m.r.pixmap, dx, dy, m.color);
        }
        Ok(out)
    }

    /// Rasterizes a COLRv1 paint-tree color glyph into a premultiplied
    /// RGBA [`ColorPixmap`].
    ///
    /// Walks the paint tree via `sigilbuzz-paint`'s evaluator, then
    /// composites every leaf paint (solid / linear / radial / sweep
    /// gradient), clipped through any enclosing `PaintGlyph` outline
    /// and blended through any `PaintComposite` mode, into a single
    /// surface sized to the union bounding box of every fill.
    ///
    /// `palette_index` selects the CPAL palette that solid fills and
    /// gradient stops resolve against. Unlike
    /// [`Rasterizer::rasterize_colrv0_glyph`], an index the font does
    /// not have is not an error: as in HarfBuzz, every palette entry
    /// then paints in the foreground color, as does an entry the palette
    /// lacks. Foreground (`0xFFFF`) entries render opaque black.
    ///
    /// # Errors
    /// - [`RenderError::ColrV1NotFound`] when the font has no v1
    ///   paint record for `gid`.
    /// - [`RenderError::BadSize`] when `size_pt` is non-finite or
    ///   non-positive.
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
        rasterize_colrv1(face, gid, palette_index, size_pt, coords, self.tolerance)
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
