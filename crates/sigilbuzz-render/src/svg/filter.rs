//! `<filter>` support: parsing the primitive chain and running it
//! against a rendered fill.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::pixmap::ColorPixmap;

use super::document::{node_cost, Defs};
use super::model::{Filter, FilterOp, FilterPrimitive};
use super::style::{parse_color, parse_length, parse_opacity};
use super::xml::{name_eq, Node};
use super::{MAX_BLUR_RADIUS, MAX_FILTER_PRIMITIVES};

/// Resolves a `<filter id="...">` definition into a [`Filter`] record.
/// Unknown / malformed primitives are skipped silently. The rest of
/// the chain still runs. Primitives past [`MAX_FILTER_PRIMITIVES`] are
/// ignored. Returns `None` if the id doesn't point at a `<filter>`
/// element or no recognized primitives were collected.
pub(super) fn resolve_filter(defs: &Defs<'_>, id: &str) -> Option<Filter> {
    let f = defs.lookup(id)?;
    if !name_eq(&f.name, "filter") || !defs.charge_work(node_cost(f)) {
        return None;
    }
    let mut primitives = Vec::new();
    for c in &f.children {
        if primitives.len() >= MAX_FILTER_PRIMITIVES {
            break;
        }
        // `feMerge` also reads its `feMergeNode` children.
        let cost = c
            .children
            .iter()
            .map(node_cost)
            .fold(node_cost(c), usize::saturating_add);
        if !defs.charge_work(cost) {
            return None;
        }
        if let Some(p) = parse_filter_primitive(c) {
            primitives.push(p);
        }
    }
    if primitives.is_empty() {
        return None;
    }
    Some(Filter { primitives })
}

fn parse_filter_primitive(node: &Node) -> Option<FilterPrimitive> {
    let input = node.attr("in").map(|s| s.trim().to_string());
    let result = node.attr("result").map(|s| s.trim().to_string());

    let op = if name_eq(&node.name, "feGaussianBlur") {
        let (sx, sy) = parse_std_deviation(node.attr("stdDeviation").unwrap_or(""))?;
        FilterOp::GaussianBlur {
            std_dev_x: sx,
            std_dev_y: sy,
        }
    } else if name_eq(&node.name, "feColorMatrix") {
        let kind = node
            .attr("type")
            .unwrap_or("matrix")
            .trim()
            .to_ascii_lowercase();
        let values = node.attr("values").unwrap_or("");
        let matrix = parse_color_matrix(&kind, values)?;
        FilterOp::ColorMatrix { matrix }
    } else if name_eq(&node.name, "feOffset") {
        let dx = node.attr("dx").and_then(parse_length).unwrap_or(0.0);
        let dy = node.attr("dy").and_then(parse_length).unwrap_or(0.0);
        FilterOp::Offset { dx, dy }
    } else if name_eq(&node.name, "feFlood") {
        let mut color = node
            .attr("flood-color")
            .and_then(parse_color)
            .unwrap_or([0, 0, 0, 255]);
        let opa = node
            .attr("flood-opacity")
            .and_then(parse_opacity)
            .unwrap_or(1.0);
        let a = (color[3] as f32 / 255.0 * opa).clamp(0.0, 1.0);
        color[3] = (a * 255.0).round() as u8;
        FilterOp::Flood { color }
    } else if name_eq(&node.name, "feMerge") {
        let mut inputs = Vec::new();
        for c in &node.children {
            if name_eq(&c.name, "feMergeNode") {
                if let Some(r) = c.attr("in") {
                    inputs.push(r.trim().to_string());
                }
            }
        }
        if inputs.is_empty() {
            return None;
        }
        FilterOp::Merge { inputs }
    } else {
        return None;
    };

    Some(FilterPrimitive { input, result, op })
}

/// `stdDeviation` may be a single number or two whitespace-separated
/// numbers (x, y). Negative values are an SVG error; we treat them as
/// zero (no blur on that axis).
pub(super) fn parse_std_deviation(s: &str) -> Option<(f32, f32)> {
    let mut it = s
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|t| !t.is_empty());
    let a: f32 = it.next()?.parse().ok()?;
    let b = it.next().and_then(|t| t.parse::<f32>().ok()).unwrap_or(a);
    Some((a.max(0.0), b.max(0.0)))
}

