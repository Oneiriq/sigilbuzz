//! Zlib (RFC 1950) codec helpers used by the WOFF1 wrap and unwrap paths.
//!
//! WOFF1 stores per-table bodies as raw zlib streams: a 2-byte zlib
//! header, a deflate (RFC 1951) payload, and a trailing 4-byte adler32
//! over the *uncompressed* bytes. The format is not concatenated /
//! framed across tables. Every directory entry whose
//! `compLength < origLength` carries its own complete zlib stream.
//!
//! Both directions live behind the `woff1-deflate` cargo feature so
//! consumers who only ever traffic in uncompressed-pass-through WOFF1
//! files do not pay the `miniz_oxide` runtime dep.
//!
//! The helpers translate `miniz_oxide` errors and quality knobs into
//! the crate's [`WoffError`] vocabulary so the WOFF1 module above
//! stays codec-agnostic.

#![cfg(feature = "woff1-deflate")]

use alloc::vec::Vec;

use crate::error::{Result, WoffError};

/// Decompresses a complete zlib stream and validates the recovered
/// length against the directory entry's `origLength`.
///
/// `expected_len` is the WOFF1 directory's `origLength` for the table.
/// We refuse to return a buffer whose length doesn't match. A
/// truncated or over-long inflate is a malformed WOFF1 file by the
/// spec's "compressed table data must be a valid compressed stream"
/// clause, and downstream SFNT parsing would silently see a different
/// table than the directory advertises.
///
/// Inflation stops one byte past `expected_len`, so a small stream
/// that expands to far more data than the directory declares never
/// allocates more than the declared size.
///
/// # Errors
///
/// - `Malformed { context: "zlib decompress failed" }` when the
///   underlying inflate routine rejects the stream (bad header, bad
///   block, truncated payload, bad adler32).
/// - `Malformed { context: "zlib output exceeds origLength" }` when
///   the stream holds more than `expected_len + 1` bytes.
/// - `Malformed { context: "zlib origLength mismatch" }` when inflate
///   succeeds but the recovered length disagrees with `expected_len`.
pub(crate) fn inflate_zlib(input: &[u8], expected_len: usize) -> Result<Vec<u8>> {
    use miniz_oxide::inflate::{decompress_to_vec_zlib_with_limit, TINFLStatus};

    let limit = expected_len.saturating_add(1);
    let out = decompress_to_vec_zlib_with_limit(input, limit).map_err(|e| {
        let context = if e.status == TINFLStatus::HasMoreOutput {
            "WOFF1 zlib output exceeds origLength"
        } else {
            "WOFF1 zlib decompress failed"
        };
        WoffError::Malformed { offset: 0, context }
    })?;
    if out.len() != expected_len {
        return Err(WoffError::Malformed {
            offset: 0,
            context: "WOFF1 zlib origLength mismatch",
        });
    }
    Ok(out)
}

/// Compresses a single table body as a zlib stream at the given level.
///
/// `level` follows the standard zlib 0..=9 scale: 0 is store-only,
/// 1 is fastest, 9 is maximum. `miniz_oxide`'s
/// `compress_to_vec_zlib` clamps internally; we mirror that behavior
/// here so callers can pass any `u8`.
pub(crate) fn deflate_zlib(input: &[u8], level: u8) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(input, level.min(10))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_short_input() {
        // 64 bytes of structured input, short enough that compression
        // may not save space, but the codec must still round-trip.
        let raw: Vec<u8> = (0u8..64).collect();
        let z = deflate_zlib(&raw, 6);
        let back = inflate_zlib(&z, raw.len()).expect("round-trips");
        assert_eq!(back, raw);
    }

    #[test]
    fn round_trip_compressible_input() {
        // 4 KiB of 'A' should compress dramatically.
        let raw = vec![b'A'; 4096];
        let z = deflate_zlib(&raw, 6);
        assert!(
            z.len() < raw.len() / 8,
            "homogeneous input should compress 8x or better, got {} -> {}",
            raw.len(),
            z.len()
        );
        let back = inflate_zlib(&z, raw.len()).expect("round-trips");
        assert_eq!(back, raw);
    }

    #[test]
    fn inflate_rejects_garbage() {
        let garbage = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x00];
        assert!(matches!(
            inflate_zlib(&garbage, 4),
            Err(WoffError::Malformed { .. })
        ));
    }

    #[test]
    fn inflate_rejects_length_mismatch() {
        // Compress 32 bytes, claim 33 on the way back. The helper
        // must catch the discrepancy and refuse the buffer.
        let raw = vec![0xA5u8; 32];
        let z = deflate_zlib(&raw, 6);
        let err = inflate_zlib(&z, raw.len() + 1).unwrap_err();
        assert!(
            matches!(err, WoffError::Malformed { context, .. } if context.contains("origLength")),
            "expected origLength mismatch, got {err:?}"
        );
    }

    #[test]
    fn inflate_stops_at_declared_length() {
        // 8 MiB of zeros compresses to a few KiB. With a declared
        // origLength of 16 the helper used to inflate the whole 8 MiB
        // before it compared lengths. It must stop at the limit.
        let raw = vec![0u8; 8 << 20];
        let z = deflate_zlib(&raw, 6);
        let err = inflate_zlib(&z, 16).unwrap_err();
        assert!(
            matches!(err, WoffError::Malformed { context, .. } if context.contains("exceeds")),
            "expected the output limit to trip, got {err:?}"
        );
    }

    #[test]
    fn inflate_accepts_exact_length() {
        // The limit sits one byte past origLength, so a stream that
        // fills origLength exactly still round-trips.
        let raw = vec![7u8; 4096];
        let z = deflate_zlib(&raw, 9);
        assert_eq!(inflate_zlib(&z, raw.len()).expect("inflates"), raw);
        assert!(inflate_zlib(&z, raw.len() - 1).is_err());
    }

    #[test]
    fn inflate_rejects_truncated_stream() {
        let raw = vec![0x55u8; 256];
        let z = deflate_zlib(&raw, 6);
        // Lop off the adler32 trailer.
        let truncated = &z[..z.len() - 4];
        assert!(matches!(
            inflate_zlib(truncated, raw.len()),
            Err(WoffError::Malformed { .. })
        ));
    }
}
