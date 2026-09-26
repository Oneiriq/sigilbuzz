//! sbix `'dupe'`, `'jpg '`, `'jp2 '`, and `'tiff'` payload tests on
//! synthetic fonts.

use sigilbuzz::Face;
use sigilbuzz_render::{decode_jpeg, Rasterizer, RenderError};

use crate::fixtures::{build_sbix_strike, build_sfnt, hex_to_bytes, maxp_05};

#[test]
fn sbix_dupe_tag_recurses_to_target_gid() {
    // gid 0: empty; gid 1: dupe -> gid 2; gid 2: real PNG (1x1).
    let png_hex = concat!(
        "89504e470d0a1a0a",
        "0000000d49484452",
        "00000001000000010806000000",
        "1f15c489",
        "0000000b49444154789c6360000200000500017a5eab3f",
        "0000000049454e44ae426082",
    );
    let png = hex_to_bytes(png_hex);
    let glyphs = vec![
        None,                               // gid 0 .notdef
        Some((*b"dupe", vec![0x00, 0x02])), // gid 1 -> dupe to gid 2
        Some((*b"png ", png)),              // gid 2 carries the PNG
    ];
    let sbix = build_sbix_strike(3, 16, &glyphs);
    let font = build_sfnt(vec![(*b"maxp", maxp_05(3)), (*b"sbix", sbix)]);

    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .expect("dupe-tag should resolve to gid 2's PNG");
    assert_eq!(pix.width, 1);
    assert_eq!(pix.height, 1);
}

#[test]
fn sbix_dupe_self_reference_is_unsupported() {
    // gid 1 dupes back to gid 1. Guarded against; surfaces Unsupported.
    let glyphs = vec![None, Some((*b"dupe", vec![0x00, 0x01]))];
    let sbix = build_sbix_strike(2, 16, &glyphs);
    let font = build_sfnt(vec![(*b"maxp", maxp_05(2)), (*b"sbix", sbix)]);

    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .expect_err("self-dupe should refuse to recurse");
    assert!(matches!(err, RenderError::UnsupportedBitmap));
}

#[test]
fn sbix_dupe_chain_exceeding_depth_cap_is_unsupported() {
    // gid 1->2->3->4->5->6 is 5 hops; SBIX_DUPE_MAX_DEPTH is 4 so this
    // bottoms out without panicking and without infinite-looping.
    let glyphs = vec![
        None,
        Some((*b"dupe", vec![0x00, 0x02])),
        Some((*b"dupe", vec![0x00, 0x03])),
        Some((*b"dupe", vec![0x00, 0x04])),
        Some((*b"dupe", vec![0x00, 0x05])),
        Some((*b"dupe", vec![0x00, 0x06])),
        Some((*b"dupe", vec![0x00, 0x01])), // closes the loop
    ];
    let sbix = build_sbix_strike(7, 16, &glyphs);
    let font = build_sfnt(vec![(*b"maxp", maxp_05(7)), (*b"sbix", sbix)]);

    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .expect_err("dupe chain exceeding depth cap should refuse");
    assert!(matches!(err, RenderError::UnsupportedBitmap));
}

#[test]
fn sbix_jpg_truncated_payload_returns_bad_jpeg() {
    // sbix carrying a malformed 'jpg ' payload: the in-crate JPEG
    // decoder rejects it cleanly with BadJpeg rather than panicking.
    // (Pre-0.20 this surfaced UnsupportedBitmap because we didn't
    // even try; now we do, so a truncated APP0-only stream fails at
    // the marker walker.)
    let glyphs = vec![None, Some((*b"jpg ", vec![0xff, 0xd8, 0xff, 0xe0]))];
    let sbix = build_sbix_strike(2, 16, &glyphs);
    let font = build_sfnt(vec![(*b"maxp", maxp_05(2)), (*b"sbix", sbix)]);

    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .expect_err("malformed jpg sbix should surface BadJpeg");
    assert!(matches!(err, RenderError::BadJpeg(_)));
}

