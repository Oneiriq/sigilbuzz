//! AAT fallback shaping: Apple `morx` ligation and `kerx` kerning
//! when the font omits GSUB and GPOS.
//!
//! Two fixtures live here:
//!
//! - `tests/fixtures/aat_synthetic.ttf` (1 KB) carries six glyphs
//!   (.notdef, f, i, fi, A, V) plus a `morx` v2 ligature subtable
//!   `(f, i) -> fi` and a `kerx` v2 format-0 subtable `(A, V) -> -50`.
//! - `tests/fixtures/aat_kerx_fmt2.ttf` (~900 B) carries six glyphs
//!   (.notdef, A, B, V, W, X) plus a `kerx` v2 format-2 subtable
//!   that uses an AAT compound-class layout: `{A, B}` are left
//!   class 1, `V` is right class 1, `W` is right class 2, with
//!   matrix `[[0, 0, 0], [0, -30, -50]]`. So `(A, V)` and `(B, V)`
//!   kern -30, while `(A, W)` and `(B, W)` kern -50.
//!
//! Each font has neither GSUB nor GPOS, so these tests
//! exercise sigilbuzz's AAT fallback path. Regenerate with
//! `python3 tests/tools/build_aat_fixture.py` and
//! `python3 tests/tools/build_aat_kerx_fmt2_fixture.py`.
//!
//! # No rustybuzz parity
//!
//! rustybuzz 0.20 (and the underlying ttf-parser) has no AAT
//! shaper: it ignores `morx` / `kerx` entirely and returns the
//! unshaped glyph stream. This is sigilbuzz-only coverage.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

/// Built by `tests/tools/build_aat_fixture.py`.
const AAT_SYNTHETIC: &[u8] = include_bytes!("fixtures/aat_synthetic.ttf");

/// Built by `tests/tools/build_aat_kerx_fmt2_fixture.py`.
const AAT_KERX_FMT2: &[u8] = include_bytes!("fixtures/aat_kerx_fmt2.ttf");

fn shape_text(text: &str) -> Vec<sigilbuzz::Glyph> {
    shape_with(AAT_SYNTHETIC, text)
}

fn shape_with(font_bytes: &'static [u8], text: &str) -> Vec<sigilbuzz::Glyph> {
    let blob = Blob::new(font_bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 1000.0);
    let mut buf = Buffer::new();
    buf.push_str(text);
    shape(&font, &buf, &[]).unwrap().glyphs
}

#[test]
fn fixture_has_aat_tables_and_no_opentype() {
    let blob = Blob::new(AAT_SYNTHETIC);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(
        face.morx().unwrap().is_some(),
        "fixture must carry a morx table"
    );
    assert!(
        face.kerx().unwrap().is_some(),
        "fixture must carry a kerx table"
    );
    assert!(
        face.gsub().unwrap().is_none(),
        "fixture must have no GSUB so morx wins"
    );
    assert!(
        face.gpos().unwrap().is_none(),
        "fixture must have no GPOS so kerx wins"
    );
}

#[test]
fn morx_fi_ligature_collapses_two_glyphs_to_one() {
    // The fixture's morx subtable maps (f=gid 1, i=gid 2) to
    // fi=gid 3. Shaping "fi" through the AAT fallback path should
    // produce a single glyph at gid 3.
    let glyphs = shape_text("fi");
    assert_eq!(glyphs.len(), 1, "f + i should ligate into one glyph");
    assert_eq!(glyphs[0].glyph_id, 3, "ligature glyph id is gid 3");
    // The ligature inherits the first component's cluster so
    // renderers mapping clusters back to bytes find the 'f' offset.
    assert_eq!(glyphs[0].cluster, 0);
    // fi was given an 800-unit advance in hmtx; no kerning applies.
    assert_eq!(glyphs[0].x_advance, 800);
}

#[test]
fn morx_leaves_non_matching_sequences_alone() {
    // "fif" has an ambiguous tail (f without a following i); the
    // state machine should still collapse the leading pair but
    // leave the trailing f untouched.
    let glyphs = shape_text("fif");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].glyph_id, 3, "first pair ligates");
    assert_eq!(glyphs[1].glyph_id, 1, "trailing f stays as-is");
}

#[test]
fn kerx_pair_reduces_a_advance_by_fifty() {
    // hmtx gives A and V each 500-unit advances. The kerx pair
    // (A, V) -> -50 should land on A (half-split means A gets
    // delta - delta/2 = -25 - -25 = -25... actually -50/2 = -25,
    // so A gets -50 - (-25) = -25 and V gets -25, see
    // HarfBuzz/sigilbuzz legacy-kern semantics).
    let glyphs = shape_text("AV");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].glyph_id, 4);
    assert_eq!(glyphs[1].glyph_id, 5);
    // Half-split: A takes (delta - delta/2) = -25, V takes delta/2 = -25.
    assert_eq!(glyphs[0].x_advance, 500 - 25, "A kerned toward V");
    assert_eq!(glyphs[1].x_advance, 500 - 25, "V balanced half");
    // Sum of advances matches the full -50 delta either way.
    assert_eq!(
        glyphs[0].x_advance + glyphs[1].x_advance,
        950,
        "total advance tightens by 50"
    );
}

