//! COLR palette entry `0xFFFF` (the text color) renders in the
//! rasterizer's foreground color, for COLRv0 layers and COLRv1 paints
//! alike: opaque black by default, or whatever
//! [`Rasterizer::with_foreground`] picks.
//!
//! The fixture has one square outline (gid 1) and three color glyphs
//! that fill it with the foreground: gid 2 through a COLRv0 layer,
//! gid 3 through `PaintGlyph` + `PaintSolid`, and gid 4 the same at
//! paint alpha 0.5.

use sigilbuzz::{Blob, Face};
use sigilbuzz_render::{ColorPixmap, Rasterizer};

fn align4(v: &mut Vec<u8>) {
    while v.len() % 4 != 0 {
        v.push(0);
    }
}

fn f2dot14(v: f32) -> [u8; 2] {
    ((v * 16384.0).round() as i16).to_be_bytes()
}

/// A 200-unit square, gid 1 of the glyf table.
fn square() -> Vec<u8> {
    let mut g = Vec::new();
    for v in [1i16, 0, 0, 200, 200] {
        g.extend_from_slice(&v.to_be_bytes());
    }
    g.extend_from_slice(&3u16.to_be_bytes()); // endPts[0]
    g.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
    g.extend_from_slice(&[0x01; 4]); // on-curve, long deltas
    for d in [0i16, 200, 0, -200, 0, 0, 200, 0] {
        g.extend_from_slice(&d.to_be_bytes());
    }
    g
}

fn head() -> Vec<u8> {
    let mut h = Vec::new();
    h.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    h.extend_from_slice(&0u32.to_be_bytes());
    h.extend_from_slice(&0u32.to_be_bytes());
    h.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    h.extend_from_slice(&0u16.to_be_bytes());
    h.extend_from_slice(&1024u16.to_be_bytes()); // upem
    h.extend_from_slice(&[0; 16]); // created, modified
    for v in [0i16, 0, 500, 500] {
        h.extend_from_slice(&v.to_be_bytes());
    }
    for v in [0u16, 8] {
        h.extend_from_slice(&v.to_be_bytes());
    }
    for v in [2i16, 0, 0] {
        h.extend_from_slice(&v.to_be_bytes()); // direction hint, short loca, format
    }
    align4(&mut h);
    h
}

fn maxp() -> Vec<u8> {
    let mut m = Vec::new();
    m.extend_from_slice(&0x0000_5000u32.to_be_bytes());
    m.extend_from_slice(&5u16.to_be_bytes());
    align4(&mut m);
    m
}

fn hhea() -> Vec<u8> {
    let mut h = Vec::new();
    h.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    for v in [800i16, -200, 0] {
        h.extend_from_slice(&v.to_be_bytes());
    }
    h.extend_from_slice(&500u16.to_be_bytes());
    for v in [0i16, 0, 500, 1, 0, 0, 0, 0, 0, 0, 0] {
        h.extend_from_slice(&v.to_be_bytes());
    }
    h.extend_from_slice(&5u16.to_be_bytes());
    align4(&mut h);
    h
}

fn hmtx() -> Vec<u8> {
    let mut m = Vec::new();
    for _ in 0..5 {
        m.extend_from_slice(&500u16.to_be_bytes());
        m.extend_from_slice(&0i16.to_be_bytes());
    }
    m
}

/// glyf + short loca: gid 0 empty, gid 1 the square, gids 2-4 empty.
fn glyf_loca() -> (Vec<u8>, Vec<u8>) {
    let glyf = square();
    let mut loca = Vec::new();
    for off in [0usize, 0, glyf.len(), glyf.len(), glyf.len(), glyf.len()] {
        loca.extend_from_slice(&((off / 2) as u16).to_be_bytes());
    }
    align4(&mut loca);
    (glyf, loca)
}

/// CPAL v0: one palette with one red entry (unused by the paints).
fn cpal() -> Vec<u8> {
    let mut out = Vec::new();
    for v in [0u16, 1, 1, 1] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(&14u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&[0, 0, 255, 255]);
    align4(&mut out);
    out
}

/// PaintGlyph(1) over PaintSolid(0xFFFF, alpha).
fn foreground_fill(alpha: f32) -> Vec<u8> {
    let mut p = vec![10u8, 0, 0, 6];
    p.extend_from_slice(&1u16.to_be_bytes());
    p.push(2);
    p.extend_from_slice(&0xFFFFu16.to_be_bytes());
    p.extend_from_slice(&f2dot14(alpha));
    p
}

