//! Tests for `<mask>` parsing and application.

use super::*;

use crate::svg::clip_mask::apply_mask;
use crate::svg::model::{MaskShape, MaskType, MaskUnits};

#[test]
fn mask_attaches_to_referencing_fill() {
    // <mask> with a luminance body: black circle on white square.
    // The fill that references it should carry a non-empty
    // MaskShape with both child fills harvested.
    let xml = r##"<svg viewBox="0 0 100 100">
        <defs>
            <mask id="m">
                <rect x="0" y="0" width="100" height="100" fill="white"/>
                <circle cx="50" cy="50" r="30" fill="black"/>
            </mask>
        </defs>
        <rect x="0" y="0" width="100" height="100" fill="red" mask="url(#m)"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    assert_eq!(doc.fills.len(), 1);
    let m = doc.fills[0].mask.as_ref().expect("mask attached");
    assert_eq!(m.fills.len(), 2, "mask should carry rect + circle fills");
}

#[test]
fn mask_unknown_id_silently_drops() {
    // Bad reference falls back to "no mask" (matches the
    // clip-path / filter degrade-gracefully policy).
    let xml = r##"<svg viewBox="0 0 10 10">
        <rect x="0" y="0" width="10" height="10" fill="#000" mask="url(#missing)"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    assert_eq!(doc.fills.len(), 1);
    assert!(doc.fills[0].mask.is_none());
}

#[test]
fn mask_does_not_emit_a_top_level_fill() {
    // The <mask> element itself must NOT emit fills into the
    // document (it's a definition, not a render target). Only the
    // top-level <rect> referencing it should produce a fill.
    let xml = r##"<svg viewBox="0 0 100 100">
        <mask id="m">
            <rect x="0" y="0" width="100" height="100" fill="white"/>
        </mask>
        <rect x="0" y="0" width="100" height="100" fill="red" mask="url(#m)"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    assert_eq!(
        doc.fills.len(),
        1,
        "mask body must not contribute top-level fills"
    );
}

#[test]
fn mask_of_mask_is_dropped() {
    // Nested masks are unsupported: a mask whose body references
    // another mask must drop the inner reference at resolve time.
    let xml = r##"<svg viewBox="0 0 100 100">
        <defs>
            <mask id="inner">
                <rect x="0" y="0" width="100" height="100" fill="white"/>
            </mask>
            <mask id="outer">
                <rect x="0" y="0" width="100" height="100" fill="white" mask="url(#inner)"/>
            </mask>
        </defs>
        <rect x="0" y="0" width="100" height="100" fill="red" mask="url(#outer)"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    let outer = doc.fills[0].mask.as_ref().expect("outer mask attached");
    // Outer's child fill must NOT carry a nested mask reference.
    assert!(outer.fills.iter().all(|f| f.mask.is_none()));
}

#[test]
fn mask_type_defaults_to_luminance() {
    // No `mask-type=` attribute -> MaskType::Luminance, matching
    // the SVG spec default and the PR #236 baseline.
    let xml = r##"<svg viewBox="0 0 10 10">
        <defs>
            <mask id="m">
                <rect x="0" y="0" width="10" height="10" fill="white"/>
            </mask>
        </defs>
        <rect x="0" y="0" width="10" height="10" fill="red" mask="url(#m)"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    let m = doc.fills[0].mask.as_ref().unwrap();
    assert_eq!(m.mask_type, MaskType::Luminance);
    assert_eq!(m.units, MaskUnits::UserSpaceOnUse);
}

#[test]
fn mask_type_alpha_is_parsed() {
    // `mask-type="alpha"` opts into the alpha-channel-direct path.
    let xml = r##"<svg viewBox="0 0 10 10">
        <defs>
            <mask id="m" mask-type="alpha">
                <rect x="0" y="0" width="10" height="10" fill="black" fill-opacity="0.5"/>
            </mask>
        </defs>
        <rect x="0" y="0" width="10" height="10" fill="red" mask="url(#m)"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    let m = doc.fills[0].mask.as_ref().unwrap();
    assert_eq!(m.mask_type, MaskType::Alpha);
}

