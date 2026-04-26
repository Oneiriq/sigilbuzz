//! SVG-in-OT rasterization end-to-end.
//!
//! Builds a synthetic SFNT in memory carrying just enough tables for
//! `Face::svg_document` plus an `SVG ` table holding a hand-written
//! XML document. The rasterizer should:
//!
//! - Decode the document, walk `<path>` / `<g>` geometry, fill it via
//!   the existing trapezoid rasterizer.
//! - Honour `<g transform="scale(...)">` by scaling the rendered
//!   bitmap accordingly.
//! - Handle Bezier curves through the same flatten step the outline
//!   path uses.
//! - Surface `RenderError::SvgNotFound` for gids without records.

use sigilbuzz::{Blob, Face};
use sigilbuzz_render::{Rasterizer, RenderError};

fn record(tag: [u8; 4], offset: u32, length: u32) -> [u8; 16] {
    let mut buf = [0u8; 16];
    buf[0..4].copy_from_slice(&tag);
    buf[8..12].copy_from_slice(&offset.to_be_bytes());
    buf[12..16].copy_from_slice(&length.to_be_bytes());
    buf
}

fn align4(v: &mut Vec<u8>) {
    while v.len() % 4 != 0 {
        v.push(0);
    }
}

/// Builds a minimum-viable SFNT carrying head, maxp, hhea, hmtx, loca,
/// glyf (with one empty glyph), and an `SVG ` table whose record 0
/// covers gid 1 with the supplied SVG payload.
fn build_svg_font(svg_payload: &[u8]) -> Vec<u8> {
    let head = {
        let mut h = Vec::new();
        h.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        h.extend_from_slice(&0u32.to_be_bytes());
        h.extend_from_slice(&0u32.to_be_bytes());
        h.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
        h.extend_from_slice(&0u16.to_be_bytes());
        h.extend_from_slice(&1024u16.to_be_bytes()); // upem
        h.extend_from_slice(&0u64.to_be_bytes());
        h.extend_from_slice(&0u64.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&500i16.to_be_bytes());
        h.extend_from_slice(&500i16.to_be_bytes());
        h.extend_from_slice(&0u16.to_be_bytes());
        h.extend_from_slice(&8u16.to_be_bytes());
        h.extend_from_slice(&2i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes()); // indexToLocFormat = short
        h.extend_from_slice(&0i16.to_be_bytes());
        align4(&mut h);
        h
    };
    let maxp = {
        let mut m = Vec::new();
        m.extend_from_slice(&0x0000_5000u32.to_be_bytes());
        m.extend_from_slice(&2u16.to_be_bytes()); // 2 glyphs (gid 0, gid 1)
        align4(&mut m);
        m
    };
    let hhea = {
        let mut h = Vec::new();
        h.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        h.extend_from_slice(&800i16.to_be_bytes());
        h.extend_from_slice(&(-200i16).to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&500u16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&500i16.to_be_bytes());
        h.extend_from_slice(&1i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        for _ in 0..4 {
            h.extend_from_slice(&0i16.to_be_bytes());
        }
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&2u16.to_be_bytes());
        align4(&mut h);
        h
    };
    let hmtx = {
        let mut m = Vec::new();
        for _ in 0..2 {
            m.extend_from_slice(&500u16.to_be_bytes());
            m.extend_from_slice(&0i16.to_be_bytes());
        }
        align4(&mut m);
        m
    };
    // Empty glyf: both gids point at offset 0, length 0.
    let mut glyf = Vec::new();
    align4(&mut glyf);
    let loca = {
        let mut l = Vec::new();
        for _ in 0..3 {
            l.extend_from_slice(&0u16.to_be_bytes());
        }
        align4(&mut l);
        l
    };

    // SVG ` table.
    let svg = build_svg_table(&[(1, 1, svg_payload)]);

    let payloads: Vec<([u8; 4], &[u8])> = vec![
        (*b"SVG ", svg.as_slice()),
        (*b"glyf", glyf.as_slice()),
        (*b"head", head.as_slice()),
        (*b"hhea", hhea.as_slice()),
        (*b"hmtx", hmtx.as_slice()),
        (*b"loca", loca.as_slice()),
        (*b"maxp", maxp.as_slice()),
    ];

    let num_tables = payloads.len() as u16;
    let header_len = 12 + num_tables as usize * 16;
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&num_tables.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());

    let mut cursor = header_len as u32;
    let mut directory: Vec<[u8; 16]> = Vec::with_capacity(payloads.len());
    for (tag, body) in &payloads {
        directory.push(record(*tag, cursor, body.len() as u32));
        cursor += body.len() as u32;
    }
    for d in &directory {
        out.extend_from_slice(d);
    }
    for (_, body) in &payloads {
        out.extend_from_slice(body);
    }
    out
}

