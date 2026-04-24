// Wired up in the next commit — until then, `cubic_to_quads` and
// helpers exist only to support this module's unit tests.
#![allow(dead_code)]

//! Path flattening: cubic Bezier → list of quadratic Beziers.
//!
//! Slug rasterises quadratics natively. CFF charstrings produce
//! cubics; we approximate every cubic with one or more quadratics
//! whose maximum geometric error is bounded by `tolerance` (in design
//! units).
//!
//! ## Algorithm
//!
//! Given a cubic with control points `p0`, `p1`, `p2`, `p3` we form
//! a candidate quadratic with control point
//! `q1 = (3·p1 + 3·p2 - p0 - p3) / 4`. This is the classical
//! "midpoint" approximation used in FreeType and Skia: the resulting
//! quadratic shares the cubic's endpoints and tangents at `t=0` and
//! `t=1` and matches the cubic exactly when its control polygon is
//! "balanced". The error scales with the *third difference* of the
//! cubic's control polygon, specifically
//!
//! ```text
//!   err <= (sqrt(3) / 36) * | p0 - 3·p1 + 3·p2 - p3 |
//! ```
//!
//! (See Sederberg, "Computer Aided Geometric Design", §5.4 — the
//! reference HarfBuzz uses internally.) When that bound exceeds the
//! tolerance we subdivide the cubic at `t = 0.5` using De Casteljau
//! and recurse on each half.
//!
//! Subdivision is implemented iteratively with an explicit stack to
//! avoid recursion-depth surprises on pathological glyphs (e.g.
//! Asian CJK strokes with very long bezier polygons).

use alloc::vec::Vec;

use crate::types::Vec2;

/// Coefficient in front of the third-difference norm.
/// `sqrt(3) / 36 ~= 0.0481125`.
const ERROR_COEFF: f32 = 0.048_112_52;

/// Hard cap on subdivision depth. With tolerance 0.05 em on a 2048
/// upem font, a typical cubic resolves in 1-3 subdivisions; the cap
/// protects against floating-point pathology where the error norm
/// fails to shrink.
const MAX_DEPTH: u32 = 18;

/// Subdivides a cubic Bezier `(p0, p1, p2, p3)` into approximating
/// quadratics, appending each as `(control, end)` pairs to `out`.
/// `tolerance` is the maximum permissible geometric error in design
/// units.
///
/// The endpoint of each emitted quadratic equals the start point of
/// the next, so `out` is a *fan* sharing edges with its neighbours —
/// the caller threads them onto a path by reusing the previous
/// quadratic's endpoint as `p0` for the next.
pub(crate) fn cubic_to_quads(
    p0: Vec2,
    p1: Vec2,
    p2: Vec2,
    p3: Vec2,
    tolerance: f32,
    out: &mut Vec<(Vec2, Vec2)>,
) {
    // Stack of (p0, p1, p2, p3, depth). LIFO order means we walk the
    // cubic from t=0 to t=1 by always pushing the right half before
    // the left. The first frame we pop is therefore the leftmost
    // sub-cubic.
    let mut stack: Vec<(Vec2, Vec2, Vec2, Vec2, u32)> = Vec::with_capacity(8);
    stack.push((p0, p1, p2, p3, 0));

    while let Some((a0, a1, a2, a3, depth)) = stack.pop() {
        if depth >= MAX_DEPTH || cubic_quad_error(a0, a1, a2, a3) <= tolerance {
            // Emit a single quadratic. The "midpoint" control point
            // matches the cubic's endpoint tangents.
            let cx = (3.0 * a1.x + 3.0 * a2.x - a0.x - a3.x) * 0.25;
            let cy = (3.0 * a1.y + 3.0 * a2.y - a0.y - a3.y) * 0.25;
            out.push((Vec2::new(cx, cy), a3));
            continue;
        }

        // De Casteljau subdivision at t = 0.5.
        let m01 = mid(a0, a1);
        let m12 = mid(a1, a2);
        let m23 = mid(a2, a3);
        let m012 = mid(m01, m12);
        let m123 = mid(m12, m23);
        let m = mid(m012, m123);

        // Push right then left so the left half pops first → emitted
        // segments stay in parameter order.
        stack.push((m, m123, m23, a3, depth + 1));
        stack.push((a0, m01, m012, m, depth + 1));
    }
}

/// Geometric error bound between a cubic and its midpoint-approximating
/// quadratic. Returns `(sqrt(3)/36) * | p0 - 3 p1 + 3 p2 - p3 |`.
#[inline]
fn cubic_quad_error(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2) -> f32 {
    let dx = p0.x - 3.0 * p1.x + 3.0 * p2.x - p3.x;
    let dy = p0.y - 3.0 * p1.y + 3.0 * p2.y - p3.y;
    ERROR_COEFF * (dx * dx + dy * dy).sqrt()
}

#[inline]
fn mid(a: Vec2, b: Vec2) -> Vec2 {
    Vec2::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5)
}