/// Parses an `feColorMatrix` `values=` attribute under the named
/// `type=` flavor. Returns a 4x5 row-major matrix (RGBA in, RGBA out
/// plus 1 column of bias). Failure modes (wrong arity, NaN) silently
/// degrade to identity so downstream rendering stays sane.
fn parse_color_matrix(kind: &str, values: &str) -> Option<[f32; 20]> {
    let nums: Vec<f32> = values
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse::<f32>().ok())
        .collect();
    match kind {
        "matrix" | "" => {
            if nums.len() != 20 {
                return None;
            }
            let mut m = [0.0_f32; 20];
            m.copy_from_slice(&nums);
            Some(m)
        }
        "saturate" => {
            // SVG 1.1 §15.18: saturation matrix.
            let s = nums.first().copied().unwrap_or(1.0);
            Some(saturate_matrix(s))
        }
        "huerotate" => {
            let deg = nums.first().copied().unwrap_or(0.0);
            Some(hue_rotate_matrix(deg))
        }
        "luminancetoalpha" => Some(LUMINANCE_TO_ALPHA_MATRIX),
        _ => None,
    }
}

/// Identity-on-luma matrix from SVG 1.1 §15.18 with `s` controlling the
/// linear interpolation between luma-only (s=0) and identity (s=1).
pub(super) fn saturate_matrix(s: f32) -> [f32; 20] {
    // Coefficients from the SVG spec.
    let r0 = 0.213 + 0.787 * s;
    let r1 = 0.715 - 0.715 * s;
    let r2 = 0.072 - 0.072 * s;
    let g0 = 0.213 - 0.213 * s;
    let g1 = 0.715 + 0.285 * s;
    let g2 = 0.072 - 0.072 * s;
    let b0 = 0.213 - 0.213 * s;
    let b1 = 0.715 - 0.715 * s;
    let b2 = 0.072 + 0.928 * s;
    [
        r0, r1, r2, 0.0, 0.0, //
        g0, g1, g2, 0.0, 0.0, //
        b0, b1, b2, 0.0, 0.0, //
        0.0, 0.0, 0.0, 1.0, 0.0,
    ]
}

/// Hue-rotation matrix from SVG 1.1 §15.18.
pub(super) fn hue_rotate_matrix(degrees: f32) -> [f32; 20] {
    let rad = degrees.to_radians();
    let c = rad.cos();
    let s = rad.sin();
    let r0 = 0.213 + c * 0.787 - s * 0.213;
    let r1 = 0.715 - c * 0.715 - s * 0.715;
    let r2 = 0.072 - c * 0.072 + s * 0.928;
    let g0 = 0.213 - c * 0.213 + s * 0.143;
    let g1 = 0.715 + c * 0.285 + s * 0.140;
    let g2 = 0.072 - c * 0.072 - s * 0.283;
    let b0 = 0.213 - c * 0.213 - s * 0.787;
    let b1 = 0.715 - c * 0.715 + s * 0.715;
    let b2 = 0.072 + c * 0.928 + s * 0.072;
    [
        r0, r1, r2, 0.0, 0.0, //
        g0, g1, g2, 0.0, 0.0, //
        b0, b1, b2, 0.0, 0.0, //
        0.0, 0.0, 0.0, 1.0, 0.0,
    ]
}

const LUMINANCE_TO_ALPHA_MATRIX: [f32; 20] = [
    0.0, 0.0, 0.0, 0.0, 0.0, //
    0.0, 0.0, 0.0, 0.0, 0.0, //
    0.0, 0.0, 0.0, 0.0, 0.0, //
    0.2125, 0.7154, 0.0721, 0.0, 0.0,
];

// =========================================================================
// Filter pipeline
// =========================================================================
//
// Each `<filter>` is a small DAG of `FilterPrimitive`s. We evaluate the
// DAG against a same-size `SourceGraphic` pixmap (the filtered shape
// rendered alone into a transparent buffer) and a `SourceAlpha` pixmap
// (the same shape with R=G=B=0). Each primitive reads from `in` (named
// or implicit-prev) and writes to `result` (named or anonymous). The
// last primitive's output is the filtered pixmap, composited under the
// canvas via Porter-Duff source-over.
//
// All intermediate buffers are full canvas size. This trades memory
// for simplicity: feOffset + feMerge etc. don't need to track filter
// regions, and shifting / blurring stays within the visible canvas.

