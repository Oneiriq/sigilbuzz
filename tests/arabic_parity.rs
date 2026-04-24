//! Arabic-shaping parity contract: sigilbuzz matches rustybuzz
//! glyph-for-glyph and advance-for-advance on a fixed Arabic corpus
//! against Amiri Regular.
//!
//! # What this test proves
//!
//! The corpus exercises the complete surface area of sigilbuzz's M4
//! Arabic pass:
//!
//! - **Every slot of the joining state machine** — `isol`, `init`,
//!   `medi`, `fina`, on real fonts. The state machine assigns forms
//!   on codepoints; Amiri's GSUB rewrites glyph ids per form.
//! - **Cursive chains across `R` / `D` boundaries** — alef (R)
//!   resets the chain, lam-alef (LA) is the classic two-letter run.
//! - **Two separate Arabic words in one buffer** — space as a joining
//!   break, both words should shape independently.
//! - **ZWJ / ZWNJ default-ignorable behaviour** — ZWJ forces a join
//!   across a visual gap; ZWNJ breaks one. HarfBuzz renders both
//!   with a zero-advance space glyph; sigilbuzz matches.
//! - **Tatweel (`C`)** — join-causing character propagates joining
//!   through itself without changing shape.
//! - **Arabic + Latin mixed runs** — the Latin half shapes the same
//!   whether or not Arabic is present in the buffer.
//!
//! # What this test does not cover
//!
//! Amiri's `rlig` feature uses ~40 lookups of GSUB type 5 (Contextual
//! Substitution) for its Quranic-grade vocalised shaping and for
//! well-known ligatures like the stand-alone word Allah
//! (alef-lam-lam-heh → single glyph). sigilbuzz does not yet
//! implement type 5 or the chained-context lookup flags that gate
//! those rules, so those specific strings are left out of the parity
//! corpus. The joining pass itself is spec-correct — positional-only
//! shaping matches rustybuzz byte-for-byte on every Amiri input we
//! have tested; divergence begins only when the font's contextual
//! `rlig` rules want to fire.
//!
//! Byte-identical glyph-id and x_advance agreement is the bar.

use rustybuzz::{Direction as RbDirection, Feature};
use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");

/// Pure Arabic strings that must shape identically in both engines.
///
/// - `ا` — lone alef, gets isolated form.
/// - `با` — beh-alef. Covers `init` + `fina` and the R-letter
///   chain-break rule on the next iteration if there was one.
/// - `لا` — lam-alef. Classic two-letter Arabic pair.
/// - `بب` — double beh. Forces `init` and `fina` on a dual-joining
///   pair.
/// - `بسم` — bism. Three dual-joining letters in sequence — hits
///   every form except `isol`.
/// - `مرحبا` — marhaba / hello. Full sweep through init / fina /
///   init / medi / fina, the most-cited Arabic shaping demo.
/// - `العربية` — al-arabiyya / the Arabic. Unvocalised; exercises the
///   alef-lam chain.
/// - `محمد` — Muhammad. Double meem plus medial hah and final dal.
/// - `بب بب` — two separate words. Space breaks the cursive chain.
/// - `مرحبا مرحبا` — hello hello. Space-broken repetition.
/// - `ـ` — lone tatweel. Join-causing (C) with no neighbours.
/// - `بـب` — beh + tatweel + beh. Tatweel bridges a joining chain.
const ARABIC_CORPUS: &[&str] = &[
    "\u{0627}",
    "\u{0628}\u{0627}",
    "\u{0644}\u{0627}",
    "\u{0628}\u{0628}",
    "\u{0628}\u{0633}\u{0645}",
    "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}",
    "\u{0627}\u{0644}\u{0639}\u{0631}\u{0628}\u{064A}\u{0629}",
    "\u{0645}\u{062D}\u{0645}\u{062F}",
    "\u{0628}\u{0628} \u{0628}\u{0628}",
    "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627} \u{0645}\u{0631}\u{062D}\u{0628}\u{0627}",
    "\u{0640}",
    "\u{0628}\u{0640}\u{0628}",
];

/// ZWJ / ZWNJ default-ignorable corpus. Both engines render the
/// format character as a zero-advance space, so the test tolerates
/// that while still verifying the joining state machine propagates
/// correctly through the format character.
const ZWJ_CORPUS: &[&str] = &[
    "\u{0628}\u{200D}",        // beh + ZWJ — beh forced into init
    "\u{200D}\u{0628}",        // ZWJ + beh — beh forced into fina
    "\u{0628}\u{200C}\u{0628}", // beh + ZWNJ + beh — joining broken
];

/// Features to disable on rustybuzz so its output reflects the
/// surface sigilbuzz currently implements. Empty — the Arabic pass
/// turns the same defaults on that rustybuzz does for Amiri.
fn disabled_features() -> [Feature; 0] {
    []
}

