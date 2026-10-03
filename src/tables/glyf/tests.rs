//! Tests for the `glyf` parser: glyph header, bounds, point counts,
//! phantom points and glyph point extraction, plus the shared fixture
//! builders. Outline decoding tests live in the child modules.

use super::*;
use crate::tables::head::IndexToLocFormat;
use crate::tables::outline::{Outline, PathOp};
use alloc::vec;
use alloc::vec::Vec;

mod composite;
mod simple;
mod variations;

fn build_header(num_contours: i16, xmin: i16, ymin: i16, xmax: i16, ymax: i16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&num_contours.to_be_bytes());
    out.extend_from_slice(&xmin.to_be_bytes());
    out.extend_from_slice(&ymin.to_be_bytes());
    out.extend_from_slice(&xmax.to_be_bytes());
    out.extend_from_slice(&ymax.to_be_bytes());
    out
}

fn build_loca_short(offsets: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    for o in offsets {
        out.extend_from_slice(&o.to_be_bytes());
    }
    out
}

/// Builds a minimal `hmtx` body with one long metric per glyph.
fn build_hmtx(longs: &[(u16, i16)]) -> Vec<u8> {
    let mut b = Vec::new();
    for (adv, lsb) in longs {
        b.extend_from_slice(&adv.to_be_bytes());
        b.extend_from_slice(&lsb.to_be_bytes());
    }
    b
}

#[test]
fn phantom_points_match_spec_formula() {
    // Single simple glyph with bbox (xMin=10, yMax=200) plus an
    // hmtx record (advance=300, lsb=4). Expected phantoms:
    //   pp1 = (xMin - lsb, 0)             = (6,   0)
    //   pp2 = (pp1 + advance, 0)          = (306, 0)
    //   pp3 = (0, 0)   (no vmtx)
    //   pp4 = (0, 0)   (no vmtx)
    let body = build_simple_glyph(
        &[0],
        &[(10, 0, true)], // single contour point at (10, 0)
    );
    // Patch the bbox bytes to set yMax=200 explicitly (build_header
    // wrote yMax=1000 by default; we want a known number).
    let mut body = body;
    body[2..4].copy_from_slice(&10i16.to_be_bytes()); // xMin
    body[8..10].copy_from_slice(&200i16.to_be_bytes()); // yMax

    let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&body);

    let hmtx_bytes = build_hmtx(&[(300, 4)]);
    let hmtx = Hmtx::parse(&hmtx_bytes, 1, 1).unwrap();
    let metrics = PhantomMetrics {
        hmtx: &hmtx,
        vmtx: None,
    };
    let pp = glyf.phantom_points(&loca, 0, &metrics).unwrap();
    assert!((pp[0].0 - 6.0).abs() < 1e-4);
    assert!((pp[0].1 - 0.0).abs() < 1e-4);
    assert!((pp[1].0 - 306.0).abs() < 1e-4);
    assert!((pp[1].1 - 0.0).abs() < 1e-4);
    assert!((pp[2].0 - 0.0).abs() < 1e-4);
    assert!((pp[2].1 - 0.0).abs() < 1e-4);
    assert!((pp[3].0 - 0.0).abs() < 1e-4);
    assert!((pp[3].1 - 0.0).abs() < 1e-4);
}

#[test]
fn phantom_points_use_vmtx_when_present() {
    // Same glyph, this time with vmtx supplying advance=1000,
    // tsb=50. yMax=200 -> pp3 = (0, 250); pp4 = (0, -750).
    let body = build_simple_glyph(&[0], &[(10, 0, true)]);
    let mut body = body;
    body[2..4].copy_from_slice(&10i16.to_be_bytes());
    body[8..10].copy_from_slice(&200i16.to_be_bytes());
    let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&body);

    let hmtx_bytes = build_hmtx(&[(300, 4)]);
    let hmtx = Hmtx::parse(&hmtx_bytes, 1, 1).unwrap();
    // vmtx body: one long metric (advance=1000, tsb=50).
    let mut vmtx_bytes = Vec::new();
    vmtx_bytes.extend_from_slice(&1000u16.to_be_bytes());
    vmtx_bytes.extend_from_slice(&50i16.to_be_bytes());
    let vmtx = Vmtx::parse(&vmtx_bytes, 1, 1).unwrap();
    let metrics = PhantomMetrics {
        hmtx: &hmtx,
        vmtx: Some(&vmtx),
    };
    let pp = glyf.phantom_points(&loca, 0, &metrics).unwrap();
    assert!((pp[2].1 - 250.0).abs() < 1e-4, "pp3 y = {}", pp[2].1);
    assert!((pp[3].1 + 750.0).abs() < 1e-4, "pp4 y = {}", pp[3].1);
}

