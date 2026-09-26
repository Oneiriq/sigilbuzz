//! Unit tests for the bitmap decoders: PNG, EBDT mono and composite,
//! and bilinear rescale.

use alloc::vec;

use super::ebdt::blit_source_over;
use super::png::{paeth, PNG_SIGNATURE};
use super::*;

/// Builds an in-memory PNG with the given solid color. RGBA,
/// 8-bit, no interlacing. Exercises the `IHDR/IDAT/IEND` walk
/// without needing a real font fixture.
fn build_solid_rgba_png(r: u8, g: u8, b: u8, a: u8, w: u32, h: u32) -> Vec<u8> {
    let mut raw = Vec::with_capacity(((w * 4 + 1) * h) as usize);
    for _y in 0..h {
        raw.push(0u8); // filter: None
        for _x in 0..w {
            raw.push(r);
            raw.push(g);
            raw.push(b);
            raw.push(a);
        }
    }
    let idat = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);

    let mut out = Vec::new();
    out.extend_from_slice(&PNG_SIGNATURE);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // color type RGBA
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    write_chunk(&mut out, *b"IHDR", &ihdr);
    write_chunk(&mut out, *b"IDAT", &idat);
    write_chunk(&mut out, *b"IEND", &[]);
    out
}

fn write_chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    // CRC: decoder ignores it, but include 4 bytes so any future
    // CRC-checking decoder (e.g. validation tests) can still walk
    // chunk boundaries.
    out.extend_from_slice(&[0u8; 4]);
}

#[test]
fn decode_png_solid_red_2x2() {
    let png = build_solid_rgba_png(255, 0, 0, 255, 2, 2);
    let pix = decode_png(&png).unwrap();
    assert_eq!(pix.width, 2);
    assert_eq!(pix.height, 2);
    for y in 0..2 {
        for x in 0..2 {
            assert_eq!(pix.get(x, y), [255, 0, 0, 255]);
        }
    }
}

#[test]
fn decode_png_premultiplies_alpha() {
    // Half-alpha green should premultiply to (0, 128, 0, 128).
    let png = build_solid_rgba_png(0, 255, 0, 128, 1, 1);
    let pix = decode_png(&png).unwrap();
    let p = pix.get(0, 0);
    assert_eq!(p[0], 0);
    assert!((p[1] as i32 - 128).abs() <= 1);
    assert_eq!(p[2], 0);
    assert_eq!(p[3], 128);
}

#[test]
fn decode_png_rejects_bad_signature() {
    let mut bad = vec![0u8; 8];
    bad[0] = b'?';
    assert!(matches!(decode_png(&bad), Err(RenderError::BadPng(_))));
}

#[test]
fn rescale_identity_returns_clone() {
    let mut src = ColorPixmap::new(2, 2);
    src.data = vec![
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
    ];
    let out = rescale_bilinear(&src, 2, 2);
    assert_eq!(out, src);
}

#[test]
fn rescale_doubles_pixel_count() {
    let mut src = ColorPixmap::new(2, 2);
    // Top-left red, top-right green, bottom-left blue, bottom-right white.
    src.data = vec![
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
    ];
    let out = rescale_bilinear(&src, 4, 4);
    assert_eq!(out.width, 4);
    assert_eq!(out.height, 4);
    // Corners stay close to source corners.
    assert_eq!(out.get(0, 0), [255, 0, 0, 255]);
    assert_eq!(out.get(3, 0), [0, 255, 0, 255]);
    assert_eq!(out.get(0, 3), [0, 0, 255, 255]);
    assert_eq!(out.get(3, 3), [255, 255, 255, 255]);
}

#[test]
fn rescale_empty_inputs_yield_empty_output() {
    let src = ColorPixmap::new(0, 0);
    assert!(rescale_bilinear(&src, 4, 4).is_empty());
    let src = ColorPixmap::new(2, 2);
    assert!(rescale_bilinear(&src, 0, 0).is_empty());
}

// -------- EBDT mono -> RGBA --------
use sigilbuzz::tables::cblc::{BigGlyphMetrics, SmallGlyphMetrics};
use sigilbuzz::tables::ebdt::{BitPacking, EbdtBitmap, EbdtMetrics};

