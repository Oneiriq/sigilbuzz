//! COLRv1 paint-tree rasterization round-trip.
//!
//! Builds synthetic in-memory SFNTs with:
//!   - `head` / `maxp` / `hhea` / `hmtx` / `loca` / `glyf`: a real
//!     square outline so the rasterizer has something to scanline.
//!   - `COLR` v1: a `BaseGlyphPaintRecord` for gid 1 pointing at
//!     either a `PaintGlyph` clipping a `PaintSolid`, a
//!     `PaintLinearGradient`, or a `PaintVarSolid` whose alpha varies
//!     with the active coords.
//!   - `CPAL` v0: small palette feeding the leaves.
//!
//! The driver is [`Rasterizer::rasterize_colrv1_glyph`]; the asserts
//! cover the union-bbox sizing, the paint-mask interaction, and (for
//! the var fixture) the deltas wiring up correctly.

use sigilbuzz::{Blob, Face};
use sigilbuzz_render::{Rasterizer, RenderError};

// =========================================================================
// SFNT directory + table-builders shared by every fixture
// =========================================================================

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

fn align2(v: &mut Vec<u8>) {
    while v.len() % 2 != 0 {
        v.push(0);
    }
}

fn f2dot14(v: f32) -> [u8; 2] {
    let raw = (v * 16384.0).round() as i16;
    raw.to_be_bytes()
}

fn build_square_glyph(x0: i16, y0: i16, x1: i16, y1: i16) -> Vec<u8> {
    let mut g = Vec::new();
    g.extend_from_slice(&1i16.to_be_bytes()); // numberOfContours
    g.extend_from_slice(&x0.to_be_bytes());
    g.extend_from_slice(&y0.to_be_bytes());
    g.extend_from_slice(&x1.to_be_bytes());
    g.extend_from_slice(&y1.to_be_bytes());
    g.extend_from_slice(&3u16.to_be_bytes()); // endPts[0] = 3
    g.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
    g.extend_from_slice(&[0x01u8; 4]); // ON_CURVE flags, long-form deltas
    for d in [x0, x1 - x0, 0, x0 - x1] {
        g.extend_from_slice(&d.to_be_bytes());
    }
    for d in [y0, 0, y1 - y0, 0] {
        g.extend_from_slice(&d.to_be_bytes());
    }
    g
}

fn build_head() -> Vec<u8> {
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
    h.extend_from_slice(&0i16.to_be_bytes());
    h.extend_from_slice(&0i16.to_be_bytes());
    align4(&mut h);
    h
}

fn build_maxp(num_glyphs: u16) -> Vec<u8> {
    let mut m = Vec::new();
    m.extend_from_slice(&0x0000_5000u32.to_be_bytes());
    m.extend_from_slice(&num_glyphs.to_be_bytes());
    align4(&mut m);
    m
}

fn build_hhea(num_glyphs: u16) -> Vec<u8> {
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
    h.extend_from_slice(&num_glyphs.to_be_bytes());
    align4(&mut h);
    h
}

fn build_hmtx(num_glyphs: u16) -> Vec<u8> {
    let mut m = Vec::new();
    for _ in 0..num_glyphs {
        m.extend_from_slice(&500u16.to_be_bytes());
        m.extend_from_slice(&0i16.to_be_bytes());
    }
    align4(&mut m);
    m
}

fn build_cpal_v0(colors: &[(u8, u8, u8, u8)]) -> Vec<u8> {
    build_cpal_palettes(&[colors])
}

/// Builds a v0 CPAL with one palette per slice. Every palette must
/// have the same number of entries.
fn build_cpal_palettes(palettes: &[&[(u8, u8, u8, u8)]]) -> Vec<u8> {
    let num_palettes = palettes.len() as u16;
    let entries = palettes[0].len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&entries.to_be_bytes());
    out.extend_from_slice(&num_palettes.to_be_bytes());
    out.extend_from_slice(&(entries * num_palettes).to_be_bytes());
    let header_plus_indices = 12 + num_palettes as usize * 2;
    out.extend_from_slice(&(header_plus_indices as u32).to_be_bytes());
    for i in 0..num_palettes {
        out.extend_from_slice(&(i * entries).to_be_bytes()); // colorRecordIndices
    }
    for colors in palettes {
        assert_eq!(colors.len(), usize::from(entries));
        for (r, g, b, a) in *colors {
            out.push(*b);
            out.push(*g);
            out.push(*r);
            out.push(*a);
        }
    }
    align4(&mut out);
    out
}