#[test]
fn phantom_points_no_glyph_body_yields_zero_pp1_pp2() {
    // Empty glyph (zero loca range) -> bounds returns None ->
    // phantom calc folds xMin/yMax to 0. With advance=500, lsb=10,
    // pp1=(0-10,0)=(-10,0), pp2=(490,0).
    let loca_bytes = build_loca_short(&[0, 0]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&[]);
    let hmtx_bytes = build_hmtx(&[(500, 10)]);
    let hmtx = Hmtx::parse(&hmtx_bytes, 1, 1).unwrap();
    let metrics = PhantomMetrics {
        hmtx: &hmtx,
        vmtx: None,
    };
    let pp = glyf.phantom_points(&loca, 0, &metrics).unwrap();
    assert!((pp[0].0 + 10.0).abs() < 1e-4);
    assert!((pp[1].0 - 490.0).abs() < 1e-4);
}

/// Pads `body` to an even length with a trailing zero byte. Short
/// `loca` offsets are u16 word indices, so an odd-length glyph
/// would otherwise be truncated by 1 byte at the end.
fn pad_even(mut body: Vec<u8>) -> Vec<u8> {
    if body.len() % 2 != 0 {
        body.push(0);
    }
    body
}

#[test]
fn glyph_points_returns_contours_then_four_phantoms() {
    // Single contour with four on-curve points (length stays even):
    // (10, 20), (40, 20), (40, 80), (10, 80). Bbox patched to
    // xMin=10, yMax=80. hmtx supplies advance=300, lsb=4 ->
    // pp1=(10-4, 0)=(6, 0), pp2=(306, 0). No vmtx -> pp3 = pp4 = 0.
    let mut body = build_simple_glyph(
        &[3],
        &[
            (10, 20, true),
            (40, 20, true),
            (40, 80, true),
            (10, 80, true),
        ],
    );
    body[2..4].copy_from_slice(&10i16.to_be_bytes()); // xMin
    body[8..10].copy_from_slice(&80i16.to_be_bytes()); // yMax
    let body = pad_even(body);
    let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&body);

    let hmtx_bytes = build_hmtx(&[(300, 4)]);
    let hmtx = Hmtx::parse(&hmtx_bytes, 1, 1).unwrap();

    let pts = glyf.glyph_points(&loca, 0, &hmtx, None).unwrap().unwrap();
    // 4 contour points + 4 phantoms.
    assert_eq!(pts.len(), 8);
    assert_eq!(pts[0], (10, 20));
    assert_eq!(pts[1], (40, 20));
    assert_eq!(pts[2], (40, 80));
    assert_eq!(pts[3], (10, 80));
    // pp1 = (xMin - lsb, 0) = (6, 0).
    assert_eq!(pts[4], (6, 0));
    // pp2 = pp1 + advance = (306, 0).
    assert_eq!(pts[5], (306, 0));
    // No vmtx -> pp3 / pp4 collapse to zero.
    assert_eq!(pts[6], (0, 0));
    assert_eq!(pts[7], (0, 0));
}

#[test]
fn glyph_points_keeps_off_curve_points_in_glyf_order() {
    // Four points: on, off, on, on. The off-curve control at index
    // 1 must survive: kerx fmt 4 type 0 can reference it.
    let body = pad_even(build_simple_glyph(
        &[3],
        &[
            (0, 0, true),
            (50, 50, false),
            (100, 0, true),
            (150, 50, true),
        ],
    ));
    let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&body);

    let hmtx_bytes = build_hmtx(&[(200, 0)]);
    let hmtx = Hmtx::parse(&hmtx_bytes, 1, 1).unwrap();
    let pts = glyf.glyph_points(&loca, 0, &hmtx, None).unwrap().unwrap();
    assert_eq!(pts[0], (0, 0));
    assert_eq!(pts[1], (50, 50)); // off-curve survives
    assert_eq!(pts[2], (100, 0));
    assert_eq!(pts[3], (150, 50));
}

#[test]
fn glyph_points_uses_vmtx_phantoms_when_present() {
    // yMax=200, vmtx advance=1000, tsb=50 -> pp3=(0, 250),
    // pp4=(0, 250 - 1000)=(0, -750).
    let mut body = build_simple_glyph(&[1], &[(0, 0, true), (10, 0, true)]);
    body[8..10].copy_from_slice(&200i16.to_be_bytes());
    let body = pad_even(body);
    let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&body);
    let hmtx_bytes = build_hmtx(&[(300, 0)]);
    let hmtx = Hmtx::parse(&hmtx_bytes, 1, 1).unwrap();
    let mut vmtx_bytes = Vec::new();
    vmtx_bytes.extend_from_slice(&1000u16.to_be_bytes());
    vmtx_bytes.extend_from_slice(&50i16.to_be_bytes());
    let vmtx = Vmtx::parse(&vmtx_bytes, 1, 1).unwrap();

    let pts = glyf
        .glyph_points(&loca, 0, &hmtx, Some(&vmtx))
        .unwrap()
        .unwrap();
    // 2 contour points + 4 phantoms.
    assert_eq!(pts.len(), 6);
    assert_eq!(pts[4], (0, 250)); // pp3
    assert_eq!(pts[5], (0, -750)); // pp4
}

