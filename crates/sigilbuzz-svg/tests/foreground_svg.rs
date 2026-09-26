//! `glyph_to_svg_color` fills COLR foreground paints with
//! `currentColor`, so an embedded glyph takes the surrounding text
//! color, and fills palette entries the font cannot supply with the
//! evaluator's opaque black foreground.
//!
//! The fixture has a real square outline (gid 1) so the SVG has a path
//! to fill, and COLRv1 glyphs clipping it: gid 3 foreground at alpha
//! 1, gid 4 foreground at alpha 0.5, gid 5 palette red, gid 6 a
//! palette entry past the end of the palette.

#![cfg(feature = "color")]

use sigilbuzz::Face;
use sigilbuzz_svg::glyph_to_svg_color;

fn align4(v: &mut Vec<u8>) {
    while v.len() % 4 != 0 {
        v.push(0);
    }
}

fn f2dot14(v: f32) -> [u8; 2] {
    ((v * 16384.0).round() as i16).to_be_bytes()
}

fn square() -> Vec<u8> {
    let mut g = Vec::new();
    for v in [1i16, 0, 0, 200, 200] {
        g.extend_from_slice(&v.to_be_bytes());
    }
    g.extend_from_slice(&3u16.to_be_bytes());
    g.extend_from_slice(&0u16.to_be_bytes());
    g.extend_from_slice(&[0x01; 4]);
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
    h.extend_from_slice(&1000u16.to_be_bytes());
    h.extend_from_slice(&[0; 16]);
    for v in [0i16, 0, 200, 200] {
        h.extend_from_slice(&v.to_be_bytes());
    }
    for v in [0u16, 8] {
        h.extend_from_slice(&v.to_be_bytes());
    }
    for v in [2i16, 0, 0] {
        h.extend_from_slice(&v.to_be_bytes());
    }
    align4(&mut h);
    h
}

fn maxp() -> Vec<u8> {
    let mut m = Vec::new();
    m.extend_from_slice(&0x0000_5000u32.to_be_bytes());
    m.extend_from_slice(&7u16.to_be_bytes());
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
    h.extend_from_slice(&7u16.to_be_bytes());
    align4(&mut h);
    h
}

fn hmtx() -> Vec<u8> {
    let mut m = Vec::new();
    for _ in 0..7 {
        m.extend_from_slice(&500u16.to_be_bytes());
        m.extend_from_slice(&0i16.to_be_bytes());
    }
    m
}

fn glyf_loca() -> (Vec<u8>, Vec<u8>) {
    let glyf = square();
    let n = glyf.len();
    let mut loca = Vec::new();
    for off in [0usize, 0, n, n, n, n, n, n] {
        loca.extend_from_slice(&((off / 2) as u16).to_be_bytes());
    }
    align4(&mut loca);
    (glyf, loca)
}

fn cpal() -> Vec<u8> {
    let mut out = Vec::new();
    for v in [0u16, 1, 1, 1] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(&14u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&[0, 0, 255, 255]); // red
    align4(&mut out);
    out
}

/// PaintGlyph(1) over PaintSolid(entry, alpha).
fn fill(entry: u16, alpha: f32) -> Vec<u8> {
    let mut p = vec![10u8, 0, 0, 6];
    p.extend_from_slice(&1u16.to_be_bytes());
    p.push(2);
    p.extend_from_slice(&entry.to_be_bytes());
    p.extend_from_slice(&f2dot14(alpha));
    p
}

fn colr() -> Vec<u8> {
    let paints = [
        (3u16, fill(0xFFFF, 1.0)),
        (4, fill(0xFFFF, 0.5)),
        (5, fill(0, 1.0)),
        (6, fill(9, 1.0)),
    ];
    let mut c = Vec::new();
    c.extend_from_slice(&1u16.to_be_bytes());
    c.extend_from_slice(&0u16.to_be_bytes());
    c.extend_from_slice(&30u32.to_be_bytes());
    c.extend_from_slice(&30u32.to_be_bytes());
    c.extend_from_slice(&0u16.to_be_bytes());
    c.extend_from_slice(&30u32.to_be_bytes());
    c.extend_from_slice(&[0; 12]);
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

fn svg(gid: u16) -> String {
    let bytes = font_bytes();
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    glyph_to_svg_color(&face, gid).expect("color glyph renders")
}

#[test]
fn foreground_fill_is_current_color() {
    let out = svg(3);
    assert!(out.contains(r#"fill="currentColor""#), "{out}");
    assert!(!out.contains("fill-opacity"), "{out}");
    assert!(!out.contains("rgb("), "{out}");
}

#[test]
fn foreground_fill_keeps_the_paint_alpha() {
    let out = svg(4);
    assert!(
        out.contains(r#"fill="currentColor" fill-opacity="0.5""#),
        "{out}"
    );
}

#[test]
fn palette_fill_stays_a_literal_color() {
    let out = svg(5);
    assert!(out.contains(r#"fill="rgb(255,0,0)""#), "{out}");
    assert!(!out.contains("currentColor"), "{out}");
}

#[test]
fn missing_palette_entry_fills_black() {
    let out = svg(6);
    assert!(out.contains(r#"fill="rgb(0,0,0)""#), "{out}");
    assert!(!out.contains("currentColor"), "{out}");
}
