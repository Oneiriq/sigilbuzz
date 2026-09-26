//! Forward direction: SFNT -> WOFF2.
//!
//! Mirrors the unwrap pipeline in `mod.rs`:
//!
//! 1. Parse the input SFNT directory.
//! 2. Apply the WOFF2 `glyf`/`loca` forward transform (split each
//!    glyph into the eight WOFF2 streams; `loca` is dropped because
//!    it falls out of the glyph offsets at unwrap time).
//! 3. Emit the WOFF2 header + directory (5-bit known-tag enum +
//!    UIntBase128 lengths).
//! 4. Brotli-compress the concatenation of all per-table payloads in
//!    directory order.
//!
//! The forward triplet encoder is the inverse of the decoder in
//! `transform.rs`. We pick the *smallest* of the 128 encodings that
//! covers the (dx, dy, on_curve) triple, never larger than the
//! 4-byte fallback.
//!
//! Hmtx transform v1 is intentionally not emitted: hmtx stays in its
//! raw SFNT form. The unwrapper supports both, so a wrapped+unwrapped
//! round-trip is byte-equivalent for hmtx regardless.

use alloc::vec::Vec;

use crate::error::{Result, WoffError};
use crate::reader::Reader;

use super::{KNOWN_TAGS, TAG_GLYF, TAG_LOCA, WOFF2_SIGNATURE};

/// Knobs for `wrap_woff2`. Defaults: max-quality Brotli (slowest,
/// smallest), preserve TT instructions.
#[derive(Debug, Clone, Copy)]
pub struct WrapOptions {
    /// Brotli quality, 0..=11. 11 is maximum compression and the
    /// default; lower values trade size for speed.
    pub brotli_quality: u8,
    /// If `false`, simple-glyph instructions are dropped when
    /// emitting the instruction_stream. Composite-glyph
    /// `WE_HAVE_INSTRUCTIONS` records are still preserved (the flag
    /// lives in the component record itself), but the body is
    /// truncated. Default `true`.
    pub retain_hints: bool,
}

impl Default for WrapOptions {
    fn default() -> Self {
        Self {
            brotli_quality: 11,
            retain_hints: true,
        }
    }
}

/// Wraps an SFNT (TTF / OTF) byte slice into a WOFF2 file.
///
/// Equivalent to `wrap_woff2_with_options(sfnt_bytes,
/// WrapOptions::default())`.
///
/// # Errors
///
/// - `BadMagic` for an input whose SFNT signature isn't one of the
///   recognized values.
/// - `Malformed` / `UnexpectedEof` for a truncated or
///   self-inconsistent SFNT.
/// - `Unsupported` for tables this release doesn't know how to
///   transform forward (currently a no-op: the only forward
///   transform shipped is glyf/loca; everything else is copied
///   verbatim).
pub fn wrap_woff2(sfnt_bytes: &[u8]) -> Result<Vec<u8>> {
    wrap_woff2_with_options(sfnt_bytes, WrapOptions::default())
}

/// `wrap_woff2` with caller-supplied [`WrapOptions`].
///
/// # Errors
///
/// Same as [`wrap_woff2`].
pub fn wrap_woff2_with_options(sfnt_bytes: &[u8], opts: WrapOptions) -> Result<Vec<u8>> {
    let parsed = ParsedSfnt::parse(sfnt_bytes)?;

    // Build the per-table payloads + directory entries.
    let (entries, payload) = build_directory_and_payload(&parsed, opts)?;

    // Brotli-compress the concatenated payload.
    let compressed = brotli_compress(&payload, opts.brotli_quality)?;

    // Emit the WOFF2 header + directory + compressed body.
    let mut out: Vec<u8> = Vec::new();
    write_header(
        &mut out,
        parsed.flavor,
        parsed.total_sfnt_size,
        &entries,
        compressed.len(),
    )?;
    write_directory(&mut out, &entries);
    out.extend_from_slice(&compressed);
    // WOFF2 §3: the file is padded to a 4-byte boundary.
    while out.len() % 4 != 0 {
        out.push(0);
    }
    // Patch the header `length` field with the final file size now
    // that we know it. The spec stores total file length at offset 8.
    let final_len = u32::try_from(out.len()).map_err(|_| WoffError::Malformed {
        offset: 0,
        context: "WOFF2 output is larger than 4 GiB",
    })?;
    if let Some(length_field) = out.get_mut(8..12) {
        length_field.copy_from_slice(&final_len.to_be_bytes());
    }

    Ok(out)
}

