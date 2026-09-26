//! Scanline rasterizer.
//!
//! Implements a clean-room non-zero-winding scanline fill with vertical
//! supersampling for anti-aliasing. The scheme:
//!
//! 1. For every output row, rasterize `OVERSAMPLE` sub-rows at
//!    integer-aligned y positions inside the pixel.
//! 2. For each sub-row, walk the edge list and find every
//!    intersection of the edge with the sub-row's center line. Each
//!    intersection carries a `+1` or `-1` winding contribution from
//!    the sign of the edge's `dy`.
//! 3. Sort the intersections by x. Walk left to right keeping a
//!    running winding count; while it's non-zero, the row is "inside".
//! 4. A pixel's coverage in this sub-row equals how much of `[x, x+1]`
//!    sat inside the filled run. Summing across `OVERSAMPLE` sub-rows
//!    and rounding gives the 8-bit alpha.
//!
//! The algorithm is `O(edges * height)` worst case but in practice the
//! glyph footprint is small enough that it screams. Accuracy ties
//! out to FreeType's smooth rasterizer for outlines flattened with the
//! `0.25`-pixel tolerance the higher level picks.

use alloc::vec;
use alloc::vec::Vec;

use crate::flatten::Segment;
use crate::pixmap::Pixmap;

/// Vertical oversampling factor. Eight sub-rows is enough to resolve
/// 256 distinct alpha levels; combined with the natural horizontal
/// coverage from the edge math we get smooth diagonals.
const OVERSAMPLE: u32 = 8;

/// Tolerated horizontal range a single edge contributes per row.
/// Edges shorter than this are dropped (they're below the
/// anti-aliasing floor).
const EPS: f32 = 1e-6;

/// Output pixmap and the device-space offset of its `(0, 0)` pixel.
pub(crate) struct Render {
    /// The rendered pixmap.
    pub pixmap: Pixmap,
    /// Pixel-space x-coordinate of column 0.
    pub origin_x: i32,
    /// Pixel-space y-coordinate of row 0.
    pub origin_y: i32,
}

/// Largest width or height, in pixels, of any pixmap the rasterizer
/// allocates. Matches the per-side ceiling of the PNG, JPEG, and TIFF
/// decoders and of the SVG canvas. It keeps a hostile outline (tiny
/// `unitsPerEm`, huge coordinates, or extreme transforms) from
/// requesting a multi-gigabyte coverage buffer.
pub(crate) const MAX_RASTER_DIM: u32 = 16_384;

