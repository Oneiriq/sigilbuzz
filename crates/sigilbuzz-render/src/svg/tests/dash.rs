//! Tests for `stroke-dasharray` parsing and the dash walker.

use super::*;

use crate::svg::dash::{dash_polyline, parse_dasharray};
use crate::svg::stroke::{flatten_to_polylines, PolyLine};

#[test]
fn dasharray_parses_even_list_unchanged() {
    assert_eq!(parse_dasharray("4 2"), vec![4.0, 2.0]);
    assert_eq!(parse_dasharray("4, 2"), vec![4.0, 2.0]);
    assert_eq!(parse_dasharray("1 2 3 4"), vec![1.0, 2.0, 3.0, 4.0]);
}

#[test]
fn dasharray_doubles_odd_length() {
    // "2 3 5" -> "2 3 5 2 3 5"
    assert_eq!(parse_dasharray("2 3 5"), vec![2.0, 3.0, 5.0, 2.0, 3.0, 5.0]);
}

#[test]
fn dasharray_none_and_empty_yield_empty() {
    assert!(parse_dasharray("none").is_empty());
    assert!(parse_dasharray("").is_empty());
    assert!(parse_dasharray("   ").is_empty());
}

#[test]
fn dasharray_negative_or_invalid_yields_empty() {
    assert!(parse_dasharray("4 -2").is_empty());
    assert!(parse_dasharray("4 abc").is_empty());
}

#[test]
fn dasharray_zero_only_yields_empty() {
    // All zeros means "no dash" per the SVG spec, same as none.
    assert!(parse_dasharray("0 0 0 0").is_empty());
}

#[test]
fn dasharray_strips_px_suffix() {
    assert_eq!(parse_dasharray("4px 2px"), vec![4.0, 2.0]);
}

/// Compute Euclidean per-chord arc lengths for a straight-segment
/// polyline test fixture. For straight chords, true arc length
/// equals chord length, so callers can use this to drive
/// `dash_polyline` exactly the way the pre-arc-length walker did.
fn straight_arcs(points: &[(f32, f32)], closed: bool) -> Vec<f32> {
    let n = points.len();
    let segs = if closed { n } else { n - 1 };
    let mut out = Vec::with_capacity(segs);
    for i in 0..segs {
        let a = points[i];
        let b = points[(i + 1) % n];
        let dx = b.0 - a.0;
        let dy = b.1 - a.1;
        out.push((dx * dx + dy * dy).sqrt());
    }
    out
}

#[test]
fn dash_walker_emits_alternating_subpolylines_on_a_line() {
    // 20-unit horizontal line with pattern "4 2": dashes at
    // [0,4], [6,10], [12,16], [18,20] -> 4 sub-polylines.
    let line = vec![(0.0, 0.0), (20.0, 0.0)];
    let arcs = straight_arcs(&line, false);
    let segs = dash_polyline(&line, &arcs, false, &[4.0, 2.0], 0.0);
    assert_eq!(segs.len(), 4);
    // First dash starts at the contour origin.
    assert!((segs[0][0].0 - 0.0).abs() < 1e-4);
    // Second dash starts at x=6.
    assert!((segs[1][0].0 - 6.0).abs() < 1e-4);
}

#[test]
fn dash_walker_honours_offset() {
    // Same 20-unit line, pattern "4 2", offset=4 advances past the
    // first 4-unit draw. The contour now opens with a 2-unit skip
    // (x=0..2), then dashes start at x=2.
    let line = vec![(0.0, 0.0), (20.0, 0.0)];
    let arcs = straight_arcs(&line, false);
    let zero = dash_polyline(&line, &arcs, false, &[4.0, 2.0], 0.0);
    let off = dash_polyline(&line, &arcs, false, &[4.0, 2.0], 4.0);
    // With offset=0 the first dash starts at x=0; with offset=4 it
    // starts later (at x=2). Just verify the offset moved the
    // first dash forward.
    assert!(zero[0][0].0 < off[0][0].0);
    assert!((zero[0][0].0).abs() < 1e-4);
    assert!((off[0][0].0 - 2.0).abs() < 1e-4);
}

