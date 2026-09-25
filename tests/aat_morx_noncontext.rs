//! AAT `morx` type 4 (non-contextual substitution): end-to-end
//! shape pass.
//!
//! Fixture: `tests/fixtures/aat_morx_noncontext.ttf` (~900 B) carries
//! five glyphs (`.notdef`, A, B, A.smcp, B.smcp) plus a `morx` v2
//! type-4 subtable with a single mapping `A (gid 1) -> A.smcp (gid 3)`.
//! B is intentionally *not* mapped so the test can prove the
//! substitution is per-glyph rather than blanket.
//!
//! The font has no GSUB / GPOS, so this test exercises *only* the
//! morx type-4 apply path.
//!
//! # No rustybuzz parity
//!
//! rustybuzz 0.20 has no AAT shaper. It ignores `morx` entirely.
//! This is sigilbuzz-only coverage. Regenerate with
//! `python3 tests/tools/build_aat_morx_noncontext_fixture.py`.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

/// Built by `tests/tools/build_aat_morx_noncontext_fixture.py`.
const AAT_MORX_NONCONTEXT: &[u8] = include_bytes!("fixtures/aat_morx_noncontext.ttf");

fn shape_text(text: &str) -> Vec<sigilbuzz::Glyph> {
    let blob = Blob::new(AAT_MORX_NONCONTEXT);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 1000.0);
    let mut buf = Buffer::new();
    buf.push_str(text);
    shape(&font, &buf, &[]).unwrap().glyphs
}

#[test]
fn fixture_has_morx_and_no_gsub() {
    let blob = Blob::new(AAT_MORX_NONCONTEXT);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(
        face.morx().unwrap().is_some(),
        "fixture must carry a morx table"
    );
    assert!(
        face.gsub().unwrap().is_none(),
        "fixture must omit GSUB so morx is consulted"
    );
}

#[test]
fn type4_substitutes_a_for_smcp() {
    // Single A -> A.smcp via the type-4 lookup. Output is one glyph
    // at gid 3.
    let glyphs = shape_text("A");
    assert_eq!(glyphs.len(), 1);
    assert_eq!(glyphs[0].glyph_id, 3, "A maps to gid 3 (A.smcp)");
}

#[test]
fn type4_leaves_unmapped_glyph_alone() {
    // B is in the font but not in the lookup; the original gid 2
    // must survive.
    let glyphs = shape_text("B");
    assert_eq!(glyphs.len(), 1);
    assert_eq!(glyphs[0].glyph_id, 2, "B is not in the lookup");
}

#[test]
fn type4_run_substitutes_only_mapped_glyphs() {
    // "AB" yields gid 3 (A.smcp) followed by gid 2 (B unchanged).
    let glyphs = shape_text("AB");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].glyph_id, 3);
    assert_eq!(glyphs[1].glyph_id, 2);
}

#[test]
fn type4_substitutes_every_occurrence() {
    // "AA" yields two gid-3 glyphs.
    let glyphs = shape_text("AA");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].glyph_id, 3);
    assert_eq!(glyphs[1].glyph_id, 3);
}

#[test]
fn type4_preserves_clusters() {
    // Substitution doesn't merge or split: clusters stay 1:1.
    let glyphs = shape_text("AB");
    assert_eq!(glyphs[0].cluster, 0);
    assert_eq!(glyphs[1].cluster, 1);
}