// -----------------------------------------------------------------------------
// Input SFNT parsing
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SfntTable<'a> {
    tag: [u8; 4],
    /// Table bytes, borrowed from the input. Records may point at
    /// shared bytes, so copying them could cost far more memory than
    /// the input holds.
    body: &'a [u8],
}

#[derive(Debug)]
struct ParsedSfnt<'a> {
    flavor: u32,
    /// Tables in original directory order.
    tables: Vec<SfntTable<'a>>,
    /// Sum of (header + per-table-record + each table padded to 4):
    /// the value the WOFF2 header advertises so the consumer can
    /// pre-allocate the unwrapped buffer.
    total_sfnt_size: u32,
}

impl<'a> ParsedSfnt<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(bytes);
        let flavor = r.read_u32("SFNT sfntVersion")?;
        // Accept TrueType (0x00010000), CFF (`OTTO`), and the legacy
        // Apple `true` and `typ1` flavors. Font collections (`ttcf`)
        // are not supported.
        match flavor {
            0x0001_0000 | 0x4F54_544F /* OTTO */ | 0x7472_7565 /* true */ |
            0x7479_7031 /* typ1 */ => {}
            _ => {
                return Err(WoffError::BadMagic {
                    offset: 0,
                    context: "unrecognised SFNT sfntVersion",
                });
            }
        }
        let num_tables = r.read_u16("SFNT numTables")? as usize;
        // searchRange, entrySelector, rangeShift: derivable; ignored.
        r.skip(6, "SFNT search params")?;

        let mut records: Vec<(usize, [u8; 4], u32, u32)> = Vec::with_capacity(num_tables);
        for _ in 0..num_tables {
            let tag = r.read_tag("SFNT table tag")?;
            let _checksum = r.read_u32("SFNT table checksum")?;
            let offset = r.read_u32("SFNT table offset")?;
            let length = r.read_u32("SFNT table length")?;
            records.push((records.len(), tag, offset, length));
        }

        // Read each table body. Tables in the SFNT are not guaranteed
        // to be in directory order, but we keep the *directory* order
        // for the WOFF2 emitter.
        let mut tables: Vec<SfntTable<'a>> = Vec::with_capacity(num_tables);
        let mut total_sfnt_size: u64 = 12 + 16 * (num_tables as u64);
        for (_, tag, offset, length) in &records {
            let off = *offset as usize;
            let end = off
                .checked_add(*length as usize)
                .ok_or(WoffError::Malformed {
                    offset: 0,
                    context: "SFNT table offset+length overflows",
                })?;
            let body = bytes.get(off..end).ok_or(WoffError::UnexpectedEof {
                offset: off,
                context: "SFNT table body",
            })?;
            tables.push(SfntTable { tag: *tag, body });
            // Each table is padded to 4 bytes in the SFNT footprint
            // we advertise.
            total_sfnt_size += (u64::from(*length) + 3) & !3;
        }

        let Ok(total_sfnt_size) = u32::try_from(total_sfnt_size) else {
            return Err(WoffError::Malformed {
                offset: 0,
                context: "SFNT total size overflows u32",
            });
        };

        Ok(Self {
            flavor,
            tables,
            total_sfnt_size,
        })
    }

    fn find(&self, tag: &[u8; 4]) -> Option<&SfntTable<'a>> {
        self.tables.iter().find(|t| &t.tag == tag)
    }
}