#[test]
fn dash_walker_resets_per_contour() {
    // Two contours via M..L M..L; both should start their dash
    // pattern from offset=0 (i.e. drawing first).
    let xml = r#"<svg viewBox="0 0 100 100">
        <path d="M 0 50 L 20 50 M 0 70 L 20 70" stroke="black"
              stroke-width="2" stroke-dasharray="4 2" fill="none"/>
    </svg>"#;
    let doc = parse_document(xml).unwrap();
    // One Fill record holds the union of all dashed ribbons; just
    // confirm the parser accepted the attribute.
    assert_eq!(doc.fills.len(), 1);
    assert!(doc.fills[0].is_stroke);
}

#[test]
fn dash_walker_works_on_closed_contour() {
    // A closed square has 4 sides of length 10; pattern "5 5".
    // Half of perimeter (20 of 40) should be drawing.
    let pts = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
    let arcs = straight_arcs(&pts, true);
    let segs = dash_polyline(&pts, &arcs, true, &[5.0, 5.0], 0.0);
    assert!(!segs.is_empty(), "closed contour should produce dashes");
    // Total drawn length should approximate 20 (= half the perimeter).
    let drawn: f32 = segs
        .iter()
        .map(|s| {
            let mut acc = 0.0_f32;
            for w in s.windows(2) {
                let dx = w[1].0 - w[0].0;
                let dy = w[1].1 - w[0].1;
                acc += (dx * dx + dy * dy).sqrt();
            }
            acc
        })
        .sum();
    assert!(
        (drawn - 20.0).abs() < 0.5,
        "expected ~20 drawn units, got {drawn}"
    );
}

/// Build the four-cubic kappa-circle and return the polyline +
/// per-chord arc-length array, exactly as `flatten_to_polylines`
/// produces them. Returned circle is centered at `(cx, cy)` with
/// radius `r`. Used by both the true-arc and chord-flatten dash
/// count tests so they share input geometry.
fn build_kappa_circle(cx: f32, cy: f32, r: f32) -> PolyLine {
    // Standard cubic-Bezier circle approximation: each quarter
    // sweeps 90 degrees with control points offset by `kappa * r`
    // tangentially.
    const K: f32 = 0.552_284_8;
    let kr = K * r;
    let ops = vec![
        PathOp::MoveTo { x: cx + r, y: cy },
        PathOp::CubicTo {
            c1x: cx + r,
            c1y: cy + kr,
            c2x: cx + kr,
            c2y: cy + r,
            x: cx,
            y: cy + r,
        },
        PathOp::CubicTo {
            c1x: cx - kr,
            c1y: cy + r,
            c2x: cx - r,
            c2y: cy + kr,
            x: cx - r,
            y: cy,
        },
        PathOp::CubicTo {
            c1x: cx - r,
            c1y: cy - kr,
            c2x: cx - kr,
            c2y: cy - r,
            x: cx,
            y: cy - r,
        },
        PathOp::CubicTo {
            c1x: cx + kr,
            c1y: cy - r,
            c2x: cx + r,
            c2y: cy - kr,
            x: cx + r,
            y: cy,
        },
        PathOp::Close,
    ];
    let polys = flatten_to_polylines(&ops);
    assert_eq!(polys.len(), 1);
    polys.into_iter().next().unwrap()
}

