//! Stroke geometry: flattens paths to polylines and expands them into
//! filled ribbons with caps and joins.

use alloc::vec::Vec;

use sigilbuzz::tables::PathOp;

use super::dash::dash_polyline;
use super::document::{LineCap, LineJoin};
use super::MITER_LIMIT;

// =========================================================================
// Stroke geometry: walk polyline -> emit closed quad ribbons with caps
// and joins.
// =========================================================================

/// Expands an open / closed polyline into a closed filled outline that
/// represents the stroke. The output is a sequence of `MoveTo` /
/// `LineTo` / `Close` ops that the existing fill pipeline can consume.
///
/// The polyline is obtained by flattening the input ops (curves
/// flattened to chords at default tolerance). For each segment we emit
/// a quadrilateral of width `stroke_width` perpendicular to the
/// segment direction. Joins between segments are filled with
/// miter / round / bevel geometry, and the open ends carry the
/// configured cap shape.
pub(super) fn stroke_to_fill(
    ops: &[PathOp],
    stroke_width: f32,
    cap: LineCap,
    join: LineJoin,
    dasharray: &[f32],
    dashoffset: f32,
) -> Vec<PathOp> {
    if stroke_width <= 0.0 {
        return Vec::new();
    }
    let polylines = flatten_to_polylines(ops);
    let half = stroke_width * 0.5;
    let mut out: Vec<PathOp> = Vec::new();

    let dashed = !dasharray.is_empty() && dasharray.iter().any(|&v| v > 0.0);

    for poly in &polylines {
        if poly.points.len() < 2 {
            continue;
        }
        if dashed {
            // Per-contour: walk *true Bezier arc length* (not the
            // chord-flattened polyline cumulative length, which is
            // always slightly short of the curve), emit only the "draw"
            // phase segments as fresh open polylines.
            let segs = dash_polyline(
                &poly.points,
                &poly.arc_lengths,
                poly.closed,
                dasharray,
                dashoffset,
            );
            for seg in segs {
                if seg.len() >= 2 {
                    emit_stroked_polyline(&mut out, &seg, false, half, cap, join);
                }
            }
        } else {
            emit_stroked_polyline(&mut out, &poly.points, poly.closed, half, cap, join);
        }
    }
    out
}

#[derive(Debug, Clone)]
pub(super) struct PolyLine {
    pub(super) points: Vec<(f32, f32)>,
    /// Per-chord *true* arc length. `arc_lengths[i]` is the arc length
    /// from `points[i]` to `points[(i + 1) % n]` along the original
    /// Bezier the chord came from. For straight `LineTo` chords this is
    /// the Euclidean distance and matches `(b - a).norm()`. For chords
    /// produced by curve flattening this is computed via the Roger
    /// Willcocks chord+control-polygon estimator at the leaf of curve
    /// subdivision, so it captures the curve's true sweep length
    /// instead of the (always-shorter) chord length.
    ///
    /// Length is `points.len() - 1` for open contours; for closed
    /// contours the implicit close-line's length is appended, giving
    /// `points.len()` entries.
    pub(super) arc_lengths: Vec<f32>,
    pub(super) closed: bool,
}

