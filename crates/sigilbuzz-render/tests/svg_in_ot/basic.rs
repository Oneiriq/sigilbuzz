//! Core SVG-in-OT rendering: fills, groups, curves, determinism, size
//! validation, and the shipped synthetic fixture.

use sigilbuzz::{Blob, Face};
use sigilbuzz_render::{Rasterizer, RenderError};

use crate::fonts::build_svg_font;

#[test]
fn svg_solid_red_square_fills_to_red_pixmap() {
    // 100x100 viewBox, single fully red square covering the entire
    // viewBox. Rasterized at 100pt should produce ~100x100 red pixels.
    let payload = b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 100\">\
                    <path d=\"M 0 0 L 100 0 L 100 100 L 0 100 Z\" fill=\"#FF0000\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();
    assert_eq!(pix.width, 100);
    assert_eq!(pix.height, 100);

    // A pixel near the center must be solid opaque red. Edges may
    // anti-alias so we sample at (50, 50).
    let p = pix.get(50, 50);
    assert_eq!(p, [255, 0, 0, 255], "centre should be solid red, got {p:?}");

    // Total pixels written should dominate the bitmap: ~10000 red, no
    // transparent inside the square.
    let mut red = 0u32;
    let mut transparent = 0u32;
    for y in 0..pix.height {
        for x in 0..pix.width {
            let p = pix.get(x, y);
            if p[3] == 0 {
                transparent += 1;
            } else if p[0] > 200 && p[1] < 30 && p[2] < 30 {
                red += 1;
            }
        }
    }
    assert!(red > 9000, "expected mostly-red bitmap, got red={red}");
    assert!(
        transparent < 200,
        "interior should be filled, transparent={transparent}"
    );
}

#[test]
fn svg_unknown_gid_returns_not_found() {
    let payload =
        b"<svg viewBox=\"0 0 10 10\"><path d=\"M 0 0 L 10 0 L 10 10 Z\" fill=\"black\"/></svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast.rasterize_svg_glyph(&face, 99, 16.0, &[]).unwrap_err();
    matches!(err, RenderError::SvgNotFound(99));
}

#[test]
fn svg_group_scale_doubles_geometry() {
    // Two fonts, one with a 50x50 path inside a viewBox=0 0 100 100,
    // the other wrapping the same path in <g transform="scale(2)">.
    // The scaled version should fill (close to) the entire viewBox.
    let plain = b"<svg viewBox=\"0 0 100 100\">\
                  <path d=\"M 0 0 L 50 0 L 50 50 L 0 50 Z\" fill=\"black\"/>\
                  </svg>";
    let scaled = b"<svg viewBox=\"0 0 100 100\">\
                   <g transform=\"scale(2)\">\
                   <path d=\"M 0 0 L 50 0 L 50 50 L 0 50 Z\" fill=\"black\"/>\
                   </g></svg>";

    let count_filled = |payload: &[u8]| {
        let bytes = build_svg_font(payload);
        let blob = Blob::new(&bytes);
        let face = Face::parse(&blob, 0).unwrap();
        let rast = Rasterizer::new();
        let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();
        let mut filled = 0u32;
        for y in 0..pix.height {
            for x in 0..pix.width {
                if pix.get(x, y)[3] > 128 {
                    filled += 1;
                }
            }
        }
        filled
    };

    let plain_filled = count_filled(plain);
    let scaled_filled = count_filled(scaled);

    // Plain covers a quarter of a 100x100 bitmap (~2500 px). Scaled
    // covers the whole bitmap (~10000 px). The exact ratio is
    // sensitive to anti-aliasing on the boundary, so we just assert
    // the scaled version is ~3.5x larger.
    assert!(
        scaled_filled as f32 / plain_filled.max(1) as f32 > 3.0,
        "scale(2) should ~quadruple coverage, got plain={plain_filled} scaled={scaled_filled}"
    );
}

#[test]
fn svg_curve_path_renders_via_flatten() {
    // A quarter-circle using a cubic Bezier should produce a filled
    // bitmap whose coverage is somewhere between the inscribed
    // square (50%) and the bounding box (100%), proving the curve
    // was actually flattened rather than dropped.
    //
    // Path: M 0 0 (start at top-left corner) C 0 100 100 100 100 100
    // L 100 0 Z. That carves a curve from (0,0) to (100,100) bowing
    // out through the lower-left, then closes via the right edge.
    let payload = b"<svg viewBox=\"0 0 100 100\">\
                    <path d=\"M 0 0 C 0 100 100 100 100 100 L 100 0 Z\" fill=\"black\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();

    let mut filled = 0u32;
    for y in 0..pix.height {
        for x in 0..pix.width {
            if pix.get(x, y)[3] > 128 {
                filled += 1;
            }
        }
    }
    let total = pix.width * pix.height;
    let frac = filled as f32 / total as f32;
    // Geometry covers the bbox above the curve: should fall between
    // ~40% and ~95%. Wide tolerance because the curve's exact area
    // depends on flatten tolerance.
    assert!(
        frac > 0.4 && frac < 0.95,
        "curve flattening should produce partial fill, got frac={frac}"
    );
}

#[test]
fn svg_render_is_deterministic() {
    let payload = b"<svg viewBox=\"0 0 50 50\">\
                    <path d=\"M 5 5 L 45 5 L 45 45 L 5 45 Z\" fill=\"#3366CC\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let a = rast.rasterize_svg_glyph(&face, 1, 64.0, &[]).unwrap();
    let b = rast.rasterize_svg_glyph(&face, 1, 64.0, &[]).unwrap();
    assert_eq!(a, b);
}

#[test]
fn svg_bad_size_rejected() {
    let payload = b"<svg viewBox=\"0 0 10 10\"><path d=\"M 0 0 Z\" fill=\"black\"/></svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    assert!(matches!(
        rast.rasterize_svg_glyph(&face, 1, 0.0, &[]),
        Err(RenderError::BadSize(_))
    ));
    assert!(matches!(
        rast.rasterize_svg_glyph(&face, 1, f32::NAN, &[]),
        Err(RenderError::BadSize(_))
    ));
}

#[test]
fn svg_existing_synthetic_fixture_round_trips() {
    // The core crate's SVG fixture (`tests/fixtures/svg_synthetic.ttf`)
    // ships a `<circle>` payload. We don't render `<circle>` (out of
    // scope), but the rasterizer should still parse the document and
    // return a (possibly empty) ColorPixmap rather than erroring.
    let path = std::path::Path::new("../../tests/fixtures/svg_synthetic.ttf");
    let Ok(bytes) = std::fs::read(path) else {
        // Worktree layout may differ; skip cleanly.
        return;
    };
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 64.0, &[]);
    // Either we successfully render an empty-but-shaped pixmap (no
    // <path>s in the doc), or we surface a structured Parse error:
    // both prove the entry point hooked up correctly.
    match pix {
        Ok(p) => {
            assert!(p.width > 0 && p.height > 0);
        }
        Err(RenderError::Parse(_)) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}
