//! Bilinear resampling of decoded bitmap strikes to the requested size.

use super::MAX_BITMAP_DIM;
use crate::pixmap::ColorPixmap;

// ---------------------------------------------------------------------------
// Bilinear rescale.
// ---------------------------------------------------------------------------

/// Bilinearly resample `src` to a new size. Operates on premultiplied
/// RGBA so the alpha channel stays consistent with the rest of the
/// render pipeline; sampling premul is the right thing here because
/// edges that are partly transparent already have their colors
/// scaled by alpha.
///
/// Degenerate cases:
/// - `src` empty or `dst_w == 0 || dst_h == 0` -> empty pixmap.
/// - `dst_w == src.width && dst_h == src.height` -> clone of `src`.
#[must_use]
pub fn rescale_bilinear(src: &ColorPixmap, dst_w: u32, dst_h: u32) -> ColorPixmap {
    if src.is_empty() || dst_w == 0 || dst_h == 0 {
        return ColorPixmap::new(0, 0);
    }
    if dst_w == src.width && dst_h == src.height {
        return src.clone();
    }
    // Cap target dimensions: an out-of-range `dst_w` / `dst_h` (e.g.
    // a caller miscomputing from a hostile size_pt) would otherwise
    // panic in `vec![0u8; w*h*4]`. The ceiling matches the PNG
    // decoder's bound; callers that need larger surfaces
    // should resample in tiles.
    if dst_w as f32 > MAX_BITMAP_DIM || dst_h as f32 > MAX_BITMAP_DIM {
        return ColorPixmap::new(0, 0);
    }
    let mut out = ColorPixmap::new(dst_w, dst_h);
    let sw = src.width as f32;
    let sh = src.height as f32;
    let dw = dst_w as f32;
    let dh = dst_h as f32;
    // Map dst pixel centers to src space. The half-pixel offset keeps
    // the rescale edge-aligned: a 2x upscale of a 2-px image lands the
    // first dst pixel at src x = 0.25 etc.
    for y in 0..dst_h {
        let sy = ((y as f32 + 0.5) * sh / dh) - 0.5;
        let y0 = sy.floor().max(0.0) as u32;
        let y1 = y0.saturating_add(1).min(src.height - 1);
        let fy = (sy - y0 as f32).clamp(0.0, 1.0);
        for x in 0..dst_w {
            let sx = ((x as f32 + 0.5) * sw / dw) - 0.5;
            let x0 = sx.floor().max(0.0) as u32;
            let x1 = x0.saturating_add(1).min(src.width - 1);
            let fx = (sx - x0 as f32).clamp(0.0, 1.0);
            let p00 = src.get(x0, y0);
            let p10 = src.get(x1, y0);
            let p01 = src.get(x0, y1);
            let p11 = src.get(x1, y1);
            let mut rgba = [0u8; 4];
            for c in 0..4 {
                let top = lerp(p00[c] as f32, p10[c] as f32, fx);
                let bot = lerp(p01[c] as f32, p11[c] as f32, fx);
                let v = lerp(top, bot, fy);
                rgba[c] = v.clamp(0.0, 255.0).round() as u8;
            }
            let idx = (y as usize * dst_w as usize + x as usize) * 4;
            out.data[idx..idx + 4].copy_from_slice(&rgba);
        }
    }
    out
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
