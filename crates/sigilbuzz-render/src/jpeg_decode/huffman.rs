//! Huffman tables, the byte-stuffing-aware entropy bit reader, and
//! baseline block coefficient decode.

use alloc::vec;
use alloc::vec::Vec;

use crate::error::RenderError;

#[derive(Default, Clone)]
pub(super) struct HuffmanTable {
    /// Number of codes of each length 1..=16.
    /// Indexed 0..16 where index `i` is the count of length `i + 1`.
    pub(super) counts: [u8; 16],
    /// Symbol values, in the order they appear in the DHT segment.
    pub(super) symbols: Vec<u8>,
    /// Decode lookup: maps `code` (right-padded to 16 bits) -> (symbol,
    /// code length). We build a flat 16-bit table during `finalize`.
    /// `lookup[code]` is `(symbol, length)` for any 16-bit code whose
    /// top `length` bits match the canonical code, with `length = 0`
    /// meaning "no code at this prefix" (decode error).
    pub(super) lookup: Vec<(u8, u8)>,
}

impl HuffmanTable {
    /// Build the canonical-code lookup table from `counts` + `symbols`.
    /// Returns `BadJpeg` if the table is malformed (overflows the
    /// 16-bit code space or symbol count exceeds the running tally).
    pub(super) fn finalize(&mut self) -> Result<(), RenderError> {
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

// ---------------------------------------------------------------------------
// Bit-reader over the entropy-coded segment. JPEG entropy data uses
// byte stuffing: every literal `0xFF` is followed by a `0x00` which
// must be skipped. A `0xFF` followed by anything else is a marker
// (typically EOI) and ends the stream.
// ---------------------------------------------------------------------------

pub(super) struct BitReader<'a> {
    src: &'a [u8],
    pub(super) pos: usize,
    /// Right-aligned bit buffer of 0..=24 bits.
    buf: u32,
    /// Bits currently valid in `buf`.
    len: u8,
    /// Once we hit a marker (0xFF nn with nn != 0x00) the stream is
    /// ended; further reads return 0 (zero-padding behavior matches
    /// libjpeg for tail-truncated streams).
    eos: bool,
}

impl<'a> BitReader<'a> {
    pub(super) fn new(src: &'a [u8]) -> Self {
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
                    // Stuffed byte: consume and emit literal 0xFF.
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

    pub(super) fn read_bits(&mut self, n: u8) -> u32 {
        let v = self.peek_bits(n);
        self.consume(n);
        v
    }

    /// Decode a Huffman-coded symbol against `tbl`.
    pub(super) fn decode_huff(&mut self, tbl: &HuffmanTable) -> Result<u8, RenderError> {
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

/// Sign-extend an `n`-bit JPEG receive value: top bit 1 -> positive,
/// top bit 0 -> negative, where the negative range is
/// `-(2^n - 1) ..= -2^(n-1)`.
pub(super) fn extend(v: u32, n: u8) -> i32 {
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
pub(super) const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

pub(super) fn decode_block(
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
    // The running DC predictor is unbounded across blocks, so the
    // product can leave i32 range on hostile streams. Wrap the way a
    // release build always has.
    out[0] = prev_dc.wrapping_mul(qt[0]);

    // AC coefficients.
    let mut k = 1;
    while k < 64 {
        let rs = br.decode_huff(ac_tbl)?;
        let run = (rs >> 4) as usize;
        let size = rs & 0x0F;
        if size == 0 {
            if run == 15 {
                // ZRL: 16 zeros, then continue.
                k += 16;
                continue;
            }
            // EOB: rest of block is zero.
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
