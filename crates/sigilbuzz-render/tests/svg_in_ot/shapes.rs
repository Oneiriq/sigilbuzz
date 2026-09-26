//! `<polygon>`, `<polyline>`, and `<line>` primitives plus
//! stroke-dasharray.

use sigilbuzz::{Blob, Face};
use sigilbuzz_render::Rasterizer;

use crate::fonts::build_svg_font;

// =========================================================================
// PR #223 deferral coverage: <polygon> / <polyline> / <line> shape
// primitives + stroke-dasharray. Each new feature gets a fixture and a
// render-side invariant that's hard to satisfy without the new code.
// =========================================================================

/// `<polygon>` filled red: a triangle covering the lower half of the
/// viewBox. Opaque red pixels in the interior, transparent above.
#[test]
fn svg_polygon_fills_a_triangle() {
    let payload = b"<svg viewBox=\"0 0 100 100\">\
                    <polygon points=\"10,90 90,90 50,30\" fill=\"#FF0000\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();

    // Centroid (~ (50, 70)) should be solid red.
    let p = pix.get(50, 70);
    assert_eq!(
        p,
        [255, 0, 0, 255],
        "centroid should be solid red, got {p:?}"
    );
    // Far above the triangle apex (y < 30) is empty.
    let above = pix.get(50, 5);
    assert_eq!(above[3], 0, "above triangle should be transparent");

    // Coverage should be ~half the bbox of the triangle (60x60 -> ~1800).
    let mut filled = 0u32;
    for y in 0..pix.height {
        for x in 0..pix.width {
            if pix.get(x, y)[3] > 128 {
                filled += 1;
            }
        }
    }
    assert!(
        filled > 1800 && filled < 2700,
        "triangle area out of range: {filled}"
    );
}

/// `<polyline>` stroked black, `fill="none"`. The path is "C-shaped"
/// (top + right + bottom edges of a square): pixels along the stroke
/// are opaque, but the interior of the C remains transparent because
/// polylines aren't closed.
#[test]
fn svg_polyline_strokes_without_filling() {
    let payload = b"<svg viewBox=\"0 0 100 100\">\
                    <polyline points=\"20,20 80,20 80,80 20,80\" \
                              stroke=\"#000\" stroke-width=\"4\" fill=\"none\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();

    // Stroke pixels along the top edge (y=20) should be opaque black.
    let top = pix.get(50, 20);
    assert!(
        top[3] > 200 && top[0] < 30,
        "top stroke should be opaque black, got {top:?}"
    );
    // Interior of the C (y=50, x=50) should be transparent. Polyline
    // doesn't auto-close.
    let inside = pix.get(50, 50);
    assert_eq!(
        inside[3], 0,
        "polyline interior should not fill, got {inside:?}"
    );
    // The "open" left side (x=20, y=50) should also be transparent.
    let left_open = pix.get(20, 50);
    assert_eq!(
        left_open[3], 0,
        "polyline open side should be transparent, got {left_open:?}"
    );
}

/// `<line>` with a 4-unit black stroke. Pixels on the line are opaque
/// black; pixels above the line are clear.
#[test]
fn svg_line_strokes_a_segment() {
    let payload = b"<svg viewBox=\"0 0 100 100\">\
                    <line x1=\"10\" y1=\"50\" x2=\"90\" y2=\"50\" \
                          stroke=\"#000\" stroke-width=\"4\" fill=\"none\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();

    let mid = pix.get(50, 50);
    assert!(
        mid[3] > 200 && mid[0] < 30,
        "line should be opaque black, got {mid:?}"
    );
    // 20px above the line: clear.
    let above = pix.get(50, 20);
    assert_eq!(above[3], 0, "above line should be transparent");
}

