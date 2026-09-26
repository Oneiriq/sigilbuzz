//! Tests for simple glyph outline decoding.

use super::*;

// ------------------------------------------------------------------
// Simple-glyph outline decoding.
// ------------------------------------------------------------------

#[test]
fn simple_glyph_rectangle_emits_four_lines() {
    // Closed rectangle: four on-curve corners. ttf-parser's
    // convention (and ours) emits an explicit LineTo back to the
    // start before Close.
    let pts = [
        (100, 100, true),
        (500, 100, true),
        (500, 400, true),
        (100, 400, true),
    ];
    let body = build_simple_glyph(&[3], &pts);
    let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&body);
    let mut o = Outline::new();
    glyf.outline(&loca, 0, None, None, &mut o).unwrap();
    assert_eq!(
        o.ops(),
        &[
            PathOp::MoveTo { x: 100.0, y: 100.0 },
            PathOp::LineTo { x: 500.0, y: 100.0 },
            PathOp::LineTo { x: 500.0, y: 400.0 },
            PathOp::LineTo { x: 100.0, y: 400.0 },
            PathOp::LineTo { x: 100.0, y: 100.0 },
            PathOp::Close,
        ]
    );
}

#[test]
fn simple_glyph_two_consecutive_off_curve_implies_midpoint() {
    // Contour: on(0,0), off(10,20), off(30,20), on(40,0). Two
    // off-curve points in a row -> implicit midpoint at (20, 20).
    let pts = [
        (0, 0, true),
        (10, 20, false),
        (30, 20, false),
        (40, 0, true),
    ];
    let body = build_simple_glyph(&[3], &pts);
    let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&body);
    let mut o = Outline::new();
    glyf.outline(&loca, 0, None, None, &mut o).unwrap();
    // Expected: MoveTo(0,0), QuadTo(10,20 -> 20,20),
    //           QuadTo(30,20 -> 40,0), LineTo(0,0), Close.
    let ops = o.ops();
    assert!(matches!(ops[0], PathOp::MoveTo { x: 0.0, y: 0.0 }));
    match ops[1] {
        PathOp::QuadTo { cx, cy, x, y } => {
            assert!((cx - 10.0).abs() < 1e-4);
            assert!((cy - 20.0).abs() < 1e-4);
            assert!((x - 20.0).abs() < 1e-4);
            assert!((y - 20.0).abs() < 1e-4);
        }
        _ => panic!("expected QuadTo at 1"),
    }
    match ops[2] {
        PathOp::QuadTo { cx, cy, x, y } => {
            assert!((cx - 30.0).abs() < 1e-4);
            assert!((cy - 20.0).abs() < 1e-4);
            assert!((x - 40.0).abs() < 1e-4);
            assert!((y - 0.0).abs() < 1e-4);
        }
        _ => panic!("expected QuadTo at 2"),
    }
    assert!(matches!(ops[3], PathOp::LineTo { x: 0.0, y: 0.0 }));
    assert!(matches!(ops[4], PathOp::Close));
}

#[test]
fn simple_glyph_with_deltas_shifts_points() {
    // Rectangle again, this time with gvar deltas moving every
    // point by (+5, -3).
    let pts = [
        (100, 100, true),
        (500, 100, true),
        (500, 400, true),
        (100, 400, true),
    ];
    let body = build_simple_glyph(&[3], &pts);
    let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&body);
    let mut o = Outline::new();
    let deltas: Vec<(f32, f32)> = vec![(5.0, -3.0); 4];
    glyf.outline(&loca, 0, Some(&deltas), None, &mut o).unwrap();
    assert!(matches!(o.ops()[0], PathOp::MoveTo { x: 105.0, y: 97.0 }));
    assert!(matches!(o.ops()[1], PathOp::LineTo { x: 505.0, y: 97.0 }));
}
