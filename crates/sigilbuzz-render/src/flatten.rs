//! Outline curve flattening.
//!
//! Converts a [`sigilbuzz::tables::PathOp`] stream into a closed list
//! of straight line segments suitable for the scanline rasterizer. The
//! transform from font design units into pixel space is applied here
//! so the rasterizer never has to think in design units.
//!
//! Quadratic and cubic Beziers are flattened with adaptive midpoint
//! subdivision. The recursion stops when the maximum perpendicular
//! distance from the curve to the chord falls below a tolerance
//! (`~0.25` device pixels), which yields anti-aliased edges
//! indistinguishable from a true curve at the target em size.

use alloc::vec::Vec;

use sigilbuzz::tables::PathOp;

use crate::affine::Affine;

/// Default curve flattening tolerance in device pixels.
///
/// Matches the value used internally by [`crate::Rasterizer`] and is
/// suitable for AA glyph rendering at typical UI sizes. Downstream
/// MSDF / glyph-cache consumers should generally pass this unless they
/// have a specific reason to subdivide more or less aggressively.
pub const DEFAULT_TOLERANCE: f32 = 0.25;

/// One flattened source curve, grouping the straight chord
/// [`Segment`]s that came from a single Bézier in the input
/// [`PathOp`] stream.
///
/// Produced by [`flatten_grouped`]. Where [`flatten`] returns a single
/// flat `Vec<Segment>` and forgets per-source-Bézier identity (which
/// is fine for fill rasterization), `flatten_grouped` keeps each
/// chord chunk grouped under its source variant. This is the shape
/// MSDF generators want: edge-coloring decisions have to be made
/// *per source curve*, not per chord, so downstream code needs to
/// know which subset of chords came from one quadratic vs. cubic vs.
/// straight `LineTo` (or implicit close-line).
///
/// `Quad` and `Cubic` always carry at least one chord. A degenerate
/// curve still emits a single accept-the-chord segment, so consumers
/// can rely on `segs.first()` / `segs.last()` being meaningful.
#[derive(Debug, Clone, PartialEq)]
pub enum FlattenedCurve {
    /// One straight edge from a `LineTo`, or the implicit close-line
    /// emitted by `Close` when the current point hasn't returned to
    /// the contour start.
    Line(Segment),
    /// Chord chain from one quadratic Bézier (`QuadTo`).
    Quad(Vec<Segment>),
    /// Chord chain from one cubic Bézier (`CubicTo`).
    Cubic(Vec<Segment>),
}

/// One straight edge in pixel coordinates.
///
/// Produced by [`flatten`]. Coordinates are post-transform — the
/// [`Affine`] applied during flattening has already moved them into
/// device-pixel space, so consumers can read them directly without
/// re-applying the design-units → pixels mapping.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    /// Start x (pixel space).
    pub x0: f32,
    /// Start y (pixel space).
    pub y0: f32,
    /// End x (pixel space).
    pub x1: f32,
    /// End y (pixel space).
    pub y1: f32,
}

