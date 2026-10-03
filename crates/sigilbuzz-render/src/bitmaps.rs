//! Embedded bitmap rasterization.
//!
//! Decodes the pre-rasterized payloads carried by `CBDT`/`CBLC` (Google
//! color emoji), `sbix` (Apple color emoji), and `EBDT`/`EBLC`
//! (Microsoft monochrome bitmap embeds) and returns the result as a
//! [`ColorPixmap`]. Pulls strikes through the `Face::glyph_bitmap`
//! unified accessor. Strike selection happens inside the parser,
//! this module is responsible for the pixel-side work: PNG decode,
//! 1bpp-mask unpack, sbix dupe-tag recursion, and (when the requested
//! size doesn't match the strike) bilinear rescale.
//!
//! ```text
//!   Face::glyph_bitmap(gid, ppem)
//!         |
//!         v
//!   GlyphBitmapEntry::{Cbdt | Sbix | Ebdt}
//!         |
//!         +-- Cbdt -> decode_png
//!         +-- Sbix png  -> decode_png
//!         +-- Sbix dupe -> recurse to referenced gid
//!         +-- Sbix jpg -> decode_jpeg (8-bit YCbCr/gray)
//!         +-- Sbix tiff -> decode_tiff (baseline 8-bit RGB/RGBA)
//!         +-- Sbix jp2 -> UnsupportedBitmap
//!         +-- Ebdt -> unpack 1bpp mask -> black-on-transparent RGBA
//!         |
//!         v
//!   ColorPixmap (premul RGBA)
//!         |
//!         v  rescale_bilinear if size_pt mismatches strike ppem
//!   ColorPixmap (rendered to caller's size)
//! ```
//!
//! # Scope
//!
//! - **PNG, JPEG, baseline TIFF, and 1bpp mono.** sbix `'jpg '` is
//!   decoded via the in-crate decoder ([`crate::decode_jpeg`]) and
//!   sbix `'tiff'` via [`crate::decode_tiff`]. sbix `'jp2 '` returns
//!   [`RenderError::UnsupportedBitmap`]: a JPEG 2000 decoder is its
//!   own project and the format is rare in font embeds. The
//!   bitmap-emoji ecosystem in 2026 is overwhelmingly PNG-only.
//! - **sbix `'dupe'`** is supported via a depth-limited recursion to
//!   the referenced gid's bitmap.
//! - **EBDT formats 1, 2, 5, 6, 7** (1bpp byte-aligned and bit-aligned
//!   masks) are decoded into black-on-transparent premultiplied RGBA.
//! - **EBDT formats 8 and 9** (composite mono bitmaps) recurse into
//!   the referenced component gids at the parent's strike, alpha-
//!   overlay each on a parent canvas, and surface
//!   [`RenderError::BitmapDecodeFailed`] on cycles, self-references,
//!   missing-at-strike components, recursion past
//!   [`EBDT_COMPOSITE_MAX_DEPTH`], or more than
//!   [`EBDT_COMPOSITE_MAX_COMPONENTS`] component expansions in total.
//! - **No interlacing.** Adam7 isn't used in font embeds.
//! - **No ancillary chunks beyond IHDR / IDAT / IEND.** The PNG
//!   decoder skips unknown chunks (per PNG spec) but doesn't apply
//!   gAMA / sRGB / iCCP: the colors emerge in whatever space the
//!   embed already lives in, which is conventionally sRGB.

use alloc::vec::Vec;

use sigilbuzz::tables::sbix::{SbixGlyph, TAG_DUPE, TAG_JP2, TAG_JPG, TAG_PNG, TAG_TIFF};
use sigilbuzz::tables::{EbdtMetrics, GlyphBitmapMetrics};
use sigilbuzz::{Face, GlyphBitmapEntry};

use crate::error::RenderError;
use crate::jpeg_decode::decode_jpeg;
use crate::pixmap::{ColorPixmap, Placement};
use crate::rasterizer::Rasterizer;
use crate::tiff_decode::decode_tiff;

mod ebdt;
mod png;
mod rescale;

pub use ebdt::decode_ebdt_mono;
pub use png::decode_png;
pub use rescale::rescale_bilinear;

use ebdt::decode_ebdt_composite;

/// Maximum sbix `'dupe'` recursion depth before sigilbuzz-render bails
/// with [`RenderError::UnsupportedBitmap`]. Real fonts dupe at most
/// once (e.g. emoji presentation variants pointing at a base glyph);
/// a deeper chain is malformed or maliciously cyclic.
const SBIX_DUPE_MAX_DEPTH: u8 = 4;

