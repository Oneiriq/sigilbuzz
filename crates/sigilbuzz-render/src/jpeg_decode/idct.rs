//! Inverse DCT from dequantized 8x8 coefficients to level-shifted
//! spatial samples.
// ---------------------------------------------------------------------------
// Inverse DCT: straightforward float-domain DCT-III, applied first
// across rows then across columns. Adequate for tiny font emoji
// bitmaps; not optimized for throughput.
// ---------------------------------------------------------------------------

#[allow(clippy::excessive_precision)]
pub(super) fn idct(coeffs: &[i32; 64], out: &mut [u8; 64]) {
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
