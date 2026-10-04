//! Progressive (SOF2) scan decode: per-band coefficient accumulation
//! across SOS segments, then IDCT and compose at EOI.

use alloc::vec;
use alloc::vec::Vec;

use super::huffman::{extend, BitReader, ZIGZAG};
use super::idct::{idct_cos_table, idct_with_table};
use super::{table_slot, Decoder};
use crate::error::RenderError;
use crate::pixmap::ColorPixmap;

impl<'a> Decoder<'a> {
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
        let Some(&n_scan) = body.first() else {
            return Err(RenderError::BadJpeg("empty SOS"));
        };
        let n_scan = usize::from(n_scan);
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
        let Some(&[ss, se, ah_al, ..]) = body.get(1 + 2 * n_scan..) else {
            return Err(RenderError::BadJpeg("truncated SOS"));
        };
        let ah = ah_al >> 4;
        let al = ah_al & 0x0F;

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

        // AC successive-approximation refinement (bit-plane walking
        // over the existing nonzero coefficients) is not implemented.
        // Surface it before any table validation so callers see a
        // stable error message regardless of the upstream stream's
        // table layout.
        if !is_dc && ah != 0 {
            return Err(RenderError::BadJpeg(
                "progressive AC refinement scans not supported",
            ));
        }

        // Validate Huffman tables for the scan participants.
        for &ci in &scan_indices {
            let comp = &self.components[ci];
            if is_dc && table_slot(&self.dc_huff, comp.dc_huff).is_none() {
                return Err(RenderError::BadJpeg("missing DC Huffman table"));
            }
            if !is_dc && table_slot(&self.ac_huff, comp.ac_huff).is_none() {
                return Err(RenderError::BadJpeg("missing AC Huffman table"));
            }
        }

