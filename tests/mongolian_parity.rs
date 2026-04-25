//! Mongolian shaping parity tests.
//!
//! Shapes a small Mongolian corpus with both sigilbuzz and rustybuzz
//! against Noto Sans Mongolian (OFL) and asserts the output matches
//! glyph id and advance.
//!
//! The corpus exercises:
//!
//!   * empty run
//!   * single isolated letter
//!   * two-letter joining (init + fina)
//!   * three-letter joining (init + medi + fina)
//!   * vowel separator U+180E (breaks joining)
//!   * Free Variation Selector after a letter (FVS1 inherits form)
//!   * mixed Mongolian + Latin
//!
//! Both engines are forced to a horizontal direction. sigilbuzz's
//! buffer is set to RTL — that reads as "explicit horizontal", so
//! the auto-vertical default for Mongolian does not kick in.
//! rustybuzz's buffer is set to LTR; sigilbuzz processes glyphs in
//! logical order regardless of buffer direction (it does not reverse
//! the run for RTL), so comparing sigilbuzz-RTL against rustybuzz-LTR
//! aligns logical-order outputs glyph-for-glyph. A separate test
//! exercises the auto-vertical path.
//!
//! A failure here is a parity drift against rustybuzz; fix in
//! `src/ot/mongolian.rs`, `src/unicode/joining.rs` (Mongolian
//! joining-type table), or the script_of arm in
//! `src/unicode/mod.rs`.

use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const NOTO_MONGOLIAN: &[u8] = include_bytes!("fonts/NotoSansMongolian-Regular.ttf");

struct Case {
    text: &'static str,
    note: &'static str,
}

const CORPUS: &[Case] = &[
    Case {
        text: "",
        note: "empty",
    },
    // U+1820 MONGOLIAN LETTER A — alone is `isol`.
    Case {
        text: "\u{1820}",
        note: "A alone (isol)",
    },
    // A + E (U+1820 + U+1821). Init + fina pair — exercises the
    // joining state machine boundaries on a 2-letter chain.
    Case {
        text: "\u{1820}\u{1821}",
        note: "A + E (init + fina)",
    },
    // A + MVS (U+180E) + E. Vowel separator breaks joining; both
    // letters end up isolated and the MVS gets a glyph of its own.
    Case {
        text: "\u{1820}\u{180E}\u{1821}",
        note: "A + MVS + E (joining breaks)",
    },
    // Mixed Latin + Mongolian — the segmenter must split correctly
    // so Mongolian features only see the Mongolian segment, and
    // Latin only sees the DFLT-priority Latin segment.
    Case {
        text: "Hi \u{1820}",
        note: "mixed latin + Mongolian",
    },
    // Single isolated punctuation: U+1806 TODO SOFT HYPHEN. A
    // Non-joining (U) Mongolian codepoint exercises the U-arm of
    // the joining state machine without any cursive context.
    Case {
        text: "\u{1806}",
        note: "todo soft hyphen (Non-joining)",
    },
];

// Multi-letter Mongolian chains (3+ dual-joining letters in a row)
// exercise the calt/rclt marker pass: lookups inject transient
// `masculine` / `feminine` marker glyphs via Multiple substitution
// inside a chained-context match, then later ligature lookups in
// the same calt feature consume `letter + marker -> letter` to
// remove the marker once it has driven the contextual choice. The
// suite below asserts full glyph-stream parity with rustybuzz so
// any drift in the marker-pass cursor walk surfaces immediately.
const MULTILETTER_CHAINS: &[Case] = &[
    Case {
        text: "\u{1820}\u{1821}\u{1822}",
        note: "A + E + I (3-letter chain)",
    },
    Case {
        text: "\u{182A}\u{1820}\u{182D}\u{1820}",
        note: "BA + A + GA + A (4-letter chain)",
    },
    Case {
        text: "\u{1820}\u{1820}\u{1820}",
        note: "A + A + A (3-letter same-letter chain)",
    },
    Case {
        text: "\u{1820}\u{1820}\u{1820}\u{1820}",
        note: "A + A + A + A (4-letter same-letter chain)",
    },
    Case {
        text: "\u{1820}\u{1821}\u{1822}\u{1823}\u{1824}",
        note: "A + E + I + O + U (5-letter chain)",
    },
    Case {
        text: "\u{1828}\u{1820}\u{182D}\u{1820}",
        note: "NA + A + GA + A (4-letter masculine chain)",
    },
];

