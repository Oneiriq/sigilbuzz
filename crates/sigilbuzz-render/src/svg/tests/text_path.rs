//! Tests for the textPath arc-length and outline helpers.

use super::*;

use crate::svg::text_path::{
    build_arc_length_polyline, sample_polyline_position, transform_outline_ops, PolyPoint,
};

// ---- textPath helpers ------------------------------------------------

#[test]
fn arc_length_polyline_horizontal_line_lays_out_endpoints() {
    // A simple horizontal line from (10,50) to (210,50). The
    // flattener emits one segment so the polyline has two points,
    // with cum 0 and cum 200.
    let ops = vec![
        PathOp::MoveTo { x: 10.0, y: 50.0 },
        PathOp::LineTo { x: 210.0, y: 50.0 },
    ];
    let poly = build_arc_length_polyline(&ops);
    assert_eq!(poly.len(), 2);
    assert!((poly[0].cum - 0.0).abs() < 1e-5);
    assert!((poly[1].cum - 200.0).abs() < 1e-3);
    assert!((poly[0].x - 10.0).abs() < 1e-5);
    assert!((poly[1].x - 210.0).abs() < 1e-5);
}

#[test]
fn arc_length_polyline_keeps_open_subpaths_open() {
    // Fill flattening closes open contours, but a textPath reference is
    // a curve to walk: neither subpath gets a return leg. Two 100-unit
    // strokes give a total length of 200, not the 400 or so that the
    // two close-lines would add.
    let ops = vec![
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 100.0, y: 0.0 },
        PathOp::MoveTo { x: 0.0, y: 50.0 },
        PathOp::LineTo { x: 100.0, y: 50.0 },
    ];
    let poly = build_arc_length_polyline(&ops);
    assert_eq!(poly.len(), 3);
    assert!((poly[2].cum - 200.0).abs() < 1e-3, "got {}", poly[2].cum);
    assert!((poly[2].x - 100.0).abs() < 1e-5 && (poly[2].y - 50.0).abs() < 1e-5);
}

#[test]
fn sample_polyline_position_lerps_between_chord_endpoints() {
    let poly = vec![
        PolyPoint {
            x: 0.0,
            y: 0.0,
            cum: 0.0,
        },
        PolyPoint {
            x: 100.0,
            y: 0.0,
            cum: 100.0,
        },
        PolyPoint {
            x: 100.0,
            y: 100.0,
            cum: 200.0,
        },
    ];
    let p0 = sample_polyline_position(&poly, 0.0).unwrap();
    assert!((p0.0 - 0.0).abs() < 1e-5 && (p0.1 - 0.0).abs() < 1e-5);
    let p_mid_first = sample_polyline_position(&poly, 50.0).unwrap();
    assert!((p_mid_first.0 - 50.0).abs() < 1e-5 && (p_mid_first.1).abs() < 1e-5);
    let p_corner = sample_polyline_position(&poly, 100.0).unwrap();
    assert!((p_corner.0 - 100.0).abs() < 1e-5 && (p_corner.1 - 0.0).abs() < 1e-5);
    let p_mid_second = sample_polyline_position(&poly, 150.0).unwrap();
    assert!((p_mid_second.0 - 100.0).abs() < 1e-5 && (p_mid_second.1 - 50.0).abs() < 1e-5);
    // Past the total length -> None (silent drop policy in
    // emit_text_path_fills).
    assert!(sample_polyline_position(&poly, 250.0).is_none());
}

#[test]
fn transform_outline_ops_translates_and_flips_y() {
    // Design-unit point (0, 100) at scale 0.5 with origin
    // (50, 200) maps to (50 + 0*0.5, 200 - 100*0.5) = (50, 150).
    // Y is flipped so OT y-up matches SVG y-down.
    let ops = vec![
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 0.0, y: 100.0 },
        PathOp::QuadTo {
            cx: 50.0,
            cy: 50.0,
            x: 100.0,
            y: 0.0,
        },
        PathOp::Close,
    ];
    let out = transform_outline_ops(&ops, 0.5, 50.0, 200.0);
    assert_eq!(out.len(), ops.len());
    match out[0] {
        PathOp::MoveTo { x, y } => {
            assert!((x - 50.0).abs() < 1e-5);
            assert!((y - 200.0).abs() < 1e-5);
        }
        _ => panic!("expected MoveTo"),
    }
    match out[1] {
        PathOp::LineTo { x, y } => {
            assert!((x - 50.0).abs() < 1e-5);
            assert!((y - 150.0).abs() < 1e-5);
        }
        _ => panic!("expected LineTo"),
    }
    match out[2] {
        PathOp::QuadTo { cx, cy, x, y } => {
            assert!((cx - 75.0).abs() < 1e-5);
            assert!((cy - 175.0).abs() < 1e-5);
            assert!((x - 100.0).abs() < 1e-5);
            assert!((y - 200.0).abs() < 1e-5);
        }
        _ => panic!("expected QuadTo"),
    }
    assert!(matches!(out[3], PathOp::Close));
}

#[test]
fn transform_outline_ops_handles_cubic() {
    let ops = vec![
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::CubicTo {
            c1x: 10.0,
            c1y: 20.0,
            c2x: 30.0,
            c2y: 40.0,
            x: 50.0,
            y: 60.0,
        },
    ];
    let out = transform_outline_ops(&ops, 1.0, 0.0, 0.0);
    match out[1] {
        PathOp::CubicTo {
            c1x,
            c1y,
            c2x,
            c2y,
            x,
            y,
        } => {
            assert!((c1x - 10.0).abs() < 1e-5);
            assert!((c1y - -20.0).abs() < 1e-5); // y-flipped
            assert!((c2x - 30.0).abs() < 1e-5);
            assert!((c2y - -40.0).abs() < 1e-5);
            assert!((x - 50.0).abs() < 1e-5);
            assert!((y - -60.0).abs() < 1e-5);
        }
        _ => panic!("expected CubicTo"),
    }
}

#[test]
fn arc_length_polyline_cubic_aggregates_chord_lengths() {
    // A single cubic Bézier: M 0 0 C 0 100, 100 100, 100 0. A
    // hump from (0,0) to (100,0). Its true arc length is about 146.
    // The polyline should have len > 1 chords and a non-trivial
    // total cum.
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
    ];
    let poly = build_arc_length_polyline(&ops);
    assert!(poly.len() > 2, "cubic should subdivide into many chords");
    let total = poly.last().unwrap().cum;
    // The cubic with controls at y=100 sweeps well above a tight
    // arc. Empirical chord-length total at default tolerance is
    // ~200 (the curve's true arc length), not the much smaller
    // straight-line chord. Bound conservatively.
    assert!(
        (180.0..=220.0).contains(&total),
        "expected chord total ~200, got {total}"
    );
}