/// Maximum EBDT composite (formats 8 / 9) recursion depth before
/// sigilbuzz-render bails with
/// [`RenderError::BitmapDecodeFailed`]. Real composite mono bitmaps
/// reference plain (non-composite) glyphs one level deep; legitimate
/// nested composites are exceedingly rare and deeper than 4 levels
/// almost certainly indicates a cycle or pathological font. Mirrors
/// the sbix `'dupe'` cap from #221 for the same reason: bound the
/// blast radius of a malicious or malformed font.
const EBDT_COMPOSITE_MAX_DEPTH: u8 = 4;

/// Maximum number of EBDT composite components expanded while
/// rendering one glyph, counted across every recursion level. The depth
/// cap alone still allows a fan-out of `n^4` expansions when each
/// composite lists `n` components, so a small font could otherwise
/// request billions of blits. Real composites list a handful of
/// components.
const EBDT_COMPOSITE_MAX_COMPONENTS: u32 = 1024;

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
/// Dispatch priority is **CBDT (color) > sbix png > EBDT (mono)**,
/// see [`Face::glyph_bitmap`](sigilbuzz::Face::glyph_bitmap). Within
/// the sbix variant, `'png '` decodes inline, `'jpg '` decodes via
/// the in-crate JPEG decoder ([`crate::decode_jpeg`]), `'tiff'`
/// decodes via [`crate::decode_tiff`], `'dupe'` recurses (with a depth
/// cap) into the referenced gid, and `'jp2 '` surfaces
/// [`RenderError::UnsupportedBitmap`].
///
/// Returns:
/// - `Ok(pixmap)`: decoded and (optionally) rescaled bitmap.
/// - `Err(RenderError::NoBitmap(gid))`: no strike covers `gid`, or
///   the font carries no bitmap tables.
/// - `Err(RenderError::UnsupportedBitmap)`: sbix payload is jp2 or an
///   unknown tag, a TIFF uses an unsupported feature, or `'dupe'`
///   recursion exceeds the depth cap.
/// - `Err(RenderError::BadJpeg(...))`: sbix `'jpg '` payload failed
///   to decode (truncated, arithmetic-coded, etc.).
/// - `Err(RenderError::BadTiff(...))`: sbix `'tiff'` payload failed
///   structural validation.
/// - `Err(RenderError::BitmapDecodeFailed(_))`: EBDT composite
///   (formats 8 / 9) recursion hit a cycle, self-reference, OOB
///   component glyph id, missing-at-strike component,
///   `EBDT_COMPOSITE_MAX_DEPTH`, or `EBDT_COMPOSITE_MAX_COMPONENTS`.
/// - `Err(RenderError::BadPng(...))`: PNG payload failed to decode.
///
/// [`rasterize_bitmap_glyph_placed`] returns the same pixmap together
/// with its offset from the glyph origin.
///
/// # Errors
/// Surfaces all of the above plus [`RenderError::BadSize`] for a
/// non-finite or non-positive `size_pt`, and [`RenderError::Parse`]
/// for any underlying sigilbuzz parser failure pulling tables.
pub fn rasterize_bitmap_glyph(
    rasterizer: &Rasterizer,
    face: &Face<'_>,
    gid: u16,
    size_pt: f32,
    coords: &[f32],
) -> Result<ColorPixmap, RenderError> {
    rasterize_bitmap_glyph_placed(rasterizer, face, gid, size_pt, coords).map(|(pixmap, _)| pixmap)
}

