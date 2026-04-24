//! Outline iteration primitives.
//!
//! A glyph outline is a stream of drawing operations: move-to, line-to,
//! quadratic / cubic Bezier curves, close. Downstream renderers, GPU
//! encoders (Slug), and colour-glyph evaluators (COLRv1) all consume
//! the same shape. sigilbuzz exposes that stream through the
//! [`PathOp`] enum and the [`Outline`] type.
//!
//! TrueType glyphs produce [`PathOp::QuadTo`]; CFF charstrings produce
//! [`PathOp::CubicTo`]. Both backends flatten composites / subroutines
//! so callers never see those details.

use alloc::vec::Vec;

/// One drawing operation in a glyph outline.
///
/// Coordinates are in font design units as `f32`. Rendering pipelines
/// scale to device space using the font's `unitsPerEm`; variable-font
/// deltas are already folded in when the outline was produced via
/// `glyph_outline_at_coords`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathOp {
    /// Begin a new contour at `(x, y)` without drawing.
    MoveTo {
        /// X coordinate.
        x: f32,
        /// Y coordinate.
        y: f32,
    },
    /// Draw a straight line from the current point to `(x, y)`.
    LineTo {
        /// X coordinate of the line endpoint.
        x: f32,
        /// Y coordinate of the line endpoint.
        y: f32,
    },
    /// Draw a quadratic Bezier with control point `(cx, cy)` and
    /// endpoint `(x, y)`. Emitted by the TrueType `glyf` backend.
    QuadTo {
        /// Control point X.
        cx: f32,
        /// Control point Y.
        cy: f32,
        /// Endpoint X.
        x: f32,
        /// Endpoint Y.
        y: f32,
    },
    /// Draw a cubic Bezier with control points `(c1x, c1y)` and
    /// `(c2x, c2y)` and endpoint `(x, y)`. Emitted by the CFF
    /// backend.
    CubicTo {
        /// First control point X.
        c1x: f32,
        /// First control point Y.
        c1y: f32,
        /// Second control point X.
        c2x: f32,
        /// Second control point Y.
        c2y: f32,
        /// Endpoint X.
        x: f32,
        /// Endpoint Y.
        y: f32,
    },
    /// Close the current contour back to its [`PathOp::MoveTo`].
    Close,
}

/// A glyph outline as a flat list of [`PathOp`]s.
///
/// Produced by [`crate::Face::glyph_outline`] and
/// [`crate::Face::glyph_outline_at_coords`]. The ops are stored in a
/// small owned buffer so the outline can outlive the borrowed font
/// bytes; the sigilbuzz core never allocates during shaping, only
/// during explicit outline extraction.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outline {
    ops: Vec<PathOp>,
}

impl Outline {
    /// Creates an empty outline.
    #[must_use]
    pub const fn new() -> Self {
        Self { ops: Vec::new() }
    }

    /// Wraps a pre-built op list.
    #[must_use]
    pub fn from_ops(ops: Vec<PathOp>) -> Self {
        Self { ops }
    }

    /// Returns the full op sequence.
    #[must_use]
    pub fn ops(&self) -> &[PathOp] {
        &self.ops
    }

    /// Number of recorded ops. An empty outline is the convention
    /// for whitespace glyphs and invisible glyph substitutions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// True when no ops were recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Appends a single op. Intended for backend construction.
    pub fn push(&mut self, op: PathOp) {
        self.ops.push(op);
    }
}

/// Sink for streaming outline construction. Backends (glyf, CFF)
/// drive one of these and the caller materialises the result into an
/// [`Outline`]. The sink abstraction keeps the parity-test path
/// simple — a test builder can implement `OutlineSink` and record
/// op-by-op without allocating an intermediate `Outline`.
pub trait OutlineSink {
    /// Records a move-to.
    fn move_to(&mut self, x: f32, y: f32);
    /// Records a line-to.
    fn line_to(&mut self, x: f32, y: f32);
    /// Records a quadratic curve-to.
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32);
    /// Records a cubic curve-to.
    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32);
    /// Records a close.
    fn close(&mut self);
}

impl OutlineSink for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.ops.push(PathOp::MoveTo { x, y });
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.ops.push(PathOp::LineTo { x, y });
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.ops.push(PathOp::QuadTo { cx, cy, x, y });
    }
    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.ops.push(PathOp::CubicTo {
            c1x,
            c1y,
            c2x,
            c2y,
            x,
            y,
        });
    }
    fn close(&mut self) {
        self.ops.push(PathOp::Close);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outline_sink_records_ops_in_order() {
        let mut o = Outline::new();
        o.move_to(1.0, 2.0);
        o.line_to(3.0, 4.0);
        o.quad_to(5.0, 6.0, 7.0, 8.0);
        o.curve_to(9.0, 10.0, 11.0, 12.0, 13.0, 14.0);
        o.close();
        assert_eq!(o.len(), 5);
        assert!(matches!(o.ops()[0], PathOp::MoveTo { x: 1.0, y: 2.0 }));
        assert!(matches!(o.ops()[4], PathOp::Close));
    }

    #[test]
    fn outline_default_is_empty() {
        let o = Outline::default();
        assert!(o.is_empty());
    }
}
