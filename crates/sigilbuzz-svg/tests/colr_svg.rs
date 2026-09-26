//! Integration tests for the `color` feature path: COLRv1 -> SVG.
//!
//! These build a synthetic SFNT carrying a hand-crafted COLR + CPAL
//! pair (the technique mirrors `sigilbuzz-paint`'s evaluator tests so
//! both crates stay parser-bug-equivalent). Detailed gradient-shape
//! assertions live in the unit tests in `src/color.rs`; the
//! integration tests here cover the public API contract: when the
//! face has no COLR, when the face has COLR but no backing outline,
//! and when the face has neither (Open Sans fallback path).

#![cfg(feature = "color")]

use sigilbuzz::Face;
use sigilbuzz_svg::glyph_to_svg_color;

// =========================================================================
// SFNT builders.
// =========================================================================

fn build_face_bytes(colr: &[u8], cpal: &[u8]) -> Vec<u8> {
    let dir_len = 12 + 2 * 16;
    let cpal_off = dir_len;
    let colr_off = cpal_off + cpal.len();
    let mut out = Vec::new();
    out.extend_from_slice(&0x00010000u32.to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(b"COLR");
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(colr_off as u32).to_be_bytes());
    out.extend_from_slice(&(colr.len() as u32).to_be_bytes());
    out.extend_from_slice(b"CPAL");
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(cpal_off as u32).to_be_bytes());
    out.extend_from_slice(&(cpal.len() as u32).to_be_bytes());
    out.extend_from_slice(cpal);
    out.extend_from_slice(colr);
    out
}

fn build_cpal_v0(colors: &[(u8, u8, u8, u8)]) -> Vec<u8> {
    let num_palettes: u16 = 1;
    let entries = colors.len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&entries.to_be_bytes());
    out.extend_from_slice(&num_palettes.to_be_bytes());
    out.extend_from_slice(&entries.to_be_bytes());
    let header_plus_indices = 12 + num_palettes as usize * 2;
    out.extend_from_slice(&(header_plus_indices as u32).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    for (r, g, b, a) in colors {
        out.push(*b);
        out.push(*g);
        out.push(*r);
        out.push(*a);
    }
    out
}

fn build_v1_header(glyph_id: u16) -> Vec<u8> {
    let header_len = 34;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(header_len as u32).to_be_bytes());
    out.extend_from_slice(&(header_len as u32).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(header_len as u32).to_be_bytes());
    // Layer list, clip list, index map, variation store: none.
    out.extend_from_slice(&[0; 16]);
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&glyph_id.to_be_bytes());
    out.extend_from_slice(&10u32.to_be_bytes());
    out
}

fn f2dot14(v: f32) -> [u8; 2] {
    let raw = (v * 16384.0).round() as i16;
    raw.to_be_bytes()
}

// =========================================================================
// Linear-gradient face: COLR present, no glyf. Outline lookup yields
// nothing, so the color SVG path returns None. This pins the
// "no outline = no SVG" contract: the caller must fall back to
// glyph_to_svg or to a renderer that doesn't depend on outlines.
// =========================================================================

fn linear_gradient_face_bytes() -> Vec<u8> {
    let mut colr = build_v1_header(33);
    let paint_start = colr.len();
    colr.push(4); // PaintLinearGradient
    colr.extend_from_slice(&[0, 0, 0]);
    colr.extend_from_slice(&10i16.to_be_bytes());
    colr.extend_from_slice(&20i16.to_be_bytes());
    colr.extend_from_slice(&30i16.to_be_bytes());
    colr.extend_from_slice(&40i16.to_be_bytes());
    colr.extend_from_slice(&50i16.to_be_bytes());
    colr.extend_from_slice(&60i16.to_be_bytes());

    let cl_start = colr.len();
    let cl_rel = (cl_start - paint_start) as u32;
    colr[paint_start + 1] = ((cl_rel >> 16) & 0xff) as u8;
    colr[paint_start + 2] = ((cl_rel >> 8) & 0xff) as u8;
    colr[paint_start + 3] = (cl_rel & 0xff) as u8;
    colr.push(0); // extend = Pad
    colr.extend_from_slice(&2u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(0.0));
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));

    let cpal = build_cpal_v0(&[(255, 0, 0, 255), (0, 0, 255, 255)]);
    build_face_bytes(&colr, &cpal)
}

#[test]
fn colr_face_without_outline_returns_none() {
    let bytes = linear_gradient_face_bytes();
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    // The fixture has no glyf table; the color SVG path needs at
    // least one outline-bearing leaf. Expect None and require the
    // caller to choose a fallback.
    assert!(
        glyph_to_svg_color(&face, 33).is_none(),
        "color SVG should yield None when no outline backs the paint"
    );
}

#[test]
fn opensans_outline_only_face_returns_none_for_color() {
    // Open Sans has glyf but no COLR. glyph_to_svg_color must
    // therefore yield None; callers route to glyph_to_svg.
    let bytes: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
    let face = Face::parse_bytes(bytes, 0).unwrap();
    let gid = face.cmap().unwrap().glyph_id('A').unwrap();
    assert!(
        glyph_to_svg_color(&face, gid).is_none(),
        "Open Sans has no COLR; color path must return None"
    );
}
