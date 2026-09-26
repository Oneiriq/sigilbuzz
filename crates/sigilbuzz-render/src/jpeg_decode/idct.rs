//! Inverse DCT from dequantized 8x8 coefficients to level-shifted
//! spatial samples.
// ---------------------------------------------------------------------------
// Inverse DCT: straightforward float-domain DCT-III, applied first
// across rows then across columns. Adequate for tiny font emoji
// bitmaps; not optimized for throughput.
// ---------------------------------------------------------------------------

/// `cos((2 * s + 1) * f * PI / 16)` indexed `[s][f]` for spatial index
/// `s` and frequency `f`. Built once per image so the per-block IDCT
/// does no trigonometry. Each entry uses the exact expression the
/// transform used to evaluate inline, so the output is unchanged.
pub(super) fn idct_cos_table() -> [[f32; 8]; 8] {
    let mut table = [[0.0f32; 8]; 8];
    for (s, row) in table.iter_mut().enumerate() {
        for (f, entry) in row.iter_mut().enumerate() {
            let theta = ((2 * s + 1) as f32) * (f as f32) * core::f32::consts::PI / 16.0;
            *entry = theta.cos();
        }
    }
    table
}

/// Inverse DCT of one block with a freshly built cosine table.
#[cfg(test)]
pub(super) fn idct(coeffs: &[i32; 64], out: &mut [u8; 64]) {
    idct_with_table(coeffs, out, &idct_cos_table());
}

/// Inverse DCT of one block. `cos` comes from [`idct_cos_table`].
pub(super) fn idct_with_table(coeffs: &[i32; 64], out: &mut [u8; 64], cos: &[[f32; 8]; 8]) {
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
                acc += cu * tmp[base + u] * cos[x][u];
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
                acc += cv * work[v * 8 + col] * cos[y][v];
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
