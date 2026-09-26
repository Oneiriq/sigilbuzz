//! WOFF2 unwrap.
//!
//! WOFF2 is meaningfully more involved than WOFF1: a single
//! Brotli-compressed payload holds every table back-to-back, and the
//! `glyf` and `loca` tables are stored in a per-stream "transformed"
//! form rather than as raw SFNT bytes. The unwrapper here:
//!
//! 1. Parses the 48-byte header and validates the signature
//!    (`0x774F4632`, "wOF2") and reserved field.
//! 2. Walks the table directory, decoding the 5-bit known-tag
//!    enumeration and the `UIntBase128` length fields.
//! 3. Brotli-decompresses the payload (`brotli`), capped at the
//!    declared table sum.
//! 4. Splits the decompressed buffer back into per-table slices.
//! 5. Inverts the WOFF2 `glyf` / `loca` transform when present so the
//!    SFNT we hand back parses with `sigilbuzz::Face`.
//! 6. Stitches an SFNT directory + bodies, padded to 4 bytes.
//!
//! The forward direction (SFNT to WOFF2) lives in `wrap.rs` and
//! `wrap_transform.rs`.
//!
//! Spec: <https://www.w3.org/TR/WOFF2/>

use alloc::vec::Vec;

use crate::error::{Result, WoffError};
use crate::reader::Reader;

mod transform;
mod wrap;
mod wrap_transform;

pub use wrap::{wrap_woff2, wrap_woff2_with_options, WrapOptions};

pub(crate) const WOFF2_SIGNATURE: u32 = 0x774F_4632; // 'wOF2'
pub(crate) const TAG_GLYF: [u8; 4] = *b"glyf";
pub(crate) const TAG_LOCA: [u8; 4] = *b"loca";

/// 5-bit known-tag table from the WOFF2 spec, table 3.
///
/// Index 63 is reserved as the "arbitrary tag follows" marker, so we
/// don't list it here.
pub(crate) const KNOWN_TAGS: [&[u8; 4]; 63] = [
    b"cmap", b"head", b"hhea", b"hmtx", b"maxp", b"name", b"OS/2", b"post", b"cvt ", b"fpgm",
    b"glyf", b"loca", b"prep", b"CFF ", b"VORG", b"EBDT", b"EBLC", b"gasp", b"hdmx", b"kern",
    b"LTSH", b"PCLT", b"VDMX", b"vhea", b"vmtx", b"BASE", b"GDEF", b"GPOS", b"GSUB", b"EBSC",
    b"JSTF", b"MATH", b"CBDT", b"CBLC", b"COLR", b"CPAL", b"SVG ", b"sbix", b"acnt", b"avar",
    b"bdat", b"bloc", b"bsln", b"cvar", b"fdsc", b"feat", b"fmtx", b"fvar", b"gvar", b"hsty",
    b"just", b"lcar", b"ltag", b"mort", b"morx", b"opbd", b"prop", b"trak", b"Zapf", b"Silf",
    b"Glat", b"Gloc", b"Sill",
];

#[derive(Debug, Clone)]
struct DirEntry {
    tag: [u8; 4],
    /// Transform version, bits 6-7 of the flag byte.
    transform_version: u8,
    /// Length of the table after any inverse transform, the size we
    /// expect to land in the final SFNT.
    orig_length: u32,
    /// Length of the *bytes inside the brotli payload* for this table.
    /// Equal to `orig_length` if the table was stored untransformed.
    transform_length: u32,
}

impl DirEntry {
    /// Whether the table sits in the brotli stream in its transformed
    /// form. WOFF2 mandates the `glyf`/`loca` transform with version
    /// 0; version 3 means "stored as raw SFNT bytes". For non-glyf /
    /// non-loca tables only version 0 is defined and means "no
    /// transform".
    fn transformed(&self) -> bool {
        match self.tag {
            TAG_GLYF | TAG_LOCA => self.transform_version != 3,
            _ => false,
        }
    }

    /// Bytes the table occupies inside the decompressed payload.
    fn payload_len(&self) -> u32 {
        if self.transformed() {
            self.transform_length
        } else {
            self.orig_length
        }
    }
}

