//! Test-only JPEG encoder. It writes given quantized coefficients as a
//! baseline (SOF0) stream or as a progressive (SOF2) stream under any
//! scan script, so decode tests can check that both paths rebuild the
//! same coefficients. Entropy coding follows T.81 Annex G and the
//! libjpeg encoder (`jcphuff.c`), EOB runs included.

use super::*;

/// One frame's quantized coefficients. Every component's blocks cover
/// its whole MCU-padded grid, row-major, each in zig-zag order. The
/// quantization table is all ones, so a coefficient is its dequantized
/// value.
pub(super) struct Image {
    pub(super) width: u16,
    pub(super) height: u16,
    /// `(h, v)` sampling factors per component.
    pub(super) sampling: Vec<(u8, u8)>,
    pub(super) blocks: Vec<Vec<[i16; 64]>>,
}

/// One scan of a progressive script: the components it covers, the
/// band `ss..=se`, and the successive-approximation bits.
#[derive(Clone)]
pub(super) struct Scan {
    pub(super) comps: Vec<usize>,
    pub(super) ss: u8,
    pub(super) se: u8,
    pub(super) ah: u8,
    pub(super) al: u8,
}

pub(super) const fn scan(comps: Vec<usize>, ss: u8, se: u8, ah: u8, al: u8) -> Scan {
    Scan {
        comps,
        ss,
        se,
        ah,
        al,
    }
}

impl Image {
    fn max_sampling(&self) -> (u32, u32) {
        let h = self.sampling.iter().map(|s| u32::from(s.0)).max();
        let v = self.sampling.iter().map(|s| u32::from(s.1)).max();
        (h.unwrap_or(1), v.unwrap_or(1))
    }

    fn mcus(&self) -> (u32, u32) {
        let (h, v) = self.max_sampling();
        (
            u32::from(self.width).div_ceil(8 * h),
            u32::from(self.height).div_ceil(8 * v),
        )
    }

    /// The MCU-padded block grid of component `ci`.
    fn grid(&self, ci: usize) -> (u32, u32) {
        let (mx, my) = self.mcus();
        let (h, v) = self.sampling[ci];
        (mx * u32::from(h), my * u32::from(v))
    }

    /// The blocks a non-interleaved scan of component `ci` visits.
    fn scan_grid(&self, ci: usize) -> (u32, u32) {
        let (mh, mv) = self.max_sampling();
        let (h, v) = self.sampling[ci];
        let w = (u32::from(self.width) * u32::from(h)).div_ceil(mh);
        let hh = (u32::from(self.height) * u32::from(v)).div_ceil(mv);
        (w.div_ceil(8), hh.div_ceil(8))
    }

    /// `(component, block index)` in the order a scan over `comps`
    /// visits them: MCU order when it interleaves several components,
    /// the component's own sample grid when it holds one.
    fn scan_order(&self, comps: &[usize]) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        if let &[ci] = comps {
            let (cols, rows) = self.scan_grid(ci);
            let (stride, _) = self.grid(ci);
            for by in 0..rows {
                for bx in 0..cols {
                    out.push((ci, (by * stride + bx) as usize));
                }
            }
            return out;
        }
        let (mx, my) = self.mcus();
        for mcu_y in 0..my {
            for mcu_x in 0..mx {
                for &ci in comps {
                    let (h, v) = self.sampling[ci];
                    let (stride, _) = self.grid(ci);
                    for by in 0..u32::from(v) {
                        for bx in 0..u32::from(h) {
                            let x = mcu_x * u32::from(h) + bx;
                            let y = mcu_y * u32::from(v) + by;
                            out.push((ci, (y * stride + x) as usize));
                        }
                    }
                }
            }
        }
        out
    }

    /// Pseudo-random coefficients in `-amplitude..=amplitude`, with
    /// about one coefficient in `sparsity` nonzero past the DC, so
    /// scans see runs of zeros, ZRLs, and EOB runs.
    pub(super) fn random(
        width: u16,
        height: u16,
        sampling: &[(u8, u8)],
        seed: u32,
        amplitude: i16,
        sparsity: u32,
    ) -> Self {
        let mut img = Self {
            width,
            height,
            sampling: sampling.to_vec(),
            blocks: Vec::new(),
        };
        let mut state = seed | 1;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        let span = 2 * u32::from(amplitude.unsigned_abs()) + 1;
        for ci in 0..sampling.len() {
            let (bw, bh) = img.grid(ci);
            let mut blocks = Vec::new();
            for _ in 0..bw * bh {
                let mut block = [0i16; 64];
                for (k, c) in block.iter_mut().enumerate() {
                    if k == 0 || next() % sparsity == 0 {
                        *c = (next() % span) as i16 - amplitude;
                    }
                }
                blocks.push(block);
            }
            img.blocks.push(blocks);
        }
        img
    }
}