#[test]
fn glyph_points_empty_glyph_returns_none() {
    let loca_bytes = build_loca_short(&[0, 0]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&[]);
    let hmtx_bytes = build_hmtx(&[(500, 0)]);
    let hmtx = Hmtx::parse(&hmtx_bytes, 1, 1).unwrap();
    assert!(glyf.glyph_points(&loca, 0, &hmtx, None).unwrap().is_none());
}

#[test]
fn reads_bounds_from_simple_glyph() {
    let g0_body: Vec<u8> = Vec::new();
    let g1_body = build_header(1, 10, -200, 500, 1500);

    let mut glyf = Vec::new();
    glyf.extend_from_slice(&g0_body);
    glyf.extend_from_slice(&g1_body);

    let loca_bytes = build_loca_short(&[0, 0, (g1_body.len() as u16) / 2]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 2).unwrap();
    let glyf_view = Glyf::new(&glyf);

    assert!(glyf_view.bounds(&loca, 0).unwrap().is_none());
    let b = glyf_view.bounds(&loca, 1).unwrap().unwrap();
    assert_eq!(b.num_contours, 1);
    assert_eq!(b.x_min, 10);
    assert_eq!(b.y_min, -200);
    assert_eq!(b.x_max, 500);
    assert_eq!(b.y_max, 1500);
}

#[test]
fn composite_glyph_reports_negative_contour_count() {
    let body = build_header(-1, 0, 0, 1000, 1000);
    let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&body);
    let b = glyf.bounds(&loca, 0).unwrap().unwrap();
    assert_eq!(b.num_contours, -1);
}

#[test]
fn out_of_range_glyph_yields_none_from_loca() {
    let loca_bytes = build_loca_short(&[0, 10]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&[0u8; 20]);
    assert!(glyf.bounds(&loca, 7).unwrap().is_none());
}

#[test]
fn rejects_range_past_glyf_end() {
    let loca_bytes = build_loca_short(&[0, 50]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&[0u8; 8]);
    assert!(glyf.bounds(&loca, 0).is_err());
}

#[test]
fn point_count_simple_glyph_adds_four_phantom_points() {
    let mut body = build_header(1, 0, 0, 100, 100);
    body.extend_from_slice(&3u16.to_be_bytes());
    let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&body);
    assert_eq!(glyf.point_count(&loca, 0).unwrap(), Some(8));
}

#[test]
fn point_count_composite_glyph_returns_none() {
    let body = build_header(-1, 0, 0, 100, 100);
    let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&body);
    assert_eq!(glyf.point_count(&loca, 0).unwrap(), None);
}

#[test]
fn point_count_empty_glyph_returns_none() {
    let loca_bytes = build_loca_short(&[0, 0]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&[]);
    assert_eq!(glyf.point_count(&loca, 0).unwrap(), None);
}

#[test]
fn rejects_range_shorter_than_header() {
    let loca_bytes = build_loca_short(&[0, 3]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&[0u8; 10]);
    assert!(glyf.bounds(&loca, 0).is_err());
}

/// Builds a simple glyph with absolute point coordinates, marking
/// each point as on/off curve. Forces long-form x/y flags so the
/// test fixtures are easy to read.
fn build_simple_glyph(end_pts: &[u16], pts: &[(i16, i16, bool)]) -> Vec<u8> {
    let mut body = build_header(end_pts.len() as i16, 0, 0, 1000, 1000);
    for e in end_pts {
        body.extend_from_slice(&e.to_be_bytes());
    }
    body.extend_from_slice(&0u16.to_be_bytes()); // instructions length

    // Flags: ON_CURVE bit only; neither short nor same.
    for &(_, _, on) in pts {
        let f = if on { FLAG_ON_CURVE } else { 0 };
        body.push(f);
    }
    // X as deltas from previous (starting at 0), long form.
    let mut prev = 0i16;
    for &(x, _, _) in pts {
        let dx = x - prev;
        body.extend_from_slice(&dx.to_be_bytes());
        prev = x;
    }
    let mut prev = 0i16;
    for &(_, y, _) in pts {
        let dy = y - prev;
        body.extend_from_slice(&dy.to_be_bytes());
        prev = y;
    }
    body
}