/// Parses a WOFF2 file and returns the embedded SFNT bytes.
///
/// This requires the `woff2` cargo feature (on by default). With it
/// disabled the function still exists so calling code compiles; it
/// returns `WoffError::Woff2Disabled` at runtime.
///
/// # Errors
///
/// - `BadMagic` if the signature isn't `wOF2` or the reserved field
///   isn't zero.
/// - `UnexpectedEof` / `Malformed` for truncated or self-inconsistent
///   inputs.
/// - `BrotliDecode` if the payload can't be decompressed.
/// - `Unsupported` for transforms or table layouts the current
///   release doesn't yet handle (e.g. composite-glyph instructions
///   are supported, but `hmtx` transform v1 isn't).
pub fn unwrap_woff2(woff2_bytes: &[u8]) -> Result<Vec<u8>> {
    let mut r = Reader::new(woff2_bytes);

    // --- Header ------------------------------------------------------------

    let signature = r.read_u32("WOFF2 signature")?;
    if signature != WOFF2_SIGNATURE {
        return Err(WoffError::BadMagic {
            offset: 0,
            context: "WOFF2 signature",
        });
    }
    let flavor = r.read_u32("WOFF2 flavor")?;
    let _length = r.read_u32("WOFF2 length")?;
    let num_tables_u16 = r.read_u16("WOFF2 numTables")?;
    let num_tables = usize::from(num_tables_u16);
    let reserved = r.read_u16("WOFF2 reserved")?;
    if reserved != 0 {
        return Err(WoffError::BadMagic {
            offset: 14,
            context: "WOFF2 reserved must be zero",
        });
    }
    let _total_sfnt_size = r.read_u32("WOFF2 totalSfntSize")?;
    let total_compressed_size = r.read_u32("WOFF2 totalCompressedSize")? as usize;
    // majorVersion, minorVersion, metaOffset, metaLength,
    // metaOrigLength, privOffset, privLength. None affect the SFNT
    // we rebuild.
    r.skip(2 + 2 + 4 + 4 + 4 + 4 + 4, "WOFF2 header tail")?;

    // --- Directory --------------------------------------------------------

    let mut entries: Vec<DirEntry> = Vec::with_capacity(num_tables);
    for _ in 0..num_tables {
        let flags = r.read_u8("WOFF2 flag byte")?;
        let known = flags & 0x3F;
        let transform_version = (flags >> 6) & 0x03;
        // Index 63 is the only value past the end of the table.
        let tag = match KNOWN_TAGS.get(usize::from(known)) {
            Some(tag) => **tag,
            None => r.read_tag("WOFF2 arbitrary tag")?,
        };
        let orig_length = r.read_uint_base128()?;
        let transform_length = match tag {
            TAG_GLYF | TAG_LOCA if transform_version != 3 => r.read_uint_base128()?,
            _ => orig_length,
        };
        // WOFF2 §4.1: only `glyf`/`loca` define a non-zero
        // transformVersion (0 = transformed, 3 = stored as SFNT). A
        // non-zero version on any other tag means a transform we
        // don't implement (e.g. `hmtx` transformVersion 1). Falling
        // through and copying the bytes verbatim hands a transformed
        // table back as if it were the raw SFNT payload, silently
        // corrupting the output. The crate doc explicitly calls out
        // hmtx v1 as Unsupported; surface that contract here.
        if transform_version != 0 && !matches!(tag, TAG_GLYF | TAG_LOCA) {
            return Err(WoffError::Unsupported {
                context: "WOFF2 transformVersion != 0 on a non-glyf/loca table",
            });
        }
        entries.push(DirEntry {
            tag,
            transform_version,
            orig_length,
            transform_length,
        });
    }

    // --- Brotli payload ---------------------------------------------------

    let payload_offset = r.position();
    let payload = r.read_bytes(total_compressed_size, "WOFF2 brotli payload")?;
    let total_uncompressed: u64 = entries.iter().map(|e| u64::from(e.payload_len())).sum();
    // Reject a declared size that no real font compresses to. The
    // reference decoder applies the same ratio limit. Without it a
    // tiny file could make us reserve and fill gigabytes.
    let plausible_limit = MAX_PLAUSIBLE_COMPRESSION_RATIO.saturating_mul(woff2_bytes.len() as u64);
    if total_uncompressed > plausible_limit {
        return Err(WoffError::Malformed {
            offset: payload_offset,
            context: "declared table sum implausibly large for the file size",
        });
    }
    let Ok(total_uncompressed) = usize::try_from(total_uncompressed) else {
        return Err(WoffError::Malformed {
            offset: payload_offset,
            context: "declared table sum does not fit in memory",
        });
    };
    let decompressed = brotli_decompress(payload, total_uncompressed)?;
    if decompressed.len() < total_uncompressed {
        return Err(WoffError::Malformed {
            offset: r.position(),
            context: "decompressed payload smaller than declared table sum",
        });
    }

    // --- Per-table slicing + inverse transform ----------------------------

    // Collect the final table bytes, keyed by directory order. We
    // need an indirection because the transformed glyf/loca pair is
    // emitted together: loca's bytes are a side product of glyf
    // reconstruction.
    let mut bodies: Vec<Vec<u8>> = Vec::with_capacity(num_tables);

    // Locate any transformed glyf+loca pair so we can run them
    // together; loca is then overwritten when we get to its
    // directory slot.
    let glyf_transformed = entries
        .iter()
        .find(|e| e.tag == TAG_GLYF)
        .is_some_and(DirEntry::transformed);
    let loca_idx = entries.iter().position(|e| e.tag == TAG_LOCA);

    // First pass: copy untransformed bodies verbatim and reconstruct
    // glyf/loca when they're transformed. Table payloads sit back to
    // back in directory order.
    let mut reconstructed_loca: Option<Vec<u8>> = None;
    let mut payload_cursor = 0usize;
    for e in &entries {
        let start = payload_cursor;
        let end = start.saturating_add(e.payload_len() as usize);
        payload_cursor = end;
        let body = decompressed.get(start..end).ok_or(WoffError::Malformed {
            offset: payload_offset,
            context: "table extends past the decompressed payload",
        })?;

        if e.tag == TAG_GLYF && e.transformed() {
            let (new_glyf, new_loca) = transform::reconstruct_glyf_and_loca(body)?;
            bodies.push(new_glyf);
            reconstructed_loca = Some(new_loca);
        } else if e.tag == TAG_LOCA && glyf_transformed {
            // Placeholder; we'll patch it after this loop using
            // reconstructed_loca, since loca's payload_len under
            // transform v0 is *zero* (the spec stores loca as a
            // by-product of glyf, not as its own stream).
            bodies.push(Vec::new());
        } else {
            // Untransformed table: copy as-is.
            if e.transformed() {
                // Future-proofing: any other transform we don't
                // recognize is rejected loudly.
                return Err(WoffError::Unsupported {
                    context: "unknown WOFF2 transform on a non-glyf table",
                });
            }
            bodies.push(body.to_vec());
        }
    }

    // Patch loca with the reconstructed table.
    if let (Some(li), Some(loca)) = (loca_idx, reconstructed_loca) {
        // Spec: when glyf is transformed, loca's `transformLength`
        // must be zero. We don't enforce that here: the loca slot is
        // empty regardless because we initialized it as such.
        if let Some(slot) = bodies.get_mut(li) {
            *slot = loca;
        }
    }

    // --- Stitch SFNT ------------------------------------------------------

    let (search_range, entry_selector, range_shift) = crate::sfnt::search_params(num_tables_u16);
    let header_size = 12 + 16 * num_tables;
    let mut sfnt = Vec::new();
    sfnt.extend_from_slice(&flavor.to_be_bytes());
    sfnt.extend_from_slice(&num_tables_u16.to_be_bytes());
    sfnt.extend_from_slice(&search_range.to_be_bytes());
    sfnt.extend_from_slice(&entry_selector.to_be_bytes());
    sfnt.extend_from_slice(&range_shift.to_be_bytes());

    // Directory. Each body lands at the next 4-byte boundary after the
    // previous one, so every offset is known up front. The SFNT
    // directory stores u32 offsets and lengths, so the rebuilt font
    // must fit in u32.
    let too_large = WoffError::Malformed {
        offset: 0,
        context: "rebuilt SFNT is larger than 4 GiB",
    };
    let mut body_offset = header_size;
    for (e, body) in entries.iter().zip(&bodies) {
        let (Ok(offset), Ok(len)) = (u32::try_from(body_offset), u32::try_from(body.len())) else {
            return Err(too_large);
        };
        sfnt.extend_from_slice(&e.tag);
        sfnt.extend_from_slice(&checksum_table(body).to_be_bytes());
        sfnt.extend_from_slice(&offset.to_be_bytes());
        sfnt.extend_from_slice(&len.to_be_bytes());
        body_offset = body_offset.saturating_add(body.len().saturating_add(3) & !3);
    }
    if u32::try_from(body_offset).is_err() {
        return Err(too_large);
    }
    debug_assert_eq!(sfnt.len(), header_size);

    sfnt.reserve(body_offset - header_size);
    for body in &bodies {
        sfnt.extend_from_slice(body);
        while sfnt.len() % 4 != 0 {
            sfnt.push(0);
        }
    }

    Ok(sfnt)
}

