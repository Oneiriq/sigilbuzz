//! Public-surface integration tests for [`sigilbuzz_render::flatten_grouped`].
//!
//! Mirrors the MSDF use case described in issue #218: take a
//! single `PathOp` stream, flatten it once, and walk the per-source-
//! Bézier groups in order to make per-curve edge-coloring decisions.
//! These tests exercise the exact public path
//! (`sigilbuzz_render::{flatten_grouped, FlattenedCurve}`) so a
//! visibility regression would hard-fail rather than slip silently
//! behind a `pub(crate)`.

use sigilbuzz::tables::PathOp;
use sigilbuzz_render::{flatten, flatten_grouped, Affine, FlattenedCurve, DEFAULT_TOLERANCE};

/// Walk-the-curves convenience: assigns a synthetic edge-color tag
/// to each `FlattenedCurve` in a `Vec`. This is the *shape* of what
/// MSDF generators actually do: pick a channel per source curve,
/// then for every chord in that curve, paint into that channel.
/// We don't need a real MSDF here; we just need to prove the
/// per-curve identity is preserved end-to-end through the public
/// API.
fn paint_per_curve(curves: &[FlattenedCurve]) -> Vec<(usize, usize)> {
    // (curve_idx, chord_count): one entry per source curve.
    curves
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let n = match c {
                FlattenedCurve::Line(_) => 1,
                FlattenedCurve::Quad(v) | FlattenedCurve::Cubic(v) => v.len(),
            };
            (i, n)
        })
        .collect()
}

#[test]
fn flatten_grouped_is_callable_from_outside_the_crate() {
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

    let curves: Vec<FlattenedCurve> = flatten_grouped(ops, &Affine::identity(), DEFAULT_TOLERANCE);

    // 1 LineTo + 1 QuadTo + 1 CubicTo + 1 implicit close-line = 4.
    assert_eq!(curves.len(), 4);
    assert!(matches!(curves[0], FlattenedCurve::Line(_)));
    assert!(matches!(curves[1], FlattenedCurve::Quad(_)));
    assert!(matches!(curves[2], FlattenedCurve::Cubic(_)));
    assert!(matches!(curves[3], FlattenedCurve::Line(_)));
}

#[test]
fn msdf_use_case_per_curve_identity_preserved() {
    // The pattern an MSDF outline cache uses to wrap one Bezier at a
    // time, done here in a single pass.
    let ops = vec![
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::CubicTo {
            c1x: 0.0,
            c1y: 100.0,
            c2x: 100.0,
            c2y: 100.0,
            x: 100.0,
            y: 0.0,
        },
        PathOp::QuadTo {
            cx: 50.0,
            cy: -50.0,
            x: 0.0,
            y: 0.0,
        },
        PathOp::Close,
    ];

    let curves = flatten_grouped(ops, &Affine::identity(), DEFAULT_TOLERANCE);
    // Cubic + Quad. Close is a no-op here because the Quad already
    // returned to (0,0).
    assert_eq!(curves.len(), 2, "got: {curves:?}");
    assert!(matches!(curves[0], FlattenedCurve::Cubic(_)));
    assert!(matches!(curves[1], FlattenedCurve::Quad(_)));

    let painted = paint_per_curve(&curves);
    assert_eq!(painted.len(), 2);
    // Every curve should subdivide into multiple chords under default tol.
    assert!(painted[0].1 > 1, "cubic chord count: {}", painted[0].1);
    assert!(painted[1].1 > 1, "quad chord count: {}", painted[1].1);
}

#[test]
fn flatten_grouped_chord_count_matches_flatten() {
    // The grouped API must produce the same chord set as flatten().
    // This is the invariant that lets MSDF consumers
    // swap from the per-Bezier wrapping pattern to flatten_grouped()
    // without changing visual output.
    let ops = vec![
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::QuadTo {
            cx: 50.0,
            cy: 80.0,
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
        PathOp::LineTo { x: 0.0, y: 0.0 },
        PathOp::Close,
    ];

    let flat = flatten(ops.clone(), &Affine::identity(), DEFAULT_TOLERANCE);
    let grouped = flatten_grouped(ops, &Affine::identity(), DEFAULT_TOLERANCE);

    let total_chords: usize = grouped
        .iter()
        .map(|c| match c {
            FlattenedCurve::Line(_) => 1,
            FlattenedCurve::Quad(v) | FlattenedCurve::Cubic(v) => v.len(),
        })
        .sum();
    assert_eq!(flat.len(), total_chords);

    // And the actual chord sequence is bit-identical.
    let mut concat = Vec::with_capacity(total_chords);
    for c in &grouped {
        match c {
            FlattenedCurve::Line(s) => concat.push(*s),
            FlattenedCurve::Quad(v) | FlattenedCurve::Cubic(v) => concat.extend_from_slice(v),
        }
    }
    assert_eq!(flat, concat);
}

#[test]
fn flatten_grouped_with_xform() {
    // Confirm the xform is applied (and applied once) in the grouped
    // path, same as in flatten().
    let ops = [
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 1.0, y: 1.0 },
    ];
    let xf = Affine::scale(50.0, 50.0);
    let curves = flatten_grouped(ops, &xf, DEFAULT_TOLERANCE);
    // The LineTo, then the close-line of the open contour.
    assert_eq!(curves.len(), 2);
    assert!(matches!(curves[1], FlattenedCurve::Line(_)));
    match &curves[0] {
        FlattenedCurve::Line(s) => {
            assert!((s.x0).abs() < 1e-5 && (s.y0).abs() < 1e-5);
            assert!((s.x1 - 50.0).abs() < 1e-5 && (s.y1 - 50.0).abs() < 1e-5);
        }
        other => panic!("expected Line, got {other:?}"),
    }
}
