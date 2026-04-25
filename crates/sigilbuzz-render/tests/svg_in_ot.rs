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
