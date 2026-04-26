//! Minimum-viable baseline JPEG decoder for sbix `'jpg '` payloads.
//!
//! Hand-rolled from ITU-T T.81 (1992). Scope is deliberately narrow —
//! enough to decode the JPEG payloads that appear in real-world font
//! sbix tables (some Apple Color Emoji variants and a handful of CJK
//! emoji fonts) and nothing more:
//!
//! - **8-bit precision only.** All real-world font sbix JPEGs are 8-bit.
//! - **Baseline sequential DCT only (SOF0).** No progressive (SOF2),
//!   no arithmetic coding (SOF9..15), no hierarchical (SOFE).
//! - **YCbCr** (3-component) and **grayscale** (1-component).
//! - **Sampling factors:** 4:4:4, 4:2:2, 4:2:0 — and any combination
//!   where each component's max sampling factor is `<= 2`.
//! - **In-stream Huffman + quantization tables** (DHT / DQT). No JFIF
//!   "default" tables are assumed.
//! - **No restart markers, no thumbnails, no EXIF parsing.** APP*
//!   segments are skipped silently; RSTm segments produce
//!   [`RenderError::BadJpeg`].
//!
//! Output is a premultiplied RGBA [`ColorPixmap`] with alpha = 255
//! (JPEG has no transparency channel).
//!
//! # Pipeline
//!
//! 1. **Marker walker** parses the JPEG byte stream until SOS, then
//!    hands off to the entropy decoder.
//! 2. **Huffman decode** pulls DC + 63 AC zig-zag-ordered coefficients
//!    per 8×8 block.
//! 3. **Dequantize + de-zig-zag** turns the coefficient stream into
//!    natural-order 8×8 blocks.
//! 4. **IDCT** (a straightforward float-domain row+column DCT-III) maps
//!    coefficients back to spatial-domain samples.
//! 5. **Level shift +128** restores the 0..=255 range.
//! 6. **Chroma upsample** (nearest-neighbor) widens subsampled Cb/Cr to
//!    luma resolution.
//! 7. **YCbCr → RGB** via ITU-R BT.601 with clamping.
//!
//! # Non-goals
//!
//! - Progressive scan, arithmetic coding, lossless JPEG, JPEG-LS,
//!   JPEG 2000 (`'jp2 '`), TIFF (`'tiff'`).
//! - Color-managed output (ICC profiles), EXIF orientation.
//! - SIMD or fixed-point IDCT — the hot path here is tiny font emoji
//!   bitmaps, not high-throughput photo decode.
//!
//! See [`super::bitmaps::decode_sbix_glyph`] for the dispatch site.
//! Spec reference: ITU-T T.81 Annex F (sequential DCT-based mode of
//! operation).

use alloc::vec;
use alloc::vec::Vec;

use crate::error::RenderError;
use crate::pixmap::ColorPixmap;

// ---------------------------------------------------------------------------
// Marker codes (ITU-T T.81 Table B.1).
// ---------------------------------------------------------------------------

const MARKER_SOI: u8 = 0xD8;
const MARKER_EOI: u8 = 0xD9;
const MARKER_SOS: u8 = 0xDA;
const MARKER_DQT: u8 = 0xDB;
const MARKER_DHT: u8 = 0xC4;
const MARKER_DRI: u8 = 0xDD;
const MARKER_SOF0: u8 = 0xC0;
const MARKER_COM: u8 = 0xFE;

/// Maximum image dimension for a JPEG payload. Mirrors the PNG
/// decoder's per-dim ceiling so a malicious or malformed font can't
/// blow up `usize` math via a 65535×65535 SOF0.
const MAX_JPEG_DIM: u32 = 16384;

/// Hard cap on Huffman table count. JPEG allows 4 of each AC/DC class,
/// so the worst legitimate case is 8 tables.
const MAX_HUFF_TABLES: usize = 4;

/// Hard cap on quantization table count. Same logic — 4 destinations.
const MAX_QT_TABLES: usize = 4;

// ---------------------------------------------------------------------------
// Public API.
// ---------------------------------------------------------------------------

/// Decode a baseline JPEG byte slice into a premultiplied RGBA
/// [`ColorPixmap`].
///
/// Supports 8-bit baseline sequential JPEGs with YCbCr (3-component)
/// or grayscale (1-component) data and sampling factors where each
/// component's max h/v is `<= 2`. Progressive scan, arithmetic
/// coding, 16-bit precision, hierarchical mode, restart markers, and
/// JPEG2000 / TIFF return [`RenderError::BadJpeg`].
///
/// # Errors
/// Returns [`RenderError::BadJpeg`] on any structural problem,
/// truncated stream, or unsupported feature.
pub fn decode_jpeg(bytes: &[u8]) -> Result<ColorPixmap, RenderError> {
    let mut decoder = Decoder::new(bytes);
    decoder.decode()
}