#[test]
fn kerx_pair_not_present_leaves_advances_intact() {
    // No kern rule for (V, A); the pair should fall through
    // untouched.
    let glyphs = shape_text("VA");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].x_advance, 500);
    assert_eq!(glyphs[1].x_advance, 500);
}

// ---------------------------------------------------------------------
// kerx format 2: compound-class kerning. Uses the dedicated
// aat_kerx_fmt2.ttf fixture so the test font carries no morx and
// no GPOS, isolating the format-2 apply path.
// ---------------------------------------------------------------------

/// Half-split distribution of `delta` across a kern pair, matching
/// the policy in `apply_kerx`. Centralized here so the fixture-side
/// expectations stay readable when several pairs use different
/// deltas.
const fn half_split(delta: i32) -> (i32, i32) {
    let half = delta / 2;
    (delta - half, half)
}

#[test]
fn kerx_fmt2_fixture_has_no_legacy_kern_or_gpos() {
    let blob = Blob::new(AAT_KERX_FMT2);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(
        face.kerx().unwrap().is_some(),
        "fmt2 fixture must carry kerx"
    );
    assert!(
        face.gpos().unwrap().is_none(),
        "fmt2 fixture must omit GPOS so kerx is consulted"
    );
    assert!(
        face.kern().unwrap().is_none(),
        "fmt2 fixture must omit legacy kern so kerx is consulted"
    );
}

#[test]
fn kerx_fmt2_class_pair_av_kerns_minus_thirty() {
    // (A, V): A is left class 1, V is right class 1, matrix[1][1] = -30.
    let glyphs = shape_with(AAT_KERX_FMT2, "AV");
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
fn kerx_fmt2_class_pair_bv_shares_av_delta() {
    // (B, V): B is left class 1 (same as A), V is right class 1.
    // The compound-class scheme is the whole point: both rows of
    // the matrix's left class should produce the same delta.
    let glyphs = shape_with(AAT_KERX_FMT2, "BV");
    assert_eq!(glyphs.len(), 2);
    let (left_delta, right_delta) = half_split(-30);
    assert_eq!(glyphs[0].x_advance, 500 + left_delta);
    assert_eq!(glyphs[1].x_advance, 500 + right_delta);
}

#[test]
fn kerx_fmt2_class_pair_aw_kerns_minus_fifty() {
    // (A, W): A left class 1, W right class 2, matrix[1][2] = -50.
    let glyphs = shape_with(AAT_KERX_FMT2, "AW");
    assert_eq!(glyphs.len(), 2);
    let (left_delta, right_delta) = half_split(-50);
    assert_eq!(glyphs[0].x_advance, 500 + left_delta);
    assert_eq!(glyphs[1].x_advance, 500 + right_delta);
}

#[test]
fn kerx_fmt2_class_pair_bw_shares_aw_delta() {
    let glyphs = shape_with(AAT_KERX_FMT2, "BW");
    assert_eq!(glyphs.len(), 2);
    let (left_delta, right_delta) = half_split(-50);
    assert_eq!(glyphs[0].x_advance, 500 + left_delta);
    assert_eq!(glyphs[1].x_advance, 500 + right_delta);
}

#[test]
fn kerx_fmt2_default_classes_kern_zero() {
    // (V, A): V is left class 0, A is right class 0, matrix[0][0] = 0.
    let glyphs = shape_with(AAT_KERX_FMT2, "VA");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].x_advance, 500, "V untouched");
    assert_eq!(glyphs[1].x_advance, 500, "A untouched");
}

#[test]
fn kerx_fmt2_handles_multi_pair_run() {
    // "ABVW" yields three pairs:
    //   (A, B): left 1, right 0 -> matrix[1][0] = 0
    //   (B, V): left 1, right 1 -> -30
    //   (V, W): left 0, right 2 -> matrix[0][2] = 0
    // Only the middle pair contributes; the run's total advance
    // should drop by exactly 30 vs the un-kerned run.
    let glyphs = shape_with(AAT_KERX_FMT2, "ABVW");
    assert_eq!(glyphs.len(), 4);
    let total: i32 = glyphs.iter().map(|g| g.x_advance).sum();
    assert_eq!(total, 4 * 500 - 30);
}