/// Builds an empty v0 + v1 COLR header that points at a single
/// `BaseGlyphPaintRecord { glyph_id, paintOffset = 10 }`. Append the
/// paint body bytes after this header to land at offset 10 from the
/// BaseGlyphList start (same convention as the paint-crate evaluator
/// fixtures).
fn build_v1_header(glyph_id: u16) -> Vec<u8> {
    let header_len: u32 = 34;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // version
    out.extend_from_slice(&0u16.to_be_bytes()); // numBaseGlyphRecords (v0)
    out.extend_from_slice(&header_len.to_be_bytes()); // baseGlyphRecordsOffset
    out.extend_from_slice(&header_len.to_be_bytes()); // layerRecordsOffset
    out.extend_from_slice(&0u16.to_be_bytes()); // numLayerRecords
    out.extend_from_slice(&header_len.to_be_bytes()); // baseGlyphListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // layerListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // clipListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // varIndexMapOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // itemVariationStoreOffset

    // BaseGlyphList: numRecords, then (gid, paintOffset).
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&glyph_id.to_be_bytes());
    out.extend_from_slice(&10u32.to_be_bytes());
    out
}

/// Emits an SFNT containing the given (already aligned-to-2) tables.
fn emit_sfnt(tables: &[([u8; 4], &[u8])]) -> Vec<u8> {
    let num_tables = tables.len() as u16;
    let header_len = 12 + num_tables as usize * 16;
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&num_tables.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    let mut cursor = header_len as u32;
    let mut directory: Vec<[u8; 16]> = Vec::with_capacity(tables.len());
    for (tag, body) in tables {
        directory.push(record(*tag, cursor, body.len() as u32));
        cursor += body.len() as u32;
    }
    for d in &directory {
        out.extend_from_slice(d);
    }
    for (_, body) in tables {
        out.extend_from_slice(body);
    }
    out
}

/// Standard glyf+loca pair: gid 0 empty, gid 1 = a 200-unit square at
/// (0..200, 0..200) in design units. With UPEM=1024 and size=100pt
/// the rasterizer yields ~19.5 px squares.
fn standard_glyf_loca() -> (Vec<u8>, Vec<u8>) {
    let sq = build_square_glyph(0, 0, 200, 200);
    let mut glyf = Vec::new();
    let off0 = glyf.len();
    let off1 = glyf.len();
    glyf.extend_from_slice(&sq);
    align2(&mut glyf);
    let off2 = glyf.len();
    align4(&mut glyf);
    let mut loca = Vec::new();
    for off in [off0, off1, off2] {
        loca.extend_from_slice(&((off / 2) as u16).to_be_bytes());
    }
    align4(&mut loca);
    (glyf, loca)
}

// =========================================================================
// Test 1: PaintGlyph wrapping a PaintSolid. Verifies the basic
// outline-clipped color fill goes round-trip.
// =========================================================================

fn build_glyph_solid_font() -> Vec<u8> {
    // CPAL: one entry, opaque red.
    build_glyph_solid_font_with_cpal(&build_cpal_v0(&[(255, 0, 0, 255)]))
}

