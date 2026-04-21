//! Parity contract: sigilbuzz matches rustybuzz glyph-for-glyph and
//! advance-for-advance on a fixed Latin corpus under the feature set
//! sigilbuzz currently implements (see [`disabled_features`]).
//!
//! As milestones add parsers, the disabled-features list shrinks
//! and eventually disappears — at which point sigilbuzz is on the
//! shape()-level par with rustybuzz for the supported scripts.
//!
//! If a test here fails, sigilbuzz has drifted or rustybuzz has
//! changed output for the same feature set. Pin rustybuzz exactly
//! in Cargo.toml to make the latter unambiguous.

use rustybuzz::ttf_parser::Tag;
use rustybuzz::Feature;
use sigilbuzz::{shape, Blob, Buffer, Face, Feature as SigilFeature, Font};

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
    // Strings that exercise common ligatures. Open Sans carries
    // `fi`/`fl` in its `liga` feature, so these should collapse to
    // single glyphs in both engines.
    "office",
    "sufficient",
    "flight",
    "definite",
    // A ligature right next to a kerning pair so both passes must
    // run in the correct order.
    "flyover",
];

/// Features to disable on rustybuzz so its output reflects the
/// surface sigilbuzz currently implements.
///
/// As of M2, sigilbuzz applies:
/// - GSUB ligature substitution (`liga`, lookup type 4)
/// - GPOS pair adjustment (`kern`, lookup type 2) with Extension
///   wrapper support
/// - Legacy `kern` table as a GPOS-less fallback
///
/// Contextual and contextual-ligature variants (`clig`, `calt`)
/// belong to GSUB lookup types sigilbuzz has not implemented yet;
/// they rejoin the default set as those parsers come online.
fn disabled_features() -> [Feature; 2] {
    [
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

#[test]
fn fi_ligature_collapses_two_chars_into_one_glyph() {
    // Proof-of-fire for GSUB liga: "fi" must shape to a single
    // glyph when the font ships the ligature, which Open Sans does.
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let mut buffer = Buffer::new();
    buffer.push_str("fi");
    let shaped = shape(&font, &buffer, &[]).expect("shape fi");
    assert_eq!(shaped.len(), 1, "expected `fi` to collapse to one glyph");

    // Disabling liga should give us two glyphs again.
    let mut buffer = Buffer::new();
    buffer.push_str("fi");
    let disabled = [SigilFeature {
        tag: *b"liga",
        value: 0,
    }];
    let shaped = shape(&font, &buffer, &disabled).expect("shape fi no-liga");
    assert_eq!(
        shaped.len(),
        2,
        "with liga disabled `fi` must stay as two glyphs"
    );
}
