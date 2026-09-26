//! Minimum-viable baseline JPEG decoder for sbix `'jpg '` payloads.
//!
//! Hand-rolled from ITU-T T.81 (1992). Scope is narrow:
//! enough to decode the JPEG payloads that appear in real-world font
//! sbix tables (some Apple Color Emoji variants and a handful of CJK
//! emoji fonts) and nothing more:
//!
//! - **8-bit precision only.** All real-world font sbix JPEGs are 8-bit.
//! - **Baseline sequential DCT (SOF0)** plus **progressive DCT (SOF2)**
//!   first-time DC and AC scans, plus DC successive-approximation
//!   refinement. AC successive-approximation refinement scans (Ah > 0
//!   on an AC band) are surfaced as [`RenderError::BadJpeg`],
//!   uncommon in real-world font payloads but explicitly out of scope
//!   for this PR. No arithmetic coding (SOF9..15), no hierarchical
//!   (SOFE).
//! - **YCbCr** (3-component) and **grayscale** (1-component).
//! - **Sampling factors:** 4:4:4, 4:2:2, 4:2:0, and any combination
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
//!    per 8x8 block.
//! 3. **Dequantize + de-zig-zag** turns the coefficient stream into
//!    natural-order 8x8 blocks.
//! 4. **IDCT** (a straightforward float-domain row+column DCT-III) maps
//!    coefficients back to spatial-domain samples.
//! 5. **Level shift +128** restores the 0..=255 range.
//! 6. **Chroma upsample** (nearest-neighbor) widens subsampled Cb/Cr to
//!    luma resolution.
//! 7. **YCbCr -> RGB** via ITU-R BT.601 with clamping.
//!
//! # Non-goals
//!
//! - AC successive-approximation refinement scans (rare; the bit-plane
//!   walking over existing nonzeros is the thorny progressive
//!   subroutine and not seen in font sbix payloads we tested against).
//! - Arithmetic coding, lossless JPEG, JPEG-LS, JPEG 2000 (`'jp2 '`),
//!   TIFF (`'tiff'`).
//! - Color-managed output (ICC profiles), EXIF orientation.
//! - SIMD or fixed-point IDCT: the hot path here is tiny font emoji
//!   bitmaps, not high-throughput photo decode.
//!
//! See [`super::bitmaps::decode_sbix_glyph`] for the dispatch site.
//! Spec reference: ITU-T T.81 Annex F (sequential DCT-based mode of
//! operation).

use alloc::vec;
use alloc::vec::Vec;

use crate::error::RenderError;
use crate::pixmap::ColorPixmap;

mod baseline;
mod huffman;
mod idct;
mod progressive;

use huffman::HuffmanTable;

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
const MARKER_SOF2: u8 = 0xC2;
const MARKER_COM: u8 = 0xFE;

/// Maximum image dimension for a JPEG payload. Mirrors the PNG
/// decoder's per-dim ceiling so a malicious or malformed font can't
/// blow up `usize` math via a 65535x65535 SOF0.
const MAX_JPEG_DIM: u32 = 16384;

/// Hard cap on Huffman table count. JPEG allows 4 of each AC/DC class,
/// so the worst legitimate case is 8 tables.
const MAX_HUFF_TABLES: usize = 4;

/// Hard cap on quantization table count. Same logic: 4 destinations.
const MAX_QT_TABLES: usize = 4;

// ---------------------------------------------------------------------------
// Public API.
// ---------------------------------------------------------------------------

/// Decode a baseline or progressive JPEG byte slice into a
/// premultiplied RGBA [`ColorPixmap`].
///
/// Supports 8-bit JPEGs with YCbCr (3-component) or grayscale
/// (1-component) data and sampling factors where each component's
/// max h/v is `<= 2`. Both baseline (SOF0) and progressive (SOF2,
/// first-time DC/AC scans plus DC successive-approximation
/// refinement) modes are handled. AC successive-approximation
/// refinement scans, arithmetic coding, 16-bit precision,
/// hierarchical mode, restart markers, and JPEG2000 / TIFF return
/// [`RenderError::BadJpeg`].
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
    /// SOF mode: false = SOF0 baseline, true = SOF2 progressive.
    progressive: bool,
    /// Per-component coefficient buffers. Used only in progressive
    /// mode to accumulate coefficients across multiple SOS scans.
    /// Each buffer holds `num_blocks_x * num_blocks_y * 64` `i16`
    /// entries in zig-zag order.
    coeffs: Vec<Vec<i16>>,
    /// Per-component block grid dimensions (in 8x8 blocks). Sized
    /// to `mcus_x * h_sampling` and `mcus_y * v_sampling` for each
    /// component.
    blocks_per_comp: Vec<(u32, u32)>,
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
            progressive: false,
            coeffs: Vec::new(),
            blocks_per_comp: Vec::new(),
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
                MARKER_SOF0 => self.read_sof(false)?,
                MARKER_SOF2 => self.read_sof(true)?,
                MARKER_SOS => {
                    // SOS header sets per-component huff dests, then
                    // the entropy-coded data starts immediately.
                    if self.progressive {
                        self.read_sos_progressive()?;
                        // Loop continues for subsequent SOS / EOI.
                    } else {
                        return self.read_sos_and_decode();
                    }
                }
                MARKER_EOI if self.progressive => {
                    // Progressive: all scans done, run IDCT + compose.
                    return self.finalize_progressive();
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
                    // APP0..APP15: skip.
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
                    // Unknown marker: try to skip via length.
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

    fn read_sof(&mut self, progressive: bool) -> Result<(), RenderError> {
        let body = self.read_segment()?;
        if body.len() < 6 {
            return Err(RenderError::BadJpeg("truncated SOF"));
        }
        let precision = body[0];
        if precision != 8 {
            return Err(RenderError::BadJpeg("SOF precision not 8-bit"));
        }
        let height = u16::from_be_bytes([body[1], body[2]]);
        let width = u16::from_be_bytes([body[3], body[4]]);
        if width == 0 || height == 0 {
            return Err(RenderError::BadJpeg("SOF zero dimension"));
        }
        if u32::from(width) > MAX_JPEG_DIM || u32::from(height) > MAX_JPEG_DIM {
            return Err(RenderError::BadJpeg("SOF dimension exceeds max"));
        }
        self.width = u32::from(width);
        self.height = u32::from(height);
        self.progressive = progressive;
        let n_comp = body[5] as usize;
        if !matches!(n_comp, 1 | 3) {
            return Err(RenderError::BadJpeg("SOF component count must be 1 or 3"));
        }
        if body.len() < 6 + 3 * n_comp {
            return Err(RenderError::BadJpeg("truncated SOF components"));
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
                return Err(RenderError::BadJpeg("SOF qt_dest out of range"));
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
        if progressive {
            self.allocate_progressive_buffers();
        }
        Ok(())
    }

    /// Allocate per-component coefficient buffers sized to the
    /// component's full block grid. Called after SOF2 parses the
    /// component list.
    fn allocate_progressive_buffers(&mut self) {
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
        self.coeffs = Vec::with_capacity(self.components.len());
        self.blocks_per_comp = Vec::with_capacity(self.components.len());
        for comp in &self.components {
            let bw = mcus_x * u32::from(comp.h_sampling);
            let bh = mcus_y * u32::from(comp.v_sampling);
            self.blocks_per_comp.push((bw, bh));
            self.coeffs.push(vec![0i16; (bw * bh * 64) as usize]);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