        // Hand off to the appropriate scan handler. The bit reader
        // consumes from `self.cursor`; on completion we advance the
        // cursor past the entropy bytes it consumed (up to but not
        // including the next marker).
        let consumed = {
            let src: &'a [u8] = self.src;
            let mut br = BitReader::new(src.get(self.cursor..).unwrap_or_default());
            if is_dc {
                if ah == 0 {
                    self.scan_dc_first(&mut br, &scan_indices, al)?;
                } else {
                    self.scan_dc_refine(&mut br, &scan_indices, al)?;
                }
            } else if let &[ci] = scan_indices.as_slice() {
                self.scan_ac_first(&mut br, ci, ss, se, al)?;
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
        let (mcus_x, mcus_y) = self.mcu_grid();
        let mut prev_dc = vec![0i32; self.components.len()];

        // Single-component scans are non-interleaved: they visit only
        // the blocks that hold the component's samples. Multi-component
        // scans walk in MCU order.
        if let &[ci] = scan_indices {
            let (bw, bh) = self.scan_blocks(ci);
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
        let dc_tbl = table_slot(&self.dc_huff, comp.dc_huff)
            .ok_or(RenderError::BadJpeg("missing DC Huffman table"))?;
        let t = br.decode_huff(dc_tbl)?;
        if t > 15 {
            return Err(RenderError::BadJpeg("invalid DC magnitude"));
        }
        let raw = br.read_bits(t);
        let diff = extend(raw, t);
        *prev_dc = prev_dc.wrapping_add(diff);
        // Point-transform: shift left by `al`. The value can fit in
        // i16 because JPEG DC differences are bounded by ±2^11.
        if let Some(slot) = self.dc_slot(ci, bx, by) {
            *slot = ((*prev_dc) << al) as i16;
        }
        Ok(())
    }

    /// Mutable DC coefficient of block `(bx, by)` in component `ci`,
    /// or `None` when the block lies outside the component's grid.
    fn dc_slot(&mut self, ci: usize, bx: u32, by: u32) -> Option<&mut i16> {
        let &(bw, _bh) = self.blocks_per_comp.get(ci)?;
        let block_idx = (by as usize)
            .checked_mul(bw as usize)?
            .checked_add(bx as usize)?;
        self.coeffs.get_mut(ci)?.get_mut(block_idx.checked_mul(64)?)
    }

    /// Block columns and rows a non-interleaved (single-component)
    /// scan visits for component `ci`: the blocks that hold the
    /// component's own samples, `ceil(ceil(X * H / Hmax) / 8)` by
    /// `ceil(ceil(Y * V / Vmax) / 8)` (T.81 A.2.2). An interleaved
    /// scan pads every component out to whole MCUs, so this can be
    /// smaller than the stored grid, never larger.
    fn scan_blocks(&self, ci: usize) -> (u32, u32) {
        let (Some(comp), Some(&(bw, bh))) = (self.components.get(ci), self.blocks_per_comp.get(ci))
        else {
            return (0, 0);
        };
        let max_h = self
            .components
            .iter()
            .map(|c| u32::from(c.h_sampling))
            .max()
            .unwrap_or(1);
        let max_v = self
            .components
            .iter()
            .map(|c| u32::from(c.v_sampling))
            .max()
            .unwrap_or(1);
        let comp_w = (self.width * u32::from(comp.h_sampling)).div_ceil(max_h);
        let comp_h = (self.height * u32::from(comp.v_sampling)).div_ceil(max_v);
        (comp_w.div_ceil(8).min(bw), comp_h.div_ceil(8).min(bh))
    }

    /// Refinement DC scan (Ah > 0). Reads one bit per block and ORs
    /// it into bit position `al` of the existing DC coefficient.
    fn scan_dc_refine(
        &mut self,
        br: &mut BitReader<'_>,
        scan_indices: &[usize],
        al: u8,
    ) -> Result<(), RenderError> {
        let (mcus_x, mcus_y) = self.mcu_grid();

        if let &[ci] = scan_indices {
            let (bw, bh) = self.scan_blocks(ci);
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
            if let Some(slot) = self.dc_slot(ci, bx, by) {
                *slot |= 1i16 << al;
            }
        }
    }

    /// First-pass AC scan (Ah == 0). An AC scan holds one component
    /// and is non-interleaved, so it walks the blocks that hold the
    /// component's samples in row-major order and decodes run/value
    /// pairs over the band `ss..=se`, including EOB-run tracking for
    /// large skips. Each value lands at its zig-zag index, the order
    /// [`Self::finalize_progressive`] reads the buffer in.
    fn scan_ac_first(
        &mut self,
        br: &mut BitReader<'_>,
        ci: usize,
        ss: u8,
        se: u8,
        al: u8,
    ) -> Result<(), RenderError> {
        let comp = self.components[ci];
        let (cols, rows) = self.scan_blocks(ci);
        let stride = self.blocks_per_comp.get(ci).map_or(0, |&(bw, _)| bw);
        let ac_tbl = table_slot(&self.ac_huff, comp.ac_huff)
            .ok_or(RenderError::BadJpeg("missing AC Huffman table"))?;
        let Some(coeffs) = self.coeffs.get_mut(ci) else {
            return Ok(());
        };
        let mut eob_run: u32 = 0;
        for by in 0..rows {
            for bx in 0..cols {
                let block_idx = (by * stride + bx) as usize;
                let coeff_off = block_idx * 64;
                if eob_run > 0 {
                    eob_run -= 1;
                    continue;
                }
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
                    if let Some(slot) = coeffs.get_mut(coeff_off + usize::from(k)) {
                        *slot = (val << al) as i16;
                    }
                    k = k.saturating_add(1);
                }
            }
        }
        Ok(())
    }

    /// Run IDCT over each component's accumulated coefficient buffer
    /// and compose into the final RGBA pixmap.
    pub(super) fn finalize_progressive(&mut self) -> Result<ColorPixmap, RenderError> {
        // Resolve quantization tables for every component.
        let mut qts: Vec<&[i32; 64]> = Vec::with_capacity(self.components.len());
        for comp in &self.components {
            let qt = table_slot(&self.qt, comp.qt_dest)
                .ok_or(RenderError::BadJpeg("missing quantization table"))?;
            qts.push(qt);
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
        let (mcus_x, mcus_y) = self.mcu_grid();
        let cos = idct_cos_table();

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

        for (ci, (qt, plane)) in qts.iter().zip(planes.iter_mut()).enumerate() {
            let (bw, bh) = self.blocks_per_comp.get(ci).copied().unwrap_or((0, 0));
            let (Some(coeffs), Some(&stride)) = (self.coeffs.get(ci), plane_strides.get(ci)) else {
                continue;
            };
            // One 64-coefficient zig-zag block per grid cell. The buffer
            // was sized from the same grid, so this visits every block.
            for (block_idx, block) in coeffs.chunks_exact(64).enumerate().take((bw * bh) as usize) {
                let bx = block_idx as u32 % bw;
                let by = block_idx as u32 / bw;
                // Dequantize + de-zig-zag into a natural-order
                // buffer the IDCT consumes. The accumulator is in
                // zig-zag order with the DC at index 0.
                let mut natural = [0i32; 64];
                for (k, (&v, &q)) in block.iter().zip(qt.iter()).enumerate() {
                    natural[ZIGZAG[k]] = i32::from(v) * q;
                }
                let mut samples = [0u8; 64];
                idct_with_table(&natural, &mut samples, &cos);
                let block_x = (bx * 8) as usize;
                let block_y = (by * 8) as usize;
                for j in 0..8 {
                    for i in 0..8 {
                        let dst_idx = (block_y + j) * stride + (block_x + i);
                        if let Some(px) = plane.get_mut(dst_idx) {
                            *px = samples[j * 8 + i];
                        }
                    }
                }
            }
        }

        self.compose_planes(&planes, &plane_strides, max_h, max_v)
    }
}