/// A Huffman table that codes all 256 symbols: 128 eight-bit codes and
/// 128 nine-bit codes. Returns the DHT counts and symbols.
fn flat_table() -> ([u8; 16], Vec<u8>) {
    let mut counts = [0u8; 16];
    counts[7] = 128;
    counts[8] = 128;
    (counts, (0..=255).collect())
}

/// Canonical `(code, length)` per symbol of [`flat_table`].
fn flat_codes() -> Vec<(u32, u8)> {
    let (counts, symbols) = flat_table();
    let mut codes = vec![(0u32, 0u8); 256];
    let mut code = 0u32;
    let mut next = symbols.iter();
    for (len_minus_one, &count) in counts.iter().enumerate() {
        for _ in 0..count {
            let sym = *next.next().unwrap();
            codes[usize::from(sym)] = (code, len_minus_one as u8 + 1);
            code += 1;
        }
        code <<= 1;
    }
    codes
}

/// Magnitude category and the bits that follow it (T.81 F.1.2.1).
fn magnitude(v: i32) -> (u8, u32) {
    if v == 0 {
        return (0, 0);
    }
    let size = 32 - v.unsigned_abs().leading_zeros();
    let bits = if v < 0 { v - 1 } else { v } as u32 & ((1u32 << size) - 1);
    (size as u8, bits)
}

struct Entropy {
    out: BitWriter,
    codes: Vec<(u32, u8)>,
}

impl Entropy {
    fn new() -> Self {
        Self {
            out: BitWriter::default(),
            codes: flat_codes(),
        }
    }

    fn symbol(&mut self, sym: u8) {
        let (code, len) = self.codes[usize::from(sym)];
        self.out.write_bits(code, len);
    }

