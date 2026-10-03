//! Scanline rasterizer.
//!
//! Implements a clean-room non-zero-winding scanline fill with vertical
//! supersampling for anti-aliasing. The scheme:
//!
//! 1. For every output row, rasterize `OVERSAMPLE` sub-rows at
//!    integer-aligned y positions inside the pixel.
//! 2. For each sub-row, walk the edges whose y-range reaches the row
//!    (an active list kept in input order as the scan moves down) and
//!    find every intersection of the edge with the sub-row's center
//!    line. Each intersection carries a `+1` or `-1` winding
//!    contribution from the sign of the edge's `dy`.
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

    // Translate every segment into local pixel-space (0..width,
    // 0..height) and keep the ones that can cross a sub-row, in input
    // order, with the rows they can reach.
    let edges = edges_in_rows(segments, ox, oy, row_lo, row_hi);

    // Edge indices by first row. The sort is stable, so edges that start
    // on the same row stay in input order.
    let mut by_first_row: Vec<u32> = (0..edges.len() as u32).collect();
    by_first_row.sort_by_key(|&i| edges.get(i as usize).map_or(0, |e| e.first_row));
    let mut next_edge = 0;
    // Edges whose rows include the current one, in input order, so the
    // crossings of every sub-row reach the sort in the order a scan of
    // the whole list would push them.
    let mut active: Vec<u32> = Vec::new();

    // Reusable scratch buffers, one per scanline pass.
    let mut crossings: Vec<(f32, i32)> = Vec::with_capacity(edges.len());
    // Per-row coverage accumulator for the stored columns: f32
    // `0..=OVERSAMPLE` summed sub-row contribution per pixel. We
    // convert to u8 at the end.
    let mut row_cov: Vec<f32> = vec![0.0; out_w as usize];

    let out_rows = pixmap.data.chunks_exact_mut(out_w as usize);
    for (py, out_row) in (row_lo..row_hi).zip(out_rows) {
        active.retain(|&i| edges.get(i as usize).is_some_and(|e| e.end_row > py));
        let before = active.len();
        while let Some(&i) = by_first_row.get(next_edge) {
            if edges.get(i as usize).map_or(true, |e| e.first_row > py) {
                break;
            }
            active.push(i);
            next_edge += 1;
        }
        if active.len() != before {
            active.sort_unstable();
        }
        if active.is_empty() {
            continue;
        }

        row_cov.fill(0.0);
        for sub in 0..OVERSAMPLE {
            let y = py as f32 + (sub as f32 + 0.5) / OVERSAMPLE as f32;
            crossings.clear();
            for e in active.iter().filter_map(|&i| edges.get(i as usize)) {
                // Half-open interval test in y avoids double-counting
                // shared vertices: an edge belongs to its lower y but
                // not its upper y.
                if y < e.lo || y >= e.hi {
                    continue;
                }
                let t = (y - e.y0) / e.dy;
                let x = e.x0 + t * e.dx;
                crossings.push((x, e.sign));
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
        for (dst, &c) in out_row.iter_mut().zip(&row_cov) {
            if c > 0.0 {
                *dst = round_unit(c * scale).clamp(0.0, 255.0) as u8;
            }
        }
    }

    Render {
        pixmap,
        origin_x,
        origin_y,
    }
}

/// One segment in local pixel space, ready for the sub-row scan.
struct Edge {
    /// Start point.
    x0: f32,
    y0: f32,
    /// `x1 - x0`.
    dx: f32,
    /// `y1 - y0`.
    dy: f32,
    /// The edge covers sub-rows `y` with `lo <= y < hi`.
    lo: f32,
    hi: f32,
    /// Winding contribution: the sign of `dy`.
    sign: i32,
    /// Rows `first_row..end_row` hold every sub-row the edge can cross.
    first_row: u32,
    end_row: u32,
}

/// Moves `segments` into the local pixel space whose origin is
/// `(ox, oy)` and returns, in input order, every one that can cross a
/// sub-row of rows `row_lo..row_hi`.
///
/// The arithmetic matches a per-sub-row scan of the whole segment list
/// operation for operation: a horizontal segment (`|dy| < EPS`) never
/// crosses, and the row range only narrows where the scan's own
/// half-open test can pass. A segment whose `lo` or `hi` is not finite
/// keeps every row, so the scan's test decides as before.
fn edges_in_rows(segments: &[Segment], ox: i32, oy: i32, row_lo: u32, row_hi: u32) -> Vec<Edge> {
    let (fx, fy) = (ox as f32, oy as f32);
    let clamp_row = |v: f32| (v as i64).clamp(i64::from(row_lo), i64::from(row_hi)) as u32;
    let mut edges = Vec::with_capacity(segments.len());
    for s in segments {
        let (x0, y0, x1, y1) = (s.x0 - fx, s.y0 - fy, s.x1 - fx, s.y1 - fy);
        let dy = y1 - y0;
        if dy.abs() < EPS {
            continue;
        }
        let (lo, hi, sign) = if dy > 0.0 {
            (y0, y1, 1_i32)
        } else {
            (y1, y0, -1_i32)
        };
        // A sub-row `y` of row `py` lies in `[py, py + 1)`, so
        // `lo <= y < hi` needs `floor(lo) <= py < ceil(hi)`.
        let (first_row, end_row) = if lo.is_finite() && hi.is_finite() {
            (clamp_row(lo.floor()), clamp_row(hi.ceil()))
        } else {
            (row_lo, row_hi)
        };
        if first_row >= end_row {
            continue;
        }
        edges.push(Edge {
            x0,
            y0,
            dx: x1 - x0,
            dy,
            lo,
            hi,
            sign,
            first_row,
            end_row,
        });
    }
    edges
}

/// `v.round()` for `v` in `[0, 2^23)`, without the library call that
/// `f32::round` compiles to on x86-64 and wasm32, which have no
/// instruction for rounding halves away from zero. In that range the
/// truncation and the subtraction are exact, so the result is exactly
/// `v.round()`. Above it both results are at least `2^23`, and NaN maps
/// to 0, so after a clamp to `[0, 255]` and a cast to `u8` the two
/// agree for every non-negative or NaN input.
#[inline]
pub(crate) fn round_unit(v: f32) -> f32 {
    let t = v as i32 as f32;
    if v - t >= 0.5 {
        t + 1.0
    } else {
        t
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
    // `0 <= lo < hi <= width` here, so truncation is `floor` and the
    // adjusted truncation is `ceil`, exactly and without the library
    // calls `f32::floor` and `f32::ceil` compile to on x86-64.
    let i_lo = lo as i32;
    let hi_int = hi as i32;
    let hi_ceil = if (hi_int as f32) < hi {
        hi_int + 1
    } else {
        hi_int
    };
    let i_hi = (hi_ceil - 1).max(i_lo);
    if i_hi < 0 || i_lo as u32 >= width {
        return;
    }
    let i_lo_u = (i_lo.max(0) as u32).max(col_lo);
    let i_hi_u = (i_hi as u32).min(width - 1);
    // Stored columns touched: `first..=last`.
    let Some(stored_last) = (row.len() as u32).checked_sub(1) else {
        return;
    };
    let (first, last) = (i_lo_u, i_hi_u.min(col_lo.saturating_add(stored_last)));
    if first > last {
        return;
    }
    // Here `0 <= lo < hi <= width`, so `0 <= i_lo <= i_hi`. A column
    // strictly between `i_lo` and `i_hi` lies inside `[lo, hi]`, and
    // the general update below adds exactly `(px + 1) - px = 1.0` to
    // it (every column index is far below 2^24, so both ends are
    // exact). Adding 1.0 directly gives the same bits.
    let (edge_lo, edge_hi) = (i_lo.max(0) as u32, i_hi.max(0) as u32);
    let inner = first.max(edge_lo + 1)..=last.min(edge_hi.saturating_sub(1));
    if !inner.is_empty() {
        let cells = (inner.start() - col_lo) as usize..=(inner.end() - col_lo) as usize;
        if let Some(cells) = row.get_mut(cells) {
            for c in cells {
                *c += 1.0;
            }
        }
    }
    // The end columns: fractional coverage.
    let ends = if edge_hi == edge_lo {
        [Some(edge_lo), None]
    } else {
        [Some(edge_lo), Some(edge_hi)]
    };
    for px in ends.into_iter().flatten() {
        if px < first || px > last {
            continue;
        }
        let Some(cov) = row.get_mut((px - col_lo) as usize) else {
            continue;
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

    /// The scanline as first written: every sub-row scans the whole
    /// segment list, and every covered column takes the general
    /// fractional update. [`rasterize_in`] must match it bit for bit.
    fn reference_rasterize_in(segments: &[Segment], window: Option<Window>) -> Render {
        let Some(RasterBounds {
            origin_x: ox,
            origin_y: oy,
            width,
            height,
        }) = raster_bounds(segments)
        else {
            return empty_render();
        };
        let (col_lo, col_hi, row_lo, row_hi) = match window {
            None => (0, width, 0, height),
            Some(w) => {
                let clamp_x =
                    |v: i32| (i64::from(v) - i64::from(ox)).clamp(0, i64::from(width)) as u32;
                let clamp_y =
                    |v: i32| (i64::from(v) - i64::from(oy)).clamp(0, i64::from(height)) as u32;
                let (c0, c1) = (clamp_x(w.x0), clamp_x(w.x1));
                let (r0, r1) = (clamp_y(w.y0), clamp_y(w.y1));
                (c0, c1.max(c0), r0, r1.max(r0))
            }
        };
        let (out_w, out_h) = (col_hi - col_lo, row_hi - row_lo);
        if out_w > MAX_RASTER_DIM || out_h > MAX_RASTER_DIM {
            return empty_render();
        }
        let mut pixmap = Pixmap::new(out_w, out_h);
        let local: Vec<Segment> = segments
            .iter()
            .map(|s| Segment {
                x0: s.x0 - ox as f32,
                y0: s.y0 - oy as f32,
                x1: s.x1 - ox as f32,
                y1: s.y1 - oy as f32,
            })
            .collect();
        let mut row_cov: Vec<f32> = vec![0.0; out_w as usize];
        for py in row_lo..row_hi {
            row_cov.fill(0.0);
            for sub in 0..OVERSAMPLE {
                let y = py as f32 + (sub as f32 + 0.5) / OVERSAMPLE as f32;
                let mut crossings: Vec<(f32, i32)> = Vec::new();
                for s in &local {
                    let dy = s.y1 - s.y0;
                    if dy.abs() < EPS {
                        continue;
                    }
                    let (lo, hi, sign) = if dy > 0.0 {
                        (s.y0, s.y1, 1_i32)
                    } else {
                        (s.y1, s.y0, -1_i32)
                    };
                    if y < lo || y >= hi {
                        continue;
                    }
                    let t = (y - s.y0) / dy;
                    crossings.push((s.x0 + t * (s.x1 - s.x0), sign));
                }
                crossings
                    .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal));
                let (mut winding, mut last_x, mut inside) = (0_i32, 0.0_f32, false);
                for &(x, sign) in &crossings {
                    if inside {
                        let (x0, x1) = (last_x.max(0.0), x.min(width as f32));
                        if x1 > x0 {
                            reference_accumulate(&mut row_cov, x0, x1, width, col_lo);
                        }
                    }
                    winding += sign;
                    inside = winding != 0;
                    last_x = x;
                }
            }
            for (px, &c) in row_cov.iter().enumerate() {
                if c > 0.0 {
                    let a = (c * (255.0 / OVERSAMPLE as f32)).round().clamp(0.0, 255.0) as u8;
                    if a > 0 {
                        pixmap.set(px as u32, py - row_lo, a);
                    }
                }
            }
        }
        Render {
            pixmap,
            origin_x: ox.saturating_add(col_lo as i32),
            origin_y: oy.saturating_add(row_lo as i32),
        }
    }

    fn reference_accumulate(row: &mut [f32], x0: f32, x1: f32, width: u32, col_lo: u32) {
        if x1 <= x0 {
            return;
        }
        let (lo, hi) = (x0.max(0.0), x1.min(width as f32));
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
            let (cell_lo, cell_hi) = (px as f32, px as f32 + 1.0);
            let (a, b) = (lo.max(cell_lo), hi.min(cell_hi));
            if b > a {
                *cov += b - a;
            }
        }
    }

    #[test]
    fn scanline_matches_the_reference_bit_for_bit() {
        let mut state = 0x2545_F491_u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for case in 0..250 {
            // Random closed polygons with fractional, repeated, and
            // horizontal edges, a few hundred units across.
            let mut pts: Vec<(f32, f32)> = Vec::new();
            for _ in 0..(3 + next() % 12) {
                let x = (next() % 30_000) as f32 / 97.0 - 40.0;
                let y = match next() % 4 {
                    // Repeat the previous y: a horizontal edge.
                    0 => pts.last().map_or(1.5, |p| p.1),
                    _ => (next() % 30_000) as f32 / 89.0 - 60.0,
                };
                pts.push((x, y));
            }
            let mut segs: Vec<Segment> = (0..pts.len())
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
            // Some cases add a segment with a NaN or infinite end. The
            // bounds ignore a NaN, so the scan sees it and must treat
            // it the same way.
            if case % 5 == 0 {
                let v = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY][(next() % 3) as usize];
                let mut s = segs[0];
                match next() % 4 {
                    0 => s.x0 = v,
                    1 => s.y0 = v,
                    2 => s.x1 = v,
                    _ => s.y1 = v,
                }
                let at = (next() as usize) % segs.len();
                segs.insert(at, s);
            }
            let windows = [
                None,
                Some(Window {
                    x0: -5,
                    y0: -70,
                    x1: 400,
                    y1: 400,
                }),
                Some(Window {
                    x0: (next() % 200) as i32 - 50,
                    y0: (next() % 200) as i32 - 80,
                    x1: (next() % 300) as i32,
                    y1: (next() % 300) as i32,
                }),
            ];
            for window in windows {
                let got = rasterize_in(&segs, window);
                let want = reference_rasterize_in(&segs, window);
                assert_eq!(got.pixmap, want.pixmap, "case {case}, window {window:?}");
                assert_eq!(
                    (got.origin_x, got.origin_y),
                    (want.origin_x, want.origin_y),
                    "case {case}"
                );
            }
        }
    }

    #[test]
    fn round_unit_matches_round_on_its_range() {
        let mut v = 0.0_f32;
        while v < 300.0 {
            assert_eq!(round_unit(v).to_bits(), v.round().to_bits(), "{v}");
            // Exact halves and their neighbors.
            let h = v.floor() + 0.5;
            for w in [
                h,
                f32::from_bits(h.to_bits() - 1),
                f32::from_bits(h.to_bits() + 1),
            ] {
                assert_eq!(round_unit(w), w.round(), "{w}");
            }
            v += 0.013;
        }
        assert_eq!(round_unit(f32::NAN).clamp(0.0, 255.0) as u8, 0);
        assert_eq!(round_unit(1.0e9).clamp(0.0, 255.0) as u8, 255);
    }
}