/// Rasterizes an embedded bitmap glyph like [`rasterize_bitmap_glyph`]
/// and also returns where the pixmap sits relative to the glyph origin
/// (see [`Placement`]).
///
/// The offset comes from the strike that supplied the bitmap:
///
/// - CBDT and EBDT: the horizontal metrics, so `left` is the bearing
///   X and `top` is minus the bearing Y.
/// - sbix: the origin offset, which places the image's bottom-left
///   corner, so `left` is the X offset and `top` is minus the Y offset
///   minus the image height. A `'dupe'` glyph takes the offset of the
///   glyph it aliases, as in HarfBuzz.
///
/// When the bitmap is resampled to `size_pt`, the offset scales with it
/// by `size_pt / strike_ppem` and rounds to the nearest pixel.
///
/// ```
/// use sigilbuzz::Face;
/// use sigilbuzz_render::{rasterize_bitmap_glyph_placed, Rasterizer};
///
/// // One 32 ppem sbix strike; gid 1 sits on the glyph origin.
/// let data = include_bytes!("../../../tests/fixtures/sbix_synthetic.ttf");
/// let face = Face::parse_bytes(data, 0).unwrap();
/// let (pix, at) = rasterize_bitmap_glyph_placed(&Rasterizer::new(), &face, 1, 32.0, &[]).unwrap();
/// // Its bottom edge rests on the baseline.
/// assert_eq!(at.left, 0);
/// assert_eq!(at.top + pix.height as i32, 0);
/// ```
///
/// # Errors
/// The same as [`rasterize_bitmap_glyph`].
pub fn rasterize_bitmap_glyph_placed(
    _rasterizer: &Rasterizer,
    face: &Face<'_>,
    gid: u16,
    size_pt: f32,
    _coords: &[f32],
) -> Result<(ColorPixmap, Placement), RenderError> {
    let mut composite = CompositeState {
        chain: Vec::new(),
        depth: 0,
        components_left: EBDT_COMPOSITE_MAX_COMPONENTS,
    };
    rasterize_bitmap_inner(face, gid, size_pt, 0, &mut composite)
}

/// Top-left corner of a CBDT image relative to the glyph origin, in
/// strike pixels, y down. The bearings measure x to the right and y up
/// to the image's top edge.
fn cbdt_origin(metrics: GlyphBitmapMetrics) -> (f32, f32) {
    let (x, y) = match metrics {
        GlyphBitmapMetrics::Small(m) => (m.bearing_x, m.bearing_y),
        GlyphBitmapMetrics::Big(m) => (m.hori_bearing_x, m.hori_bearing_y),
    };
    (f32::from(x), -f32::from(y))
}

/// [`cbdt_origin`] for an EBDT image.
fn ebdt_origin(metrics: EbdtMetrics) -> (f32, f32) {
    let (x, y) = match metrics {
        EbdtMetrics::Small(m) => (m.bearing_x, m.bearing_y),
        EbdtMetrics::Big(m) => (m.hori_bearing_x, m.hori_bearing_y),
    };
    (f32::from(x), -f32::from(y))
}

/// A decoded sbix image with its strike ppem and its top-left corner
/// relative to the glyph origin, in strike pixels, y down. The sbix
/// origin offset places the image's bottom-left corner, y up.
fn sbix_image(
    decoded: ColorPixmap,
    ppem: u16,
    glyph: &SbixGlyph<'_>,
) -> (ColorPixmap, f32, (f32, f32)) {
    let left = f32::from(glyph.origin_offset_x);
    let top = -(f32::from(glyph.origin_offset_y) + decoded.height as f32);
    (decoded, f32::from(ppem), (left, top))
}

/// EBDT composite recursion bookkeeping, threaded through
/// [`rasterize_bitmap_inner`].
struct CompositeState {
    /// Gids currently being expanded as EBDT composite parents, used
    /// for cycle detection (a component referring back to any ancestor
    /// in the chain is a cycle, not just a direct self-reference).
    chain: Vec<u16>,
    /// EBDT-composite expansion level. Independent of the sbix-side
    /// `dupe_depth` counter.
    depth: u8,
    /// Component expansions still allowed for this glyph. Shared by
    /// every recursion level so the total work stays bounded.
    components_left: u32,
}

