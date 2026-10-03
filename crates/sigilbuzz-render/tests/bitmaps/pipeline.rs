//! Fixture-driven CBDT / sbix pipeline tests, error surfaces, and
//! PNG / rescale smoke tests.

use sigilbuzz::{Blob, Face};
use sigilbuzz_render::{
    decode_png, rescale_bilinear, ColorPixmap, Placement, Rasterizer, RenderError,
};

use crate::fixtures::hex_to_bytes;

const CBDT_FONT: &[u8] = include_bytes!("../../../../tests/fixtures/cbdt_synthetic.ttf");
const SBIX_FONT: &[u8] = include_bytes!("../../../../tests/fixtures/sbix_synthetic.ttf");
const OPEN_SANS: &[u8] = include_bytes!("../../../../tests/fixtures/opensans_regular.ttf");

#[test]
fn cbdt_synthetic_renders_at_strike_size() {
    let blob = Blob::new(CBDT_FONT);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    // Strike is 32 ppem; ask for 32 -> no rescale.
    let pix = rast
        .rasterize_bitmap_glyph(&face, 1, 32.0, &[])
        .expect("CBDT bitmap renders");
    // Synthetic PNG is 1x1 RGBA-transparent.
    assert_eq!(pix.width, 1);
    assert_eq!(pix.height, 1);
    assert_eq!(pix.get(0, 0), [0, 0, 0, 0]);
}

#[test]
fn cbdt_synthetic_rescales_when_size_off_strike() {
    let blob = Blob::new(CBDT_FONT);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    // Strike is 32 ppem; ask for 64 -> 2x upscale of a 1x1 to 2x2.
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

/// Issue #226: a hostile combination of small-ppem strike + extreme
/// `size_pt` used to multiply up to a `u32::MAX * u32::MAX * 4`
/// allocation that panicked with "capacity overflow" in
/// `ColorPixmap::new`. The fix caps the rescale target at 16384 per
/// dim (matching the PNG decoder ceiling) and surfaces the structured
/// `BadSize` error.
#[test]
fn rasterize_bitmap_extreme_size_pt_returns_bad_size_not_oom_panic() {
    let blob = Blob::new(CBDT_FONT);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    // Strike is 32 ppem with a 1x1 PNG. size_pt = 1e9 yields scale =
    // 1e9 / 32 ~ 3.1e7, dst dims would be ~3.1e7, way past the cap.
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 1.0e9, &[])
        .expect_err("extreme size_pt must not OOM-panic");
    assert!(
        matches!(err, RenderError::BadSize(_)),
        "expected BadSize, got {err:?}"
    );
}

/// Public `rescale_bilinear` mirror: an out-of-range `dst_w` /
/// `dst_h` (e.g. caller miscomputing from a hostile size_pt) must
/// return an empty pixmap instead of panicking in the destination
/// allocation.
#[test]
fn rescale_bilinear_extreme_target_is_empty_not_panic() {
    let src = ColorPixmap::new(2, 2);
    let out = rescale_bilinear(&src, u32::MAX, u32::MAX);
    assert!(
        out.is_empty(),
        "extreme dst dims must clamp to empty, got {}x{}",
        out.width,
        out.height
    );
}

#[test]
fn decode_png_round_trips_with_known_payload() {
    // 1x1 fully transparent RGBA PNG. Built deterministically by
    // `build_cbdt_fixture.py`'s `_make_png(1, 1, b"\x00\x00\x00\x00")`
    // (the same shape the CBDT/sbix fixtures embed).
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

#[test]
fn cbdt_synthetic_placement_follows_the_bearings() {
    // gid 1's small metrics: bearing (0, 10) at the 32 ppem strike.
    let blob = Blob::new(CBDT_FONT);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let at = |size: f32| {
        let (pix, at) = rast
            .rasterize_bitmap_glyph_placed(&face, 1, size, &[])
            .unwrap();
        assert_eq!(
            pix,
            rast.rasterize_bitmap_glyph(&face, 1, size, &[]).unwrap()
        );
        at
    };
    assert_eq!(at(32.0), Placement::new(0, -10));
    // Scaled with the bitmap: exact at twice the strike, rounded to the
    // nearest pixel (away from zero on a tie) at 1.25 times.
    assert_eq!(at(64.0), Placement::new(0, -20));
    assert_eq!(at(40.0), Placement::new(0, -13));
}

#[test]
fn sbix_synthetic_placement_rests_on_the_baseline() {
    // Zero origin offset: the 1x1 image's bottom edge is the baseline.
    let blob = Blob::new(SBIX_FONT);
    let face = Face::parse(&blob, 0).unwrap();
    let (pix, at) = Rasterizer::new()
        .rasterize_bitmap_glyph_placed(&face, 1, 32.0, &[])
        .unwrap();
    assert_eq!((pix.width, pix.height), (1, 1));
    assert_eq!(at, Placement::new(0, -1));
}
