//! Baseline (SOF0) scan decode and the plane-to-RGBA compose step
//! shared with the progressive path.

use alloc::vec;
use alloc::vec::Vec;

use super::huffman::{decode_block, BitReader, HuffmanTable};
use super::idct::{idct_cos_table, idct_with_table};
use super::{table_slot, Decoder};
use crate::error::RenderError;
use crate::pixmap::ColorPixmap;

impl Decoder<'_> {
    pub(super) fn read_sos_and_decode(&mut self) -> Result<ColorPixmap, RenderError> {
        let body = self.read_segment()?;
        let Some(&n_scan) = body.first() else {
            return Err(RenderError::BadJpeg("empty SOS"));
        };
        let n_scan = usize::from(n_scan);
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
        if body.get(1 + 2 * n_scan..).and_then(|t| t.get(..3)) != Some(&[0, 63, 0][..]) {
            return Err(RenderError::BadJpeg("non-baseline scan parameters"));
        }
        if self.components.is_empty() {
            // No frame header yet, so there is no image to decode into.
            return Err(RenderError::BadJpeg("SOS before SOF"));
        }
        // Hand off to the entropy stage. The remainder of `self.src`
        // from `self.cursor` is the entropy-coded segment ending at
        // EOI.
        self.decode_scan()
    }

    fn decode_scan(&mut self) -> Result<ColorPixmap, RenderError> {
        // Resolve every referenced table up front. A selector outside
        // the four table slots reads as a missing table.
        let mut tables: Vec<(&HuffmanTable, &HuffmanTable, &[i32; 64])> =
            Vec::with_capacity(self.components.len());
        for comp in &self.components {
            let qt = table_slot(&self.qt, comp.qt_dest)
                .ok_or(RenderError::BadJpeg("missing quantization table"))?;
            let dc = table_slot(&self.dc_huff, comp.dc_huff)
                .ok_or(RenderError::BadJpeg("missing DC Huffman table"))?;
            let ac = table_slot(&self.ac_huff, comp.ac_huff)
                .ok_or(RenderError::BadJpeg("missing AC Huffman table"))?;
            tables.push((dc, ac, qt));
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

        let mut bit_reader = BitReader::new(self.src.get(self.cursor..).unwrap_or_default());
        let mut prev_dc = vec![0i32; self.components.len()];

        for mcu_y in 0..mcus_y {
            for mcu_x in 0..mcus_x {
                for (ci, (comp, &(dc_tbl, ac_tbl, qt))) in
                    self.components.iter().zip(&tables).enumerate()
                {
                    let h = u32::from(comp.h_sampling);
                    let v = u32::from(comp.v_sampling);
                    for by in 0..v {
                        for bx in 0..h {
                            let mut coeffs = [0i32; 64];
                            decode_block(
                                &mut bit_reader,
                                dc_tbl,
                                ac_tbl,
                                qt,
                                &mut prev_dc[ci],
                                &mut coeffs,
                            )?;
                            let mut samples = [0u8; 64];
                            idct_with_table(&coeffs, &mut samples, &cos);
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

        self.compose_planes(&planes, &plane_strides, max_h, max_v)
    }

    /// Build the final RGBA `ColorPixmap` from per-component sample
    /// planes. Shared by baseline and progressive paths.
    pub(super) fn compose_planes(
        &self,
        planes: &[Vec<u8>],
        plane_strides: &[usize],
        max_h: u8,
        max_v: u8,
    ) -> Result<ColorPixmap, RenderError> {
        let w = self.width as usize;
        let h = self.height as usize;
        let mut out = ColorPixmap::new(self.width, self.height);

        match (self.components.as_slice(), planes, plane_strides) {
            ([_], [plane], &[stride]) => {
                // Grayscale.
                for y in 0..h {
                    for x in 0..w {
                        let g = plane[y * stride + x];
                        let off = (y * w + x) * 4;
                        out.data[off] = g;
                        out.data[off + 1] = g;
                        out.data[off + 2] = g;
                        out.data[off + 3] = 255;
                    }
                }
            }
            (
                [luma, blue, red],
                [plane_y, plane_cb, plane_cr],
                &[stride_y, stride_cb, stride_cr],
            ) => {
                // YCbCr -> RGB. Sample chroma via nearest-neighbor at the
                // luma grid: pixel (x, y) in luma maps to
                // (x * h_chroma / max_h, y * v_chroma / max_v) in chroma.
                let (h_y, v_y) = (u32::from(luma.h_sampling), u32::from(luma.v_sampling));
                let (h_cb, v_cb) = (u32::from(blue.h_sampling), u32::from(blue.v_sampling));
                let (h_cr, v_cr) = (u32::from(red.h_sampling), u32::from(red.v_sampling));
                let max_h_u = u32::from(max_h);
                let max_v_u = u32::from(max_v);
                for y in 0..h {
                    for x in 0..w {
                        let yx = (x as u32) * h_y / max_h_u;
                        let yy = (y as u32) * v_y / max_v_u;
                        let cbx = (x as u32) * h_cb / max_h_u;
                        let cby = (y as u32) * v_cb / max_v_u;
                        let crx = (x as u32) * h_cr / max_h_u;
                        let cry = (y as u32) * v_cr / max_v_u;
                        let yv = i32::from(plane_y[yy as usize * stride_y + yx as usize]);
                        let cb = i32::from(plane_cb[cby as usize * stride_cb + cbx as usize]) - 128;
                        let cr = i32::from(plane_cr[cry as usize * stride_cr + crx as usize]) - 128;
                        // ITU-R BT.601 in fixed-point Q16.
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
            // SOF only accepts one or three components and every caller
            // builds one plane per component, so this arm only guards
            // against a frame that never declared its components.
            _ => return Err(RenderError::BadJpeg("unsupported component layout")),
        }

        Ok(out)
    }
}

#[inline]
fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}
