//! Minimum-viable baseline TIFF decoder for sbix `'tiff'` payloads.
//!
//! Hand-rolled from TIFF 6.0 (1992). Scope is narrow:
//! just enough to decode the TIFF payloads that show up in the very
//! occasional sbix font that uses them, and nothing more. Real-world
//! sbix TIFFs are vanishingly rare in 2026 (Apple has shipped PNG-only
//! Color Emoji for years, and the TIFF arm exists mostly for historical
//! Mac graphics interchange), so a full TIFF reader would be a poor
//! return on engineering time. We aim at the format's "baseline" core:
//!
//! - **8-bit per sample**, 3-component RGB or 4-component RGBA.
//! - **PhotometricInterpretation = 2** (RGB).
//! - **Compression = 1** (none) or **32773** (PackBits RLE). CCITT,
//!   LZW, deflate, JPEG-in-TIFF, and the "old style" JPEG (compression
//!   6/7) all surface [`RenderError::UnsupportedBitmap`].
//! - **Strip-organized** (`StripOffsets` + `StripByteCounts` +
//!   `RowsPerStrip`); tiled TIFFs are rejected.
//! - **Single image (one IFD)**. SubIFDs and additional IFD chains are
//!   ignored. We only ever look at the first IFD's image data.
//! - **PlanarConfiguration = 1** (chunky / interleaved). Planar (one
//!   plane per sample) is rejected.
//!
//! Output is a premultiplied RGBA [`ColorPixmap`]. RGB inputs get
//! alpha = 255; RGBA inputs are premultiplied via the same
//! `(r * a + 127) / 255` round used by the PNG decoder so a downstream
//! compositor sees consistent samples regardless of source format.
//!
//! # Layout primer
//!
//! TIFF is a tagged-table format. The header is 8 bytes:
//!
//! ```text
//!   bytes 0..2 : byte order ("II" = little-endian, "MM" = big-endian)
//!   bytes 2..4 : magic 42 (in the chosen byte order)
//!   bytes 4..8 : offset to first IFD
//! ```
//!
//! An IFD (Image File Directory) is a u16 entry count, then that many
//! 12-byte entries, then a u32 "next IFD" offset (zero terminates).
//! Each entry is `{ tag: u16, type: u16, count: u32, value: u32 }`:
//! `value` is either the inline data (when it fits in 4 bytes) or an
//! offset into the file. The tags we care about are:
//!
//! ```text
//!   256 ImageWidth                 257 ImageLength
//!   258 BitsPerSample (per-sample) 259 Compression
//!   262 PhotometricInterpretation  273 StripOffsets
//!   277 SamplesPerPixel            278 RowsPerStrip
//!   279 StripByteCounts            284 PlanarConfiguration
//! ```
//!
//! # Non-goals
//!
//! - LZW / CCITT / deflate / JPEG-in-TIFF compression.
//! - Tiled TIFFs, planar TIFFs, multi-IFD TIFFs.
//! - `BitsPerSample` other than 8 (1-bit fax / 16-bit deep color).
//! - Photometrics other than RGB (PaletteColor, BlackIsZero,
//!   WhiteIsZero, YCbCr, CIELab, etc.).
//! - EXIF / GeoTIFF / GeoKey extensions.
//!
//! Spec reference: TIFF Revision 6.0 (Adobe, 3 June 1992), sections
//! 1-7 ("Baseline TIFF") and 9 (PackBits compression).

use alloc::vec::Vec;

use crate::error::RenderError;
use crate::pixmap::ColorPixmap;

// ---------------------------------------------------------------------------
// Tag identifiers (TIFF 6.0 §3 + §8). We define every tag we *consume*;
// tags we don't recognize are skipped silently in [`parse_ifd`].
// ---------------------------------------------------------------------------

const TAG_IMAGE_WIDTH: u16 = 256;
const TAG_IMAGE_LENGTH: u16 = 257;
const TAG_BITS_PER_SAMPLE: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_PHOTOMETRIC: u16 = 262;
const TAG_STRIP_OFFSETS: u16 = 273;
const TAG_SAMPLES_PER_PIXEL: u16 = 277;
const TAG_ROWS_PER_STRIP: u16 = 278;
const TAG_STRIP_BYTE_COUNTS: u16 = 279;
const TAG_PLANAR_CONFIG: u16 = 284;

// ---------------------------------------------------------------------------
// Field types (TIFF 6.0 §2). Only types 1 (BYTE), 3 (SHORT), and 4
// (LONG) appear in baseline tags we read; the rest are tolerated only
// for tag values we don't act on.
// ---------------------------------------------------------------------------