/// Walks the primitive list and returns the final pixmap. Built-in
/// inputs `SourceGraphic` and `SourceAlpha` are materialized lazily.
pub(super) fn apply_filter(filter: &Filter, source: &ColorPixmap) -> ColorPixmap {
    use alloc::collections::BTreeMap;
    let mut named: BTreeMap<String, ColorPixmap> = BTreeMap::new();
    let mut prev: Option<ColorPixmap> = None;
    let mut source_alpha: Option<ColorPixmap> = None;

    for prim in &filter.primitives {
        let in_pix: ColorPixmap = match prim.input.as_deref() {
            Some("SourceGraphic") => source.clone(),
            Some("SourceAlpha") => source_alpha
                .get_or_insert_with(|| make_source_alpha(source))
                .clone(),
            Some(name) => named
                .get(name)
                .cloned()
                .unwrap_or_else(|| ColorPixmap::new(source.width, source.height)),
            None => prev.clone().unwrap_or_else(|| source.clone()),
        };

        let out = match &prim.op {
            FilterOp::GaussianBlur {
                std_dev_x,
                std_dev_y,
            } => apply_gaussian_blur(&in_pix, *std_dev_x, *std_dev_y),
            FilterOp::ColorMatrix { matrix } => apply_color_matrix(&in_pix, matrix),
            FilterOp::Offset { dx, dy } => apply_offset(&in_pix, *dx, *dy),
            FilterOp::Flood { color } => apply_flood(in_pix.width, in_pix.height, *color),
            FilterOp::Merge { inputs } => {
                let mut acc = ColorPixmap::new(source.width, source.height);
                for name in inputs {
                    let layer = match name.as_str() {
                        "SourceGraphic" => source.clone(),
                        "SourceAlpha" => source_alpha
                            .get_or_insert_with(|| make_source_alpha(source))
                            .clone(),
                        other => named
                            .get(other)
                            .cloned()
                            .unwrap_or_else(|| ColorPixmap::new(source.width, source.height)),
                    };
                    composite_over(&mut acc, &layer);
                }
                acc
            }
        };

        if let Some(name) = &prim.result {
            named.insert(name.clone(), out.clone());
        }
        prev = Some(out);
    }

    prev.unwrap_or_else(|| source.clone())
}

/// `SourceAlpha`: the source's alpha channel in all four channels'
/// premultiplied form (R=G=B=0, A unchanged).
fn make_source_alpha(src: &ColorPixmap) -> ColorPixmap {
    let mut out = ColorPixmap::new(src.width, src.height);
    let n = src.data.len() / 4;
    for i in 0..n {
        let a = src.data[i * 4 + 3];
        out.data[i * 4] = 0;
        out.data[i * 4 + 1] = 0;
        out.data[i * 4 + 2] = 0;
        out.data[i * 4 + 3] = a;
    }
    out
}

/// Porter-Duff source-over compositing of a same-size premultiplied
/// `top` onto `dst`. Reuses the per-pixel formula from
/// `colrv1::blend_src_over` but in a tight inner loop.
pub(super) fn composite_over(dst: &mut ColorPixmap, top: &ColorPixmap) {
    if dst.width != top.width || dst.height != top.height {
        return;
    }
    let n = dst.data.len() / 4;
    for i in 0..n {
        let sa = top.data[i * 4 + 3] as u32;
        if sa == 0 {
            continue;
        }
        let sr = top.data[i * 4] as u32;
        let sg = top.data[i * 4 + 1] as u32;
        let sb = top.data[i * 4 + 2] as u32;
        let dr = dst.data[i * 4] as u32;
        let dg = dst.data[i * 4 + 1] as u32;
        let db = dst.data[i * 4 + 2] as u32;
        let da = dst.data[i * 4 + 3] as u32;
        let inv = 255 - sa;
        dst.data[i * 4] = (sr + (dr * inv + 127) / 255) as u8;
        dst.data[i * 4 + 1] = (sg + (dg * inv + 127) / 255) as u8;
        dst.data[i * 4 + 2] = (sb + (db * inv + 127) / 255) as u8;
        dst.data[i * 4 + 3] = (sa + (da * inv + 127) / 255) as u8;
    }
}