/// Rasterizes `gid` and threads the sbix `'dupe'` depth and the EBDT
/// composite bookkeeping through the recursion.
fn rasterize_bitmap_inner(
    face: &Face<'_>,
    gid: u16,
    size_pt: f32,
    dupe_depth: u8,
    composite: &mut CompositeState,
) -> Result<(ColorPixmap, Placement), RenderError> {
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

    // `origin` is the image's top-left corner relative to the glyph
    // origin, in strike pixels, y down.
    let (decoded, strike_ppem, origin) = match entry {
        GlyphBitmapEntry::Cbdt { ppem_y, bitmap, .. } => match bitmap.image_format {
            17..=19 => (
                decode_png(bitmap.data)?,
                f32::from(ppem_y),
                cbdt_origin(bitmap.metrics),
            ),
            _ => return Err(RenderError::UnsupportedBitmap),
        },
        GlyphBitmapEntry::Sbix { ppem, glyph, .. } => match glyph.graphic_type {
            TAG_PNG => sbix_image(decode_png(glyph.data)?, ppem, &glyph),
            TAG_DUPE => {
                if dupe_depth >= SBIX_DUPE_MAX_DEPTH {
                    return Err(RenderError::UnsupportedBitmap);
                }
                // 'dupe' payload is a 2-byte big-endian glyph id of
                // the bitmap to use instead. Recurse with the same
                // size_pt so strike picking happens against the
                // referenced gid's coverage. A self-reference will
                // hit the depth cap rather than loop forever.
                let &[hi, lo, ..] = glyph.data else {
                    return Err(RenderError::UnsupportedBitmap);
                };
                let alias = u16::from_be_bytes([hi, lo]);
                if alias == gid {
                    return Err(RenderError::UnsupportedBitmap);
                }
                return rasterize_bitmap_inner(face, alias, size_pt, dupe_depth + 1, composite);
            }
            // JPEG: hand-rolled decoder. Supports 8-bit baseline and
            // progressive YCbCr (4:4:4 / 4:2:2 / 4:2:0) and grayscale
            // (the slice that real-world font sbix payloads land in).
            // Arithmetic coding, 16-bit precision, restart markers,
            // and AC refinement scans surface as `BadJpeg`.
            TAG_JPG => sbix_image(decode_jpeg(glyph.data)?, ppem, &glyph),
            // TIFF: hand-rolled baseline decoder. Supports 8-bit RGB
            // / RGBA, single IFD, strip-organized, uncompressed or
            // PackBits (compression 1 / 32773). LZW / CCITT / JPEG-in-
            // TIFF / tiled / planar / multi-IFD surface `BadTiff` or
            // `UnsupportedBitmap`.
            TAG_TIFF => sbix_image(decode_tiff(glyph.data)?, ppem, &glyph),
            // JPEG-2000: still each its own ~700-line decoder and
            // even rarer than JPEG in real fonts. Surface cleanly so
            // callers can fall back to outlines.
            TAG_JP2 => return Err(RenderError::UnsupportedBitmap),
            // Unknown four-byte tag: treat as unsupported rather
            // than guessing.
            _ => return Err(RenderError::UnsupportedBitmap),
        },
        GlyphBitmapEntry::Ebdt { ppem_y, bitmap, .. } => {
            let pix = if bitmap.is_composite() {
                decode_ebdt_composite(face, gid, &bitmap, ppem_y, composite)?
            } else {
                decode_ebdt_mono(&bitmap)?
            };
            (pix, f32::from(ppem_y), ebdt_origin(bitmap.metrics))
        }
    };

    if !needs_rescale(&decoded, strike_ppem, size_pt) {
        // Strike metrics are whole pixels, so the casts are exact.
        let at = Placement::new(origin.0 as i32, origin.1 as i32);
        return Ok((decoded, at));
    }
    let scale = size_pt / strike_ppem;
    // Cap rescale target dimensions before the destination pixmap
    // allocation. Without this guard `decoded.width as f32 * scale`
    // can saturate to `u32::MAX` (e.g. caller passing a very large
    // `size_pt` against a small-ppem strike), and the subsequent
    // `vec![0u8; w*h*4]` panics with "capacity overflow". 16384 keeps
    // us aligned with the PNG decoder's per-dim ceiling.
    let dst_w_f = (decoded.width as f32 * scale).round().max(1.0);
    let dst_h_f = (decoded.height as f32 * scale).round().max(1.0);
    if !dst_w_f.is_finite()
        || !dst_h_f.is_finite()
        || dst_w_f > MAX_BITMAP_DIM
        || dst_h_f > MAX_BITMAP_DIM
    {
        return Err(RenderError::BadSize(size_pt));
    }
    let dst_w = (dst_w_f as u32).max(1);
    let dst_h = (dst_h_f as u32).max(1);
    // The offset scales with the image and rounds to whole pixels.
    let at = Placement::new(
        (origin.0 * scale).round() as i32,
        (origin.1 * scale).round() as i32,
    );
    Ok((rescale_bilinear(&decoded, dst_w, dst_h), at))
}

/// Maximum pixel dimension for a rescaled bitmap embed. Matches the
/// PNG decoder's per-dim ceiling (`Ihdr::parse`) so caller-driven
/// `size_pt` cannot multiply a small-ppem strike up to an allocation
/// that overflows `usize`.
const MAX_BITMAP_DIM: f32 = 16384.0;

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

#[cfg(test)]
mod tests;