    fn bits(&mut self, bits: u32, n: u8) {
        if n > 0 {
            self.out.write_bits(bits, n);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        self.out.flush();
        self.out.bytes
    }
}

/// SOI, DQT, SOF, and DHT for `img`.
fn header(img: &Image, sof: u8) -> Vec<u8> {
    let mut out = vec![0xFF, MARKER_SOI, 0xFF, MARKER_DQT, 0x00, 67, 0x00];
    out.extend_from_slice(&[1u8; 64]);
    let n = img.sampling.len() as u8;
    out.extend_from_slice(&[0xFF, sof]);
    out.extend_from_slice(&(8u16 + 3 * u16::from(n)).to_be_bytes());
    out.push(8);
    out.extend_from_slice(&img.height.to_be_bytes());
    out.extend_from_slice(&img.width.to_be_bytes());
    out.push(n);
    for (i, &(h, v)) in img.sampling.iter().enumerate() {
        out.extend_from_slice(&[i as u8 + 1, (h << 4) | v, 0]);
    }
    let (counts, symbols) = flat_table();
    for class in [0x00u8, 0x10] {
        out.extend_from_slice(&[0xFF, MARKER_DHT]);
        out.extend_from_slice(&(2u16 + 17 + symbols.len() as u16).to_be_bytes());
        out.push(class);
        out.extend_from_slice(&counts);
        out.extend_from_slice(&symbols);
    }
    out
}

fn sos(out: &mut Vec<u8>, comps: &[usize], ss: u8, se: u8, ah: u8, al: u8) {
    out.extend_from_slice(&[0xFF, MARKER_SOS]);
    out.extend_from_slice(&(6u16 + 2 * comps.len() as u16).to_be_bytes());
    out.push(comps.len() as u8);
    for &ci in comps {
        out.extend_from_slice(&[ci as u8 + 1, 0x00]);
    }
    out.extend_from_slice(&[ss, se, (ah << 4) | al]);
}

/// `img` as a baseline (SOF0) stream: one interleaved scan of every
/// component.
pub(super) fn encode_baseline(img: &Image) -> Vec<u8> {
    let mut out = header(img, MARKER_SOF0);
    let comps: Vec<usize> = (0..img.sampling.len()).collect();
    sos(&mut out, &comps, 0, 63, 0, 0);
    let mut e = Entropy::new();
    let mut pred = vec![0i32; comps.len()];
    for (ci, b) in img.scan_order(&comps) {
        let block = &img.blocks[ci][b];
        let dc = i32::from(block[0]);
        let (size, bits) = magnitude(dc - pred[ci]);
        pred[ci] = dc;
        e.symbol(size);
        e.bits(bits, size);
        let mut run = 0u8;
        for &c in &block[1..] {
            if c == 0 {
                run += 1;
                continue;
            }
            while run > 15 {
                e.symbol(0xF0);
                run -= 16;
            }
            let (size, bits) = magnitude(i32::from(c));
            e.symbol((run << 4) | size);
            e.bits(bits, size);
            run = 0;
        }
        if run > 0 {
            e.symbol(0x00);
        }
    }
    out.extend_from_slice(&e.finish());
    out.extend_from_slice(&[0xFF, MARKER_EOI]);
    out
}

/// `img` as a progressive (SOF2) stream under `script`.
pub(super) fn encode_progressive(img: &Image, script: &[Scan]) -> Vec<u8> {
    let mut out = header(img, MARKER_SOF2);
    for s in script {
        sos(&mut out, &s.comps, s.ss, s.se, s.ah, s.al);
        let mut e = Entropy::new();
        let order = img.scan_order(&s.comps);
        match (s.ss, s.ah) {
            (0, 0) => dc_first(&mut e, img, &order, s.al),
            (0, _) => {
                for &(ci, b) in &order {
                    e.bits((img.blocks[ci][b][0] >> s.al) as u32 & 1, 1);
                }
            }
            (_, 0) => ac_first(&mut e, img, &order, s),
            _ => panic!("AC refinement scans are not encoded yet"),
        }
        out.extend_from_slice(&e.finish());
    }
    out.extend_from_slice(&[0xFF, MARKER_EOI]);
    out
}

fn dc_first(e: &mut Entropy, img: &Image, order: &[(usize, usize)], al: u8) {
    let mut pred = vec![0i32; img.sampling.len()];
    for &(ci, b) in order {
        // Arithmetic shift, as libjpeg's point transform for DC.
        let dc = i32::from(img.blocks[ci][b][0]) >> al;
        let (size, bits) = magnitude(dc - pred[ci]);
        pred[ci] = dc;
        e.symbol(size);
        e.bits(bits, size);
    }
}

/// Writes a pending EOB run: the EOBn symbol and its extra bits.
fn emit_eob_run(e: &mut Entropy, eob_run: &mut u32) {
    if *eob_run == 0 {
        return;
    }
    let nbits = 31 - eob_run.leading_zeros();
    e.symbol((nbits as u8) << 4);
    e.bits(*eob_run, nbits as u8);
    *eob_run = 0;
}

/// AC point transform: the magnitude shifted right, sign kept.
fn ac_point(c: i16, al: u8) -> i32 {
    let m = i32::from(c.unsigned_abs()) >> al;
    if c < 0 {
        -m
    } else {
        m
    }
}

fn ac_first(e: &mut Entropy, img: &Image, order: &[(usize, usize)], s: &Scan) {
    let mut eob_run = 0u32;
    for &(ci, b) in order {
        let block = &img.blocks[ci][b];
        let mut run = 0u8;
        for &c in &block[usize::from(s.ss)..=usize::from(s.se)] {
            let v = ac_point(c, s.al);
            if v == 0 {
                run += 1;
                continue;
            }
            emit_eob_run(e, &mut eob_run);
            while run > 15 {
                e.symbol(0xF0);
                run -= 16;
            }
            let (size, bits) = magnitude(v);
            e.symbol((run << 4) | size);
            e.bits(bits, size);
            run = 0;
        }
        if run > 0 {
            eob_run += 1;
            if eob_run == 0x7FFF {
                emit_eob_run(e, &mut eob_run);
            }
        }
    }
    emit_eob_run(e, &mut eob_run);
}

/// Spectral selection only, every scan at full precision: an
/// interleaved DC scan, then two AC bands per component.
pub(super) fn spectral_script(n_comps: usize) -> Vec<Scan> {
    let all: Vec<usize> = (0..n_comps).collect();
    let mut script = vec![scan(all, 0, 0, 0, 0)];
    for ci in (0..n_comps).rev() {
        script.push(scan(vec![ci], 1, 5, 0, 0));
    }
    for ci in 0..n_comps {
        script.push(scan(vec![ci], 6, 63, 0, 0));
    }
    script
}
