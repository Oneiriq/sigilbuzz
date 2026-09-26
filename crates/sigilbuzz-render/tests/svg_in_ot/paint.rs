//! Strokes, gradients, `currentColor`, `<use>`, clipPath, and basic shape
//! primitives, plus oversized viewBox / size guards.

use sigilbuzz::{Blob, Face};
use sigilbuzz_render::{Rasterizer, RenderError};

use crate::fonts::build_svg_font;

// =========================================================================
// PR #205 deferral coverage: strokes, gradients, <use>, clipPath, shape
// primitives. Each feature gets a synthetic SVG payload + a render-side
// invariant that's hard to satisfy without the new code.
// =========================================================================

/// A diagonal stroke at 45° on a transparent background should leave a
/// row of opaque pixels along the line and nothing elsewhere. Proves
/// the stroke ribbon is built and rasterized through the fill pipeline.
#[test]
fn svg_stroke_paints_a_line() {
    let payload = b"<svg viewBox=\"0 0 100 100\">\
                    <path d=\"M 20 50 L 80 50\" stroke=\"#000\" stroke-width=\"4\" fill=\"none\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();

    // Pixels at y=50 between x=20 and x=80 must be near-opaque black.
    let mid = pix.get(50, 50);
    assert!(mid[3] > 200, "mid of stroke should be opaque, got {mid:?}");
    assert!(
        mid[0] < 30 && mid[1] < 30 && mid[2] < 30,
        "stroke colour should be black, got {mid:?}"
    );

    // Pixels well outside the stroke must stay transparent.
    let above = pix.get(50, 10);
    assert_eq!(
        above[3], 0,
        "above stroke must be transparent, got {above:?}"
    );
}

/// Linear gradient red to blue. Sampling the left edge should be red,
/// the right edge blue, and the middle should be a roughly even blend.
#[test]
fn svg_linear_gradient_ramps_red_to_blue() {
    let payload = b"<svg viewBox=\"0 0 100 100\">\
                    <defs>\
                      <linearGradient id=\"g\" x1=\"0\" y1=\"0\" x2=\"100\" y2=\"0\">\
                        <stop offset=\"0\" stop-color=\"#FF0000\"/>\
                        <stop offset=\"1\" stop-color=\"#0000FF\"/>\
                      </linearGradient>\
                    </defs>\
                    <rect x=\"0\" y=\"0\" width=\"100\" height=\"100\" fill=\"url(#g)\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();

    let left = pix.get(2, 50);
    let mid = pix.get(50, 50);
    let right = pix.get(97, 50);

    assert!(
        left[0] > 200 && left[2] < 50,
        "left edge should be red, got {left:?}"
    );
    assert!(
        right[2] > 200 && right[0] < 50,
        "right edge should be blue, got {right:?}"
    );
    // Mid should have noticeable contributions from both ends:
    // anti-aliased coverage, plus the blend ramp.
    assert!(
        mid[0] > 30 && mid[2] > 30,
        "midpoint should mix red and blue, got {mid:?}"
    );
}

/// Two `<use>` references to the same `<circle>` should produce two
/// disconnected filled regions in the bitmap.
#[test]
fn svg_use_replicates_referenced_shape() {
    let payload = b"<svg viewBox=\"0 0 100 100\">\
                    <defs><circle id=\"dot\" cx=\"0\" cy=\"0\" r=\"5\" fill=\"black\"/></defs>\
                    <use xlink:href=\"#dot\" x=\"20\" y=\"50\"/>\
                    <use xlink:href=\"#dot\" x=\"80\" y=\"50\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();

    let left = pix.get(20, 50);
    let right = pix.get(80, 50);
    let between = pix.get(50, 50);
    assert!(left[3] > 200, "left dot should be opaque, got {left:?}");
    assert!(right[3] > 200, "right dot should be opaque, got {right:?}");
    assert_eq!(
        between[3], 0,
        "gap between dots should be transparent, got {between:?}"
    );
}

/// A clipPath that's a circle should mask a full-rect fill into a
/// circular shape. Pixels inside the circle are filled; pixels outside
/// stay transparent.
#[test]
fn svg_clip_path_masks_rect_to_circle() {
    let payload = b"<svg viewBox=\"0 0 100 100\">\
                    <defs><clipPath id=\"c\"><circle cx=\"50\" cy=\"50\" r=\"20\"/></clipPath></defs>\
                    <rect x=\"0\" y=\"0\" width=\"100\" height=\"100\" fill=\"#000\" clip-path=\"url(#c)\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();

    // Center is inside the clip -> opaque.
    let centre = pix.get(50, 50);
    assert!(
        centre[3] > 200,
        "centre should be inside clip, got {centre:?}"
    );
    // Corner is outside the clip -> transparent.
    let corner = pix.get(5, 5);
    assert_eq!(
        corner[3], 0,
        "corner should be outside clip, got {corner:?}"
    );
    // Far edge of the rect (well outside the 20-radius circle) ->
    // transparent.
    let far = pix.get(90, 90);
    assert_eq!(far[3], 0, "far edge should be outside clip, got {far:?}");
}