// ---------------------------------------------------------------------------
// Decoder state.
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct HuffmanTable {
    /// Number of codes of each length 1..=16.
    /// Indexed 0..16 where index `i` is the count of length `i + 1`.
    counts: [u8; 16],
    /// Symbol values, in the order they appear in the DHT segment.
    symbols: Vec<u8>,
    /// Decode lookup: maps `code` (right-padded to 16 bits) → (symbol,
    /// code length). We build a flat 16-bit table during `finalize`.
    /// `lookup[code]` is `(symbol, length)` for any 16-bit code whose
    /// top `length` bits match the canonical code, with `length = 0`
    /// meaning "no code at this prefix" (decode error).
    lookup: Vec<(u8, u8)>,
}

impl HuffmanTable {
    /// Build the canonical-code lookup table from `counts` + `symbols`.
    /// Returns `BadJpeg` if the table is malformed (overflows the
    /// 16-bit code space or symbol count exceeds the running tally).
    fn finalize(&mut self) -> Result<(), RenderError> {
        // Empty tables are legal in some malformed payloads but not
        // useful; the entropy stage will surface a decode error if any
        // codeword tries to use an empty table.
        let mut lookup = vec![(0u8, 0u8); 1 << 16];
        let mut code: u32 = 0;
        let mut sym_index = 0usize;
        for (len_minus_one, &count) in self.counts.iter().enumerate() {
            let len = (len_minus_one + 1) as u8;
            for _ in 0..count {
                if sym_index >= self.symbols.len() {
                    return Err(RenderError::BadJpeg("DHT symbol count mismatch"));
                }
                if code >= (1u32 << len) {
                    return Err(RenderError::BadJpeg("DHT code overflows length"));
                }
                let symbol = self.symbols[sym_index];
                sym_index += 1;
                // Fill every 16-bit suffix that matches this prefix.
                let shift = 16 - u32::from(len);
                let prefix = code << shift;
                let span = 1u32 << shift;
                for entry in &mut lookup[prefix as usize..(prefix + span) as usize] {
                    *entry = (symbol, len);
                }
                code += 1;
            }
            code <<= 1;
        }
        if sym_index != self.symbols.len() {
            return Err(RenderError::BadJpeg("DHT symbol count mismatch"));
        }
        self.lookup = lookup;
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct ComponentSpec {
    /// Component identifier from SOF0 (e.g. 1 = Y, 2 = Cb, 3 = Cr in
    /// most JFIF streams; the spec doesn't pin these so we resolve by
    /// position in the SOF0 component list).
    id: u8,
    h_sampling: u8,
    v_sampling: u8,
    qt_dest: u8,
    /// AC / DC Huffman destinations are filled in by the SOS parser.
    dc_huff: u8,
    ac_huff: u8,
}

struct Decoder<'a> {
    src: &'a [u8],
    cursor: usize,
    qt: [Option<[i32; 64]>; MAX_QT_TABLES],
    dc_huff: [Option<HuffmanTable>; MAX_HUFF_TABLES],
    ac_huff: [Option<HuffmanTable>; MAX_HUFF_TABLES],
    width: u32,
    height: u32,
    components: Vec<ComponentSpec>,
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            src: bytes,
            cursor: 0,
            qt: [None, None, None, None],
            dc_huff: [None, None, None, None],
            ac_huff: [None, None, None, None],
            width: 0,
            height: 0,
            components: Vec::new(),
        }
    }

    fn decode(&mut self) -> Result<ColorPixmap, RenderError> {
        // Verify SOI.
        if self.src.len() < 2 || self.src[0] != 0xFF || self.src[1] != MARKER_SOI {
            return Err(RenderError::BadJpeg("missing SOI"));
        }
        self.cursor = 2;

        // Walk markers until we hit SOS, then enter the entropy stage.
        loop {
            let marker = self.read_marker()?;
            match marker {
                MARKER_DQT => self.read_dqt()?,
                MARKER_DHT => self.read_dht()?,
                MARKER_SOF0 => self.read_sof0()?,
                MARKER_SOS => {
                    // SOS header sets per-component huff dests, then
                    // the entropy-coded data starts immediately.
                    return self.read_sos_and_decode();
                }
                MARKER_DRI => {
                    // Restart-interval definition. We don't support
                    // restart markers, so any nonzero interval is an
                    // immediate fail; an interval of 0 is benign.
                    let payload = self.read_segment()?;
                    if payload.len() != 2 {
                        return Err(RenderError::BadJpeg("DRI length not 2"));
                    }
                    let ri = u16::from_be_bytes([payload[0], payload[1]]);
                    if ri != 0 {
                        return Err(RenderError::BadJpeg("restart markers not supported"));
                    }
                }
                MARKER_COM => {
                    let _ = self.read_segment()?;
                }
                MARKER_EOI => return Err(RenderError::BadJpeg("EOI before SOS")),
                m if (0xE0..=0xEF).contains(&m) => {
                    // APP0..APP15 — skip.
                    let _ = self.read_segment()?;
                }
                m if (0xC1..=0xCF).contains(&m) && m != MARKER_DHT => {
                    // SOF1 (extended), SOF2 (progressive), SOF3
                    // (lossless), SOF5..7 (differential), SOF9..15
                    // (arithmetic). All unsupported.
                    return Err(RenderError::BadJpeg("non-baseline SOF unsupported"));
                }
                m if (0xD0..=0xD7).contains(&m) => {
                    return Err(RenderError::BadJpeg("RST marker outside scan"));
                }
                _ => {
                    // Unknown marker — try to skip via length.
                    let _ = self.read_segment()?;
                }
            }
        }
    }

    /// Read the next marker byte. Skips `0xFF` fill bytes between
    /// segments (spec allows arbitrary 0xFF padding).
    fn read_marker(&mut self) -> Result<u8, RenderError> {
        if self.cursor >= self.src.len() {
            return Err(RenderError::BadJpeg("truncated stream"));
        }
        if self.src[self.cursor] != 0xFF {
            return Err(RenderError::BadJpeg("expected 0xFF marker prefix"));
        }
        // Skip fill bytes.
        while self.cursor < self.src.len() && self.src[self.cursor] == 0xFF {
            self.cursor += 1;
        }
        if self.cursor >= self.src.len() {
            return Err(RenderError::BadJpeg("truncated marker"));
        }
        let m = self.src[self.cursor];
        self.cursor += 1;
        if m == 0x00 {
            return Err(RenderError::BadJpeg("0xFF 0x00 outside entropy stream"));
        }
        Ok(m)
    }

    /// Read a length-prefixed segment payload (length is 16-bit BE
    /// and includes its own two bytes).
    fn read_segment(&mut self) -> Result<&'a [u8], RenderError> {
        if self.cursor + 2 > self.src.len() {
            return Err(RenderError::BadJpeg("truncated segment length"));
        }
        let len = u16::from_be_bytes([self.src[self.cursor], self.src[self.cursor + 1]]) as usize;
        if len < 2 {
            return Err(RenderError::BadJpeg("segment length < 2"));
        }
        let body_start = self.cursor + 2;
        let body_end = self
            .cursor
            .checked_add(len)
            .ok_or(RenderError::BadJpeg("segment length overflow"))?;
        if body_end > self.src.len() {
            return Err(RenderError::BadJpeg("truncated segment body"));
        }
        let body = &self.src[body_start..body_end];
        self.cursor = body_end;
        Ok(body)
    }

    fn read_dqt(&mut self) -> Result<(), RenderError> {
        let body = self.read_segment()?;
        let mut i = 0usize;
        while i < body.len() {
            let pq_tq = body[i];
            i += 1;
            let precision = pq_tq >> 4; // 0 = 8-bit, 1 = 16-bit
            let dest = (pq_tq & 0x0F) as usize;
            if dest >= MAX_QT_TABLES {
                return Err(RenderError::BadJpeg("DQT destination out of range"));
            }
            if precision == 0 {
                if i + 64 > body.len() {
                    return Err(RenderError::BadJpeg("truncated DQT (8-bit)"));
                }
                let mut tbl = [0i32; 64];
                for (k, slot) in tbl.iter_mut().enumerate() {
                    *slot = i32::from(body[i + k]);
                }
                i += 64;
                self.qt[dest] = Some(tbl);
            } else if precision == 1 {
                return Err(RenderError::BadJpeg("16-bit quantization not supported"));
            } else {
                return Err(RenderError::BadJpeg("invalid DQT precision"));
            }
        }
        Ok(())
    }

    fn read_dht(&mut self) -> Result<(), RenderError> {
        let body = self.read_segment()?;
        let mut i = 0usize;
        while i < body.len() {
            if i + 17 > body.len() {
                return Err(RenderError::BadJpeg("truncated DHT header"));
            }
            let tc_th = body[i];
            i += 1;
            let class = tc_th >> 4; // 0 = DC, 1 = AC
            let dest = (tc_th & 0x0F) as usize;
            if dest >= MAX_HUFF_TABLES {
                return Err(RenderError::BadJpeg("DHT destination out of range"));
            }
            let mut counts = [0u8; 16];
            counts.copy_from_slice(&body[i..i + 16]);
            i += 16;
            let n_symbols: usize = counts.iter().map(|&c| c as usize).sum();
            if i + n_symbols > body.len() {
                return Err(RenderError::BadJpeg("truncated DHT symbols"));
            }
            let symbols = body[i..i + n_symbols].to_vec();
            i += n_symbols;
            let mut tbl = HuffmanTable {
                counts,
                symbols,
                lookup: Vec::new(),
            };
            tbl.finalize()?;
            match class {
                0 => self.dc_huff[dest] = Some(tbl),
                1 => self.ac_huff[dest] = Some(tbl),
                _ => return Err(RenderError::BadJpeg("invalid DHT class")),
            }
        }
        Ok(())
    }

    fn read_sof0(&mut self) -> Result<(), RenderError> {
        let body = self.read_segment()?;
        if body.len() < 6 {
            return Err(RenderError::BadJpeg("truncated SOF0"));
        }
        let precision = body[0];
        if precision != 8 {
            return Err(RenderError::BadJpeg("SOF0 precision not 8-bit"));
        }
        let height = u16::from_be_bytes([body[1], body[2]]);
        let width = u16::from_be_bytes([body[3], body[4]]);
        if width == 0 || height == 0 {
            return Err(RenderError::BadJpeg("SOF0 zero dimension"));
        }
        if u32::from(width) > MAX_JPEG_DIM || u32::from(height) > MAX_JPEG_DIM {
            return Err(RenderError::BadJpeg("SOF0 dimension exceeds max"));
        }
        self.width = u32::from(width);
        self.height = u32::from(height);
        let n_comp = body[5] as usize;
        if !matches!(n_comp, 1 | 3) {
            return Err(RenderError::BadJpeg("SOF0 component count must be 1 or 3"));
        }
        if body.len() < 6 + 3 * n_comp {
            return Err(RenderError::BadJpeg("truncated SOF0 components"));
        }
        let mut comps = Vec::with_capacity(n_comp);
        for c in 0..n_comp {
            let off = 6 + 3 * c;
            let id = body[off];
            let sampling = body[off + 1];
            let h = sampling >> 4;
            let v = sampling & 0x0F;
            let qt_dest = body[off + 2];
            if !(1..=2).contains(&h) || !(1..=2).contains(&v) {
                return Err(RenderError::BadJpeg("sampling factor not in 1..=2"));
            }
            if qt_dest as usize >= MAX_QT_TABLES {
                return Err(RenderError::BadJpeg("SOF0 qt_dest out of range"));
            }
            comps.push(ComponentSpec {
                id,
                h_sampling: h,
                v_sampling: v,
                qt_dest,
                dc_huff: 0,
                ac_huff: 0,
            });
        }
        self.components = comps;
        Ok(())
    }

    fn read_sos_and_decode(&mut self) -> Result<ColorPixmap, RenderError> {
        let body = self.read_segment()?;
        if body.is_empty() {
            return Err(RenderError::BadJpeg("empty SOS"));
        }
        let n_scan = body[0] as usize;
        if n_scan != self.components.len() {
            return Err(RenderError::BadJpeg("SOS component count mismatch"));
        }
        if body.len() < 1 + 2 * n_scan + 3 {
            return Err(RenderError::BadJpeg("truncated SOS"));
        }
        for c in 0..n_scan {
            let off = 1 + 2 * c;
            let id = body[off];
            let td_ta = body[off + 1];
            let dc = td_ta >> 4;
            let ac = td_ta & 0x0F;
            // Find the matching component in SOF0 order.
            let comp = self
                .components
                .iter_mut()
                .find(|cs| cs.id == id)
                .ok_or(RenderError::BadJpeg("SOS component id not in SOF0"))?;
            comp.dc_huff = dc;
            comp.ac_huff = ac;
        }
        // Last 3 bytes: Ss, Se, Ah/Al. Baseline requires Ss=0, Se=63,
        // Ah=Al=0.
        let tail = &body[1 + 2 * n_scan..];
        if tail[0] != 0 || tail[1] != 63 || tail[2] != 0 {
            return Err(RenderError::BadJpeg("non-baseline scan parameters"));
        }
        // Hand off to the entropy stage. The remainder of `self.src`
        // from `self.cursor` is the entropy-coded segment ending at
        // EOI.
        self.decode_scan()
    }

    fn decode_scan(&mut self) -> Result<ColorPixmap, RenderError> {
        // Validate that every referenced table exists.
        for comp in &self.components {
            if self.qt[comp.qt_dest as usize].is_none() {
                return Err(RenderError::BadJpeg("missing quantization table"));
            }
            if self.dc_huff[comp.dc_huff as usize].is_none() {
                return Err(RenderError::BadJpeg("missing DC Huffman table"));
            }
            if self.ac_huff[comp.ac_huff as usize].is_none() {
                return Err(RenderError::BadJpeg("missing AC Huffman table"));
            }
        }
        let max_h = self
            .components
            .iter()
            .map(|c| c.h_sampling)
            .max()
            .unwrap_or(1);
        let max_v = self
            .components
            .iter()
            .map(|c| c.v_sampling)
            .max()
            .unwrap_or(1);
        let mcu_w_px = u32::from(max_h) * 8;
        let mcu_h_px = u32::from(max_v) * 8;
        let mcus_x = self.width.div_ceil(mcu_w_px);
        let mcus_y = self.height.div_ceil(mcu_h_px);

        // Per-component sample plane at the *full* MCU grid.
        let mut planes: Vec<Vec<u8>> = self
            .components
            .iter()
            .map(|c| {
                let pw = (mcus_x * 8 * u32::from(c.h_sampling)) as usize;
                let ph = (mcus_y * 8 * u32::from(c.v_sampling)) as usize;
                vec![0u8; pw * ph]
            })
            .collect();
        let plane_strides: Vec<usize> = self
            .components
            .iter()
            .map(|c| (mcus_x * 8 * u32::from(c.h_sampling)) as usize)
            .collect();

        let mut bit_reader = BitReader::new(&self.src[self.cursor..]);
        let mut prev_dc = vec![0i32; self.components.len()];

        for mcu_y in 0..mcus_y {
            for mcu_x in 0..mcus_x {
                for (ci, comp) in self.components.iter().enumerate() {
                    let h = u32::from(comp.h_sampling);
                    let v = u32::from(comp.v_sampling);
                    for by in 0..v {
                        for bx in 0..h {
                            let mut coeffs = [0i32; 64];
                            decode_block(
                                &mut bit_reader,
                                self.dc_huff[comp.dc_huff as usize]
                                    .as_ref()
                                    .expect("validated above"),
                                self.ac_huff[comp.ac_huff as usize]
                                    .as_ref()
                                    .expect("validated above"),
                                self.qt[comp.qt_dest as usize]
                                    .as_ref()
                                    .expect("validated above"),
                                &mut prev_dc[ci],
                                &mut coeffs,
                            )?;
                            let mut samples = [0u8; 64];
                            idct(&coeffs, &mut samples);
                            let block_x = (mcu_x * h + bx) * 8;
                            let block_y = (mcu_y * v + by) * 8;
                            let stride = plane_strides[ci];
                            for j in 0..8 {
                                for i in 0..8 {
                                    let dst_idx =
                                        (block_y as usize + j) * stride + (block_x as usize + i);
                                    planes[ci][dst_idx] = samples[j * 8 + i];
                                }
                            }
                        }
                    }
                }
            }
        }

        // Compose into RGBA.
        let w = self.width as usize;
        let h = self.height as usize;
        let mut out = ColorPixmap::new(self.width, self.height);
        out.data = vec![0u8; w * h * 4];

        if self.components.len() == 1 {
            // Grayscale.
            let stride = plane_strides[0];
            for y in 0..h {
                for x in 0..w {
                    let g = planes[0][y * stride + x];
                    let off = (y * w + x) * 4;
                    out.data[off] = g;
                    out.data[off + 1] = g;
                    out.data[off + 2] = g;
                    out.data[off + 3] = 255;
                }
            }
        } else {
            // YCbCr → RGB. Sample chroma via nearest-neighbor at the
            // luma grid: pixel (x, y) in luma maps to
            // (x * h_chroma / max_h, y * v_chroma / max_v) in chroma.
            let (h_y, v_y) = (
                u32::from(self.components[0].h_sampling),
                u32::from(self.components[0].v_sampling),
            );
            let (h_cb, v_cb) = (
                u32::from(self.components[1].h_sampling),
                u32::from(self.components[1].v_sampling),
            );
            let (h_cr, v_cr) = (
                u32::from(self.components[2].h_sampling),
                u32::from(self.components[2].v_sampling),
            );
            let stride_y = plane_strides[0];
            let stride_cb = plane_strides[1];
            let stride_cr = plane_strides[2];
            let max_h_u = u32::from(max_h);
            let max_v_u = u32::from(max_v);
            for y in 0..h {
                for x in 0..w {
                    // Luma is stored at the full MCU grid; the
                    // component's own h/v is already accounted for in
                    // its plane stride. Sample at (x * h_y / max_h,
                    // y * v_y / max_v).
                    let yx = (x as u32) * h_y / max_h_u;
                    let yy = (y as u32) * v_y / max_v_u;
                    let cbx = (x as u32) * h_cb / max_h_u;
                    let cby = (y as u32) * v_cb / max_v_u;
                    let crx = (x as u32) * h_cr / max_h_u;
                    let cry = (y as u32) * v_cr / max_v_u;
                    let yv = i32::from(planes[0][yy as usize * stride_y + yx as usize]);
                    let cb = i32::from(planes[1][cby as usize * stride_cb + cbx as usize]) - 128;
                    let cr = i32::from(planes[2][cry as usize * stride_cr + crx as usize]) - 128;
                    // ITU-R BT.601 in fixed-point Q16. Coefficients
                    // are 1.402, 0.34414, 0.71414, 1.772 each
                    // multiplied by 65536 and rounded.
                    let r = yv + ((91881 * cr) >> 16);
                    let g = yv - ((22554 * cb + 46802 * cr) >> 16);
                    let b = yv + ((116130 * cb) >> 16);
                    let off = (y * w + x) * 4;
                    out.data[off] = clamp_u8(r);
                    out.data[off + 1] = clamp_u8(g);
                    out.data[off + 2] = clamp_u8(b);
                    out.data[off + 3] = 255;
                }
            }
        }

        Ok(out)
    }
}

