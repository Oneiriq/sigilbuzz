//! EBDT monochrome bitmap decode, including composite (formats 8 / 9)
//! expansion.

use alloc::vec::Vec;

use sigilbuzz::tables::ebdt::{BitPacking, EbdtBitmap};
use sigilbuzz::{Face, GlyphBitmapEntry};

use super::{rasterize_bitmap_inner_full, EBDT_COMPOSITE_MAX_DEPTH};
use crate::error::RenderError;
use crate::pixmap::ColorPixmap;

// ---------------------------------------------------------------------------
// EBDT mono -> RGBA.
//
// EBDT format 1 / 6 is byte-aligned: each scanline starts on a fresh
// byte, so the row stride is `ceil(width / 8)` bytes. Format 2 / 5 / 7
// is bit-aligned: the bitstream packs across rows with no padding.
// In both cases the most-significant bit of each byte is the leftmost
// pixel.
//
// The mono -> RGBA conversion is "set bits = opaque black, unset bits
// = fully transparent". This matches the historical reading of EBDT
// (the alpha mask is the glyph) and lets the rest of the pipeline
// composite the result like any other premultiplied RGBA pixmap.
// ---------------------------------------------------------------------------

/// Unpacks an EBDT 1bpp mask into a premultiplied RGBA [`ColorPixmap`].
/// Set bits become opaque black `(0, 0, 0, 255)`; unset bits become
/// fully transparent `(0, 0, 0, 0)`.
///
/// Accepts the byte-aligned (formats 1 / 6) and bit-aligned (formats
/// 2 / 5 / 7) variants. See [`BitPacking`]. Composite formats 8 / 9
/// never reach this entry point because the underlying parser surfaces
/// them as `Unsupported`.
///
/// # Errors
/// Returns [`RenderError::UnsupportedBitmap`] when the mask payload
/// is too short for the declared `width * height` (a malformed embed).
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