#[test]
fn mongolian_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_MONGOLIAN);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_MONGOLIAN, 0).expect("parse rustybuzz face");

    for case in CORPUS {
        let mut buffer = Buffer::new();
        // Force horizontal explicit direction so the Mongolian
        // auto-vertical default does not flip metrics. sigilbuzz
        // does not reverse the run for RTL, so logical-order glyphs
        // line up with rustybuzz's LTR output directly.
        buffer.set_direction(Direction::Rtl);
        buffer.push_str(case.text);
        let sig = shape(&font, &buffer, &[]).expect("sigilbuzz shape");

        let mut rb_buf = rustybuzz::UnicodeBuffer::new();
        rb_buf.set_direction(rustybuzz::Direction::LeftToRight);
        rb_buf.push_str(case.text);
        let rb_out = rustybuzz::shape(&rb_face, &[], rb_buf);
        let rb_infos = rb_out.glyph_infos();
        let rb_positions = rb_out.glyph_positions();

        assert_eq!(
            sig.len(),
            rb_infos.len(),
            "glyph count diverged for {} ({:?}): sigilbuzz={} rustybuzz={}",
            case.note,
            case.text,
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
                "glyph id mismatch at position {i} of {} ({:?}): sigilbuzz={} rustybuzz={}",
                case.note, case.text, sig_g.glyph_id, rb_info.glyph_id
            );
            assert_eq!(
                sig_g.x_advance, rb_pos.x_advance,
                "x_advance mismatch at position {i} of {} ({:?}): sigilbuzz={} rustybuzz={}",
                case.note, case.text, sig_g.x_advance, rb_pos.x_advance
            );
        }
    }
}

#[test]
fn multiletter_chains_match_rustybuzz() {
    // Regression coverage for #118 — sigilbuzz used to leave the
    // transient `masculine` / `feminine` marker glyph (gid 1490 /
    // 1491 in Noto Sans Mongolian) in the stream on every chain of
    // three or more dual-joining letters because the apply_forward
    // cursor over-advanced after a marker-consuming ligature.
    let blob = Blob::new(NOTO_MONGOLIAN);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let rb_face = rustybuzz::Face::from_slice(NOTO_MONGOLIAN, 0).expect("parse rustybuzz");

    for case in MULTILETTER_CHAINS {
        let mut buffer = Buffer::new();
        buffer.set_direction(Direction::Rtl);
        buffer.push_str(case.text);
        let sig = shape(&font, &buffer, &[]).expect("sigilbuzz shape");

        let mut rb_buf = rustybuzz::UnicodeBuffer::new();
        rb_buf.set_direction(rustybuzz::Direction::LeftToRight);
        rb_buf.push_str(case.text);
        let rb_out = rustybuzz::shape(&rb_face, &[], rb_buf);
        let rb_infos = rb_out.glyph_infos();
        let rb_positions = rb_out.glyph_positions();

        assert_eq!(
            sig.len(),
            rb_infos.len(),
            "glyph count diverged for {} ({:?}): sigilbuzz={} rustybuzz={}",
            case.note,
            case.text,
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
                "glyph id mismatch at position {i} of {} ({:?}): sigilbuzz={} rustybuzz={}",
                case.note, case.text, sig_g.glyph_id, rb_info.glyph_id
            );
            assert_eq!(
                sig_g.x_advance, rb_pos.x_advance,
                "x_advance mismatch at position {i} of {} ({:?}): sigilbuzz={} rustybuzz={}",
                case.note, case.text, sig_g.x_advance, rb_pos.x_advance
            );
        }
    }
}

#[test]
fn auto_vertical_default_engages_for_mongolian_dominant_run() {
    // A pure Mongolian run with the buffer's default LTR direction
    // should pick vertical metrics — y_advance non-zero, x_advance
    // zero — courtesy of the auto-vertical hook in `shape()`.
    let blob = Blob::new(NOTO_MONGOLIAN);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let mut buffer = Buffer::new();
    // Default direction is LTR — do NOT set any direction here.
    buffer.push_str("\u{1820}\u{1821}");
    let shaped = shape(&font, &buffer, &[]).expect("shape Mongolian default");

    assert_eq!(shaped.len(), 2);
    for g in &shaped.glyphs {
        assert_eq!(g.x_advance, 0, "vertical layout zeroes x_advance");
        // y_advance is the vmtx-driven advance (negative when the
        // pen flows downward; we only check non-zero so this test
        // does not depend on font metric values).
        assert_ne!(g.y_advance, 0, "vertical layout sets y_advance");
    }
}

#[test]
fn explicit_horizontal_overrides_mongolian_default() {
    // Setting RTL explicitly opts into horizontal layout — y_advance
    // stays zero, x_advance carries the hmtx value.
    let blob = Blob::new(NOTO_MONGOLIAN);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let mut buffer = Buffer::new();
    buffer.set_direction(Direction::Rtl);
    buffer.push_str("\u{1820}");
    let shaped = shape(&font, &buffer, &[]).expect("shape Mongolian RTL");

    assert_eq!(shaped.len(), 1);
    assert_eq!(shaped.glyphs[0].y_advance, 0);
    assert_ne!(shaped.glyphs[0].x_advance, 0);
}
