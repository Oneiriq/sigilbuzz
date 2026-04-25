//! AAT `kerx` format-1 state-machine kerning — end-to-end shape
//! pass.
//!
//! Fixture: `tests/fixtures/aat_kerx_state.ttf` (~1 KB) carries six
//! glyphs (.notdef, A, B, C, D, space) plus a `kerx` v2 format-1
//! subtable with two states. The state machine kerns:
//!
//! - (A, B) → -40 when the A is at run start or follows a non-space
//!   glyph. After a space the kern is suppressed.
//! - (C, D) → -26 (the AAT value list reserves bit 0 so deltas are
//!   even-only — see the fixture script for the rounding from -25).
//!
//! The font has no GSUB / GPOS / legacy `kern`, so this test
//! exercises *only* the kerx format-1 apply path.
//!
//! # No rustybuzz parity
//!
//! rustybuzz 0.20 has no AAT shaper — it ignores `kerx` entirely.
//! This is sigilbuzz-only coverage. Regenerate with
//! `python3 tests/tools/build_aat_kerx_state_fixture.py`.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

/// Built by `tests/tools/build_aat_kerx_state_fixture.py`.
const AAT_KERX_STATE: &[u8] = include_bytes!("fixtures/aat_kerx_state.ttf");

fn shape_text(text: &str) -> Vec<sigilbuzz::Glyph> {
    let blob = Blob::new(AAT_KERX_STATE);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 1000.0);
    let mut buf = Buffer::new();
    buf.push_str(text);
    shape(&font, &buf, &[]).unwrap().glyphs
}

#[test]
fn fixture_has_kerx_and_no_pair_kerning_paths() {
    let blob = Blob::new(AAT_KERX_STATE);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(
        face.kerx().unwrap().is_some(),
        "fixture must carry kerx"
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
fn state_machine_kerns_ab_at_run_start() {
    // "AB" — A at run start triggers state 0's PUSH; the B then
    // pops the A and emits the -40 kern. The whole delta lands on
    // the popped glyph (the A) since format-1 is direct-apply, not
    // half-split.
    let glyphs = shape_text("AB");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].x_advance, 500 - 40, "A advance shrinks by 40");
    assert_eq!(glyphs[1].x_advance, 500, "B advance untouched");
}

#[test]
fn state_machine_kerns_cd_pair() {
    // "CD" — C pushes; D pops and emits -26.
    let glyphs = shape_text("CD");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].x_advance, 500 - 26, "C advance shrinks by 26");
    assert_eq!(glyphs[1].x_advance, 500, "D advance untouched");
}

#[test]
fn state_machine_suppresses_ab_after_space() {
    // " AB" — the leading space sends the machine to state 1; A in
    // state 1 takes the no-push path so when B arrives the kern
    // stack is empty and no kern lands.
    let glyphs = shape_text(" AB");
    assert_eq!(glyphs.len(), 3);
    assert_eq!(glyphs[0].x_advance, 250, "space untouched");
    assert_eq!(glyphs[1].x_advance, 500, "A untouched after space");
    assert_eq!(glyphs[2].x_advance, 500, "B untouched after space");
}

#[test]
fn state_machine_kerns_ab_after_letter() {
    // "DAB" — D doesn't change state; A at the second slot still
    // pushes; B then kerns.
    let glyphs = shape_text("DAB");
    assert_eq!(glyphs.len(), 3);
    assert_eq!(glyphs[0].x_advance, 500, "D untouched (no D push at this slot)");
    assert_eq!(glyphs[1].x_advance, 500 - 40, "A kerns -40 when reached after D");
    assert_eq!(glyphs[2].x_advance, 500, "B advance untouched");
}

#[test]
fn state_machine_handles_multi_pair_run() {
    // "ABCD" — both AB and CD pairs should kern.
    let glyphs = shape_text("ABCD");
    assert_eq!(glyphs.len(), 4);
    assert_eq!(glyphs[0].x_advance, 500 - 40, "A kerned by AB rule");
    assert_eq!(glyphs[1].x_advance, 500, "B advance untouched");
    assert_eq!(glyphs[2].x_advance, 500 - 26, "C kerned by CD rule");
    assert_eq!(glyphs[3].x_advance, 500, "D advance untouched");
    let total: i32 = glyphs.iter().map(|g| g.x_advance).sum();
    assert_eq!(total, 4 * 500 - 40 - 26, "total tightens by 66");
}

#[test]
fn state_machine_kerns_repeated_ab_pairs() {
    // "ABAB" — both AB pairs fire because each B's apply leaves the
    // machine in state 0 and the next A re-pushes.
    let glyphs = shape_text("ABAB");
    assert_eq!(glyphs.len(), 4);
    assert_eq!(glyphs[0].x_advance, 500 - 40);
    assert_eq!(glyphs[1].x_advance, 500);
    assert_eq!(glyphs[2].x_advance, 500 - 40);
    assert_eq!(glyphs[3].x_advance, 500);
}

#[test]
fn state_machine_leaves_unrelated_pairs_alone() {
    // "AD" / "BC" don't fire any rule.
    let glyphs = shape_text("AD");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].x_advance, 500);
    assert_eq!(glyphs[1].x_advance, 500);

    let glyphs = shape_text("BC");
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].x_advance, 500);
    assert_eq!(glyphs[1].x_advance, 500);
}