/// Shape `text` with both engines and assert byte-identical output.
///
/// Rustybuzz is forced into RTL mode so it runs its Arabic shaper
/// deterministically; auto-detection would do the same but being
/// explicit removes a dependency on rustybuzz's internal heuristics.
/// Rustybuzz emits visual order for RTL; sigilbuzz emits logical
/// order. The test reverses the rustybuzz side before zipping.
fn assert_parity_on(text: &str) {
    let blob = Blob::new(AMIRI);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(AMIRI, 0).expect("parse rustybuzz face");
    let features = disabled_features();

    let mut buffer = Buffer::new();
    buffer.push_str(text);
    let sig = shape(&font, &buffer, &[]).expect("sigilbuzz shape");

    let mut rb_buf = rustybuzz::UnicodeBuffer::new();
    rb_buf.push_str(text);
    rb_buf.set_direction(RbDirection::RightToLeft);
    let rb_out = rustybuzz::shape(&rb_face, &features, rb_buf);
    let rb_infos: Vec<_> = rb_out.glyph_infos().iter().rev().copied().collect();
    let rb_positions: Vec<_> = rb_out.glyph_positions().iter().rev().copied().collect();

    assert_eq!(
        sig.len(),
        rb_infos.len(),
        "glyph count diverged for input {text:?}: sigilbuzz={} rustybuzz={}",
        sig.len(),
        rb_infos.len()
    );

    for (i, (sig_g, (rb_info, rb_pos))) in sig
        .glyphs
        .iter()
        .zip(rb_infos.iter().zip(rb_positions.iter()))
        .enumerate()
    {
        assert_eq!(
            sig_g.glyph_id, rb_info.glyph_id,
            "glyph id mismatch at position {i} of {text:?}: \
             sigilbuzz={} rustybuzz={}",
            sig_g.glyph_id, rb_info.glyph_id
        );
        assert_eq!(
            sig_g.x_advance, rb_pos.x_advance,
            "x_advance mismatch at position {i} of {text:?} \
             (glyph {}): sigilbuzz={} rustybuzz={}",
            sig_g.glyph_id, sig_g.x_advance, rb_pos.x_advance
        );
    }
}

#[test]
fn pure_arabic_corpus_matches_rustybuzz_glyph_for_glyph() {
    for &text in ARABIC_CORPUS {
        assert_parity_on(text);
    }
}

#[test]
fn zwj_and_zwnj_corpus_matches_rustybuzz() {
    for &text in ZWJ_CORPUS {
        assert_parity_on(text);
    }
}

#[test]
fn marhaba_shapes_to_different_glyphs_than_isolated_letters() {
    // Sanity check independent of rustybuzz: the joining pass must
    // actually rewrite glyph ids. Shape "مرحبا" and verify that at
    // least one glyph id differs from the isolated-form glyph id the
    // cmap returns for the same codepoint. If the positional pass
    // were a no-op the glyph stream would be identical to the cmap
    // output.
    let blob = Blob::new(AMIRI);
    let face = Face::parse(&blob, 0).expect("parse face");
    let cmap = face.cmap().expect("cmap");
    let font = Font::new(face, 1000.0);

    let text = "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}";
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    let shaped = shape(&font, &buffer, &[]).expect("shape");

    let isolated: Vec<u32> = text
        .chars()
        .map(|c| u32::from(cmap.glyph_id(c).unwrap_or(0)))
        .collect();
    let shaped_ids: Vec<u32> = shaped.glyphs.iter().map(|g| g.glyph_id).collect();

    assert_eq!(
        shaped_ids.len(),
        isolated.len(),
        "word should stay at 5 glyphs (no ligatures)"
    );
    assert_ne!(
        shaped_ids, isolated,
        "Arabic joining must rewrite at least one glyph id"
    );
}

#[test]
fn mixed_arabic_and_latin_runs_shape_each_half_correctly() {
    // The Latin portion should produce the same glyphs whether or
    // not Arabic is in the buffer. sigilbuzz does not reorder across
    // a mixed run yet, but the per-character cmap + positional
    // features should leave the Latin glyphs intact.
    let blob = Blob::new(AMIRI);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let latin_only = "hello";
    let mixed = "hello \u{0645}\u{0631}\u{062D}\u{0628}\u{0627}";

    let mut buf = Buffer::new();
    buf.push_str(latin_only);
    let shaped_latin = shape(&font, &buf, &[]).expect("shape latin");

    let mut buf = Buffer::new();
    buf.push_str(mixed);
    let shaped_mixed = shape(&font, &buf, &[]).expect("shape mixed");

    // First 5 glyphs of the mixed run should equal the pure-Latin
    // shape — the Arabic codepoints start at byte offset 6 (after
    // "hello ").
    for (i, (lat, mix)) in shaped_latin
        .glyphs
        .iter()
        .zip(shaped_mixed.glyphs.iter())
        .enumerate()
    {
        assert_eq!(
            lat.glyph_id, mix.glyph_id,
            "Latin glyph {i} diverged in mixed run: pure={} mixed={}",
            lat.glyph_id, mix.glyph_id
        );
        assert_eq!(
            lat.x_advance, mix.x_advance,
            "Latin glyph {i} advance diverged in mixed run"
        );
    }
}
