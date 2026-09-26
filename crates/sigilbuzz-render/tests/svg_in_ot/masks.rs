//! Luminance `<mask>` rendering.

use sigilbuzz::{Blob, Face};
use sigilbuzz_render::Rasterizer;

use crate::fonts::build_svg_font;

/// Generic alpha mask: a luminance mask carrying a black circle on a
/// white background should cut a circular hole out of an underlying
/// red square. Pixels inside the black-circle region drop to alpha=0;
/// pixels in the white surround remain opaque red.
///
/// This is the spec-quoted minimum-viable mask test from PR #205 /
/// #219's deferral: the discriminating output is "red square with a
/// circular cutout", which proves both luminance derivation
/// (white->1.0, black->0.0) and per-pixel alpha multiplication.
#[test]
fn svg_mask_cuts_circular_hole_in_red_square() {
    let payload = b"<svg viewBox=\"0 0 100 100\">\
                    <defs>\
                      <mask id=\"m\">\
                        <rect x=\"0\" y=\"0\" width=\"100\" height=\"100\" fill=\"white\"/>\
                        <circle cx=\"50\" cy=\"50\" r=\"30\" fill=\"black\"/>\
                      </mask>\
                    </defs>\
                    <rect x=\"0\" y=\"0\" width=\"100\" height=\"100\" fill=\"red\" mask=\"url(#m)\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();

    // Center of the black circle -> fully masked out (alpha = 0).
    let centre = pix.get(50, 50);
    assert_eq!(
        centre[3], 0,
        "centre of mask black region should be transparent, got {centre:?}"
    );

    // Corner of the rect (well outside the circle, in the mask's
    // white surround) -> opaque red, mask alpha is 1.0.
    let corner = pix.get(5, 5);
    assert!(
        corner[3] > 200 && corner[0] > 200 && corner[1] < 30 && corner[2] < 30,
        "corner should be opaque red, got {corner:?}"
    );

    // A point just inside the circle's edge should still be
    // transparent (mask black center is a luminance-zero region).
    let inside = pix.get(50, 30);
    assert!(
        inside[3] < 50,
        "inside circle region should be near-transparent, got {inside:?}"
    );
}

/// Mask with `mask=url(#missing)`: the masked element should still
/// render as if no mask were applied. Matches the clipPath / filter
/// degrade-gracefully policy.
#[test]
fn svg_mask_missing_id_renders_unmasked() {
    let payload = b"<svg viewBox=\"0 0 100 100\">\
                    <rect x=\"0\" y=\"0\" width=\"100\" height=\"100\" fill=\"red\" mask=\"url(#nope)\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();
    let p = pix.get(50, 50);
    assert_eq!(
        p,
        [255, 0, 0, 255],
        "missing mask id should leave fill unchanged, got {p:?}"
    );
}

/// Mask with a half-luminance gray body should produce a half-opaque
/// red square. The luminance derivation must produce a *continuous*
/// alpha multiplier rather than the binary in/out a clipPath would.
/// This is the discriminating signal between mask and clipPath
/// handling.
#[test]
fn svg_mask_grey_body_produces_partial_alpha() {
    // Pure-luminance gray (#808080) -> BT.709 luminance ~0.502, so
    // we expect alpha ~128 over the rect.
    let payload = b"<svg viewBox=\"0 0 50 50\">\
                    <defs>\
                      <mask id=\"g\">\
                        <rect x=\"0\" y=\"0\" width=\"50\" height=\"50\" fill=\"#808080\"/>\
                      </mask>\
                    </defs>\
                    <rect x=\"0\" y=\"0\" width=\"50\" height=\"50\" fill=\"red\" mask=\"url(#g)\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 50.0, &[]).unwrap();
    let p = pix.get(25, 25);
    // Alpha should land near the half-luminance band (allow plenty of
    // slack for rounding).
    assert!(
        p[3] > 100 && p[3] < 160,
        "grey mask should produce ~half alpha, got {p:?}"
    );
    // Source color was opaque red; the surviving pixel should be a
    // ~half-opaque premultiplied red.
    assert!(p[0] > 100 && p[1] < 30 && p[2] < 30, "channels: {p:?}");
}