/// Renders an EBDT composite glyph (formats 8 / 9) by alpha-overlaying
/// each component's mono mask onto a parent canvas sized by the
/// composite's own metrics. Recurses through the EBDT pipeline at the
/// **same strike** the parent was found in: a component whose
/// `glyph_id` has no EBDT entry at the parent's `ppem_y` surfaces
/// [`RenderError::BitmapDecodeFailed`] rather than silently falling
/// back to a different strike.
///
/// The recursion is bounded three ways:
///   1. A hard depth cap ([`EBDT_COMPOSITE_MAX_DEPTH`]).
///   2. A self-reference guard (a component whose `glyph_id` equals
///      the current composite's `gid`).
///   3. A cycle guard against the full ancestor chain (a component
///      whose `glyph_id` matches any gid currently being expanded).
///
/// Compositing uses source-over alpha blending in premultiplied space.
/// EBDT mono masks are 0/255 alpha so the result is conceptually a
/// union of the component masks; the same code path will give a sane
/// result if a future caller ever feeds a partially-opaque pixmap into
/// the same compositor.
pub(super) fn decode_ebdt_composite(
    face: &Face<'_>,
    gid: u16,
    bitmap: &EbdtBitmap<'_>,
    parent_ppem_y: u8,
    composite_chain: &mut Vec<u16>,
    composite_depth: u8,
) -> Result<ColorPixmap, RenderError> {
    if composite_depth >= EBDT_COMPOSITE_MAX_DEPTH {
        return Err(RenderError::BitmapDecodeFailed("composite depth exceeded"));
    }
    let parent_w = u32::from(bitmap.metrics.width());
    let parent_h = u32::from(bitmap.metrics.height());
    if parent_w == 0 || parent_h == 0 {
        return Ok(ColorPixmap::new(0, 0));
    }
    let canvas_w = parent_w;
    let canvas_h = parent_h;
    let mut canvas = ColorPixmap::new(canvas_w, canvas_h);

    composite_chain.push(gid);
    let result = (|| -> Result<(), RenderError> {
        for comp in bitmap.components() {
            // Self-reference and ancestor-cycle guards, separate from
            // the depth cap so they surface a precise error message
            // even at depth 1.
            if comp.glyph_id == gid {
                return Err(RenderError::BitmapDecodeFailed("composite self-reference"));
            }
            if composite_chain.contains(&comp.glyph_id) {
                return Err(RenderError::BitmapDecodeFailed("composite cycle"));
            }
            // OOB rejection: glyph id beyond what the font enumerates.
            // maxp may fail on malformed fonts; fall through
            // to the strike resolution below in that case rather than
            // letting a parse failure mask a strike-mismatch error.
            if let Ok(maxp) = face.maxp() {
                if comp.glyph_id >= maxp.num_glyphs {
                    return Err(RenderError::BitmapDecodeFailed(
                        "composite component glyph id out of range",
                    ));
                }
            }
            // Strike consistency: the component must resolve at the
            // parent strike's ppem_y. We pass `parent_ppem_y` and then
            // verify the entry that came back is at that
            // ppem_y; if Face::glyph_bitmap fell back to a different
            // strike (or returned None), we surface an error rather
            // than silently composite from the wrong size.
            let parent_ppem_size = f32::from(parent_ppem_y);
            let entry = face
                .glyph_bitmap(comp.glyph_id, u16::from(parent_ppem_y))
                .map_err(|_| RenderError::Parse("glyph_bitmap"))?
                .ok_or(RenderError::BitmapDecodeFailed(
                    "composite component missing at strike",
                ))?;
            match &entry {
                GlyphBitmapEntry::Ebdt {
                    ppem_y: comp_ppem_y,
                    ..
                } => {
                    if *comp_ppem_y != parent_ppem_y {
                        return Err(RenderError::BitmapDecodeFailed(
                            "composite component strike mismatch",
                        ));
                    }
                }
                _ => {
                    // The parent was EBDT, so the component must also
                    // resolve through EBDT. CBDT/sbix here means a
                    // pathological font.
                    return Err(RenderError::BitmapDecodeFailed(
                        "composite component non-EBDT",
                    ));
                }
            }
            let comp_pix = rasterize_bitmap_inner_full(
                face,
                comp.glyph_id,
                parent_ppem_size,
                0,
                composite_chain,
                composite_depth + 1,
            )?;
            blit_source_over(
                &mut canvas,
                &comp_pix,
                i32::from(comp.x_offset),
                i32::from(comp.y_offset),
            );
        }
        Ok(())
    })();
    composite_chain.pop();
    result?;
    Ok(canvas)
}

/// Source-over alpha-blends `src` onto `dst` at integer pixel offset
/// `(dx, dy)`. Both pixmaps must be premultiplied RGBA. Pixels of
/// `src` that fall outside `dst` are clipped silently. Fully
/// transparent source pixels are skipped. This matters for EBDT
/// masks where most of the source is alpha=0.
pub(super) fn blit_source_over(dst: &mut ColorPixmap, src: &ColorPixmap, dx: i32, dy: i32) {
    if src.is_empty() || dst.is_empty() {
        return;
    }
    let dw = dst.width as i32;
    let dh = dst.height as i32;
    let sw = src.width as i32;
    let sh = src.height as i32;
    for sy in 0..sh {
        let ty = sy + dy;
        if ty < 0 || ty >= dh {
            continue;
        }
        for sx in 0..sw {
            let tx = sx + dx;
            if tx < 0 || tx >= dw {
                continue;
            }
            #[allow(clippy::cast_sign_loss)]
            let s_idx = (sy as usize * src.width as usize + sx as usize) * 4;
            let sa = src.data[s_idx + 3];
            if sa == 0 {
                continue;
            }
            #[allow(clippy::cast_sign_loss)]
            let d_idx = (ty as usize * dst.width as usize + tx as usize) * 4;
            // Premultiplied source-over: out = src + dst * (1 - src.a).
            let inv = 255u32 - u32::from(sa);
            for c in 0..4 {
                let s = u32::from(src.data[s_idx + c]);
                let d = u32::from(dst.data[d_idx + c]);
                // (d * inv + 127) / 255 keeps rounding stable; matches
                // push_premul above.
                let blended = s + (d * inv + 127) / 255;
                #[allow(clippy::cast_possible_truncation)]
                let v = blended.min(255) as u8;
                dst.data[d_idx + c] = v;
            }
        }
    }
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