#[inline]
fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

// ---------------------------------------------------------------------------
// Bit-reader over the entropy-coded segment. JPEG entropy data uses
// byte stuffing: every literal `0xFF` is followed by a `0x00` which
// must be skipped. A `0xFF` followed by anything else is a marker
// (typically EOI) and ends the stream.
// ---------------------------------------------------------------------------

struct BitReader<'a> {
    src: &'a [u8],
    pos: usize,
    /// Right-aligned bit buffer of 0..=24 bits.
    buf: u32,
    /// Bits currently valid in `buf`.
    len: u8,
    /// Once we hit a marker (0xFF nn with nn != 0x00) the stream is
    /// ended; further reads return 0 (zero-padding behaviour matches
    /// libjpeg for tail-truncated streams).
    eos: bool,
}

impl<'a> BitReader<'a> {
    fn new(src: &'a [u8]) -> Self {
        Self {
            src,
            pos: 0,
            buf: 0,
            len: 0,
            eos: false,
        }
    }

    fn fill(&mut self) {
        while self.len <= 24 {
            if self.eos || self.pos >= self.src.len() {
                self.eos = true;
                // Pad with zeros once the entropy stream ends. A
                // well-formed stream stops naturally before this loop
                // exhausts all bits, so we should never produce
                // meaningful zero-bits in practice.
                self.buf <<= 8;
                self.len += 8;
                continue;
            }
            let b = self.src[self.pos];
            self.pos += 1;
            if b == 0xFF {
                if self.pos >= self.src.len() {
                    self.eos = true;
                    self.buf <<= 8;
                    self.len += 8;
                    continue;
                }
                let next = self.src[self.pos];
                if next == 0x00 {
                    // Stuffed byte — consume and emit literal 0xFF.
                    self.pos += 1;
                    self.buf = (self.buf << 8) | u32::from(b);
                    self.len += 8;
                } else {
                    // Real marker. End the stream.
                    self.eos = true;
                    // Don't consume the marker bytes; the surrounding
                    // walker doesn't need them once the scan is over.
                    self.pos -= 1; // step back to the 0xFF
                    self.buf <<= 8;
                    self.len += 8;
                }
            } else {
                self.buf = (self.buf << 8) | u32::from(b);
                self.len += 8;
            }
        }
    }

