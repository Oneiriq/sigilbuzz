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

/// Rasterizes `segments` into a pixmap sized to their bounding box
/// (with a 1-pixel margin so anti-aliased edges don't clip). Returns
/// an empty pixmap when the segment list contains no real edges.
pub(crate) fn rasterize(segments: &[Segment]) -> Render {
    if segments.is_empty() {
        return Render {
            pixmap: Pixmap::new(0, 0),
            origin_x: 0,
            origin_y: 0,
        };
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
        return Render {
            pixmap: Pixmap::new(0, 0),
            origin_x: 0,
            origin_y: 0,
        };
    }

    // Reject bboxes whose extents don't fit in a sane pixel grid.
    // `as i32` saturates at i32::MIN / i32::MAX for f32 values out of
    // integer range, so finite-but-extreme coords could otherwise
    // overflow the `+/- pad` and `ex - ox` arithmetic and either
    // panic in debug or allocate a multi-gig pixmap. The cap is
    // generous (any glyph that needs > 1M pixels per side is already
    // pathological).
    const MAX_EXTENT: f32 = 1_048_576.0;
    if min_x < -MAX_EXTENT || max_x > MAX_EXTENT || min_y < -MAX_EXTENT || max_y > MAX_EXTENT {
        return Render {
            pixmap: Pixmap::new(0, 0),
            origin_x: 0,
            origin_y: 0,
        };
    }
    let pad = 1_i32;
    let ox = (min_x.floor() as i32).saturating_sub(pad);
    let oy = (min_y.floor() as i32).saturating_sub(pad);
    let ex = (max_x.ceil() as i32).saturating_add(pad);
    let ey = (max_y.ceil() as i32).saturating_add(pad);
    let width = ex.saturating_sub(ox).max(0) as u32;
    let height = ey.saturating_sub(oy).max(0) as u32;

    let mut pixmap = Pixmap::new(width, height);
    if width == 0 || height == 0 {
        return Render {
            pixmap,
            origin_x: ox,
            origin_y: oy,
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
    // Per-row coverage accumulator: f32 `0..=OVERSAMPLE` summed sub-row
    // contribution per pixel. We convert to u8 at the end.
    let mut row_cov: Vec<f32> = vec![0.0; width as usize];

    for py in 0..height {
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
                        accumulate(&mut row_cov, x0, x1, width);
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
                    pixmap.set(px as u32, py, a);
                }
            }
        }
    }

    Render {
        pixmap,
        origin_x: ox,
        origin_y: oy,
    }
}

/// Adds horizontal coverage in `[x0, x1]` to the row accumulator. The
/// accumulator stores per-pixel sub-row weights summed across all
/// `OVERSAMPLE` passes; one sub-row's contribution to a pixel equals
/// the fraction of `[px, px+1]` overlapped by `[x0, x1]`.
fn accumulate(row: &mut [f32], x0: f32, x1: f32, width: u32) {
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
    let i_lo_u = i_lo.max(0) as u32;
    let i_hi_u = (i_hi as u32).min(width - 1);
    for px in i_lo_u..=i_hi_u {
        let cell_lo = px as f32;
        let cell_hi = cell_lo + 1.0;
        let a = lo.max(cell_lo);
        let b = hi.min(cell_hi);
        if b > a {
            row[px as usize] += b - a;
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
}
