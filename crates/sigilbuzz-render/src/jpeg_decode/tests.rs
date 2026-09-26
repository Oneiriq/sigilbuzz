//! Unit tests for the JPEG decoder: marker walker, Huffman tables,
//! IDCT, and synthetic baseline streams.

use super::huffman::extend;
use super::idct::idct;
use super::*;

mod adversarial;
mod progressive;

/// Build a minimal baseline JPEG that encodes a single 8x8 block
/// of a constant Y/Cb/Cr value, with sampling 1x1 for each
/// component (4:4:4). Uses very small Huffman tables (one DC
/// symbol per class, one AC symbol) and identity quantization.
///
/// The block has DC = `dc_y` for luma, `dc_cb` for Cb, `dc_cr`
/// for Cr, and zero AC. After IDCT + level-shift the spatial
/// samples are uniformly `dc + 128` (clamped).
fn build_constant_jpeg(dc_y: i32, dc_cb: i32, dc_cr: i32) -> Vec<u8> {
    // We build:
    //   SOI
    //   DQT (3 identity tables, dest 0/1/2, but we use only 0 + 1)
    //   SOF0 (8x8, 3 components, 1x1 sampling each)
    //   DHT (DC/AC tables for class 0/1, dest 0/1)
    //   SOS (3 components)
    //   entropy-coded data
    //   EOI
    let mut out = vec![0xFF, MARKER_SOI];

    // DQT: two identity tables (dest 0 = luma, dest 1 = chroma).
    out.push(0xFF);
    out.push(MARKER_DQT);
    // Length = 2 + (1 + 64) * 2 = 132.
    out.extend_from_slice(&132u16.to_be_bytes());
    for dest in 0..2u8 {
        out.push(dest); // precision 0 + dest
        out.extend_from_slice(&[1u8; 64]);
    }

    // SOF0: 8x8, 3 components, 1x1 sampling each.
    out.push(0xFF);
    out.push(MARKER_SOF0);
    // Length = 2 + 6 + 3*3 = 17.
    out.extend_from_slice(&17u16.to_be_bytes());
    out.push(8); // precision
    out.extend_from_slice(&8u16.to_be_bytes()); // height
    out.extend_from_slice(&8u16.to_be_bytes()); // width
    out.push(3); // components
    out.push(1); // Y
    out.push((1 << 4) | 1); // 1x1
    out.push(0); // qt 0
    out.push(2); // Cb
    out.push((1 << 4) | 1);
    out.push(1); // qt 1
    out.push(3); // Cr
    out.push((1 << 4) | 1);
    out.push(1); // qt 1

    // DHT (four tables): DC/0, DC/1, AC/0, AC/1.
    // We use the standard JPEG DC luma + DC chroma + AC luma +
    // AC chroma tables (from the spec). They're long, but
    // necessary so the encoder side has real codes to use.
    let dc_lum_counts: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
    let dc_lum_syms: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    let dc_chr_counts: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
    let dc_chr_syms: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    let ac_lum_counts: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
    let ac_lum_syms: [u8; 162] = [
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
        0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52,
        0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25,
        0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45,
        0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64,
        0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
        0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6,
        0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3,
        0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8,
        0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ];
    let ac_chr_counts: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
    let ac_chr_syms: [u8; 162] = [
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
        0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33,
        0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18,
        0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44,
        0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63,
        0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
        0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4,
        0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca,
        0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7,
        0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ];
    let mut dht_body = Vec::new();
    let push_table = |body: &mut Vec<u8>, class: u8, dest: u8, counts: &[u8], syms: &[u8]| {
        body.push((class << 4) | dest);
        body.extend_from_slice(counts);
        body.extend_from_slice(syms);
    };
    push_table(&mut dht_body, 0, 0, &dc_lum_counts, &dc_lum_syms);
    push_table(&mut dht_body, 0, 1, &dc_chr_counts, &dc_chr_syms);
    push_table(&mut dht_body, 1, 0, &ac_lum_counts, &ac_lum_syms);
    push_table(&mut dht_body, 1, 1, &ac_chr_counts, &ac_chr_syms);
    out.push(0xFF);
    out.push(MARKER_DHT);
    out.extend_from_slice(&((dht_body.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&dht_body);

    // SOS: 3 components, DC=0/1/1 AC=0/1/1.
    out.push(0xFF);
    out.push(MARKER_SOS);
    // Length = 2 + 1 + 2*3 + 3 = 12.
    out.extend_from_slice(&12u16.to_be_bytes());
    out.push(3);
    out.push(1);
    out.push(0); // DC=0 AC=0
    out.push(2);
    out.push((1 << 4) | 1); // DC=1 AC=1
    out.push(3);
    out.push((1 << 4) | 1);
    out.push(0); // Ss
    out.push(63); // Se
    out.push(0); // Ah/Al

    // Entropy stream: encode three blocks (Y, Cb, Cr) where each
    // has a single DC value and EOB.
    let mut bw = BitWriter::default();
    encode_block(&mut bw, dc_y, true);
    encode_block(&mut bw, dc_cb, false);
    encode_block(&mut bw, dc_cr, false);
    bw.flush();
    out.extend_from_slice(&bw.bytes);

    out.push(0xFF);
    out.push(MARKER_EOI);
    out
}

/// Tiny encoder helper for the test fixture only.
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    buf: u32,
    len: u8,
}

impl BitWriter {
    fn write_bits(&mut self, code: u32, n: u8) {
        self.buf = (self.buf << n) | (code & ((1u32 << n) - 1));
        self.len += n;
        while self.len >= 8 {
            self.len -= 8;
            let b = ((self.buf >> self.len) & 0xFF) as u8;
            self.bytes.push(b);
            if b == 0xFF {
                self.bytes.push(0x00);
            }
        }
    }
    fn flush(&mut self) {
        if self.len > 0 {
            // Pad with 1-bits per spec.
            let pad = 8 - self.len;
            self.write_bits((1u32 << pad) - 1, pad);
        }
    }
}

/// Encode a single 8x8 block with a single DC coefficient and EOB.
///
/// `is_luma` selects between the luma (dest 0) and chroma (dest 1)
/// standard tables. Implements just enough of the spec encoder to
/// produce a stream the decoder will read back.
fn encode_block(bw: &mut BitWriter, dc: i32, is_luma: bool) {
    // Compute (size, code) for the DC value (delta from prev,
    // which we track by always using the absolute value since
    // encode_block is called with prev_dc=0 each test).
    let (size, code) = magnitude_encode(dc);
    // Look up the DC Huffman code for `size` in the standard
    // luma/chroma table. We hard-code the relevant prefixes for
    // size 0..=11.
    let (huff_code, huff_len) = if is_luma {
        std_dc_lum_code(size)
    } else {
        std_dc_chr_code(size)
    };
    bw.write_bits(huff_code, huff_len);
    if size > 0 {
        bw.write_bits(code, size);
    }
    // EOB: AC table code for symbol 0x00.
    let (eob_code, eob_len) = if is_luma {
        std_ac_lum_code(0x00)
    } else {
        std_ac_chr_code(0x00)
    };
    bw.write_bits(eob_code, eob_len);
}

/// Returns (size, code) for a JPEG magnitude-encoded value.
fn magnitude_encode(v: i32) -> (u8, u32) {
    if v == 0 {
        return (0, 0);
    }
    let abs = v.unsigned_abs();
    let size = 32 - abs.leading_zeros();
    let code = if v > 0 {
        v as u32
    } else {
        ((v - 1) & ((1i32 << size) - 1)) as u32
    };
    (size as u8, code)
}

// Hard-coded standard Huffman codes (T.81 K.3).
fn std_dc_lum_code(size: u8) -> (u32, u8) {
    match size {
        0 => (0b00, 2),
        1 => (0b010, 3),
        2 => (0b011, 3),
        3 => (0b100, 3),
        4 => (0b101, 3),
        5 => (0b110, 3),
        6 => (0b1110, 4),
        7 => (0b11110, 5),
        8 => (0b111110, 6),
        9 => (0b1111110, 7),
        10 => (0b11111110, 8),
        11 => (0b111111110, 9),
        _ => unreachable!(),
    }
}
fn std_dc_chr_code(size: u8) -> (u32, u8) {
    match size {
        0 => (0b00, 2),
        1 => (0b01, 2),
        2 => (0b10, 2),
        3 => (0b110, 3),
        4 => (0b1110, 4),
        5 => (0b11110, 5),
        6 => (0b111110, 6),
        7 => (0b1111110, 7),
        8 => (0b11111110, 8),
        9 => (0b111111110, 9),
        10 => (0b1111111110, 10),
        11 => (0b11111111110, 11),
        _ => unreachable!(),
    }
}
fn std_ac_lum_code(sym: u8) -> (u32, u8) {
    // Only EOB (0x00) is used in the constant-block fixture.
    match sym {
        0x00 => (0b1010, 4),
        _ => unreachable!("test fixture only emits EOB"),
    }
}
fn std_ac_chr_code(sym: u8) -> (u32, u8) {
    match sym {
        0x00 => (0b00, 2),
        _ => unreachable!("test fixture only emits EOB"),
    }
}

#[test]
fn marker_walker_rejects_missing_soi() {
    let bytes = [0u8; 16];
    let err = decode_jpeg(&bytes).unwrap_err();
    assert!(matches!(err, RenderError::BadJpeg("missing SOI")));
}

#[test]
fn marker_walker_rejects_arithmetic_sof() {
    // SOF9 (arithmetic). We accept SOF0 + SOF2 only; everything
    // else in the SOFn range must surface BadJpeg.
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xC9];
    bytes.extend_from_slice(&8u16.to_be_bytes()); // length
    bytes.extend_from_slice(&[8, 0, 8, 0, 8, 0]); // dummy body
    let err = decode_jpeg(&bytes).unwrap_err();
    assert!(matches!(
        err,
        RenderError::BadJpeg("non-baseline SOF unsupported")
    ));
}

