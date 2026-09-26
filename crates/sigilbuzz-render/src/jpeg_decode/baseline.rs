//! Baseline (SOF0) scan decode and the plane-to-RGBA compose step
//! shared with the progressive path.

use alloc::vec;
use alloc::vec::Vec;

use super::huffman::{decode_block, BitReader};
use super::idct::idct;
use super::Decoder;
use crate::error::RenderError;
use crate::pixmap::ColorPixmap;

impl Decoder<'_> {
    pub(super) fn read_sos_and_decode(&mut self) -> Result<ColorPixmap, RenderError> {
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

        Ok(self.compose_planes(&planes, &plane_strides, max_h, max_v))
    }

    /// Build the final RGBA `ColorPixmap` from per-component sample
    /// planes. Shared by baseline and progressive paths.
    pub(super) fn compose_planes(
        &self,
        planes: &[Vec<u8>],
        plane_strides: &[usize],
        max_h: u8,
        max_v: u8,
    ) -> ColorPixmap {
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
            // YCbCr -> RGB. Sample chroma via nearest-neighbor at the
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
                    let yx = (x as u32) * h_y / max_h_u;
                    let yy = (y as u32) * v_y / max_v_u;
                    let cbx = (x as u32) * h_cb / max_h_u;
                    let cby = (y as u32) * v_cb / max_v_u;
                    let crx = (x as u32) * h_cr / max_h_u;
                    let cry = (y as u32) * v_cr / max_v_u;
                    let yv = i32::from(planes[0][yy as usize * stride_y + yx as usize]);
                    let cb = i32::from(planes[1][cby as usize * stride_cb + cbx as usize]) - 128;
                    let cr = i32::from(planes[2][cry as usize * stride_cr + crx as usize]) - 128;
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

        out
    }
}

#[inline]
fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}
