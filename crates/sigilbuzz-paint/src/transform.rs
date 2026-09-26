//! 2x3 affine transform composition.
//!
//! All COLRv1 transforms compose into the same 6-tuple:
//!
//! ```text
//!   | xx  xy  dx |
//!   | yx  yy  dy |
//!   |  0   0   1 |
//! ```
//!
//! Following the convention used by CoreGraphics, Skia, and the SVG
//! `matrix(a b c d e f)` directive: `(xx, yx, xy, yy, dx, dy)`. This
//! also matches the on-disk layout of the COLRv1 `Affine2x3` record so
//! the conversion from a parsed `ColrPaint::Transform` is just a
//! field-by-field copy.
//!
//! A "child paint inherits the parent's transform", i.e. transforms
//! compose left-to-right as the walker descends the tree. The math is
//! the same as standard 3x3 matrix multiplication with the implicit
//! bottom row pinned to `[0, 0, 1]`; the helpers here are inlined and
//! avoid `glam` so the crate has no external math dependencies.

use core::f32::consts::PI;

/// 2x3 affine transform stored as `(xx, yx, xy, yy, dx, dy)`.
///
/// Applies to a point `(x, y)` as
/// `(xx * x + xy * y + dx, yx * x + yy * y + dy)`. Composition with
/// [`Transform2D::then`] applies `self` first, then `next`, the same
/// order a depth-first walk of a COLRv1 paint tree produces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform2D {
    /// Top-left matrix component.
    pub xx: f32,
    /// Bottom-left matrix component.
    pub yx: f32,
    /// Top-right matrix component.
    pub xy: f32,
    /// Bottom-right matrix component.
    pub yy: f32,
    /// X translation.
    pub dx: f32,
    /// Y translation.
    pub dy: f32,
}

impl Transform2D {
    /// Identity transform.
    pub const IDENTITY: Self = Self {
        xx: 1.0,
        yx: 0.0,
        xy: 0.0,
        yy: 1.0,
        dx: 0.0,
        dy: 0.0,
    };

    /// Pure translation by `(dx, dy)`.
    #[must_use]
    pub const fn translate(dx: f32, dy: f32) -> Self {
        Self {
            xx: 1.0,
            yx: 0.0,
            xy: 0.0,
            yy: 1.0,
            dx,
            dy,
        }
    }

    /// Non-uniform scale around the origin.
    #[must_use]
    pub const fn scale(sx: f32, sy: f32) -> Self {
        Self {
            xx: sx,
            yx: 0.0,
            xy: 0.0,
            yy: sy,
            dx: 0.0,
            dy: 0.0,
        }
    }

    /// Rotation by `radians` around the origin.
    #[must_use]
    pub fn rotate(radians: f32) -> Self {
        let c = radians.cos();
        let s = radians.sin();
        Self {
            xx: c,
            yx: s,
            xy: -s,
            yy: c,
            dx: 0.0,
            dy: 0.0,
        }
    }

    /// Skew (shear) by the given x and y angles in radians.
    #[must_use]
    pub fn skew(x_radians: f32, y_radians: f32) -> Self {
        Self {
            xx: 1.0,
            yx: y_radians.tan(),
            xy: -x_radians.tan(),
            yy: 1.0,
            dx: 0.0,
            dy: 0.0,
        }
    }

    /// Composition: returns `next * self` so that points transform as
    /// `self` first, then `next` (matching depth-first paint traversal).
    #[must_use]
    pub fn then(self, next: Self) -> Self {
        Self {
            xx: next.xx * self.xx + next.xy * self.yx,
            yx: next.yx * self.xx + next.yy * self.yx,
            xy: next.xx * self.xy + next.xy * self.yy,
            yy: next.yx * self.xy + next.yy * self.yy,
            dx: next.xx * self.dx + next.xy * self.dy + next.dx,
            dy: next.yx * self.dx + next.yy * self.dy + next.dy,
        }
    }

    /// Sandwich: `translate(cx, cy) * inner * translate(-cx, -cy)`.
    /// COLRv1 *AroundCenter variants apply a transform around a
    /// non-origin pivot; this is the canonical way to express that.
    #[must_use]
    pub fn around_center(self, cx: f32, cy: f32) -> Self {
        Transform2D::translate(-cx, -cy)
            .then(self)
            .then(Transform2D::translate(cx, cy))
    }

