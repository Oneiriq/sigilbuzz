//! Unit tests for curve flattening, grouped flattening, and the
//! arc-length estimators.

use super::arc_length::cubic_prefix_length;
use super::*;

#[test]
fn lines_pass_through_unchanged() {
    let ops = [
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 10.0, y: 0.0 },
        PathOp::LineTo { x: 10.0, y: 10.0 },
        PathOp::Close,
    ];
    let segs = flatten(ops, &Affine::identity(), 0.25);
    assert_eq!(segs.len(), 3);
    assert!((segs[0].x0 - 0.0).abs() < 1e-5);
    assert!((segs[2].x1 - 0.0).abs() < 1e-5);
}

#[test]
fn affine_applied_to_segments() {
    let ops = [
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 1.0, y: 0.0 },
    ];
    let xf = Affine::scale(10.0, 10.0);
    let segs = flatten(ops, &xf, 0.25);
    assert!((segs[0].x1 - 10.0).abs() < 1e-5);
}

#[test]
fn quadratic_subdivides_to_chord_when_tight() {
    // Big arc forces several subdivisions.
    let ops = [
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::QuadTo {
            cx: 50.0,
            cy: 100.0,
            x: 100.0,
            y: 0.0,
        },
    ];
    let segs = flatten(ops, &Affine::identity(), 0.25);
    assert!(
        segs.len() > 8,
        "expected adaptive subdivision, got {}",
        segs.len()
    );
    // First segment starts at (0,0).
    assert!((segs[0].x0).abs() < 1e-5);
    // Last segment ends at (100,0).
    assert!((segs[segs.len() - 1].x1 - 100.0).abs() < 1e-5);
}

#[test]
fn cubic_subdivides() {
    let ops = [
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::CubicTo {
            c1x: 0.0,
            c1y: 100.0,
            c2x: 100.0,
            c2y: 100.0,
            x: 100.0,
            y: 0.0,
        },
    ];
    let segs = flatten(ops, &Affine::identity(), 0.25);
    assert!(segs.len() > 4);
}

#[test]
fn close_emits_terminator_segment() {
    let ops = [
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 10.0, y: 0.0 },
        PathOp::LineTo { x: 10.0, y: 10.0 },
        PathOp::Close,
    ];
    let segs = flatten(ops, &Affine::identity(), 0.25);
    // Three explicit edges including the close.
    assert_eq!(segs.len(), 3);
    let last = segs[2];
    assert!((last.x1).abs() < 1e-5 && (last.y1).abs() < 1e-5);
}

// -------- flatten_grouped --------

/// Helper: flatten the per-curve segment lists back to a single
/// flat `Vec<Segment>` so we can cross-check against `flatten()`.
fn ungroup(curves: &[FlattenedCurve]) -> Vec<Segment> {
    let mut out = Vec::new();
    for c in curves {
        match c {
            FlattenedCurve::Line(s) => out.push(*s),
            FlattenedCurve::Quad(v) | FlattenedCurve::Cubic(v) => out.extend_from_slice(v),
        }
    }
    out
}

#[test]
fn flatten_grouped_mlqcz_yields_four_entries() {
    // M / L / Q / C / Z. The Z emits an implicit close-line.
    let ops = [
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 10.0, y: 0.0 },
        PathOp::QuadTo {
            cx: 50.0,
            cy: 100.0,
            x: 100.0,
            y: 0.0,
        },
        PathOp::CubicTo {
            c1x: 100.0,
            c1y: 50.0,
            c2x: 50.0,
            c2y: 50.0,
            x: 0.0,
            y: 50.0,
        },
        PathOp::Close,
    ];
    let curves = flatten_grouped(ops, &Affine::identity(), 0.25);
    // L + Q + C + implicit close-Line = 4 entries.
    assert_eq!(curves.len(), 4, "got: {curves:?}");
    assert!(matches!(curves[0], FlattenedCurve::Line(_)));
    assert!(matches!(curves[1], FlattenedCurve::Quad(_)));
    assert!(matches!(curves[2], FlattenedCurve::Cubic(_)));
    assert!(matches!(curves[3], FlattenedCurve::Line(_)));
    // Quad / Cubic both subdivide.
    if let FlattenedCurve::Quad(segs) = &curves[1] {
        assert!(segs.len() > 1);
    }
    if let FlattenedCurve::Cubic(segs) = &curves[2] {
        assert!(segs.len() > 1);
    }
}

#[test]
fn flatten_grouped_empty_input_is_empty() {
    let curves = flatten_grouped(core::iter::empty::<PathOp>(), &Affine::identity(), 0.25);
    assert!(curves.is_empty());
}

