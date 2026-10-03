//! Tests for [`Gvar::glyph_point_deltas`]: inferred deltas, phantom
//! points, and malformed data.

use alloc::vec;
use alloc::vec::Vec;

use super::testing::{build_gvar, Tuple};
use super::*;

fn assert_deltas(got: &[(f32, f32)], want: &[(f32, f32)]) {
    assert_eq!(got.len(), want.len(), "{got:?}");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert!(
            (g.0 - w.0).abs() < 1e-4 && (g.1 - w.1).abs() < 1e-4,
            "point {i}: got {g:?}, want {w:?} in {got:?}"
        );
    }
}

/// Three contours, a point on no contour, and the phantom points:
///
/// - contour A, points 0 to 4, lists points 0 and 2;
/// - contour B, points 5 to 7, lists point 6 only;
/// - contour C, points 8 and 9, lists nothing;
/// - point 10 follows the last contour end and is listed;
/// - phantom point 12 (pp2) is listed, and so is point 99, past the end.
const POINTS: &[(i32, i32)] = &[
    (0, 0),
    (25, 40),
    (100, 100),
    (50, 200),
    (-10, 50),
    (300, 0),
    (400, 0),
    (350, 100),
    (500, 0),
    (600, 0),
    (700, 700),
];
const END_POINTS: &[u16] = &[4, 7, 9];

fn three_contours() -> Vec<u8> {
    build_gvar(
        1,
        &[vec![Tuple {
            peak: vec![1.0],
            points: Some(vec![0, 2, 6, 10, 12, 99]),
            deltas: vec![(10, 20), (30, -20), (5, -7), (3, 3), (40, 0), (1000, 1000)],
        }]],
    )
}

#[test]
fn unlisted_points_take_inferred_deltas() {
    let bytes = three_contours();
    let gvar = Gvar::parse(&bytes).unwrap();
    let got = gvar
        .glyph_point_deltas(0, &[1.0], POINTS, END_POINTS)
        .unwrap();
    assert_deltas(
        &got,
        &[
            (10.0, 20.0),
            // Between points 0 and 2 on both axes: interpolated. x 25
            // is a quarter of the way from 0 to 100, y 40 is 40%.
            (15.0, 4.0),
            (30.0, -20.0),
            // Between points 2 and 0 (wrapping around): x 50 is half
            // way, so interpolated; y 200 is past both, so it takes the
            // delta of the larger coordinate, point 2.
            (20.0, -20.0),
            // x -10 is below both, so the delta of the smaller
            // coordinate, point 0; y 50 is half way.
            (10.0, 0.0),
            // Contour B lists one point: the contour moves with it.
            (5.0, -7.0),
            (5.0, -7.0),
            (5.0, -7.0),
            // Contour C lists nothing: it stays.
            (0.0, 0.0),
            (0.0, 0.0),
            // On no contour: the listed delta only.
            (3.0, 3.0),
            // Phantom points: listed deltas only. Point 99 is ignored.
            (0.0, 0.0),
            (40.0, 0.0),
            (0.0, 0.0),
            (0.0, 0.0),
        ],
    );
}

#[test]
fn inference_runs_per_tuple_on_scaled_deltas() {
    // At coord 0.5 the first tuple (peak 1.0) scales by 0.5 and the
    // second (peak 0.5) by 1.0. Inferring after summing would give
    // point 1 only the second tuple's 8.
    let bytes = build_gvar(
        1,
        &[vec![
            Tuple {
                peak: vec![1.0],
                points: Some(vec![0, 2]),
                deltas: vec![(10, 0), (30, 0)],
            },
            Tuple {
                peak: vec![0.5],
                points: Some(vec![1]),
                deltas: vec![(8, 0)],
            },
        ]],
    );
    let gvar = Gvar::parse(&bytes).unwrap();
    let points = [(0, 0), (50, 0), (100, 0)];
    let got = gvar.glyph_point_deltas(0, &[0.5], &points, &[2]).unwrap();
    assert_deltas(
        &got,
        &[
            (13.0, 0.0),
            (18.0, 0.0),
            (23.0, 0.0),
            (0.0, 0.0),
            (0.0, 0.0),
            (0.0, 0.0),
            (0.0, 0.0),
        ],
    );
}