/// The solid-fill font of [`build_glyph_solid_font`] with its CPAL
/// replaced by `cpal`. The PaintSolid uses palette entry 0.
fn build_glyph_solid_font_with_cpal(cpal: &[u8]) -> Vec<u8> {
    let head = build_head();
    let maxp = build_maxp(2);
    let hhea = build_hhea(2);
    let hmtx = build_hmtx(2);
    let (glyf, loca) = standard_glyf_loca();

    // COLR: BaseGlyphPaintRecord for gid 1 -> PaintGlyph(child=Solid,
    // outline=gid 1). The PaintGlyph's child paint is right after.
    let mut colr = build_v1_header(1);
    let pglyph_start = colr.len();
    colr.push(10); // PaintGlyph
    colr.extend_from_slice(&[0, 0, 0]); // Offset24 placeholder for child
    colr.extend_from_slice(&1u16.to_be_bytes()); // outline gid 1
    let solid_start = colr.len();
    let rel = (solid_start - pglyph_start) as u32;
    colr[pglyph_start + 1] = ((rel >> 16) & 0xff) as u8;
    colr[pglyph_start + 2] = ((rel >> 8) & 0xff) as u8;
    colr[pglyph_start + 3] = (rel & 0xff) as u8;
    colr.push(2); // PaintSolid
    colr.extend_from_slice(&0u16.to_be_bytes()); // palette index 0
    colr.extend_from_slice(&f2dot14(1.0)); // alpha = 1.0
    align4(&mut colr);

    let tables: &[([u8; 4], &[u8])] = &[
        (*b"COLR", colr.as_slice()),
        (*b"CPAL", cpal),
        (*b"glyf", glyf.as_slice()),
        (*b"head", head.as_slice()),
        (*b"hhea", hhea.as_slice()),
        (*b"hmtx", hmtx.as_slice()),
        (*b"loca", loca.as_slice()),
        (*b"maxp", maxp.as_slice()),
    ];
    emit_sfnt(tables)
}

/// CPAL v0 with two one-entry palettes.
fn build_cpal_two_palettes(p0: (u8, u8, u8, u8), p1: (u8, u8, u8, u8)) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&1u16.to_be_bytes()); // numPaletteEntries
    out.extend_from_slice(&2u16.to_be_bytes()); // numPalettes
    out.extend_from_slice(&2u16.to_be_bytes()); // numColorRecords
    out.extend_from_slice(&16u32.to_be_bytes()); // colorRecordsArrayOffset
    out.extend_from_slice(&0u16.to_be_bytes()); // palette 0 -> record 0
    out.extend_from_slice(&1u16.to_be_bytes()); // palette 1 -> record 1
    for (r, g, b, a) in [p0, p1] {
        out.extend_from_slice(&[b, g, r, a]);
    }
    align4(&mut out);
    out
}