/// `<rect>`, `<circle>`, `<ellipse>` each rendered as a stand-alone
/// shape. Verifies the path conversions plumb into the fill pipeline.
#[test]
fn svg_rect_circle_ellipse_render_as_filled_shapes() {
    fn count_filled(payload: &[u8]) -> u32 {
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
    }

    let rect = b"<svg viewBox=\"0 0 100 100\">\
                 <rect x=\"10\" y=\"10\" width=\"80\" height=\"80\" fill=\"black\"/>\
                 </svg>";
    let circle = b"<svg viewBox=\"0 0 100 100\">\
                   <circle cx=\"50\" cy=\"50\" r=\"40\" fill=\"black\"/>\
                   </svg>";
    let ellipse = b"<svg viewBox=\"0 0 100 100\">\
                    <ellipse cx=\"50\" cy=\"50\" rx=\"40\" ry=\"20\" fill=\"black\"/>\
                    </svg>";

    let r = count_filled(rect);
    let c = count_filled(circle);
    let e = count_filled(ellipse);
    // Rect 80x80 ~6400. Circle pi*40^2 ~5026. Ellipse pi*40*20 ~2513.
    assert!(r > 5500 && r < 7000, "rect coverage out of range: {r}");
    assert!(c > 4400 && c < 5600, "circle coverage out of range: {c}");
    assert!(e > 2100 && e < 2900, "ellipse coverage out of range: {e}");
}

/// Round-cornered rect emits cubic geometry. Coverage should be lower
/// than a sharp-cornered rect with the same outer bounds (the corners
/// are shaved off).
#[test]
fn svg_rect_with_rounded_corners_loses_corner_pixels() {
    let sharp = b"<svg viewBox=\"0 0 100 100\">\
                  <rect x=\"10\" y=\"10\" width=\"80\" height=\"80\" fill=\"black\"/>\
                  </svg>";
    let round = b"<svg viewBox=\"0 0 100 100\">\
                  <rect x=\"10\" y=\"10\" width=\"80\" height=\"80\" rx=\"20\" ry=\"20\" fill=\"black\"/>\
                  </svg>";

    fn count(payload: &[u8]) -> u32 {
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
    }
    let s = count(sharp);
    let r = count(round);
    assert!(
        r < s,
        "rounded rect should cover fewer pixels than sharp, sharp={s} round={r}"
    );
}

/// Issue #225: extreme finite `viewBox` + matching `size_pt` used to
/// land in `ColorPixmap::new(u32::MAX, u32::MAX)` and panic with
/// "capacity overflow" before any rasterization ran. The fix caps the
/// post-cast dimensions at 16384 (matching the PNG decoder's ceiling)
/// and surfaces the structured `BadSize` error instead.
#[test]
fn svg_extreme_viewbox_returns_bad_size_not_oom_panic() {
    // 1e30 viewBox with 1e30 size_pt -> scale s = 1, width_f = 1e30,
    // (width_f as u32) saturates to u32::MAX, and the destination
    // pixmap allocation would otherwise overflow.
    let payload =
        b"<svg viewBox=\"0 0 1e30 1e30\"><path d=\"M 0 0 L 1 0 L 1 1 Z\" fill=\"black\"/></svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_svg_glyph(&face, 1, 1e30, &[])
        .expect_err("extreme viewBox + size_pt must not OOM-panic");
    assert!(
        matches!(err, RenderError::BadSize(_)),
        "expected BadSize, got {err:?}"
    );
}

/// Companion to `svg_extreme_viewbox_returns_bad_size_not_oom_panic`:
/// a normally-sized viewBox with a hostile-but-still-finite `size_pt`
/// must also hit the dimension cap.
#[test]
fn svg_extreme_size_pt_returns_bad_size_not_oom_panic() {
    let payload =
        b"<svg viewBox=\"0 0 100 100\"><path d=\"M 0 0 L 1 0 L 1 1 Z\" fill=\"black\"/></svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_svg_glyph(&face, 1, 1.0e9, &[])
        .expect_err("extreme size_pt must not OOM-panic");
    assert!(
        matches!(err, RenderError::BadSize(_)),
        "expected BadSize, got {err:?}"
    );
}

/// `currentColor` is the rasterizer's foreground: the text color the
/// OpenType SVG spec hands a glyph document. A `color` attribute
/// changes it for its subtree, and gradient stops read it too.
#[test]
fn svg_current_color_is_the_rasterizer_foreground() {
    let payload = b"<svg viewBox=\"0 0 30 10\">\
                    <defs><linearGradient id=\"g\">\
                      <stop offset=\"0\" stop-color=\"currentColor\"/>\
                      <stop offset=\"1\" stop-color=\"currentColor\"/>\
                    </linearGradient></defs>\
                    <rect x=\"0\" y=\"0\" width=\"10\" height=\"10\" fill=\"currentColor\"/>\
                    <g color=\"#00FF00\">\
                      <rect x=\"10\" y=\"0\" width=\"10\" height=\"10\" fill=\"currentColor\"/>\
                    </g>\
                    <rect x=\"20\" y=\"0\" width=\"10\" height=\"10\" fill=\"url(#g)\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    let blue = Rasterizer::new().with_foreground([0, 0, 255, 255]);
    let pix = blue.rasterize_svg_glyph(&face, 1, 30.0, &[]).unwrap();
    assert_eq!((pix.width, pix.height), (30, 10));
    assert_eq!(pix.get(5, 5), [0, 0, 255, 255], "foreground fill");
    assert_eq!(pix.get(15, 5), [0, 255, 0, 255], "color attribute");
    assert_eq!(pix.get(25, 5), [0, 0, 255, 255], "gradient stops");

    // The default foreground is opaque black.
    let pix = Rasterizer::new()
        .rasterize_svg_glyph(&face, 1, 30.0, &[])
        .unwrap();
    assert_eq!(pix.get(5, 5), [0, 0, 0, 255]);
}