/// Pixel-grid placement of a rasterized segment list: the device-space
/// origin of pixel `(0, 0)` and the pixmap size, margin included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RasterBounds {
    /// Pixel-space x-coordinate of column 0.
    pub origin_x: i32,
    /// Pixel-space y-coordinate of row 0.
    pub origin_y: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Axis-aligned device-space pixel rectangle `[x0, x1) x [y0, y1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    /// Left edge, inclusive.
    pub x0: i32,
    /// Top edge, inclusive.
    pub y0: i32,
    /// Right edge, exclusive.
    pub x1: i32,
    /// Bottom edge, exclusive.
    pub y1: i32,
}

fn empty_render() -> Render {
    Render {
        pixmap: Pixmap::new(0, 0),
        origin_x: 0,
        origin_y: 0,
    }
}

/// Rasterizes `segments` into a pixmap sized to their bounding box
/// (with a 1-pixel margin so anti-aliased edges don't clip). Returns
/// an empty pixmap when the segment list contains no real edges or the
/// box is wider or taller than [`MAX_RASTER_DIM`].
pub(crate) fn rasterize(segments: &[Segment]) -> Render {
    rasterize_in(segments, None)
}

/// Computes where [`rasterize`] places `segments`, without allocating.
/// Returns `None` when the input is empty, non-finite, or out of
/// range, which are the cases where [`rasterize`] returns an empty
/// pixmap regardless of size. The size cap is not applied here:
/// [`rasterize`] also returns an empty pixmap when either side of the
/// result exceeds [`MAX_RASTER_DIM`], and callers that composite
/// several masks check that cap against their union.
pub(crate) fn raster_bounds(segments: &[Segment]) -> Option<RasterBounds> {
    if segments.is_empty() {
        return None;
    }

    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for s in segments {
        if s.x0 < min_x {
            min_x = s.x0;
        }
        if s.x0 > max_x {
            max_x = s.x0;
        }
        if s.x1 < min_x {
            min_x = s.x1;
        }
        if s.x1 > max_x {
            max_x = s.x1;
        }
        if s.y0 < min_y {
            min_y = s.y0;
        }
        if s.y0 > max_y {
            max_y = s.y0;
        }
        if s.y1 < min_y {
            min_y = s.y1;
        }
        if s.y1 > max_y {
            max_y = s.y1;
        }
    }
    if !min_x.is_finite() || !max_x.is_finite() || !min_y.is_finite() || !max_y.is_finite() {
        return None;
    }

    // Reject bboxes whose extents don't fit in a sane pixel grid.
    // `as i32` saturates at i32::MIN / i32::MAX for f32 values out of
    // integer range, so finite-but-extreme coords could otherwise
    // overflow the `+/- pad` and `ex - ox` arithmetic. Memory is capped
    // separately: an unclipped raster by `MAX_RASTER_DIM`, a clipped
    // one by its window.
    const MAX_EXTENT: f32 = 1_048_576.0;
    if min_x < -MAX_EXTENT || max_x > MAX_EXTENT || min_y < -MAX_EXTENT || max_y > MAX_EXTENT {
        return None;
    }
    let pad = 1_i32;
    let ox = (min_x.floor() as i32).saturating_sub(pad);
    let oy = (min_y.floor() as i32).saturating_sub(pad);
    let ex = (max_x.ceil() as i32).saturating_add(pad);
    let ey = (max_y.ceil() as i32).saturating_add(pad);
    Some(RasterBounds {
        origin_x: ox,
        origin_y: oy,
        width: ex.saturating_sub(ox).max(0) as u32,
        height: ey.saturating_sub(oy).max(0) as u32,
    })
}

/// Rasterizes `segments` like [`rasterize`], but when `window` is set
/// only the pixels inside it are computed and stored. Every stored
/// pixel has exactly the value the unclipped raster would give it.
/// The returned origin is that of the clipped pixmap. The size cap
/// applies to the stored region, so a window keeps huge shapes cheap.
pub(crate) fn rasterize_in(segments: &[Segment], window: Option<Window>) -> Render {
    let Some(RasterBounds {
        origin_x: ox,
        origin_y: oy,
        width,
        height,
    }) = raster_bounds(segments)
    else {
        return empty_render();
    };

    // Stored region in local coordinates (relative to `ox`, `oy`):
    // columns `col_lo..col_hi`, rows `row_lo..row_hi`.
    let (col_lo, col_hi, row_lo, row_hi) = match window {
        None => (0, width, 0, height),
        Some(w) => {
            let clamp_x = |v: i32| (i64::from(v) - i64::from(ox)).clamp(0, i64::from(width)) as u32;
            let clamp_y =
                |v: i32| (i64::from(v) - i64::from(oy)).clamp(0, i64::from(height)) as u32;
            let (c0, c1) = (clamp_x(w.x0), clamp_x(w.x1));
            let (r0, r1) = (clamp_y(w.y0), clamp_y(w.y1));
            (c0, c1.max(c0), r0, r1.max(r0))
        }
    };
    let out_w = col_hi - col_lo;
    let out_h = row_hi - row_lo;
    if out_w > MAX_RASTER_DIM || out_h > MAX_RASTER_DIM {
        return empty_render();
    }
    let origin_x = ox.saturating_add(col_lo as i32);
    let origin_y = oy.saturating_add(row_lo as i32);

    let mut pixmap = Pixmap::new(out_w, out_h);
    if out_w == 0 || out_h == 0 {
        return Render {
            pixmap,
            origin_x,
            origin_y,
        };
    }

    // Translate every segment into local pixel-space (0..width, 0..height).
    let local: Vec<Segment> = segments
        .iter()
        .map(|s| Segment {
            x0: s.x0 - ox as f32,
            y0: s.y0 - oy as f32,
            x1: s.x1 - ox as f32,
            y1: s.y1 - oy as f32,
        })
        .collect();

    // Reusable scratch buffers, one per scanline pass.
    let mut crossings: Vec<(f32, i32)> = Vec::with_capacity(local.len());
    // Per-row coverage accumulator for the stored columns: f32
    // `0..=OVERSAMPLE` summed sub-row contribution per pixel. We
    // convert to u8 at the end.
    let mut row_cov: Vec<f32> = vec![0.0; out_w as usize];

    for py in row_lo..row_hi {
        row_cov.fill(0.0);
        for sub in 0..OVERSAMPLE {
            let y = py as f32 + (sub as f32 + 0.5) / OVERSAMPLE as f32;
            crossings.clear();
            for s in &local {
                let dy = s.y1 - s.y0;
                if dy.abs() < EPS {
                    continue;
                }
                // Half-open interval test in y avoids double-counting
                // shared vertices: an edge belongs to its lower y but
                // not its upper y. Sign of dy gives winding direction.
                let (lo, hi, sign) = if dy > 0.0 {
                    (s.y0, s.y1, 1_i32)
                } else {
                    (s.y1, s.y0, -1_i32)
                };
                if y < lo || y >= hi {
                    continue;
                }
                let t = (y - s.y0) / dy;
                let x = s.x0 + t * (s.x1 - s.x0);
                crossings.push((x, sign));
            }
            if crossings.is_empty() {
                continue;
            }
            crossings.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal));

            // Walk crossings, maintain winding.
            let mut winding = 0_i32;
            let mut last_x: f32 = 0.0;
            let mut inside = false;
            for &(x, sign) in &crossings {
                if inside {
                    let x0 = last_x.max(0.0);
                    let x1 = x.min(width as f32);
                    if x1 > x0 {
                        accumulate(&mut row_cov, x0, x1, width, col_lo);
                    }
                }
                winding += sign;
                inside = winding != 0;
                last_x = x;
            }
        }
        // Convert accumulator to alpha (255 / OVERSAMPLE per sub-row).
        let scale = 255.0 / OVERSAMPLE as f32;
        for (px, &c) in row_cov.iter().enumerate() {
            if c > 0.0 {
                let a = (c * scale).round().clamp(0.0, 255.0) as u8;
                if a > 0 {
                    pixmap.set(px as u32, py - row_lo, a);
                }
            }
        }
    }

    Render {
        pixmap,
        origin_x,
        origin_y,
    }
}

/// Adds horizontal coverage in `[x0, x1]` to the row accumulator. The
/// accumulator stores per-pixel sub-row weights summed across all
/// `OVERSAMPLE` passes; one sub-row's contribution to a pixel equals
/// the fraction of `[px, px+1]` overlapped by `[x0, x1]`.
///
/// `row[i]` holds local column `col_lo + i`. Columns outside that
/// range are skipped without changing the value of any stored one.
fn accumulate(row: &mut [f32], x0: f32, x1: f32, width: u32, col_lo: u32) {
    if x1 <= x0 {
        return;
    }
    let lo = x0.max(0.0);
    let hi = x1.min(width as f32);
    if hi <= lo {
        return;
    }
    let i_lo = lo.floor() as i32;
    let i_hi = (hi.ceil() as i32 - 1).max(i_lo);
    if i_hi < 0 || i_lo as u32 >= width {
        return;
    }
    let i_lo_u = (i_lo.max(0) as u32).max(col_lo);
    let i_hi_u = (i_hi as u32).min(width - 1);
    for px in i_lo_u..=i_hi_u {
        let Some(cov) = row.get_mut((px - col_lo) as usize) else {
            break;
        };
        let cell_lo = px as f32;
        let cell_hi = cell_lo + 1.0;
        let a = lo.max(cell_lo);
        let b = hi.min(cell_hi);
        if b > a {
            *cov += b - a;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Square at (1,1)-(11,11), entirely within a 13x13 pixmap.
    fn square_segments() -> Vec<Segment> {
        vec![
            Segment {
                x0: 1.0,
                y0: 1.0,
                x1: 11.0,
                y1: 1.0,
            },
            Segment {
                x0: 11.0,
                y0: 1.0,
                x1: 11.0,
                y1: 11.0,
            },
            Segment {
                x0: 11.0,
                y0: 11.0,
                x1: 1.0,
                y1: 11.0,
            },
            Segment {
                x0: 1.0,
                y0: 11.0,
                x1: 1.0,
                y1: 1.0,
            },
        ]
    }

    #[test]
    fn empty_segments_yield_empty_pixmap() {
        let r = rasterize(&[]);
        assert!(r.pixmap.is_empty());
    }

    #[test]
    fn axis_aligned_square_fills_interior() {
        let r = rasterize(&square_segments());
        // Interior pixel must be fully opaque.
        let lx = (5 - r.origin_x) as u32;
        let ly = (5 - r.origin_y) as u32;
        assert_eq!(r.pixmap.get(lx, ly), 255, "interior should be opaque");
    }

    #[test]
    fn axis_aligned_square_anti_aliases_at_subpixel_offset() {
        let segs = vec![
            Segment {
                x0: 1.5,
                y0: 1.0,
                x1: 11.5,
                y1: 1.0,
            },
            Segment {
                x0: 11.5,
                y0: 1.0,
                x1: 11.5,
                y1: 11.0,
            },
            Segment {
                x0: 11.5,
                y0: 11.0,
                x1: 1.5,
                y1: 11.0,
            },
            Segment {
                x0: 1.5,
                y0: 11.0,
                x1: 1.5,
                y1: 1.0,
            },
        ];
        let r = rasterize(&segs);
        let lx_left = (1 - r.origin_x) as u32;
        let ly_mid = (5 - r.origin_y) as u32;
        let a_left = r.pixmap.get(lx_left, ly_mid);
        // Half-pixel inset on the left edge gives ~50% coverage there.
        assert!(
            a_left > 90 && a_left < 180,
            "expected anti-aliased left edge, got {a_left}"
        );
    }

    #[test]
    fn winding_rule_punches_holes() {
        // Outer CCW box and inner CW box (subtractive).
        let outer = [
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
            (0.0, 0.0),
        ];
        let inner = [(3.0, 3.0), (3.0, 7.0), (7.0, 7.0), (7.0, 3.0), (3.0, 3.0)];
        let mut segs = Vec::new();
        for w in outer.windows(2) {
            segs.push(Segment {
                x0: w[0].0,
                y0: w[0].1,
                x1: w[1].0,
                y1: w[1].1,
            });
        }
        for w in inner.windows(2) {
            segs.push(Segment {
                x0: w[0].0,
                y0: w[0].1,
                x1: w[1].0,
                y1: w[1].1,
            });
        }
        let r = rasterize(&segs);
        // Hole interior pixel.
        let hx = (5 - r.origin_x) as u32;
        let hy = (5 - r.origin_y) as u32;
        assert_eq!(r.pixmap.get(hx, hy), 0, "hole should be transparent");
        // Wall interior pixel.
        let wx = (1 - r.origin_x) as u32;
        let wy = (5 - r.origin_y) as u32;
        assert_eq!(r.pixmap.get(wx, wy), 255, "wall should be opaque");
    }

    #[test]
    fn nonfinite_y_does_not_panic_returns_empty() {
        // Regression for issue #202: y bbox was previously not
        // checked for finiteness, so an INF y crashed the i32 cast +
        // pad with `attempt to add with overflow`.
        let segs = vec![
            Segment {
                x0: 0.0,
                y0: f32::INFINITY,
                x1: 10.0,
                y1: 0.0,
            },
            Segment {
                x0: 10.0,
                y0: 0.0,
                x1: 0.0,
                y1: 10.0,
            },
        ];
        let r = rasterize(&segs);
        assert!(
            r.pixmap.is_empty(),
            "non-finite bbox must yield empty pixmap"
        );
    }

    #[test]
    fn nan_y_does_not_panic_returns_empty() {
        let segs = vec![Segment {
            x0: 0.0,
            y0: f32::NAN,
            x1: 1.0,
            y1: 1.0,
        }];
        let r = rasterize(&segs);
        // Either empty pixmap, or the segment was effectively dropped;
        // either way no panic.
        assert!(r.pixmap.width <= 4 && r.pixmap.height <= 4);
    }

    #[test]
    fn extreme_finite_x_does_not_panic_returns_empty() {
        // f32::MAX is finite but saturates to i32::MAX after .floor()
        // / .ceil() casts; the previous `+ pad` and `ex - ox`
        // arithmetic overflowed in debug builds. Regression for
        // issue #202: a pathologically large bbox now bails out to
        // an empty pixmap rather than panic-or-oom.
        let segs = vec![Segment {
            x0: 0.0,
            y0: 0.0,
            x1: f32::MAX,
            y1: 1.0,
        }];
        let r = rasterize(&segs);
        assert!(
            r.pixmap.is_empty(),
            "extreme x bbox must yield empty pixmap, got {}x{}",
            r.pixmap.width,
            r.pixmap.height
        );
    }

    #[test]
    fn determinism() {
        let segs = square_segments();
        let a = rasterize(&segs);
        let b = rasterize(&segs);
        assert_eq!(a.pixmap, b.pixmap);
        assert_eq!(a.origin_x, b.origin_x);
        assert_eq!(a.origin_y, b.origin_y);
    }

    fn square(x0: f32, y0: f32, side: f32) -> Vec<Segment> {
        let (x1, y1) = (x0 + side, y0 + side);
        let pts = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
        (0..4)
            .map(|i| {
                let (a, b) = (pts[i], pts[(i + 1) % 4]);
                Segment {
                    x0: a.0,
                    y0: a.1,
                    x1: b.0,
                    y1: b.1,
                }
            })
            .collect()
    }

    #[test]
    fn oversized_bbox_is_not_allocated() {
        // A 20000-pixel square used to allocate a 400 MB coverage
        // buffer and scan 160000 sub-rows.
        let segs = square(0.0, 0.0, 20_000.0);
        assert!(rasterize(&segs).pixmap.is_empty());
        let b = raster_bounds(&segs).expect("finite, in range");
        assert!(b.width > MAX_RASTER_DIM);
        // With a window only the window is computed and stored.
        let win = Window {
            x0: 100,
            y0: 100,
            x1: 110,
            y1: 108,
        };
        let r = rasterize_in(&segs, Some(win));
        assert_eq!(
            (r.pixmap.width, r.pixmap.height, r.origin_x, r.origin_y),
            (10, 8, 100, 100)
        );
        assert!(r.pixmap.data.iter().all(|&a| a == 255));
    }

    #[test]
    fn windowed_raster_matches_full_raster_pixel_for_pixel() {
        let mut state = 0x9E37_79B9_u32;
        let mut rnd = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state % 10_000) as f32 / 100.0
        };
        for _ in 0..40 {
            // A random closed polygon with fractional vertices.
            let pts: Vec<(f32, f32)> = (0..7).map(|_| (rnd(), rnd())).collect();
            let segs: Vec<Segment> = (0..pts.len())
                .map(|i| {
                    let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                    Segment {
                        x0: a.0,
                        y0: a.1,
                        x1: b.0,
                        y1: b.1,
                    }
                })
                .collect();
            let full = rasterize(&segs);
            for win in [
                Window {
                    x0: 10,
                    y0: 20,
                    x1: 60,
                    y1: 45,
                },
                Window {
                    x0: -30,
                    y0: -5,
                    x1: 17,
                    y1: 200,
                },
                Window {
                    x0: 0,
                    y0: 0,
                    x1: 1,
                    y1: 1,
                },
            ] {
                let part = rasterize_in(&segs, Some(win));
                for y in win.y0..win.y1 {
                    for x in win.x0..win.x1 {
                        let at = |r: &Render| {
                            let (lx, ly) = (x - r.origin_x, y - r.origin_y);
                            if lx < 0 || ly < 0 {
                                0
                            } else {
                                r.pixmap.get(lx as u32, ly as u32)
                            }
                        };
                        assert_eq!(at(&part), at(&full), "pixel ({x}, {y})");
                    }
                }
            }
        }
    }
}