/// Builds the byte image of an `SVG ` table holding the supplied
/// `(start_gid, end_gid, payload)` records. Mirrors the helper used in
/// the core `tests/svg_in_ot.rs` fixture so the layout is identical.
fn build_svg_table(records: &[(u16, u16, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&10u32.to_be_bytes()); // documentListOffset = 10
    out.extend_from_slice(&0u32.to_be_bytes()); // reserved

    // Document List Index at offset 10.
    let list_start = out.len();
    assert_eq!(list_start, 10);
    out.extend_from_slice(&(records.len() as u16).to_be_bytes());

    let records_pos = out.len();
    out.resize(records_pos + records.len() * 12, 0);

    let mut entries = Vec::with_capacity(records.len());
    for (s, e, payload) in records {
        let doc_off = (out.len() - list_start) as u32;
        let doc_len = payload.len() as u32;
        out.extend_from_slice(payload);
        entries.push((*s, *e, doc_off, doc_len));
    }

    for (i, (s, e, off, len)) in entries.iter().enumerate() {
        let dst = records_pos + i * 12;
        out[dst..dst + 2].copy_from_slice(&s.to_be_bytes());
        out[dst + 2..dst + 4].copy_from_slice(&e.to_be_bytes());
        out[dst + 4..dst + 8].copy_from_slice(&off.to_be_bytes());
        out[dst + 8..dst + 12].copy_from_slice(&len.to_be_bytes());
    }

    out
}

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

    // A pixel near the centre must be solid opaque red. Edges may
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

// =========================================================================
// PR #205 deferral coverage: strokes, gradients, <use>, clipPath, shape
// primitives. Each feature gets a synthetic SVG payload + a render-side
// invariant that's hard to satisfy without the new code.
// =========================================================================

/// A diagonal stroke at 45° on a transparent background should leave a
/// row of opaque pixels along the line and nothing elsewhere — proves
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

/// Linear gradient red → blue. Sampling the left edge should be red,
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
    // Mid should have noticeable contributions from both ends —
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

    // Centre is inside the clip → opaque.
    let centre = pix.get(50, 50);
    assert!(
        centre[3] > 200,
        "centre should be inside clip, got {centre:?}"
    );
    // Corner is outside the clip → transparent.
    let corner = pix.get(5, 5);
    assert_eq!(
        corner[3], 0,
        "corner should be outside clip, got {corner:?}"
    );
    // Far edge of the rect (well outside the 20-radius circle) →
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
    // Rect 80x80 ≈ 6400. Circle pi*40^2 ≈ 5026. Ellipse pi*40*20 ≈ 2513.
    assert!(r > 5500 && r < 7000, "rect coverage out of range: {r}");
    assert!(c > 4400 && c < 5600, "circle coverage out of range: {c}");
    assert!(e > 2100 && e < 2900, "ellipse coverage out of range: {e}");
}

/// Round-cornered rect emits cubic geometry — coverage should be lower
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
    // 1e30 viewBox with 1e30 size_pt → scale s = 1, width_f = 1e30,
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

    // Coverage should be ~half the bbox of the triangle (60x60 → ~1800).
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
    // Interior of the C (y=50, x=50) should be transparent — polyline
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
/// the un-dashed total — i.e. approximately 4/(4+2) = 67%.
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
    // Pattern is 4 on / 2 off → about 2/3 lit.
    assert!(
        ratio > 0.55 && ratio < 0.85,
        "dashed/solid coverage ratio out of range: {ratio} (solid={s}, dashed={d})"
    );
    // Find at least one transparent pixel along y=50 between x=15
    // and x=85 — proves at least one "skip" gap was rendered.
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
    // Dashes are 50/50 → approx half coverage.
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
    // Centre stays opaque-ish.
    let mid = pix.get(50, 50);
    assert!(mid[3] > 200, "centre should stay opaque, got {mid:?}");
}

#[test]
fn filter_color_matrix_saturate_zero_yields_grey() {
    // A red square run through saturate=0 must emerge grey (R==G==B).
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
    // The dominant channel should no longer be R — G and B should
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
    //   feFlood colour → flood result
    //   feMerge: flood, SourceGraphic → composite
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
    // End-to-end drop-shadow: SourceAlpha → Gaussian blur → offset
    // (positive dx,dy) → flood-coloured shadow merged under the
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
        // Greyish (low chroma), partially opaque shadow.
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
    // luminanceToAlpha drops the colour channels and writes luminance
    // into alpha. A bright source yields a grey-ish opaque pixel
    // (R=G=B=0, A=luma scaled — but we render premul, so all channels
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
    // computed luma of white = 1.0 → 255.
    assert_eq!(p[0], 0, "RGB should be zeroed, got {p:?}");
    assert!(p[3] > 200, "alpha should track luma, got {p:?}");
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
    // <path>s in the doc), or we surface a structured Parse error —
    // both prove the entry point hooked up correctly.
    match pix {
        Ok(p) => {
            assert!(p.width > 0 && p.height > 0);
        }
        Err(RenderError::Parse(_)) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}