/// `<line stroke-dasharray="4 2">`: dashed horizontal line. Coverage
/// of the stroke band should fall to roughly draw / (draw + skip) of
/// the un-dashed total, i.e. approximately 4/(4+2) = 67%.
#[test]
fn svg_line_with_dasharray_alternates_lit_and_unlit() {
    let solid = b"<svg viewBox=\"0 0 100 100\">\
                  <line x1=\"10\" y1=\"50\" x2=\"90\" y2=\"50\" \
                        stroke=\"#000\" stroke-width=\"4\" fill=\"none\"/>\
                  </svg>";
    let dashed = b"<svg viewBox=\"0 0 100 100\">\
                   <line x1=\"10\" y1=\"50\" x2=\"90\" y2=\"50\" \
                         stroke=\"#000\" stroke-width=\"4\" fill=\"none\" \
                         stroke-dasharray=\"4 2\"/>\
                   </svg>";

    fn count_opaque(payload: &[u8]) -> u32 {
        let bytes = build_svg_font(payload);
        let blob = Blob::new(&bytes);
        let face = Face::parse(&blob, 0).unwrap();
        let rast = Rasterizer::new();
        let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();
        let mut n = 0u32;
        for y in 0..pix.height {
            for x in 0..pix.width {
                if pix.get(x, y)[3] > 128 {
                    n += 1;
                }
            }
        }
        n
    }

    let s = count_opaque(solid);
    let d = count_opaque(dashed);
    let ratio = d as f32 / s.max(1) as f32;
    // Pattern is 4 on / 2 off -> about 2/3 lit.
    assert!(
        ratio > 0.55 && ratio < 0.85,
        "dashed/solid coverage ratio out of range: {ratio} (solid={s}, dashed={d})"
    );
    // Find at least one transparent pixel along y=50 between x=15
    // and x=85. Proves at least one "skip" gap was rendered.
    let bytes = build_svg_font(dashed);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();
    let mut saw_gap = false;
    for x in 15..85 {
        if pix.get(x, 50)[3] < 30 {
            saw_gap = true;
            break;
        }
    }
    assert!(saw_gap, "dashed line should contain at least one gap");
}

/// Dashed rectangle outline: `<rect stroke-dasharray="3 3" fill="none">`.
/// Each of the four edges should carry alternating dashes; total
/// stroke coverage should be roughly half of the un-dashed outline
/// (the pattern is 50/50).
#[test]
fn svg_rect_with_dasharray_strokes_all_four_edges() {
    let solid = b"<svg viewBox=\"0 0 100 100\">\
                  <rect x=\"20\" y=\"20\" width=\"60\" height=\"60\" \
                        stroke=\"#000\" stroke-width=\"4\" fill=\"none\"/>\
                  </svg>";
    let dashed = b"<svg viewBox=\"0 0 100 100\">\
                   <rect x=\"20\" y=\"20\" width=\"60\" height=\"60\" \
                         stroke=\"#000\" stroke-width=\"4\" fill=\"none\" \
                         stroke-dasharray=\"3 3\"/>\
                   </svg>";

    fn count_opaque(payload: &[u8]) -> u32 {
        let bytes = build_svg_font(payload);
        let blob = Blob::new(&bytes);
        let face = Face::parse(&blob, 0).unwrap();
        let rast = Rasterizer::new();
        let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();
        let mut n = 0u32;
        for y in 0..pix.height {
            for x in 0..pix.width {
                if pix.get(x, y)[3] > 128 {
                    n += 1;
                }
            }
        }
        n
    }

    let s = count_opaque(solid);
    let d = count_opaque(dashed);
    let ratio = d as f32 / s.max(1) as f32;
    // Dashes are 50/50 -> approx half coverage.
    assert!(
        ratio > 0.30 && ratio < 0.70,
        "dashed/solid rect coverage ratio out of range: {ratio} (solid={s}, dashed={d})"
    );
    assert!(d > 0, "dashed rect should have some opaque pixels");

    // Sample each of the four edges of the dashed pixmap. At least
    // one opaque pixel must exist on each edge (otherwise the dash
    // pattern silently dropped a side).
    let bytes = build_svg_font(dashed);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();
    let any_opaque = |xs: core::ops::RangeInclusive<u32>, ys: core::ops::RangeInclusive<u32>| {
        for y in ys.clone() {
            for x in xs.clone() {
                if pix.get(x, y)[3] > 128 {
                    return true;
                }
            }
        }
        false
    };
    assert!(any_opaque(20..=80, 19..=22), "top edge missing dashes");
    assert!(any_opaque(20..=80, 78..=81), "bottom edge missing dashes");
    assert!(any_opaque(19..=22, 20..=80), "left edge missing dashes");
    assert!(any_opaque(78..=81, 20..=80), "right edge missing dashes");
}
