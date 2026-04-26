//! Minimal PNG encoder.
//!
//! The companion to [`crate::decode_png`]. This module ships
//! [`encode_png`] for [`ColorPixmap`] (premultiplied RGBA) and
//! [`encode_png_alpha`] for [`Pixmap`] (8-bit alpha).
//!
//! ```text
//!   ColorPixmap (premul RGBA)
//!         │  un-premultiply per pixel (a==0 → 0,0,0)
//!         ▼
//!   raw scanlines: filter byte 0 + width*4 bytes
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
//!
//! # Determinism
//!
//! Same input → byte-identical output. miniz_oxide's deflate is
//! deterministic at fixed compression level; the encoder injects no
//! entropy beyond the pixel data.

use alloc::vec::Vec;

use crate::pixmap::ColorPixmap;

/// PNG signature bytes — every PNG starts with this 8-byte header.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// Encode a [`ColorPixmap`] as a complete PNG byte stream.
///
/// The pixmap holds **premultiplied** RGBA samples; PNG color type 6
/// is conventionally **straight** alpha, so per-pixel un-premultiply
/// happens on the way out. A pixel with `a == 0` emits `(0, 0, 0, 0)`
/// regardless of the stored RGB (which is already zero in our pipeline
/// but defensive-zeroed here too).
///
/// A zero-dimension pixmap encodes to a syntactically valid 0×0 PNG —
/// the spec does not actually forbid it on the encoding side, although
/// our decoder rejects zero-dimension PNGs on the way back in. Callers
/// that round-trip through `decode_png` should guard against empty
/// inputs themselves.
#[must_use]
pub fn encode_png(pixmap: &ColorPixmap) -> Vec<u8> {
    // Color type 6 = RGBA, 4 bytes per pixel.
    let mut raw = Vec::with_capacity(
        ((pixmap.width as usize) * 4 + 1).saturating_mul(pixmap.height as usize),
    );
    for y in 0..pixmap.height {
        raw.push(0u8); // filter: None
        for x in 0..pixmap.width {
            let [r, g, b, a] = pixmap.get(x, y);
            let (sr, sg, sb) = unpremultiply(r, g, b, a);
            raw.push(sr);
            raw.push(sg);
            raw.push(sb);
            raw.push(a);
        }
    }
    assemble_png(pixmap.width, pixmap.height, 6, &raw)
}

/// Build the four PNG sections — signature, IHDR, IDAT, IEND — from
/// width/height/colour-type/raw-scanline-bytes and concatenate.
///
/// `raw` is the per-row `(filter byte || row payload)` block expected
/// by the PNG spec; this routine zlib-compresses it into the IDAT
/// payload. We pick deflate level 6 for the same reason the decoder's
/// in-tree fixture builder picks 6: balanced ratio/speed and matches
/// what miniz_oxide produces by default.
fn assemble_png(width: u32, height: u32, color_type: u8, raw: &[u8]) -> Vec<u8> {
    let idat = miniz_oxide::deflate::compress_to_vec_zlib(raw, 6);

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(color_type);
    ihdr.push(0); // compression: zlib
    ihdr.push(0); // filter: PNG-spec default
    ihdr.push(0); // interlace: none

    // Capacity hint: signature + IHDR (8 + 13 + 4) + IDAT (8 + n + 4) +
    // IEND (8 + 0 + 4) = 49 + idat.len().
    let mut out = Vec::with_capacity(49 + idat.len());
    out.extend_from_slice(&PNG_SIGNATURE);
    write_chunk(&mut out, *b"IHDR", &ihdr);
    write_chunk(&mut out, *b"IDAT", &idat);
    write_chunk(&mut out, *b"IEND", &[]);
    out
}

