//! Pathological-input coverage for the public `flatten()` API.
//!
//! Issue #208 surface tests in `flatten_public_api.rs` lock the happy
//! path; this companion suite exercises the corners called out in the
//! wave 16 brief — empty op streams, degenerate cubics, very tight /
//! zero / negative tolerances, NaN-poisoned coords. Goal: prove the
//! public entry point degrades gracefully rather than panicking,
//! infinite-looping, or blowing the stack.

use sigilbuzz::tables::PathOp;
use sigilbuzz_render::{flatten, Affine, DEFAULT_TOLERANCE};

#[test]
fn flatten_empty_input() {
    let ops: Vec<PathOp> = vec![];
    let segs = flatten(ops, &Affine::identity(), DEFAULT_TOLERANCE);
    assert!(segs.is_empty());
}

#[test]
fn flatten_only_movetos() {
    let ops = vec![PathOp::MoveTo { x: 1.0, y: 1.0 }];
    let segs = flatten(ops, &Affine::identity(), DEFAULT_TOLERANCE);
    assert!(segs.is_empty());
}

#[test]
fn flatten_close_with_no_move() {
    let ops = vec![PathOp::Close];
    let segs = flatten(ops, &Affine::identity(), DEFAULT_TOLERANCE);
    assert!(segs.is_empty());
}

#[test]
fn flatten_pathological_cubic_cusp() {
    // Both control points coincide at a single point, creating a cusp
    // with the chord. Should not infinite-loop or stack overflow; the
    // MAX_DEPTH guard caps it.
    let ops = vec![
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::CubicTo {
            c1x: 100.0,
            c1y: 100.0,
            c2x: 100.0,
            c2y: 100.0,
            x: 0.0,
            y: 0.0,
        },
    ];
    let segs = flatten(ops, &Affine::identity(), DEFAULT_TOLERANCE);
    eprintln!("segs len = {}", segs.len());
    assert!(!segs.is_empty());
}

#[test]
fn flatten_very_tight_tolerance() {
    // Tolerance 0.01 — should produce many segments but not panic.
    let ops = vec![
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
    let segs = flatten(ops, &Affine::identity(), 0.01);
    eprintln!("tight tol cubic segs = {}", segs.len());
    assert!(segs.len() > 8);
}

#[test]
fn flatten_zero_tolerance() {
    // Zero tolerance: should still terminate via MAX_DEPTH.
    let ops = vec![
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::QuadTo {
            cx: 50.0,
            cy: 100.0,
            x: 100.0,
            y: 0.0,
        },
    ];
    let segs = flatten(ops, &Affine::identity(), 0.0);
    eprintln!("zero tol segs = {}", segs.len());
    assert!(!segs.is_empty());
}

#[test]
fn flatten_negative_tolerance() {
    let ops = vec![
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::QuadTo {
            cx: 50.0,
            cy: 100.0,
            x: 100.0,
            y: 0.0,
        },
    ];
    let segs = flatten(ops, &Affine::identity(), -1.0);
    assert!(!segs.is_empty());
}

#[test]
fn flatten_nan_input_does_not_panic() {
    // Pathological input with NaN should terminate via MAX_DEPTH and
    // not panic the recursive subdivider. The flatness comparison is
    // NaN ≤ x → false on every recursion, so the depth cap is the
    // only thing keeping the call tree finite. Document the
    // worst-case segment count so a future regression to a stricter
    // bound is visible (today: 2^MAX_DEPTH = 65_536).
    let ops = vec![
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::QuadTo {
            cx: f32::NAN,
            cy: 0.0,
            x: 100.0,
            y: 0.0,
        },
    ];
    let segs = flatten(ops, &Affine::identity(), DEFAULT_TOLERANCE);
    assert!(!segs.is_empty(), "NaN curve still emits the chord segments");
    assert!(
        segs.len() <= 1 << 17,
        "depth cap broken: {} segments emitted",
        segs.len()
    );
}

#[test]
fn flatten_lineto_before_moveto() {
    // LineTo without preceding MoveTo: cx/cy initialized to 0,0, so the
    // segment goes from (0,0). Don't crash.
    let ops = vec![PathOp::LineTo { x: 10.0, y: 10.0 }];
    let segs = flatten(ops, &Affine::identity(), DEFAULT_TOLERANCE);
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].x0, 0.0);
    assert_eq!(segs[0].y0, 0.0);
    assert_eq!(segs[0].x1, 10.0);
}

#[test]
fn flatten_downstream_consumer_smoke_test_through_face_outline() {
    // Walks the exact path a downstream MSDF / glyph-cache consumer
    // would: load a real font, pull Face::glyph_outline, hand the ops
    // to the public flatten() entry point. Locks the contract that
    // sigilbuzz_render exports the *types* PathOp consumers need —
    // including `sigilbuzz::tables::PathOp` itself — and that flatten
    // accepts the iterator shape `Outline::ops().iter().copied()`.
    use sigilbuzz::{Blob, Face};

    const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).unwrap();
    let cmap = face.cmap().unwrap();
    let gid = cmap.glyph_id('A').expect("OpenSans has 'A'");
    let outline = face
        .glyph_outline(gid)
        .expect("glyph_outline ok")
        .expect("glyph_outline drew");
    assert!(
        !outline.is_empty(),
        "OpenSans 'A' outline must be non-empty"
    );

    // Mimic a 24px raster: scale design-units → pixel space.
    let upem = face.head().unwrap().units_per_em as f32;
    let s = 24.0 / upem;
    let xform = Affine::scale(s, -s);
    let segs = flatten(outline.ops().iter().copied(), &xform, DEFAULT_TOLERANCE);
    assert!(
        segs.len() > 8,
        "downstream consumer must get a populated edge list, got {} segments",
        segs.len()
    );
    // Edge list must form a chain — the load-bearing property MSDF
    // generators rely on. Tolerate small per-op rounding plus the
    // explicit Close-emitted terminator that jumps to the last MoveTo.
    let mut chain_breaks = 0;
    for window in segs.windows(2) {
        let prev = window[0];
        let next = window[1];
        if (prev.x1 - next.x0).abs() > 1e-3 || (prev.y1 - next.y0).abs() > 1e-3 {
            chain_breaks += 1;
        }
    }
    // OpenSans 'A' has at least one subpath boundary; expect a small
    // (single-digit) number of chain breaks, definitely not most segs.
    assert!(
        chain_breaks <= segs.len() / 4,
        "too many chain breaks: {} of {} segments",
        chain_breaks,
        segs.len()
    );
}