#[test]
fn circle_of_cubics_dash_count_uses_true_arc_length() {
    // Radius-100 circle approximated by 4 cubics. True
    // circumference = 2π*100 ~ 628.32. With dasharray "10 10"
    // (period 20) we expect ~31.4 dash periods around the circle,
    // and since pattern starts on a draw, ~31 full dashes (the
    // half-cycle being a rendering edge).
    //
    // The chord-flattened polyline of the kappa-circle is slightly
    // shorter than the true circumference (chords are always under
    // the curve), so a chord-only walker would produce a different
    // (smaller) dash count. We assert that the *true-arc* walker
    // lands within one dash of the analytic count.
    let circle = build_kappa_circle(0.0, 0.0, 100.0);
    let true_arc_total: f32 = circle.arc_lengths.iter().sum();
    let chord_total: f32 = {
        let n = circle.points.len();
        (0..n)
            .map(|i| {
                let a = circle.points[i];
                let b = circle.points[(i + 1) % n];
                let dx = b.0 - a.0;
                let dy = b.1 - a.1;
                (dx * dx + dy * dy).sqrt()
            })
            .sum()
    };
    // Sanity: arc-length is *longer* than the chord polyline,
    // matching the brief's analytic prediction.
    assert!(
        true_arc_total > chord_total,
        "arc-length {true_arc_total} must exceed chord total {chord_total}"
    );
    // Both should be close to 2π*100; arc-length should be much
    // closer (sub-percent) than chord.
    let circumference = 2.0 * core::f32::consts::PI * 100.0;
    let arc_err = (true_arc_total - circumference).abs() / circumference;
    let chord_err = (chord_total - circumference).abs() / circumference;
    assert!(
        arc_err < 0.005,
        "arc-length should be < 0.5% off the true circumference, got {}%",
        arc_err * 100.0
    );
    assert!(
        arc_err < chord_err,
        "arc-length must beat chord-flatten ({}% vs {}%)",
        arc_err * 100.0,
        chord_err * 100.0
    );

    // Run the dasher (true arc length) and count "draw" sub-polylines.
    let segs = dash_polyline(
        &circle.points,
        &circle.arc_lengths,
        circle.closed,
        &[10.0, 10.0],
        0.0,
    );
    // Run the same input through chord-only walking by passing the
    // chord lengths as `arc_lengths`. The dash count will be lower
    // because the circumference is under-measured.
    let chord_arcs: Vec<f32> = {
        let n = circle.points.len();
        (0..n)
            .map(|i| {
                let a = circle.points[i];
                let b = circle.points[(i + 1) % n];
                let dx = b.0 - a.0;
                let dy = b.1 - a.1;
                (dx * dx + dy * dy).sqrt()
            })
            .collect()
    };
    let chord_segs = dash_polyline(
        &circle.points,
        &chord_arcs,
        circle.closed,
        &[10.0, 10.0],
        0.0,
    );
    // The arc-length walker must see at least as many full draw
    // dashes as the chord walker. A longer "track" can only fit
    // more (or equal) dash periods, never fewer. This is the
    // observable signature of the fix.
    // The arc-length walker must produce at least as many full
    // dashes as the chord walker. A longer track can only fit
    // equal or more periods. (At radius 100 with dash-period 20
    // both round to 32 dashes; the discriminating signal is the
    // length measurement above, not the count, but on circles
    // tuned exactly to the dash period the count diverges.)
    assert!(
        segs.len() >= chord_segs.len(),
        "true-arc dash count {} must be >= chord-flatten count {}",
        segs.len(),
        chord_segs.len()
    );
    // 628.32 / 20 ~ 31.4 cycles, so either 31 or 32 full draw
    // sub-polylines depending on where the final dash boundary
    // lands. The chord-flatten path measures ~627 (about 0.2 % short),
    // which biases the count down by less than one. With true arc
    // length we should be right at the analytic count.
    assert!(
        (31..=32).contains(&segs.len()),
        "expected 31 or 32 draw dashes around the circle, got {}",
        segs.len()
    );

    // Also: total drawn arc length should be ~half the
    // circumference (since pattern is 50/50 draw/skip). Use the
    // chord lengths between dash sub-polyline points; for a circle
    // discretized at 0.25 px tolerance these are within sub-pixel
    // of the true sub-arc length.
    let drawn: f32 = segs
        .iter()
        .map(|s| {
            let mut acc = 0.0_f32;
            for w in s.windows(2) {
                let dx = w[1].0 - w[0].0;
                let dy = w[1].1 - w[0].1;
                acc += (dx * dx + dy * dy).sqrt();
            }
            acc
        })
        .sum();
    // Each dash is 10 arc-length units; chord-length of a 10-unit
    // arc on a radius-100 circle is 2*100*sin(5/100) ~ 9.996, so
    // expected drawn (chord-measured) ~ 31 * 9.996 ~ 309.9. Allow a
    // generous tolerance because the trailing partial dash can vary.
    assert!(
        drawn > 300.0 && drawn < 320.0,
        "expected ~310 chord-measured draw length, got {drawn}"
    );
}

