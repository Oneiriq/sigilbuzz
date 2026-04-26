//! Embedded bitmap rasterization.
//!
//! Decodes the pre-rasterized payloads carried by `CBDT`/`CBLC` (Google
//! color emoji), `sbix` (Apple color emoji), and `EBDT`/`EBLC`
//! (Microsoft monochrome bitmap embeds) and returns the result as a
//! [`ColorPixmap`]. Pulls strikes through the `Face::glyph_bitmap`
//! unified accessor — strike selection happens inside the parser,
//! this module is responsible for the pixel-side work: PNG decode,
//! 1bpp-mask unpack, sbix dupe-tag recursion, and (when the requested
//! size doesn't match the strike) bilinear rescale.
//!
//! ```text
//!   Face::glyph_bitmap(gid, ppem)
//!         │
//!         ▼
//!   GlyphBitmapEntry::{Cbdt | Sbix | Ebdt}
//!         │
//!         ├── Cbdt → decode_png
//!         ├── Sbix png  → decode_png
//!         ├── Sbix dupe → recurse to referenced gid
//!         ├── Sbix jpg/tiff/jp2 → UnsupportedBitmap
//!         └── Ebdt → unpack 1bpp mask → black-on-transparent RGBA
//!         │
//!         ▼
//!   ColorPixmap (premul RGBA)
//!         │
//!         ▼  rescale_bilinear if size_pt mismatches strike ppem
//!   ColorPixmap (rendered to caller's size)
//! ```
//!
//! # Scope
//!
//! - **PNG and 1bpp mono.** sbix `'jpg '` / `'tiff'` / `'jp2 '` return
//!   [`RenderError::UnsupportedBitmap`]; implementing those decoders
//!   from scratch is each its own project and they're rare in font
//!   embeds. The bitmap-emoji ecosystem in 2026 is overwhelmingly
//!   PNG-only.
//! - **sbix `'dupe'`** is supported via a depth-limited recursion to
//!   the referenced gid's bitmap.
//! - **EBDT formats 1, 2, 5, 6, 7** (1bpp byte-aligned and bit-aligned
//!   masks) are decoded into black-on-transparent premultiplied RGBA.
//!   Composite formats 8/9 surface `UnsupportedBitmap`.
//! - **No interlacing.** Adam7 isn't used in font embeds.
//! - **No ancillary chunks beyond IHDR / IDAT / IEND.** The PNG
//!   decoder skips unknown chunks (per PNG spec) but doesn't apply
//!   gAMA / sRGB / iCCP — the colours emerge in whatever space the
//!   embed already lives in, which is conventionally sRGB.

use alloc::vec;
use alloc::vec::Vec;

use sigilbuzz::tables::ebdt::{BitPacking, EbdtBitmap};
use sigilbuzz::tables::sbix::{TAG_DUPE, TAG_JP2, TAG_JPG, TAG_PNG, TAG_TIFF};
use sigilbuzz::{Face, GlyphBitmapEntry};

use crate::error::RenderError;
use crate::pixmap::ColorPixmap;
use crate::rasterizer::Rasterizer;

/// Maximum sbix `'dupe'` recursion depth before sigilbuzz-render bails
/// with [`RenderError::UnsupportedBitmap`]. Real fonts dupe at most
/// once (e.g. emoji presentation variants pointing at a base glyph);
/// a deeper chain is malformed or maliciously cyclic.
const SBIX_DUPE_MAX_DEPTH: u8 = 4;

