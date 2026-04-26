//! Public API contract tests for [`sigilbuzz_render::flatten`].
//!
//! These exercise the *public* call path through the crate root —
//! `sigilbuzz_render::{flatten, Segment, DEFAULT_TOLERANCE, Affine}`
//! — to lock the surface area downstream MSDF generators and
//! glyph-cache consumers depend on. The internal flattener has its
//! own unit tests in `flatten.rs`; the goal here is to make a
//! visibility regression a hard-failing test rather than a
//! `pub`-vs-`pub(crate)` slip nobody notices.

use sigilbuzz::tables::PathOp;
use sigilbuzz_render::{flatten, Affine, Segment, DEFAULT_TOLERANCE};

#[test]
fn flatten_is_callable_from_outside_the_crate() {
    // Triangle of straight edges — no subdivision involved, so the
    // segment count is exactly the explicit edges including Close.
    let ops = [
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::LineTo { x: 10.0, y: 0.0 },
        PathOp::LineTo { x: 10.0, y: 10.0 },
        PathOp::Close,
    ];

    let segs: Vec<Segment> = flatten(ops, &Affine::identity(), DEFAULT_TOLERANCE);

    assert_eq!(segs.len(), 3, "triangle yields 3 edges via Close");
    // First edge starts at the path origin.
    assert!(segs[0].x0.abs() < 1e-5 && segs[0].y0.abs() < 1e-5);
    // Close edge returns to origin.
    let last = segs[2];
    assert!(last.x1.abs() < 1e-5 && last.y1.abs() < 1e-5);
}

#[test]
fn flatten_subdivides_a_quadratic_under_default_tolerance() {
    // Tall arc — at 0.25 px tolerance this must produce many chords,
    // proving the public entry point really walks the subdivider and
    // is not an empty stub.
    let ops = vec![
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::QuadTo {
            cx: 50.0,
            cy: 100.0,
            x: 100.0,
            y: 0.0,
        },
    ];

    let segs = flatten(ops, &Affine::identity(), DEFAULT_TOLERANCE);

    assert!(
        segs.len() > 8,
        "expected adaptive subdivision under default tolerance, got {}",
        segs.len()
    );
    // Chain is contiguous: each segment's start matches the previous
    // segment's end. This is the load-bearing property MSDF consumers
    // assume.
    for window in segs.windows(2) {
        let prev = window[0];
        let next = window[1];
        assert!((prev.x1 - next.x0).abs() < 1e-5);
        assert!((prev.y1 - next.y0).abs() < 1e-5);
    }
    // Endpoints land on (0,0) → (100,0).
    assert!(segs[0].x0.abs() < 1e-5);
    let last = segs[segs.len() - 1];
    assert!((last.x1 - 100.0).abs() < 1e-5 && last.y1.abs() < 1e-5);
}

#[test]
fn default_tolerance_is_quarter_pixel() {
    // Lock the documented value — downstream consumers may rely on
    // it for cache-key computation.
    assert!((DEFAULT_TOLERANCE - 0.25).abs() < f32::EPSILON);
}
