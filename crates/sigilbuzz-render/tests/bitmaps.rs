//! Integration tests for embedded bitmap rasterization.
//!
//! Drives [`Rasterizer::rasterize_bitmap_glyph`] against the
//! synthetic CBDT and sbix fixtures already used by the parser tests
//! (`tests/fixtures/cbdt_synthetic.ttf`,
//! `tests/fixtures/sbix_synthetic.ttf`). Both fixtures ship a 1×1
//! transparent RGBA PNG at a 32 ppem strike — small but enough to
//! validate the full pipeline (face → strike → PNG decode → rescale).

use sigilbuzz::{Blob, Face};
use sigilbuzz_render::{decode_png, rescale_bilinear, ColorPixmap, Rasterizer, RenderError};

const CBDT_FONT: &[u8] = include_bytes!("../../../tests/fixtures/cbdt_synthetic.ttf");
const SBIX_FONT: &[u8] = include_bytes!("../../../tests/fixtures/sbix_synthetic.ttf");
const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

#[test]
fn cbdt_synthetic_renders_at_strike_size() {
    let blob = Blob::new(CBDT_FONT);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    // Strike is 32 ppem; ask for 32 → no rescale.
    let pix = rast
        .rasterize_bitmap_glyph(&face, 1, 32.0, &[])
        .expect("CBDT bitmap renders");
    // Synthetic PNG is 1×1 RGBA-transparent.
    assert_eq!(pix.width, 1);
    assert_eq!(pix.height, 1);
    assert_eq!(pix.get(0, 0), [0, 0, 0, 0]);
}

#[test]
fn cbdt_synthetic_rescales_when_size_off_strike() {
    let blob = Blob::new(CBDT_FONT);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    // Strike is 32 ppem; ask for 64 → 2× upscale of a 1×1 to 2×2.
    let pix = rast
        .rasterize_bitmap_glyph(&face, 1, 64.0, &[])
        .expect("CBDT bitmap renders at upscaled size");
    assert_eq!(pix.width, 2);
    assert_eq!(pix.height, 2);
}

#[test]
fn sbix_synthetic_renders_at_strike_size() {
    let blob = Blob::new(SBIX_FONT);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast
        .rasterize_bitmap_glyph(&face, 1, 32.0, &[])
        .expect("sbix bitmap renders");
    assert_eq!(pix.width, 1);
    assert_eq!(pix.height, 1);
    assert_eq!(pix.get(0, 0), [0, 0, 0, 0]);
}

#[test]
fn outline_only_font_returns_no_bitmap() {
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 32.0, &[])
        .expect_err("Open Sans has no bitmap embeds");
    assert!(matches!(err, RenderError::NoBitmap(1)));
}

#[test]
fn bad_size_yields_bad_size_error() {
    let blob = Blob::new(CBDT_FONT);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    for &bad in &[0.0_f32, -1.0, f32::NAN, f32::INFINITY] {
        let err = rast.rasterize_bitmap_glyph(&face, 1, bad, &[]).unwrap_err();
        assert!(matches!(err, RenderError::BadSize(_)));
    }
}

#[test]
fn rescale_zero_size_is_empty() {
    let pix = ColorPixmap::new(2, 2);
    assert!(rescale_bilinear(&pix, 0, 4).is_empty());
}

#[test]
fn decode_png_round_trips_with_known_payload() {
    // 1×1 fully transparent RGBA PNG. Built deterministically by
    // `build_cbdt_fixture.py`'s `_make_png(1, 1, b"\x00\x00\x00\x00")`
    // — the same shape the CBDT/sbix fixtures embed.
    let png_hex = concat!(
        "89504e470d0a1a0a",
        "0000000d49484452",
        "00000001000000010806000000",
        "1f15c489",
        "0000000b49444154789c6360000200000500017a5eab3f",
        "0000000049454e44ae426082",
    );
    let bytes = hex_to_bytes(png_hex);
    let pix = decode_png(&bytes).expect("known-good PNG decodes");
    assert_eq!(pix.width, 1);
    assert_eq!(pix.height, 1);
    assert_eq!(pix.get(0, 0), [0, 0, 0, 0]);
}

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = from_hex(bytes[i]);
        let lo = from_hex(bytes[i + 1]);
        out.push(hi << 4 | lo);
        i += 2;
    }
    out
}

fn from_hex(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => panic!("non-hex digit"),
    }
}