/// Flattens curves into a polyline list. One [`PolyLine`] per
/// sub-path. Closed sub-paths (terminated by `Close`) get
/// `closed = true`. Each polyline carries a parallel `arc_lengths`
/// array recording the *true Bezier arc length* of each chord segment;
/// for straight chords this equals the Euclidean distance, for
/// curve-flattened chords it is the leaf-level Roger Willcocks
/// approximation against the original control points.
pub(super) fn flatten_to_polylines(ops: &[PathOp]) -> Vec<PolyLine> {
    let mut out: Vec<PolyLine> = Vec::new();
    let mut cur: Vec<(f32, f32)> = Vec::new();
    let mut cur_arc: Vec<f32> = Vec::new();
    let mut sx = 0.0_f32;
    let mut sy = 0.0_f32;
    let mut cx = 0.0_f32;
    let mut cy = 0.0_f32;
    let mut open = false;

    let push_line = |cur: &mut Vec<(f32, f32)>, arcs: &mut Vec<f32>, x: f32, y: f32| {
        let dup = cur
            .last()
            .map(|p| (p.0 - x).abs() <= 1e-6 && (p.1 - y).abs() <= 1e-6)
            .unwrap_or(false);
        if !dup {
            if let Some(prev) = cur.last() {
                let dx = x - prev.0;
                let dy = y - prev.1;
                arcs.push((dx * dx + dy * dy).sqrt());
            }
            cur.push((x, y));
        }
    };

    let finalize_close = |cur: &Vec<(f32, f32)>, arcs: &mut Vec<f32>| {
        // Closed contours need a wrap-segment arc length appended for
        // the implicit edge from `points[n-1]` back to `points[0]`.
        if let (Some(first), Some(last)) = (cur.first(), cur.last()) {
            let dx = first.0 - last.0;
            let dy = first.1 - last.1;
            arcs.push((dx * dx + dy * dy).sqrt());
        }
    };

    for op in ops {
        match *op {
            PathOp::MoveTo { x, y } => {
                if open && cur.len() >= 2 {
                    out.push(PolyLine {
                        points: core::mem::take(&mut cur),
                        arc_lengths: core::mem::take(&mut cur_arc),
                        closed: false,
                    });
                } else {
                    cur.clear();
                    cur_arc.clear();
                }
                cur.push((x, y));
                sx = x;
                sy = y;
                cx = x;
                cy = y;
                open = true;
            }
            PathOp::LineTo { x, y } => {
                push_line(&mut cur, &mut cur_arc, x, y);
                cx = x;
                cy = y;
            }
            PathOp::QuadTo {
                cx: ccx,
                cy: ccy,
                x,
                y,
            } => {
                flatten_quad_polyline(&mut cur, &mut cur_arc, cx, cy, ccx, ccy, x, y, 0.25, 0);
                cx = x;
                cy = y;
            }
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => {
                flatten_cubic_polyline(
                    &mut cur,
                    &mut cur_arc,
                    cx,
                    cy,
                    c1x,
                    c1y,
                    c2x,
                    c2y,
                    x,
                    y,
                    0.25,
                    0,
                );
                cx = x;
                cy = y;
            }
            PathOp::Close => {
                if open && cur.len() >= 2 {
                    finalize_close(&cur, &mut cur_arc);
                    out.push(PolyLine {
                        points: core::mem::take(&mut cur),
                        arc_lengths: core::mem::take(&mut cur_arc),
                        closed: true,
                    });
                }
                cx = sx;
                cy = sy;
                open = false;
            }
        }
    }
    if open && cur.len() >= 2 {
        out.push(PolyLine {
            points: cur,
            arc_lengths: cur_arc,
            closed: false,
        });
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn flatten_quad_polyline(
    out: &mut Vec<(f32, f32)>,
    arcs: &mut Vec<f32>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    tol: f32,
    depth: u32,
) {
    let dx = x2 - x0;
    let dy = y2 - y0;
    let denom = dx * dx + dy * dy;
    let cross = (x1 - x0) * dy - (y1 - y0) * dx;
    let dist_sq = if denom > 0.0 {
        (cross * cross) / denom
    } else {
        let ex = x1 - x0;
        let ey = y1 - y0;
        ex * ex + ey * ey
    };
    if depth >= 16 || dist_sq <= 4.0 * tol * tol {
        if out
            .last()
            .map(|p| (p.0 - x2).abs() > 1e-6 || (p.1 - y2).abs() > 1e-6)
            .unwrap_or(true)
        {
            // Roger Willcocks arc-length estimate for the leaf curve
            // segment we're about to accept as a chord: the chord is
            // shorter than the curve, so dasharray walking against
            // chord length would land dashes early on long sweeps.
            let chord = (dx * dx + dy * dy).sqrt();
            let poly = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt()
                + ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
            arcs.push(0.5 * (chord + poly));
            out.push((x2, y2));
        }
        return;
    }
    let m01 = (0.5 * (x0 + x1), 0.5 * (y0 + y1));
    let m12 = (0.5 * (x1 + x2), 0.5 * (y1 + y2));
    let m = (0.5 * (m01.0 + m12.0), 0.5 * (m01.1 + m12.1));
    flatten_quad_polyline(out, arcs, x0, y0, m01.0, m01.1, m.0, m.1, tol, depth + 1);
    flatten_quad_polyline(out, arcs, m.0, m.1, m12.0, m12.1, x2, y2, tol, depth + 1);
}

#[allow(clippy::too_many_arguments)]
fn flatten_cubic_polyline(
    out: &mut Vec<(f32, f32)>,
    arcs: &mut Vec<f32>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    x3: f32,
    y3: f32,
    tol: f32,
    depth: u32,
) {
    let dx = x3 - x0;
    let dy = y3 - y0;
    let denom = dx * dx + dy * dy;
    let (d1, d2) = if denom > 0.0 {
        let c1 = (x1 - x0) * dy - (y1 - y0) * dx;
        let c2 = (x2 - x0) * dy - (y2 - y0) * dx;
        ((c1 * c1) / denom, (c2 * c2) / denom)
    } else {
        let e1x = x1 - x0;
        let e1y = y1 - y0;
        let e2x = x2 - x0;
        let e2y = y2 - y0;
        (e1x * e1x + e1y * e1y, e2x * e2x + e2y * e2y)
    };
    if depth >= 16 || (d1 <= tol * tol && d2 <= tol * tol) {
        if out
            .last()
            .map(|p| (p.0 - x3).abs() > 1e-6 || (p.1 - y3).abs() > 1e-6)
            .unwrap_or(true)
        {
            // Leaf-level Roger Willcocks arc-length estimate using the
            // four control points: closer to the true Bezier arc than
            // the chord (x0,y0)-(x3,y3) for non-degenerate curves.
            let chord = (dx * dx + dy * dy).sqrt();
            let poly = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt()
                + ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt()
                + ((x3 - x2).powi(2) + (y3 - y2).powi(2)).sqrt();
            arcs.push(0.5 * (chord + poly));
            out.push((x3, y3));
        }
        return;
    }
    let m01 = (0.5 * (x0 + x1), 0.5 * (y0 + y1));
    let m12 = (0.5 * (x1 + x2), 0.5 * (y1 + y2));
    let m23 = (0.5 * (x2 + x3), 0.5 * (y2 + y3));
    let m012 = (0.5 * (m01.0 + m12.0), 0.5 * (m01.1 + m12.1));
    let m123 = (0.5 * (m12.0 + m23.0), 0.5 * (m12.1 + m23.1));
    let m = (0.5 * (m012.0 + m123.0), 0.5 * (m012.1 + m123.1));
    flatten_cubic_polyline(
        out,
        arcs,
        x0,
        y0,
        m01.0,
        m01.1,
        m012.0,
        m012.1,
        m.0,
        m.1,
        tol,
        depth + 1,
    );
    flatten_cubic_polyline(
        out,
        arcs,
        m.0,
        m.1,
        m123.0,
        m123.1,
        m23.0,
        m23.1,
        x3,
        y3,
        tol,
        depth + 1,
    );
}

/// Emits the stroke ribbon for one polyline. For the minimum-viable
/// path this draws each segment as a separate rectangle (butt cap +
/// miter-style overlap). Adjacent segments overlap at joins so
/// scanline winding fills the joint cleanly without explicit miter
/// geometry. The result is visually identical to "miter" for typical
/// stroke widths and avoids the corner-case math.
///
/// Round / square caps emit half-circles / extended rectangles at the
/// open ends (best-effort follow-up, for now butt is the default).
fn emit_stroked_polyline(
    out: &mut Vec<PathOp>,
    points: &[(f32, f32)],
    closed: bool,
    half: f32,
    cap: LineCap,
    join: LineJoin,
) {
    if points.len() < 2 || half <= 0.0 {
        return;
    }
    let n = points.len();
    let segs = if closed { n } else { n - 1 };

    for i in 0..segs {
        let a = points[i];
        let b = points[(i + 1) % n];
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-6 {
            continue;
        }
        let (nx, ny) = (-dy / len, dx / len); // unit perpendicular (left)
        let (px, py) = (nx * half, ny * half);

        // Per-segment cap extension for square cap on the end caps.
        let mut a_ex = (0.0, 0.0);
        let mut b_ex = (0.0, 0.0);
        if !closed && cap == LineCap::Square {
            let (tx, ty) = (dx / len, dy / len);
            if i == 0 {
                a_ex = (-tx * half, -ty * half);
            }
            if i == segs - 1 {
                b_ex = (tx * half, ty * half);
            }
        }

        let p0 = (a.0 + a_ex.0 + px, a.1 + a_ex.1 + py);
        let p1 = (b.0 + b_ex.0 + px, b.1 + b_ex.1 + py);
        let p2 = (b.0 + b_ex.0 - px, b.1 + b_ex.1 - py);
        let p3 = (a.0 + a_ex.0 - px, a.1 + a_ex.1 - py);
        out.push(PathOp::MoveTo { x: p0.0, y: p0.1 });
        out.push(PathOp::LineTo { x: p1.0, y: p1.1 });
        out.push(PathOp::LineTo { x: p2.0, y: p2.1 });
        out.push(PathOp::LineTo { x: p3.0, y: p3.1 });
        out.push(PathOp::Close);
    }

    // Joins. For miter (default): overlapping rectangles already paint
    // the joint correctly. For round / bevel we approximate with a
    // disk / triangle at each vertex.
    if join == LineJoin::Round || cap == LineCap::Round {
        let join_at = |out: &mut Vec<PathOp>, p: (f32, f32)| {
            emit_disk(out, p.0, p.1, half);
        };
        let start = if closed { 0 } else { 1 };
        let end = if closed { n } else { n - 1 };
        for p in &points[start..end] {
            join_at(out, *p);
        }
        if !closed && cap == LineCap::Round {
            join_at(out, points[0]);
            join_at(out, points[n - 1]);
        }
    }

    // Miter spikes: when adjacent segments don't form a near-straight
    // angle, fill the wedge between them so a sharp corner doesn't
    // leave a notch. Falls back to bevel beyond the miter limit.
    if join == LineJoin::Miter && n >= 3 {
        let span = if closed { n } else { n - 2 };
        for i in 0..span {
            let prev = points[if closed && i == 0 { n - 1 } else { i }];
            let cur = points[if closed { (i + 1) % n } else { i + 1 }];
            let next = points[if closed { (i + 2) % n } else { i + 2 }];
            emit_miter_join(out, prev, cur, next, half);
        }
    }
}

/// Emits an axis-aligned octagon ("disk") of radius `r` centered at
/// `(cx, cy)`. 8 segments is the documented round-cap approximation.
fn emit_disk(out: &mut Vec<PathOp>, cx: f32, cy: f32, r: f32) {
    if r <= 0.0 {
        return;
    }
    const N: usize = 8;
    let two_pi = core::f32::consts::TAU;
    let mut first = (0.0, 0.0);
    for i in 0..N {
        let theta = (i as f32) / (N as f32) * two_pi;
        let x = cx + r * theta.cos();
        let y = cy + r * theta.sin();
        if i == 0 {
            out.push(PathOp::MoveTo { x, y });
            first = (x, y);
        } else {
            out.push(PathOp::LineTo { x, y });
        }
    }
    let _ = first;
    out.push(PathOp::Close);
}

/// Emits a miter-join wedge at vertex `cur`, given the previous and
/// next polyline points. When the join angle is reflex enough that the
/// miter would exceed `MITER_LIMIT * width`, a bevel triangle is used
/// instead (matching SVG's stroke-miterlimit default of 4).
fn emit_miter_join(
    out: &mut Vec<PathOp>,
    prev: (f32, f32),
    cur: (f32, f32),
    next: (f32, f32),
    half: f32,
) {
    let (ax, ay) = (cur.0 - prev.0, cur.1 - prev.1);
    let la = (ax * ax + ay * ay).sqrt();
    let (bx, by) = (next.0 - cur.0, next.1 - cur.1);
    let lb = (bx * bx + by * by).sqrt();
    if la < 1e-6 || lb < 1e-6 {
        return;
    }
    let (tax, tay) = (ax / la, ay / la);
    let (tbx, tby) = (bx / lb, by / lb);
    // Outer perpendicular (left of travel) on each segment.
    let (na, na2) = ((-tay) * half, tax * half);
    let (nb, nb2) = ((-tby) * half, tbx * half);
    // Outer corners.
    let p_a_left = (cur.0 + na, cur.1 + na2);
    let p_b_left = (cur.0 + nb, cur.1 + nb2);
    let p_a_right = (cur.0 - na, cur.1 - na2);
    let p_b_right = (cur.0 - nb, cur.1 - nb2);

    // Compute miter point on the outer side. A small angle between
    // segments means a long spike. Bail to bevel beyond the limit.
    let dot = tax * tbx + tay * tby;
    let denom = 1.0 + dot;
    if denom <= 1e-6 {
        // Near 180° turn; bevel triangle on each side handles it.
        out.push(PathOp::MoveTo { x: cur.0, y: cur.1 });
        out.push(PathOp::LineTo {
            x: p_a_left.0,
            y: p_a_left.1,
        });
        out.push(PathOp::LineTo {
            x: p_b_left.0,
            y: p_b_left.1,
        });
        out.push(PathOp::Close);
        out.push(PathOp::MoveTo { x: cur.0, y: cur.1 });
        out.push(PathOp::LineTo {
            x: p_a_right.0,
            y: p_a_right.1,
        });
        out.push(PathOp::LineTo {
            x: p_b_right.0,
            y: p_b_right.1,
        });
        out.push(PathOp::Close);
        return;
    }
    // Miter spike length per the SVG appendix:
    //   m = half / sin(theta/2)   where  cos(theta) = -dot for "turn"
    let miter_ratio = (2.0_f32 / denom).sqrt(); // = 1 / sin(theta/2)
    if miter_ratio > MITER_LIMIT {
        // Bevel: just two triangles connecting outer corners to the
        // join center.
        out.push(PathOp::MoveTo { x: cur.0, y: cur.1 });
        out.push(PathOp::LineTo {
            x: p_a_left.0,
            y: p_a_left.1,
        });
        out.push(PathOp::LineTo {
            x: p_b_left.0,
            y: p_b_left.1,
        });
        out.push(PathOp::Close);
        out.push(PathOp::MoveTo { x: cur.0, y: cur.1 });
        out.push(PathOp::LineTo {
            x: p_a_right.0,
            y: p_a_right.1,
        });
        out.push(PathOp::LineTo {
            x: p_b_right.0,
            y: p_b_right.1,
        });
        out.push(PathOp::Close);
        return;
    }
    // Bisector direction.
    let bis_x = tax + tbx;
    let bis_y = tay + tby;
    let bis_len = (bis_x * bis_x + bis_y * bis_y).sqrt();
    if bis_len < 1e-6 {
        return;
    }
    let (bxn, byn) = (bis_x / bis_len, bis_y / bis_len);
    // Outer normal (left of join travel).
    let (n_left_x, n_left_y) = (-byn, bxn);
    let dx_m = n_left_x * half * miter_ratio;
    let dy_m = n_left_y * half * miter_ratio;
    let p_left_miter = (cur.0 + dx_m, cur.1 + dy_m);
    let p_right_miter = (cur.0 - dx_m, cur.1 - dy_m);

    // Outer-side miter wedge.
    out.push(PathOp::MoveTo { x: cur.0, y: cur.1 });
    out.push(PathOp::LineTo {
        x: p_a_left.0,
        y: p_a_left.1,
    });
    out.push(PathOp::LineTo {
        x: p_left_miter.0,
        y: p_left_miter.1,
    });
    out.push(PathOp::LineTo {
        x: p_b_left.0,
        y: p_b_left.1,
    });
    out.push(PathOp::Close);
    // Inner-side miter wedge (mirrors the outer one).
    out.push(PathOp::MoveTo { x: cur.0, y: cur.1 });
    out.push(PathOp::LineTo {
        x: p_a_right.0,
        y: p_a_right.1,
    });
    out.push(PathOp::LineTo {
        x: p_right_miter.0,
        y: p_right_miter.1,
    });
    out.push(PathOp::LineTo {
        x: p_b_right.0,
        y: p_b_right.1,
    });
    out.push(PathOp::Close);
}