#[test]
fn mask_units_object_bounding_box_is_parsed() {
    // `maskUnits="objectBoundingBox"` plus a region rect must round-
    // trip through resolve_mask_shape.
    let xml = r##"<svg viewBox="0 0 100 100">
        <defs>
            <mask id="m" maskUnits="objectBoundingBox" x="0.25" y="0.25" width="0.5" height="0.5">
                <rect x="0" y="0" width="100" height="100" fill="white"/>
            </mask>
        </defs>
        <rect x="0" y="0" width="100" height="100" fill="red" mask="url(#m)"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    let m = doc.fills[0].mask.as_ref().unwrap();
    assert_eq!(m.units, MaskUnits::ObjectBoundingBox);
    assert!((m.region_x - 0.25).abs() < 1e-5);
    assert!((m.region_y - 0.25).abs() < 1e-5);
    assert!((m.region_w - 0.5).abs() < 1e-5);
    assert!((m.region_h - 0.5).abs() < 1e-5);
}

#[test]
fn apply_mask_alpha_uses_alpha_channel_directly() {
    // mask-type="alpha" means: ignore RGB luminance, sample the
    // mask buffer's alpha channel directly. A mask body that paints
    // opaque BLACK (luminance = 0, alpha = 255) would zero the
    // output under luminance, but must keep it under alpha.
    let world = Affine::identity();
    let mut dst = ColorPixmap::new(4, 4);
    // Fill dst with opaque red (premultiplied: r=255, a=255).
    for px in dst.data.chunks_exact_mut(4) {
        px[0] = 255;
        px[1] = 0;
        px[2] = 0;
        px[3] = 255;
    }
    // Mask body: a black rect that fully covers the canvas.
    let ops = vec![
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 4.0, y: 0.0 },
        PathOp::LineTo { x: 4.0, y: 4.0 },
        PathOp::LineTo { x: 0.0, y: 4.0 },
        PathOp::Close,
    ];
    let body_fill = Fill {
        ops,
        paint: Paint::Solid([0, 0, 0, 255]),
        xform: Affine::identity(),
        clip: None,
        is_stroke: false,
        filter: None,
        mask: None,
    };
    let mask_shape = MaskShape {
        fills: vec![body_fill],
        mask_type: MaskType::Alpha,
        units: MaskUnits::UserSpaceOnUse,
        region_x: 0.0,
        region_y: 0.0,
        region_w: 1.0,
        region_h: 1.0,
    };
    apply_mask(&mut dst, &mask_shape, &world, 0.25);
    // Under alpha-mode the opaque-black mask body keeps every dst
    // pixel intact (alpha = 255 -> m = 255). Under luminance it
    // would have zeroed the pixels.
    for px in dst.data.chunks_exact(4) {
        assert_eq!(px[0], 255, "alpha-mask kept red channel intact");
        assert_eq!(px[3], 255, "alpha-mask kept dst alpha intact");
    }
}

#[test]
fn apply_mask_object_bounding_box_clips_to_region() {
    // maskUnits="objectBoundingBox" with x=0.25 y=0.25 w=0.5 h=0.5
    // on a 100x100 opaque rect: only the [25, 75) x [25, 75) pixel
    // region survives; everything outside is zeroed.
    let world = Affine::identity();
    let mut dst = ColorPixmap::new(100, 100);
    for px in dst.data.chunks_exact_mut(4) {
        px[0] = 255;
        px[1] = 0;
        px[2] = 0;
        px[3] = 255;
    }
    let ops = vec![
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 100.0, y: 0.0 },
        PathOp::LineTo { x: 100.0, y: 100.0 },
        PathOp::LineTo { x: 0.0, y: 100.0 },
        PathOp::Close,
    ];
    let body_fill = Fill {
        ops,
        paint: Paint::Solid([255, 255, 255, 255]),
        xform: Affine::identity(),
        clip: None,
        is_stroke: false,
        filter: None,
        mask: None,
    };
    let mask_shape = MaskShape {
        fills: vec![body_fill],
        mask_type: MaskType::Luminance,
        units: MaskUnits::ObjectBoundingBox,
        region_x: 0.25,
        region_y: 0.25,
        region_w: 0.5,
        region_h: 0.5,
    };
    apply_mask(&mut dst, &mask_shape, &world, 0.25);
    // Inside the [25, 75) box: pixels survive (white luminance *
    // opaque alpha = 255 -> unchanged premultiplied red).
    let inside = dst.get(50, 50);
    assert_eq!(inside, [255, 0, 0, 255]);
    // Outside the box: forced to zero.
    let outside_tl = dst.get(5, 5);
    let outside_br = dst.get(95, 95);
    assert_eq!(outside_tl, [0, 0, 0, 0]);
    assert_eq!(outside_br, [0, 0, 0, 0]);
    // Just outside the upper-left region edge.
    let edge_just_outside = dst.get(24, 24);
    assert_eq!(edge_just_outside, [0, 0, 0, 0]);
}
