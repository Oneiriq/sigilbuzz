//! The rectangle `hb_font_paint_glyph` clips a COLRv1 glyph to, at
//! font scale.
//!
//! HarfBuzz pushes it before the root transform, so it is in font
//! units:
//!
//! - A ClipList box goes through `hb_font_t::scale_glyph_extents`, which
//!   scales each edge with the font's 16.16 integer multiplier and
//!   rounds to a whole unit (`em_scale`), then snaps the result outward
//!   with `floor` / `ceil`, a no-op on integers.
//! - Bounds computed from the paint tree are already at font scale in
//!   HarfBuzz, as floats. sigilbuzz computes them in design units, so
//!   they are scaled by the root transform here.

use sigilbuzz_paint::walk::RootClip;

/// Font scale state the clip rectangle depends on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Scale {
    /// Units per em as HarfBuzz reads it.
    pub(crate) upem: u16,
    pub(crate) x_scale: i32,
    pub(crate) y_scale: i32,
}

impl Scale {
    /// `(x_min, y_min, x_max, y_max)` in font units for `clip`.
    pub(crate) fn rect(self, clip: RootClip) -> [f32; 4] {
        match clip {
            RootClip::ClipBox {
                x_min,
                y_min,
                x_max,
                y_max,
            } => {
                let (xm, ym) = (mult(self.x_scale, self.upem), mult(self.y_scale, self.upem));
                [
                    em_mult(x_min, xm),
                    em_mult(y_min, ym),
                    em_mult(x_max, xm),
                    em_mult(y_max, ym),
                ]
                .map(|v| v as f32)
            }
            RootClip::Extents {
                x_min,
                y_min,
                x_max,
                y_max,
                ..
            } => {
                let upem = f32::from(self.upem);
                let (sx, sy) = (self.x_scale as f32 / upem, self.y_scale as f32 / upem);
                let (x0, x1) = ordered(x_min * sx, x_max * sx, sx);
                let (y0, y1) = ordered(y_min * sy, y_max * sy, sy);
                [x0, y0, x1, y1]
            }
        }
    }
}

/// HarfBuzz's `x_mult` / `y_mult`: the scale over upem in 16.16.
fn mult(scale: i32, upem: u16) -> i64 {
    let upem = i64::from(upem.max(1));
    let scale = i64::from(scale);
    if scale < 0 {
        -((-scale) << 16) / upem
    } else {
        (scale << 16) / upem
    }
}

/// HarfBuzz's `em_mult`: `(v * mult + 32768) >> 16` on an `int16_t`
/// design value.
fn em_mult(v: i32, mult: i64) -> i32 {
    let v = i64::from(v as i16);
    ((v * mult + 32768) >> 16) as i32
}

/// Keeps a scaled pair's order when the scale mirrors the axis.
fn ordered(a: f32, b: f32, scale: f32) -> (f32, f32) {
    if scale < 0.0 {
        (b, a)
    } else {
        (a, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip_box(x_min: i32, y_min: i32, x_max: i32, y_max: i32) -> RootClip {
        RootClip::ClipBox {
            x_min,
            y_min,
            x_max,
            y_max,
        }
    }

    #[test]
    fn clip_boxes_scale_with_integer_rounding() {
        let unit = Scale {
            upem: 1000,
            x_scale: 1000,
            y_scale: 1000,
        };
        assert_eq!(
            unit.rect(clip_box(-10, 0, 500, 800)),
            [-10.0, 0.0, 500.0, 800.0]
        );
        // 1000 upem at 3 units per em: the 16.16 multiplier truncates
        // to 196 / 65536, so 333 -> 0.996 rounds to 1 but 500 -> 1.495
        // rounds to 1 as well.
        let small = Scale {
            upem: 1000,
            x_scale: 3,
            y_scale: 3,
        };
        assert_eq!(
            small.rect(clip_box(333, -333, 500, 1000)),
            [1.0, -1.0, 1.0, 3.0]
        );
        // A mirrored scale keeps HarfBuzz's edge order.
        let flipped = Scale {
            upem: 1000,
            x_scale: -2000,
            y_scale: 2000,
        };
        assert_eq!(
            flipped.rect(clip_box(0, 0, 10, 10)),
            [0.0, 0.0, -20.0, 20.0]
        );
    }

    #[test]
    fn computed_extents_scale_by_the_root_transform() {
        let scale = Scale {
            upem: 1000,
            x_scale: 2000,
            y_scale: -500,
        };
        let extents = RootClip::Extents {
            x_min: 1.5,
            y_min: -10.0,
            x_max: 20.0,
            y_max: 40.0,
            bounded: true,
        };
        assert_eq!(scale.rect(extents), [3.0, -20.0, 40.0, 5.0]);
    }
}