/// Entry point: rasterizes the embedded bitmap glyph for `gid` at
/// the requested pixel size.
///
/// `size_pt` is the rendering size in pixels (the same convention
/// [`Rasterizer::rasterize_glyph`] uses). The closest strike whose
/// `ppem >= size_pt` is preferred; ties and the no-strike-large-
/// enough case fall back to the closest available strike. When the
/// strike's ppem differs from `size_pt`, the decoded payload is
/// bilinearly resampled so the output matches the caller's size.
///
/// `coords` is unused for bitmap embeds (CBDT/sbix/EBDT don't carry
/// variable-axis variants) but kept in the signature for parity with
/// the outline path; future variants may consume it.
///
/// Dispatch priority is **CBDT (color) > sbix png > EBDT (mono)** —
/// see [`Face::glyph_bitmap`](sigilbuzz::Face::glyph_bitmap). Within
/// the sbix variant, `'png '` decodes inline, `'dupe'` recurses (with
/// a depth cap) into the referenced gid, and `'jpg '` / `'tiff'` /
/// `'jp2 '` surface [`RenderError::UnsupportedBitmap`].
///
/// Returns:
/// - `Ok(pixmap)` — decoded and (optionally) rescaled bitmap.
/// - `Err(RenderError::NoBitmap(gid))` — no strike covers `gid`, or
///   the font carries no bitmap tables.
/// - `Err(RenderError::UnsupportedBitmap)` — sbix payload is jpg /
///   tiff / jp2, EBDT format is one of the composite variants
///   (8 / 9), or `'dupe'` recursion exceeds the depth cap.
/// - `Err(RenderError::BadPng(...))` — PNG payload failed to decode.
///
/// # Errors
/// Surfaces all of the above plus [`RenderError::BadSize`] for a
/// non-finite or non-positive `size_pt`, and [`RenderError::Parse`]
/// for any underlying sigilbuzz parser failure pulling tables.
pub fn rasterize_bitmap_glyph(
    _rasterizer: &Rasterizer,
    face: &Face<'_>,
    gid: u16,
    size_pt: f32,
    _coords: &[f32],
) -> Result<ColorPixmap, RenderError> {
    rasterize_bitmap_inner(face, gid, size_pt, 0)
}

fn rasterize_bitmap_inner(
    face: &Face<'_>,
    gid: u16,
    size_pt: f32,
    dupe_depth: u8,
) -> Result<ColorPixmap, RenderError> {
    if !size_pt.is_finite() || size_pt <= 0.0 {
        return Err(RenderError::BadSize(size_pt));
    }
    // sigilbuzz's glyph_bitmap takes a u16 ppem; round up so we err
    // toward the larger strike when the request lands between two
    // integer ppems. `clamp` keeps a conservative ceiling so a very
    // large request still maps to a valid u16.
    let req_ppem = size_pt.ceil().clamp(1.0, f32::from(u16::MAX)) as u16;
    let entry = face
        .glyph_bitmap(gid, req_ppem)
        .map_err(|_| RenderError::Parse("glyph_bitmap"))?
        .ok_or(RenderError::NoBitmap(gid))?;

    let (decoded, strike_ppem) = match entry {
        GlyphBitmapEntry::Cbdt { ppem_y, bitmap, .. } => match bitmap.image_format {
            17..=19 => (decode_png(bitmap.data)?, f32::from(ppem_y)),
            _ => return Err(RenderError::UnsupportedBitmap),
        },
        GlyphBitmapEntry::Sbix { ppem, glyph, .. } => match glyph.graphic_type {
            TAG_PNG => (decode_png(glyph.data)?, f32::from(ppem)),
            TAG_DUPE => {
                if dupe_depth >= SBIX_DUPE_MAX_DEPTH {
                    return Err(RenderError::UnsupportedBitmap);
                }
                // 'dupe' payload is a 2-byte big-endian glyph id of
                // the bitmap to use instead. Recurse with the same
                // size_pt so strike picking happens against the
                // referenced gid's coverage. A self-reference will
                // hit the depth cap rather than loop forever.
                if glyph.data.len() < 2 {
                    return Err(RenderError::UnsupportedBitmap);
                }
                let alias = u16::from_be_bytes([glyph.data[0], glyph.data[1]]);
                if alias == gid {
                    return Err(RenderError::UnsupportedBitmap);
                }
                return rasterize_bitmap_inner(face, alias, size_pt, dupe_depth + 1);
            }
            // JPEG / TIFF / JPEG-2000: implementing those decoders
            // from scratch is each its own project and they're rare
            // in font embeds. Surface cleanly so callers can fall
            // back to outlines.
            TAG_JPG | TAG_TIFF | TAG_JP2 => return Err(RenderError::UnsupportedBitmap),
            // Unknown four-byte tag — treat as unsupported rather
            // than guessing.
            _ => return Err(RenderError::UnsupportedBitmap),
        },
        GlyphBitmapEntry::Ebdt { ppem_y, bitmap, .. } => {
            (decode_ebdt_mono(&bitmap)?, f32::from(ppem_y))
        }
    };

    if !needs_rescale(&decoded, strike_ppem, size_pt) {
        return Ok(decoded);
    }
    let scale = size_pt / strike_ppem;
    let dst_w = ((decoded.width as f32 * scale).round() as u32).max(1);
    let dst_h = ((decoded.height as f32 * scale).round() as u32).max(1);
    Ok(rescale_bilinear(&decoded, dst_w, dst_h))
}

