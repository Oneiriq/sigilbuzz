//! AAT fallback shaping — Apple `morx` ligation and `kerx` kerning
//! when the font omits GSUB and GPOS.
//!
//! The fixture is `tests/fixtures/aat_synthetic.ttf`, a hand-built
//! 1-KB TrueType font carrying six glyphs (.notdef, f, i, fi, A, V)
//! plus hand-authored AAT tables:
//!
//! - `morx` version 2, one chain, one type-2 ligature subtable:
//!   `(f, i) → fi`.
//! - `kerx` version 2, one format-0 subtable: `(A, V) → -50`.
//!
//! The font has deliberately neither GSUB nor GPOS, so these tests
//! exercise sigilbuzz's AAT fallback path. Regenerate the fixture
//! with `python3 tests/tools/build_aat_fixture.py`.
//!
//! # No rustybuzz parity
//!
//! rustybuzz 0.20 (and the underlying ttf-parser) has no AAT
//! shaper — it ignores `morx` / `kerx` entirely and returns the
//! unshaped glyph stream. This is sigilbuzz-only coverage.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

/// Built by `tests/tools/build_aat_fixture.py`.
const AAT_SYNTHETIC: &[u8] = include_bytes!("fixtures/aat_synthetic.ttf");

fn shape_text(text: &str) -> Vec<sigilbuzz::Glyph> {
    let blob = Blob::new(AAT_SYNTHETIC);
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
    // (A, V) → -50 should land on A (half-split means A gets
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
