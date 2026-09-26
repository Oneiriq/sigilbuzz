//! COLRv0 layered color glyph composition.
//!
//! Builds a synthetic SFNT in memory carrying just enough tables for
//! the rasterizer's COLRv0 path:
//!   - `head`, `maxp`, `hhea`, `hmtx`, `loca`, `glyf`: two real
//!     glyphs (gid 1 and gid 2), each a 100x100 square at different
//!     positions inside the design-units grid.
//!   - `COLR` v0: gid 0 (the base glyph) layers gid 1 (palette entry
//!     0 = red) under gid 2 (palette entry 1 = blue).
//!   - `CPAL` v0: one palette of two BGRA colors.
//!
//! The expectation is:
//!   - The composed pixmap covers the union of both squares.
//!   - Pixels under only the first square come out red.
//!   - Pixels under only the second square come out blue.
//!   - Pixels in the overlap take the second layer (blue), since
//!     COLRv0 layers draw bottom-up.

use sigilbuzz::{Blob, Face};
use sigilbuzz_render::Rasterizer;

fn record(tag: [u8; 4], offset: u32, length: u32) -> [u8; 16] {
    let mut buf = [0u8; 16];
    buf[0..4].copy_from_slice(&tag);
    buf[8..12].copy_from_slice(&offset.to_be_bytes());
    buf[12..16].copy_from_slice(&length.to_be_bytes());
    buf
}

/// Builds one simple-glyf glyph: a square with corners (x0,y0)-(x1,y1).
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
                                       // x deltas: x0, x1-x0, 0, x0-x1
    for d in [x0, x1 - x0, 0, x0 - x1] {
        g.extend_from_slice(&d.to_be_bytes());
    }
    // y deltas: y0, 0, y1-y0, 0
    for d in [y0, 0, y1 - y0, 0] {
        g.extend_from_slice(&d.to_be_bytes());
    }
    g
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