/// True iff the source pixmap needs to be resampled to match the
/// caller's size_pt. We compare the strike ppem to the requested
/// size with a small tolerance so an exact match short-circuits the
/// resample.
fn needs_rescale(decoded: &ColorPixmap, strike_ppem: f32, size_pt: f32) -> bool {
    if decoded.is_empty() {
        return false;
    }
    if strike_ppem <= 0.0 {
        return false;
    }
    (strike_ppem - size_pt).abs() > 0.5
}

// ---------------------------------------------------------------------------
// PNG decoder — minimal IHDR / IDAT / IEND walk, miniz_oxide for the
// zlib step, hand-rolled per-row defilter. No interlacing.
// ---------------------------------------------------------------------------

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// Decode a PNG byte slice into a premultiplied RGBA [`ColorPixmap`].
///
/// Supports the embed-typical shapes: 8-bit grayscale, 8-bit
/// grayscale+alpha, 8-bit RGB, 8-bit RGBA, 8-bit indexed (PLTE +
/// optional tRNS). 16-bit depth, paeth-only deep-color, and Adam7
/// interlacing return [`RenderError::BadPng`].
///
/// # Errors
/// Returns [`RenderError::BadPng`] on any structural problem.
pub fn decode_png(bytes: &[u8]) -> Result<ColorPixmap, RenderError> {
    if bytes.len() < 8 || bytes[..8] != PNG_SIGNATURE {
        return Err(RenderError::BadPng("missing PNG signature"));
    }
    let mut cursor = 8usize;
    let mut ihdr: Option<Ihdr> = None;
    let mut idat = Vec::<u8>::new();
    let mut plte: Option<Vec<[u8; 3]>> = None;
    let mut trns: Option<Vec<u8>> = None;
    loop {
        if cursor + 8 > bytes.len() {
            return Err(RenderError::BadPng("truncated chunk header"));
        }
        let len = read_u32(&bytes[cursor..cursor + 4]) as usize;
        let kind = [
            bytes[cursor + 4],
            bytes[cursor + 5],
            bytes[cursor + 6],
            bytes[cursor + 7],
        ];
        cursor += 8;
        let data_end = cursor
            .checked_add(len)
            .ok_or(RenderError::BadPng("chunk length overflow"))?;
        if data_end + 4 > bytes.len() {
            return Err(RenderError::BadPng("truncated chunk body"));
        }
        let data = &bytes[cursor..data_end];
        // Skip CRC; PNG embeds in fonts have already been validated
        // by the font producer and we don't gain anything by failing
        // a render on a CRC mismatch.
        cursor = data_end + 4;

        match &kind {
            b"IHDR" => {
                if ihdr.is_some() {
                    return Err(RenderError::BadPng("duplicate IHDR"));
                }
                ihdr = Some(Ihdr::parse(data)?);
            }
            b"IDAT" => {
                if ihdr.is_none() {
                    return Err(RenderError::BadPng("IDAT before IHDR"));
                }
                idat.extend_from_slice(data);
            }
            b"PLTE" => {
                if data.len() % 3 != 0 {
                    return Err(RenderError::BadPng("PLTE length not multiple of 3"));
                }
                let mut entries = Vec::with_capacity(data.len() / 3);
                for c in data.chunks_exact(3) {
                    entries.push([c[0], c[1], c[2]]);
                }
                plte = Some(entries);
            }
            b"tRNS" => {
                trns = Some(data.to_vec());
            }
            b"IEND" => break,
            _ => {
                // Ancillary chunk — skip silently. (PNG spec says
                // unknown chunks with the lower-case first letter are
                // safe to ignore; for embeds we ignore them all.)
            }
        }
    }
    let ihdr = ihdr.ok_or(RenderError::BadPng("missing IHDR"))?;
    if idat.is_empty() {
        return Err(RenderError::BadPng("missing IDAT"));
    }
    decode_image(&ihdr, &idat, plte.as_deref(), trns.as_deref())
}

