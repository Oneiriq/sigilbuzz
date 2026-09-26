//! Minimal PNG decoder for CBDT and sbix `'png '` payloads.

use alloc::vec;
use alloc::vec::Vec;

use crate::error::RenderError;
use crate::pixmap::ColorPixmap;

// ---------------------------------------------------------------------------
// PNG decoder: minimal IHDR / IDAT / IEND walk, miniz_oxide for the
// zlib step, hand-rolled per-row defilter. No interlacing.
// ---------------------------------------------------------------------------

pub(super) const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

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
                // Ancillary chunk: skip silently. (PNG spec says
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

pub(super) fn paeth(a: i32, b: i32, c: i32) -> i32 {
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
            // low byte (index 1, big-endian) is what counts. A tRNS
            // chunk whose length is not exactly 2 bytes is malformed
            // (PNG spec 11.3.2.1): silently ignore it rather than fall
            // back to `t.last()`, which would mis-mark every black
            // pixel transparent on an empty tRNS and produce arbitrary
            // results on a wrong-length one. Issue #231.
            let trns_v = trns.and_then(|t| if t.len() == 2 { Some(t[1]) } else { None });
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
