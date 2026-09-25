//! AAT `kerx` format-4 (control-point + anchor-point): *apply*-path
//! end-to-end coverage.
//!
//! Format 4's apply path resolves anchor pairs to FUnit (dx, dy)
//! deltas that land on the current glyph's positioning offset. Three
//! action types exist; this file covers the two that need extra
//! tables to resolve:
//!
//! 1. **Type 0 (control points)**: pairs of glyf-point indices.
//!    `Face::glyph_points` returns each glyph's points in glyf-natural
//!    order (contour points + 4 phantoms); the shaper looks up
//!    `mark[mpi]` and `current[cpi]` and applies `mark - current`.
//!
//! 2. **Type 1 (anchor points)**: pairs of `ankr` indices. The
//!    `ankr` table resolves each `(gid, idx)` to a concrete (x, y);
//!    same `mark - current` math.
//!
//! Both fixtures ship a state machine that fires action 0 on the
//! "AB" pattern. The action records are crafted so the resolved
//! offset is `(500, 0)`, large and asymmetric enough to be obvious
//! in the assertion.
//!
//! Regenerate with `python3 tests/tools/build_aat_kerx_fmt4_apply_fixture.py`.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const AAT_KERX_FMT4_TYPE0: &[u8] = include_bytes!("fixtures/aat_kerx_fmt4_type0.ttf");
const AAT_KERX_FMT4_TYPE1: &[u8] = include_bytes!("fixtures/aat_kerx_fmt4_type1.ttf");

fn shape_text(font_bytes: &'static [u8], text: &str) -> Vec<sigilbuzz::Glyph> {
    let blob = Blob::new(font_bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 1000.0);
    let mut buf = Buffer::new();
    buf.push_str(text);
    shape(&font, &buf, &[]).unwrap().glyphs
}

#[test]
fn fmt4_type0_control_points_apply_offset_to_current_glyph() {
    // Fixture: A and B are 500-wide rectangles. The state machine
    // fires action 0 on "AB"; the action record references mark
    // point 1 (A's bottom-right corner = (500, 0)) and current
    // point 0 (B's bottom-left corner = (0, 0)). Expected offset on
    // B: (500, 0).
    let glyphs = shape_text(AAT_KERX_FMT4_TYPE0, "AB");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].x_offset, 0, "A is the marked glyph, no offset");
    assert_eq!(glyphs[0].y_offset, 0);
    assert_eq!(
        glyphs[1].x_offset, 500,
        "B's x_offset gets mark.x - current.x = 500 - 0"
    );
    assert_eq!(glyphs[1].y_offset, 0, "y components are both 0");
}

#[test]
fn fmt4_type0_skips_when_no_pair_match() {
    // "BB": class 5 in state 0 falls to entry 0 (noop). No action
    // fires; B keeps its untouched (x_offset, y_offset).
    let glyphs = shape_text(AAT_KERX_FMT4_TYPE0, "BB");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].x_offset, 0);
    assert_eq!(glyphs[1].x_offset, 0);
}

#[test]
fn fmt4_type0_advance_is_unaffected() {
    // Format 4 uses the *positioning offset*, not the advance. The
    // pen still moves by the glyph's hmtx width. Both glyphs are
    // 500-wide rectangles, so each advance stays at 500.
    let glyphs = shape_text(AAT_KERX_FMT4_TYPE0, "AB");
    assert_eq!(glyphs[0].x_advance, 500);
    assert_eq!(glyphs[1].x_advance, 500);
}

#[test]
fn fmt4_type1_anchor_points_apply_offset_to_current_glyph() {
    // ankr: A->anchor 0 at (500, 0), B->anchor 0 at (0, 0). Action
    // record references both index 0. Expected B offset = (500, 0).
    let glyphs = shape_text(AAT_KERX_FMT4_TYPE1, "AB");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].x_offset, 0);
    assert_eq!(glyphs[1].x_offset, 500);
    assert_eq!(glyphs[1].y_offset, 0);
}

#[test]
fn fmt4_type1_face_ankr_is_present() {
    let blob = Blob::new(AAT_KERX_FMT4_TYPE1);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(
        face.ankr().unwrap().is_some(),
        "fixture must ship a parseable ankr"
    );
    // Sanity: the lookup returns the recorded coordinates.
    let ankr = face.ankr().unwrap().unwrap();
    assert_eq!(ankr.anchor_for(1, 0), Some((500, 0)));
    assert_eq!(ankr.anchor_for(2, 0), Some((0, 0)));
}