#[derive(Debug, Clone, Copy)]
struct Ihdr {
    width: u32,
    height: u32,
    color_type: u8,
}

impl Ihdr {
    fn parse(data: &[u8]) -> Result<Self, RenderError> {
        if data.len() != 13 {
            return Err(RenderError::BadPng("IHDR length not 13"));
        }
        let width = read_u32(&data[0..4]);
        let height = read_u32(&data[4..8]);
        let bit_depth = data[8];
        let color_type = data[9];
        let compression = data[10];
        let filter = data[11];
        let interlace = data[12];
        if compression != 0 || filter != 0 {
            return Err(RenderError::BadPng("unsupported compression/filter"));
        }
        if interlace != 0 {
            return Err(RenderError::BadPng("interlaced PNG not supported"));
        }
        if bit_depth != 8 {
            // Indexed PNGs are sometimes 1/2/4-bit; for emoji embeds
            // 8-bit is the universal shape. Reject the rest cleanly.
            return Err(RenderError::BadPng("only 8-bit PNGs supported"));
        }
        // Validate color type early.
        match color_type {
            0 | 2 | 3 | 4 | 6 => {}
            _ => return Err(RenderError::BadPng("unknown color type")),
        }
        if width == 0 || height == 0 {
            return Err(RenderError::BadPng("zero dimension"));
        }
        // Bound check to prevent allocator abuse on a malformed embed.
        if width > 16_384 || height > 16_384 {
            return Err(RenderError::BadPng("dimensions exceed 16384"));
        }
        // bit_depth is validated above; we don't store it because we
        // only accept 8-bit and never re-check downstream.
        let _ = bit_depth;
        Ok(Self {
            width,
            height,
            color_type,
        })
    }

    fn bytes_per_pixel(&self) -> usize {
        match self.color_type {
            0 => 1, // gray
            2 => 3, // rgb
            3 => 1, // indexed
            4 => 2, // gray + alpha
            6 => 4, // rgba
            _ => 1,
        }
    }
}

fn decode_image(
    ihdr: &Ihdr,
    idat: &[u8],
    plte: Option<&[[u8; 3]]>,
    trns: Option<&[u8]>,
) -> Result<ColorPixmap, RenderError> {
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib(idat)
        .map_err(|_| RenderError::BadPng("zlib inflate failed"))?;
    let bpp = ihdr.bytes_per_pixel();
    let row_bytes = (ihdr.width as usize)
        .checked_mul(bpp)
        .ok_or(RenderError::BadPng("row size overflow"))?;
    // Filter byte + row payload, height rows.
    let expected = (row_bytes + 1)
        .checked_mul(ihdr.height as usize)
        .ok_or(RenderError::BadPng("decompressed size overflow"))?;
    if raw.len() != expected {
        return Err(RenderError::BadPng("decompressed length mismatch"));
    }

    // De-filter row by row. PNG filters are stateful: each row's
    // reconstruction uses the previous row.
    let mut prev_row = vec![0u8; row_bytes];
    let mut cur_row = vec![0u8; row_bytes];
    let mut pixels = Vec::with_capacity((ihdr.width * ihdr.height) as usize * 4);

    let mut cursor = 0usize;
    for _y in 0..ihdr.height {
        let filter = raw[cursor];
        cursor += 1;
        let row = &raw[cursor..cursor + row_bytes];
        cursor += row_bytes;
        defilter_row(filter, row, &prev_row, &mut cur_row, bpp)?;
        // Transcode this row into RGBA.
        emit_row(ihdr, plte, trns, &cur_row, &mut pixels)?;
        // Roll prev = cur for the next iteration.
        prev_row.copy_from_slice(&cur_row);
    }

    let mut out = ColorPixmap::new(ihdr.width, ihdr.height);
    out.data = pixels;
    Ok(out)
}

