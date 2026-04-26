//! Minimal PNG encoder.
//!
//! The companion to [`crate::decode_png`]. This commit lands the
//! shared primitives — CRC32 and chunk writer — and the next commits
//! layer on the [`ColorPixmap`] and [`Pixmap`] entry points.
//!
//! ```text
//!   raw scanlines (filter byte || row payload, repeated)
//!         │  miniz_oxide zlib (level 6)
//!         ▼
//!   IDAT body
//!         │
//!         ▼
//!   signature || IHDR || IDAT || IEND
//! ```
//!
//! # Subset of the PNG spec implemented
//!
//! - **Bit depth 8 only.** No 1/2/4/16-bit depths.
//! - **Color types 0 (gray) and 6 (RGBA) only.**
//! - **Filter type 0 (None) on every row.** Deterministic, fast,
//!   slightly larger files than a smart filter heuristic would yield.
//! - **No interlacing.** Adam7 is decoder-side only.
//! - **No ancillary chunks.** No `gAMA` / `sRGB` / `pHYs` /
//!   `tEXt` — bytes carry the colour they were given.

use alloc::vec::Vec;

/// PNG signature bytes — every PNG starts with this 8-byte header.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// Append a single PNG chunk (length, type, data, CRC) to `out`.
///
/// The CRC32 is computed over the chunk **type and data** together —
/// the length prefix is excluded, per the PNG spec.
fn write_chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    // u32 BE length.
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);

    let mut crc = Crc32::new();
    crc.update(&kind);
    crc.update(data);
    out.extend_from_slice(&crc.finalize().to_be_bytes());
}

// ---------------------------------------------------------------------------
// CRC32 (ISO 3309), the standard PNG flavour.
// ---------------------------------------------------------------------------

/// Streaming CRC32 with the PNG-standard polynomial (`0xEDB88320`,
/// reflected `0x04C11DB7`). The table is built at compile time via a
/// `const fn` — no `std::sync::Once`, no allocator touch, no thread
/// synchronisation needed (pure function of the polynomial).
#[derive(Debug, Clone, Copy)]
struct Crc32 {
    state: u32,
}

impl Crc32 {
    const fn new() -> Self {
        Self { state: 0xFFFF_FFFF }
    }

    fn update(&mut self, bytes: &[u8]) {
        let table = crc32_table();
        let mut s = self.state;
        for &b in bytes {
            let idx = ((s ^ b as u32) & 0xFF) as usize;
            s = (s >> 8) ^ table[idx];
        }
        self.state = s;
    }

    const fn finalize(self) -> u32 {
        self.state ^ 0xFFFF_FFFF
    }
}

/// Build the 256-entry CRC32 lookup table. `const fn` so the table is
/// available at compile time — no runtime initialisation cost and no
/// static-mut data. The polynomial constant `0xEDB88320` is the
/// reflected form of the ISO 3309 polynomial, which is what the PNG
/// spec uses (Section 5.5).
const fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0u32;
    while n < 256 {
        let mut c = n;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n as usize] = c;
        n += 1;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_known_iend() {
        // The IEND chunk has type "IEND" and zero-length data, so its
        // CRC over `b"IEND"` is the well-known constant 0xAE426082.
        let mut c = Crc32::new();
        c.update(b"IEND");
        assert_eq!(c.finalize(), 0xAE42_6082);
    }

    #[test]
    fn crc32_empty_input_finalizes_to_zero() {
        // CRC over no bytes: state 0xFFFFFFFF XOR 0xFFFFFFFF = 0.
        let c = Crc32::new();
        assert_eq!(c.finalize(), 0);
    }

    #[test]
    fn crc32_known_iend_via_chunk_writer() {
        let mut buf = Vec::new();
        write_chunk(&mut buf, *b"IEND", &[]);
        // length(0) + "IEND" + CRC(IEND) = 4 + 4 + 4 = 12 bytes.
        assert_eq!(buf.len(), 12);
        assert_eq!(&buf[..4], &[0, 0, 0, 0]);
        assert_eq!(&buf[4..8], b"IEND");
        assert_eq!(&buf[8..12], &0xAE42_6082u32.to_be_bytes());
    }

    #[test]
    fn write_chunk_emits_length_type_data_crc() {
        // A 13-byte IHDR-shaped payload: width=1, height=1, all zeros
        // for the rest. We only verify the framing here, not the IHDR
        // semantics — that's the next commit.
        let mut buf = Vec::new();
        let payload = [0u8; 13];
        write_chunk(&mut buf, *b"IHDR", &payload);
        assert_eq!(buf.len(), 4 + 4 + 13 + 4);
        // Length BE.
        assert_eq!(&buf[..4], &13u32.to_be_bytes());
        // Type.
        assert_eq!(&buf[4..8], b"IHDR");
        // Payload.
        assert_eq!(&buf[8..21], &payload);
        // CRC matches a fresh recomputation over type + payload.
        let mut c = Crc32::new();
        c.update(b"IHDR");
        c.update(&payload);
        assert_eq!(&buf[21..25], &c.finalize().to_be_bytes());
    }

    #[test]
    fn png_signature_matches_spec() {
        assert_eq!(
            PNG_SIGNATURE,
            [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
        );
    }
}