/// Three-pass separable box-blur approximation. Each axis is convolved
/// with a box kernel of radius `r ~= ceil(sigma)` three times, which approaches
/// a true Gaussian by the central-limit theorem and is visually
/// indistinguishable for σ >= 1.
pub(super) fn apply_gaussian_blur(src: &ColorPixmap, sx: f32, sy: f32) -> ColorPixmap {
    if (sx <= 0.0 && sy <= 0.0) || src.is_empty() {
        return src.clone();
    }
    let rx = ((sx.max(0.0)).ceil() as i32).min(MAX_BLUR_RADIUS);
    let ry = ((sy.max(0.0)).ceil() as i32).min(MAX_BLUR_RADIUS);
    let mut buf = src.clone();
    if rx > 0 {
        for _ in 0..3 {
            buf = box_blur_h(&buf, rx);
        }
    }
    if ry > 0 {
        for _ in 0..3 {
            buf = box_blur_v(&buf, ry);
        }
    }
    buf
}

/// Sum of `sample(k)` over `k` in `-r..=r` with `k` clamped into
/// `0..len`, as the edge-extending blur window needs. Counts the
/// clamped samples instead of visiting them, so the cost is at most
/// `len` samples however large `r` is. `r >= 0` and `len >= 1`.
pub(super) fn clamped_window_sum(r: i32, len: i32, sample: impl Fn(i32) -> u32) -> u32 {
    let inside = r.min(len - 1);
    let below = r as u32 * sample(0);
    let above = (r - inside) as u32 * sample(len - 1);
    (0..=inside).map(&sample).sum::<u32>() + below + above
}

fn box_blur_h(src: &ColorPixmap, r: i32) -> ColorPixmap {
    let w = src.width as i32;
    let h = src.height as i32;
    let mut out = ColorPixmap::new(src.width, src.height);
    if w == 0 || h == 0 || r == 0 {
        out.data.copy_from_slice(&src.data);
        return out;
    }
    let kernel = (r * 2 + 1) as u32;
    for y in 0..h {
        let row = (y * w) as usize * 4;
        // Sliding-window sum over the kernel. Out-of-bounds samples
        // clamp to the edge ("EDGE" mode in SVG terms, closer to what
        // browser engines do for filter regions touching the canvas
        // edge).
        // Prime the window with [-r, r] samples.
        let sample = |c: usize| move |kx: i32| src.data[row + kx as usize * 4 + c] as u32;
        let mut sr = clamped_window_sum(r, w, sample(0));
        let mut sg = clamped_window_sum(r, w, sample(1));
        let mut sb = clamped_window_sum(r, w, sample(2));
        let mut sa = clamped_window_sum(r, w, sample(3));
        for x in 0..w {
            let oi = row + x as usize * 4;
            out.data[oi] = (sr / kernel) as u8;
            out.data[oi + 1] = (sg / kernel) as u8;
            out.data[oi + 2] = (sb / kernel) as u8;
            out.data[oi + 3] = (sa / kernel) as u8;
            // Slide window: drop pixel at x-r, add pixel at x+r+1.
            let drop_x = (x - r).clamp(0, w - 1);
            let add_x = (x + r + 1).clamp(0, w - 1);
            let di = row + drop_x as usize * 4;
            let ai = row + add_x as usize * 4;
            sr = sr + src.data[ai] as u32 - src.data[di] as u32;
            sg = sg + src.data[ai + 1] as u32 - src.data[di + 1] as u32;
            sb = sb + src.data[ai + 2] as u32 - src.data[di + 2] as u32;
            sa = sa + src.data[ai + 3] as u32 - src.data[di + 3] as u32;
        }
    }
    out
}

fn box_blur_v(src: &ColorPixmap, r: i32) -> ColorPixmap {
    let w = src.width as i32;
    let h = src.height as i32;
    let mut out = ColorPixmap::new(src.width, src.height);
    if w == 0 || h == 0 || r == 0 {
        out.data.copy_from_slice(&src.data);
        return out;
    }
    let kernel = (r * 2 + 1) as u32;
    let stride = (w as usize) * 4;
    for x in 0..w {
        let col = x as usize * 4;
        let sample = |c: usize| move |ky: i32| src.data[col + ky as usize * stride + c] as u32;
        let mut sr = clamped_window_sum(r, h, sample(0));
        let mut sg = clamped_window_sum(r, h, sample(1));
        let mut sb = clamped_window_sum(r, h, sample(2));
        let mut sa = clamped_window_sum(r, h, sample(3));
        for y in 0..h {
            let oi = col + y as usize * stride;
            out.data[oi] = (sr / kernel) as u8;
            out.data[oi + 1] = (sg / kernel) as u8;
            out.data[oi + 2] = (sb / kernel) as u8;
            out.data[oi + 3] = (sa / kernel) as u8;
            let drop_y = (y - r).clamp(0, h - 1);
            let add_y = (y + r + 1).clamp(0, h - 1);
            let di = col + drop_y as usize * stride;
            let ai = col + add_y as usize * stride;
            sr = sr + src.data[ai] as u32 - src.data[di] as u32;
            sg = sg + src.data[ai + 1] as u32 - src.data[di + 1] as u32;
            sb = sb + src.data[ai + 2] as u32 - src.data[di + 2] as u32;
            sa = sa + src.data[ai + 3] as u32 - src.data[di + 3] as u32;
        }
    }
    out
}