// -----------------------------------------------------------------------------
// Directory / payload assembly
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct OutEntry {
    tag: [u8; 4],
    /// 0 = transformed (glyf/loca), 3 = stored as raw SFNT for
    /// glyf/loca, 0 = no transform for everything else.
    transform_version: u8,
    /// Original (post-inverse-transform) length: what the unwrapper
    /// must produce.
    orig_length: u32,
    /// Length in the brotli payload. For transformed glyf this is
    /// the length of the transformed payload; for transformed loca
    /// this is **0** by spec (the byproduct sits in the glyf
    /// transform). For everything else, equal to `orig_length`.
    transform_length: u32,
    /// Whether to emit the second UIntBase128 length field.
    emit_transform_length: bool,
}

fn build_directory_and_payload(
    parsed: &ParsedSfnt,
    opts: WrapOptions,
) -> Result<(Vec<OutEntry>, Vec<u8>)> {
    // First pass: produce the transformed glyf payload (if any) so
    // we know its size before we lay out the directory. WOFF2 only
    // defines the transform for a glyf and loca pair, so one without
    // the other is an error, as in the reference encoder.
    let glyf_transformed = match (parsed.find(&TAG_GLYF), parsed.find(&TAG_LOCA)) {
        (Some(glyf), Some(loca)) => super::wrap_transform::transform_glyf(
            glyf.body,
            loca.body,
            parsed.find(b"head").map(|t| t.body),
            parsed.find(b"maxp").map(|t| t.body),
            opts.retain_hints,
        )?,
        (None, None) => Vec::new(),
        (Some(_), None) | (None, Some(_)) => {
            return Err(WoffError::Malformed {
                offset: 0,
                context: "wrap_woff2: SFNT has only one of glyf and loca",
            });
        }
    };

    // Table bodies come from u32 lengths, so `as u32` on their lengths
    // is exact. The transformed glyf is built here and gets checked.
    let transformed_len =
        u32::try_from(glyf_transformed.len()).map_err(|_| WoffError::Malformed {
            offset: 0,
            context: "wrap_woff2: transformed glyf is larger than 4 GiB",
        })?;

    let mut entries: Vec<OutEntry> = Vec::with_capacity(parsed.tables.len());
    let mut payload: Vec<u8> = Vec::new();

    for t in &parsed.tables {
        let orig_length = t.body.len() as u32;
        match t.tag {
            TAG_GLYF => {
                // Emit transformed glyf.
                entries.push(OutEntry {
                    tag: TAG_GLYF,
                    transform_version: 0,
                    orig_length,
                    transform_length: transformed_len,
                    emit_transform_length: true,
                });
                payload.extend_from_slice(&glyf_transformed);
            }
            TAG_LOCA => {
                // Loca is dropped from the payload when glyf is
                // transformed; spec mandates transformLength = 0.
                entries.push(OutEntry {
                    tag: TAG_LOCA,
                    transform_version: 0,
                    orig_length,
                    transform_length: 0,
                    emit_transform_length: true,
                });
            }
            _ => {
                entries.push(OutEntry {
                    tag: t.tag,
                    transform_version: 0,
                    orig_length,
                    transform_length: orig_length,
                    emit_transform_length: false,
                });
                payload.extend_from_slice(t.body);
            }
        }
    }

    Ok((entries, payload))
}

// -----------------------------------------------------------------------------
// Header / directory writers
// -----------------------------------------------------------------------------

