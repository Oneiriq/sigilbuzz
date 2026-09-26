//! Adversarial inputs for deferred mask and textPath features that
//! must degrade without panicking.

use sigilbuzz::{Blob, Face};
use sigilbuzz_render::{Rasterizer, RenderError};

use crate::fonts::build_svg_font;

// =========================================================================
// Wave-21 adversarial pass: sibling-feature deferral surfaces.
//
// The named features (mask-type=alpha, maskUnits=objectBoundingBox,
// <textPath>) are currently *deferred* on release/0.21.0. Siblings own
// the implementation. These tests pin the documented degrade-to-default
// behavior so that:
//
//   (a) on the current release tip, malformed adversarial input does
//       not panic / propagate parse errors, and
//   (b) when the sibling PRs land, any change in semantics is caught
//       at the existing assertions (a sibling that flips behavior from
//       "render unmasked" to "panic on empty body" would fail here).
// =========================================================================

/// `mask-type="alpha"` is documented as deferred. Falls back to the
/// luminance default. An *empty* mask body should still parse cleanly
/// and degrade gracefully (the masked element renders as if the mask
/// wasn't applied, since `resolve_mask_shape` returns `None` for an
/// empty body and `emit_paint` drops the mask href on a `None`
/// resolution).
#[test]
fn svg_mask_alpha_with_empty_body_does_not_panic() {
    let payload = b"<svg viewBox=\"0 0 50 50\">\
                    <defs>\
                      <mask id=\"empty\" mask-type=\"alpha\"></mask>\
                    </defs>\
                    <rect x=\"0\" y=\"0\" width=\"50\" height=\"50\" fill=\"red\" mask=\"url(#empty)\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    // Must not panic and must produce *some* output. With an empty
    // mask body the masked rect renders unmasked (red opaque).
    let pix = rast
        .rasterize_svg_glyph(&face, 1, 50.0, &[])
        .expect("empty mask body should degrade, not error");
    let p = pix.get(25, 25);
    assert_eq!(
        p[3], 255,
        "empty mask body should leave fill opaque, got alpha={}",
        p[3]
    );
}

/// `maskUnits="objectBoundingBox"` with a zero-area shape's bbox.
/// Currently deferred, falls back to userSpaceOnUse. The assertion
/// here is purely structural: rasterizer doesn't panic, doesn't divide
/// by zero, and returns a finite pixmap.
#[test]
fn svg_mask_object_bounding_box_with_zero_size_target_does_not_panic() {
    // The masked <rect> has zero width: bbox is degenerate. A naive
    // objectBoundingBox implementation that scales by `1/bbox_w` would
    // hit /0 here.
    let payload = b"<svg viewBox=\"0 0 100 100\">\
                    <defs>\
                      <mask id=\"obb\" maskUnits=\"objectBoundingBox\">\
                        <rect x=\"0\" y=\"0\" width=\"1\" height=\"1\" fill=\"white\"/>\
                      </mask>\
                    </defs>\
                    <rect x=\"50\" y=\"0\" width=\"0\" height=\"100\" fill=\"red\" mask=\"url(#obb)\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    // Should produce a pixmap (possibly fully transparent: zero-width
    // rect emits nothing) without panicking.
    let pix = rast
        .rasterize_svg_glyph(&face, 1, 100.0, &[])
        .expect("zero-bbox + objectBoundingBox should not error");
    assert!(pix.width > 0 && pix.height > 0);
}

/// `<textPath>` is documented as unsupported on release/0.21.0. The
/// rasterizer must skip the element silently and return an empty
/// pixmap rather than panic. Sibling PR will replace this assertion.
#[test]
fn svg_textpath_empty_text_does_not_panic() {
    let payload = b"<svg viewBox=\"0 0 100 100\">\
                    <defs><path id=\"p\" d=\"M 10 50 L 90 50\"/></defs>\
                    <text><textPath href=\"#p\"></textPath></text>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]);
    // Either an empty pixmap or a structured Parse error, never a
    // panic. (BadSvg/Parse counts as 'structured' here.)
    match pix {
        Ok(p) => assert!(p.width > 0 && p.height > 0),
        Err(RenderError::Parse(_)) => {}
        Err(other) => panic!("unexpected: {other:?}"),
    }
}

/// `<textPath>` whose `href` points at a non-existent id. Sibling work
/// should resolve to "no glyphs emitted"; today it falls through to
/// the unsupported-text path and renders nothing. Either way: no panic.
#[test]
fn svg_textpath_dangling_href_does_not_panic() {
    let payload = b"<svg viewBox=\"0 0 100 100\">\
                    <text><textPath href=\"#does-not-exist\">hello</textPath></text>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]);
    match pix {
        Ok(p) => assert!(p.width > 0 && p.height > 0),
        Err(RenderError::Parse(_)) => {}
        Err(other) => panic!("unexpected: {other:?}"),
    }
}

/// `<textPath>` whose path is zero-length (M with no following
/// segments). Naive arc-length code might 0/0; we want a no-panic
/// guarantee.
#[test]
fn svg_textpath_zero_length_path_does_not_panic() {
    let payload = b"<svg viewBox=\"0 0 100 100\">\
                    <defs><path id=\"p\" d=\"M 50 50\"/></defs>\
                    <text><textPath href=\"#p\">x</textPath></text>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]);
    match pix {
        Ok(p) => assert!(p.width > 0 && p.height > 0),
        Err(RenderError::Parse(_)) => {}
        Err(other) => panic!("unexpected: {other:?}"),
    }
}

/// Adversarial: a `<mask>` containing only a *nested* `<mask>`
/// reference. The deferred `mask-of-mask` case must not recurse
/// infinitely or panic. `resolve_mask_shape` strips inner mask refs.
#[test]
fn svg_mask_of_mask_does_not_recurse_forever() {
    let payload = b"<svg viewBox=\"0 0 50 50\">\
                    <defs>\
                      <mask id=\"a\">\
                        <rect x=\"0\" y=\"0\" width=\"50\" height=\"50\" fill=\"white\" mask=\"url(#b)\"/>\
                      </mask>\
                      <mask id=\"b\">\
                        <rect x=\"0\" y=\"0\" width=\"50\" height=\"50\" fill=\"white\" mask=\"url(#a)\"/>\
                      </mask>\
                    </defs>\
                    <rect x=\"0\" y=\"0\" width=\"50\" height=\"50\" fill=\"red\" mask=\"url(#a)\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast
        .rasterize_svg_glyph(&face, 1, 50.0, &[])
        .expect("mask-of-mask cycle should be defused, not panic");
    assert!(pix.width > 0 && pix.height > 0);
}

/// Adversarial: a `<filter>` whose first primitive references an
/// unknown named result. `apply_filter` falls back to a same-size
/// transparent pixmap for unknown names, no panic on `unwrap_or_else`.
#[test]
fn svg_filter_with_unknown_named_input_does_not_panic() {
    let payload = b"<svg viewBox=\"0 0 50 50\">\
                    <defs>\
                      <filter id=\"f\">\
                        <feOffset in=\"DoesNotExist\" dx=\"5\" dy=\"5\"/>\
                      </filter>\
                    </defs>\
                    <rect x=\"0\" y=\"0\" width=\"50\" height=\"50\" fill=\"red\" filter=\"url(#f)\"/>\
                    </svg>";
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast
        .rasterize_svg_glyph(&face, 1, 50.0, &[])
        .expect("unknown filter input should fall back, not panic");
    assert!(pix.width > 0 && pix.height > 0);
}
