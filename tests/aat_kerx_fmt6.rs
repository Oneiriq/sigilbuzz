//! AAT `kerx` format-6 (simple n x m kerning array): end-to-end shape
//! pass.
//!
//! Fixture: `tests/fixtures/aat_kerx_fmt6.ttf` (~900 B) carries six
//! glyphs (.notdef, A, B, V, W, X) plus a `kerx` v2 format-6 subtable
//! with a 2x3 grid:
//!
//! - row 0 (default) / row 1 ({A, B})
//! - col 0 (default) / col 1 ({V}) / col 2 ({W})
//! - matrix: row 0 = `[0, 0, 0]`; row 1 = `[0, -30, -50]`.
//!
//! So `(A, V)` and `(B, V)` kern -30, while `(A, W)` and `(B, W)`
//! kern -50; everything else falls through to the row-0 default.
//!
//! The font has no GSUB / GPOS / legacy `kern`, so this test
//! exercises *only* the kerx format-6 apply path.
//!
//! # No rustybuzz parity
//!
//! rustybuzz 0.20 has no AAT shaper. It ignores `kerx` entirely.
//! This is sigilbuzz-only coverage. Regenerate with
//! `python3 tests/tools/build_aat_kerx_fmt6_fixture.py`.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

/// Built by `tests/tools/build_aat_kerx_fmt6_fixture.py`.
const AAT_KERX_FMT6: &[u8] = include_bytes!("fixtures/aat_kerx_fmt6.ttf");

fn shape_text(text: &str) -> Vec<sigilbuzz::Glyph> {
    let blob = Blob::new(AAT_KERX_FMT6);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 1000.0);
    let mut buf = Buffer::new();
    buf.push_str(text);
    shape(&font, &buf, &[]).unwrap().glyphs
}

/// Half-split distribution of `delta` across a kern pair, matching
/// the policy in `apply_kerx`.
const fn half_split(delta: i32) -> (i32, i32) {
    let half = delta / 2;
    (delta - half, half)
}

#[test]
fn fmt6_fixture_has_kerx_and_no_pair_kerning_paths() {
    let blob = Blob::new(AAT_KERX_FMT6);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(face.kerx().unwrap().is_some(), "fixture must carry kerx");
    assert!(
        face.gpos().unwrap().is_none(),
        "fixture must omit GPOS so kerx is consulted"
    );
    assert!(
        face.kern().unwrap().is_none(),
        "fixture must omit legacy kern so kerx is consulted"
    );
}

#[test]
fn fmt6_class_pair_av_kerns_minus_thirty() {
    // (A, V): A is row 1, V is col 1, matrix[1][1] = -30.
    let glyphs = shape_text("AV");
    assert_eq!(glyphs.len(), 2);
    let (left_delta, right_delta) = half_split(-30);
    assert_eq!(glyphs[0].x_advance, 500 + left_delta);
    assert_eq!(glyphs[1].x_advance, 500 + right_delta);
    assert_eq!(
        glyphs[0].x_advance + glyphs[1].x_advance,
        1000 - 30,
        "total tightens by 30"
    );
}

#[test]
fn fmt6_class_pair_bv_shares_av_delta() {
    // (B, V): B is row 1 (same as A), V is col 1.
    let glyphs = shape_text("BV");
    assert_eq!(glyphs.len(), 2);
    let (left_delta, right_delta) = half_split(-30);
    assert_eq!(glyphs[0].x_advance, 500 + left_delta);
    assert_eq!(glyphs[1].x_advance, 500 + right_delta);
}

#[test]
fn fmt6_class_pair_aw_kerns_minus_fifty() {
    // (A, W): A row 1, W col 2, matrix[1][2] = -50.
    let glyphs = shape_text("AW");
    assert_eq!(glyphs.len(), 2);
    let (left_delta, right_delta) = half_split(-50);
    assert_eq!(glyphs[0].x_advance, 500 + left_delta);
    assert_eq!(glyphs[1].x_advance, 500 + right_delta);
}

#[test]
fn fmt6_class_pair_bw_shares_aw_delta() {
    let glyphs = shape_text("BW");
    assert_eq!(glyphs.len(), 2);
    let (left_delta, right_delta) = half_split(-50);
    assert_eq!(glyphs[0].x_advance, 500 + left_delta);
    assert_eq!(glyphs[1].x_advance, 500 + right_delta);
}

#[test]
fn fmt6_default_classes_kern_zero() {
    // (V, A): V is row 0, A is col 0, matrix[0][0] = 0.
    let glyphs = shape_text("VA");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].x_advance, 500, "V untouched");
    assert_eq!(glyphs[1].x_advance, 500, "A untouched");
}

#[test]
fn fmt6_handles_multi_pair_run() {
    // "ABVW" yields three pairs:
    //   (A, B): row 1, col 0 -> matrix[1][0] = 0
    //   (B, V): row 1, col 1 -> -30
    //   (V, W): row 0, col 2 -> matrix[0][2] = 0
    // Only the middle pair contributes; the run's total advance
    // should drop by exactly 30 vs the un-kerned run.
    let glyphs = shape_text("ABVW");
    assert_eq!(glyphs.len(), 4);
    let total: i32 = glyphs.iter().map(|g| g.x_advance).sum();
    assert_eq!(total, 4 * 500 - 30);
}