#[test]
fn decode_jpeg_rejects_empty_input() {
    // Public API smoke test: confirm the JPEG decoder is wired up and
    // surfaces BadJpeg on a clearly-malformed (empty) payload rather
    // than panicking.
    let err = decode_jpeg(&[]).unwrap_err();
    assert!(matches!(err, RenderError::BadJpeg(_)));
}

#[test]
fn sbix_tiff_truncated_surfaces_bad_tiff() {
    // 4-byte payload trips the baseline TIFF decoder's header-length
    // guard; the dedicated `BadTiff` arm replaces the old wholesale
    // `UnsupportedBitmap` deferral.
    let glyphs = vec![None, Some((*b"tiff", vec![0x49, 0x49, 0x2a, 0x00]))];
    let sbix = build_sbix_strike(2, 16, &glyphs);
    let font = build_sfnt(vec![(*b"maxp", maxp_05(2)), (*b"sbix", sbix)]);

    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .unwrap_err();
    assert!(matches!(err, RenderError::BadTiff(_)));
}

#[test]
fn sbix_jp2_returns_unsupported_not_panic() {
    let glyphs = vec![None, Some((*b"jp2 ", vec![0x00, 0x00, 0x00, 0x0c]))];
    let sbix = build_sbix_strike(2, 16, &glyphs);
    let font = build_sfnt(vec![(*b"maxp", maxp_05(2)), (*b"sbix", sbix)]);

    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .unwrap_err();
    assert!(matches!(err, RenderError::UnsupportedBitmap));
}

// =========================================================================
// Wave-21 adversarial: sbix `'tiff'` payload-shape coverage.
//
// The TIFF decoder is owned by the sibling PR (feature/sbix-tiff-decoder)
// and not yet on release/0.21.0. Every payload here lands at the
// `UnsupportedBitmap` fast path. The point of these tests is to pin
// no-panic behavior for the malformed shapes the sibling brief calls
// out (bad magic / wrong byte order / unknown compression / truncated
// strip / multi-IFD) so when the sibling implementation lands, the
// regression bar already has the adversarial fixtures wired up.
// =========================================================================

/// Truncated TIFF header (just the LE byte-order bytes, no magic).
#[test]
fn sbix_tiff_bad_magic_returns_unsupported() {
    // "II" little-endian intent but missing magic 42 + IFD offset.
    let payload = vec![0x49, 0x49, 0x00, 0x00];
    let glyphs = vec![None, Some((*b"tiff", payload))];
    let sbix = build_sbix_strike(2, 16, &glyphs);
    let font = build_sfnt(vec![(*b"maxp", maxp_05(2)), (*b"sbix", sbix)]);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .unwrap_err();
    assert!(
        matches!(
            err,
            RenderError::UnsupportedBitmap | RenderError::BadTiff(_)
        ),
        "got {err:?}"
    );
}

/// Big-endian "MM" signature with magic. Sibling brief calls out
/// "wrong byte order"; the TIFF decoder either accepts BE (returns
/// BadTiff for the empty content) or rejects up front. Either way:
/// no panic.
#[test]
fn sbix_tiff_big_endian_byte_order_returns_unsupported() {
    let payload = vec![0x4D, 0x4D, 0x00, 0x2A, 0x00, 0x00, 0x00, 0x08];
    let glyphs = vec![None, Some((*b"tiff", payload))];
    let sbix = build_sbix_strike(2, 16, &glyphs);
    let font = build_sfnt(vec![(*b"maxp", maxp_05(2)), (*b"sbix", sbix)]);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .unwrap_err();
    assert!(
        matches!(
            err,
            RenderError::UnsupportedBitmap | RenderError::BadTiff(_)
        ),
        "got {err:?}"
    );
}

/// Empty payload. Most parsers explode on unindexed reads: the
/// dispatcher / TIFF decoder must catch this before any unguarded
/// indexing happens.
#[test]
fn sbix_tiff_empty_payload_returns_unsupported() {
    let glyphs = vec![None, Some((*b"tiff", vec![]))];
    let sbix = build_sbix_strike(2, 16, &glyphs);
    let font = build_sfnt(vec![(*b"maxp", maxp_05(2)), (*b"sbix", sbix)]);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .unwrap_err();
    assert!(
        matches!(
            err,
            RenderError::UnsupportedBitmap | RenderError::BadTiff(_)
        ),
        "got {err:?}"
    );
}