/// Largest accepted ratio between the declared table sum and the WOFF2
/// file size. Real fonts stay far below it. Google's reference decoder
/// rejects files above the same ratio.
const MAX_PLAUSIBLE_COMPRESSION_RATIO: u64 = 100;

/// Brotli-decompresses `input`, refusing to produce more than
/// `expected_len` bytes.
///
/// Brotli can expand a few bytes into gigabytes, so decoding stops as
/// soon as the output would pass the declared table sum.
fn brotli_decompress(input: &[u8], expected_len: usize) -> Result<Vec<u8>> {
    use brotli::BrotliDecompress;
    use std::io::{self, Cursor, Write};

    /// A `Vec` sink that fails instead of growing past `limit`.
    struct CappedSink {
        out: Vec<u8>,
        limit: usize,
        overflowed: bool,
    }

    impl Write for CappedSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if buf.len() > self.limit.saturating_sub(self.out.len()) {
                self.overflowed = true;
                return Err(io::Error::other("brotli output passes the declared size"));
            }
            self.out.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut sink = CappedSink {
        out: Vec::with_capacity(expected_len),
        limit: expected_len,
        overflowed: false,
    };
    let mut reader = Cursor::new(input);
    if BrotliDecompress(&mut reader, &mut sink).is_err() {
        if sink.overflowed {
            return Err(WoffError::Malformed {
                offset: 0,
                context: "decompressed payload larger than declared table sum",
            });
        }
        return Err(WoffError::BrotliDecode {
            context: "BrotliDecompress returned an error",
        });
    }
    Ok(sink.out)
}