const FIELD_BYTE: u16 = 1;
const FIELD_SHORT: u16 = 3;
const FIELD_LONG: u16 = 4;

const COMPRESSION_NONE: u32 = 1;
const COMPRESSION_PACKBITS: u32 = 32773;

/// Largest output-to-input ratio PackBits can reach: a two-byte repeat
/// record expands to 128 bytes.
const PACKBITS_MAX_EXPANSION: usize = 64;

/// The two strip encodings the decoder accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compression {
    /// Compression tag 1: raw samples.
    None,
    /// Compression tag 32773: PackBits run-length encoding.
    PackBits,
}

const PHOTOMETRIC_RGB: u32 = 2;

const PLANAR_CHUNKY: u32 = 1;

/// Mirrors the PNG / JPEG decoders' per-dim ceiling so a malicious
/// font cannot drive a hostile-but-valid TIFF header into a multi-GB
/// allocation.
const MAX_TIFF_DIM: u32 = 16_384;

/// Decodes a TIFF byte slice into a premultiplied RGBA [`ColorPixmap`].
///
/// See the module docs for the supported subset (baseline 8-bit RGB /
/// RGBA, single IFD, strip-organized, no-compression or PackBits).
/// Anything outside that subset surfaces either
/// [`RenderError::BadTiff`] (structural failure: bad magic, truncated
/// IFD, missing required tag, length mismatch) or
/// [`RenderError::UnsupportedBitmap`] (well-formed but a feature we
/// don't implement: LZW / JPEG-in-TIFF, tiled, planar, 16-bit).
///
/// # Errors
/// Returns [`RenderError::BadTiff`] when the payload's structural
/// invariants are violated, and [`RenderError::UnsupportedBitmap`]
/// for legal-but-out-of-scope features.
pub fn decode_tiff(bytes: &[u8]) -> Result<ColorPixmap, RenderError> {
    let (endian, ifd_offset) = parse_header(bytes)?;
    let ifd = parse_ifd(bytes, ifd_offset, endian)?;

    let width = ifd.require_scalar(TAG_IMAGE_WIDTH)?;
    let height = ifd.require_scalar(TAG_IMAGE_LENGTH)?;
    if width == 0 || height == 0 {
        return Err(RenderError::BadTiff("zero dimension"));
    }
    if width > MAX_TIFF_DIM || height > MAX_TIFF_DIM {
        return Err(RenderError::BadTiff("dimensions exceed 16384"));
    }

    let samples_per_pixel = ifd.optional_scalar(TAG_SAMPLES_PER_PIXEL)?.unwrap_or(1);
    if !(samples_per_pixel == 3 || samples_per_pixel == 4) {
        return Err(RenderError::UnsupportedBitmap);
    }

    let bits = ifd.require_array(TAG_BITS_PER_SAMPLE, bytes, endian)?;
    if bits.len() as u32 != samples_per_pixel {
        return Err(RenderError::BadTiff("BitsPerSample count mismatch"));
    }
    if bits.iter().any(|&b| b != 8) {
        return Err(RenderError::UnsupportedBitmap);
    }

    let photometric = ifd
        .optional_scalar(TAG_PHOTOMETRIC)?
        .ok_or(RenderError::BadTiff("missing PhotometricInterpretation"))?;
    if photometric != PHOTOMETRIC_RGB {
        return Err(RenderError::UnsupportedBitmap);
    }

    let compression = match ifd.optional_scalar(TAG_COMPRESSION)?.unwrap_or(1) {
        COMPRESSION_NONE => Compression::None,
        COMPRESSION_PACKBITS => Compression::PackBits,
        _ => return Err(RenderError::UnsupportedBitmap),
    };

    if let Some(planar) = ifd.optional_scalar(TAG_PLANAR_CONFIG)? {
        if planar != PLANAR_CHUNKY {
            return Err(RenderError::UnsupportedBitmap);
        }
    }

    let rows_per_strip = ifd
        .optional_scalar(TAG_ROWS_PER_STRIP)?
        .filter(|&n| n != 0)
        .unwrap_or(height);
    let strip_offsets = ifd.require_array(TAG_STRIP_OFFSETS, bytes, endian)?;
    let strip_byte_counts = ifd.require_array(TAG_STRIP_BYTE_COUNTS, bytes, endian)?;
    if strip_offsets.len() != strip_byte_counts.len() {
        return Err(RenderError::BadTiff("strip offset/length count mismatch"));
    }
    let expected_strips = (height as usize).div_ceil(rows_per_strip as usize);
    if strip_offsets.len() != expected_strips {
        return Err(RenderError::BadTiff("strip count mismatch"));
    }

    let row_bytes = (width as usize)
        .checked_mul(samples_per_pixel as usize)
        .ok_or(RenderError::BadTiff("row size overflow"))?;
    let total_bytes = row_bytes
        .checked_mul(height as usize)
        .ok_or(RenderError::BadTiff("image size overflow"))?;
    // PackBits expands at most 64x (a two-byte repeat record yields 128
    // bytes) and uncompressed strips copy 1:1, so a payload that cannot
    // hold `total_bytes` even at that ratio is malformed. Checking this
    // before allocating keeps a tiny header from reserving up to a
    // gigabyte, including through strips that alias one another.
    if total_bytes / PACKBITS_MAX_EXPANSION > bytes.len() {
        return Err(RenderError::BadTiff("image size exceeds payload"));
    }

    // Collect all strip payloads (decompressed if needed) into a single
    // contiguous buffer of `height * width * samples_per_pixel` bytes.
    // This keeps the per-pixel emit loop simple.
    let mut raw = Vec::with_capacity(total_bytes);
    for (i, (&off, &len)) in strip_offsets
        .iter()
        .zip(strip_byte_counts.iter())
        .enumerate()
    {
        let off = off as usize;
        let len = len as usize;
        let end = off
            .checked_add(len)
            .ok_or(RenderError::BadTiff("strip offset overflow"))?;
        if end > bytes.len() {
            return Err(RenderError::BadTiff("strip out of bounds"));
        }
        let strip_bytes = &bytes[off..end];
        let rows_in_strip = if i + 1 == expected_strips {
            // Last strip may be short: `height % rows_per_strip` rows.
            let r = height as usize - i * rows_per_strip as usize;
            r.max(1)
        } else {
            rows_per_strip as usize
        };
        let strip_pixels = rows_in_strip
            .checked_mul(row_bytes)
            .ok_or(RenderError::BadTiff("strip size overflow"))?;
        match compression {
            Compression::None => {
                if strip_bytes.len() != strip_pixels {
                    return Err(RenderError::BadTiff("uncompressed strip length mismatch"));
                }
                raw.extend_from_slice(strip_bytes);
            }
            Compression::PackBits => {
                packbits_decode(strip_bytes, strip_pixels, &mut raw)?;
            }
        }
    }
    if raw.len() != total_bytes {
        return Err(RenderError::BadTiff("decoded size mismatch"));
    }

    // Emit RGBA. RGB inputs (samples_per_pixel == 3) just append
    // alpha = 255; RGBA inputs go through the same premultiply ladder
    // as the PNG decoder so the downstream compositor sees consistent
    // samples no matter which arm produced the pixmap.
    let mut pixels = Vec::with_capacity((width as usize) * (height as usize) * 4);
    if samples_per_pixel == 3 {
        for chunk in raw.chunks_exact(3) {
            pixels.push(chunk[0]);
            pixels.push(chunk[1]);
            pixels.push(chunk[2]);
            pixels.push(255);
        }
    } else {
        for chunk in raw.chunks_exact(4) {
            push_premul(&mut pixels, chunk[0], chunk[1], chunk[2], chunk[3]);
        }
    }

    let mut out = ColorPixmap::new(width, height);
    out.data = pixels;
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endian {
    Little,
    Big,
}

fn parse_header(bytes: &[u8]) -> Result<(Endian, u32), RenderError> {
    if bytes.len() < 8 {
        return Err(RenderError::BadTiff("header too short"));
    }
    let endian = match &bytes[0..2] {
        b"II" => Endian::Little,
        b"MM" => Endian::Big,
        _ => return Err(RenderError::BadTiff("bad byte order mark")),
    };
    let magic = read_u16(&bytes[2..4], endian);
    if magic != 42 {
        return Err(RenderError::BadTiff("bad magic"));
    }
    let ifd_offset = read_u32(&bytes[4..8], endian);
    Ok((endian, ifd_offset))
}

/// Decoded representation of a single IFD entry. The raw value field
/// is kept so callers can decide on a per-tag basis whether to treat
/// the 4 inline bytes as an integer, an offset, or a length+offset.
#[derive(Debug, Clone, Copy)]
struct IfdEntry {
    tag: u16,
    field_type: u16,
    count: u32,
    value_or_offset: [u8; 4],
}

struct Ifd {
    entries: Vec<IfdEntry>,
    endian: Endian,
}

impl Ifd {
    fn find(&self, tag: u16) -> Option<&IfdEntry> {
        self.entries.iter().find(|e| e.tag == tag)
    }

    /// Returns a single-valued integer tag (count == 1) as `u32`.
    /// `Ok(None)` if the tag is absent; `Err` if the tag is present
    /// but doesn't decode to a single integer.
    fn optional_scalar(&self, tag: u16) -> Result<Option<u32>, RenderError> {
        let Some(entry) = self.find(tag) else {
            return Ok(None);
        };
        if entry.count != 1 {
            return Err(RenderError::BadTiff("expected scalar tag"));
        }
        Ok(Some(read_inline_scalar(entry, self.endian)?))
    }

    /// Like [`optional_scalar`] but errors out if the tag is missing.
    fn require_scalar(&self, tag: u16) -> Result<u32, RenderError> {
        self.optional_scalar(tag)?
            .ok_or(RenderError::BadTiff("missing required scalar tag"))
    }

    /// Returns the full integer array for a tag. Resolves the inline-
    /// vs-offset branch for us. The output is `Vec<u32>` for both BYTE
    /// (1-byte) and SHORT (2-byte) and LONG (4-byte) tags so callers
    /// don't have to fork on field type.
    fn require_array(
        &self,
        tag: u16,
        file: &[u8],
        endian: Endian,
    ) -> Result<Vec<u32>, RenderError> {
        let entry = self
            .find(tag)
            .ok_or(RenderError::BadTiff("missing required array tag"))?;
        read_array(entry, file, endian)
    }
}

fn parse_ifd(bytes: &[u8], offset: u32, endian: Endian) -> Result<Ifd, RenderError> {
    let off = offset as usize;
    if off.checked_add(2).map_or(true, |o| o > bytes.len()) {
        return Err(RenderError::BadTiff("IFD offset out of bounds"));
    }
    let count = read_u16(&bytes[off..off + 2], endian) as usize;
    // 12 bytes per entry, +2 for the count, +4 for the next-IFD offset.
    let body_end = off
        .checked_add(2)
        .and_then(|o| o.checked_add(count.checked_mul(12)?))
        .ok_or(RenderError::BadTiff("IFD body overflow"))?;
    if body_end
        .checked_add(4)
        .map_or(true, |end| end > bytes.len())
    {
        return Err(RenderError::BadTiff("IFD truncated"));
    }
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let entry_off = off + 2 + i * 12;
        let tag = read_u16(&bytes[entry_off..entry_off + 2], endian);
        let field_type = read_u16(&bytes[entry_off + 2..entry_off + 4], endian);
        let count_field = read_u32(&bytes[entry_off + 4..entry_off + 8], endian);
        let mut value = [0u8; 4];
        value.copy_from_slice(&bytes[entry_off + 8..entry_off + 12]);
        entries.push(IfdEntry {
            tag,
            field_type,
            count: count_field,
            value_or_offset: value,
        });
    }
    Ok(Ifd { entries, endian })
}

