//! WOFF1 wrap / unwrap.
//!
//! WOFF1 is a thin SFNT envelope: a 44-byte header, one 20-byte
//! directory record per table, optional zlib-compressed table bodies,
//! and optional metadata / private blocks. We support both directions,
//! including the zlib path when the `woff1-deflate` cargo feature is
//! enabled (the default).
//!
//! Per the spec, a table is uncompressed when `compLength == origLength`
//! and zlib-compressed otherwise. The zlib stream is RFC 1950 (header
//! + deflate payload + adler32 trailer) over the raw table bytes.
//!
//! Spec: <https://www.w3.org/TR/WOFF/>

use alloc::vec::Vec;

use crate::error::{Result, WoffError};
use crate::reader::Reader;

const WOFF1_SIGNATURE: u32 = 0x774F_4646; // 'wOFF'

/// Knobs for [`wrap_woff1_with_options`].
///
/// Defaults to `deflate_quality = 6` — a balanced
/// compression/throughput trade-off that matches `zlib`'s out-of-the-box
/// behaviour and is what most WOFF1 producers (including fontTools)
/// ship by default.
#[derive(Debug, Clone, Copy)]
pub struct WrapWoff1Options {
    /// Zlib compression level, `0..=9`. `0` is store-only; `9` is
    /// maximum compression and slowest. Values above `9` are clamped.
    /// Ignored when the `woff1-deflate` cargo feature is disabled
    /// — the wrapper then always emits uncompressed pass-through
    /// bodies.
    pub deflate_quality: u8,
}

impl Default for WrapWoff1Options {
    fn default() -> Self {
        Self { deflate_quality: 6 }
    }
}