/// Counts pixels that are mostly `channel` (0 = red, 2 = blue).
fn count_dominant(pix: &sigilbuzz_render::ColorPixmap, channel: usize) -> usize {
    let mut n = 0;
    for y in 0..pix.height {
        for x in 0..pix.width {
            let p = pix.get(x, y);
            let others = (0..3).filter(|&c| c != channel).all(|c| p[c] < 30);
            if p[3] > 0 && p[channel] > 200 && others {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn colrv1_palette_index_selects_cpal_palette() {
    let cpal = build_cpal_two_palettes((255, 0, 0, 255), (0, 0, 255, 255));
    let bytes = build_glyph_solid_font_with_cpal(&cpal);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();

    let light = rast
        .rasterize_colrv1_glyph(&face, 1, 0, 100.0, &[])
        .expect("palette 0 renders");
    assert!(count_dominant(&light, 0) > 0, "palette 0 paints red");
    assert_eq!(count_dominant(&light, 2), 0, "palette 0 has no blue");

    let dark = rast
        .rasterize_colrv1_glyph(&face, 1, 1, 100.0, &[])
        .expect("palette 1 renders");
    assert!(count_dominant(&dark, 2) > 0, "palette 1 paints blue");
    assert_eq!(count_dominant(&dark, 0), 0, "palette 1 has no red");

    // A palette the font does not have paints every palette entry in
    // the foreground color, opaque black by default, as HarfBuzz does.
    let fallback = rast
        .rasterize_colrv1_glyph(&face, 1, 9, 100.0, &[])
        .expect("out-of-range palette still renders");
    assert_eq!(
        (fallback.width, fallback.height),
        (light.width, light.height)
    );
    assert_eq!(count_dominant(&fallback, 0), 0, "no palette red");
    let opaque: Vec<[u8; 4]> = fallback
        .data
        .chunks_exact(4)
        .filter(|p| p[3] == 255)
        .map(|p| [p[0], p[1], p[2], p[3]])
        .collect();
    assert!(!opaque.is_empty(), "the glyph still paints");
    assert!(opaque.iter().all(|p| *p == [0, 0, 0, 255]), "all black");
}

#[test]
fn colrv1_paint_glyph_solid_fills_inside_outline() {
    let bytes = build_glyph_solid_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast
        .rasterize_colrv1_glyph(&face, 1, 0, 100.0, &[])
        .expect("rasterize succeeds");
    assert!(pix.width > 0 && pix.height > 0, "non-empty");

    // Most opaque red pixel should sit somewhere in the middle.
    let mut red_pixels = 0;
    let mut transparent_pixels = 0;
    for y in 0..pix.height {
        for x in 0..pix.width {
            let p = pix.get(x, y);
            if p[3] == 0 {
                transparent_pixels += 1;
            } else if p[0] > 200 && p[1] < 30 && p[2] < 30 {
                red_pixels += 1;
            }
        }
    }
    assert!(red_pixels > 0, "expected red pixels, got {red_pixels}");
    assert!(
        transparent_pixels > 0,
        "expected the 1-px margin to be transparent"
    );
}

#[test]
fn colrv1_resolves_colors_in_the_requested_palette() {
    // Palette 0 is red and palette 1 is blue.
    let cpal = build_cpal_palettes(&[&[(255, 0, 0, 255)], &[(0, 0, 255, 255)]]);
    let bytes = build_glyph_solid_font_with_cpal(&cpal);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let count = |palette: u16, want: fn(&[u8; 4]) -> bool| {
        let pix = rast
            .rasterize_colrv1_glyph(&face, 1, palette, 100.0, &[])
            .expect("rasterize succeeds");
        let mut hits = 0;
        for y in 0..pix.height {
            for x in 0..pix.width {
                hits += usize::from(want(&pix.get(x, y)));
            }
        }
        hits
    };
    let red = |p: &[u8; 4]| p[0] > 200 && p[2] < 30;
    let blue = |p: &[u8; 4]| p[2] > 200 && p[0] < 30;
    assert!(count(0, red) > 0 && count(0, blue) == 0, "palette 0 is red");
    assert!(
        count(1, blue) > 0 && count(1, red) == 0,
        "palette 1 is blue"
    );

    // A palette the font lacks paints in the foreground color, opaque
    // black by default, as in HarfBuzz.
    let black = |p: &[u8; 4]| p[3] == 255 && p[0] < 30 && p[1] < 30 && p[2] < 30;
    assert!(
        count(2, black) > 0 && count(2, red) == 0 && count(2, blue) == 0,
        "palette 2 paints the foreground"
    );
}

#[test]
fn colrv1_missing_record_returns_dedicated_error() {
    let bytes = build_glyph_solid_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_colrv1_glyph(&face, 99, 0, 24.0, &[])
        .unwrap_err();
    assert!(
        matches!(err, RenderError::ColrV1NotFound(99)),
        "got {err:?}"
    );
}

#[test]
fn colrv1_rasterization_is_deterministic() {
    let bytes = build_glyph_solid_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let a = rast.rasterize_colrv1_glyph(&face, 1, 0, 64.0, &[]).unwrap();
    let b = rast.rasterize_colrv1_glyph(&face, 1, 0, 64.0, &[]).unwrap();
    assert_eq!(a, b);
}

// =========================================================================
// Test 2: PaintGlyph wrapping a PaintLinearGradient. The gradient
// pixels should vary across the glyph, masked by the outline.
// =========================================================================

fn build_glyph_linear_gradient_font() -> Vec<u8> {
    let head = build_head();
    let maxp = build_maxp(2);
    let hhea = build_hhea(2);
    let hmtx = build_hmtx(2);
    let (glyf, loca) = standard_glyf_loca();

    // Two-stop palette: red at 0, blue at 1.
    let cpal = build_cpal_v0(&[(255, 0, 0, 255), (0, 0, 255, 255)]);

    let mut colr = build_v1_header(1);
    // PaintGlyph wrapping a PaintLinearGradient.
    let pglyph_start = colr.len();
    colr.push(10); // PaintGlyph
    colr.extend_from_slice(&[0, 0, 0]); // child offset24 placeholder
    colr.extend_from_slice(&1u16.to_be_bytes()); // outline gid 1
    let lin_start = colr.len();
    let rel = (lin_start - pglyph_start) as u32;
    colr[pglyph_start + 1] = ((rel >> 16) & 0xff) as u8;
    colr[pglyph_start + 2] = ((rel >> 8) & 0xff) as u8;
    colr[pglyph_start + 3] = (rel & 0xff) as u8;
    // PaintLinearGradient (format 4): { u8 fmt; Offset24 colorLine;
    //   FWord x0,y0,x1,y1,x2,y2 }
    colr.push(4);
    colr.extend_from_slice(&[0, 0, 0]); // colorLine placeholder
                                        // Gradient axis: from x=0 (left of square) to x=200 (right of
                                        // square). Anchor (0, 200) is outside the line per the spec's
                                        // unused third point convention.
    colr.extend_from_slice(&0i16.to_be_bytes()); // x0
    colr.extend_from_slice(&100i16.to_be_bytes()); // y0
    colr.extend_from_slice(&200i16.to_be_bytes()); // x1
    colr.extend_from_slice(&100i16.to_be_bytes()); // y1
    colr.extend_from_slice(&0i16.to_be_bytes()); // x2
    colr.extend_from_slice(&200i16.to_be_bytes()); // y2
    let cl_start = colr.len();
    let cl_rel = (cl_start - lin_start) as u32;
    colr[lin_start + 1] = ((cl_rel >> 16) & 0xff) as u8;
    colr[lin_start + 2] = ((cl_rel >> 8) & 0xff) as u8;
    colr[lin_start + 3] = (cl_rel & 0xff) as u8;
    // ColorLine: { u8 extend; u16 numStops; ColorStop[] }
    colr.push(0); // extend = Pad
    colr.extend_from_slice(&2u16.to_be_bytes()); // numStops
                                                 // Stop 0: offset 0.0, palette 0 (red), alpha 1.0.
    colr.extend_from_slice(&f2dot14(0.0));
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));
    // Stop 1: offset 1.0, palette 1 (blue), alpha 1.0.
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));
    align4(&mut colr);

    let tables: &[([u8; 4], &[u8])] = &[
        (*b"COLR", colr.as_slice()),
        (*b"CPAL", cpal.as_slice()),
        (*b"glyf", glyf.as_slice()),
        (*b"head", head.as_slice()),
        (*b"hhea", hhea.as_slice()),
        (*b"hmtx", hmtx.as_slice()),
        (*b"loca", loca.as_slice()),
        (*b"maxp", maxp.as_slice()),
    ];
    emit_sfnt(tables)
}