/// COLR with the 34-byte v1 header: one v0 base glyph (2) with one
/// foreground layer on gid 1, and v1 paints for gids 3 and 4.
fn colr() -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(&1u16.to_be_bytes()); // version
    c.extend_from_slice(&1u16.to_be_bytes()); // numBaseGlyphRecords
    c.extend_from_slice(&34u32.to_be_bytes()); // baseGlyphRecordsOffset
    c.extend_from_slice(&40u32.to_be_bytes()); // layerRecordsOffset
    c.extend_from_slice(&1u16.to_be_bytes()); // numLayerRecords
    c.extend_from_slice(&44u32.to_be_bytes()); // baseGlyphListOffset
    c.extend_from_slice(&[0; 16]); // layer list, clip list, index map, var store
    for v in [2u16, 0, 1] {
        c.extend_from_slice(&v.to_be_bytes()); // base glyph record
    }
    c.extend_from_slice(&1u16.to_be_bytes()); // layer: gid 1
    c.extend_from_slice(&0xFFFFu16.to_be_bytes()); // foreground
    let paints = [(3u16, foreground_fill(1.0)), (4, foreground_fill(0.5))];
    c.extend_from_slice(&(paints.len() as u32).to_be_bytes());
    let mut offset = 4 + 6 * paints.len();
    for (gid, p) in &paints {
        c.extend_from_slice(&gid.to_be_bytes());
        c.extend_from_slice(&(offset as u32).to_be_bytes());
        offset += p.len();
    }
    for (_, p) in &paints {
        c.extend_from_slice(p);
    }
    align4(&mut c);
    c
}

fn font_bytes() -> Vec<u8> {
    let (glyf, loca) = glyf_loca();
    let tables: [(&[u8; 4], Vec<u8>); 8] = [
        (b"COLR", colr()),
        (b"CPAL", cpal()),
        (b"glyf", glyf),
        (b"head", head()),
        (b"hhea", hhea()),
        (b"hmtx", hmtx()),
        (b"loca", loca),
        (b"maxp", maxp()),
    ];
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    let mut offset = 12 + 16 * tables.len();
    for (tag, body) in &tables {
        out.extend_from_slice(*tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        offset += body.len();
    }
    for (_, body) in &tables {
        out.extend_from_slice(body);
    }
    out
}

/// Every fully covered pixel, which for a solid fill is the fill color
/// (premultiplied).
fn solid_pixels(pix: &ColorPixmap) -> Vec<[u8; 4]> {
    let mut out: Vec<[u8; 4]> = pix
        .data
        .chunks_exact(4)
        .map(|p| [p[0], p[1], p[2], p[3]])
        .filter(|p| p[3] > 0)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

#[test]
fn foreground_defaults_to_opaque_black_for_both_colr_versions() {
    let bytes = font_bytes();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).expect("face parses");
    let rast = Rasterizer::new();
    assert_eq!(rast.foreground(), [0, 0, 0, 255]);

    let v0 = rast
        .rasterize_colrv0_glyph(&face, 2, 0, 100.0, &[])
        .expect("v0 renders");
    let v1 = rast
        .rasterize_colrv1_glyph(&face, 3, 0, 100.0, &[])
        .expect("v1 renders");
    assert!(!v0.is_empty());
    assert!(
        solid_pixels(&v1).contains(&[0, 0, 0, 255]),
        "COLRv1 foreground is black, not white"
    );
    assert!(solid_pixels(&v1).iter().all(|p| p[..3] == [0, 0, 0]));
    // Same outline, same ink: the two formats agree pixel for pixel.
    assert_eq!(v0, v1);
}

#[test]
fn with_foreground_recolors_both_colr_versions() {
    let bytes = font_bytes();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).expect("face parses");
    let blue = Rasterizer::new().with_foreground([0, 0, 255, 255]);

    let v0 = blue
        .rasterize_colrv0_glyph(&face, 2, 0, 100.0, &[])
        .expect("v0 renders");
    let v1 = blue
        .rasterize_colrv1_glyph(&face, 3, 0, 100.0, &[])
        .expect("v1 renders");
    assert!(solid_pixels(&v1).contains(&[0, 0, 255, 255]));
    assert!(solid_pixels(&v1).iter().all(|p| p[..2] == [0, 0]));
    assert_eq!(v0, v1);
}

#[test]
fn paint_alpha_multiplies_the_foreground_alpha() {
    let bytes = font_bytes();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).expect("face parses");
    // Foreground alpha 128 times paint alpha 0.5: 64 at full coverage.
    let rast = Rasterizer::new().with_foreground([255, 255, 255, 128]);
    let pix = rast
        .rasterize_colrv1_glyph(&face, 4, 0, 100.0, &[])
        .expect("v1 renders");
    let max_alpha = pix.data.chunks_exact(4).map(|p| p[3]).max();
    assert_eq!(max_alpha, Some(64));
}
