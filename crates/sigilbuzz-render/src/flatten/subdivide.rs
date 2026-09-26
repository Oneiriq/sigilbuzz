//! Adaptive midpoint subdivision of quadratic and cubic Beziers into
//! chord segments.

use alloc::vec::Vec;

use super::{push_segment, Segment, MAX_DEPTH};

/// True when subdividing further cannot help: the depth cap or the
/// segment budget is reached, or a control point is NaN or infinite.
/// Midpoints of non-finite points stay non-finite, so splitting them
/// would only emit `2^MAX_DEPTH` unusable segments.
fn stop_subdividing(depth: u32, budget: usize, points: &[f32]) -> bool {
    depth >= MAX_DEPTH || budget == 0 || !points.iter().all(|v| v.is_finite())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn flatten_quad(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    tol_sq: f32,
    out: &mut Vec<Segment>,
    budget: &mut usize,
    depth: u32,
) {
    // Squared perpendicular distance from the control point to the
    // chord (x0,y0)-(x2,y2). For quadratics the maximum chord-to-curve
    // distance is bounded by half this; subdivide while it's too big.
    let dx = x2 - x0;
    let dy = y2 - y0;
    let denom_sq = dx * dx + dy * dy;
    let cross = (x1 - x0) * dy - (y1 - y0) * dx;
    let dist_sq = if denom_sq > 0.0 {
        (cross * cross) / denom_sq
    } else {
        let ex = x1 - x0;
        let ey = y1 - y0;
        ex * ex + ey * ey
    };
    if stop_subdividing(depth, *budget, &[x0, y0, x1, y1, x2, y2]) || dist_sq <= 4.0 * tol_sq {
        push_segment(
            out,
            budget,
            Segment {
                x0,
                y0,
                x1: x2,
                y1: y2,
            },
        );
        return;
    }
    // Midpoint subdivide via de Casteljau.
    let m01x = 0.5 * (x0 + x1);
    let m01y = 0.5 * (y0 + y1);
    let m12x = 0.5 * (x1 + x2);
    let m12y = 0.5 * (y1 + y2);
    let mx = 0.5 * (m01x + m12x);
    let my = 0.5 * (m01y + m12y);
    flatten_quad(x0, y0, m01x, m01y, mx, my, tol_sq, out, budget, depth + 1);
    flatten_quad(mx, my, m12x, m12y, x2, y2, tol_sq, out, budget, depth + 1);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn flatten_cubic(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    x3: f32,
    y3: f32,
    tol_sq: f32,
    out: &mut Vec<Segment>,
    budget: &mut usize,
    depth: u32,
) {
    // Wang's bound for cubic flatness: sample the perpendicular
    // distance of both control points to the chord; if both are below
    // the tolerance, accept the chord.
    let dx = x3 - x0;
    let dy = y3 - y0;
    let denom_sq = dx * dx + dy * dy;
    let (d1_sq, d2_sq) = if denom_sq > 0.0 {
        let c1 = (x1 - x0) * dy - (y1 - y0) * dx;
        let c2 = (x2 - x0) * dy - (y2 - y0) * dx;
        ((c1 * c1) / denom_sq, (c2 * c2) / denom_sq)
    } else {
        let e1x = x1 - x0;
        let e1y = y1 - y0;
        let e2x = x2 - x0;
        let e2y = y2 - y0;
        (e1x * e1x + e1y * e1y, e2x * e2x + e2y * e2y)
    };
    if stop_subdividing(depth, *budget, &[x0, y0, x1, y1, x2, y2, x3, y3])
        || (d1_sq <= tol_sq && d2_sq <= tol_sq)
    {
        push_segment(
            out,
            budget,
            Segment {
                x0,
                y0,
                x1: x3,
                y1: y3,
            },
        );
        return;
    }
    // de Casteljau subdivide at t=0.5.
    let m01x = 0.5 * (x0 + x1);
    let m01y = 0.5 * (y0 + y1);
    let m12x = 0.5 * (x1 + x2);
    let m12y = 0.5 * (y1 + y2);
    let m23x = 0.5 * (x2 + x3);
    let m23y = 0.5 * (y2 + y3);
    let m012x = 0.5 * (m01x + m12x);
    let m012y = 0.5 * (m01y + m12y);
    let m123x = 0.5 * (m12x + m23x);
    let m123y = 0.5 * (m12y + m23y);
    let mx = 0.5 * (m012x + m123x);
    let my = 0.5 * (m012y + m123y);
    flatten_cubic(
        x0,
        y0,
        m01x,
        m01y,
        m012x,
        m012y,
        mx,
        my,
        tol_sq,
        out,
        budget,
        depth + 1,
    );
    flatten_cubic(
        mx,
        my,
        m123x,
        m123y,
        m23x,
        m23y,
        x3,
        y3,
        tol_sq,
        out,
        budget,
        depth + 1,
    );
}