#[test]
fn colrv1_paint_glyph_linear_gradient_varies_across_outline() {
    let bytes = build_glyph_linear_gradient_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast
        .rasterize_colrv1_glyph(&face, 1, 0, 100.0, &[])
        .expect("rasterize succeeds");
    assert!(pix.width > 1 && pix.height > 1);

    // Walk a horizontal strip across the middle of the glyph and tally
    // the dominant channel. Going left to right, red should dominate
    // first then blue.
    let mid_y = pix.height / 2;
    let mut red_left = false;
    let mut blue_right = false;
    for x in 0..pix.width {
        let p = pix.get(x, mid_y);
        if p[3] == 0 {
            continue;
        }
        // Pre-multiplied: the raw channel values are already weighted
        // by alpha so direct compare works for fully-opaque pixels.
        if x < pix.width / 3 && p[0] > p[2] {
            red_left = true;
        }
        if x > 2 * pix.width / 3 && p[2] > p[0] {
            blue_right = true;
        }
    }
    assert!(red_left, "expected red on the left");
    assert!(blue_right, "expected blue on the right");
}

// =========================================================================
// Test 2b: PaintGlyph wrapping a PaintSweepGradient centered on the
// square, sweeping counter-clockwise from 0 to pi (stored, with the
// half-turn bias, as -1.0 and 0.0). Red at 0, blue at 1, pad.
// =========================================================================