/// Reads a single-valued integer entry. The inline bytes are
/// interpreted in the endian of the file. We accept BYTE / SHORT /
/// LONG; anything else for a count-1 integer tag is malformed.
fn read_inline_scalar(entry: &IfdEntry, endian: Endian) -> Result<u32, RenderError> {
    let v = entry.value_or_offset;
    match entry.field_type {
        FIELD_BYTE => Ok(u32::from(v[0])),
        FIELD_SHORT => Ok(u32::from(read_u16(&v[..2], endian))),
        FIELD_LONG => Ok(read_u32(&v, endian)),
        _ => Err(RenderError::BadTiff("unsupported field type for scalar")),
    }
}

/// Reads the array values for an IFD entry, regardless of inline-vs-
/// offset placement and BYTE / SHORT / LONG type.
fn read_array(entry: &IfdEntry, file: &[u8], endian: Endian) -> Result<Vec<u32>, RenderError> {
    /// Integer field types an array tag may use.
    enum Elem {
        Byte,
        Short,
        Long,
    }
    let (elem, elem_size) = match entry.field_type {
        FIELD_BYTE => (Elem::Byte, 1),
        FIELD_SHORT => (Elem::Short, 2),
        FIELD_LONG => (Elem::Long, 4),
        _ => return Err(RenderError::BadTiff("unsupported field type for array")),
    };
    let total = (entry.count as usize)
        .checked_mul(elem_size)
        .ok_or(RenderError::BadTiff("array byte length overflow"))?;
    let data: &[u8] = if total <= 4 {
        &entry.value_or_offset[..total]
    } else {
        let off = read_u32(&entry.value_or_offset, endian) as usize;
        let end = off
            .checked_add(total)
            .ok_or(RenderError::BadTiff("array offset overflow"))?;
        if end > file.len() {
            return Err(RenderError::BadTiff("array out of bounds"));
        }
        &file[off..end]
    };
    let mut out = Vec::with_capacity(entry.count as usize);
    match elem {
        Elem::Byte => out.extend(data.iter().map(|&b| u32::from(b))),
        Elem::Short => {
            for chunk in data.chunks_exact(2) {
                out.push(u32::from(read_u16(chunk, endian)));
            }
        }
        Elem::Long => {
            for chunk in data.chunks_exact(4) {
                out.push(read_u32(chunk, endian));
            }
        }
    }
    Ok(out)
}