#[test]
fn marker_walker_rejects_restart_interval_nonzero() {
    // SOI, DRI=1.
    let bytes = vec![0xFF, 0xD8, 0xFF, 0xDD, 0x00, 0x04, 0x00, 0x01];
    let err = decode_jpeg(&bytes).unwrap_err();
    assert!(matches!(
        err,
        RenderError::BadJpeg("restart markers not supported")
    ));
}

#[test]
fn truncated_segment_is_rejected() {
    // SOI then SOF0 with claimed length 100 but no body.
    let bytes = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x64];
    let err = decode_jpeg(&bytes).unwrap_err();
    assert!(matches!(err, RenderError::BadJpeg(_)));
}

#[test]
fn dqt_16bit_rejected() {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, MARKER_DQT];
    // Length = 2 + 1 + 128 = 131.
    bytes.extend_from_slice(&131u16.to_be_bytes());
    bytes.push(0x10); // precision=1, dest=0
    bytes.extend_from_slice(&[0u8; 128]);
    let err = decode_jpeg(&bytes).unwrap_err();
    assert!(matches!(
        err,
        RenderError::BadJpeg("16-bit quantization not supported")
    ));
}

#[test]
fn huffman_table_finalizes_valid_canonical_codes() {
    // Two codes of length 1: should be 0 and 1.
    let mut tbl = HuffmanTable {
        counts: [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        symbols: vec![0xAA, 0xBB],
        lookup: Vec::new(),
    };
    tbl.finalize().unwrap();
    // Code 0 (top bit 0) -> 0xAA, length 1.
    assert_eq!(tbl.lookup[0x0000], (0xAA, 1));
    // Code 1 (top bit 1) -> 0xBB, length 1.
    assert_eq!(tbl.lookup[0x8000], (0xBB, 1));
}

#[test]
fn huffman_table_rejects_count_overflow() {
    // 17 codes of length 4. Only 16 codes fit in 4 bits.
    let mut tbl = HuffmanTable {
        counts: [0, 0, 0, 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        symbols: vec![0u8; 17],
        lookup: Vec::new(),
    };
    let err = tbl.finalize().unwrap_err();
    assert!(matches!(err, RenderError::BadJpeg(_)));
}

#[test]
fn extend_signs_correctly() {
    // Receive of 4 bits, value 0b1010 = 10 -> top bit 1, positive.
    assert_eq!(extend(0b1010, 4), 10);
    // Receive of 4 bits, value 0b0010 = 2, top bit 0 -> negative.
    // Range is -(2^4 - 1) ..= -2^3 = -15..=-8. Specifically the
    // formula gives 2 + (-16) + 1 = -13.
    assert_eq!(extend(0b0010, 4), -13);
    assert_eq!(extend(0, 0), 0);
}

#[test]
fn idct_dc_only_block_is_constant() {
    // DC = 1024 (after dequant) -> spatial value = 1024 / 8 = 128.
    // After level shift: 128 + 128 = 256 -> clamped to 255.
    let mut coeffs = [0i32; 64];
    coeffs[0] = 1024;
    let mut out = [0u8; 64];
    idct(&coeffs, &mut out);
    for &v in &out {
        assert_eq!(v, 255);
    }
}

#[test]
fn synthetic_constant_jpeg_decodes_to_expected_color() {
    // dc_y = 0, dc_cb = 0, dc_cr = 64 -> red shift.
    // After IDCT + level shift each spatial sample is 128 for Y,
    // 128 for Cb, and 128 + (64/8) = 136 for Cr (since identity
    // quantization means dequant = 1, and with our test encoder
    // we wrote dc_cr = 64, prev_dc = 0, so dequant DC = 64).
    // After IDCT the spatial sample is 64 / 8 = 8. Plus level
    // shift = 136. So Cr - 128 = 8.
    // R = 128 + 1.402 * 8 = ~139, G = 128 - 0.71414*8 = ~122,
    // B = 128.
    let bytes = build_constant_jpeg(0, 0, 64);
    let pix = decode_jpeg(&bytes).unwrap();
    assert_eq!(pix.width, 8);
    assert_eq!(pix.height, 8);
    // Spot-check a center pixel. JPEG ringing is zero with a
    // single DC, so every pixel should match.
    let center = pix.get(4, 4);
    assert_eq!(center[3], 255);
    // R should be > G and > B.
    assert!(
        center[0] > center[1],
        "R ({}) > G ({})",
        center[0],
        center[1]
    );
    assert!(
        center[0] > center[2],
        "R ({}) > B ({})",
        center[0],
        center[2]
    );
    // R should land in the expected window 130..=145.
    assert!(
        (130..=145).contains(&center[0]),
        "R ({}) within expected red shift",
        center[0]
    );
}

#[test]
fn synthetic_neutral_jpeg_decodes_to_gray() {
    // All-zero DC -> spatial sample 0 -> after level shift 128.
    // YCbCr (128, 128, 128) -> RGB (128, 128, 128).
    let bytes = build_constant_jpeg(0, 0, 0);
    let pix = decode_jpeg(&bytes).unwrap();
    let center = pix.get(4, 4);
    assert_eq!(center[3], 255);
    for (ch, value) in center.iter().take(3).enumerate() {
        assert!(
            (126..=130).contains(value),
            "channel {ch} value {value} near 128"
        );
    }
}
