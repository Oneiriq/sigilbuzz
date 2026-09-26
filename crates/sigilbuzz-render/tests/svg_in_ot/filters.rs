//! SVG filter primitives: blur, color matrix, offset, flood / merge,
//! and drop shadow chains.

use sigilbuzz::{Blob, Face};
use sigilbuzz_render::Rasterizer;

use crate::fonts::build_svg_font;

// =========================================================================
// Filter primitives
// =========================================================================

/// Counts pixels in `pix` that satisfy the predicate.
fn count_pixels<F>(pix: &sigilbuzz_render::ColorPixmap, pred: F) -> u32
where
    F: Fn([u8; 4]) -> bool,
{
    let mut n = 0u32;
    for y in 0..pix.height {
        for x in 0..pix.width {
            if pred(pix.get(x, y)) {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn filter_gaussian_blur_softens_sharp_edges() {
    // 50x50 black square centered in 100x100. Without blur, the edge
    // is a hard step from alpha=255 to alpha=0. With stdDeviation=4 the
    // edge becomes a gradient: pixels just outside the square pick up
    // partial alpha.
    let payload = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
        <defs>
            <filter id="b"><feGaussianBlur in="SourceGraphic" stdDeviation="4"/></filter>
        </defs>
        <rect x="25" y="25" width="50" height="50" fill="#000000" filter="url(#b)"/>
    </svg>"##;
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();

    // Outside the original square but still within the blur halo:
    // alpha should be > 0 and < 255.
    let halo = pix.get(15, 50);
    assert!(
        halo[3] > 0 && halo[3] < 255,
        "blur halo pixel should have partial alpha, got {halo:?}"
    );
    // Center stays opaque-ish.
    let mid = pix.get(50, 50);
    assert!(mid[3] > 200, "centre should stay opaque, got {mid:?}");
}

#[test]
fn filter_color_matrix_saturate_zero_yields_grey() {
    // A red square run through saturate=0 must emerge gray (R==G==B).
    let payload = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 50 50">
        <defs>
            <filter id="g">
                <feColorMatrix in="SourceGraphic" type="saturate" values="0"/>
            </filter>
        </defs>
        <rect x="0" y="0" width="50" height="50" fill="#FF0000" filter="url(#g)"/>
    </svg>"##;
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 50.0, &[]).unwrap();
    let p = pix.get(25, 25);
    // R, G, B should be near each other (within rounding noise).
    let max = p[0].max(p[1]).max(p[2]);
    let min = p[0].min(p[1]).min(p[2]);
    assert!(max - min <= 4, "saturate=0 should produce grey, got {p:?}");
    // And the alpha is opaque.
    assert!(p[3] > 250, "alpha lost: {p:?}");
}

#[test]
fn filter_color_matrix_hue_rotate_180_inverts_hue() {
    // Red rotated 180 degrees lands roughly in cyan space.
    let payload = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 50 50">
        <defs>
            <filter id="h">
                <feColorMatrix in="SourceGraphic" type="hueRotate" values="180"/>
            </filter>
        </defs>
        <rect x="0" y="0" width="50" height="50" fill="#FF0000" filter="url(#h)"/>
    </svg>"##;
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 50.0, &[]).unwrap();
    let p = pix.get(25, 25);
    // The dominant channel should no longer be R. G and B should
    // dominate over R after a 180-degree hue rotation of pure red.
    assert!(
        (p[1] as i32 + p[2] as i32) > p[0] as i32,
        "hueRotate(180) of red should shift toward cyan, got {p:?}"
    );
}

#[test]
fn filter_offset_translates_output() {
    // Black 10x10 square at (0,0) offset by +20,+20 should land at
    // roughly (20,20)..(30,30) with the original location empty.
    let payload = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 50 50">
        <defs>
            <filter id="o"><feOffset in="SourceGraphic" dx="20" dy="20"/></filter>
        </defs>
        <rect x="0" y="0" width="10" height="10" fill="#000000" filter="url(#o)"/>
    </svg>"##;
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 50.0, &[]).unwrap();

    // Original (5,5) is now empty.
    assert_eq!(
        pix.get(5, 5)[3],
        0,
        "offset should clear the original square"
    );
    // Shifted destination (25,25) is opaque.
    assert!(
        pix.get(25, 25)[3] > 200,
        "offset destination should be opaque, got {:?}",
        pix.get(25, 25)
    );
}

#[test]
fn filter_flood_plus_merge_under_source_yields_drop_shadow() {
    // The classic feFlood + feMerge drop-shadow chain:
    //   feFlood color -> flood result
    //   feMerge: flood, SourceGraphic -> composite
    // Both layers cover the canvas; we verify the source is on top
    // (visible at the rect) and the flood is visible elsewhere.
    let payload = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 50 50">
        <defs>
            <filter id="dt">
                <feFlood flood-color="#0000FF" flood-opacity="0.5" result="bg"/>
                <feMerge>
                    <feMergeNode in="bg"/>
                    <feMergeNode in="SourceGraphic"/>
                </feMerge>
            </filter>
        </defs>
        <rect x="20" y="20" width="10" height="10" fill="#FF0000" filter="url(#dt)"/>
    </svg>"##;
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 50.0, &[]).unwrap();

    // Inside the source rect: red dominates (source on top).
    let inside = pix.get(25, 25);
    assert!(
        inside[0] > 150 && inside[2] < 100,
        "source should sit on top, got {inside:?}"
    );
    // Outside the rect: blue dominates (flood layer).
    let outside = pix.get(5, 5);
    assert!(
        outside[2] > 50,
        "flood layer should fill the background, got {outside:?}"
    );
}

