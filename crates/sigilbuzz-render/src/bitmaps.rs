//! Embedded bitmap rasterization.
//!
//! Decodes the pre-rasterized PNG payloads carried by `CBDT`/`CBLC`
//! (Google color emoji) and `sbix` (Apple color emoji) and returns
//! the result as a [`ColorPixmap`]. Pulls strikes through the
//! `Face::glyph_bitmap` unified accessor — strike selection happens
//! inside the parser, this module is responsible for the pixel-side
//! work: PNG decode and (when the requested size doesn't match the
//! strike) bilinear rescale.
//!
//! ```text
//!   Face::glyph_bitmap(gid, ppem)
//!         │
//!         ▼
//!   GlyphBitmapEntry::{Cbdt | Sbix}
//!         │
//!         ▼  (pick a PNG payload; reject jpg/tiff/dupe for now)
//!   decode_png(bytes)
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
//! - **PNG only.** sbix `'jpg '` / `'tiff'` / `'jp2 '` / `'dupe'` and
//!   the legacy 1-bit/8-bit EBDT mask formats return
//!   [`RenderError::UnsupportedBitmap`]. The bitmap-emoji ecosystem
//!   in 2026 is overwhelmingly PNG-only, so the JPEG path is deferred
//!   and EBDT mono is deferred.
//! - **No interlacing.** Adam7 isn't used in font embeds.
//! - **No ancillary chunks beyond IHDR / IDAT / IEND.** The PNG
//!   decoder skips unknown chunks (per PNG spec) but doesn't apply
//!   gAMA / sRGB / iCCP — the colours emerge in whatever space the
//!   embed already lives in, which is conventionally sRGB.

use alloc::vec;
use alloc::vec::Vec;

use sigilbuzz::tables::sbix::TAG_PNG;
use sigilbuzz::{Face, GlyphBitmapEntry};

use crate::error::RenderError;
use crate::pixmap::ColorPixmap;
use crate::rasterizer::Rasterizer;

/// Entry point: rasterizes the embedded bitmap glyph for `gid` at
/// the requested pixel size.
///
/// `size_pt` is the rendering size in pixels (the same convention
/// [`Rasterizer::rasterize_glyph`] uses). The closest strike whose
/// `ppem >= size_pt` is preferred; ties and the no-strike-large-
/// enough case fall back to the closest available strike. When the
/// strike's ppem differs from `size_pt`, the decoded PNG is bilinearly
/// resampled so the output matches the caller's size.
///
/// `coords` is unused for bitmap embeds (CBDT/sbix don't carry
/// variable-axis variants) but kept in the signature for parity with
/// the outline path; future EBDT/COLRv1 variants may consume it.
///
/// Returns:
/// - `Ok(pixmap)` — decoded and (optionally) rescaled bitmap.
/// - `Err(RenderError::NoBitmap(gid))` — no strike covers `gid`, or
///   the font carries no bitmap tables.
/// - `Err(RenderError::UnsupportedBitmap)` — sbix payload is jpg /
///   tiff / dupe etc., or CBDT format is one of the legacy mask
///   variants.
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

    let (png_bytes, strike_ppem) = match entry {
        GlyphBitmapEntry::Cbdt {
            ppem_y, bitmap, ..
        } => match bitmap.image_format {
            17..=19 => (bitmap.data, f32::from(ppem_y)),
            _ => return Err(RenderError::UnsupportedBitmap),
        },
        GlyphBitmapEntry::Sbix { ppem, glyph, .. } => {
            if glyph.graphic_type != TAG_PNG {
                return Err(RenderError::UnsupportedBitmap);
            }
            (glyph.data, f32::from(ppem))
        }
    };

    let decoded = decode_png(png_bytes)?;
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
        let data_end = cursor.checked_add(len).ok_or(RenderError::BadPng(
            "chunk length overflow",
        ))?;
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
                dst[i] = (src[i] as i32)
                    .wrapping_add(paeth(left, up, upleft))
                    as u8;
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
                let a = trns
                    .and_then(|t| t.get(i).copied())
                    .unwrap_or(255);
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
        src.data = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255];
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