/// Parses a WOFF1 file and returns the embedded SFNT bytes.
///
/// The output is exactly the font program a `Face::parse_bytes`
/// caller expects: an SFNT directory (12-byte header + 16-byte
/// records, all 4-byte aligned) followed by each table body.
///
/// Tables stored zlib-compressed (`compLength < origLength`) are
/// inflated when the `woff1-deflate` feature is on. With the feature
/// disabled, compressed tables return [`WoffError::Unsupported`].
///
/// # Errors
///
/// - `BadMagic` if the signature isn't `wOFF`.
/// - `UnexpectedEof` / `Malformed` for truncated or self-inconsistent
///   inputs (including zlib streams that fail to inflate or whose
///   recovered length disagrees with `origLength`).
/// - `Unsupported` if the build was made with `--no-default-features`
///   (or otherwise without `woff1-deflate`) and the input contains a
///   zlib-compressed table.
pub fn unwrap_woff1(woff_bytes: &[u8]) -> Result<Vec<u8>> {
    let mut r = Reader::new(woff_bytes);

    let signature = r.read_u32("WOFF1 signature")?;
    if signature != WOFF1_SIGNATURE {
        return Err(WoffError::BadMagic {
            offset: 0,
            context: "WOFF1 signature",
        });
    }
    let flavor = r.read_u32("WOFF1 flavor")?;
    let _length = r.read_u32("WOFF1 length")?;
    let num_tables = r.read_u16("WOFF1 numTables")? as usize;
    let reserved = r.read_u16("WOFF1 reserved")?;
    if reserved != 0 {
        return Err(WoffError::BadMagic {
            offset: 14,
            context: "WOFF1 reserved must be zero",
        });
    }
    let _total_sfnt_size = r.read_u32("WOFF1 totalSfntSize")?;
    // majorVersion, minorVersion, metaOffset, metaLength,
    // metaOrigLength, privOffset, privLength — none of which affect
    // the SFNT we rebuild.
    r.skip(4 + 4 + 4 + 4 + 4 + 4, "WOFF1 header tail")?;

    // --- Read directory ----------------------------------------------------

    struct Entry {
        tag: [u8; 4],
        offset: u32,
        comp_length: u32,
        orig_length: u32,
        orig_checksum: u32,
    }

    let mut entries = Vec::with_capacity(num_tables);
    for _ in 0..num_tables {
        let tag = r.read_tag("WOFF1 table tag")?;
        let offset = r.read_u32("WOFF1 table offset")?;
        let comp_length = r.read_u32("WOFF1 compLength")?;
        let orig_length = r.read_u32("WOFF1 origLength")?;
        let orig_checksum = r.read_u32("WOFF1 origChecksum")?;

        if comp_length > orig_length {
            return Err(WoffError::Malformed {
                offset: r.position() - 16,
                context: "WOFF1 compLength > origLength",
            });
        }
        // Bounds-check the body now so the body-copy loop below can
        // unwrap without surprises.
        let end =
            (offset as usize)
                .checked_add(comp_length as usize)
                .ok_or(WoffError::Malformed {
                    offset: r.position() - 16,
                    context: "table offset + compLength overflows",
                })?;
        if end > woff_bytes.len() {
            return Err(WoffError::Malformed {
                offset: r.position() - 16,
                context: "table extends past end of WOFF1 file",
            });
        }

        entries.push(Entry {
            tag,
            offset,
            comp_length,
            orig_length,
            orig_checksum,
        });
    }

    // --- Build SFNT --------------------------------------------------------

    // Compute search params per the SFNT spec — a no-op for our
    // parser but required for spec-compliant readers.
    let (search_range, entry_selector, range_shift) = sfnt_search_params(num_tables as u16);

    // SFNT layout: header (12) + directory (16 * num_tables) +
    // padded table bodies. Each table body must be 4-byte aligned;
    // the offset in the SFNT directory points at the table itself.
    let header_size = 12 + 16 * num_tables;
    let mut sfnt = Vec::with_capacity(
        header_size
            + entries
                .iter()
                .map(|e| pad4(e.orig_length as usize))
                .sum::<usize>(),
    );

    sfnt.extend_from_slice(&flavor.to_be_bytes());
    sfnt.extend_from_slice(&(num_tables as u16).to_be_bytes());
    sfnt.extend_from_slice(&search_range.to_be_bytes());
    sfnt.extend_from_slice(&entry_selector.to_be_bytes());
    sfnt.extend_from_slice(&range_shift.to_be_bytes());

    // Directory placeholder; we'll overwrite offsets as we go.
    let dir_start = sfnt.len();
    sfnt.resize(dir_start + 16 * num_tables, 0);

    // Bodies. Walk the WOFF1 directory in file order so the SFNT we
    // emit is deterministic.
    for (i, e) in entries.iter().enumerate() {
        let body_offset = sfnt.len() as u32;

        let body = &woff_bytes[e.offset as usize..e.offset as usize + e.comp_length as usize];
        if e.comp_length == e.orig_length {
            // Uncompressed pass-through.
            sfnt.extend_from_slice(body);
        } else {
            // zlib-compressed.
            #[cfg(feature = "woff1-deflate")]
            {
                let inflated = crate::zlib::inflate_zlib(body, e.orig_length as usize)?;
                sfnt.extend_from_slice(&inflated);
            }
            #[cfg(not(feature = "woff1-deflate"))]
            {
                return Err(WoffError::Unsupported {
                    context: "WOFF1 zlib-compressed tables (woff1-deflate feature disabled)",
                });
            }
        }
        // Pad table to 4-byte boundary.
        while sfnt.len() % 4 != 0 {
            sfnt.push(0);
        }

        // Write the directory record.
        let rec = dir_start + i * 16;
        sfnt[rec..rec + 4].copy_from_slice(&e.tag);
        sfnt[rec + 4..rec + 8].copy_from_slice(&e.orig_checksum.to_be_bytes());
        sfnt[rec + 8..rec + 12].copy_from_slice(&body_offset.to_be_bytes());
        sfnt[rec + 12..rec + 16].copy_from_slice(&e.orig_length.to_be_bytes());
    }

    Ok(sfnt)
}

/// Wraps SFNT bytes in a WOFF1 envelope with default options.
///
/// Equivalent to `wrap_woff1_with_options(sfnt_bytes,
/// WrapWoff1Options::default())`. With the `woff1-deflate` feature on
/// (default), each table is emitted compressed when deflate saves
/// space and uncompressed otherwise — the per-table decision the spec
/// expects. With the feature disabled, every table is emitted
/// uncompressed (`compLength == origLength`).
///
/// # Errors
///
/// - `UnexpectedEof` / `Malformed` if the input isn't a valid SFNT
///   directory.
pub fn wrap_woff1(sfnt_bytes: &[u8]) -> Result<Vec<u8>> {
    wrap_woff1_with_options(sfnt_bytes, WrapWoff1Options::default())
}

