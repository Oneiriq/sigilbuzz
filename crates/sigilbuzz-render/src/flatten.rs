//! Outline curve flattening.
//!
//! Converts a [`sigilbuzz::tables::PathOp`] stream into a list of
//! straight line segments. The transform from font design units into
//! pixel space is applied here so the rasterizer never has to think in
//! design units.
//!
//! The public [`flatten`] and [`flatten_grouped`] return the edges the
//! path draws and nothing more: a contour without `Close` stays open.
//! Curve consumers rely on that. An MSDF generator, for one, flattens a
//! single `MoveTo` plus one drawing op per Bezier, where a closing edge
//! would add a reversed duplicate of every edge.
//!
//! A fill treats an open contour as closed, and the scanline rasterizer
//! would streak from its unbalanced edge to the edge of the glyph box.
//! The crate's own fill paths therefore flatten with
//! [`flatten_fill`], which closes a contour without `Close` at the next
//! `MoveTo` and at the end of the path.
//!
//! Quadratic and cubic Beziers are flattened with adaptive midpoint
//! subdivision. The recursion stops when the maximum perpendicular
//! distance from the curve to the chord falls below a tolerance
//! (`~0.25` device pixels), which yields anti-aliased edges
//! indistinguishable from a true curve at the target em size.

use alloc::vec::Vec;

use sigilbuzz::tables::PathOp;

use crate::affine::Affine;

mod arc_length;
mod subdivide;

pub use arc_length::{
    arc_length_cubic, arc_length_cubic_solve_t, arc_length_quad, arc_length_quad_solve_t,
};

use subdivide::{flatten_cubic, flatten_quad};

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
/// Produced by [`flatten`]. Coordinates are post-transform: the
/// [`Affine`] applied during flattening has already moved them into
/// device-pixel space, so consumers can read them directly without
/// re-applying the design-units -> pixels mapping.
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
/// A contour without `Close` stays open: no segment returns from its
/// last point to its start. The segments are exactly the edges the
/// path draws, so flattening one `MoveTo` plus one drawing op at a time
/// yields that op's chords and nothing else. A fill treats an open
/// contour as closed, so a caller that fills the segments itself should
/// close such a contour first, for example by appending `Close`. The
/// crate's rasterizer does this for every outline it fills.
///
/// Work is bounded for hostile input: a curve with a NaN or infinite
/// control point is emitted as its chord, and once 2^20 segments exist
/// every further curve is emitted as its chord too.
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
///
/// // Without `Close`, a single line stays a single segment.
/// let line = [
///     PathOp::MoveTo { x: 0.0, y: 0.0 },
///     PathOp::LineTo { x: 10.0, y: 0.0 },
/// ];
/// assert_eq!(flatten(line, &Affine::identity(), DEFAULT_TOLERANCE).len(), 1);
/// ```
pub fn flatten<I>(ops: I, xform: &Affine, tolerance: f32) -> Vec<Segment>
where
    I: IntoIterator<Item = PathOp>,
{
    flatten_impl(ops, xform, tolerance, MAX_SEGMENTS, false)
}

/// [`flatten`] for a fill: a contour without `Close` is closed
/// implicitly. When the next `MoveTo` or the end of `ops` arrives while
/// the current point is away from the contour start, the segment back
/// to the start is emitted, the one `Close` would emit. Every contour
/// is then a closed loop, as fill rules assume, so an open contour
/// cannot leave an unbalanced edge that streaks across the fill.
///
/// Outlines that close every contour flatten exactly as with
/// [`flatten`].
pub(crate) fn flatten_fill<I>(ops: I, xform: &Affine, tolerance: f32) -> Vec<Segment>
where
    I: IntoIterator<Item = PathOp>,
{
    flatten_fill_limited(ops, xform, tolerance, MAX_SEGMENTS)
}

/// [`flatten_fill`] with an explicit subdivision budget. Once `limit`
/// segments have been emitted, each remaining curve contributes only
/// its chord. Output below the budget is identical to
/// [`flatten_fill`].
pub(crate) fn flatten_fill_limited<I>(
    ops: I,
    xform: &Affine,
    tolerance: f32,
    limit: usize,
) -> Vec<Segment>
where
    I: IntoIterator<Item = PathOp>,
{
    flatten_impl(ops, xform, tolerance, limit, true)
}

