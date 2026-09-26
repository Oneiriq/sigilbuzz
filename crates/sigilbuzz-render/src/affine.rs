//! 2D affine transforms.
//!
//! Pure-Rust 2x3 matrix supporting translate, scale, rotate, and
//! composition. Used to map glyph design-units into pixel space and to
//! pass per-layer transforms around inside the rasterizer.

/// A 2x3 affine transform stored column-major.
///
/// The mapping is:
/// ```text
///   x' = xx*x + xy*y + dx
///   y' = yx*x + yy*y + dy
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    /// Row 0, column 0.
    pub xx: f32,
    /// Row 1, column 0.
    pub yx: f32,
    /// Row 0, column 1.
    pub xy: f32,
    /// Row 1, column 1.
    pub yy: f32,
    /// X translation.
    pub dx: f32,
    /// Y translation.
    pub dy: f32,
}

impl Default for Affine {
    fn default() -> Self {
        Self::identity()
    }
}

impl Affine {
    /// The identity transform.
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            xx: 1.0,
            yx: 0.0,
            xy: 0.0,
            yy: 1.0,
            dx: 0.0,
            dy: 0.0,
        }
    }

    /// Pure translation.
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

    /// Pure (anisotropic) scale around the origin.
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

    /// Rotation by `angle_rad` radians around the origin.
    #[must_use]
    pub fn rotate(angle_rad: f32) -> Self {
        let c = angle_rad.cos();
        let s = angle_rad.sin();
        Self {
            xx: c,
            yx: s,
            xy: -s,
            yy: c,
            dx: 0.0,
            dy: 0.0,
        }
    }

    /// Returns `self * other`. Reading left-to-right, this applies
    /// `other` first and then `self`, the same convention HarfBuzz
    /// and Cairo use for nested COLR transforms.
    #[must_use]
    pub fn compose(&self, other: &Self) -> Self {
        Self {
            xx: self.xx * other.xx + self.xy * other.yx,
            yx: self.yx * other.xx + self.yy * other.yx,
            xy: self.xx * other.xy + self.xy * other.yy,
            yy: self.yx * other.xy + self.yy * other.yy,
            dx: self.xx * other.dx + self.xy * other.dy + self.dx,
            dy: self.yx * other.dx + self.yy * other.dy + self.dy,
        }
    }

    /// Applies the transform to a point.
    #[must_use]
    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.xx * x + self.xy * y + self.dx,
            self.yx * x + self.yy * y + self.dy,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    fn approx_pt(p: (f32, f32), q: (f32, f32)) -> bool {
        approx(p.0, q.0) && approx(p.1, q.1)
    }

    #[test]
    fn identity_is_a_no_op() {
        let m = Affine::identity();
        assert!(approx_pt(m.apply(3.0, 4.0), (3.0, 4.0)));
    }

    #[test]
    fn translate_offsets_points() {
        let m = Affine::translate(10.0, -5.0);
        assert!(approx_pt(m.apply(1.0, 2.0), (11.0, -3.0)));
    }

    #[test]
    fn scale_multiplies_axes() {
        let m = Affine::scale(2.0, 3.0);
        assert!(approx_pt(m.apply(4.0, 5.0), (8.0, 15.0)));
    }

    #[test]
    fn rotate_quarter_turn() {
        let m = Affine::rotate(core::f32::consts::FRAC_PI_2);
        let (x, y) = m.apply(1.0, 0.0);
        assert!(approx(x, 0.0));
        assert!(approx(y, 1.0));
    }

    #[test]
    fn compose_applies_other_first() {
        // compose(scale, translate) should translate THEN scale.
        let s = Affine::scale(2.0, 2.0);
        let t = Affine::translate(3.0, 4.0);
        let m = s.compose(&t);
        assert!(approx_pt(m.apply(0.0, 0.0), (6.0, 8.0)));
        assert!(approx_pt(m.apply(1.0, 1.0), (8.0, 10.0)));
    }

    #[test]
    fn compose_with_identity_is_identity_either_side() {
        let r = Affine::rotate(0.7);
        let i = Affine::identity();
        let lhs = r.compose(&i);
        let rhs = i.compose(&r);
        for (a, b) in [
            (lhs.xx, r.xx),
            (lhs.yx, r.yx),
            (lhs.xy, r.xy),
            (lhs.yy, r.yy),
            (lhs.dx, r.dx),
            (lhs.dy, r.dy),
            (rhs.xx, r.xx),
            (rhs.yx, r.yx),
            (rhs.xy, r.xy),
            (rhs.yy, r.yy),
            (rhs.dx, r.dx),
            (rhs.dy, r.dy),
        ] {
            assert!(approx(a, b), "got {a} expected {b}");
        }
    }

    #[test]
    fn default_is_identity() {
        let d = Affine::default();
        assert_eq!(d, Affine::identity());
    }
}
