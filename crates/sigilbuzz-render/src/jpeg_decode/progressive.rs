//! Progressive (SOF2) scan decode: per-band coefficient accumulation
//! across SOS segments, then IDCT and compose at EOI.

use alloc::vec;
use alloc::vec::Vec;

use super::huffman::{extend, BitReader, ZIGZAG};
use super::idct::idct;
use super::Decoder;
use crate::error::RenderError;
use crate::pixmap::ColorPixmap;

impl Decoder<'_> {
    // -----------------------------------------------------------------
    // Progressive (SOF2) entropy decode.
    //
    // Each SOS in a progressive stream covers a coefficient band
    // `Ss..=Se` at successive-approximation bits `Ah` (already-set high
    // bits) and `Al` (low bit being set this scan). Multiple SOS
    // markers fill in different bands until EOI; only then do we run
    // IDCT + compose.
    //
    // Spec: ITU-T T.81 §F.2.
    // -----------------------------------------------------------------

    pub(super) fn read_sos_progressive(&mut self) -> Result<(), RenderError> {
        let body = self.read_segment()?;
        if body.is_empty() {
            return Err(RenderError::BadJpeg("empty SOS"));
        }
        let n_scan = body[0] as usize;
        if n_scan == 0 || n_scan > self.components.len() {
            return Err(RenderError::BadJpeg("SOS component count out of range"));
        }
        if body.len() < 1 + 2 * n_scan + 3 {
            return Err(RenderError::BadJpeg("truncated SOS"));
        }
        // Parse per-scan component selectors and resolve to indices in
        // the SOF component list (preserving order from SOS).
        let mut scan_indices: Vec<usize> = Vec::with_capacity(n_scan);
        for c in 0..n_scan {
            let off = 1 + 2 * c;
            let id = body[off];
            let td_ta = body[off + 1];
            let dc = td_ta >> 4;
            let ac = td_ta & 0x0F;
            let comp_idx = self
                .components
                .iter()
                .position(|cs| cs.id == id)
                .ok_or(RenderError::BadJpeg("SOS component id not in SOF"))?;
            self.components[comp_idx].dc_huff = dc;
            self.components[comp_idx].ac_huff = ac;
            scan_indices.push(comp_idx);
        }
        let tail = &body[1 + 2 * n_scan..];
        let ss = tail[0];
        let se = tail[1];
        let ah = tail[2] >> 4;
        let al = tail[2] & 0x0F;

        // Validate band parameters per T.81 §F.2.2.1.
        if ss > 63 || se > 63 {
            return Err(RenderError::BadJpeg("SOS Ss/Se out of range"));
        }
        let is_dc = ss == 0;
        if is_dc {
            // DC scans must have Se=0 and may include multiple comps.
            if se != 0 {
                return Err(RenderError::BadJpeg("progressive DC scan Se != 0"));
            }
        } else {
            // AC scans: Ss must be <= Se, and must cover exactly one
            // component (T.81 §F.1.4.2).
            if ss > se {
                return Err(RenderError::BadJpeg("progressive AC scan Ss > Se"));
            }
            if n_scan != 1 {
                return Err(RenderError::BadJpeg(
                    "progressive AC scan must be single-component",
                ));
            }
        }
        if al > 13 || ah > 13 {
            return Err(RenderError::BadJpeg("progressive Ah/Al out of range"));
        }

        // AC successive-approximation refinement is thorny
        // (bit-plane walking over the existing nonzero coefficients),
        // and the deferred-scope note in the module-level docs makes
        // this an explicit non-goal for the PR. Surface it before any
        // table validation so callers see a stable error message
        // regardless of the upstream stream's table layout.
        if !is_dc && ah != 0 {
            return Err(RenderError::BadJpeg(
                "progressive AC refinement scans not supported",
            ));
        }

        // Validate Huffman tables for the scan participants.
        for &ci in &scan_indices {
            let comp = &self.components[ci];
            if is_dc && self.dc_huff[comp.dc_huff as usize].is_none() {
                return Err(RenderError::BadJpeg("missing DC Huffman table"));
            }
            if !is_dc && self.ac_huff[comp.ac_huff as usize].is_none() {
                return Err(RenderError::BadJpeg("missing AC Huffman table"));
            }
        }

        // Hand off to the appropriate scan handler. The bit reader
        // consumes from `self.cursor`; on completion we advance the
        // cursor past the entropy bytes it consumed (up to but not
        // including the next marker).
        let consumed = {
            let entropy = &self.src[self.cursor..];
            let mut br = BitReader::new(entropy);
            if is_dc {
                if ah == 0 {
                    self.scan_dc_first(&mut br, &scan_indices, al)?;
                } else {
                    self.scan_dc_refine(&mut br, &scan_indices, al)?;
                }
            } else {
                self.scan_ac_first(&mut br, scan_indices[0], ss, se, al)?;
            }
            br.pos
        };
        self.cursor += consumed;
        Ok(())
    }

    /// First-pass DC scan (Ah == 0). Reads one DC coefficient per
    /// 8x8 block in MCU order and writes its value, point-shifted
    /// left by `al`, into the coefficient buffer at zig-zag index 0.
    fn scan_dc_first(
        &mut self,
        br: &mut BitReader<'_>,
        scan_indices: &[usize],
        al: u8,
    ) -> Result<(), RenderError> {
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
        let mcus_x = self.width.div_ceil(u32::from(max_h) * 8);
        let mcus_y = self.height.div_ceil(u32::from(max_v) * 8);
        let mut prev_dc = vec![0i32; self.components.len()];

        // Single-component scans iterate the component's own block
        // grid; multi-component scans walk in MCU order.
        if scan_indices.len() == 1 {
            let ci = scan_indices[0];
            let (bw, bh) = self.blocks_per_comp[ci];
            for by in 0..bh {
                for bx in 0..bw {
                    self.decode_dc_first_block(br, ci, bx, by, &mut prev_dc[ci], al)?;
                }
            }
        } else {
            for mcu_y in 0..mcus_y {
                for mcu_x in 0..mcus_x {
                    for &ci in scan_indices {
                        let comp = self.components[ci];
                        let h = u32::from(comp.h_sampling);
                        let v = u32::from(comp.v_sampling);
                        for by in 0..v {
                            for bx in 0..h {
                                let block_x = mcu_x * h + bx;
                                let block_y = mcu_y * v + by;
                                self.decode_dc_first_block(
                                    br,
                                    ci,
                                    block_x,
                                    block_y,
                                    &mut prev_dc[ci],
                                    al,
                                )?;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn decode_dc_first_block(
        &mut self,
        br: &mut BitReader<'_>,
        ci: usize,
        bx: u32,
        by: u32,
        prev_dc: &mut i32,
        al: u8,
    ) -> Result<(), RenderError> {
        let comp = self.components[ci];
        let dc_tbl = self.dc_huff[comp.dc_huff as usize]
            .as_ref()
            .expect("validated");
        let t = br.decode_huff(dc_tbl)?;
        if t > 15 {
            return Err(RenderError::BadJpeg("invalid DC magnitude"));
        }
        let raw = br.read_bits(t);
        let diff = extend(raw, t);
        *prev_dc = prev_dc.wrapping_add(diff);
        let (bw, _bh) = self.blocks_per_comp[ci];
        let block_idx = (by * bw + bx) as usize;
        let coeff_off = block_idx * 64;
        // Point-transform: shift left by `al`. The value can fit in
        // i16 because JPEG DC differences are bounded by ±2^11.
        self.coeffs[ci][coeff_off] = ((*prev_dc) << al) as i16;
        Ok(())
    }

    /// Refinement DC scan (Ah > 0). Reads one bit per block and ORs
    /// it into bit position `al` of the existing DC coefficient.
    fn scan_dc_refine(
        &mut self,
        br: &mut BitReader<'_>,
        scan_indices: &[usize],
        al: u8,
    ) -> Result<(), RenderError> {
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
        let mcus_x = self.width.div_ceil(u32::from(max_h) * 8);
        let mcus_y = self.height.div_ceil(u32::from(max_v) * 8);

        if scan_indices.len() == 1 {
            let ci = scan_indices[0];
            let (bw, bh) = self.blocks_per_comp[ci];
            for by in 0..bh {
                for bx in 0..bw {
                    self.refine_dc_block(br, ci, bx, by, al);
                }
            }
        } else {
            for mcu_y in 0..mcus_y {
                for mcu_x in 0..mcus_x {
                    for &ci in scan_indices {
                        let comp = self.components[ci];
                        let h = u32::from(comp.h_sampling);
                        let v = u32::from(comp.v_sampling);
                        for by in 0..v {
                            for bx in 0..h {
                                let block_x = mcu_x * h + bx;
                                let block_y = mcu_y * v + by;
                                self.refine_dc_block(br, ci, block_x, block_y, al);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn refine_dc_block(&mut self, br: &mut BitReader<'_>, ci: usize, bx: u32, by: u32, al: u8) {
        let bit = br.read_bits(1);
        if bit != 0 {
            let (bw, _bh) = self.blocks_per_comp[ci];
            let coeff_off = ((by * bw + bx) as usize) * 64;
            self.coeffs[ci][coeff_off] |= 1i16 << al;
        }
    }

    /// First-pass AC scan (Ah == 0). Walks the component's blocks in
    /// row-major order and decodes run/value pairs over the band
    /// `ss..=se`, including EOB-run tracking for large skips.
    fn scan_ac_first(
        &mut self,
        br: &mut BitReader<'_>,
        ci: usize,
        ss: u8,
        se: u8,
        al: u8,
    ) -> Result<(), RenderError> {
        let comp = self.components[ci];
        let (bw, bh) = self.blocks_per_comp[ci];
        let mut eob_run: u32 = 0;
        // Avoid borrowing &self.ac_huff across &mut self.coeffs.
        // Decode in a tight loop with the Huffman table reference held
        // for just the inner block.
        for by in 0..bh {
            for bx in 0..bw {
                let block_idx = (by * bw + bx) as usize;
                let coeff_off = block_idx * 64;
                if eob_run > 0 {
                    eob_run -= 1;
                    continue;
                }
                let ac_tbl = self.ac_huff[comp.ac_huff as usize]
                    .as_ref()
                    .expect("validated");
                let mut k = ss;
                while k <= se {
                    let rs = br.decode_huff(ac_tbl)?;
                    let run = rs >> 4;
                    let size = rs & 0x0F;
                    if size == 0 {
                        if run == 15 {
                            // ZRL: 16 zero coefficients.
                            k = k.saturating_add(16);
                            continue;
                        }
                        // EOBn: skip 2^run blocks (this one + run more).
                        eob_run = (1u32 << run) - 1;
                        if run > 0 {
                            eob_run += br.read_bits(run);
                        }
                        break;
                    }
                    k = k.saturating_add(run);
                    if k > se {
                        return Err(RenderError::BadJpeg("AC run overflow in band"));
                    }
                    let raw = br.read_bits(size);
                    let val = extend(raw, size);
                    let nat = ZIGZAG[k as usize];
                    self.coeffs[ci][coeff_off + nat] = (val << al) as i16;
                    k = k.saturating_add(1);
                }
            }
        }
        Ok(())
    }

    /// Run IDCT over each component's accumulated coefficient buffer
    /// and compose into the final RGBA pixmap.
    pub(super) fn finalize_progressive(&mut self) -> Result<ColorPixmap, RenderError> {
        // Validate quantization tables for every component.
        for comp in &self.components {
            if self.qt[comp.qt_dest as usize].is_none() {
                return Err(RenderError::BadJpeg("missing quantization table"));
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

        for (ci, comp) in self.components.iter().enumerate() {
            let (bw, bh) = self.blocks_per_comp[ci];
            let qt = self.qt[comp.qt_dest as usize].as_ref().expect("validated");
            let stride = plane_strides[ci];
            for by in 0..bh {
                for bx in 0..bw {
                    let coeff_off = ((by * bw + bx) as usize) * 64;
                    // Dequantize + de-zig-zag into a natural-order
                    // buffer the IDCT consumes. The accumulator is in
                    // zig-zag order with the DC at index 0.
                    let mut natural = [0i32; 64];
                    natural[0] = i32::from(self.coeffs[ci][coeff_off]) * qt[0];
                    for k in 1..64 {
                        let v = i32::from(self.coeffs[ci][coeff_off + k]);
                        let nat = ZIGZAG[k];
                        natural[nat] = v * qt[k];
                    }
                    let mut samples = [0u8; 64];
                    idct(&natural, &mut samples);
                    let block_x = (bx * 8) as usize;
                    let block_y = (by * 8) as usize;
                    for j in 0..8 {
                        for i in 0..8 {
                            let dst_idx = (block_y + j) * stride + (block_x + i);
                            if dst_idx < planes[ci].len() {
                                planes[ci][dst_idx] = samples[j * 8 + i];
                            }
                        }
                    }
                }
            }
        }

        Ok(self.compose_planes(&planes, &plane_strides, max_h, max_v))
    }
}