/// Multi-IFD shape: single IFD containing one entry whose chained
/// next-IFD offset points back to itself (would loop forever in a
/// naive walker). Either rejected outright or surfaced as BadTiff,
/// must NOT recurse forever.
#[test]
fn sbix_tiff_multi_ifd_self_chain_returns_unsupported() {
    // II 42 IFD-off=8; IFD: count=1; entry (12 bytes of zeros);
    // next-IFD-offset = 8 (same as first IFD -> cycle).
    let mut payload = vec![0x49, 0x49, 0x2a, 0x00];
    payload.extend_from_slice(&8u32.to_le_bytes());
    payload.extend_from_slice(&1u16.to_le_bytes()); // entry count
    payload.extend_from_slice(&[0u8; 12]); // single IFD entry
    payload.extend_from_slice(&8u32.to_le_bytes()); // next IFD = 8 (cycle)
    let glyphs = vec![None, Some((*b"tiff", payload))];
    let sbix = build_sbix_strike(2, 16, &glyphs);
    let font = build_sfnt(vec![(*b"maxp", maxp_05(2)), (*b"sbix", sbix)]);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .unwrap_err();
    assert!(
        matches!(
            err,
            RenderError::UnsupportedBitmap | RenderError::BadTiff(_)
        ),
        "got {err:?}"
    );
}

/// Truncated-strip shape: claims to have an image but strip-offset
/// points past EOF.
#[test]
fn sbix_tiff_truncated_strip_returns_unsupported() {
    // Magic + IFD pointer to offset 8; entry tag 273 (StripOffsets)
    // claiming the strip starts at offset 0xFFFF (past EOF).
    let mut payload = vec![0x49, 0x49, 0x2a, 0x00];
    payload.extend_from_slice(&8u32.to_le_bytes());
    payload.extend_from_slice(&1u16.to_le_bytes());
    payload.extend_from_slice(&273u16.to_le_bytes()); // StripOffsets
    payload.extend_from_slice(&4u16.to_le_bytes()); // type LONG
    payload.extend_from_slice(&1u32.to_le_bytes()); // count
    payload.extend_from_slice(&0xFFFFu32.to_le_bytes()); // value (OOB)
    payload.extend_from_slice(&0u32.to_le_bytes()); // next IFD = 0
    let glyphs = vec![None, Some((*b"tiff", payload))];
    let sbix = build_sbix_strike(2, 16, &glyphs);
    let font = build_sfnt(vec![(*b"maxp", maxp_05(2)), (*b"sbix", sbix)]);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .unwrap_err();
    assert!(
        matches!(
            err,
            RenderError::UnsupportedBitmap | RenderError::BadTiff(_)
        ),
        "got {err:?}"
    );
}

/// Unknown-compression shape: tag 259 (Compression) with an
/// unrecognized value.
#[test]
fn sbix_tiff_unknown_compression_returns_unsupported() {
    let mut payload = vec![0x49, 0x49, 0x2a, 0x00];
    payload.extend_from_slice(&8u32.to_le_bytes());
    payload.extend_from_slice(&1u16.to_le_bytes());
    payload.extend_from_slice(&259u16.to_le_bytes()); // Compression
    payload.extend_from_slice(&3u16.to_le_bytes()); // type SHORT
    payload.extend_from_slice(&1u32.to_le_bytes()); // count
    payload.extend_from_slice(&0xFFFFu32.to_le_bytes()); // unknown comp
    payload.extend_from_slice(&0u32.to_le_bytes());
    let glyphs = vec![None, Some((*b"tiff", payload))];
    let sbix = build_sbix_strike(2, 16, &glyphs);
    let font = build_sfnt(vec![(*b"maxp", maxp_05(2)), (*b"sbix", sbix)]);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .unwrap_err();
    assert!(
        matches!(
            err,
            RenderError::UnsupportedBitmap | RenderError::BadTiff(_)
        ),
        "got {err:?}"
    );
}