#[test]
fn flatten_grouped_lone_moveto_is_empty() {
    let ops = [PathOp::MoveTo { x: 5.0, y: 5.0 }];
    let curves = flatten_grouped(ops, &Affine::identity(), 0.25);
    assert!(
        curves.is_empty(),
        "MoveTo with no draw ops should yield no curves, got {curves:?}"
    );
}

#[test]
fn flatten_grouped_tight_tolerance_subdivides_cubic_heavily() {
    let ops = [
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::CubicTo {
            c1x: 0.0,
            c1y: 100.0,
            c2x: 100.0,
            c2y: 100.0,
            x: 100.0,
            y: 0.0,
        },
    ];
    let curves = flatten_grouped(ops, &Affine::identity(), 0.01);
    assert_eq!(curves.len(), 1);
    match &curves[0] {
        FlattenedCurve::Cubic(segs) => {
            assert!(segs.len() > 8, "tight tolerance got {} segs", segs.len());
        }
        other => panic!("expected Cubic, got {other:?}"),
    }
}

#[test]
fn flatten_grouped_is_deterministic() {
    let ops = [
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::QuadTo {
            cx: 50.0,
            cy: 100.0,
            x: 100.0,
            y: 0.0,
        },
        PathOp::CubicTo {
            c1x: 0.0,
            c1y: 50.0,
            c2x: 100.0,
            c2y: 50.0,
            x: 100.0,
            y: 0.0,
        },
        PathOp::Close,
    ];
    let a = flatten_grouped(ops, &Affine::identity(), 0.25);
    let b = flatten_grouped(ops, &Affine::identity(), 0.25);
    assert_eq!(a, b);
}

#[test]
fn flatten_grouped_segment_count_matches_flatten() {
    // Same chord output, just grouped. Concatenating the per-curve
    // segment lists must equal flatten()'s flat output exactly.
    let ops = [
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 10.0, y: 0.0 },
        PathOp::QuadTo {
            cx: 50.0,
            cy: 100.0,
            x: 100.0,
            y: 0.0,
        },
        PathOp::CubicTo {
            c1x: 100.0,
            c1y: 50.0,
            c2x: 50.0,
            c2y: 50.0,
            x: 0.0,
            y: 50.0,
        },
        PathOp::Close,
    ];
    let flat = flatten(ops, &Affine::identity(), 0.25);
    let grouped = flatten_grouped(ops, &Affine::identity(), 0.25);
    let ungrouped = ungroup(&grouped);
    assert_eq!(flat.len(), ungrouped.len(), "total chord count");
    assert_eq!(flat, ungrouped, "chord sequence must be bit-identical");
}

#[test]
fn flatten_grouped_close_at_start_emits_no_line() {
    // Already at the contour start when Close hits: no implicit
    // close-line, so the output is exactly the LineTo.
    let ops = [
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 10.0, y: 0.0 },
        PathOp::LineTo { x: 0.0, y: 0.0 },
        PathOp::Close,
    ];
    let curves = flatten_grouped(ops, &Affine::identity(), 0.25);
    assert_eq!(curves.len(), 2);
    assert!(matches!(curves[0], FlattenedCurve::Line(_)));
    assert!(matches!(curves[1], FlattenedCurve::Line(_)));
}

#[test]
fn flatten_grouped_applies_affine() {
    let ops = [
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 1.0, y: 0.0 },
    ];
    let curves = flatten_grouped(ops, &Affine::scale(10.0, 10.0), 0.25);
    match &curves[0] {
        FlattenedCurve::Line(s) => assert!((s.x1 - 10.0).abs() < 1e-5),
        other => panic!("expected Line, got {other:?}"),
    }
}

// -------- arc-length estimators --------

#[test]
fn segment_arc_length_is_euclidean_distance() {
    let s = Segment {
        x0: 0.0,
        y0: 0.0,
        x1: 3.0,
        y1: 4.0,
    };
    assert!((s.arc_length() - 5.0).abs() < 1e-6);
}

#[test]
fn arc_length_quad_straight_equals_chord() {
    // Control point exactly on the chord: curve degenerates to the
    // straight chord; arc length is the chord length.
    let l = arc_length_quad(0.0, 0.0, 50.0, 0.0, 100.0, 0.0, 0.01);
    assert!((l - 100.0).abs() < 1e-3, "got {l}");
}