fn ebdt_byte_aligned(width: u8, height: u8, data: &'static [u8]) -> EbdtBitmap<'static> {
    EbdtBitmap {
        image_format: 1,
        metrics: EbdtMetrics::Small(SmallGlyphMetrics {
            height,
            width,
            bearing_x: 0,
            bearing_y: i8::try_from(height).unwrap_or(i8::MAX),
            advance: width,
        }),
        packing: BitPacking::ByteAligned,
        data,
        components_raw: &[],
    }
}

fn ebdt_bit_aligned(width: u8, height: u8, data: &'static [u8]) -> EbdtBitmap<'static> {
    EbdtBitmap {
        image_format: 5,
        metrics: EbdtMetrics::Big(BigGlyphMetrics {
            height,
            width,
            hori_bearing_x: 0,
            hori_bearing_y: i8::try_from(height).unwrap_or(i8::MAX),
            hori_advance: width,
            vert_bearing_x: 0,
            vert_bearing_y: 0,
            vert_advance: 0,
        }),
        packing: BitPacking::BitAligned,
        data,
        components_raw: &[],
    }
}

#[test]
fn ebdt_byte_aligned_decodes_mono_to_rgba() {
    // 8 wide, 2 tall, byte-aligned: 2 bytes total.
    // Row 0: 0b10101010 -> set,unset,set,unset,...
    // Row 1: 0b11110000 -> 4 set then 4 unset.
    let bm = ebdt_byte_aligned(8, 2, &[0b1010_1010, 0b1111_0000]);
    let pix = decode_ebdt_mono(&bm).unwrap();
    assert_eq!(pix.width, 8);
    assert_eq!(pix.height, 2);
    // Row 0
    assert_eq!(pix.get(0, 0), [0, 0, 0, 255]); // bit 7
    assert_eq!(pix.get(1, 0), [0, 0, 0, 0]); // bit 6
    assert_eq!(pix.get(2, 0), [0, 0, 0, 255]);
    assert_eq!(pix.get(7, 0), [0, 0, 0, 0]);
    // Row 1: first four set, last four unset.
    for x in 0..4 {
        assert_eq!(pix.get(x, 1), [0, 0, 0, 255]);
    }
    for x in 4..8 {
        assert_eq!(pix.get(x, 1), [0, 0, 0, 0]);
    }
}

#[test]
fn ebdt_byte_aligned_handles_non_byte_widths() {
    // 5 wide, 2 tall: row stride = ceil(5/8) = 1 byte. Trailing
    // 3 bits in each byte are padding.
    // Row 0: 0b11111000 -> all 5 pixels set.
    // Row 1: 0b00000000 -> all 5 pixels unset.
    let bm = ebdt_byte_aligned(5, 2, &[0b1111_1000, 0b0000_0000]);
    let pix = decode_ebdt_mono(&bm).unwrap();
    for x in 0..5 {
        assert_eq!(pix.get(x, 0), [0, 0, 0, 255]);
        assert_eq!(pix.get(x, 1), [0, 0, 0, 0]);
    }
}

#[test]
fn ebdt_bit_aligned_packs_across_rows() {
    // 5 wide, 3 tall = 15 bits; bit-aligned packs into ceil(15/8)
    // = 2 bytes with 1 bit of slack at the end.
    // Bits (MSB-first across the stream):
    //   Row 0: 1 1 1 1 1
    //   Row 1: 0 0 0 0 0
    //   Row 2: 1 0 1 0 1
    // Packed: 1111 1000 | 0010 1010 (last bit is padding)
    let bm = ebdt_bit_aligned(5, 3, &[0b1111_1000, 0b0010_1010]);
    let pix = decode_ebdt_mono(&bm).unwrap();
    for x in 0..5 {
        assert_eq!(pix.get(x, 0), [0, 0, 0, 255]);
        assert_eq!(pix.get(x, 1), [0, 0, 0, 0]);
    }
    // Row 2: 1 0 1 0 1
    assert_eq!(pix.get(0, 2), [0, 0, 0, 255]);
    assert_eq!(pix.get(1, 2), [0, 0, 0, 0]);
    assert_eq!(pix.get(2, 2), [0, 0, 0, 255]);
    assert_eq!(pix.get(3, 2), [0, 0, 0, 0]);
    assert_eq!(pix.get(4, 2), [0, 0, 0, 255]);
}

#[test]
fn ebdt_zero_size_returns_empty_pixmap() {
    let bm = ebdt_byte_aligned(0, 0, &[]);
    let pix = decode_ebdt_mono(&bm).unwrap();
    assert!(pix.is_empty());
}

#[test]
fn ebdt_short_payload_is_unsupported() {
    // 16x16 byte-aligned needs 32 bytes; provide 4.
    let bm = ebdt_byte_aligned(16, 16, &[0u8; 4]);
    let err = decode_ebdt_mono(&bm).unwrap_err();
    assert!(matches!(err, RenderError::UnsupportedBitmap));
}

#[test]
fn ebdt_decodes_an_a_shape_at_8x8() {
    // Hand-crafted 'A' shape, 8 wide x 8 tall, byte-aligned.
    //   . X X X X X . .
    //   X . . . . . X .
    //   X . . . . . X .
    //   X X X X X X X .
    //   X . . . . . X .
    //   X . . . . . X .
    //   X . . . . . X .
    //   . . . . . . . .
    static ROWS: [u8; 8] = [
        0b0111_1100,
        0b1000_0010,
        0b1000_0010,
        0b1111_1110,
        0b1000_0010,
        0b1000_0010,
        0b1000_0010,
        0b0000_0000,
    ];
    let bm = ebdt_byte_aligned(8, 8, &ROWS);
    let pix = decode_ebdt_mono(&bm).unwrap();
    // Spot-check the crossbar (row 3) is fully opaque except the
    // last column.
    for x in 0..7 {
        assert_eq!(pix.get(x, 3), [0, 0, 0, 255], "crossbar pixel {x} set");
    }
    assert_eq!(pix.get(7, 3), [0, 0, 0, 0], "crossbar tail unset");
    // The hollow center (row 1, x 1..=5) is transparent.
    for x in 1..=5 {
        assert_eq!(pix.get(x, 1), [0, 0, 0, 0]);
    }
}

#[test]
fn blit_source_over_pastes_opaque_pixels_at_offset() {
    // Source is a 2x2 fully-opaque white square; canvas is 4x4
    // black. Blitting at (1, 1) should leave (0, 0) and the right
    // / bottom edges black, and the four pixels (1,1)..(2,2) white.
    let mut dst = ColorPixmap::new(4, 4);
    for i in 0..16 {
        dst.data[i * 4 + 3] = 255; // opaque black background
    }
    let mut src = ColorPixmap::new(2, 2);
    src.data = vec![
        255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
    blit_source_over(&mut dst, &src, 1, 1);
    assert_eq!(dst.get(0, 0), [0, 0, 0, 255]);
    assert_eq!(dst.get(1, 1), [255, 255, 255, 255]);
    assert_eq!(dst.get(2, 2), [255, 255, 255, 255]);
    assert_eq!(dst.get(3, 3), [0, 0, 0, 255]);
}

#[test]
fn blit_source_over_skips_transparent_pixels() {
    let mut dst = ColorPixmap::new(2, 2);
    dst.data = vec![
        10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255,
    ];
    let original = dst.data.clone();
    let src = ColorPixmap::new(2, 2); // all-zero alpha
    blit_source_over(&mut dst, &src, 0, 0);
    // Fully-transparent source must not perturb destination.
    assert_eq!(dst.data, original);
}

#[test]
fn blit_source_over_clips_negative_offsets() {
    // Source 4x4 white, canvas 2x2 black. Blitting at (-2, -2)
    // should hit just the bottom-right 2x2 of the source onto the
    // top-left 2x2 of the canvas. No panics.
    let mut dst = ColorPixmap::new(2, 2);
    for i in 0..4 {
        dst.data[i * 4 + 3] = 255;
    }
    let mut src = ColorPixmap::new(4, 4);
    for i in 0..16 {
        src.data[i * 4] = 255;
        src.data[i * 4 + 1] = 255;
        src.data[i * 4 + 2] = 255;
        src.data[i * 4 + 3] = 255;
    }
    blit_source_over(&mut dst, &src, -2, -2);
    // All canvas pixels should now be white (the clipped portion
    // covers the whole 2x2 canvas).
    for y in 0..2 {
        for x in 0..2 {
            assert_eq!(dst.get(x, y), [255, 255, 255, 255]);
        }
    }
}

#[test]
fn ebdt_composite_format8_dispatch_routes_through_composite_path() {
    // Constructing an EbdtBitmap with image_format = 8 and a single
    // component bytes triple. The bitmap is composite-shaped:
    // is_composite() is true and component_count() reports 1.
    // (This validates the parser-side surface that the renderer
    // dispatches against; the recursive Face-driven test lives in
    // tests/bitmaps.rs.)
    let comp_raw: [u8; 4] = [0x00, 0x02, 0x00, 0x00];
    let bm = EbdtBitmap {
        image_format: 8,
        metrics: EbdtMetrics::Small(SmallGlyphMetrics {
            height: 4,
            width: 4,
            bearing_x: 0,
            bearing_y: 4,
            advance: 4,
        }),
        packing: BitPacking::ByteAligned,
        data: &[],
        components_raw: &comp_raw,
    };
    assert!(bm.is_composite());
    assert_eq!(bm.component_count(), 1);
    let c = bm.components().next().unwrap();
    assert_eq!(c.glyph_id, 2);
    assert_eq!(c.x_offset, 0);
    assert_eq!(c.y_offset, 0);
}

/// Build an 8-bit grayscale PNG with a single solid-gray pixel and
/// an arbitrary tRNS chunk payload. Used to exercise the spec's
/// "tRNS for color type 0 must be exactly 2 bytes" validation.
fn build_gray_png_with_trns(gray: u8, w: u32, h: u32, trns: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(((w + 1) * h) as usize);
    for _y in 0..h {
        raw.push(0u8); // filter: None
        for _x in 0..w {
            raw.push(gray);
        }
    }
    let idat = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);

    let mut out = Vec::new();
    out.extend_from_slice(&PNG_SIGNATURE);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(0); // color type: grayscale
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    write_chunk(&mut out, *b"IHDR", &ihdr);
    write_chunk(&mut out, *b"tRNS", trns);
    write_chunk(&mut out, *b"IDAT", &idat);
    write_chunk(&mut out, *b"IEND", &[]);
    out
}

#[test]
fn decode_png_gray_empty_trns_does_not_make_black_transparent() {
    // Issue #231: an empty tRNS chunk on a grayscale PNG used to be
    // interpreted as `Some(0)` because the decoder reached for
    // `t.last().unwrap_or(0)`. That marked every gray-0 pixel as
    // transparent: a black glyph round-tripped to a fully blank
    // pixmap.
    let png = build_gray_png_with_trns(0, 1, 1, &[]);
    let pix = decode_png(&png).unwrap();
    // The single pixel is gray 0. With the bug it decoded as
    // (0, 0, 0, 0); the spec says an empty tRNS is malformed and
    // must not mark anything transparent, so we expect opaque.
    assert_eq!(pix.get(0, 0), [0, 0, 0, 255]);
}

#[test]
fn decode_png_gray_one_byte_trns_is_ignored() {
    // tRNS for color type 0 must be 2 bytes. A 1-byte chunk used
    // to leak through as `Some(byte)`, marking arbitrary pixels
    // transparent.
    let png = build_gray_png_with_trns(7, 1, 1, &[7]);
    let pix = decode_png(&png).unwrap();
    // Without the fix, the single 7-gray pixel would be marked
    // transparent (because `t.last()` returned 7). The fix
    // rejects the malformed chunk, leaving the pixel opaque.
    assert_eq!(pix.get(0, 0), [7, 7, 7, 255]);
}

#[test]
fn decode_png_gray_three_byte_trns_is_ignored() {
    // tRNS payloads longer than 2 bytes are equally malformed.
    // `t.last()` previously surfaced the trailing byte, which
    // would mark unrelated pixels transparent.
    let png = build_gray_png_with_trns(42, 1, 1, &[0, 0, 42]);
    let pix = decode_png(&png).unwrap();
    assert_eq!(pix.get(0, 0), [42, 42, 42, 255]);
}

#[test]
fn decode_png_gray_well_formed_trns_marks_match_transparent() {
    // Sanity check that the well-formed 2-byte tRNS path keeps
    // working: the gray value 5 in the second byte (low byte of
    // the big-endian 16-bit sample) marks gray-5 pixels transparent.
    let png = build_gray_png_with_trns(5, 1, 1, &[0, 5]);
    let pix = decode_png(&png).unwrap();
    assert_eq!(pix.get(0, 0), [0, 0, 0, 0]);
}

#[test]
fn paeth_predictor_matches_spec_examples() {
    // PNG spec: p = a + b - c; predictor = whichever of {a, b, c}
    // is closest to p (ties -> a, then b).
    // a=10 b=20 c=30 -> p=0; pa=10 pb=20 pc=30 -> returns a=10.
    assert_eq!(paeth(10, 20, 30), 10);
    // a=b=c -> p == a, all distances zero -> a wins.
    assert_eq!(paeth(50, 50, 50), 50);
    // a=0 b=0 c=255 -> p = -255; pa=255 pb=255 pc=510 -> tie pa==pb,
    // ties prefer a.
    assert_eq!(paeth(0, 0, 255), 0);
}