/// Flattens an outline path into a list of straight pixel-space segments.
///
/// Walks `ops` (a [`PathOp`] stream from
/// [`sigilbuzz::Face::glyph_outline`]) and emits one [`Segment`] per
/// straight edge in the flattened contour. Quadratic and cubic Béziers
/// are subdivided with adaptive midpoint de Casteljau until each chord
/// is within `tolerance` pixels of the true curve. `xform` is applied
/// to each control point before subdivision, so the returned segments
/// are already in device-pixel space.
///
/// `tolerance` is the maximum chord-to-curve perpendicular error in
/// pixel-space units. Smaller is more accurate and slower; pass
/// [`DEFAULT_TOLERANCE`] (`0.25`) for AA-quality rendering.
///
/// `Close` ops emit an explicit terminator segment back to the last
/// `MoveTo`, so the returned `Vec<Segment>` is a complete edge list
/// suitable for scanline rasterization, MSDF generation, or any other
/// edge-list consumer.
///
/// # Example
///
/// ```
/// use sigilbuzz_render::{flatten, Affine, DEFAULT_TOLERANCE};
/// use sigilbuzz::tables::PathOp;
///
/// let ops = vec![
///     PathOp::MoveTo { x: 0.0, y: 0.0 },
///     PathOp::QuadTo { cx: 50.0, cy: 100.0, x: 100.0, y: 0.0 },
///     PathOp::Close,
/// ];
/// let segments = flatten(ops, &Affine::identity(), DEFAULT_TOLERANCE);
/// assert!(!segments.is_empty());
/// // First segment starts at the path origin.
/// assert!(segments[0].x0.abs() < 1e-5 && segments[0].y0.abs() < 1e-5);
/// ```
pub fn flatten<I>(ops: I, xform: &Affine, tolerance: f32) -> Vec<Segment>
where
    I: IntoIterator<Item = PathOp>,
{
    let mut segs = Vec::new();
    let mut sx = 0.0_f32;
    let mut sy = 0.0_f32;
    let mut cx = 0.0_f32;
    let mut cy = 0.0_f32;
    let mut have_start = false;
    let tol = tolerance.max(1e-3);
    let tol_sq = tol * tol;

    for op in ops {
        match op {
            PathOp::MoveTo { x, y } => {
                let (px, py) = xform.apply(x, y);
                sx = px;
                sy = py;
                cx = px;
                cy = py;
                have_start = true;
            }
            PathOp::LineTo { x, y } => {
                let (px, py) = xform.apply(x, y);
                segs.push(Segment {
                    x0: cx,
                    y0: cy,
                    x1: px,
                    y1: py,
                });
                cx = px;
                cy = py;
            }
            PathOp::QuadTo {
                cx: ccx,
                cy: ccy,
                x,
                y,
            } => {
                let (p1x, p1y) = xform.apply(ccx, ccy);
                let (p2x, p2y) = xform.apply(x, y);
                flatten_quad(cx, cy, p1x, p1y, p2x, p2y, tol_sq, &mut segs, 0);
                cx = p2x;
                cy = p2y;
            }
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => {
                let (p1x, p1y) = xform.apply(c1x, c1y);
                let (p2x, p2y) = xform.apply(c2x, c2y);
                let (p3x, p3y) = xform.apply(x, y);
                flatten_cubic(cx, cy, p1x, p1y, p2x, p2y, p3x, p3y, tol_sq, &mut segs, 0);
                cx = p3x;
                cy = p3y;
            }
            PathOp::Close => {
                if have_start && (cx != sx || cy != sy) {
                    segs.push(Segment {
                        x0: cx,
                        y0: cy,
                        x1: sx,
                        y1: sy,
                    });
                }
                cx = sx;
                cy = sy;
            }
        }
    }
    segs
}