fn build_glyph_sweep_gradient_font() -> Vec<u8> {
    let head = build_head();
    let maxp = build_maxp(2);
    let hhea = build_hhea(2);
    let hmtx = build_hmtx(2);
    let (glyf, loca) = standard_glyf_loca();
    let cpal = build_cpal_v0(&[(255, 0, 0, 255), (0, 0, 255, 255)]);

    let mut colr = build_v1_header(1);
    // PaintGlyph(1), child right after its 6 bytes.
    colr.extend_from_slice(&[10, 0, 0, 6]);
    colr.extend_from_slice(&1u16.to_be_bytes());
    // PaintSweepGradient: color line right after its 12 bytes.
    colr.extend_from_slice(&[8, 0, 0, 12]);
    colr.extend_from_slice(&100i16.to_be_bytes()); // centerX
    colr.extend_from_slice(&100i16.to_be_bytes()); // centerY
    colr.extend_from_slice(&f2dot14(-1.0)); // startAngle: 0 rad
    colr.extend_from_slice(&f2dot14(0.0)); // endAngle: pi rad
    colr.push(0); // extend = Pad
    colr.extend_from_slice(&2u16.to_be_bytes());
    for (offset, entry) in [(0.0, 0u16), (1.0, 1)] {
        colr.extend_from_slice(&f2dot14(offset));
        colr.extend_from_slice(&entry.to_be_bytes());
        colr.extend_from_slice(&f2dot14(1.0));
    }
    align4(&mut colr);

    let tables: &[([u8; 4], &[u8])] = &[
        (*b"COLR", colr.as_slice()),
        (*b"CPAL", cpal.as_slice()),
        (*b"glyf", glyf.as_slice()),
        (*b"head", head.as_slice()),
        (*b"hhea", hhea.as_slice()),
        (*b"hmtx", hmtx.as_slice()),
        (*b"loca", loca.as_slice()),
        (*b"maxp", maxp.as_slice()),
    ];
    emit_sfnt(tables)
}

#[test]
fn colrv1_sweep_gradient_runs_counter_clockwise_in_design_space() {
    let bytes = build_glyph_sweep_gradient_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let pix = Rasterizer::new()
        .rasterize_colrv1_glyph(&face, 1, 0, 100.0, &[])
        .expect("rasterize succeeds");
    assert!(pix.width > 4 && pix.height > 4);
    // Pixel rows run downward, so the top half is design y above the
    // center: angles in (0, pi), early in the sweep, mostly red. The
    // bottom half is past the end angle and pads to blue.
    let x = pix.width - 2;
    let upper = pix.get(x, pix.height / 4);
    let lower = pix.get(x, 3 * pix.height / 4);
    assert!(upper[0] > upper[2], "upper right should be red: {upper:?}");
    assert!(lower[2] > lower[0], "lower right should be blue: {lower:?}");
}

// =========================================================================
// Test 3: PaintVarSolid alpha varies with normalized coords. Builds a
// font with a 1-axis fvar + a tiny ItemVariationStore that maps a
// single F2DOT14 alpha delta of -0.5 onto the solid's `var_index_base`
// (= 0).
// =========================================================================

