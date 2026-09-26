//! Resolved gradient descriptors emitted by the evaluator.
//!
//! sigilbuzz exposes the COLRv1 color line as palette indices + raw
//! coordinates. The evaluator turns those into the float-channel
//! gradients consumers actually want: palette entries already
//! resolved, alpha already multiplied, geometry already transformed
//! through the active design-unit space (transform composition is the
//! consumer's job since they may want to defer it for hardware-driven
//! pipelines, so the gradient still ships in the pre-transform paint
//! frame and the matching `Transform2D` is part of `DrawCmd`).

use alloc::vec::Vec;

use crate::color::Color;

/// Resolved color stop. Position along the color line plus the
/// resolved RGBA. The renderer needs no further palette lookups.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorStop {
    /// Stop offset along the gradient axis. Typically in `[0.0, 1.0]`
    /// but the spec allows out-of-range values for `Repeat` / `Reflect`
    /// extends.
    pub offset: f32,
    /// Resolved color with the per-stop alpha already folded in. For a
    /// foreground stop this is the evaluation's foreground color (see
    /// [`crate::EvalOptions::with_foreground`]) with the stop alpha
    /// applied.
    pub color: Color,
    /// True when the stop used COLR palette entry `0xFFFF`, the
    /// foreground (text) color. A renderer with its own text color can
    /// substitute it here, keeping `color.a` relative to the evaluation
    /// foreground's alpha.
    pub is_foreground: bool,
}

impl ColorStop {
    /// A stop with an ordinary (non-foreground) color.
    ///
    /// ```
    /// use sigilbuzz_paint::{Color, ColorStop};
    ///
    /// let stop = ColorStop::new(0.5, Color::new(1.0, 0.0, 0.0, 1.0));
    /// assert_eq!(stop.offset, 0.5);
    /// assert!(!stop.is_foreground);
    /// ```
    #[must_use]
    pub const fn new(offset: f32, color: Color) -> Self {
        Self {
            offset,
            color,
            is_foreground: false,
        }
    }
}

/// Extend-mode mirrors [`sigilbuzz::tables::colr::Extend`] but lives
/// in the evaluator crate's surface so consumers don't need to reach
/// into sigilbuzz's table layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extend {
    /// Replicate the first / last color outward.
    Pad,
    /// Tile the gradient by repeating it.
    Repeat,
    /// Tile the gradient with every other tile reflected.
    Reflect,
}

impl From<sigilbuzz::tables::colr::Extend> for Extend {
    fn from(v: sigilbuzz::tables::colr::Extend) -> Self {
        match v {
            sigilbuzz::tables::colr::Extend::Pad => Self::Pad,
            sigilbuzz::tables::colr::Extend::Repeat => Self::Repeat,
            sigilbuzz::tables::colr::Extend::Reflect => Self::Reflect,
        }
    }
}

/// Discriminated union over the three COLRv1 gradient shapes.
///
/// Coordinates are in font design units; the renderer applies the
/// `Transform2D` that ships in the surrounding [`crate::DrawCmd`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GradientKind {
    /// Linear gradient between two endpoints. The third point in the
    /// COLRv1 record (`p2`) anchors the gradient line's rotation; the
    /// evaluator has already folded it into `p0` / `p1`.
    Linear {
        /// Start point.
        p0: (f32, f32),
        /// End point.
        p1: (f32, f32),
        /// Rotation anchor. Renderers using the projected-line
        /// formulation can ignore it; renderers using the spec's
        /// "rotate the line through p2" formulation need it.
        p2: (f32, f32),
    },
    /// Two-circle radial gradient. `t = 0` rides the inner circle,
    /// `t = 1` the outer.
    Radial {
        /// Inner circle center.
        c0: (f32, f32),
        /// Inner circle radius.
        r0: f32,
        /// Outer circle center.
        c1: (f32, f32),
        /// Outer circle radius.
        r1: f32,
    },
    /// Sweep (conic) gradient around `center`. Angles in radians; `0`
    /// is the +x axis, increasing counter-clockwise.
    Sweep {
        /// Center of the sweep.
        center: (f32, f32),
        /// Start angle in radians.
        start_angle: f32,
        /// End angle in radians.
        end_angle: f32,
    },
}

/// Resolved gradient: shape + stops + extend mode.
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    /// Geometry of the gradient.
    pub kind: GradientKind,
    /// Resolved stops in input order.
    pub stops: Vec<ColorStop>,
    /// Extend behavior beyond `[0, 1]`.
    pub extend: Extend,
}