#[test]
fn filter_drop_shadow_chain_produces_offset_blur_under_source() {
    // End-to-end drop-shadow: SourceAlpha -> Gaussian blur -> offset
    // (positive dx,dy) -> flood-colored shadow merged under the
    // SourceGraphic.
    let payload = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
        <defs>
            <filter id="ds">
                <feGaussianBlur in="SourceAlpha" stdDeviation="3" result="blur"/>
                <feOffset in="blur" dx="6" dy="6" result="off"/>
                <feMerge>
                    <feMergeNode in="off"/>
                    <feMergeNode in="SourceGraphic"/>
                </feMerge>
            </filter>
        </defs>
        <rect x="20" y="20" width="40" height="40" fill="#FF0000" filter="url(#ds)"/>
    </svg>"##;
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap();

    // Original rect interior: solid red on top.
    let src = pix.get(40, 40);
    assert!(
        src[0] > 200 && src[3] > 200,
        "source rect should still be solid red on top, got {src:?}"
    );
    // Shadow shows as semi-transparent dark pixels in the
    // bottom-right halo (just past the rect edge).
    let shadow_count = count_pixels(&pix, |p| {
        // Grayish (low chroma), partially opaque shadow.
        let max = p[0].max(p[1]).max(p[2]);
        p[3] > 0 && p[3] < 250 && max < 50
    });
    assert!(
        shadow_count > 50,
        "expected a blurred-offset shadow halo, found {shadow_count} shadow pixels"
    );

    // The shadow should land *below-right* of the rect, not above-left.
    // Sample (70, 70) should have non-zero alpha; (10, 10) should be
    // empty (no shadow there).
    assert!(
        pix.get(70, 70)[3] > 0,
        "shadow should reach bottom-right, got {:?}",
        pix.get(70, 70)
    );
    assert_eq!(
        pix.get(10, 10)[3],
        0,
        "no shadow expected above-left, got {:?}",
        pix.get(10, 10)
    );
}

#[test]
fn filter_color_matrix_luminance_to_alpha() {
    // luminanceToAlpha drops the color channels and writes luminance
    // into alpha. A bright source yields a gray-ish opaque pixel
    // (R=G=B=0, A=luma scaled, but we render premul, so all channels
    // end up zero with positive alpha).
    let payload = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 50 50">
        <defs>
            <filter id="lt">
                <feColorMatrix in="SourceGraphic" type="luminanceToAlpha"/>
            </filter>
        </defs>
        <rect x="0" y="0" width="50" height="50" fill="#FFFFFF" filter="url(#lt)"/>
    </svg>"##;
    let bytes = build_svg_font(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast.rasterize_svg_glyph(&face, 1, 50.0, &[]).unwrap();
    let p = pix.get(25, 25);
    // RGB should be 0 (luminanceToAlpha zeroes them); alpha is the
    // computed luma of white = 1.0 -> 255.
    assert_eq!(p[0], 0, "RGB should be zeroed, got {p:?}");
    assert!(p[3] > 200, "alpha should track luma, got {p:?}");
}