/// Applies a 4x5 color matrix (RGBA + bias column) to a premultiplied
/// pixmap. Per SVG 1.1 §15.18, `feColorMatrix` operates on
/// non-premultiplied RGBA, so we un-premultiply, transform, clamp, and
/// re-premultiply.
fn apply_color_matrix(src: &ColorPixmap, m: &[f32; 20]) -> ColorPixmap {
    let mut out = ColorPixmap::new(src.width, src.height);
    let n = src.data.len() / 4;
    for i in 0..n {
        let pr = src.data[i * 4] as f32 / 255.0;
        let pg = src.data[i * 4 + 1] as f32 / 255.0;
        let pb = src.data[i * 4 + 2] as f32 / 255.0;
        let pa = src.data[i * 4 + 3] as f32 / 255.0;
        // Un-premultiply (avoid div-by-zero).
        let (r, g, b) = if pa > 0.0 {
            (pr / pa, pg / pa, pb / pa)
        } else {
            (0.0, 0.0, 0.0)
        };
        let nr = (m[0] * r + m[1] * g + m[2] * b + m[3] * pa + m[4]).clamp(0.0, 1.0);
        let ng = (m[5] * r + m[6] * g + m[7] * b + m[8] * pa + m[9]).clamp(0.0, 1.0);
        let nb = (m[10] * r + m[11] * g + m[12] * b + m[13] * pa + m[14]).clamp(0.0, 1.0);
        let na = (m[15] * r + m[16] * g + m[17] * b + m[18] * pa + m[19]).clamp(0.0, 1.0);
        out.data[i * 4] = (nr * na * 255.0).round() as u8;
        out.data[i * 4 + 1] = (ng * na * 255.0).round() as u8;
        out.data[i * 4 + 2] = (nb * na * 255.0).round() as u8;
        out.data[i * 4 + 3] = (na * 255.0).round() as u8;
    }
    out
}

/// Translates a pixmap by `(dx, dy)` device-space pixels. Out-of-bounds
/// reads return transparent black; the destination is fresh.
pub(super) fn apply_offset(src: &ColorPixmap, dx: f32, dy: f32) -> ColorPixmap {
    let mut out = ColorPixmap::new(src.width, src.height);
    // `as i32` saturates for huge offsets, so the subtractions below
    // saturate too. Any saturated source index is out of range.
    let dxi = dx.round() as i32;
    let dyi = dy.round() as i32;
    let w = src.width as i32;
    let h = src.height as i32;
    for y in 0..h {
        let sy = y.saturating_sub(dyi);
        if sy < 0 || sy >= h {
            continue;
        }
        for x in 0..w {
            let sx = x.saturating_sub(dxi);
            if sx < 0 || sx >= w {
                continue;
            }
            let s = (sy as usize * w as usize + sx as usize) * 4;
            let d = (y as usize * w as usize + x as usize) * 4;
            out.data[d] = src.data[s];
            out.data[d + 1] = src.data[s + 1];
            out.data[d + 2] = src.data[s + 2];
            out.data[d + 3] = src.data[s + 3];
        }
    }
    out
}

/// Returns a same-size pixmap filled with a solid premultiplied color.
fn apply_flood(width: u32, height: u32, color: [u8; 4]) -> ColorPixmap {
    let mut out = ColorPixmap::new(width, height);
    // Premultiply.
    let a = color[3] as u32;
    let r = (color[0] as u32 * a + 127) / 255;
    let g = (color[1] as u32 * a + 127) / 255;
    let b = (color[2] as u32 * a + 127) / 255;
    let n = out.data.len() / 4;
    for i in 0..n {
        out.data[i * 4] = r as u8;
        out.data[i * 4 + 1] = g as u8;
        out.data[i * 4 + 2] = b as u8;
        out.data[i * 4 + 3] = a as u8;
    }
    out
}