fn defilter_row(
    filter: u8,
    src: &[u8],
    prev: &[u8],
    dst: &mut [u8],
    bpp: usize,
) -> Result<(), RenderError> {
    // PNG filter types:
    //   0 None, 1 Sub, 2 Up, 3 Average, 4 Paeth.
    match filter {
        0 => dst.copy_from_slice(src),
        1 => {
            for i in 0..dst.len() {
                let left = if i >= bpp { dst[i - bpp] as u16 } else { 0 };
                dst[i] = (src[i] as u16).wrapping_add(left) as u8;
            }
        }
        2 => {
            for i in 0..dst.len() {
                dst[i] = (src[i] as u16).wrapping_add(prev[i] as u16) as u8;
            }
        }
        3 => {
            for i in 0..dst.len() {
                let left = if i >= bpp { dst[i - bpp] as u16 } else { 0 };
                let up = prev[i] as u16;
                let avg = (left + up) / 2;
                dst[i] = (src[i] as u16).wrapping_add(avg) as u8;
            }
        }
        4 => {
            for i in 0..dst.len() {
                let left = if i >= bpp { dst[i - bpp] as i32 } else { 0 };
                let up = prev[i] as i32;
                let upleft = if i >= bpp { prev[i - bpp] as i32 } else { 0 };
                dst[i] = (src[i] as i32).wrapping_add(paeth(left, up, upleft)) as u8;
            }
        }
        _ => return Err(RenderError::BadPng("unknown filter type")),
    }
    Ok(())
}

