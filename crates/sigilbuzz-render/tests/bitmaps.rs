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

/// Issue #226: a hostile combination of small-ppem strike + extreme
/// `size_pt` used to multiply up to a `u32::MAX × u32::MAX × 4`
/// allocation that panicked with "capacity overflow" in
/// `ColorPixmap::new`. The fix caps the rescale target at 16384 per
/// dim (matching the PNG decoder ceiling) and surfaces the structured
/// `BadSize` error.
#[test]
fn rasterize_bitmap_extreme_size_pt_returns_bad_size_not_oom_panic() {
    let blob = Blob::new(CBDT_FONT);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    // Strike is 32 ppem with a 1×1 PNG. size_pt = 1e9 yields scale =
    // 1e9 / 32 ≈ 3.1e7, dst dims would be ≈ 3.1e7 — way past the cap.
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
        "extreme dst dims must clamp to empty, got {}×{}",
        out.width,
        out.height
    );
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

// ---------------------------------------------------------------------------
// Synthetic SFNT builder for sbix dupe-tag and Unsupported-bitmap tests.
//
// Builds a minimal SFNT carrying just `maxp` and `sbix`. The rest of
// the font directory is unnecessary because Face::glyph_bitmap only
// needs maxp.numGlyphs to walk sbix; the renderer never touches hmtx /
// head / cmap on the bitmap path.
// ---------------------------------------------------------------------------

/// Builds an SFNT directory + payload with the given tagged tables.
/// Tables are written in the order supplied; the SFNT directory is
/// sorted by tag, as the spec requires.
fn build_sfnt(mut tables: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
    tables.sort_by_key(|(tag, _)| *tag);
    let n = tables.len();
    let header_len = 12 + 16 * n;
    let mut payloads_off: u32 = header_len as u32;
    // Pad each payload to a 4-byte boundary, as the SFNT spec requires.
    let padded_lens: Vec<usize> = tables.iter().map(|(_, p)| (p.len() + 3) & !3).collect();
    let total: usize = header_len + padded_lens.iter().sum::<usize>();

    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // sfnt_version (TrueType)
    out.extend_from_slice(&(n as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // searchRange
    out.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
    out.extend_from_slice(&0u16.to_be_bytes()); // rangeShift

    for ((tag, payload), &padded) in tables.iter().zip(padded_lens.iter()) {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes()); // checksum (parser ignores)
        out.extend_from_slice(&payloads_off.to_be_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        payloads_off += padded as u32;
    }
    for ((_, payload), &padded) in tables.iter().zip(padded_lens.iter()) {
        out.extend_from_slice(payload);
        out.resize(out.len() + (padded - payload.len()), 0);
    }
    out
}

fn maxp_05(num_glyphs: u16) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&0x0000_5000u32.to_be_bytes()); // version 0.5
    b.extend_from_slice(&num_glyphs.to_be_bytes());
    b
}

/// Builds an `sbix` table with a single 16-ppem strike whose glyphs
/// carry the supplied tagged payloads. `glyphs[gid] = Some((tag,
/// payload))` writes a glyph entry; `None` writes an empty (length-0)
/// slot. `glyphs.len()` must equal `num_glyphs`.
fn build_sbix_strike(num_glyphs: u16, ppem: u16, glyphs: &[Option<([u8; 4], Vec<u8>)>]) -> Vec<u8> {
    assert_eq!(glyphs.len(), num_glyphs as usize);
    // Strike body: header (4) + offsets[num_glyphs+1] (u32 each) + per-glyph payloads.
    let offset_arr_bytes = 4 * (num_glyphs as usize + 1);
    let mut offsets = Vec::with_capacity(num_glyphs as usize + 1);
    let mut payloads = Vec::new();
    let mut cursor: u32 = 4 + offset_arr_bytes as u32;
    for slot in glyphs {
        offsets.push(cursor);
        if let Some((tag, payload)) = slot {
            // 4 bytes of origin offset, 4-byte tag, payload bytes.
            payloads.extend_from_slice(&[0u8; 4]);
            payloads.extend_from_slice(tag);
            payloads.extend_from_slice(payload);
            cursor += 8 + payload.len() as u32;
        }
    }
    offsets.push(cursor); // sentinel
    let mut strike = Vec::new();
    strike.extend_from_slice(&ppem.to_be_bytes());
    strike.extend_from_slice(&72u16.to_be_bytes()); // ppi
    for o in &offsets {
        strike.extend_from_slice(&o.to_be_bytes());
    }
    strike.extend_from_slice(&payloads);

    // sbix table: header (8) + strike offsets (u32 per strike) + strike body.
    let mut sbix = Vec::new();
    sbix.extend_from_slice(&1u16.to_be_bytes()); // version
    sbix.extend_from_slice(&0u16.to_be_bytes()); // flags
    sbix.extend_from_slice(&1u32.to_be_bytes()); // numStrikes
    let strike_off: u32 = 8 + 4;
    sbix.extend_from_slice(&strike_off.to_be_bytes());
    sbix.extend_from_slice(&strike);
    sbix
}