/// Shared body of [`flatten`] and [`flatten_fill_limited`].
/// `close_open` selects whether a contour without `Close` gets the
/// implicit terminator segment at the next `MoveTo` and at the end.
fn flatten_impl<I>(
    ops: I,
    xform: &Affine,
    tolerance: f32,
    limit: usize,
    close_open: bool,
) -> Vec<Segment>
where
    I: IntoIterator<Item = PathOp>,
{
    let mut segs = Vec::new();
    let mut sx = 0.0_f32;
    let mut sy = 0.0_f32;
    let mut cx = 0.0_f32;
    let mut cy = 0.0_f32;
    let mut have_start = false;
    // True from a `MoveTo` (or a drawing op after one) until `Close`:
    // the contour still needs closing.
    let mut open = false;
    let tol = tolerance.max(1e-3);
    let tol_sq = tol * tol;
    let mut budget = limit;

    for op in ops {
        match op {
            PathOp::MoveTo { x, y } => {
                if close_open {
                    if let Some(seg) = closing_segment(open, cx, cy, sx, sy) {
                        push_segment(&mut segs, &mut budget, seg);
                    }
                }
                let (px, py) = xform.apply(x, y);
                sx = px;
                sy = py;
                cx = px;
                cy = py;
                have_start = true;
                open = true;
            }
            PathOp::LineTo { x, y } => {
                let (px, py) = xform.apply(x, y);
                push_segment(
                    &mut segs,
                    &mut budget,
                    Segment {
                        x0: cx,
                        y0: cy,
                        x1: px,
                        y1: py,
                    },
                );
                cx = px;
                cy = py;
                open = have_start;
            }
            PathOp::QuadTo {
                cx: ccx,
                cy: ccy,
                x,
                y,
            } => {
                let (p1x, p1y) = xform.apply(ccx, ccy);
                let (p2x, p2y) = xform.apply(x, y);
                flatten_quad(
                    cx,
                    cy,
                    p1x,
                    p1y,
                    p2x,
                    p2y,
                    tol_sq,
                    &mut segs,
                    &mut budget,
                    0,
                );
                cx = p2x;
                cy = p2y;
                open = have_start;
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
                flatten_cubic(
                    cx,
                    cy,
                    p1x,
                    p1y,
                    p2x,
                    p2y,
                    p3x,
                    p3y,
                    tol_sq,
                    &mut segs,
                    &mut budget,
                    0,
                );
                cx = p3x;
                cy = p3y;
                open = have_start;
            }
            PathOp::Close => {
                if let Some(seg) = closing_segment(have_start, cx, cy, sx, sy) {
                    push_segment(&mut segs, &mut budget, seg);
                }
                cx = sx;
                cy = sy;
                open = false;
            }
        }
    }
    if close_open {
        if let Some(seg) = closing_segment(open, cx, cy, sx, sy) {
            push_segment(&mut segs, &mut budget, seg);
        }
    }
    segs
}

/// Flattens an outline path, preserving per-source-Bézier boundaries.
///
/// Sibling to [`flatten`]. Same input, same chord output, same
/// `xform` and `tolerance` semantics, but the result is a
/// `Vec<FlattenedCurve>` where each entry corresponds to exactly one
/// drawing op from the input stream. A `LineTo` becomes one
/// [`FlattenedCurve::Line`]; a `QuadTo` becomes one
/// [`FlattenedCurve::Quad`] holding all chords produced by adaptive
/// subdivision of that quadratic; a `CubicTo` becomes one
/// [`FlattenedCurve::Cubic`]; a `Close` that needs an explicit
/// terminator emits a final `FlattenedCurve::Line` back to the
/// contour start. `MoveTo` and no-op `Close` (already at start)
/// produce no entries. As with [`flatten`], a contour without `Close`
/// stays open.
///
/// This is what MSDF-style generators want: RGB edge coloring picks
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
    // Shared with every curve so the chord output matches `flatten`.
    let mut budget = MAX_SEGMENTS;

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
                budget = budget.saturating_sub(1);
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
                flatten_quad(
                    cx,
                    cy,
                    p1x,
                    p1y,
                    p2x,
                    p2y,
                    tol_sq,
                    &mut segs,
                    &mut budget,
                    0,
                );
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
                flatten_cubic(
                    cx,
                    cy,
                    p1x,
                    p1y,
                    p2x,
                    p2y,
                    p3x,
                    p3y,
                    tol_sq,
                    &mut segs,
                    &mut budget,
                    0,
                );
                out.push(FlattenedCurve::Cubic(segs));
                cx = p3x;
                cy = p3y;
            }
            PathOp::Close => {
                if let Some(seg) = closing_segment(have_start, cx, cy, sx, sy) {
                    budget = budget.saturating_sub(1);
                    out.push(FlattenedCurve::Line(seg));
                }
                cx = sx;
                cy = sy;
            }
        }
    }
    out
}

/// The segment that closes a contour, from the current point
/// `(cx, cy)` back to the contour start `(sx, sy)`. `None` when
/// `active` is false (no contour to close) or the current point is
/// already at the start.
fn closing_segment(active: bool, cx: f32, cy: f32, sx: f32, sy: f32) -> Option<Segment> {
    (active && (cx != sx || cy != sy)).then_some(Segment {
        x0: cx,
        y0: cy,
        x1: sx,
        y1: sy,
    })
}

const MAX_DEPTH: u32 = 16;

/// Segment budget for one [`flatten`] or [`flatten_grouped`] call, and
/// for the combined layers of one color glyph or SVG document. Curve
/// subdivision stops once this many segments exist, so hostile control
/// points cannot turn each curve into `2^MAX_DEPTH` segments. Real
/// glyph outlines produce a few thousand.
pub(crate) const MAX_SEGMENTS: usize = 1 << 20;

/// Appends `seg` and charges it against the subdivision budget.
fn push_segment(out: &mut Vec<Segment>, budget: &mut usize, seg: Segment) {
    *budget = budget.saturating_sub(1);
    out.push(seg);
}

#[cfg(test)]
mod tests;
