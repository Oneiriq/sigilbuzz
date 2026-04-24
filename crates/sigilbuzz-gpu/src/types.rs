//! Plain-old-data types that make up an encoded Slug glyph.
//!
//! These structs are deliberately `repr`-friendly: every field is a
//! primitive `f32` / `u32`, every aggregate is `Copy` where possible.
//! Consumers can transmute / cast `&[QuadSegment]` and `&[Band]` to
//! byte slices for SSBO upload without further marshalling.

use alloc::vec::Vec;

/// A 2-D point in font design units (the same coordinate space the
/// upstream [`sigilbuzz::tables::PathOp`] stream uses).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct Vec2 {
    /// X coordinate.
    pub x: f32,
    /// Y coordinate.
    pub y: f32,
}

impl Vec2 {
    /// Constructs a [`Vec2`] from an `(x, y)` pair.
    #[must_use]
    #[inline]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Axis-aligned bounding box in design units.
///
/// `xmin <= xmax` and `ymin <= ymax` for a non-empty box; an empty
/// glyph (all whitespace, or no contours) reports `xmin > xmax`.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct Bbox {
    /// Minimum X.
    pub xmin: f32,
    /// Minimum Y.
    pub ymin: f32,
    /// Maximum X.
    pub xmax: f32,
    /// Maximum Y.
    pub ymax: f32,
}

impl Bbox {
    /// Returns an empty bbox (positive infinity / negative infinity).
    /// Used as the seed of an "expand by point" loop.
    #[must_use]
    #[inline]
    pub const fn empty() -> Self {
        Self {
            xmin: f32::INFINITY,
            ymin: f32::INFINITY,
            xmax: f32::NEG_INFINITY,
            ymax: f32::NEG_INFINITY,
        }
    }

    /// Width of the box. Negative when [`Bbox::empty`].
    #[must_use]
    #[inline]
    pub fn width(&self) -> f32 {
        self.xmax - self.xmin
    }

    /// Height of the box. Negative when [`Bbox::empty`].
    #[must_use]
    #[inline]
    pub fn height(&self) -> f32 {
        self.ymax - self.ymin
    }

    /// True when the box has zero or negative extent on either axis,
    /// the typical signal that the glyph carries no rasterisable
    /// outline.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        !(self.xmax > self.xmin && self.ymax > self.ymin)
    }

    /// Expands this box to include `(x, y)`.
    #[inline]
    pub fn expand(&mut self, x: f32, y: f32) {
        if x < self.xmin {
            self.xmin = x;
        }
        if y < self.ymin {
            self.ymin = y;
        }
        if x > self.xmax {
            self.xmax = x;
        }
        if y > self.ymax {
            self.ymax = y;
        }
    }
}

impl Default for Bbox {
    fn default() -> Self {
        Self::empty()
    }
}

/// A single quadratic Bezier segment of a glyph contour.
///
/// All Slug primitives are quadratic — cubic curves from CFF fonts
/// are flattened upstream by [`crate::encode_glyph`] before band
/// decomposition.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct QuadSegment {
    /// Start point.
    pub p0: Vec2,
    /// Control point.
    pub p1: Vec2,
    /// End point.
    pub p2: Vec2,
}

/// One horizontal band's slice into the segment pool.
///
/// `segment_offset` indexes into [`SlugGlyph::segments`];
/// `segment_count` is the number of consecutive segments that
/// participate in this band's coverage. Bands are stored in y-order
/// from `ymin` upward.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct Band {
    /// First segment in [`SlugGlyph::segments`].
    pub segment_offset: u32,
    /// Number of segments owned by this band.
    pub segment_count: u32,
}

/// A glyph encoded for GPU rasterisation.
///
/// Layout note: the segment pool is *not* deduplicated across bands.
/// A curve crossing several bands is recorded once per band so the
/// fragment shader can iterate the band's slice without touching
/// any out-of-band data. This trades memory for shader simplicity —
/// it is the standard Slug layout.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SlugGlyph {
    /// Design-unit bounding box of the encoded glyph.
    pub bbox: Bbox,
    /// Horizontal bands tiling the bbox y-range.
    pub bands: Vec<Band>,
    /// Pool of quadratic segments referenced by the bands.
    pub segments: Vec<QuadSegment>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbox_expand_grows_extents() {
        let mut b = Bbox::empty();
        assert!(b.is_empty());
        b.expand(1.0, 2.0);
        b.expand(-3.0, 5.0);
        b.expand(7.0, 4.0);
        assert_eq!(b.xmin, -3.0);
        assert_eq!(b.ymin, 2.0);
        assert_eq!(b.xmax, 7.0);
        assert_eq!(b.ymax, 5.0);
        assert!((b.width() - 10.0).abs() < 1e-6);
        assert!((b.height() - 3.0).abs() < 1e-6);
    }

    #[test]
    fn empty_bbox_reports_empty() {
        let b = Bbox::empty();
        assert!(b.is_empty());
        let mut b = b;
        b.expand(0.0, 0.0); // single point => still empty (no extent)
        assert!(b.is_empty());
    }
}
