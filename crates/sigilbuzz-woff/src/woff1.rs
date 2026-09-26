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
use crate::sfnt;

const WOFF1_SIGNATURE: u32 = 0x774F_4646; // 'wOFF'
const WOFF1_HEADER_LEN: usize = 44;
const WOFF1_ENTRY_LEN: usize = 20;

/// Knobs for [`wrap_woff1_with_options`].
///
/// Defaults to `deflate_quality = 6`, a balanced
/// compression/throughput trade-off that matches `zlib`'s out-of-the-box
/// behavior and is what most WOFF1 producers (including fontTools)
/// ship by default.
#[derive(Debug, Clone, Copy)]
pub struct WrapWoff1Options {
    /// Zlib compression level, `0..=9`. `0` is store-only; `9` is
    /// maximum compression and slowest. Values above `9` are clamped.
    /// Ignored when the `woff1-deflate` cargo feature is disabled
    /// since the wrapper then always emits uncompressed pass-through
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
    let num_tables_u16 = r.read_u16("WOFF1 numTables")?;
    let num_tables = usize::from(num_tables_u16);
    let reserved = r.read_u16("WOFF1 reserved")?;
    if reserved != 0 {
        return Err(WoffError::BadMagic {
            offset: 14,
            context: "WOFF1 reserved must be zero",
        });
    }
    let _total_sfnt_size = r.read_u32("WOFF1 totalSfntSize")?;
    // majorVersion, minorVersion, metaOffset, metaLength,
    // metaOrigLength, privOffset, privLength, none of which affect
    // the SFNT we rebuild.
    r.skip(4 + 4 + 4 + 4 + 4 + 4, "WOFF1 header tail")?;

    // --- Read directory ----------------------------------------------------

    struct Entry<'a> {
        tag: [u8; 4],
        /// Stored table bytes, `compLength` long.
        body: &'a [u8],
        /// Byte offset of `body` in the WOFF1 file.
        start: usize,
        orig_length: u32,
        orig_checksum: u32,
    }

    let mut entries = Vec::with_capacity(num_tables);
    for _ in 0..num_tables {
        let record_offset = r.position();
        let tag = r.read_tag("WOFF1 table tag")?;
        let offset = r.read_u32("WOFF1 table offset")?;
        let comp_length = r.read_u32("WOFF1 compLength")?;
        let orig_length = r.read_u32("WOFF1 origLength")?;
        let orig_checksum = r.read_u32("WOFF1 origChecksum")?;

        if comp_length > orig_length {
            return Err(WoffError::Malformed {
                offset: record_offset,
                context: "WOFF1 compLength > origLength",
            });
        }
        let start = offset as usize;
        let end = start
            .checked_add(comp_length as usize)
            .ok_or(WoffError::Malformed {
                offset: record_offset,
                context: "table offset + compLength overflows",
            })?;
        let body = woff_bytes.get(start..end).ok_or(WoffError::Malformed {
            offset: record_offset,
            context: "table extends past end of WOFF1 file",
        })?;

        entries.push(Entry {
            tag,
            body,
            start,
            orig_length,
            orig_checksum,
        });
    }

    // --- Validate layout ---------------------------------------------------

    // The spec requires readers to reject table data that overlaps
    // another table or the header and directory. Accepting it would
    // also let one stored body expand once per directory entry that
    // points at it. Empty bodies cover no bytes and cannot overlap.
    let mut ranges: Vec<(usize, usize)> = entries
        .iter()
        .filter(|e| !e.body.is_empty())
        .map(|e| (e.start, e.start + e.body.len()))
        .collect();
    ranges.sort_unstable();
    let mut covered_until = WOFF1_HEADER_LEN + WOFF1_ENTRY_LEN * num_tables;
    for (start, end) in ranges {
        if start < covered_until {
            return Err(WoffError::Malformed {
                offset: start,
                context: "WOFF1 table data overlaps another block",
            });
        }
        covered_until = end;
    }

    // SFNT layout: header (12) + directory (16 * num_tables) +
    // padded table bodies. The SFNT directory addresses tables with
    // u32 offsets, so the whole rebuilt font must fit in u32.
    let header_size = 12 + 16 * num_tables;
    let sfnt_size = entries.iter().fold(header_size as u64, |acc, e| {
        acc + pad4_u64(u64::from(e.orig_length))
    });
    if sfnt_size > u64::from(u32::MAX) {
        return Err(WoffError::Malformed {
            offset: 16,
            context: "WOFF1 tables add up to more than 4 GiB",
        });
    }

    // --- Build SFNT --------------------------------------------------------

    // Compute search params per the SFNT spec, a no-op for our
    // parser but required for spec-compliant readers.
    let (search_range, entry_selector, range_shift) = sfnt::search_params(num_tables_u16);

    // Reserve the declared size, but never more than the stored bytes
    // can inflate to. Deflate cannot expand data by more than 1032 to 1.
    let reserve = entries.iter().fold(header_size as u64, |acc, e| {
        let stored = e.body.len() as u64;
        acc + pad4_u64(u64::from(e.orig_length).min(stored.saturating_mul(1032)))
    });
    let mut sfnt = Vec::with_capacity(usize::try_from(reserve).unwrap_or(0));

    sfnt.extend_from_slice(&flavor.to_be_bytes());
    sfnt.extend_from_slice(&num_tables_u16.to_be_bytes());
    sfnt.extend_from_slice(&search_range.to_be_bytes());
    sfnt.extend_from_slice(&entry_selector.to_be_bytes());
    sfnt.extend_from_slice(&range_shift.to_be_bytes());

    // Directory. Every body comes out exactly `origLength` bytes long
    // (inflate is checked against it below), so each offset is known
    // before any body is written. The size check above keeps every
    // offset within u32.
    let mut body_offset = header_size as u64;
    for e in &entries {
        let offset = u32::try_from(body_offset).map_err(|_| WoffError::Malformed {
            offset: 16,
            context: "WOFF1 tables add up to more than 4 GiB",
        })?;
        sfnt.extend_from_slice(&e.tag);
        sfnt.extend_from_slice(&e.orig_checksum.to_be_bytes());
        sfnt.extend_from_slice(&offset.to_be_bytes());
        sfnt.extend_from_slice(&e.orig_length.to_be_bytes());
        body_offset += pad4_u64(u64::from(e.orig_length));
    }

    // Bodies. Walk the WOFF1 directory in file order so the SFNT we
    // emit is deterministic.
    for e in &entries {
        let body = e.body;
        if body.len() == e.orig_length as usize {
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
    }

    Ok(sfnt)
}