fn build_var_solid_font() -> Vec<u8> {
    let head = build_head();
    let maxp = build_maxp(2);
    let hhea = build_hhea(2);
    let hmtx = build_hmtx(2);
    let (glyf, loca) = standard_glyf_loca();
    let cpal = build_cpal_v0(&[(255, 255, 255, 255)]);

    // fvar: 1 axis, default value 0.0, range [0, 1].
    let fvar = {
        let mut f = Vec::new();
        // Header.
        f.extend_from_slice(&1u16.to_be_bytes()); // major
        f.extend_from_slice(&0u16.to_be_bytes()); // minor
        f.extend_from_slice(&16u16.to_be_bytes()); // axesArrayOffset
        f.extend_from_slice(&2u16.to_be_bytes()); // reserved (countSizePairs)
        f.extend_from_slice(&1u16.to_be_bytes()); // axisCount
        f.extend_from_slice(&20u16.to_be_bytes()); // axisSize
        f.extend_from_slice(&0u16.to_be_bytes()); // instanceCount
        f.extend_from_slice(&0u16.to_be_bytes()); // instanceSize
                                                  // VariationAxisRecord (20 bytes).
        f.extend_from_slice(b"WGHT");
        // Fixed (16.16) min, default, max.
        f.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        f.extend_from_slice(&0u16.to_be_bytes()); // flags
        f.extend_from_slice(&0u16.to_be_bytes()); // axisNameID
        align4(&mut f);
        f
    };

    // COLR with an inline ItemVariationStore. The PaintVarSolid uses
    // var_index_base = 0; field_index 0 lookup -> outer=0, inner=0.
    //
    // ItemVariationStore layout (we hand-craft the minimum spec
    // permits): one axis, one region with (start=0, peak=1, end=1)
    // -> at coord 1.0 the region scalar is 1.0. One subtable with one
    // delta row of [-8192] (= -0.5 in F2DOT14) and one short-delta
    // entry. The PaintVarSolid then gets alpha = 1.0 + (-0.5) = 0.5
    // when coords = [1.0].
    let ivs = build_ivs_one_axis_one_short_delta(-8192_i16);

    // Compute COLR layout: header (34 bytes), then the paint body,
    // then the IVS. The IVS lives inside the COLR table data, reached
    // through `itemVariationStoreOffset`, which points past the paint
    // body.
    //
    // Plan: write the v1 header with a placeholder store offset, then
    // append PaintVarSolid (8 bytes), then align, then the IVS, then
    // patch the store offset in the header.
    let mut colr = build_v1_header(1);
    let var_store_off_slot = 30; // itemVariationStoreOffset is the last u32 in the header
                                 // PaintGlyph(child=PaintVarSolid, outline=gid 1).
    let pglyph_start = colr.len();
    colr.push(10); // PaintGlyph
    colr.extend_from_slice(&[0, 0, 0]); // Offset24 placeholder for child
    colr.extend_from_slice(&1u16.to_be_bytes()); // outline gid 1
    let varsolid_start = colr.len();
    let rel = (varsolid_start - pglyph_start) as u32;
    colr[pglyph_start + 1] = ((rel >> 16) & 0xff) as u8;
    colr[pglyph_start + 2] = ((rel >> 8) & 0xff) as u8;
    colr[pglyph_start + 3] = (rel & 0xff) as u8;
    // PaintVarSolid (format 3): { u8 fmt; u16 paletteIndex; F2Dot14
    // alpha; VarIndexBase varIndexBase }
    colr.push(3);
    colr.extend_from_slice(&0u16.to_be_bytes()); // paletteIndex 0
    colr.extend_from_slice(&f2dot14(1.0)); // alpha base = 1.0
    colr.extend_from_slice(&0u32.to_be_bytes()); // varIndexBase = 0
    align4(&mut colr);
    let ivs_off = colr.len() as u32;
    colr.extend_from_slice(&ivs);
    align4(&mut colr);
    colr[var_store_off_slot..var_store_off_slot + 4].copy_from_slice(&ivs_off.to_be_bytes());

    let tables: &[([u8; 4], &[u8])] = &[
        (*b"COLR", colr.as_slice()),
        (*b"CPAL", cpal.as_slice()),
        (*b"fvar", fvar.as_slice()),
        (*b"glyf", glyf.as_slice()),
        (*b"head", head.as_slice()),
        (*b"hhea", hhea.as_slice()),
        (*b"hmtx", hmtx.as_slice()),
        (*b"loca", loca.as_slice()),
        (*b"maxp", maxp.as_slice()),
    ];
    emit_sfnt(tables)
}