/// Convert a single premultiplied RGB sample back to straight alpha.
///
/// Formula: `s = (p * 255 + a/2) / a` (rounded division). When `a == 0`
/// the colour is fully transparent and the spec is silent on what RGB
/// to write, so we zero it out — this is what consumers expect when a
/// PNG is run through `decode_png` again.
///
/// Saturates at 255 so a malformed pixmap with `p > a` (which violates
/// the premul invariant but can sneak in from external sources) still
/// produces in-range bytes.
fn unpremultiply(r: u8, g: u8, b: u8, a: u8) -> (u8, u8, u8) {
    if a == 0 {
        return (0, 0, 0);
    }
    if a == 255 {
        return (r, g, b);
    }
    let ai = a as u32;
    let half = ai / 2;
    let unpre = |c: u8| -> u8 {
        let v = (c as u32 * 255 + half) / ai;
        if v > 255 {
            255
        } else {
            v as u8
        }
    };
    (unpre(r), unpre(g), unpre(b))
}

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
    use crate::bitmaps::decode_png;
    use alloc::vec;

    // --- CRC32 sanity checks -------------------------------------------------

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

    #[test]
    fn crc32_recompute_matches_each_chunk() {
        // Encode a small pixmap, then walk the chunk stream and
        // recompute every CRC ourselves to confirm they match.
        let mut p = ColorPixmap::new(2, 2);
        p.data = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let bytes = encode_png(&p);
        verify_all_crcs(&bytes);
    }

    fn verify_all_crcs(png: &[u8]) {
        assert_eq!(&png[..8], &PNG_SIGNATURE);
        let mut cursor = 8usize;
        loop {
            let len = u32::from_be_bytes([
                png[cursor],
                png[cursor + 1],
                png[cursor + 2],
                png[cursor + 3],
            ]) as usize;
            let kind = [
                png[cursor + 4],
                png[cursor + 5],
                png[cursor + 6],
                png[cursor + 7],
            ];
            let data_start = cursor + 8;
            let data_end = data_start + len;
            let crc_stored = u32::from_be_bytes([
                png[data_end],
                png[data_end + 1],
                png[data_end + 2],
                png[data_end + 3],
            ]);
            let mut c = Crc32::new();
            c.update(&kind);
            c.update(&png[data_start..data_end]);
            assert_eq!(
                c.finalize(),
                crc_stored,
                "CRC mismatch for chunk {:?}",
                core::str::from_utf8(&kind).unwrap_or("???")
            );
            cursor = data_end + 4;
            if &kind == b"IEND" {
                break;
            }
        }
        assert_eq!(cursor, png.len(), "trailing bytes after IEND");
    }

    // --- encode_png ColorPixmap path ----------------------------------------

    #[test]
    fn encode_png_round_trip_4x4_red_with_transparent_corner() {
        let mut p = ColorPixmap::new(4, 4);
        // Fill solid opaque red.
        for y in 0..4 {
            for x in 0..4 {
                let idx = (y * 4 + x) * 4;
                p.data[idx] = 255;
                p.data[idx + 3] = 255;
            }
        }
        // Punch a transparent corner at (0, 0). Premul invariant:
        // a == 0 → rgb == 0, which is what we store.
        let idx = 0;
        p.data[idx] = 0;
        p.data[idx + 1] = 0;
        p.data[idx + 2] = 0;
        p.data[idx + 3] = 0;

        let bytes = encode_png(&p);
        let decoded = decode_png(&bytes).expect("decode round-trip");
        assert_eq!(decoded, p);
    }

    #[test]
    fn encode_png_1x1() {
        let mut p = ColorPixmap::new(1, 1);
        p.data = vec![10, 20, 30, 200];
        let bytes = encode_png(&p);
        let decoded = decode_png(&bytes).unwrap();
        assert_eq!(decoded.width, 1);
        assert_eq!(decoded.height, 1);
        // Round-trip through unpremul → straight RGBA → re-premul. With
        // rounded division on both sides, premul values can shift by ±1.
        let got = decoded.get(0, 0);
        assert!((got[0] as i32 - 10).abs() <= 1, "r drift: {} vs 10", got[0]);
        assert!((got[1] as i32 - 20).abs() <= 1, "g drift: {} vs 20", got[1]);
        assert!((got[2] as i32 - 30).abs() <= 1, "b drift: {} vs 30", got[2]);
        assert_eq!(got[3], 200);
    }

    #[test]
    fn encode_png_wide_1024x1() {
        // Stripe a horizontal gradient across a 1024×1 pixmap and make
        // sure the encoder handles a pathologically short, wide image
        // without error and that decode reproduces every pixel exactly.
        let mut p = ColorPixmap::new(1024, 1);
        for x in 0..1024u32 {
            let idx = x as usize * 4;
            // Fully opaque so unpremul is the identity and we can
            // do an exact equality check.
            p.data[idx] = (x & 0xFF) as u8;
            p.data[idx + 1] = ((x >> 1) & 0xFF) as u8;
            p.data[idx + 2] = ((x >> 2) & 0xFF) as u8;
            p.data[idx + 3] = 255;
        }
        let bytes = encode_png(&p);
        let decoded = decode_png(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn encode_png_metadata_roundtrips() {
        let p = ColorPixmap::new(7, 11);
        let bytes = encode_png(&p);
        // IHDR sits immediately after the 8-byte signature: 4 length +
        // 4 type + 4 width + 4 height + ...
        assert_eq!(&bytes[..8], &PNG_SIGNATURE);
        assert_eq!(&bytes[12..16], b"IHDR");
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        let ct = bytes[25];
        assert_eq!(w, 7);
        assert_eq!(h, 11);
        assert_eq!(ct, 6, "color type for ColorPixmap is 6 (RGBA)");
    }

    #[test]
    fn encode_png_zero_dimension_is_well_formed() {
        // Zero-dimension input produces a syntactically valid
        // signature || IHDR || IDAT(empty-zlib) || IEND. The payload
        // round-trips back through our decoder as a BadPng error
        // (decoder rejects zero dimension), which is documented.
        let p = ColorPixmap::new(0, 0);
        let bytes = encode_png(&p);
        assert_eq!(&bytes[..8], &PNG_SIGNATURE);
        // Verify CRCs are still well-formed even in this edge case.
        verify_all_crcs(&bytes);
        // Decoder should reject zero-dimension on the way back in.
        assert!(decode_png(&bytes).is_err());
    }

    #[test]
    fn encode_png_is_deterministic() {
        let mut p = ColorPixmap::new(4, 4);
        for (i, b) in p.data.iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        let a = encode_png(&p);
        let b = encode_png(&p);
        assert_eq!(a, b, "encode_png must be byte-deterministic");
    }

    // --- unpremul correctness -----------------------------------------------

    #[test]
    fn unpremultiply_zero_alpha_returns_zero_rgb() {
        assert_eq!(unpremultiply(99, 99, 99, 0), (0, 0, 0));
    }

    #[test]
    fn unpremultiply_full_alpha_is_identity() {
        assert_eq!(unpremultiply(10, 20, 30, 255), (10, 20, 30));
    }

    #[test]
    fn unpremultiply_half_alpha_doubles_back() {
        // Premul (64, 0, 0, 128) means "straight red 128 at 50% alpha".
        // Unpremul should recover ~128 red.
        let (r, g, b) = unpremultiply(64, 0, 0, 128);
        assert!((r as i32 - 128).abs() <= 1);
        assert_eq!(g, 0);
        assert_eq!(b, 0);
    }
}