    fn peek_bits(&mut self, n: u8) -> u32 {
        if self.len < n {
            self.fill();
        }
        if n == 0 {
            return 0;
        }
        (self.buf >> (self.len - n)) & ((1u32 << n) - 1)
    }

    fn consume(&mut self, n: u8) {
        debug_assert!(self.len >= n);
        self.len -= n;
    }

    fn read_bits(&mut self, n: u8) -> u32 {
        let v = self.peek_bits(n);
        self.consume(n);
        v
    }

    /// Decode a Huffman-coded symbol against `tbl`.
    fn decode_huff(&mut self, tbl: &HuffmanTable) -> Result<u8, RenderError> {
        if tbl.lookup.is_empty() {
            return Err(RenderError::BadJpeg("empty Huffman table"));
        }
        let code = self.peek_bits(16) as usize;
        let (sym, len) = tbl.lookup[code];
        if len == 0 {
            return Err(RenderError::BadJpeg("Huffman decode error"));
        }
        self.consume(len);
        Ok(sym)
    }
}

/// Sign-extend an `n`-bit JPEG receive value: top bit 1 → positive,
/// top bit 0 → negative, where the negative range is
/// `-(2^n - 1) ..= -2^(n-1)`.
fn extend(v: u32, n: u8) -> i32 {
    if n == 0 {
        return 0;
    }
    let v = v as i32;
    let vt = 1i32 << (n - 1);
    if v < vt {
        v + (-1i32 << n) + 1
    } else {
        v
    }
}