fn paeth(a: i32, b: i32, c: i32) -> i32 {
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn emit_row(
    ihdr: &Ihdr,
    plte: Option<&[[u8; 3]]>,
    trns: Option<&[u8]>,
    row: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), RenderError> {
    match ihdr.color_type {
        0 => {
            // Grayscale; tRNS (when present) is a single 2-byte
            // gray value treated as transparent. For 8-bit depth the
            // low byte is what counts.
            let trns_v = trns.map(|t| t.last().copied().unwrap_or(0));
            for &g in row {
                let a = if Some(g) == trns_v { 0 } else { 255 };
                push_premul(out, g, g, g, a);
            }
        }
        2 => {
            // RGB; tRNS is a 6-byte triple at color depth 8 (only the
            // bottom byte of each 16-bit field matters).
            let trns_rgb = trns.and_then(|t| {
                if t.len() == 6 {
                    Some([t[1], t[3], t[5]])
                } else {
                    None
                }
            });
            for chunk in row.chunks_exact(3) {
                let (r, g, b) = (chunk[0], chunk[1], chunk[2]);
                let a = match trns_rgb {
                    Some(k) if [r, g, b] == k => 0,
                    _ => 255,
                };
                push_premul(out, r, g, b, a);
            }
        }
        3 => {
            let palette = plte.ok_or(RenderError::BadPng("indexed PNG missing PLTE"))?;
            for &idx in row {
                let i = idx as usize;
                if i >= palette.len() {
                    return Err(RenderError::BadPng("palette index out of range"));
                }
                let [r, g, b] = palette[i];
                let a = trns.and_then(|t| t.get(i).copied()).unwrap_or(255);
                push_premul(out, r, g, b, a);
            }
        }
        4 => {
            for chunk in row.chunks_exact(2) {
                let g = chunk[0];
                let a = chunk[1];
                push_premul(out, g, g, g, a);
            }
        }
        6 => {
            for chunk in row.chunks_exact(4) {
                push_premul(out, chunk[0], chunk[1], chunk[2], chunk[3]);
            }
        }
        _ => return Err(RenderError::BadPng("unknown color type at decode")),
    }
    Ok(())
}

fn push_premul(out: &mut Vec<u8>, r: u8, g: u8, b: u8, a: u8) {
    if a == 255 {
        out.push(r);
        out.push(g);
        out.push(b);
        out.push(a);
    } else if a == 0 {
        out.push(0);
        out.push(0);
        out.push(0);
        out.push(0);
    } else {
        let aa = a as u32;
        out.push(((r as u32 * aa + 127) / 255) as u8);
        out.push(((g as u32 * aa + 127) / 255) as u8);
        out.push(((b as u32 * aa + 127) / 255) as u8);
        out.push(a);
    }
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

// ---------------------------------------------------------------------------
// EBDT mono → RGBA.
//
// EBDT format 1 / 6 is byte-aligned: each scanline starts on a fresh
// byte, so the row stride is `ceil(width / 8)` bytes. Format 2 / 5 / 7
// is bit-aligned: the bitstream packs across rows with no padding.
// In both cases the most-significant bit of each byte is the leftmost
// pixel.
//
// The mono → RGBA conversion is "set bits = opaque black, unset bits
// = fully transparent". This matches the historical reading of EBDT
// (the alpha mask is the glyph) and lets the rest of the pipeline
// composite the result like any other premultiplied RGBA pixmap.
// ---------------------------------------------------------------------------

/// Unpacks an EBDT 1bpp mask into a premultiplied RGBA [`ColorPixmap`].
/// Set bits become opaque black `(0, 0, 0, 255)`; unset bits become
/// fully transparent `(0, 0, 0, 0)`.
///
/// Accepts the byte-aligned (formats 1 / 6) and bit-aligned (formats
/// 2 / 5 / 7) variants — see [`BitPacking`]. Composite formats 8 / 9
/// never reach this entry point because the underlying parser surfaces
/// them as `Unsupported`.
///
/// # Errors
/// Returns [`RenderError::UnsupportedBitmap`] when the mask payload
/// is too short for the declared `width × height` (a malformed embed).
pub fn decode_ebdt_mono(bitmap: &EbdtBitmap<'_>) -> Result<ColorPixmap, RenderError> {
    let w = bitmap.metrics.width() as u32;
    let h = bitmap.metrics.height() as u32;
    if w == 0 || h == 0 {
        return Ok(ColorPixmap::new(0, 0));
    }
    let mut out = ColorPixmap::new(w, h);
    match bitmap.packing {
        BitPacking::ByteAligned => unpack_byte_aligned(bitmap.data, w, h, &mut out)?,
        BitPacking::BitAligned => unpack_bit_aligned(bitmap.data, w, h, &mut out)?,
    }
    Ok(out)
}

fn unpack_byte_aligned(
    src: &[u8],
    width: u32,
    height: u32,
    out: &mut ColorPixmap,
) -> Result<(), RenderError> {
    let row_bytes = (width as usize).div_ceil(8);
    let need = row_bytes
        .checked_mul(height as usize)
        .ok_or(RenderError::UnsupportedBitmap)?;
    if src.len() < need {
        return Err(RenderError::UnsupportedBitmap);
    }
    for y in 0..height {
        let row_off = (y as usize) * row_bytes;
        for x in 0..width {
            let byte = src[row_off + (x as usize / 8)];
            // Bit 7 = leftmost pixel.
            let bit = (byte >> (7 - (x % 8))) & 1;
            write_mono_pixel(out, x, y, bit != 0);
        }
    }
    Ok(())
}

fn unpack_bit_aligned(
    src: &[u8],
    width: u32,
    height: u32,
    out: &mut ColorPixmap,
) -> Result<(), RenderError> {
    let total_bits = (width as usize)
        .checked_mul(height as usize)
        .ok_or(RenderError::UnsupportedBitmap)?;
    let need = total_bits.div_ceil(8);
    if src.len() < need {
        return Err(RenderError::UnsupportedBitmap);
    }
    let mut bit_idx = 0usize;
    for y in 0..height {
        for x in 0..width {
            let byte = src[bit_idx / 8];
            let shift = 7 - (bit_idx % 8);
            let bit = (byte >> shift) & 1;
            write_mono_pixel(out, x, y, bit != 0);
            bit_idx += 1;
        }
    }
    Ok(())
}

fn write_mono_pixel(out: &mut ColorPixmap, x: u32, y: u32, set: bool) {
    let idx = (y as usize * out.width as usize + x as usize) * 4;
    if set {
        out.data[idx] = 0;
        out.data[idx + 1] = 0;
        out.data[idx + 2] = 0;
        out.data[idx + 3] = 255;
    } else {
        // ColorPixmap::new zeroes already, but be explicit so a future
        // caller passing in a recycled pixmap still sees clean output.
        out.data[idx] = 0;
        out.data[idx + 1] = 0;
        out.data[idx + 2] = 0;
        out.data[idx + 3] = 0;
    }
}

// ---------------------------------------------------------------------------
// Bilinear rescale.
// ---------------------------------------------------------------------------

/// Bilinearly resample `src` to a new size. Operates on premultiplied
/// RGBA so the alpha channel stays consistent with the rest of the
/// render pipeline; sampling premul is the right thing here because
/// edges that are partly transparent already have their colours
/// scaled by alpha.
///
/// Degenerate cases:
/// - `src` empty or `dst_w == 0 || dst_h == 0` → empty pixmap.
/// - `dst_w == src.width && dst_h == src.height` → clone of `src`.
#[must_use]
pub fn rescale_bilinear(src: &ColorPixmap, dst_w: u32, dst_h: u32) -> ColorPixmap {
    if src.is_empty() || dst_w == 0 || dst_h == 0 {
        return ColorPixmap::new(0, 0);
    }
    if dst_w == src.width && dst_h == src.height {
        return src.clone();
    }
    let mut out = ColorPixmap::new(dst_w, dst_h);
    let sw = src.width as f32;
    let sh = src.height as f32;
    let dw = dst_w as f32;
    let dh = dst_h as f32;
    // Map dst pixel centers to src space. The half-pixel offset keeps
    // the rescale edge-aligned: a 2× upscale of a 2-px image lands the
    // first dst pixel at src x = 0.25 etc.
    for y in 0..dst_h {
        let sy = ((y as f32 + 0.5) * sh / dh) - 0.5;
        let y0 = sy.floor().max(0.0) as u32;
        let y1 = (y0 + 1).min(src.height - 1);
        let fy = (sy - y0 as f32).clamp(0.0, 1.0);
        for x in 0..dst_w {
            let sx = ((x as f32 + 0.5) * sw / dw) - 0.5;
            let x0 = sx.floor().max(0.0) as u32;
            let x1 = (x0 + 1).min(src.width - 1);
            let fx = (sx - x0 as f32).clamp(0.0, 1.0);
            let p00 = src.get(x0, y0);
            let p10 = src.get(x1, y0);
            let p01 = src.get(x0, y1);
            let p11 = src.get(x1, y1);
            let mut rgba = [0u8; 4];
            for c in 0..4 {
                let top = lerp(p00[c] as f32, p10[c] as f32, fx);
                let bot = lerp(p01[c] as f32, p11[c] as f32, fx);
                let v = lerp(top, bot, fy);
                rgba[c] = v.clamp(0.0, 255.0).round() as u8;
            }
            let idx = (y as usize * dst_w as usize + x as usize) * 4;
            out.data[idx..idx + 4].copy_from_slice(&rgba);
        }
    }
    out
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an in-memory PNG with the given solid colour. RGBA,
    /// 8-bit, no interlacing — exercises the `IHDR/IDAT/IEND` walk
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

    // -------- EBDT mono → RGBA --------
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
        }
    }

    #[test]
    fn ebdt_byte_aligned_decodes_mono_to_rgba() {
        // 8 wide, 2 tall, byte-aligned: 2 bytes total.
        // Row 0: 0b10101010 → set,unset,set,unset,...
        // Row 1: 0b11110000 → 4 set then 4 unset.
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
        // Row 0: 0b11111000 → all 5 pixels set.
        // Row 1: 0b00000000 → all 5 pixels unset.
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
    fn paeth_predictor_matches_spec_examples() {
        // PNG spec: p = a + b - c; predictor = whichever of {a, b, c}
        // is closest to p (ties → a, then b).
        // a=10 b=20 c=30 → p=0; pa=10 pb=20 pc=30 → returns a=10.
        assert_eq!(paeth(10, 20, 30), 10);
        // a=b=c → p == a, all distances zero → a wins.
        assert_eq!(paeth(50, 50, 50), 50);
        // a=0 b=0 c=255 → p = -255; pa=255 pb=255 pc=510 → tie pa==pb,
        // ties prefer a.
        assert_eq!(paeth(0, 0, 255), 0);
    }
}