#[test]
fn sbix_dupe_tag_recurses_to_target_gid() {
    // gid 0: empty; gid 1: dupe → gid 2; gid 2: real PNG (1×1).
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
        Some((*b"dupe", vec![0x00, 0x02])), // gid 1 → dupe to gid 2
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
    // gid 1 dupes back to gid 1 — guarded against; surfaces Unsupported.
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
    // gid 1→2→3→4→5→6 is 5 hops; SBIX_DUPE_MAX_DEPTH is 4 so this
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
fn sbix_jpg_returns_unsupported_not_panic() {
    // sbix carrying a 'jpg ' payload — sigilbuzz-render surfaces
    // UnsupportedBitmap rather than attempting a decode.
    let glyphs = vec![None, Some((*b"jpg ", vec![0xff, 0xd8, 0xff, 0xe0]))];
    let sbix = build_sbix_strike(2, 16, &glyphs);
    let font = build_sfnt(vec![(*b"maxp", maxp_05(2)), (*b"sbix", sbix)]);

    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .expect_err("jpg sbix should surface Unsupported");
    assert!(matches!(err, RenderError::UnsupportedBitmap));
}

#[test]
fn sbix_tiff_returns_unsupported_not_panic() {
    let glyphs = vec![None, Some((*b"tiff", vec![0x49, 0x49, 0x2a, 0x00]))];
    let sbix = build_sbix_strike(2, 16, &glyphs);
    let font = build_sfnt(vec![(*b"maxp", maxp_05(2)), (*b"sbix", sbix)]);

    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .unwrap_err();
    assert!(matches!(err, RenderError::UnsupportedBitmap));
}

/// Builds an EBDT format-1 entry: 5-byte SmallGlyphMetrics followed
/// by `data` (byte-aligned 1bpp mask).
fn ebdt_format1_entry(width: u8, height: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![
        height,
        width,
        0,                  // bearing_x
        height as i8 as u8, // bearing_y
        width,              // advance
    ];
    out.extend_from_slice(data);
    out
}

/// Builds an EBLC table with one strike at `ppem` covering glyph ids
/// 1..=1, using index format 1 (variable-metric u32 offsets), pointing
/// at an EBDT table where `image_data_offset` plus the offset locates
/// the entry.
fn build_eblc_one_glyph(ppem: u8, glyph_size: u32) -> Vec<u8> {
    let mut blob = Vec::new();
    // EBLC header
    blob.extend_from_slice(&2u16.to_be_bytes()); // major (v2)
    blob.extend_from_slice(&0u16.to_be_bytes()); // minor
    blob.extend_from_slice(&1u32.to_be_bytes()); // numSizes

    // BitmapSize record (48 B)
    let bitmap_size_off = blob.len() as u32; // 8
    let index_array_off = bitmap_size_off + 48; // 56
    blob.extend_from_slice(&index_array_off.to_be_bytes());
    // indexTablesSize: array entry (8) + sub header (8) + 2 u32 offsets (8) = 24
    blob.extend_from_slice(&24u32.to_be_bytes());
    blob.extend_from_slice(&1u32.to_be_bytes()); // numberOfIndexSubTables
    blob.extend_from_slice(&0u32.to_be_bytes()); // colorRef
    blob.extend_from_slice(&[0u8; 12]); // hori metrics
    blob.extend_from_slice(&[0u8; 12]); // vert metrics
    blob.extend_from_slice(&1u16.to_be_bytes()); // startGlyphIndex
    blob.extend_from_slice(&1u16.to_be_bytes()); // endGlyphIndex
    blob.push(ppem);
    blob.push(ppem);
    blob.push(1); // bitDepth = mono
    blob.push(0x01); // flags = horizontal

    // IndexSubTableArray entry — first/last/additionalOffset (=8, just past this entry)
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.extend_from_slice(&8u32.to_be_bytes()); // additional offset (relative to array base)

    // IndexSubTable header: format 1 (var-metric u32), image format 1, image data offset 0.
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes());
    // Two offsets: 0, glyph_size (sentinel)
    blob.extend_from_slice(&0u32.to_be_bytes());
    blob.extend_from_slice(&glyph_size.to_be_bytes());
    blob
}

fn build_ebdt(entry: &[u8]) -> Vec<u8> {
    let mut blob = Vec::new();
    blob.extend_from_slice(&2u16.to_be_bytes()); // major
    blob.extend_from_slice(&0u16.to_be_bytes()); // minor
    blob.extend_from_slice(entry);
    blob
}

