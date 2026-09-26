//! Unit tests for the COLRv1 rasterizer's private helpers.

use super::*;
use alloc::vec;
use sigilbuzz_paint::{ColorStop, GradientKind};

fn col(r: f32, g: f32, b: f32, a: f32) -> Color {
    Color::new(r, g, b, a)
}

#[test]
fn extend_pad_clamps() {
    assert!((apply_extend(-0.5, Extend::Pad) - 0.0).abs() < 1e-6);
    assert!((apply_extend(1.5, Extend::Pad) - 1.0).abs() < 1e-6);
    assert!((apply_extend(0.5, Extend::Pad) - 0.5).abs() < 1e-6);
}

#[test]
fn extend_repeat_wraps() {
    assert!((apply_extend(1.25, Extend::Repeat) - 0.25).abs() < 1e-6);
    assert!((apply_extend(-0.25, Extend::Repeat) - 0.75).abs() < 1e-6);
}

#[test]
fn extend_reflect_bounces() {
    // 1.25 -> 0.75 (reflected past 1)
    assert!((apply_extend(1.25, Extend::Reflect) - 0.75).abs() < 1e-6);
    // 2.25 -> 0.25 (full cycle + a bit)
    assert!((apply_extend(2.25, Extend::Reflect) - 0.25).abs() < 1e-6);
}

#[test]
fn linear_projection_endpoints() {
    let a = (0.0, 0.0);
    let b = (10.0, 0.0);
    assert!((project_linear(a, b, (0.0, 0.0)).unwrap() - 0.0).abs() < 1e-6);
    assert!((project_linear(a, b, (10.0, 0.0)).unwrap() - 1.0).abs() < 1e-6);
    assert!((project_linear(a, b, (5.0, 0.0)).unwrap() - 0.5).abs() < 1e-6);
    // Off-axis: projects to the foot of the perpendicular.
    assert!((project_linear(a, b, (5.0, 50.0)).unwrap() - 0.5).abs() < 1e-6);
}

#[test]
fn linear_projection_zero_length_is_none() {
    assert!(project_linear((1.0, 1.0), (1.0, 1.0), (0.0, 0.0)).is_none());
}

#[test]
fn sample_stops_interpolates_linearly() {
    let stops = [
        ColorStop {
            offset: 0.0,
            color: col(1.0, 0.0, 0.0, 1.0),
        },
        ColorStop {
            offset: 1.0,
            color: col(0.0, 0.0, 1.0, 1.0),
        },
    ];
    let mid = sample_stops(&stops, 0.5);
    assert!((mid.r - 0.5).abs() < 1e-6);
    assert!((mid.b - 0.5).abs() < 1e-6);
    // Below first stop clamps to first.
    let lo = sample_stops(&stops, -1.0);
    assert!((lo.r - 1.0).abs() < 1e-6);
    // Above last stop clamps to last.
    let hi = sample_stops(&stops, 2.0);
    assert!((hi.b - 1.0).abs() < 1e-6);
}

#[test]
fn to_premul_multiplies_channels() {
    let p = to_premul(col(1.0, 1.0, 1.0, 0.5));
    // 1.0 * 0.5 ~ 128 (rounded from 127.5).
    assert!(p[0] >= 127 && p[0] <= 128);
    assert_eq!(p[3], 128);
}

#[test]
fn blend_src_over_full_alpha_replaces() {
    let mut dst = ColorPixmap::new(1, 1);
    blend_src_over(&mut dst, 0, 0, [255, 0, 0, 255]);
    assert_eq!(dst.get(0, 0), [255, 0, 0, 255]);
}

#[test]
fn blend_src_over_zero_alpha_is_noop() {
    let mut dst = ColorPixmap::new(1, 1);
    // Pre-fill so we can detect any clobber.
    dst.data = vec![10, 20, 30, 40];
    blend_src_over(&mut dst, 0, 0, [0, 0, 0, 0]);
    assert_eq!(dst.data, vec![10, 20, 30, 40]);
}

#[test]
fn porter_duff_src_over_matches_legacy() {
    // (255,0,0,255) over (0,0,255,255) = (255,0,0,255).
    let r = porter_duff(CompositeMode::SrcOver, 255, 0, 0, 255, 0, 0, 255, 255);
    assert_eq!(r, (255, 0, 0, 255));
}

#[test]
fn porter_duff_dest_in_masks_dest_by_src_alpha() {
    // src.a=128 (~50%), dst opaque red: result should be ~50% red.
    let r = porter_duff(CompositeMode::DestIn, 0, 0, 0, 128, 255, 0, 0, 255);
    assert!(r.0 > 120 && r.0 < 132, "got {}", r.0);
    assert_eq!(r.1, 0);
    assert!(r.3 > 120 && r.3 < 132);
}

