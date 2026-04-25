//! AAT `morx` type 5 (insertion) — end-to-end shape pass.
//!
//! Fixture: `tests/fixtures/aat_morx_insertion.ttf` (~950 B) carries
//! six glyphs (`.notdef`, A, B, C, D, mark) plus a `morx` v2 type-5
//! subtable wired to insert one mark glyph (gid 5) AFTER every A
//! (gid 1) it sees. The state machine has one state and a single
//! insertion entry; the rest of the run passes through.
//!
//! The font has no GSUB / GPOS, so this test exercises *only* the
//! morx type-5 apply path.
//!
//! # No rustybuzz parity
//!
//! rustybuzz 0.20 has no AAT shaper — it ignores `morx` entirely.
//! This is sigilbuzz-only coverage. Regenerate with
//! `python3 tests/tools/build_aat_morx_insertion_fixture.py`.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

/// Built by `tests/tools/build_aat_morx_insertion_fixture.py`.
const AAT_MORX_INSERTION: &[u8] = include_bytes!("fixtures/aat_morx_insertion.ttf");

const GID_MARK: u32 = 5;
const GID_A: u32 = 1;
const GID_B: u32 = 2;

fn shape_text(text: &str) -> Vec<sigilbuzz::Glyph> {
    let blob = Blob::new(AAT_MORX_INSERTION);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 1000.0);
    let mut buf = Buffer::new();
    buf.push_str(text);
    shape(&font, &buf, &[]).unwrap().glyphs
}

#[test]
fn fixture_has_morx_and_no_gsub() {
    let blob = Blob::new(AAT_MORX_INSERTION);
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
fn type5_inserts_mark_after_trigger() {
    // Single A → A + mark.
    let glyphs = shape_text("A");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].glyph_id, GID_A);
    assert_eq!(glyphs[1].glyph_id, GID_MARK);
}

#[test]
fn type5_leaves_run_alone_without_trigger() {
    // "BCD" — no trigger, no insertion.
    let glyphs = shape_text("BCD");
    assert_eq!(glyphs.len(), 3);
    assert_eq!(glyphs[0].glyph_id, GID_B);
}

#[test]
fn type5_inserts_for_each_trigger() {
    // "ABA" — two A triggers, two mark glyphs inserted (one after
    // each), so the output is "A mark B A mark" = 5 glyphs.
    let glyphs = shape_text("ABA");
    assert_eq!(glyphs.len(), 5);
    assert_eq!(glyphs[0].glyph_id, GID_A);
    assert_eq!(glyphs[1].glyph_id, GID_MARK);
    assert_eq!(glyphs[2].glyph_id, GID_B);
    assert_eq!(glyphs[3].glyph_id, GID_A);
    assert_eq!(glyphs[4].glyph_id, GID_MARK);
}

#[test]
fn type5_inserts_back_to_back_for_double_trigger() {
    // "AA" — two triggers in a row produce A mark A mark.
    let glyphs = shape_text("AA");
    assert_eq!(glyphs.len(), 4);
    assert_eq!(glyphs[0].glyph_id, GID_A);
    assert_eq!(glyphs[1].glyph_id, GID_MARK);
    assert_eq!(glyphs[2].glyph_id, GID_A);
    assert_eq!(glyphs[3].glyph_id, GID_MARK);
}

#[test]
fn type5_handles_empty_input() {
    let glyphs = shape_text("");
    assert!(glyphs.is_empty());
}

#[test]
fn type5_total_advance_includes_inserted_glyph() {
    // "A" → A (500) + mark (200) = 700.
    let glyphs = shape_text("A");
    let total: i32 = glyphs.iter().map(|g| g.x_advance).sum();
    assert_eq!(total, 700);
}