#[test]
fn ebdt_synthetic_renders_mono_glyph_at_strike_size() {
    // 16×16 'A' silhouette — byte-aligned 1bpp, 32 bytes (2 per row).
    // Just need a recognisable shape; spot-check a known set bit.
    let mut mask = vec![0u8; 32];
    // Top: row 0 has crossbar bits in middle.
    mask[0] = 0b0011_1100;
    mask[1] = 0b0011_1100;
    // Bottom-left descender: row 15 has bit 0 set on left.
    mask[30] = 0b1100_0000;
    mask[31] = 0b0000_0011;

    let entry = ebdt_format1_entry(16, 16, &mask);
    // EBDT table = 4-byte header + entry; EBLC's image_data_offset is
    // 0, so glyph offset 0 lands at the *start* of EBDT (the header).
    // Shift the entry into a position the EBLC offset can address by
    // wrapping it — but our EBLC builder uses image_data_offset = 0,
    // so the glyph entry must start at byte 0 of EBDT (i.e. before the
    // header). EBDT::parse expects the 4-byte version header first;
    // adjust the EBLC builder to use image_data_offset = 4.
    let _ = entry;

    let entry = ebdt_format1_entry(16, 16, &mask);
    // Re-build the EBLC with image_data_offset = 4 (skipping the
    // EBDT version header).
    let mut blob = Vec::new();
    blob.extend_from_slice(&2u16.to_be_bytes());
    blob.extend_from_slice(&0u16.to_be_bytes());
    blob.extend_from_slice(&1u32.to_be_bytes());
    let bitmap_size_off = blob.len() as u32;
    let index_array_off = bitmap_size_off + 48;
    blob.extend_from_slice(&index_array_off.to_be_bytes());
    blob.extend_from_slice(&24u32.to_be_bytes());
    blob.extend_from_slice(&1u32.to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes());
    blob.extend_from_slice(&[0u8; 12]);
    blob.extend_from_slice(&[0u8; 12]);
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.push(16);
    blob.push(16);
    blob.push(1);
    blob.push(0x01);
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.extend_from_slice(&8u32.to_be_bytes());
    blob.extend_from_slice(&1u16.to_be_bytes()); // index format
    blob.extend_from_slice(&1u16.to_be_bytes()); // image format
    blob.extend_from_slice(&4u32.to_be_bytes()); // image_data_offset = 4 (past EBDT header)
    blob.extend_from_slice(&0u32.to_be_bytes()); // entry off 0
    blob.extend_from_slice(&(entry.len() as u32).to_be_bytes()); // sentinel
    let eblc = blob;

    let ebdt = build_ebdt(&entry);
    let font = build_sfnt(vec![
        (*b"maxp", maxp_05(2)),
        (*b"EBLC", eblc),
        (*b"EBDT", ebdt),
    ]);

    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .expect("EBDT mono glyph renders");
    assert_eq!(pix.width, 16);
    assert_eq!(pix.height, 16);
    // Top-left corner is unset (bit 7 of 0b0011_1100).
    assert_eq!(pix.get(0, 0), [0, 0, 0, 0]);
    // x=2 of row 0: bit 5 of 0b0011_1100 is set.
    assert_eq!(pix.get(2, 0), [0, 0, 0, 255]);
    // x=14 of row 15: bit 1 of 0b0000_0011 is set.
    assert_eq!(pix.get(14, 15), [0, 0, 0, 255]);
}

#[test]
fn ebdt_synthetic_falls_back_when_only_ebdt_present() {
    // Confirm EBDT triggers when the font carries no CBDT/sbix.
    // (Mirror the previous test's plumbing but assert the variant
    // routing from the unified accessor.)
    let mask = [0xffu8; 2]; // 8x2 all-set
    let entry = ebdt_format1_entry(8, 2, &mask);
    let eblc = build_eblc_one_glyph(16, entry.len() as u32);
    // Patch the image_data_offset from 0 → 4 (past EBDT version header).
    // Locate the IndexSubTable header inside the eblc blob: header(8) +
    // bitmap_size(48) + array_entry(8) = 64; image_data_offset is at
    // bytes 68..72.
    let mut eblc = eblc;
    eblc[68..72].copy_from_slice(&4u32.to_be_bytes());
    let ebdt = build_ebdt(&entry);
    let font = build_sfnt(vec![
        (*b"maxp", maxp_05(2)),
        (*b"EBLC", eblc),
        (*b"EBDT", ebdt),
    ]);
    let face = Face::parse_bytes(&font, 0).unwrap();
    // glyph_bitmap should expose this as the EBDT variant.
    let entry = face.glyph_bitmap(1, 16).unwrap().expect("EBDT entry");
    match entry {
        sigilbuzz::GlyphBitmapEntry::Ebdt { ppem_y, bitmap, .. } => {
            assert_eq!(ppem_y, 16);
            assert_eq!(bitmap.image_format, 1);
            assert_eq!(bitmap.metrics.width(), 8);
        }
        _ => panic!("expected Ebdt variant"),
    }
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