#[test]
fn arc_length_quad_symmetric_arc() {
    // Quad with control at (50, 50) over chord (0,0)-(100,0). True
    // arc length ~ 114.7793 (analytic). Roger Willcocks adaptive
    // should land within 0.05 of that.
    let l = arc_length_quad(0.0, 0.0, 50.0, 50.0, 100.0, 0.0, 0.01);
    assert!((l - 114.7793).abs() < 0.05, "true arc 114.7793, got {l}");
}

#[test]
fn arc_length_cubic_straight_equals_chord() {
    // Both controls collinear with the chord.
    let l = arc_length_cubic(0.0, 0.0, 33.3, 0.0, 66.6, 0.0, 100.0, 0.0, 0.01);
    assert!((l - 100.0).abs() < 1e-3, "got {l}");
}

#[test]
fn arc_length_cubic_quarter_circle_kappa() {
    // Single cubic approximating a quarter circle of radius 100
    // using kappa = 4/3 * (sqrt(2) - 1) ~ 0.5522847.
    // True quarter-circle arc = π/2 * 100 ~ 157.0796. The cubic
    // approximates the circle to ~1e-3 relative error in shape; arc
    // length should land within ~0.1 of the true value.
    const K: f32 = 0.552_284_8 * 100.0;
    let l = arc_length_cubic(100.0, 0.0, 100.0, K, K, 100.0, 0.0, 100.0, 0.01);
    assert!((l - 157.0796).abs() < 0.1, "true arc 157.0796, got {l}");
}

#[test]
fn arc_length_cubic_pathological_cusp_does_not_panic() {
    // Both controls collapse to one point: classic cusp shape.
    // Length should be finite and non-negative, even at MAX_DEPTH.
    let l = arc_length_cubic(0.0, 0.0, 100.0, 100.0, 100.0, 100.0, 0.0, 0.0, 0.01);
    assert!(l.is_finite() && l > 0.0, "got {l}");
}

#[test]
fn arc_length_quad_solve_t_clamps_below_zero_and_above_total() {
    // target = 0  -> t = 0
    // target = ∞ -> t = 1
    let t0 = arc_length_quad_solve_t(0.0, 0.0, 50.0, 50.0, 100.0, 0.0, 0.0, 0.01);
    let t1 = arc_length_quad_solve_t(0.0, 0.0, 50.0, 50.0, 100.0, 0.0, 1e9, 0.01);
    assert!(t0.abs() < 1e-6);
    assert!((t1 - 1.0).abs() < 1e-6);
}

#[test]
fn arc_length_quad_solve_t_finds_midpoint_arc() {
    // Arc-length total ~ 114.78. Half-length should land near
    // t = 0.5 (the curve is symmetric about t = 0.5).
    let total = arc_length_quad(0.0, 0.0, 50.0, 50.0, 100.0, 0.0, 0.01);
    let t = arc_length_quad_solve_t(0.0, 0.0, 50.0, 50.0, 100.0, 0.0, 0.5 * total, 0.01);
    assert!(
        (t - 0.5).abs() < 1e-3,
        "expected t ~ 0.5 at midpoint arc length, got {t}"
    );
}

#[test]
fn arc_length_cubic_solve_t_round_trips() {
    // Quarter-circle approx; solve for t at a known arc length and
    // verify the prefix-length round-trips.
    const K: f32 = 0.552_284_8 * 100.0;
    let total = arc_length_cubic(100.0, 0.0, 100.0, K, K, 100.0, 0.0, 100.0, 0.01);
    let target = 0.25 * total;
    let t = arc_length_cubic_solve_t(100.0, 0.0, 100.0, K, K, 100.0, 0.0, 100.0, target, 0.01);
    let recovered = cubic_prefix_length(100.0, 0.0, 100.0, K, K, 100.0, 0.0, 100.0, t, 0.01);
    assert!(
        (recovered - target).abs() < 0.05,
        "target {target}, recovered {recovered}, t {t}"
    );
}

#[test]
fn arc_length_cubic_solve_t_pathological_cusp_does_not_panic() {
    // Same cusp as arc_length_cubic_pathological_cusp_does_not_panic.
    // Bisection is monotone-stable; should always return a finite t
    // in [0, 1].
    let total = arc_length_cubic(0.0, 0.0, 100.0, 100.0, 100.0, 100.0, 0.0, 0.0, 0.01);
    let t = arc_length_cubic_solve_t(
        0.0,
        0.0,
        100.0,
        100.0,
        100.0,
        100.0,
        0.0,
        0.0,
        0.5 * total,
        0.01,
    );
    assert!(t.is_finite() && (0.0..=1.0).contains(&t), "got {t}");
}
