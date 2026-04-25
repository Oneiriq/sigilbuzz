//! AAT `kerx` format-4 (control-point kerning) — parse-only
//! end-to-end coverage.
//!
//! Format 4 attaches glyphs at exact control points (or anchor
//! points, or inline FUnit coordinates) instead of via advance
//! adjustment. The apply path needs glyf-point or ankr coordinate
//! reads which sigilbuzz's kerx module deliberately keeps
//! out-of-band; until that lands the format-4 subtable parses
//! cleanly but emits no kern.
//!
//! What this fixture proves:
//!
//! 1. A kerx that ships a format-4 subtable does not fail the parser.
//! 2. The format-4 subtable does not consume bytes that belong to
//!    its neighbours — a sibling format-0 subtable still applies
//!    its pair lookup unmolested.
//!
//! Fixture: `tests/fixtures/aat_kerx_fmt4.ttf` (~900 B) carries six
//! glyphs (.notdef, A, B, V, W, X) plus a `kerx` v2 table with two
//! subtables — a format-4 (action type 2 = coordinates, no-op state
//! machine) followed by a format-0 with a single (A, V) → -42 pair.
//!
//! Regenerate with `python3 tests/tools/build_aat_kerx_fmt4_fixture.py`.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

/// Built by `tests/tools/build_aat_kerx_fmt4_fixture.py`.
const AAT_KERX_FMT4: &[u8] = include_bytes!("fixtures/aat_kerx_fmt4.ttf");

fn shape_text(text: &str) -> Vec<sigilbuzz::Glyph> {
    let blob = Blob::new(AAT_KERX_FMT4);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 1000.0);
    let mut buf = Buffer::new();
    buf.push_str(text);
    shape(&font, &buf, &[]).unwrap().glyphs
}

#[test]
fn fmt4_fixture_parses_with_format4_and_format0_subtables() {
    let blob = Blob::new(AAT_KERX_FMT4);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(
        face.kerx().unwrap().is_some(),
        "fixture must carry a parseable kerx with the fmt4 subtable"
    );
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
fn fmt4_does_not_drop_sibling_format0_pair() {
    // (A, V) → -42 lives in the format-0 subtable that sits next to
    // the format-4. If the format-4 parse path were eating bytes
    // sloppily, the format-0 subtable would never load and (A, V)
    // would shape with no kern.
    //
    // half-split: A gets -42 - (-21) = -21, V gets -21.
    let glyphs = shape_text("AV");
    assert_eq!(glyphs.len(), 2);
    let half = -42 / 2;
    let left_delta = -42 - half;
    let right_delta = half;
    assert_eq!(glyphs[0].x_advance, 500 + left_delta, "A kerned by sibling fmt0 pair");
    assert_eq!(glyphs[1].x_advance, 500 + right_delta, "V balanced half");
    assert_eq!(
        glyphs[0].x_advance + glyphs[1].x_advance,
        1000 - 42,
        "total tightens by 42 — fmt4 neighbour did not corrupt fmt0"
    );
}

#[test]
fn fmt4_emits_no_kern_for_unrelated_pairs() {
    // Format 4's apply path is a stub — neither (B, V) nor any
    // other pair the state machine might "match" produces a kern.
    // The format-0 subtable also has no rule for these pairs, so
    // shaping must leave advances at 500.
    let glyphs = shape_text("BV");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].x_advance, 500);
    assert_eq!(glyphs[1].x_advance, 500);

    let glyphs = shape_text("AW");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].x_advance, 500);
    assert_eq!(glyphs[1].x_advance, 500);
}