/// Flattens an outline path, preserving per-source-Bézier boundaries.
///
/// Sibling to [`flatten`]. Same input, same chord output, same
/// `xform` and `tolerance` semantics — but the result is a
/// `Vec<FlattenedCurve>` where each entry corresponds to exactly one
/// drawing op from the input stream. A `LineTo` becomes one
/// [`FlattenedCurve::Line`]; a `QuadTo` becomes one
/// [`FlattenedCurve::Quad`] holding all chords produced by adaptive
/// subdivision of that quadratic; a `CubicTo` becomes one
/// [`FlattenedCurve::Cubic`]; a `Close` that needs an explicit
/// terminator emits a final `FlattenedCurve::Line` back to the
/// contour start. `MoveTo` and no-op `Close` (already at start)
/// produce no entries.
///
/// This is what MSDF-style generators want — RGB edge coloring picks
/// channels per *source curve*, not per chord, so the consumer needs
/// to know which subset of chords came from one Bézier. The previous
/// workaround was to call [`flatten`] one tiny `MoveTo+draw` op pair
/// at a time per Bézier; this API replaces that with a single walk.
///
/// Determinism: chord output for a given `(ops, xform, tolerance)`
/// triple is bit-identical across calls. Concatenating the inner
/// segment lists in-order yields the same `Vec<Segment>` that
/// [`flatten`] would have produced for the same input.
///
/// # Example
///
/// ```
/// use sigilbuzz_render::{flatten_grouped, Affine, FlattenedCurve, DEFAULT_TOLERANCE};
/// use sigilbuzz::tables::PathOp;
///
/// let ops = vec![
///     PathOp::MoveTo { x: 0.0, y: 0.0 },
///     PathOp::CubicTo {
///         c1x: 50.0, c1y: 100.0,
///         c2x: 100.0, c2y: 100.0,
///         x: 100.0, y: 0.0,
///     },
///     PathOp::Close,
/// ];
/// let curves = flatten_grouped(ops, &Affine::identity(), DEFAULT_TOLERANCE);
/// assert_eq!(curves.len(), 2); // cubic + close-line
/// match &curves[0] {
///     FlattenedCurve::Cubic(segs) => assert!(segs.len() > 1),
///     _ => panic!("expected Cubic"),
/// }
/// ```
pub fn flatten_grouped<I>(ops: I, xform: &Affine, tolerance: f32) -> Vec<FlattenedCurve>
where
    I: IntoIterator<Item = PathOp>,
{
    let mut out = Vec::new();
    let mut sx = 0.0_f32;
    let mut sy = 0.0_f32;
    let mut cx = 0.0_f32;
    let mut cy = 0.0_f32;
    let mut have_start = false;
    let tol = tolerance.max(1e-3);
    let tol_sq = tol * tol;

    for op in ops {
        match op {
            PathOp::MoveTo { x, y } => {
                let (px, py) = xform.apply(x, y);
                sx = px;
                sy = py;
                cx = px;
                cy = py;
                have_start = true;
            }
            PathOp::LineTo { x, y } => {
                let (px, py) = xform.apply(x, y);
                out.push(FlattenedCurve::Line(Segment {
                    x0: cx,
                    y0: cy,
                    x1: px,
                    y1: py,
                }));
                cx = px;
                cy = py;
            }
            PathOp::QuadTo {
                cx: ccx,
                cy: ccy,
                x,
                y,
            } => {
                let (p1x, p1y) = xform.apply(ccx, ccy);
                let (p2x, p2y) = xform.apply(x, y);
                let mut segs = Vec::new();
                flatten_quad(cx, cy, p1x, p1y, p2x, p2y, tol_sq, &mut segs, 0);
                out.push(FlattenedCurve::Quad(segs));
                cx = p2x;
                cy = p2y;
            }
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => {
                let (p1x, p1y) = xform.apply(c1x, c1y);
                let (p2x, p2y) = xform.apply(c2x, c2y);
                let (p3x, p3y) = xform.apply(x, y);
                let mut segs = Vec::new();
                flatten_cubic(cx, cy, p1x, p1y, p2x, p2y, p3x, p3y, tol_sq, &mut segs, 0);
                out.push(FlattenedCurve::Cubic(segs));
                cx = p3x;
                cy = p3y;
            }
            PathOp::Close => {
                if have_start && (cx != sx || cy != sy) {
                    out.push(FlattenedCurve::Line(Segment {
                        x0: cx,
                        y0: cy,
                        x1: sx,
                        y1: sy,
                    }));
                }
                cx = sx;
                cy = sy;
            }
        }
    }
    out
}

const MAX_DEPTH: u32 = 16;

#[allow(clippy::too_many_arguments)]
fn flatten_quad(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    tol_sq: f32,
    out: &mut Vec<Segment>,
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
    if depth >= MAX_DEPTH || dist_sq <= 4.0 * tol_sq {
        out.push(Segment {
            x0,
            y0,
            x1: x2,
            y1: y2,
        });
        return;
    }
    // Midpoint subdivide via de Casteljau.
    let m01x = 0.5 * (x0 + x1);
    let m01y = 0.5 * (y0 + y1);
    let m12x = 0.5 * (x1 + x2);
    let m12y = 0.5 * (y1 + y2);
    let mx = 0.5 * (m01x + m12x);
    let my = 0.5 * (m01y + m12y);
    flatten_quad(x0, y0, m01x, m01y, mx, my, tol_sq, out, depth + 1);
    flatten_quad(mx, my, m12x, m12y, x2, y2, tol_sq, out, depth + 1);
}