/// Builds a tiny `ItemVariationStore` covering one axis, one region
/// with a peak at 1.0, and one subtable carrying one row of one short
/// (i16) delta. The delta value is `delta`.
fn build_ivs_one_axis_one_short_delta(delta: i16) -> Vec<u8> {
    let mut ivs = Vec::new();

    // Header: format = 1; offset to VariationRegionList; count of
    // ItemVariationData subtables; offsets to each subtable.
    ivs.extend_from_slice(&1u16.to_be_bytes()); // format
    let region_off_slot = ivs.len();
    ivs.extend_from_slice(&0u32.to_be_bytes()); // varRegionListOffset
    ivs.extend_from_slice(&1u16.to_be_bytes()); // varDataCount
    let var_data_off_slot = ivs.len();
    ivs.extend_from_slice(&0u32.to_be_bytes()); // var data offsets[0]

    // VariationRegionList: { u16 axisCount; u16 regionCount;
    //   VariationRegion[regionCount * axisCount] each = 6 bytes
    //   (startCoord F2Dot14, peakCoord F2Dot14, endCoord F2Dot14). }
    let region_list_off = ivs.len() as u32;
    ivs.extend_from_slice(&1u16.to_be_bytes()); // axisCount
    ivs.extend_from_slice(&1u16.to_be_bytes()); // regionCount
                                                // Region 0, axis 0: peak at 1.0 -> scalar=1 at coord=1.0.
    ivs.extend_from_slice(&f2dot14(0.0));
    ivs.extend_from_slice(&f2dot14(1.0));
    ivs.extend_from_slice(&f2dot14(1.0));
    align4(&mut ivs);
    ivs[region_off_slot..region_off_slot + 4].copy_from_slice(&region_list_off.to_be_bytes());

    // ItemVariationData subtable:
    //   u16 itemCount
    //   u16 wordDeltaCount   (high bit = 1 if all-LONG layout; here 0)
    //   u16 regionIndexCount
    //   u16 regionIndexes[regionIndexCount]
    //   delta rows
    let var_data_off = ivs.len() as u32;
    ivs.extend_from_slice(&1u16.to_be_bytes()); // itemCount
    ivs.extend_from_slice(&0u16.to_be_bytes()); // wordDeltaCount = 0 (all short)
    ivs.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
    ivs.extend_from_slice(&0u16.to_be_bytes()); // regionIndexes[0] = 0
                                                // The single delta needs to land in `i16` range (it's -8192).
                                                // `wordDeltaCount` controls the per-row layout: bits 0..14 are
                                                // the count of leading word-deltas in the row, bit 15 ("LONG_WORDS")
                                                // upgrades each word from i16 to i32. We keep bit 15 = 0 and
                                                // set wordDeltaCount = 1 so the single delta is read as one i16.
    let wdc_off = (var_data_off as usize) + 2;
    ivs[wdc_off..wdc_off + 2].copy_from_slice(&1u16.to_be_bytes());
    // Emit the row: one i16 word delta. (`itemCount` = 1 row x one
    // delta column = 2 bytes total.)
    ivs.extend_from_slice(&delta.to_be_bytes());
    align4(&mut ivs);
    ivs[var_data_off_slot..var_data_off_slot + 4].copy_from_slice(&var_data_off.to_be_bytes());

    ivs
}

#[test]
fn colrv1_paint_var_solid_alpha_responds_to_coords() {
    let bytes = build_var_solid_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();

    // At default coords [0.0]: alpha = 1.0 -> opaque white pixels.
    let pix_default = rast
        .rasterize_colrv1_glyph(&face, 1, 0, 100.0, &[])
        .expect("default rasterizes");
    // At coord [1.0]: alpha = 1.0 - 0.5 = 0.5 -> translucent whites.
    let pix_peak = rast
        .rasterize_colrv1_glyph(&face, 1, 0, 100.0, &[1.0])
        .expect("peak rasterizes");
    assert_eq!(
        (pix_default.width, pix_default.height),
        (pix_peak.width, pix_peak.height)
    );

    // Find the brightest fully-opaque pixel in the default render and
    // compare its alpha to the same coord in the peak render. Peak
    // alpha should be roughly half.
    let mut found_default = 0u8;
    let mut found_peak = 0u8;
    for y in 0..pix_default.height {
        for x in 0..pix_default.width {
            let pd = pix_default.get(x, y);
            if pd[3] > found_default {
                found_default = pd[3];
                let pp = pix_peak.get(x, y);
                found_peak = pp[3];
            }
        }
    }
    assert!(found_default > 200, "default alpha was {found_default}");
    assert!(
        found_peak < found_default,
        "peak alpha ({found_peak}) should be less than default ({found_default})"
    );
    assert!(
        found_peak > 80 && found_peak < 200,
        "peak should be around half-alpha, got {found_peak}"
    );
}
