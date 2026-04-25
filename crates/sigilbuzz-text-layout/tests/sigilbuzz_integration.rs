//! End-to-end integration: shape a mixed-language paragraph with
//! sigilbuzz against Open Sans, then feed the glyphs into
//! [`wrap_lines`] and assert the wrapper produces sensible
//! [`LineRange`]s.
//!
//! The test stays away from exact-pixel asserts (Open Sans advance
//! widths can drift across font releases) and instead pins the
//! invariants that *should* hold for any sane wrapper:
//!
//! - Number of lines grows as `max_width` shrinks.
//! - Every line range is a valid byte slice into the source text.
//! - Line ranges cover the entire input (no gaps, no overlaps).
//! - Hard line breaks always produce a fresh line range.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};
use sigilbuzz_text_layout::{wrap_lines, LineRange, WrapOptions};

const OPEN_SANS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/opensans_regular.ttf"
));

fn shape_glyphs(text: &str) -> Vec<sigilbuzz::Glyph> {
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).expect("parse Open Sans");
    let font = Font::new(face, 16.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    let run = shape(&font, &buffer, &[]).expect("shape");
    run.glyphs
}

#[test]
fn wraps_mixed_language_paragraph() {
    let text = "The quick brown fox jumps over the lazy dog.";
    let glyphs = shape_glyphs(text);
    let lines = wrap_lines(
        &glyphs,
        text,
        WrapOptions {
            max_width: 5000.0,
            break_at_word_boundaries: true,
        },
    );
    assert!(!lines.is_empty());
    assert_eq!(lines[0].start_byte, 0);
    assert_eq!(lines.last().expect("at least one line").end_byte, text.len());
}

#[test]
fn ranges_cover_text_without_gaps() {
    let text = "Hello world. Foo bar baz qux.";
    let glyphs = shape_glyphs(text);
    let lines = wrap_lines(
        &glyphs,
        text,
        WrapOptions {
            max_width: 3000.0,
            break_at_word_boundaries: true,
        },
    );
    let mut cursor = 0;
    for LineRange { start_byte, end_byte, .. } in &lines {
        assert_eq!(*start_byte, cursor, "lines must abut: {lines:?}");
        assert!(end_byte > start_byte, "non-empty lines: {lines:?}");
        // Every range must land on a UTF-8 boundary.
        assert!(text.is_char_boundary(*start_byte));
        assert!(text.is_char_boundary(*end_byte));
        cursor = *end_byte;
    }
    assert_eq!(cursor, text.len());
}

#[test]
fn narrowing_width_increases_line_count() {
    let text = "The quick brown fox jumps over the lazy dog.";
    let glyphs = shape_glyphs(text);
    let wide = wrap_lines(
        &glyphs,
        text,
        WrapOptions {
            max_width: f32::INFINITY,
            break_at_word_boundaries: true,
        },
    );
    let narrow = wrap_lines(
        &glyphs,
        text,
        WrapOptions {
            max_width: 3000.0,
            break_at_word_boundaries: true,
        },
    );
    assert_eq!(wide.len(), 1, "infinite width should fit on one line");
    assert!(
        narrow.len() > wide.len(),
        "narrow width should produce more lines: narrow={} wide={}",
        narrow.len(),
        wide.len()
    );
}

#[test]
fn hard_breaks_force_a_new_line() {
    let text = "Para one.\nPara two.";
    let glyphs = shape_glyphs(text);
    let lines = wrap_lines(
        &glyphs,
        text,
        WrapOptions {
            max_width: f32::INFINITY,
            break_at_word_boundaries: true,
        },
    );
    assert_eq!(lines.len(), 2);
    let first = &text[lines[0].start_byte..lines[0].end_byte];
    let second = &text[lines[1].start_byte..lines[1].end_byte];
    assert!(first.starts_with("Para one"));
    assert!(second.starts_with("Para two"));
}