/// Wraps SFNT bytes in a WOFF1 envelope with default options.
///
/// Equivalent to `wrap_woff1_with_options(sfnt_bytes,
/// WrapWoff1Options::default())`. With the `woff1-deflate` feature on
/// (default), each table is emitted compressed when deflate saves
/// space and uncompressed otherwise, the per-table decision the spec
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
    let num_tables_u16 = r.read_u16("SFNT numTables")?;
    let num_tables = usize::from(num_tables_u16);
    r.skip(6, "SFNT searchRange/entrySelector/rangeShift")?;

    // WOFF1 layout: 44-byte header + 20-byte directory * num_tables
    // + padded table bodies. We don't emit metadata or private blocks.
    let header_size = WOFF1_HEADER_LEN + WOFF1_ENTRY_LEN * num_tables;

    // First pass: read and bounds-check every record. The WOFF1 header
    // stores offsets and the file length as u32, so the uncompressed
    // total must fit before any table is compressed. Compressed bodies
    // are never larger than the raw ones, so this also bounds the
    // output. Records may point at shared bytes, so the total can
    // exceed the input size.
    let mut raw_tables = Vec::with_capacity(num_tables);
    let mut padded_total = header_size as u64;
    // totalSfntSize: the SFNT header and directory plus every table
    // padded to four bytes, the size of the font this file unwraps
    // to. It is below `padded_total`, so it fits u32 once that does.
    let mut sfnt_total: u64 = 12 + 16 * num_tables as u64;
    for _ in 0..num_tables {
        let record_offset = r.position();
        let tag = r.read_tag("SFNT table tag")?;
        let checksum = r.read_u32("SFNT table checksum")?;
        let offset = r.read_u32("SFNT table offset")?;
        let length = r.read_u32("SFNT table length")?;

        let end = (offset as usize)
            .checked_add(length as usize)
            .ok_or(WoffError::Malformed {
                offset: record_offset,
                context: "SFNT table offset + length overflows",
            })?;
        let raw = sfnt_bytes
            .get(offset as usize..end)
            .ok_or(WoffError::Malformed {
                offset: record_offset,
                context: "SFNT table extends past end of input",
            })?;
        padded_total += pad4_u64(u64::from(length));
        sfnt_total += pad4_u64(u64::from(length));
        if padded_total > u64::from(u32::MAX) {
            return Err(WoffError::Malformed {
                offset: record_offset,
                context: "SFNT tables add up to more than 4 GiB",
            });
        }
        raw_tables.push((tag, checksum, length, raw));
    }

    struct Entry {
        tag: [u8; 4],
        checksum: u32,
        /// Bytes that go on the wire: either the raw SFNT slice or
        /// a freshly-allocated zlib stream.
        body: Vec<u8>,
        comp_length: u32,
        orig_length: u32,
        /// Offset of `body` in the WOFF1 file.
        offset: u32,
    }

    // Second pass: compress each table and lay out the bodies so the
    // header can carry `length`. Every value fits u32 because of the
    // total checked above.
    let too_large = WoffError::Malformed {
        offset: 0,
        context: "SFNT tables add up to more than 4 GiB",
    };
    let mut entries: Vec<Entry> = Vec::with_capacity(num_tables);
    let mut cursor = header_size;
    for (tag, checksum, orig_length, raw) in raw_tables {
        let (body, comp_length) = compress_body(raw, opts.deflate_quality);
        let Ok(offset) = u32::try_from(cursor) else {
            return Err(too_large);
        };
        cursor = pad4(cursor + body.len());
        entries.push(Entry {
            tag,
            checksum,
            body,
            comp_length,
            orig_length,
            offset,
        });
    }
    let total_length = cursor;
    let Ok(total_length_u32) = u32::try_from(total_length) else {
        return Err(too_large);
    };

    let total_sfnt_size = u32::try_from(sfnt_total).unwrap_or(u32::MAX);

    let mut woff = Vec::with_capacity(total_length);
    woff.extend_from_slice(&WOFF1_SIGNATURE.to_be_bytes());
    woff.extend_from_slice(&flavor.to_be_bytes());
    woff.extend_from_slice(&total_length_u32.to_be_bytes());
    woff.extend_from_slice(&num_tables_u16.to_be_bytes());
    woff.extend_from_slice(&0u16.to_be_bytes()); // reserved
    woff.extend_from_slice(&total_sfnt_size.to_be_bytes());
    woff.extend_from_slice(&0u16.to_be_bytes()); // majorVersion
    woff.extend_from_slice(&0u16.to_be_bytes()); // minorVersion
    woff.extend_from_slice(&0u32.to_be_bytes()); // metaOffset
    woff.extend_from_slice(&0u32.to_be_bytes()); // metaLength
    woff.extend_from_slice(&0u32.to_be_bytes()); // metaOrigLength
    woff.extend_from_slice(&0u32.to_be_bytes()); // privOffset
    woff.extend_from_slice(&0u32.to_be_bytes()); // privLength

    for e in &entries {
        woff.extend_from_slice(&e.tag);
        woff.extend_from_slice(&e.offset.to_be_bytes());
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
/// returns `(raw.to_vec(), raw.len())`. Callers pass bodies no longer
/// than a u32 table length, so both lengths fit u32.
fn compress_body(raw: &[u8], quality: u8) -> (Vec<u8>, u32) {
    #[cfg(feature = "woff1-deflate")]
    {
        // Compress, then keep the result only if it's strictly
        // smaller than the raw body. The spec uses
        // `compLength == origLength` as the "uncompressed" sentinel,
        // so equal-size compressed payloads provide no signal and
        // would just cost decode time.
        let compressed = crate::zlib::deflate_zlib(raw, quality);
        if compressed.len() < raw.len() {
            let comp_len = compressed.len() as u32;
            return (compressed, comp_len);
        }
    }
    #[cfg(not(feature = "woff1-deflate"))]
    let _ = quality;
    let len = raw.len() as u32;
    (raw.to_vec(), len)
}

const fn pad4(n: usize) -> usize {
    (n + 3) & !3
}

/// `pad4` over u64, for size totals built from u32 table lengths.
const fn pad4_u64(n: u64) -> u64 {
    (n + 3) & !3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_params_match_spec() {
        // Examples from the OpenType spec.
        assert_eq!(sfnt::search_params(0), (0, 0, 0));
        assert_eq!(sfnt::search_params(1), (16, 0, 0));
        assert_eq!(sfnt::search_params(9), (128, 3, 16));
        assert_eq!(sfnt::search_params(14), (128, 3, 96));
    }

    #[cfg(feature = "woff1-deflate")]
    #[test]
    fn compress_body_keeps_raw_when_compression_grows_it() {
        // 4 bytes of pseudo-random data. Deflate framing dwarfs the
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
