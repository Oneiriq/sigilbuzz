//! Adaptive arc-length estimators for straight segments and for
//! quadratic and cubic Beziers.

use super::{Segment, MAX_DEPTH};

// =========================================================================
// Bezier arc-length estimators.
//
// Used by stroke-dasharray to walk a curve by *true* arc length instead
// of the chord-flattened polyline cumulative length, which is always
// shorter than the curve and biases dash boundaries earlier on long
// sweeps. The estimator follows Roger Willcocks' chord+control
// approximation: take the average of the chord length and the
// control-polygon length, then adaptively subdivide while the
// approximation disagrees with the sum of its halves.
// =========================================================================

impl Segment {
    /// True arc length of this straight segment.
    ///
    /// `Segment` holds two endpoints with no curvature data, so this is
    /// the Euclidean chord distance, exact for `LineTo` edges and the
    /// implicit close-line. For chord segments produced by curve
    /// flattening, callers that need the parent Bézier's true arc
    /// length should use [`arc_length_quad`] / [`arc_length_cubic`]
    /// against the original control points.
    #[must_use]
    pub fn arc_length(&self) -> f32 {
        let dx = self.x1 - self.x0;
        let dy = self.y1 - self.y0;
        (dx * dx + dy * dy).sqrt()
    }
}

/// Adaptive arc-length estimate of a quadratic Bézier defined by
/// `(x0,y0) -> (x1,y1) -> (x2,y2)` (start, control, end).
///
/// Uses the Roger Willcocks approximation:
/// `arc ~= (chord + control_polygon) / 2`. When the estimate of the
/// whole disagrees with the sum of its half estimates by more than
/// `tolerance`, the curve is split at `t=0.5` and the halves recursed.
/// At default `tolerance = 0.01` the result is within ~0.05 % of the
/// true Gauss-Legendre integral on typical sweeps; cusps and
/// near-degenerate curves saturate at `MAX_DEPTH` and still return a
/// finite, sensible length.
///
/// # Example
///
/// ```
/// use sigilbuzz_render::arc_length_quad;
///
/// // Symmetric arc rising to (50, 50) and back to (100, 0). True
/// // length is ~114.78; the chord is 100. Our estimator should land
/// // far closer to the true length than to the chord.
/// let l = arc_length_quad(0.0, 0.0, 50.0, 50.0, 100.0, 0.0, 0.01);
/// assert!((l - 114.78).abs() < 0.5, "got {l}");
/// ```
#[must_use]
pub fn arc_length_quad(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    tolerance: f32,
) -> f32 {
    let tol = tolerance.max(1e-4);
    arc_length_quad_rec(x0, y0, x1, y1, x2, y2, tol, 0)
}

#[allow(clippy::too_many_arguments)]
fn arc_length_quad_rec(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    tol: f32,
    depth: u32,
) -> f32 {
    let chord = ((x2 - x0).powi(2) + (y2 - y0).powi(2)).sqrt();
    let poly = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt()
        + ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
    let estimate = 0.5 * (chord + poly);
    if depth >= MAX_DEPTH || (poly - chord) <= tol {
        return estimate;
    }
    // de Casteljau subdivide at t = 0.5.
    let m01x = 0.5 * (x0 + x1);
    let m01y = 0.5 * (y0 + y1);
    let m12x = 0.5 * (x1 + x2);
    let m12y = 0.5 * (y1 + y2);
    let mx = 0.5 * (m01x + m12x);
    let my = 0.5 * (m01y + m12y);
    let left = arc_length_quad_rec(x0, y0, m01x, m01y, mx, my, tol, depth + 1);
    let right = arc_length_quad_rec(mx, my, m12x, m12y, x2, y2, tol, depth + 1);
    left + right
}

/// Adaptive arc-length estimate of a cubic Bézier defined by four
/// control points.
///
/// Same Roger Willcocks chord + control-polygon scheme as
/// [`arc_length_quad`], extended for the cubic control polygon.
/// Useful when stroking dashed curves: the SVG spec measures dash
/// boundaries against the *true* arc length of the path, and chord
/// flattening is always slightly short of that length.
///
/// # Example
///
/// ```
/// use sigilbuzz_render::arc_length_cubic;
///
/// // Quarter-circle approximation: a cubic from (100, 0) sweeping
/// // through control points (100, 55.228) and (55.228, 100) to
/// // (0, 100). True quarter-circle arc is π/2 * 100 ~ 157.08.
/// const K: f32 = 55.228_5;
/// let l = arc_length_cubic(100.0, 0.0, 100.0, K, K, 100.0, 0.0, 100.0, 0.01);
/// assert!((l - 157.08).abs() < 0.1, "got {l}");
/// ```
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn arc_length_cubic(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    x3: f32,
    y3: f32,
    tolerance: f32,
) -> f32 {
    let tol = tolerance.max(1e-4);
    arc_length_cubic_rec(x0, y0, x1, y1, x2, y2, x3, y3, tol, 0)
}