#[test]
fn porter_duff_dest_out_clears_dest_where_src_opaque() {
    let r = porter_duff(CompositeMode::DestOut, 0, 0, 0, 255, 255, 255, 255, 255);
    assert_eq!(r, (0, 0, 0, 0));
}

#[test]
fn porter_duff_src_in_masks_src_by_dest_alpha() {
    let r = porter_duff(CompositeMode::SrcIn, 255, 0, 0, 255, 0, 0, 0, 128);
    // src red * dst.a/255.
    assert!(r.0 > 120 && r.0 < 132);
    assert!(r.3 > 120 && r.3 < 132);
}

#[test]
fn porter_duff_src_out_keeps_src_where_dest_transparent() {
    let r = porter_duff(CompositeMode::SrcOut, 255, 0, 0, 255, 0, 0, 0, 0);
    assert_eq!(r, (255, 0, 0, 255));
}

#[test]
fn unsupported_mode_falls_back_to_src_over() {
    let r1 = porter_duff(CompositeMode::SrcOver, 255, 0, 0, 255, 0, 0, 0, 0);
    let r2 = porter_duff(CompositeMode::Multiply, 255, 0, 0, 255, 0, 0, 0, 0);
    assert_eq!(r1, r2);
}

#[test]
fn radial_quadratic_two_circles() {
    // Two concentric circles, radii 0 and 10, centered on origin.
    // Sample at (5, 0). That's halfway between r=0 and r=10.
    let t = project_radial((0.0, 0.0), 0.0, (0.0, 0.0), 10.0, (5.0, 0.0));
    assert!(t.is_some());
    let t = t.unwrap();
    assert!((t - 0.5).abs() < 1e-3, "got {t}");
}

#[test]
fn sweep_basic_quadrants() {
    let c = (0.0, 0.0);
    // Sweep from 0 to 2pi: angle 0 -> t=0, angle pi -> t=0.5.
    let t = project_sweep(c, 0.0, core::f32::consts::TAU, (1.0, 0.0));
    assert!((t.unwrap() - 0.0).abs() < 1e-3);
    let t = project_sweep(c, 0.0, core::f32::consts::TAU, (-1.0, 0.0));
    assert!((t.unwrap() - 0.5).abs() < 1e-3);
}

/// Red at `t = 0`, blue at `t = 1`, padded.
fn red_to_blue(p0: (f32, f32), p1: (f32, f32), p2: (f32, f32)) -> Gradient {
    Gradient {
        kind: GradientKind::Linear { p0, p1, p2 },
        stops: alloc::vec![
            sigilbuzz_paint::ColorStop {
                offset: 0.0,
                color: Color::new(1.0, 0.0, 0.0, 1.0),
            },
            sigilbuzz_paint::ColorStop {
                offset: 1.0,
                color: Color::new(0.0, 0.0, 1.0, 1.0),
            },
        ],
        extend: Extend::Pad,
    }
}

#[test]
fn linear_gradient_bands_follow_p2() {
    // p2 on the diagonal: bands run at 45 degrees, so (50, 50) sits
    // on the p0 band (red) and (100, 0) on the p1 band (blue).
    // Ignoring p2 would put (50, 50) halfway.
    let g = red_to_blue((0.0, 0.0), (100.0, 0.0), (100.0, 100.0));
    let at = |x, y| sample_gradient(&g, Transform2D::IDENTITY, x, y);
    assert_eq!(at(50.0, 50.0), [255, 0, 0, 255]);
    assert_eq!(at(100.0, 0.0), [0, 0, 255, 255]);
    let mid = at(25.0, -25.0);
    assert!(mid[0] > 100 && mid[2] > 100, "halfway mixes, got {mid:?}");
}

#[test]
fn linear_gradient_bands_stay_parallel_to_p2_under_skew() {
    // A horizontal skew keeps horizontal bands horizontal. With p2
    // straight up from p0, every point on y = 50 shares a color.
    let g = red_to_blue((0.0, 0.0), (0.0, 100.0), (100.0, 0.0));
    let skew = Transform2D {
        xx: 1.0,
        yx: 0.0,
        xy: 0.7,
        yy: 1.0,
        dx: 0.0,
        dy: 0.0,
    };
    let left = sample_gradient(&g, skew, -40.0, 50.0);
    let right = sample_gradient(&g, skew, 90.0, 50.0);
    assert_eq!(left, right);
}

#[test]
fn gradient_with_no_stops_is_transparent() {
    let g = Gradient {
        kind: GradientKind::Linear {
            p0: (0.0, 0.0),
            p1: (10.0, 0.0),
            p2: (0.0, 1.0),
        },
        stops: Vec::new(),
        extend: Extend::Pad,
    };
    let p = sample_gradient(&g, Transform2D::IDENTITY, 5.0, 0.0);
    assert_eq!(p, [0, 0, 0, 0]);
}
