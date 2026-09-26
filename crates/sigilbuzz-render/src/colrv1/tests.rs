//! Unit tests for the COLRv1 rasterizer's private helpers.

use super::*;
use alloc::vec;
use alloc::vec::Vec;

fn col(r: f32, g: f32, b: f32, a: f32) -> Color {
    Color::new(r, g, b, a)
}

fn red_to_blue() -> Vec<ColorStop> {
    vec![
        ColorStop::new(0.0, col(1.0, 0.0, 0.0, 1.0)),
        ColorStop::new(1.0, col(0.0, 0.0, 1.0, 1.0)),
    ]
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
fn rotation_point_turns_the_color_lines() {
    // p0 (0, 0), p1 (100, 0), p2 (100, 100): color lines run along
    // the diagonal, so the gradient runs toward (50, -50).
    let (a, b) = reduce_linear_anchors((0.0, 0.0), (100.0, 0.0), (100.0, 100.0));
    assert_eq!(a, (0.0, 0.0));
    assert!(
        (b.0 - 50.0).abs() < 1e-4 && (b.1 + 50.0).abs() < 1e-4,
        "{b:?}"
    );
    // Points on one diagonal share a color, and p1 is on the t = 1 line.
    let stops = red_to_blue();
    let kind = GradientKind::Linear {
        p0: (0.0, 0.0),
        p1: (100.0, 0.0),
        p2: (100.0, 100.0),
    };
    let at = |p| sample_gradient(kind, &stops, Extend::Pad, p);
    assert_eq!(at((10.0, 10.0)), at((40.0, 40.0)));
    assert_eq!(at((100.0, 0.0)), [0, 0, 255, 255]);
    assert_eq!(at((0.0, 0.0)), [255, 0, 0, 255]);
    // (50, 50) is on the t = 0 line through p0, so it is exactly
    // red. Ignoring p2 would put it half way to blue.
    assert_eq!(at((50.0, 50.0)), [255, 0, 0, 255]);
    // A rotation point on p0 leaves the gradient alone.
    let (_, b) = reduce_linear_anchors((0.0, 0.0), (100.0, 0.0), (0.0, 0.0));
    assert_eq!(b, (100.0, 0.0));
}

#[test]
fn sample_stops_interpolates_linearly() {
    let stops = red_to_blue();
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
fn porter_duff_src_over_matches_legacy() {
    // (255,0,0,255) over (0,0,255,255) = (255,0,0,255).
    let r = porter_duff(CompositeMode::SrcOver, [255, 0, 0, 255], [0, 0, 255, 255]);
    assert_eq!(r, [255, 0, 0, 255]);
}

#[test]
fn porter_duff_dest_in_masks_dest_by_src_alpha() {
    // src.a=128 (~50%), dst opaque red: result should be ~50% red.
    let r = porter_duff(CompositeMode::DestIn, [0, 0, 0, 128], [255, 0, 0, 255]);
    assert!(r[0] > 120 && r[0] < 132, "got {}", r[0]);
    assert_eq!(r[1], 0);
    assert!(r[3] > 120 && r[3] < 132);
}

#[test]
fn porter_duff_dest_out_clears_dest_where_src_opaque() {
    let r = porter_duff(CompositeMode::DestOut, [0, 0, 0, 255], [255, 255, 255, 255]);
    assert_eq!(r, [0, 0, 0, 0]);
}

#[test]
fn porter_duff_src_in_masks_src_by_dest_alpha() {
    let r = porter_duff(CompositeMode::SrcIn, [255, 0, 0, 255], [0, 0, 0, 128]);
    // src red * dst.a/255.
    assert!(r[0] > 120 && r[0] < 132);
    assert!(r[3] > 120 && r[3] < 132);
}

#[test]
fn porter_duff_src_out_keeps_src_where_dest_transparent() {
    let r = porter_duff(CompositeMode::SrcOut, [255, 0, 0, 255], [0, 0, 0, 0]);
    assert_eq!(r, [255, 0, 0, 255]);
}

#[test]
fn porter_duff_covers_the_remaining_operators() {
    let s = [255, 0, 0, 255];
    let d = [0, 0, 255, 255];
    let half = [0, 0, 128, 128];
    assert_eq!(porter_duff(CompositeMode::Clear, s, d), [0, 0, 0, 0]);
    assert_eq!(porter_duff(CompositeMode::Src, s, d), s);
    assert_eq!(porter_duff(CompositeMode::Dest, s, d), d);
    assert_eq!(porter_duff(CompositeMode::DestOver, s, d), d);
    assert_eq!(
        porter_duff(CompositeMode::DestOver, s, half),
        [127, 0, 128, 255]
    );
    assert_eq!(
        porter_duff(CompositeMode::SrcAtop, s, half),
        [128, 0, 0, 128]
    );
    assert_eq!(
        porter_duff(CompositeMode::DestAtop, s, half),
        [127, 0, 128, 255]
    );
    assert_eq!(porter_duff(CompositeMode::Xor, s, d), [0, 0, 0, 0]);
    assert_eq!(porter_duff(CompositeMode::Xor, s, half), [127, 0, 0, 127]);
    assert_eq!(porter_duff(CompositeMode::Plus, s, d), [255, 0, 255, 255]);
}

#[test]
fn blend_modes_fall_back_to_src_over() {
    let over = porter_duff(CompositeMode::SrcOver, [255, 0, 0, 255], [0, 0, 0, 0]);
    let multiply = porter_duff(CompositeMode::Multiply, [255, 0, 0, 255], [0, 0, 0, 0]);
    assert_eq!(over, multiply);
}

#[test]
fn radial_quadratic_two_circles() {
    // Two concentric circles, radii 0 and 10, centered on origin.
    // Sample at (5, 0). That's halfway between r=0 and r=10.
    let t = project_radial((0.0, 0.0), 0.0, (0.0, 0.0), 10.0, (5.0, 0.0));
    assert!((t.unwrap() - 0.5).abs() < 1e-3, "got {t:?}");
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

#[test]
fn sweep_is_counter_clockwise_in_paint_space() {
    let kind = GradientKind::Sweep {
        center: (0.0, 0.0),
        start_angle: 0.0,
        end_angle: core::f32::consts::PI,
    };
    let stops = red_to_blue();
    // (1, 1) is 45 degrees, a quarter of the way from red to blue.
    let p = sample_gradient(kind, &stops, Extend::Pad, (1.0, 1.0));
    assert!(p[0] > p[2], "{p:?}");
    // (1, -1) is past the end, padded blue.
    assert_eq!(
        sample_gradient(kind, &stops, Extend::Pad, (1.0, -1.0)),
        [0, 0, 255, 255]
    );
}

#[test]
fn gradient_with_no_stops_is_transparent() {
    let kind = GradientKind::Linear {
        p0: (0.0, 0.0),
        p1: (10.0, 0.0),
        p2: (0.0, 1.0),
    };
    assert_eq!(
        sample_gradient(kind, &[], Extend::Pad, (5.0, 0.0)),
        [0, 0, 0, 0]
    );
}

#[test]
fn linear_gradient_bands_follow_p2() {
    // p2 on the diagonal: bands run at 45 degrees, so (50, 50) sits
    // on the p0 band (red) and (100, 0) on the p1 band (blue).
    // Ignoring p2 would put (50, 50) halfway.
    let stops = red_to_blue();
    let kind = GradientKind::Linear {
        p0: (0.0, 0.0),
        p1: (100.0, 0.0),
        p2: (100.0, 100.0),
    };
    let at = |x, y| sample_gradient(kind, &stops, Extend::Pad, (x, y));
    assert_eq!(at(50.0, 50.0), [255, 0, 0, 255]);
    assert_eq!(at(100.0, 0.0), [0, 0, 255, 255]);
    let mid = at(25.0, -25.0);
    assert!(mid[0] > 100 && mid[2] > 100, "halfway mixes, got {mid:?}");
}

#[test]
fn linear_gradient_bands_stay_parallel_to_p2_under_skew() {
    // A horizontal skew keeps horizontal bands horizontal. With p2
    // along x from p0, every device point on y = 50 shares a color.
    // The raster sink samples in paint space through the inverse of
    // the device transform, so the test maps the points the same way.
    let stops = red_to_blue();
    let kind = GradientKind::Linear {
        p0: (0.0, 0.0),
        p1: (0.0, 100.0),
        p2: (100.0, 0.0),
    };
    let skew = sigilbuzz_paint::Transform2D {
        xx: 1.0,
        yx: 0.0,
        xy: 0.7,
        yy: 1.0,
        dx: 0.0,
        dy: 0.0,
    };
    let to_paint = skew.inverse().unwrap();
    let at = |x, y| sample_gradient(kind, &stops, Extend::Pad, to_paint.apply(x, y));
    assert_eq!(at(-40.0, 50.0), at(90.0, 50.0));
}