    /// Applies the transform to a point.
    #[must_use]
    pub fn apply(self, x: f32, y: f32) -> (f32, f32) {
        (
            self.xx * x + self.xy * y + self.dx,
            self.yx * x + self.yy * y + self.dy,
        )
    }
}

impl Default for Transform2D {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Converts a COLRv1 F2DOT14 angle into radians.
///
/// COLRv1 stores angles as F2DOT14 multiples of 180 degrees, i.e. an
/// on-disk value of 1.0 means a half-turn. sigilbuzz already converts
/// the F2DOT14 to a fraction; this helper finishes the trip into
/// radians by multiplying by `pi`.
#[must_use]
pub(crate) fn angle_to_radians(f2dot14_angle: f32) -> f32 {
    f2dot14_angle * PI
}

/// Converts a `PaintSweepGradient` start or end angle into radians.
///
/// Sweep angles carry a bias of one half-turn: the stored F2DOT14
/// value plus 1.0 is the angle in multiples of 180 degrees, so the
/// `[-2, 2)` F2DOT14 range covers -180 to 540 degrees and a full turn
/// can be encoded. fontTools reads these fields as `BiasedAngle`, and
/// HarfBuzz hands `(angle + 1) * pi` to its sweep-gradient callback.
#[must_use]
pub(crate) fn sweep_angle_to_radians(f2dot14_angle: f32) -> f32 {
    (f2dot14_angle + 1.0) * PI
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_round_trip() {
        let t = Transform2D::IDENTITY;
        assert_eq!(t.apply(3.0, 5.0), (3.0, 5.0));
    }

    #[test]
    fn translate_then_scale_matches_compose() {
        // Apply the moves separately, then via `.then`, and confirm the
        // results agree on a sample point.
        let t = Transform2D::translate(2.0, -3.0);
        let s = Transform2D::scale(2.0, 4.0);
        let composed = t.then(s);
        let p = (1.0_f32, 1.0_f32);
        let staged = {
            let (x, y) = t.apply(p.0, p.1);
            s.apply(x, y)
        };
        let direct = composed.apply(p.0, p.1);
        assert!((staged.0 - direct.0).abs() < 1e-5);
        assert!((staged.1 - direct.1).abs() < 1e-5);
    }

    #[test]
    fn order_matters() {
        // Translate-then-scale and scale-then-translate diverge on the
        // translation: pre-scale translation gets multiplied by the
        // scale factor.
        let t = Transform2D::translate(1.0, 0.0);
        let s = Transform2D::scale(10.0, 10.0);
        let ts = t.then(s).apply(0.0, 0.0);
        let st = s.then(t).apply(0.0, 0.0);
        assert!((ts.0 - 10.0).abs() < 1e-5);
        assert!((st.0 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn rotate_around_center_pins_pivot() {
        // A 180-degree rotation around (5, 5) leaves (5, 5) untouched.
        let r = Transform2D::rotate(PI);
        let m = r.around_center(5.0, 5.0);
        let (x, y) = m.apply(5.0, 5.0);
        assert!((x - 5.0).abs() < 1e-4);
        assert!((y - 5.0).abs() < 1e-4);
    }

    #[test]
    fn skew_preserves_origin() {
        let m = Transform2D::skew(0.3, 0.0);
        let (x, y) = m.apply(0.0, 0.0);
        assert!((x).abs() < 1e-6);
        assert!((y).abs() < 1e-6);
    }

    #[test]
    fn angle_conversion_matches_spec() {
        // F2DOT14 angle of 1.0 means pi radians per the COLRv1 spec.
        assert!((angle_to_radians(1.0) - PI).abs() < 1e-6);
        assert!((angle_to_radians(0.5) - PI / 2.0).abs() < 1e-6);
    }

    #[test]
    fn sweep_angles_carry_a_half_turn_bias() {
        // Stored -1.0 is 0 degrees, 0.0 is 180, 1.0 is a full turn.
        assert_eq!(sweep_angle_to_radians(-1.0), 0.0);
        assert_eq!(sweep_angle_to_radians(0.0), PI);
        assert_eq!(sweep_angle_to_radians(1.0), 2.0 * PI);
        // Same float product HarfBuzz computes: (a + 1) * pi.
        assert_eq!(sweep_angle_to_radians(0.25), (0.25_f32 + 1.0) * PI);
    }
}