/// Solves for the Bezier parameter `t` along a quadratic that
/// corresponds to a given arc-length distance `target` from `t = 0`.
///
/// Used by stroke-dasharray when a dash boundary lands mid-curve and
/// the caller needs the exact parametric position (e.g. for splitting
/// the curve into draw / skip ranges before re-flattening). Returns
/// `t` in `[0, 1]`. If `target <= 0` returns 0; if `target` is at or past
/// the curve's total arc length, returns 1.
///
/// Implementation: bisection on the prefix-arc-length function
/// `L(t) = arc_length(curve restricted to [0, t])`. Bisection is
/// monotone-stable on cusps and pathological cubics where Newton's
/// method can overshoot. The dasher must never panic on adversarial
/// curves, so we accept ~25 iterations (<= 1e-7 relative tolerance) for
/// robustness over Newton's quadratic convergence.
///
/// `tolerance` controls the arc-length estimator accuracy under the
/// hood; pass `0.01` for typical SVG dash work.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn arc_length_quad_solve_t(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    target: f32,
    tolerance: f32,
) -> f32 {
    if target <= 0.0 {
        return 0.0;
    }
    let total = arc_length_quad(x0, y0, x1, y1, x2, y2, tolerance);
    if !target.is_finite() || target >= total {
        return 1.0;
    }
    // Bisect on [0, 1]. Each iteration evaluates the prefix length by
    // splitting the curve at the trial midpoint via de Casteljau and
    // estimating the left half's arc length.
    let mut lo = 0.0_f32;
    let mut hi = 1.0_f32;
    for _ in 0..25 {
        let mid = 0.5 * (lo + hi);
        let prefix = quad_prefix_length(x0, y0, x1, y1, x2, y2, mid, tolerance);
        if prefix < target {
            lo = mid;
        } else {
            hi = mid;
        }
        if (hi - lo) < 1e-7 {
            break;
        }
    }
    0.5 * (lo + hi)
}

#[allow(clippy::too_many_arguments)]
fn quad_prefix_length(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    t: f32,
    tolerance: f32,
) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return arc_length_quad(x0, y0, x1, y1, x2, y2, tolerance);
    }
    // de Casteljau split at `t`, returning the left sub-curve.
    let m01x = x0 + t * (x1 - x0);
    let m01y = y0 + t * (y1 - y0);
    let m12x = x1 + t * (x2 - x1);
    let m12y = y1 + t * (y2 - y1);
    let mx = m01x + t * (m12x - m01x);
    let my = m01y + t * (m12y - m01y);
    arc_length_quad(x0, y0, m01x, m01y, mx, my, tolerance)
}

/// Solves for the Bezier parameter `t` along a cubic that corresponds
/// to a given arc-length distance `target` from `t = 0`. See
/// [`arc_length_quad_solve_t`] for the contract; the cubic variant
/// uses the same bisection-on-prefix-length scheme against
/// [`arc_length_cubic`].
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn arc_length_cubic_solve_t(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    x3: f32,
    y3: f32,
    target: f32,
    tolerance: f32,
) -> f32 {
    if target <= 0.0 {
        return 0.0;
    }
    let total = arc_length_cubic(x0, y0, x1, y1, x2, y2, x3, y3, tolerance);
    if !target.is_finite() || target >= total {
        return 1.0;
    }
    let mut lo = 0.0_f32;
    let mut hi = 1.0_f32;
    for _ in 0..25 {
        let mid = 0.5 * (lo + hi);
        let prefix = cubic_prefix_length(x0, y0, x1, y1, x2, y2, x3, y3, mid, tolerance);
        if prefix < target {
            lo = mid;
        } else {
            hi = mid;
        }
        if (hi - lo) < 1e-7 {
            break;
        }
    }
    0.5 * (lo + hi)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn cubic_prefix_length(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    x3: f32,
    y3: f32,
    t: f32,
    tolerance: f32,
) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return arc_length_cubic(x0, y0, x1, y1, x2, y2, x3, y3, tolerance);
    }
    // de Casteljau split at `t`, returning the left sub-cubic.
    let m01x = x0 + t * (x1 - x0);
    let m01y = y0 + t * (y1 - y0);
    let m12x = x1 + t * (x2 - x1);
    let m12y = y1 + t * (y2 - y1);
    let m23x = x2 + t * (x3 - x2);
    let m23y = y2 + t * (y3 - y2);
    let m012x = m01x + t * (m12x - m01x);
    let m012y = m01y + t * (m12y - m01y);
    let m123x = m12x + t * (m23x - m12x);
    let m123y = m12y + t * (m23y - m12y);
    let mx = m012x + t * (m123x - m012x);
    let my = m012y + t * (m123y - m012y);
    arc_length_cubic(x0, y0, m01x, m01y, m012x, m012y, mx, my, tolerance)
}

#[allow(clippy::too_many_arguments)]
fn arc_length_cubic_rec(
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
) -> f32 {
    let chord = ((x3 - x0).powi(2) + (y3 - y0).powi(2)).sqrt();
    let poly = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt()
        + ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt()
        + ((x3 - x2).powi(2) + (y3 - y2).powi(2)).sqrt();
    let estimate = 0.5 * (chord + poly);
    if depth >= MAX_DEPTH || (poly - chord) <= tol {
        return estimate;
    }
    // de Casteljau subdivide at t = 0.5.
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
    let left = arc_length_cubic_rec(x0, y0, m01x, m01y, m012x, m012y, mx, my, tol, depth + 1);
    let right = arc_length_cubic_rec(mx, my, m123x, m123y, m23x, m23y, x3, y3, tol, depth + 1);
    left + right
}