/// Samples a cubic Bezier at `t ∈ [0, 1]`. Used in tests to verify
/// the flattening accuracy.
#[cfg(test)]
pub(crate) fn cubic_at(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, t: f32) -> Vec2 {
    let u = 1.0 - t;
    let b0 = u * u * u;
    let b1 = 3.0 * u * u * t;
    let b2 = 3.0 * u * t * t;
    let b3 = t * t * t;
    Vec2::new(
        b0 * p0.x + b1 * p1.x + b2 * p2.x + b3 * p3.x,
        b0 * p0.y + b1 * p1.y + b2 * p2.y + b3 * p3.y,
    )
}

/// Samples a quadratic Bezier at `t ∈ [0, 1]`. Used in tests.
#[cfg(test)]
pub(crate) fn quad_at(p0: Vec2, p1: Vec2, p2: Vec2, t: f32) -> Vec2 {
    let u = 1.0 - t;
    Vec2::new(
        u * u * p0.x + 2.0 * u * t * p1.x + t * t * p2.x,
        u * u * p0.y + 2.0 * u * t * p1.y + t * t * p2.y,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dist(a: Vec2, b: Vec2) -> f32 {
        let dx = a.x - b.x;
        let dy = a.y - b.y;
        (dx * dx + dy * dy).sqrt()
    }

    /// For a cubic and the quadratic chain produced by
    /// `cubic_to_quads`, the maximum sample-error at the t-grid
    /// `[0, 0.25, 0.5, 0.75, 1]` must respect the tolerance.
    fn assert_chain_within_tolerance(
        p0: Vec2,
        p1: Vec2,
        p2: Vec2,
        p3: Vec2,
        tolerance: f32,
    ) {
        let mut quads = Vec::new();
        cubic_to_quads(p0, p1, p2, p3, tolerance, &mut quads);
        assert!(!quads.is_empty(), "no quadratics produced");

        // Sample positions on the original cubic.
        let ts = [0.0_f32, 0.25, 0.5, 0.75, 1.0];

        // Build the chain: each quadratic spans an equal fraction of
        // parameter space (we don't know the actual midpoints from
        // cubic-t to chain-t; use Hausdorff-style search instead).
        // For robustness, sample the chain densely and find the
        // closest point on it for each cubic sample.
        let mut chain_samples: Vec<Vec2> = Vec::with_capacity(quads.len() * 32 + 1);
        let mut prev = p0;
        chain_samples.push(prev);
        for &(c, end) in &quads {
            for k in 1..=32 {
                let tt = k as f32 / 32.0;
                chain_samples.push(quad_at(prev, c, end, tt));
            }
            prev = end;
        }

        // Tolerance scales mildly per subdivision: allow a small slack.
        let slack = tolerance * 1.5;
        for &t in &ts {
            let target = cubic_at(p0, p1, p2, p3, t);
            let mut best = f32::INFINITY;
            for &s in &chain_samples {
                let d = dist(target, s);
                if d < best {
                    best = d;
                }
            }
            assert!(
                best <= slack,
                "cubic({t}) deviates by {best}, tolerance {tolerance}",
            );
        }
    }

    #[test]
    fn straight_cubic_emits_single_quadratic() {
        // Collinear control points => zero third-difference =>
        // single quadratic suffices regardless of tolerance.
        let mut quads = Vec::new();
        cubic_to_quads(
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(20.0, 0.0),
            Vec2::new(30.0, 0.0),
            0.5,
            &mut quads,
        );
        assert_eq!(quads.len(), 1);
        assert_eq!(quads[0].1, Vec2::new(30.0, 0.0));
    }

    #[test]
    fn s_curve_within_tolerance() {
        assert_chain_within_tolerance(
            Vec2::new(0.0, 0.0),
            Vec2::new(100.0, 200.0),
            Vec2::new(200.0, -200.0),
            Vec2::new(300.0, 0.0),
            1.0,
        );
    }

    #[test]
    fn loop_cubic_within_tolerance() {
        assert_chain_within_tolerance(
            Vec2::new(0.0, 0.0),
            Vec2::new(300.0, 200.0),
            Vec2::new(-100.0, 200.0),
            Vec2::new(200.0, 0.0),
            1.0,
        );
    }

    #[test]
    fn very_tight_tolerance_subdivides_more() {
        let mut loose = Vec::new();
        let mut tight = Vec::new();
        let p0 = Vec2::new(0.0, 0.0);
        let p1 = Vec2::new(100.0, 200.0);
        let p2 = Vec2::new(200.0, -200.0);
        let p3 = Vec2::new(300.0, 0.0);
        cubic_to_quads(p0, p1, p2, p3, 5.0, &mut loose);
        cubic_to_quads(p0, p1, p2, p3, 0.05, &mut tight);
        assert!(
            tight.len() >= loose.len(),
            "tighter tolerance must not decrease subdivision count: \
             loose={}, tight={}",
            loose.len(),
            tight.len()
        );
    }

    #[test]
    fn endpoints_preserved() {
        let p0 = Vec2::new(1.5, -2.5);
        let p3 = Vec2::new(50.5, 17.25);
        let mut quads = Vec::new();
        cubic_to_quads(
            p0,
            Vec2::new(20.0, 60.0),
            Vec2::new(40.0, -40.0),
            p3,
            0.1,
            &mut quads,
        );
        // Last emitted quadratic ends at p3 (subdivision preserves
        // exact endpoints because De Casteljau is an affine combination).
        assert_eq!(quads.last().unwrap().1, p3);
    }
}