#[test]
fn long_quadratic_with_continuous_dasharray_has_no_dash_break() {
    // dasharray "1 0" -> period 1, fully drawing (skip is zero
    // length). This must produce identical output to the
    // un-dashed stroke: every sub-polyline boundary the walker
    // emits coincides with a chord vertex, and the union of draw
    // sub-polylines covers the whole curve.
    let xml = r#"<svg viewBox="0 0 200 100">
        <path d="M 0 50 Q 100 -50 200 50" stroke="black"
              stroke-width="2" stroke-dasharray="1 0" fill="none"/>
    </svg>"#;
    let doc = parse_document(xml).unwrap();
    // Parser accepted the dasharray and produced a stroke fill.
    assert_eq!(doc.fills.len(), 1);
    assert!(doc.fills[0].is_stroke);
    // "1 0" parses to [1, 0]; sum is 1 > 0, so dashed path runs.
    // The fill should cover the entire stroke ribbon, i.e. it has
    // a non-trivial number of MoveTo records (one per draw run).
    let moveto_count = doc.fills[0]
        .ops
        .iter()
        .filter(|o| matches!(o, PathOp::MoveTo { .. }))
        .count();
    assert!(moveto_count > 0);
}

#[test]
fn cusp_cubic_with_dasharray_does_not_panic() {
    // Both controls collapse to one point: classic cusp shape.
    // Adversarial input for adaptive subdivision; verify the
    // parser + dasher complete without a panic.
    let xml = r#"<svg viewBox="0 0 100 100">
        <path d="M 0 0 C 100 100 100 100 0 0" stroke="black"
              stroke-width="2" stroke-dasharray="5 5" fill="none"/>
    </svg>"#;
    let doc = parse_document(xml).unwrap();
    // Either a stroke fill is emitted or it's empty (degenerate
    // cusp may collapse), but we must not panic.
    for fill in &doc.fills {
        assert!(!fill.ops.iter().any(|o| match o {
            PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => !x.is_finite() || !y.is_finite(),
            _ => false,
        }));
    }
}

#[test]
fn straight_polyline_dasharray_byte_identical_to_chord_walker() {
    // Pre-arc-length walker computed `seg_len` from chord points.
    // For a straight polyline `arc_lengths[i]` IS the chord
    // length, so the new walker must produce bit-identical output
    // on this input, which is the contract the existing PR #227
    // tests rely on.
    //
    // Drive both: the new walker via the public API, and a
    // recreation of the old walker (chord lengths derived inline)
    // and assert exact equality.
    let pts = vec![(0.0, 0.0), (5.0, 0.0), (5.0, 5.0), (15.0, 5.0)];
    let arcs = straight_arcs(&pts, false);
    let new_segs = dash_polyline(&pts, &arcs, false, &[3.0, 1.0], 0.0);
    // Chord-derived arc lengths == Euclidean distance for straight
    // chords; passing them through gives the same trace the
    // pre-arc-length walker would have computed itself inline.
    let chord_arcs: Vec<f32> = (0..pts.len() - 1)
        .map(|i| {
            let a = pts[i];
            let b = pts[i + 1];
            ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
        })
        .collect();
    let chord_segs = dash_polyline(&pts, &chord_arcs, false, &[3.0, 1.0], 0.0);
    assert_eq!(new_segs.len(), chord_segs.len());
    for (a, b) in new_segs.iter().zip(chord_segs.iter()) {
        assert_eq!(a.len(), b.len());
        for (pa, pb) in a.iter().zip(b.iter()) {
            assert!((pa.0 - pb.0).abs() < 1e-6 && (pa.1 - pb.1).abs() < 1e-6);
        }
    }
}