#[allow(clippy::too_many_arguments)]
fn flatten_cubic(
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
    if depth >= MAX_DEPTH || (d1_sq <= tol_sq && d2_sq <= tol_sq) {
        out.push(Segment {
            x0,
            y0,
            x1: x3,
            y1: y3,
        });
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
        depth + 1,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_pass_through_unchanged() {
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 10.0, y: 0.0 },
            PathOp::LineTo { x: 10.0, y: 10.0 },
            PathOp::Close,
        ];
        let segs = flatten(ops, &Affine::identity(), 0.25);
        assert_eq!(segs.len(), 3);
        assert!((segs[0].x0 - 0.0).abs() < 1e-5);
        assert!((segs[2].x1 - 0.0).abs() < 1e-5);
    }

    #[test]
    fn affine_applied_to_segments() {
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 1.0, y: 0.0 },
        ];
        let xf = Affine::scale(10.0, 10.0);
        let segs = flatten(ops, &xf, 0.25);
        assert!((segs[0].x1 - 10.0).abs() < 1e-5);
    }

    #[test]
    fn quadratic_subdivides_to_chord_when_tight() {
        // Big arc forces several subdivisions.
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::QuadTo {
                cx: 50.0,
                cy: 100.0,
                x: 100.0,
                y: 0.0,
            },
        ];
        let segs = flatten(ops, &Affine::identity(), 0.25);
        assert!(
            segs.len() > 8,
            "expected adaptive subdivision, got {}",
            segs.len()
        );
        // First segment starts at (0,0).
        assert!((segs[0].x0).abs() < 1e-5);
        // Last segment ends at (100,0).
        assert!((segs[segs.len() - 1].x1 - 100.0).abs() < 1e-5);
    }

    #[test]
    fn cubic_subdivides() {
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::CubicTo {
                c1x: 0.0,
                c1y: 100.0,
                c2x: 100.0,
                c2y: 100.0,
                x: 100.0,
                y: 0.0,
            },
        ];
        let segs = flatten(ops, &Affine::identity(), 0.25);
        assert!(segs.len() > 4);
    }

    #[test]
    fn close_emits_terminator_segment() {
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 10.0, y: 0.0 },
            PathOp::LineTo { x: 10.0, y: 10.0 },
            PathOp::Close,
        ];
        let segs = flatten(ops, &Affine::identity(), 0.25);
        // Three explicit edges including the close.
        assert_eq!(segs.len(), 3);
        let last = segs[2];
        assert!((last.x1).abs() < 1e-5 && (last.y1).abs() < 1e-5);
    }

    // -------- flatten_grouped --------

    /// Helper: flatten the per-curve segment lists back to a single
    /// flat `Vec<Segment>` so we can cross-check against `flatten()`.
    fn ungroup(curves: &[FlattenedCurve]) -> Vec<Segment> {
        let mut out = Vec::new();
        for c in curves {
            match c {
                FlattenedCurve::Line(s) => out.push(*s),
                FlattenedCurve::Quad(v) | FlattenedCurve::Cubic(v) => out.extend_from_slice(v),
            }
        }
        out
    }

    #[test]
    fn flatten_grouped_mlqcz_yields_four_entries() {
        // M / L / Q / C / Z. The Z emits an implicit close-line.
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 10.0, y: 0.0 },
            PathOp::QuadTo {
                cx: 50.0,
                cy: 100.0,
                x: 100.0,
                y: 0.0,
            },
            PathOp::CubicTo {
                c1x: 100.0,
                c1y: 50.0,
                c2x: 50.0,
                c2y: 50.0,
                x: 0.0,
                y: 50.0,
            },
            PathOp::Close,
        ];
        let curves = flatten_grouped(ops, &Affine::identity(), 0.25);
        // L + Q + C + implicit close-Line = 4 entries.
        assert_eq!(curves.len(), 4, "got: {curves:?}");
        assert!(matches!(curves[0], FlattenedCurve::Line(_)));
        assert!(matches!(curves[1], FlattenedCurve::Quad(_)));
        assert!(matches!(curves[2], FlattenedCurve::Cubic(_)));
        assert!(matches!(curves[3], FlattenedCurve::Line(_)));
        // Quad / Cubic both subdivide.
        if let FlattenedCurve::Quad(segs) = &curves[1] {
            assert!(segs.len() > 1);
        }
        if let FlattenedCurve::Cubic(segs) = &curves[2] {
            assert!(segs.len() > 1);
        }
    }

    #[test]
    fn flatten_grouped_empty_input_is_empty() {
        let curves = flatten_grouped(core::iter::empty::<PathOp>(), &Affine::identity(), 0.25);
        assert!(curves.is_empty());
    }

    #[test]
    fn flatten_grouped_lone_moveto_is_empty() {
        let ops = [PathOp::MoveTo { x: 5.0, y: 5.0 }];
        let curves = flatten_grouped(ops, &Affine::identity(), 0.25);
        assert!(
            curves.is_empty(),
            "MoveTo with no draw ops should yield no curves, got {curves:?}"
        );
    }

    #[test]
    fn flatten_grouped_tight_tolerance_subdivides_cubic_heavily() {
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::CubicTo {
                c1x: 0.0,
                c1y: 100.0,
                c2x: 100.0,
                c2y: 100.0,
                x: 100.0,
                y: 0.0,
            },
        ];
        let curves = flatten_grouped(ops, &Affine::identity(), 0.01);
        assert_eq!(curves.len(), 1);
        match &curves[0] {
            FlattenedCurve::Cubic(segs) => {
                assert!(segs.len() > 8, "tight tolerance got {} segs", segs.len());
            }
            other => panic!("expected Cubic, got {other:?}"),
        }
    }

    #[test]
    fn flatten_grouped_is_deterministic() {
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::QuadTo {
                cx: 50.0,
                cy: 100.0,
                x: 100.0,
                y: 0.0,
            },
            PathOp::CubicTo {
                c1x: 0.0,
                c1y: 50.0,
                c2x: 100.0,
                c2y: 50.0,
                x: 100.0,
                y: 0.0,
            },
            PathOp::Close,
        ];
        let a = flatten_grouped(ops, &Affine::identity(), 0.25);
        let b = flatten_grouped(ops, &Affine::identity(), 0.25);
        assert_eq!(a, b);
    }

    #[test]
    fn flatten_grouped_segment_count_matches_flatten() {
        // Same chord output, just grouped — concatenating the per-curve
        // segment lists must equal flatten()'s flat output exactly.
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 10.0, y: 0.0 },
            PathOp::QuadTo {
                cx: 50.0,
                cy: 100.0,
                x: 100.0,
                y: 0.0,
            },
            PathOp::CubicTo {
                c1x: 100.0,
                c1y: 50.0,
                c2x: 50.0,
                c2y: 50.0,
                x: 0.0,
                y: 50.0,
            },
            PathOp::Close,
        ];
        let flat = flatten(ops, &Affine::identity(), 0.25);
        let grouped = flatten_grouped(ops, &Affine::identity(), 0.25);
        let ungrouped = ungroup(&grouped);
        assert_eq!(flat.len(), ungrouped.len(), "total chord count");
        assert_eq!(flat, ungrouped, "chord sequence must be bit-identical");
    }

    #[test]
    fn flatten_grouped_close_at_start_emits_no_line() {
        // Already at the contour start when Close hits — no implicit
        // close-line, so the output is exactly the LineTo.
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 10.0, y: 0.0 },
            PathOp::LineTo { x: 0.0, y: 0.0 },
            PathOp::Close,
        ];
        let curves = flatten_grouped(ops, &Affine::identity(), 0.25);
        assert_eq!(curves.len(), 2);
        assert!(matches!(curves[0], FlattenedCurve::Line(_)));
        assert!(matches!(curves[1], FlattenedCurve::Line(_)));
    }

    #[test]
    fn flatten_grouped_applies_affine() {
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 1.0, y: 0.0 },
        ];
        let curves = flatten_grouped(ops, &Affine::scale(10.0, 10.0), 0.25);
        match &curves[0] {
            FlattenedCurve::Line(s) => assert!((s.x1 - 10.0).abs() < 1e-5),
            other => panic!("expected Line, got {other:?}"),
        }
    }
}