fn write_header(
    out: &mut Vec<u8>,
    flavor: u32,
    total_sfnt_size: u32,
    entries: &[OutEntry],
    compressed_len: usize,
) -> Result<()> {
    let num_tables = u16::try_from(entries.len()).map_err(|_| WoffError::Malformed {
        offset: 0,
        context: "too many tables for WOFF2",
    })?;
    let compressed_len = u32::try_from(compressed_len).map_err(|_| WoffError::Malformed {
        offset: 0,
        context: "WOFF2 compressed payload is larger than 4 GiB",
    })?;
    out.extend_from_slice(&WOFF2_SIGNATURE.to_be_bytes());
    out.extend_from_slice(&flavor.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // length, patched after body
    out.extend_from_slice(&num_tables.to_be_bytes()); // numTables
    out.extend_from_slice(&0u16.to_be_bytes()); // reserved
    out.extend_from_slice(&total_sfnt_size.to_be_bytes());
    out.extend_from_slice(&compressed_len.to_be_bytes()); // totalCompressedSize
    out.extend_from_slice(&1u16.to_be_bytes()); // majorVersion
    out.extend_from_slice(&0u16.to_be_bytes()); // minorVersion
    out.extend_from_slice(&0u32.to_be_bytes()); // metaOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // metaLength
    out.extend_from_slice(&0u32.to_be_bytes()); // metaOrigLength
    out.extend_from_slice(&0u32.to_be_bytes()); // privOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // privLength
    debug_assert_eq!(out.len(), 48);
    Ok(())
}

fn write_directory(out: &mut Vec<u8>, entries: &[OutEntry]) {
    for e in entries {
        let known = known_tag_index(&e.tag);
        let flag = (e.transform_version << 6) | known.unwrap_or(63);
        out.push(flag);
        if known.is_none() {
            out.extend_from_slice(&e.tag);
        }
        write_uint_base128(out, e.orig_length);
        if e.emit_transform_length {
            write_uint_base128(out, e.transform_length);
        }
    }
}

fn known_tag_index(tag: &[u8; 4]) -> Option<u8> {
    KNOWN_TAGS.iter().position(|t| *t == tag).map(|i| i as u8)
}

/// Writes a WOFF2 `UIntBase128`: 7-bit groups, MSB-first, with the
/// MSB of every byte except the last set as a continuation flag.
fn write_uint_base128(out: &mut Vec<u8>, mut value: u32) {
    // Find how many 7-bit groups we need (1..=5).
    let mut groups: [u8; 5] = [0; 5];
    let mut n = 0;
    if value == 0 {
        out.push(0);
        return;
    }
    while value > 0 && n < 5 {
        groups[n] = (value & 0x7F) as u8;
        value >>= 7;
        n += 1;
    }
    // We collected low-to-high; emit high-to-low. Set the
    // continuation bit on every byte except the last we'll write.
    for i in (0..n).rev() {
        let byte = groups[i];
        if i == 0 {
            out.push(byte);
        } else {
            out.push(byte | 0x80);
        }
    }
}

// -----------------------------------------------------------------------------
// Brotli encoder
// -----------------------------------------------------------------------------

fn brotli_compress(input: &[u8], quality: u8) -> Result<Vec<u8>> {
    use brotli::enc::BrotliEncoderParams;
    use brotli::BrotliCompress;
    use std::io::Cursor;

    // WOFF2 §3 specifies a 22-bit (4 MiB) window, the default for
    // Brotli quality >= 1, but pin it explicitly for determinism.
    let params = BrotliEncoderParams {
        quality: i32::from(quality.min(11)),
        lgwin: 22,
        ..BrotliEncoderParams::default()
    };

    let mut reader = Cursor::new(input);
    let mut out: Vec<u8> = Vec::with_capacity(input.len() / 2 + 16);
    BrotliCompress(&mut reader, &mut out, &params).map_err(|_| WoffError::BrotliDecode {
        context: "BrotliCompress returned an error",
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uint_base128_round_trip() {
        // Compare encoder against the existing decoder for a sweep
        // of representative values.
        for &v in &[
            0u32,
            1,
            0x7F,
            0x80,
            0x3FFF,
            0x4000,
            0x1FFFFF,
            0x200000,
            0xFFFF_FFFF,
        ] {
            let mut buf = Vec::new();
            write_uint_base128(&mut buf, v);
            let mut r = crate::reader::Reader::new(&buf);
            let decoded = r.read_uint_base128().expect("round-trips");
            assert_eq!(decoded, v, "mismatch for {v:#x}");
        }
    }

    #[test]
    fn known_tag_index_matches_unwrapper() {
        assert_eq!(known_tag_index(b"cmap"), Some(0));
        assert_eq!(known_tag_index(b"glyf"), Some(10));
        assert_eq!(known_tag_index(b"loca"), Some(11));
        assert_eq!(known_tag_index(b"zzzz"), None);
    }
}