/// Natural-order index for each zig-zag position.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

fn decode_block(
    br: &mut BitReader<'_>,
    dc_tbl: &HuffmanTable,
    ac_tbl: &HuffmanTable,
    qt: &[i32; 64],
    prev_dc: &mut i32,
    out: &mut [i32; 64],
) -> Result<(), RenderError> {
    // DC coefficient.
    let t = br.decode_huff(dc_tbl)?;
    if t > 15 {
        return Err(RenderError::BadJpeg("invalid DC magnitude"));
    }
    let raw = br.read_bits(t);
    let diff = extend(raw, t);
    *prev_dc = prev_dc.wrapping_add(diff);
    out[0] = *prev_dc * qt[0];

    // AC coefficients.
    let mut k = 1;
    while k < 64 {
        let rs = br.decode_huff(ac_tbl)?;
        let run = (rs >> 4) as usize;
        let size = rs & 0x0F;
        if size == 0 {
            if run == 15 {
                // ZRL — 16 zeros, then continue.
                k += 16;
                continue;
            }
            // EOB — rest of block is zero.
            break;
        }
        k += run;
        if k >= 64 {
            return Err(RenderError::BadJpeg("AC run overflow"));
        }
        let raw = br.read_bits(size);
        let val = extend(raw, size);
        let nat = ZIGZAG[k];
        out[nat] = val * qt[k];
        k += 1;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Inverse DCT — straightforward float-domain DCT-III, applied first
// across rows then across columns. Adequate for tiny font emoji
// bitmaps; not optimised for throughput.
// ---------------------------------------------------------------------------

#[allow(clippy::excessive_precision)]
fn idct(coeffs: &[i32; 64], out: &mut [u8; 64]) {
    // Build a float scratch.
    let mut tmp = [0.0f32; 64];
    for i in 0..64 {
        tmp[i] = coeffs[i] as f32;
    }
    let mut work = [0.0f32; 64];

    // 1D IDCT along rows: tmp -> work.
    for row in 0..8 {
        let base = row * 8;
        for x in 0..8 {
            let mut acc = 0.0f32;
            for u in 0..8 {
                let cu = if u == 0 {
                    core::f32::consts::FRAC_1_SQRT_2
                } else {
                    1.0
                };
                let theta = ((2 * x + 1) as f32) * (u as f32) * core::f32::consts::PI / 16.0;
                acc += cu * tmp[base + u] * theta.cos();
            }
            work[base + x] = acc * 0.5;
        }
    }
    // 1D IDCT along columns: work -> tmp.
    for col in 0..8 {
        for y in 0..8 {
            let mut acc = 0.0f32;
            for v in 0..8 {
                let cv = if v == 0 {
                    core::f32::consts::FRAC_1_SQRT_2
                } else {
                    1.0
                };
                let theta = ((2 * y + 1) as f32) * (v as f32) * core::f32::consts::PI / 16.0;
                acc += cv * work[v * 8 + col] * theta.cos();
            }
            tmp[y * 8 + col] = acc * 0.5;
        }
    }
    // Level shift +128 and clamp to 0..=255.
    for i in 0..64 {
        let v = (tmp[i] + 128.0).round() as i32;
        out[i] = v.clamp(0, 255) as u8;
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal baseline JPEG that encodes a single 8×8 block
    /// of a constant Y/Cb/Cr value, with sampling 1×1 for each
    /// component (4:4:4). Uses very small Huffman tables — one DC
    /// symbol per class, one AC symbol — and identity quantization.
    ///
    /// The block has DC = `dc_y` for luma, `dc_cb` for Cb, `dc_cr`
    /// for Cr, and zero AC. After IDCT + level-shift the spatial
    /// samples are uniformly `dc + 128` (clamped).
    fn build_constant_jpeg(dc_y: i32, dc_cb: i32, dc_cr: i32) -> Vec<u8> {
        // We build:
        //   SOI
        //   DQT (3 identity tables, dest 0/1/2 — but we use only 0 + 1)
        //   SOF0 (8x8, 3 components, 1x1 sampling each)
        //   DHT (DC/AC tables for class 0/1, dest 0/1)
        //   SOS (3 components)
        //   entropy-coded data
        //   EOI
        let mut out = vec![0xFF, MARKER_SOI];

        // DQT — two identity tables (dest 0 = luma, dest 1 = chroma).
        out.push(0xFF);
        out.push(MARKER_DQT);
        // Length = 2 + (1 + 64) * 2 = 132.
        out.extend_from_slice(&132u16.to_be_bytes());
        for dest in 0..2u8 {
            out.push(dest); // precision 0 + dest
            out.extend_from_slice(&[1u8; 64]);
        }

        // SOF0 — 8×8, 3 components, 1×1 sampling each.
        out.push(0xFF);
        out.push(MARKER_SOF0);
        // Length = 2 + 6 + 3*3 = 17.
        out.extend_from_slice(&17u16.to_be_bytes());
        out.push(8); // precision
        out.extend_from_slice(&8u16.to_be_bytes()); // height
        out.extend_from_slice(&8u16.to_be_bytes()); // width
        out.push(3); // components
        out.push(1); // Y
        out.push((1 << 4) | 1); // 1×1
        out.push(0); // qt 0
        out.push(2); // Cb
        out.push((1 << 4) | 1);
        out.push(1); // qt 1
        out.push(3); // Cr
        out.push((1 << 4) | 1);
        out.push(1); // qt 1

        // DHT — four tables: DC/0, DC/1, AC/0, AC/1.
        // We use the standard JPEG DC luma + DC chroma + AC luma +
        // AC chroma tables (from the spec). They're long, but
        // necessary so the encoder side has real codes to use.
        let dc_lum_counts: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
        let dc_lum_syms: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        let dc_chr_counts: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
        let dc_chr_syms: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        let ac_lum_counts: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
        let ac_lum_syms: [u8; 162] = [
            0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51,
            0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1,
            0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18,
            0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39,
            0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57,
            0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75,
            0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92,
            0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
            0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
            0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8,
            0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2,
            0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
        ];
        let ac_chr_counts: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
        let ac_chr_syms: [u8; 162] = [
            0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07,
            0x61, 0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09,
            0x23, 0x33, 0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25,
            0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38,
            0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56,
            0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74,
            0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
            0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5,
            0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba,
            0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6,
            0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2,
            0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
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

        // SOS — 3 components, DC=0/1/1 AC=0/1/1.
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

    /// Encode a single 8×8 block with a single DC coefficient and EOB.
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
    fn marker_walker_rejects_progressive_sof() {
        // SOI then SOF2 (progressive).
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xC2];
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
        // Code 0 (top bit 0) → 0xAA, length 1.
        assert_eq!(tbl.lookup[0x0000], (0xAA, 1));
        // Code 1 (top bit 1) → 0xBB, length 1.
        assert_eq!(tbl.lookup[0x8000], (0xBB, 1));
    }

    #[test]
    fn huffman_table_rejects_count_overflow() {
        // 17 codes of length 4 — only 16 codes fit in 4 bits.
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
        // Receive of 4 bits, value 0b1010 = 10 → top bit 1, positive.
        assert_eq!(extend(0b1010, 4), 10);
        // Receive of 4 bits, value 0b0010 = 2, top bit 0 → negative.
        // Range is -(2^4 - 1) ..= -2^3 = -15..=-8. Specifically the
        // formula gives 2 + (-16) + 1 = -13.
        assert_eq!(extend(0b0010, 4), -13);
        assert_eq!(extend(0, 0), 0);
    }

    #[test]
    fn idct_dc_only_block_is_constant() {
        // DC = 1024 (after dequant) → spatial value = 1024 / 8 = 128.
        // After level shift: 128 + 128 = 256 → clamped to 255.
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
        // dc_y = 0, dc_cb = 0, dc_cr = 64 → red shift.
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
        // Spot-check a center pixel — JPEG ringing is zero with a
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
        // All-zero DC → spatial sample 0 → after level shift 128.
        // YCbCr (128, 128, 128) → RGB (128, 128, 128).
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
}
