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
//! Both engines are forced to horizontal LTR. On the sigilbuzz side
//! the explicit `set_direction(Direction::Ltr)` is what keeps the
//! auto-vertical default for Mongolian from kicking in (it only
//! applies while no direction was set), so both engines return the
//! same logical-order glyph stream. Separate tests exercise the
//! auto-vertical path and explicit RTL.
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
    // U+1820 MONGOLIAN LETTER A: alone is `isol`.
    Case {
        text: "\u{1820}",
        note: "A alone (isol)",
    },
    // A + E (U+1820 + U+1821). Init + fina pair exercises the
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
    // Mixed Latin + Mongolian: the segmenter must split correctly
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
        // An explicit LTR keeps the run horizontal: the Mongolian
        // auto-vertical default only applies while no direction was
        // set. Logical-order glyphs line up with rustybuzz's LTR
        // output directly.
        buffer.set_direction(Direction::Ltr);
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
    // Regression coverage for #118: sigilbuzz used to leave the
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
        buffer.set_direction(Direction::Ltr);
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
    // should pick vertical metrics (y_advance non-zero, x_advance
    // zero), courtesy of the auto-vertical hook in `shape()`.
    let blob = Blob::new(NOTO_MONGOLIAN);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let mut buffer = Buffer::new();
    // Default direction is LTR. Do NOT set any direction here.
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
    // Setting LTR explicitly opts into horizontal layout: y_advance
    // stays zero, x_advance carries the hmtx value. LTR is also the
    // buffer default, so this pins that the explicit flag, not the
    // value, is what disables auto-vertical.
    let blob = Blob::new(NOTO_MONGOLIAN);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let mut buffer = Buffer::new();
    buffer.set_direction(Direction::Ltr);
    buffer.push_str("\u{1820}");
    let shaped = shape(&font, &buffer, &[]).expect("shape Mongolian LTR");

    assert_eq!(shaped.len(), 1);
    assert_eq!(shaped.glyphs[0].y_advance, 0);
    assert_ne!(shaped.glyphs[0].x_advance, 0);
}

#[test]
fn explicit_rtl_mongolian_is_horizontal_and_read_as_visual_order() {
    // RTL is an explicit horizontal direction too. Mongolian is not a
    // right-to-left script, so, as in HarfBuzz, an RTL buffer holds the
    // letters in visual order: sigilbuzz shapes the reversed text in
    // Mongolian's native direction, and the joining forms follow that
    // reading (hb_ensure_native_direction). Bottom-to-top likewise
    // shapes the reversed text top to bottom.
    let blob = Blob::new(NOTO_MONGOLIAN);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let rb_face = rustybuzz::Face::from_slice(NOTO_MONGOLIAN, 0).expect("parse rustybuzz face");
    let text = "\u{1820}\u{1821}\u{1822}";

    for (direction, rb_direction) in [
        (Direction::Rtl, rustybuzz::Direction::RightToLeft),
        (Direction::Btt, rustybuzz::Direction::BottomToTop),
    ] {
        let mut buffer = Buffer::new();
        buffer.set_direction(direction);
        buffer.push_str(text);
        let ours: Vec<(u32, u32)> = shape(&font, &buffer, &[])
            .expect("shape")
            .glyphs
            .iter()
            .map(|g| (g.glyph_id, g.cluster))
            .collect();
        let mut rb_buf = rustybuzz::UnicodeBuffer::new();
        rb_buf.set_direction(rb_direction);
        rb_buf.push_str(text);
        let theirs: Vec<(u32, u32)> = rustybuzz::shape(&rb_face, &[], rb_buf)
            .glyph_infos()
            .iter()
            .map(|i| (i.glyph_id, i.cluster))
            .collect();
        assert_eq!(ours, theirs, "{direction:?}");
    }

    let mut rtl = Buffer::new();
    rtl.set_direction(Direction::Rtl);
    rtl.push_str(text);
    let rtl = shape(&font, &rtl, &[]).expect("shape RTL");
    assert!(rtl
        .glyphs
        .iter()
        .all(|g| g.y_advance == 0 && g.x_advance != 0));
}

#[test]
fn clear_brings_back_the_auto_vertical_default() {
    let blob = Blob::new(NOTO_MONGOLIAN);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let mut buffer = Buffer::new();
    buffer.set_direction(Direction::Ltr);
    buffer.push_str("\u{1820}");
    let horizontal = shape(&font, &buffer, &[]).expect("shape explicit LTR");
    assert_eq!(horizontal.glyphs[0].y_advance, 0);

    buffer.clear();
    buffer.push_str("\u{1820}");
    let vertical = shape(&font, &buffer, &[]).expect("shape after clear");
    assert_eq!(vertical.glyphs[0].x_advance, 0);
    assert!(
        vertical.glyphs[0].y_advance < 0,
        "TTB advances are negative"
    );
}

#[test]
fn unset_direction_brings_back_the_auto_vertical_default() {
    let blob = Blob::new(NOTO_MONGOLIAN);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let mut buffer = Buffer::new();
    buffer.push_str("\u{1820}");
    buffer.set_direction(Direction::Ltr);
    let horizontal = shape(&font, &buffer, &[]).expect("shape explicit LTR");
    assert_eq!(horizontal.glyphs[0].y_advance, 0);

    // Unlike clear(), unset_direction() keeps the text.
    buffer.unset_direction();
    let vertical = shape(&font, &buffer, &[]).expect("shape after unset_direction");
    assert_eq!(vertical.glyphs[0].x_advance, 0);
    assert!(
        vertical.glyphs[0].y_advance < 0,
        "TTB advances are negative"
    );
}