/// SFNT table checksum: sum of big-endian u32s, with the table
/// zero-padded to a 4-byte boundary.
fn checksum_table(body: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    let chunks = body.chunks_exact(4);
    let rem = chunks.remainder();
    for c in chunks {
        sum = sum.wrapping_add(u32::from_be_bytes([c[0], c[1], c[2], c[3]]));
    }
    if !rem.is_empty() {
        let mut last = [0u8; 4];
        last[..rem.len()].copy_from_slice(rem);
        sum = sum.wrapping_add(u32::from_be_bytes(last));
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_tag_table_lengths() {
        // Spec table 3 has exactly 63 entries; index 63 is reserved.
        assert_eq!(KNOWN_TAGS.len(), 63);
        assert_eq!(KNOWN_TAGS[10], b"glyf");
        assert_eq!(KNOWN_TAGS[11], b"loca");
    }

    /// Builds a WOFF2 file with one untransformed `name` table that
    /// declares `orig_length` bytes and carries `payload` as its Brotli
    /// stream.
    fn single_table_woff2(orig_length: u8, payload: &[u8]) -> Vec<u8> {
        let mut woff2 = Vec::new();
        woff2.extend_from_slice(&WOFF2_SIGNATURE.to_be_bytes());
        woff2.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // flavor
        woff2.extend_from_slice(&0u32.to_be_bytes()); // length
        woff2.extend_from_slice(&1u16.to_be_bytes()); // numTables
        woff2.extend_from_slice(&0u16.to_be_bytes()); // reserved
        woff2.extend_from_slice(&0u32.to_be_bytes()); // totalSfntSize
        woff2.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
        woff2.extend_from_slice(&[0u8; 24]); // header tail
        woff2.push(5); // known tag 5 = `name`, transformVersion 0
        woff2.push(orig_length); // UIntBase128, single byte
        woff2.extend_from_slice(payload);
        woff2
    }

    fn brotli(raw: &[u8]) -> Vec<u8> {
        let params = brotli::enc::BrotliEncoderParams {
            quality: 1,
            ..Default::default()
        };
        let mut out = Vec::new();
        brotli::BrotliCompress(&mut std::io::Cursor::new(raw), &mut out, &params)
            .expect("brotli compresses");
        out
    }

    #[test]
    fn unwraps_exact_payload() {
        let raw: Vec<u8> = (0u8..16).collect();
        let sfnt = unwrap_woff2(&single_table_woff2(16, &brotli(&raw))).expect("unwraps");
        assert_eq!(&sfnt[12 + 16..], raw.as_slice());
    }

    #[test]
    fn rejects_payload_larger_than_declared() {
        // The table declares 16 bytes but the Brotli stream expands to
        // 8 MiB. The whole stream used to be decoded into memory and
        // the extra bytes silently dropped. Decoding must now stop at
        // the declared size.
        let bomb = brotli(&vec![0u8; 8 << 20]);
        let result = unwrap_woff2(&single_table_woff2(16, &bomb));
        assert!(
            matches!(result, Err(WoffError::Malformed { context, .. }) if context.contains("larger")),
            "got {result:?}"
        );
    }

    #[test]
    fn rejects_payload_smaller_than_declared() {
        let result = unwrap_woff2(&single_table_woff2(16, &brotli(&[1, 2, 3])));
        assert!(matches!(result, Err(WoffError::Malformed { .. })));
    }

    #[test]
    fn rejects_non_zero_transform_version_on_non_glyf_loca() {
        // Build a minimal 1-table WOFF2 whose single directory entry
        // is `cmap` (known tag index 0) with transformVersion=1. The
        // bytes after the directory are irrelevant. The crate must
        // error before attempting Brotli decompression. Without the
        // guard, the entry was treated as untransformed and `cmap`'s
        // transformed payload (which doesn't exist for `cmap`) would
        // be copied verbatim into the output SFNT.
        let mut woff2 = Vec::new();
        woff2.extend_from_slice(&WOFF2_SIGNATURE.to_be_bytes()); // signature
        woff2.extend_from_slice(b"\x00\x01\x00\x00"); // flavor (TrueType)
        woff2.extend_from_slice(&64u32.to_be_bytes()); // length (placeholder)
        woff2.extend_from_slice(&1u16.to_be_bytes()); // numTables
        woff2.extend_from_slice(&0u16.to_be_bytes()); // reserved
        woff2.extend_from_slice(&100u32.to_be_bytes()); // totalSfntSize
        woff2.extend_from_slice(&0u32.to_be_bytes()); // totalCompressedSize
                                                      // header tail: majorVersion (u16), minorVersion (u16),
                                                      // metaOffset (u32), metaLength (u32), metaOrigLength (u32),
                                                      // privOffset (u32), privLength (u32) = 24 bytes.
        woff2.extend_from_slice(&[0u8; 24]);

        // Directory: flags = (transformVersion << 6) | knownTag.
        // knownTag 0 = "cmap"; transformVersion 1.
        let flags: u8 = 1 << 6;
        woff2.push(flags);
        // origLength: UIntBase128 of 16 -> single byte 0x10.
        woff2.push(0x10);

        let result = unwrap_woff2(&woff2);
        assert!(
            matches!(result, Err(WoffError::Unsupported { .. })),
            "expected Unsupported on non-glyf/loca with transformVersion != 0; got {result:?}"
        );
    }
}