/// Wraps SFNT bytes in a WOFF1 envelope using caller-supplied
/// [`WrapWoff1Options`].
///
/// Per-table compression decision: a table is stored zlib-compressed
/// only when the compressed body is **strictly smaller** than the raw
/// body. For tiny or already-incompressible tables the cost of the
/// 6-byte zlib framing (2-byte header + 4-byte adler32) plus the
/// deflate block overhead would make compressed > raw; the wrapper
/// keeps such tables uncompressed (`compLength == origLength`).
///
/// # Errors
///
/// Same as [`wrap_woff1`].
pub fn wrap_woff1_with_options(sfnt_bytes: &[u8], opts: WrapWoff1Options) -> Result<Vec<u8>> {
    let mut r = Reader::new(sfnt_bytes);

    let flavor = r.read_u32("SFNT version")?;
    let num_tables = r.read_u16("SFNT numTables")? as usize;
    r.skip(6, "SFNT searchRange/entrySelector/rangeShift")?;

    struct Entry {
        tag: [u8; 4],
        checksum: u32,
        /// Bytes that go on the wire — either the raw SFNT slice or
        /// a freshly-allocated zlib stream.
        body: Vec<u8>,
        comp_length: u32,
        orig_length: u32,
    }

    let mut entries: Vec<Entry> = Vec::with_capacity(num_tables);
    for _ in 0..num_tables {
        let tag = r.read_tag("SFNT table tag")?;
        let checksum = r.read_u32("SFNT table checksum")?;
        let offset = r.read_u32("SFNT table offset")?;
        let length = r.read_u32("SFNT table length")?;

        let end = (offset as usize)
            .checked_add(length as usize)
            .ok_or(WoffError::Malformed {
                offset: r.position() - 16,
                context: "SFNT table offset + length overflows",
            })?;
        if end > sfnt_bytes.len() {
            return Err(WoffError::Malformed {
                offset: r.position() - 16,
                context: "SFNT table extends past end of input",
            });
        }
        let raw = &sfnt_bytes[offset as usize..end];

        let (body, comp_length) = compress_body(raw, opts.deflate_quality);

        entries.push(Entry {
            tag,
            checksum,
            body,
            comp_length,
            orig_length: length,
        });
    }

    // WOFF1 layout: 44-byte header + 20-byte directory * num_tables
    // + padded table bodies. We don't emit metadata or private blocks.
    let header_size = 44 + 20 * num_tables;
    // Compute total file size and per-body offsets up front so the
    // header can carry `length`.
    let mut body_offsets = Vec::with_capacity(num_tables);
    let mut cursor = header_size;
    for e in &entries {
        body_offsets.push(cursor as u32);
        cursor = pad4(cursor + e.comp_length as usize);
    }
    let total_length = cursor;

    let mut woff = Vec::with_capacity(total_length);
    woff.extend_from_slice(&WOFF1_SIGNATURE.to_be_bytes());
    woff.extend_from_slice(&flavor.to_be_bytes());
    woff.extend_from_slice(&(total_length as u32).to_be_bytes());
    woff.extend_from_slice(&(num_tables as u16).to_be_bytes());
    woff.extend_from_slice(&0u16.to_be_bytes()); // reserved
    woff.extend_from_slice(&(sfnt_bytes.len() as u32).to_be_bytes()); // totalSfntSize (approx)
    woff.extend_from_slice(&0u16.to_be_bytes()); // majorVersion
    woff.extend_from_slice(&0u16.to_be_bytes()); // minorVersion
    woff.extend_from_slice(&0u32.to_be_bytes()); // metaOffset
    woff.extend_from_slice(&0u32.to_be_bytes()); // metaLength
    woff.extend_from_slice(&0u32.to_be_bytes()); // metaOrigLength
    woff.extend_from_slice(&0u32.to_be_bytes()); // privOffset
    woff.extend_from_slice(&0u32.to_be_bytes()); // privLength

    for (i, e) in entries.iter().enumerate() {
        woff.extend_from_slice(&e.tag);
        woff.extend_from_slice(&body_offsets[i].to_be_bytes());
        woff.extend_from_slice(&e.comp_length.to_be_bytes());
        woff.extend_from_slice(&e.orig_length.to_be_bytes());
        woff.extend_from_slice(&e.checksum.to_be_bytes());
    }

    debug_assert_eq!(woff.len(), header_size);

    for e in &entries {
        woff.extend_from_slice(&e.body);
        while woff.len() % 4 != 0 {
            woff.push(0);
        }
    }

    debug_assert_eq!(woff.len(), total_length);
    Ok(woff)
}