#[test]
fn a_point_listed_twice_gets_both_deltas() {
    let bytes = build_gvar(
        1,
        &[vec![Tuple {
            peak: vec![1.0],
            points: Some(vec![1, 1]),
            deltas: vec![(2, 0), (3, 1)],
        }]],
    );
    let gvar = Gvar::parse(&bytes).unwrap();
    let got = gvar
        .glyph_point_deltas(0, &[1.0], &[(0, 0), (10, 0)], &[1])
        .unwrap();
    assert_deltas(&got[..2], &[(5.0, 1.0), (5.0, 1.0)]);
}

#[test]
fn every_point_tuples_and_composites_infer_nothing() {
    // An all-points tuple lists every point, phantoms included.
    let bytes = build_gvar(
        1,
        &[vec![Tuple {
            peak: vec![1.0],
            points: None,
            deltas: vec![(1, 2), (3, 4), (5, 6), (7, 8), (9, 10), (11, 12)],
        }]],
    );
    let gvar = Gvar::parse(&bytes).unwrap();
    let got = gvar
        .glyph_point_deltas(0, &[1.0], &[(0, 0), (10, 0)], &[1])
        .unwrap();
    assert_deltas(
        &got,
        &[
            (1.0, 2.0),
            (3.0, 4.0),
            (5.0, 6.0),
            (7.0, 8.0),
            (9.0, 10.0),
            (11.0, 12.0),
        ],
    );
    // A composite passes no contour ends: its second component keeps
    // a zero delta even though the first one moves.
    let bytes = build_gvar(
        1,
        &[vec![Tuple {
            peak: vec![1.0],
            points: Some(vec![0]),
            deltas: vec![(50, 60)],
        }]],
    );
    let gvar = Gvar::parse(&bytes).unwrap();
    let got = gvar
        .glyph_point_deltas(0, &[1.0], &[(0, 0), (10, 0)], &[])
        .unwrap();
    assert_deltas(&got[..2], &[(50.0, 60.0), (0.0, 0.0)]);
}

#[test]
fn inferred_delta_cases() {
    // Same coordinate on both sides: their delta if they agree, else 0.
    assert!((infer_delta(5.0, 10.0, 10.0, 3.0, 3.0) - 3.0).abs() < 1e-6);
    assert!(infer_delta(5.0, 10.0, 10.0, 3.0, 4.0).abs() < 1e-6);
    // At or outside the range: the nearer side's delta.
    assert!((infer_delta(0.0, 10.0, 20.0, 1.0, 2.0) - 1.0).abs() < 1e-6);
    assert!((infer_delta(10.0, 10.0, 20.0, 1.0, 2.0) - 1.0).abs() < 1e-6);
    assert!((infer_delta(30.0, 20.0, 10.0, 1.0, 2.0) - 1.0).abs() < 1e-6);
    assert!((infer_delta(5.0, 20.0, 10.0, 1.0, 2.0) - 2.0).abs() < 1e-6);
    // Inside: linear.
    assert!((infer_delta(12.5, 10.0, 20.0, 0.0, 8.0) - 2.0).abs() < 1e-6);
    assert!((infer_delta(12.5, 20.0, 10.0, 8.0, 0.0) - 2.0).abs() < 1e-6);
}

#[test]
fn glyphs_without_data_get_zero_deltas() {
    let bytes = build_gvar(
        1,
        &[
            vec![],
            vec![Tuple {
                peak: vec![1.0],
                points: None,
                deltas: vec![(1, 1); 5],
            }],
        ],
    );
    let gvar = Gvar::parse(&bytes).unwrap();
    let zeros = vec![(0.0, 0.0); 5];
    // No data, past the glyph count, and a coord the tuple ignores.
    assert_deltas(
        &gvar.glyph_point_deltas(0, &[1.0], &[(0, 0)], &[0]).unwrap(),
        &zeros,
    );
    assert_deltas(
        &gvar.glyph_point_deltas(7, &[1.0], &[(0, 0)], &[0]).unwrap(),
        &zeros,
    );
    assert_deltas(
        &gvar
            .glyph_point_deltas(1, &[-1.0], &[(0, 0)], &[0])
            .unwrap(),
        &zeros,
    );
    assert_deltas(
        &gvar.glyph_point_deltas(1, &[1.0], &[(0, 0)], &[0]).unwrap(),
        &[(1.0, 1.0); 5],
    );
}

