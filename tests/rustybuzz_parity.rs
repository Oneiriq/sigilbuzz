//! M1 parity contract: sigilbuzz matches rustybuzz glyph-for-glyph and
//! advance-for-advance on a fixed Latin corpus *when rustybuzz has all
//! OpenType features disabled*.
//!
//! sigilbuzz at M1 does cmap + hmtx only — no GSUB (ligatures,
//! contextual alternates) and no GPOS (kerning, mark attachment).
//! The reference has to match that reduced surface or every Latin
//! corpus entry with a kerning pair would diverge. The feature
//! disable list below shuts off the defaults rustybuzz applies for
//! the Latin script.
//!
//! If a test in this file fails, the M1 contract is broken. Either
//! sigilbuzz has drifted (the common case) or rustybuzz changed its
//! output under the same disabled-feature set (pin rustybuzz to the
//! exact version that authored the contract in Cargo.toml).
//!
//! When sigilbuzz acquires GSUB / GPOS (M2), the `disabled_features`
//! list shrinks and eventually disappears.

use rustybuzz::ttf_parser::Tag;
use rustybuzz::Feature;
use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");

const CORPUS: &[&str] = &[
    "",
    "A",
    "Hello",
    "Hello, world!",
    "The quick brown fox jumps over the lazy dog.",
    "0123456789",
    "héllo",
    "  multiple   spaces  ",
    // A private-use codepoint intermixed with mapped chars exercises
    // the notdef fallback path — if Open Sans maps this PUA
    // character, sigilbuzz and rustybuzz should both emit the same
    // glyph id. If it doesn't, both should emit .notdef.
    "A\u{E000}B",
];

/// Features to disable on rustybuzz so its output reflects the
/// cmap+hmtx-only surface sigilbuzz currently implements.
///
/// `kern` is the critical one (it adjusts advances). `liga`, `clig`,
/// and `calt` are preemptively disabled so ligature substitution
/// cannot surprise the test if the corpus grows.
fn disabled_features() -> [Feature; 4] {
    [
        Feature::new(Tag::from_bytes(b"kern"), 0, ..),
        Feature::new(Tag::from_bytes(b"liga"), 0, ..),
        Feature::new(Tag::from_bytes(b"clig"), 0, ..),
        Feature::new(Tag::from_bytes(b"calt"), 0, ..),
    ]
}

#[test]
fn every_corpus_entry_matches_rustybuzz_glyph_for_glyph() {
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0); // Irrelevant today — advances are in design units.

    let rb_face = rustybuzz::Face::from_slice(OPEN_SANS, 0).expect("parse rustybuzz face");
    let features = disabled_features();

    for &text in CORPUS {
        let mut buffer = Buffer::new();
        buffer.push_str(text);
        let sig = shape(&font, &buffer, &[]).expect("sigilbuzz shape");

        let mut rb_buf = rustybuzz::UnicodeBuffer::new();
        rb_buf.push_str(text);
        let rb_out = rustybuzz::shape(&rb_face, &features, rb_buf);
        let rb_infos = rb_out.glyph_infos();
        let rb_positions = rb_out.glyph_positions();

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
                "glyph id mismatch at position {i} of {text:?}"
            );
            assert_eq!(
                sig_g.x_advance, rb_pos.x_advance,
                "x_advance mismatch at position {i} of {text:?} \
                 (glyph {}): sigilbuzz={} rustybuzz={}",
                sig_g.glyph_id, sig_g.x_advance, rb_pos.x_advance
            );
        }
    }
}