#[allow(clippy::too_many_lines)]
fn build_colrv0_font() -> Vec<u8> {
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
        m.extend_from_slice(&3u16.to_be_bytes()); // 3 glyphs
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
        h.extend_from_slice(&3u16.to_be_bytes());
        align4(&mut h);
        h
    };
    let hmtx = {
        let mut m = Vec::new();
        for _ in 0..3 {
            m.extend_from_slice(&500u16.to_be_bytes());
            m.extend_from_slice(&0i16.to_be_bytes());
        }
        align4(&mut m);
        m
    };

    // Glyphs:
    //   gid 0: empty (.notdef)
    //   gid 1: red square at (0..200, 0..200)
    //   gid 2: blue square at (100..300, 100..300)
    let red = build_square_glyph(0, 0, 200, 200);
    let blue = build_square_glyph(100, 100, 300, 300);
    let mut glyf = Vec::new();
    let off0 = glyf.len(); // gid 0 empty
    let off1 = glyf.len();
    glyf.extend_from_slice(&red);
    align2(&mut glyf);
    let off2 = glyf.len();
    glyf.extend_from_slice(&blue);
    align2(&mut glyf);
    let off3 = glyf.len();
    align4(&mut glyf);
    let loca = {
        let mut l = Vec::new();
        for off in [off0, off1, off2, off3] {
            l.extend_from_slice(&((off / 2) as u16).to_be_bytes());
        }
        align4(&mut l);
        l
    };

    // CPAL v0: 1 palette, 2 entries: red (255,0,0,255) and blue (0,0,255,255).
    let cpal = {
        let mut c = Vec::new();
        c.extend_from_slice(&0u16.to_be_bytes()); // version
        c.extend_from_slice(&2u16.to_be_bytes()); // numPaletteEntries
        c.extend_from_slice(&1u16.to_be_bytes()); // numPalettes
        c.extend_from_slice(&2u16.to_be_bytes()); // numColorRecords
                                                  // Header (12) + colorRecordIndices[1 palette] (2 bytes) = 14.
        c.extend_from_slice(&14u32.to_be_bytes()); // offsetFirstColorRecord
        c.extend_from_slice(&0u16.to_be_bytes()); // colorRecordIndices[0] = 0
                                                  // BGRA red
        c.push(0);
        c.push(0);
        c.push(255);
        c.push(255);
        // BGRA blue
        c.push(255);
        c.push(0);
        c.push(0);
        c.push(255);
        align4(&mut c);
        c
    };

    // COLR v0: 1 base record (gid 0 -> 2 layers starting at index 0),
    // 2 layer records: [gid 1 / palette 0], [gid 2 / palette 1].
    let colr = {
        let mut c = Vec::new();
        c.extend_from_slice(&0u16.to_be_bytes()); // version 0
        c.extend_from_slice(&1u16.to_be_bytes()); // numBaseGlyphRecords
        let base_slot = c.len();
        c.extend_from_slice(&0u32.to_be_bytes()); // baseGlyphRecordsOffset
        let layer_slot = c.len();
        c.extend_from_slice(&0u32.to_be_bytes()); // layerRecordsOffset
        c.extend_from_slice(&2u16.to_be_bytes()); // numLayerRecords
                                                  // Base records: { gid, firstLayerIndex, numLayers } each 6B.
        let base_off = c.len() as u32;
        c[base_slot..base_slot + 4].copy_from_slice(&base_off.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes()); // gid 0
        c.extend_from_slice(&0u16.to_be_bytes()); // firstLayerIndex
        c.extend_from_slice(&2u16.to_be_bytes()); // numLayers
        let layer_off = c.len() as u32;
        c[layer_slot..layer_slot + 4].copy_from_slice(&layer_off.to_be_bytes());
        // Layer 0: gid 1, palette 0 (red, drawn first -> underneath).
        c.extend_from_slice(&1u16.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes());
        // Layer 1: gid 2, palette 1 (blue, drawn second -> on top).
        c.extend_from_slice(&2u16.to_be_bytes());
        c.extend_from_slice(&1u16.to_be_bytes());
        align4(&mut c);
        c
    };

    let payloads: Vec<([u8; 4], &[u8])> = vec![
        (*b"COLR", colr.as_slice()),
        (*b"CPAL", cpal.as_slice()),
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

#[test]
fn colrv0_two_layers_compose_with_palette() {
    let bytes = build_colrv0_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    // Sanity: face sees the COLR / CPAL we shipped.
    let colr = face.colr().unwrap().expect("colr present");
    assert!(colr.v0_layers(0).is_some());
    let cpal = face.cpal().unwrap().expect("cpal present");
    assert_eq!(cpal.color(0, 0).unwrap().r, 255);
    assert_eq!(cpal.color(0, 1).unwrap().b, 255);

    let rast = Rasterizer::new();
    // 1024 upem x 100pt size means 1 design unit ~ 0.0977 px. The
    // glyphs are 200x200 design units, so we get ~19.5 px squares.
    let pix = rast
        .rasterize_colrv0_glyph(&face, 0, 0, 100.0, &[])
        .expect("composes");
    assert!(pix.width > 0 && pix.height > 0, "non-empty");

    // Find a pixel that is "only-red" by looking near the lower-left
    // corner (gid 1 only: the red square). Find a "only-blue" pixel
    // near the upper-right (gid 2 only: the blue square).
    // The exact pixel grid depends on the Y-flip and offset, so we
    // scan and tally instead of asserting one indexed pixel.
    let mut red_only = 0;
    let mut blue_only = 0;
    let mut overlap_blue = 0;
    let mut transparent = 0;
    for y in 0..pix.height {
        for x in 0..pix.width {
            let p = pix.get(x, y);
            // Premultiplied: if alpha is 255, we read straight RGB.
            if p[3] == 0 {
                transparent += 1;
                continue;
            }
            let r = p[0];
            let b = p[2];
            if r > 200 && b < 30 {
                red_only += 1;
            } else if b > 200 && r < 30 {
                // Could be either pure-blue pixels in the overlap or
                // outside-overlap blue. Both are blue-only in the
                // composed RGBA.
                if y < pix.height / 2 {
                    blue_only += 1;
                } else {
                    overlap_blue += 1;
                }
            }
        }
    }
    assert!(red_only > 0, "expected red-only pixels (got {red_only})");
    assert!(
        blue_only + overlap_blue > 0,
        "expected blue pixels (got {blue_only} + {overlap_blue})"
    );
    assert!(transparent > 0, "outside the union should be transparent");
}

#[test]
fn colrv0_no_v0_record_for_unknown_glyph_returns_error() {
    let bytes = build_colrv0_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_colrv0_glyph(&face, 99, 0, 24.0, &[])
        .unwrap_err();
    matches!(err, sigilbuzz_render::RenderError::NoColrV0(99));
}

#[test]
fn colrv0_composition_is_deterministic() {
    let bytes = build_colrv0_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let a = rast.rasterize_colrv0_glyph(&face, 0, 0, 64.0, &[]).unwrap();
    let b = rast.rasterize_colrv0_glyph(&face, 0, 0, 64.0, &[]).unwrap();
    assert_eq!(a, b);
}

#[test]
fn colrv0_oob_palette_index_with_real_layers_errors() {
    // Sanity: when a layer has a real palette entry, an out-of-range
    // palette index already errors via the per-layer cpal lookup.
    let bytes = build_colrv0_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_colrv0_glyph(&face, 0, 999, 24.0, &[])
        .unwrap_err();
    assert!(
        matches!(
            err,
            sigilbuzz_render::RenderError::BadPaletteIndex { palette: 999, .. }
        ),
        "got {err:?}"
    );
}

/// Build a COLRv0 font where both layers are flagged as foreground
/// (`palette_index == 0xFFFF`). The pre-fix rasterizer would happily
/// accept any user-supplied palette_index here because the
/// per-layer `cpal.color()` lookup is skipped for foreground layers.
#[allow(clippy::too_many_lines)]
fn build_colrv0_foreground_only_font() -> Vec<u8> {
    let head = {
        let mut h = Vec::new();
        h.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        h.extend_from_slice(&0u32.to_be_bytes());
        h.extend_from_slice(&0u32.to_be_bytes());
        h.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
        h.extend_from_slice(&0u16.to_be_bytes());
        h.extend_from_slice(&1024u16.to_be_bytes());
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
    };
    let maxp = {
        let mut m = Vec::new();
        m.extend_from_slice(&0x0000_5000u32.to_be_bytes());
        m.extend_from_slice(&3u16.to_be_bytes());
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
        h.extend_from_slice(&3u16.to_be_bytes());
        align4(&mut h);
        h
    };
    let hmtx = {
        let mut m = Vec::new();
        for _ in 0..3 {
            m.extend_from_slice(&500u16.to_be_bytes());
            m.extend_from_slice(&0i16.to_be_bytes());
        }
        align4(&mut m);
        m
    };
    let red = build_square_glyph(0, 0, 200, 200);
    let blue = build_square_glyph(100, 100, 300, 300);
    let mut glyf = Vec::new();
    let off0 = glyf.len();
    let off1 = glyf.len();
    glyf.extend_from_slice(&red);
    align2(&mut glyf);
    let off2 = glyf.len();
    glyf.extend_from_slice(&blue);
    align2(&mut glyf);
    let off3 = glyf.len();
    align4(&mut glyf);
    let loca = {
        let mut l = Vec::new();
        for off in [off0, off1, off2, off3] {
            l.extend_from_slice(&((off / 2) as u16).to_be_bytes());
        }
        align4(&mut l);
        l
    };
    // Single-palette CPAL. Important: num_palettes = 1.
    let cpal = {
        let mut c = Vec::new();
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&1u16.to_be_bytes());
        c.extend_from_slice(&1u16.to_be_bytes());
        c.extend_from_slice(&1u16.to_be_bytes());
        c.extend_from_slice(&14u32.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes());
        c.push(0);
        c.push(0);
        c.push(0);
        c.push(255);
        align4(&mut c);
        c
    };
    // COLR v0: both layers carry palette_index = 0xFFFF (foreground).
    let colr = {
        let mut c = Vec::new();
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&1u16.to_be_bytes());
        let base_slot = c.len();
        c.extend_from_slice(&0u32.to_be_bytes());
        let layer_slot = c.len();
        c.extend_from_slice(&0u32.to_be_bytes());
        c.extend_from_slice(&2u16.to_be_bytes());
        let base_off = c.len() as u32;
        c[base_slot..base_slot + 4].copy_from_slice(&base_off.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&2u16.to_be_bytes());
        let layer_off = c.len() as u32;
        c[layer_slot..layer_slot + 4].copy_from_slice(&layer_off.to_be_bytes());
        c.extend_from_slice(&1u16.to_be_bytes());
        c.extend_from_slice(&0xFFFFu16.to_be_bytes());
        c.extend_from_slice(&2u16.to_be_bytes());
        c.extend_from_slice(&0xFFFFu16.to_be_bytes());
        align4(&mut c);
        c
    };
    let payloads: Vec<([u8; 4], &[u8])> = vec![
        (*b"COLR", colr.as_slice()),
        (*b"CPAL", cpal.as_slice()),
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

#[test]
fn colrv0_oob_palette_index_with_foreground_only_layers_errors() {
    // Regression for issue #203: pre-fix rasterizer accepted any
    // palette_index when every layer was a foreground sentinel
    // (0xFFFF) because the per-layer cpal lookup that would have
    // detected the bad index was skipped.
    let bytes = build_colrv0_foreground_only_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    // Valid palette index 0 must succeed.
    let ok = rast.rasterize_colrv0_glyph(&face, 0, 0, 32.0, &[]);
    assert!(ok.is_ok(), "valid palette: {:?}", ok.err());
    // Out-of-range palette index 5 (font has 1 palette) must error.
    let err = rast
        .rasterize_colrv0_glyph(&face, 0, 5, 32.0, &[])
        .expect_err("oob palette must error");
    assert!(
        matches!(
            err,
            sigilbuzz_render::RenderError::BadPaletteIndex { palette: 5, .. }
        ),
        "got {err:?}"
    );
}