fn read_u16(bytes: &[u8], endian: Endian) -> u16 {
    match endian {
        Endian::Little => u16::from_le_bytes([bytes[0], bytes[1]]),
        Endian::Big => u16::from_be_bytes([bytes[0], bytes[1]]),
    }
}

fn read_u32(bytes: &[u8], endian: Endian) -> u32 {
    match endian {
        Endian::Little => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        Endian::Big => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
    }
}

/// PackBits decompression (TIFF 6.0 §9). Decodes `src` into `out`,
/// appending exactly `expected` bytes. Each record is one signed-byte
/// header `n`:
///
///   * `0..=127`   -> copy the next `n + 1` bytes literally.
///   * `-127..=-1` -> repeat the next byte `1 - n` times.
///   * `-128`      -> no-op (skip the header byte and continue).
///
/// We bound the output to `expected` so a malformed strip can't blow
/// up our accumulator past what the strip's row_bytes / rows_per_strip
/// budget allows.
fn packbits_decode(src: &[u8], expected: usize, out: &mut Vec<u8>) -> Result<(), RenderError> {
    let start = out.len();
    let mut i = 0usize;
    while i < src.len() {
        let n = src[i] as i8;
        i += 1;
        if n >= 0 {
            // Literal run of n + 1 bytes.
            let run = n as usize + 1;
            let end = i
                .checked_add(run)
                .ok_or(RenderError::BadTiff("packbits literal overflow"))?;
            if end > src.len() {
                return Err(RenderError::BadTiff("packbits truncated literal"));
            }
            if out.len() - start + run > expected {
                return Err(RenderError::BadTiff("packbits output overrun"));
            }
            out.extend_from_slice(&src[i..end]);
            i = end;
        } else if n == -128 {
            // No-op header.
            continue;
        } else {
            // Repeat run: 1 - n copies of the next byte.
            let run = 1 - n as i32;
            // n in [-127, -1] => run in [2, 128]. Both fit u8.
            let run = run as usize;
            if i >= src.len() {
                return Err(RenderError::BadTiff("packbits truncated repeat"));
            }
            let byte = src[i];
            i += 1;
            if out.len() - start + run > expected {
                return Err(RenderError::BadTiff("packbits output overrun"));
            }
            out.resize(out.len() + run, byte);
        }
    }
    if out.len() - start != expected {
        return Err(RenderError::BadTiff("packbits short output"));
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
        let aa = u32::from(a);
        out.push(((u32::from(r) * aa + 127) / 255) as u8);
        out.push(((u32::from(g) * aa + 127) / 255) as u8);
        out.push(((u32::from(b) * aa + 127) / 255) as u8);
        out.push(a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Builds a minimal little-endian baseline TIFF: one IFD, no
    /// compression, 8-bit RGB, single strip. The IFD entries are
    /// sorted in ascending tag order (TIFF §2 requires this).
    ///
    /// Layout:
    /// - bytes 0..8  : header (II, magic 42, ifd_offset = 8)
    /// - bytes 8..12 : IFD count + 8 entries (12 bytes each = 96)
    ///   + next IFD offset (zero)
    /// - then BitsPerSample inline (3 SHORTs = 6 bytes don't fit
    ///   in 4 bytes inline -> external)
    /// - then strip data
    fn build_uncompressed_rgb_tiff(
        width: u32,
        height: u32,
        rows_per_strip: u32,
        pixels: &[u8],
    ) -> Vec<u8> {
        assert_eq!(pixels.len(), (width * height * 3) as usize);
        let mut buf = Vec::new();
        // Header
        buf.extend_from_slice(b"II");
        buf.extend_from_slice(&42u16.to_le_bytes());
        buf.extend_from_slice(&8u32.to_le_bytes()); // first IFD at offset 8

        // IFD: 8 entries
        let entries: [(u16, u16, u32, u32); 8] = [
            // tag, type, count, value_or_offset
            (TAG_IMAGE_WIDTH, FIELD_LONG, 1, width),
            (TAG_IMAGE_LENGTH, FIELD_LONG, 1, height),
            // BitsPerSample needs 6 bytes (3 SHORTs); placed at the
            // first byte after the IFD body (count + 8*12 + 4 = 102).
            (TAG_BITS_PER_SAMPLE, FIELD_SHORT, 3, 0 /* fill below */),
            (TAG_COMPRESSION, FIELD_SHORT, 1, COMPRESSION_NONE),
            (TAG_PHOTOMETRIC, FIELD_SHORT, 1, PHOTOMETRIC_RGB),
            (TAG_STRIP_OFFSETS, FIELD_LONG, 1, 0 /* fill below */),
            (TAG_SAMPLES_PER_PIXEL, FIELD_SHORT, 1, 3),
            (TAG_ROWS_PER_STRIP, FIELD_LONG, 1, rows_per_strip),
        ];
        // We have 9 entries in spirit; the actual array also needs
        // StripByteCounts. Move to a dynamic build to keep entry
        // ordering correct.
        let _ = entries; // discard above; rebuild dynamically below.

        let mut ifd_entries: Vec<(u16, u16, u32, [u8; 4])> = Vec::new();
        ifd_entries.push((TAG_IMAGE_WIDTH, FIELD_LONG, 1, width.to_le_bytes()));
        ifd_entries.push((TAG_IMAGE_LENGTH, FIELD_LONG, 1, height.to_le_bytes()));

        // Compute external-data layout. After IFD body we place:
        //   - BitsPerSample (3 SHORTs = 6 bytes)
        //   - StripOffsets / StripByteCounts: one strip per
        //     rows_per_strip-row chunk; if multiple strips, each is
        //     emitted as a LONG array.
        let strips = (height as usize).div_ceil(rows_per_strip as usize);
        let ifd_count = 9;
        let ifd_body_size = 2 + ifd_count * 12 + 4;
        let bps_off = 8 + ifd_body_size;
        let strip_offsets_off = bps_off + 6;
        // If strips == 1 the offset / byte-count fit inline in the
        // IFD entry's value field, so no external array is emitted.
        let strip_offsets_array_size = if strips == 1 { 0 } else { strips * 4 };
        let strip_byte_counts_off = strip_offsets_off + strip_offsets_array_size;
        let strip_byte_counts_array_size = if strips == 1 { 0 } else { strips * 4 };
        let pixel_data_off = strip_byte_counts_off + strip_byte_counts_array_size;

        // BitsPerSample (3 SHORTs) external: value is the offset.
        ifd_entries.push((
            TAG_BITS_PER_SAMPLE,
            FIELD_SHORT,
            3,
            (bps_off as u32).to_le_bytes(),
        ));
        ifd_entries.push((
            TAG_COMPRESSION,
            FIELD_SHORT,
            1,
            // SHORT inline: u16 in low 2 bytes, then 2 bytes of zero.
            {
                let mut b = [0u8; 4];
                b[..2].copy_from_slice(&(COMPRESSION_NONE as u16).to_le_bytes());
                b
            },
        ));
        ifd_entries.push((TAG_PHOTOMETRIC, FIELD_SHORT, 1, {
            let mut b = [0u8; 4];
            b[..2].copy_from_slice(&(PHOTOMETRIC_RGB as u16).to_le_bytes());
            b
        }));

        // StripOffsets: if 1 strip, inline; else external.
        if strips == 1 {
            ifd_entries.push((
                TAG_STRIP_OFFSETS,
                FIELD_LONG,
                1,
                (pixel_data_off as u32).to_le_bytes(),
            ));
        } else {
            ifd_entries.push((
                TAG_STRIP_OFFSETS,
                FIELD_LONG,
                strips as u32,
                (strip_offsets_off as u32).to_le_bytes(),
            ));
        }

        ifd_entries.push((TAG_SAMPLES_PER_PIXEL, FIELD_SHORT, 1, {
            let mut b = [0u8; 4];
            b[..2].copy_from_slice(&3u16.to_le_bytes());
            b
        }));
        ifd_entries.push((
            TAG_ROWS_PER_STRIP,
            FIELD_LONG,
            1,
            rows_per_strip.to_le_bytes(),
        ));

        // StripByteCounts: same inline-or-external logic.
        let row_bytes = (width * 3) as usize;
        if strips == 1 {
            ifd_entries.push((
                TAG_STRIP_BYTE_COUNTS,
                FIELD_LONG,
                1,
                (height * width * 3).to_le_bytes(),
            ));
        } else {
            ifd_entries.push((
                TAG_STRIP_BYTE_COUNTS,
                FIELD_LONG,
                strips as u32,
                (strip_byte_counts_off as u32).to_le_bytes(),
            ));
        }

        // Sort by tag ascending.
        ifd_entries.sort_by_key(|e| e.0);
        assert_eq!(ifd_entries.len(), ifd_count);

        // Emit IFD.
        buf.extend_from_slice(&(ifd_count as u16).to_le_bytes());
        for (tag, ty, count, val) in &ifd_entries {
            buf.extend_from_slice(&tag.to_le_bytes());
            buf.extend_from_slice(&ty.to_le_bytes());
            buf.extend_from_slice(&count.to_le_bytes());
            buf.extend_from_slice(val);
        }
        buf.extend_from_slice(&0u32.to_le_bytes()); // next IFD = 0
        assert_eq!(buf.len(), bps_off);

        // External BitsPerSample = [8, 8, 8] as u16-LE.
        buf.extend_from_slice(&8u16.to_le_bytes());
        buf.extend_from_slice(&8u16.to_le_bytes());
        buf.extend_from_slice(&8u16.to_le_bytes());
        assert_eq!(buf.len(), strip_offsets_off);

        // StripOffsets / StripByteCounts arrays (only used when
        // strips > 1; the inline case still reserves zero bytes here).
        if strips > 1 {
            for i in 0..strips {
                let off = pixel_data_off + i * rows_per_strip as usize * row_bytes;
                buf.extend_from_slice(&(off as u32).to_le_bytes());
            }
            for i in 0..strips {
                let rows_in = if i + 1 == strips {
                    height as usize - i * rows_per_strip as usize
                } else {
                    rows_per_strip as usize
                };
                let len = rows_in * row_bytes;
                buf.extend_from_slice(&(len as u32).to_le_bytes());
            }
        }
        assert_eq!(buf.len(), pixel_data_off);

        buf.extend_from_slice(pixels);
        buf
    }

    #[test]
    fn decode_uncompressed_rgb_4x4_red() {
        let mut pixels = Vec::new();
        for _ in 0..16 {
            pixels.push(255);
            pixels.push(0);
            pixels.push(0);
        }
        let tiff = build_uncompressed_rgb_tiff(4, 4, 4, &pixels);
        let pix = decode_tiff(&tiff).unwrap();
        assert_eq!(pix.width, 4);
        assert_eq!(pix.height, 4);
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(pix.get(x, y), [255, 0, 0, 255]);
            }
        }
    }

    #[test]
    fn decode_uncompressed_rgb_multiple_strips() {
        // 4x4 image with 2 rows per strip -> 2 strips. Pattern: row y
        // is filled with rgb = (y * 16, 128, 255 - y * 16) so we can
        // catch off-by-one strip stitching.
        let mut pixels = Vec::new();
        for y in 0..4u8 {
            for _ in 0..4 {
                pixels.push(y * 16);
                pixels.push(128);
                pixels.push(255 - y * 16);
            }
        }
        let tiff = build_uncompressed_rgb_tiff(4, 4, 2, &pixels);
        let pix = decode_tiff(&tiff).unwrap();
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(
                    pix.get(x, y),
                    [(y as u8) * 16, 128, 255 - (y as u8) * 16, 255]
                );
            }
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bad = vec![b'I', b'I', 0, 0, 0, 0, 0, 0]; // II, magic = 0
        bad[2] = 0;
        bad[3] = 0;
        let err = decode_tiff(&bad).unwrap_err();
        assert!(matches!(err, RenderError::BadTiff(_)));
    }

    #[test]
    fn rejects_bad_byte_order() {
        let bad = vec![b'X', b'X', 42, 0, 8, 0, 0, 0];
        let err = decode_tiff(&bad).unwrap_err();
        assert!(matches!(err, RenderError::BadTiff(_)));
    }

    #[test]
    fn rejects_short_header() {
        let bad = vec![b'I', b'I', 42, 0];
        let err = decode_tiff(&bad).unwrap_err();
        assert!(matches!(err, RenderError::BadTiff(_)));
    }

    #[test]
    fn rejects_unknown_compression() {
        // Build a TIFF then poke compression to LZW (5).
        let mut tiff = build_uncompressed_rgb_tiff(2, 2, 2, &[0u8; 12]);
        // Find Compression tag entry and patch it. The IFD body starts
        // at byte 8; entry layout is sorted by tag, so Compression
        // (259) is the 4th entry (indices 256, 257, 258, 259, ...).
        // Walk the IFD and patch the matching entry's value bytes.
        let count = u16::from_le_bytes([tiff[8], tiff[9]]) as usize;
        for i in 0..count {
            let off = 10 + i * 12;
            let tag = u16::from_le_bytes([tiff[off], tiff[off + 1]]);
            if tag == TAG_COMPRESSION {
                tiff[off + 8] = 5; // LZW
                tiff[off + 9] = 0;
                break;
            }
        }
        let err = decode_tiff(&tiff).unwrap_err();
        assert!(matches!(err, RenderError::UnsupportedBitmap));
    }

    #[test]
    fn packbits_round_trip_known_pattern() {
        // Encode a simple known pattern by hand and verify the decoder
        // reproduces it. The encoded form below corresponds to:
        //   literal "AB" then "CCC" then literal "D":
        //   header 1 (n=1 -> 2 literals), 'A', 'B',
        //   header -2 (1 - (-2) = 3 repeats), 'C',
        //   header 0 (n=0 -> 1 literal), 'D'.
        let encoded: [u8; 7] = [1, b'A', b'B', (-2i8) as u8, b'C', 0, b'D'];
        let mut out = Vec::new();
        packbits_decode(&encoded, 6, &mut out).unwrap();
        assert_eq!(&out, b"ABCCCD");
    }

    #[test]
    fn packbits_no_op_header_is_skipped() {
        // -128 is documented as a no-op; a stream consisting purely of
        // it followed by a literal must decode cleanly.
        let encoded: [u8; 4] = [(-128i8) as u8, 1, b'X', b'Y'];
        let mut out = Vec::new();
        packbits_decode(&encoded, 2, &mut out).unwrap();
        assert_eq!(&out, b"XY");
    }

    #[test]
    fn packbits_overrun_is_rejected() {
        // header says copy 4 bytes, but only 2 exist.
        let encoded: [u8; 3] = [3, b'A', b'B'];
        let mut out = Vec::new();
        let err = packbits_decode(&encoded, 4, &mut out).unwrap_err();
        assert!(matches!(err, RenderError::BadTiff(_)));
    }

    /// Little-endian RGB TIFF with one strip of `strip_len` bytes at
    /// `strip_off`, whatever the declared size. `extra` is appended.
    fn one_strip_tiff(
        width: u32,
        height: u32,
        strip_off: u32,
        strip_len: u32,
        extra: &[u8],
    ) -> Vec<u8> {
        let entries: [(u16, u16, u32, u32); 8] = [
            (TAG_IMAGE_WIDTH, FIELD_LONG, 1, width),
            (TAG_IMAGE_LENGTH, FIELD_LONG, 1, height),
            // Three SHORTs of 8 at offset 110, right after the IFD.
            (TAG_BITS_PER_SAMPLE, FIELD_SHORT, 3, 110),
            (TAG_PHOTOMETRIC, FIELD_SHORT, 1, PHOTOMETRIC_RGB),
            (TAG_STRIP_OFFSETS, FIELD_LONG, 1, strip_off),
            (TAG_SAMPLES_PER_PIXEL, FIELD_SHORT, 1, 3),
            (TAG_ROWS_PER_STRIP, FIELD_LONG, 1, height),
            (TAG_STRIP_BYTE_COUNTS, FIELD_LONG, 1, strip_len),
        ];
        let mut buf = Vec::new();
        buf.extend_from_slice(b"II");
        buf.extend_from_slice(&42u16.to_le_bytes());
        buf.extend_from_slice(&8u32.to_le_bytes());
        buf.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for (tag, ty, count, value) in entries {
            buf.extend_from_slice(&tag.to_le_bytes());
            buf.extend_from_slice(&ty.to_le_bytes());
            buf.extend_from_slice(&count.to_le_bytes());
            buf.extend_from_slice(&value.to_le_bytes());
        }
        buf.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(buf.len(), 110);
        for _ in 0..3 {
            buf.extend_from_slice(&8u16.to_le_bytes());
        }
        buf.extend_from_slice(extra);
        buf
    }

    #[test]
    fn declared_size_larger_than_the_payload_is_rejected_before_allocating() {
        // A 16384x16384 RGB header on a tiny file used to reserve the
        // full 768 MiB raw buffer before looking at the strip.
        let bytes = one_strip_tiff(16_384, 16_384, 116, 3, &[1, 2, 3]);
        assert_eq!(
            decode_tiff(&bytes).unwrap_err(),
            RenderError::BadTiff("image size exceeds payload")
        );
        // A small image whose strip holds all of its pixels still
        // decodes.
        let ok = one_strip_tiff(1, 1, 116, 3, &[1, 2, 3]);
        let pix = decode_tiff(&ok).expect("decodes");
        assert_eq!(pix.get(0, 0), [1, 2, 3, 255]);
    }
}