/// Pick the smaller of `(zlib(raw), raw)` for a single table body.
///
/// Returns the bytes to write and the matching `compLength`. With the
/// feature off, the function is a thin pass-through that always
/// returns `(raw.to_vec(), raw.len())`.
fn compress_body(raw: &[u8], _quality: u8) -> (Vec<u8>, u32) {
    #[cfg(feature = "woff1-deflate")]
    {
        // Compress, then keep the result only if it's strictly
        // smaller than the raw body — the spec uses
        // `compLength == origLength` as the "uncompressed" sentinel,
        // so equal-size compressed payloads provide no signal and
        // would just cost decode time.
        let compressed = crate::zlib::deflate_zlib(raw, _quality);
        if compressed.len() < raw.len() {
            let comp_len = compressed.len() as u32;
            return (compressed, comp_len);
        }
    }
    let len = raw.len() as u32;
    (raw.to_vec(), len)
}

const fn pad4(n: usize) -> usize {
    (n + 3) & !3
}

/// SFNT search-param triple (`searchRange`, `entrySelector`,
/// `rangeShift`). All deterministic functions of `num_tables`.
fn sfnt_search_params(num_tables: u16) -> (u16, u16, u16) {
    if num_tables == 0 {
        return (0, 0, 0);
    }
    // Largest power of two <= num_tables, scaled by record size 16.
    let mut entry_selector: u16 = 0;
    let mut pow2: u16 = 1;
    while pow2 * 2 <= num_tables {
        pow2 *= 2;
        entry_selector += 1;
    }
    let search_range = pow2 * 16;
    let range_shift = num_tables * 16 - search_range;
    (search_range, entry_selector, range_shift)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_params_match_spec() {
        // Examples from the OpenType spec.
        assert_eq!(sfnt_search_params(0), (0, 0, 0));
        assert_eq!(sfnt_search_params(1), (16, 0, 0));
        assert_eq!(sfnt_search_params(9), (128, 3, 16));
        assert_eq!(sfnt_search_params(14), (128, 3, 96));
    }

    #[cfg(feature = "woff1-deflate")]
    #[test]
    fn compress_body_keeps_raw_when_compression_grows_it() {
        // 4 bytes of pseudo-random data — deflate framing dwarfs the
        // payload, so the wrapper must keep the body uncompressed.
        let raw = [0x00, 0xFF, 0x37, 0x42];
        let (out, comp_len) = compress_body(&raw, 6);
        assert_eq!(out.as_slice(), raw.as_slice());
        assert_eq!(comp_len as usize, raw.len());
    }

    #[cfg(feature = "woff1-deflate")]
    #[test]
    fn compress_body_uses_zlib_when_it_saves_space() {
        let raw = vec![b'Z'; 1024];
        let (out, comp_len) = compress_body(&raw, 6);
        assert!(out.len() < raw.len(), "expected compression to win");
        assert_eq!(comp_len as usize, out.len());
        // Sanity-check: the first two bytes are a valid zlib header
        // (CMF & 0x0F == 8 -> deflate; FLG check bits valid).
        assert_eq!(out[0] & 0x0F, 0x08);
        let cmf_flg = ((out[0] as u16) << 8) | out[1] as u16;
        assert_eq!(cmf_flg % 31, 0);
    }
}