#[test]
fn phantom_deltas_match_the_dense_tail() {
    let bytes = three_contours();
    let gvar = Gvar::parse(&bytes).unwrap();
    let dense = gvar
        .glyph_point_deltas(0, &[1.0], POINTS, END_POINTS)
        .unwrap();
    let phantoms = gvar.phantom_deltas(0, &[1.0], POINTS.len()).unwrap();
    assert_deltas(&phantoms, &dense[POINTS.len()..]);
}

/// The table [`three_contours`] builds, with its one tuple's data size
/// cut by one byte, so the last y delta runs past the tuple.
fn short_tuple() -> (Vec<u8>, usize) {
    let mut bytes = three_contours();
    // Header (20 bytes) and two long offsets put the glyph data at 28;
    // its tuple header's data size follows the 4-byte glyph header.
    let size_at = 28 + 4;
    let size = u16::from_be_bytes([bytes[size_at], bytes[size_at + 1]]);
    bytes[size_at..size_at + 2].copy_from_slice(&(size - 1).to_be_bytes());
    // The y deltas' payload: past the 4-byte glyph header, the 6-byte
    // tuple header, 14 bytes of point numbers (count, control, six
    // words), and 13 bytes of x deltas (control, six words), then the y
    // control byte.
    (bytes, 28 + 4 + 6 + 14 + 13 + 1)
}

#[test]
fn malformed_data_reports_offsets_in_the_table() {
    let (bytes, y_payload) = short_tuple();
    let gvar = Gvar::parse(&bytes).unwrap();
    let err = gvar
        .glyph_point_deltas(0, &[1.0], POINTS, END_POINTS)
        .unwrap_err();
    assert_eq!(
        err,
        Error::Truncated {
            offset: y_payload,
            context: "packed deltas: i16 run (n)",
        }
    );
    // The listed-only view keeps reporting no deltas.
    assert!(gvar.glyph_deltas(0, &[1.0], 15).is_empty());

    // A table cut short: the glyph's data runs past its end.
    let mut bytes = three_contours();
    bytes.pop();
    let gvar = Gvar::parse(&bytes).unwrap();
    assert_eq!(
        gvar.glyph_point_deltas(0, &[1.0], POINTS, END_POINTS),
        Err(Error::Truncated {
            offset: 28,
            context: "gvar glyph data past end of table",
        })
    );

    // Offsets that run backward.
    let mut bytes = three_contours();
    bytes[24..28].copy_from_slice(&0u32.to_be_bytes());
    bytes[20..24].copy_from_slice(&4u32.to_be_bytes());
    let gvar = Gvar::parse(&bytes).unwrap();
    assert!(matches!(
        gvar.glyph_point_deltas(0, &[1.0], POINTS, END_POINTS),
        Err(Error::Malformed { offset: 20, .. })
    ));
}

#[test]
fn every_truncation_fails_cleanly() {
    let bytes = three_contours();
    for len in 0..bytes.len() {
        let Ok(gvar) = Gvar::parse(&bytes[..len]) else {
            continue;
        };
        let result = gvar.glyph_point_deltas(0, &[1.0], POINTS, END_POINTS);
        assert!(result.is_err() || len >= bytes.len(), "length {len}");
    }
}

#[test]
fn many_tuples_over_many_points_hit_the_work_cap() {
    // Each tuple moves point 0 of a 65,532-point glyph (65,536 with
    // the phantom points): 256 such tuples fit the cap, 257 do not.
    let tuples = |n: usize| -> Vec<u8> {
        let tuple = || Tuple {
            peak: vec![1.0],
            points: Some(vec![0]),
            deltas: vec![(1, 0)],
        };
        build_gvar(1, &[(0..n).map(|_| tuple()).collect()])
    };
    let fits = tuples(256);
    let gvar = Gvar::parse(&fits).unwrap();
    assert!(gvar.phantom_deltas(0, &[1.0], 65_532).is_ok());
    let over = tuples(257);
    let gvar = Gvar::parse(&over).unwrap();
    assert!(matches!(
        gvar.phantom_deltas(0, &[1.0], 65_532),
        Err(Error::Malformed { offset: 28, .. })
    ));
}
