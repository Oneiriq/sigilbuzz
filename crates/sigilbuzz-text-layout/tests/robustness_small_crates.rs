//! Robustness checks for arbitrary text and arbitrary glyph streams.
//!
//! Every public entry point must return without panicking or stalling
//! on any string, any glyph slice, and any width budget.

use sigilbuzz::Glyph;
use sigilbuzz_text_layout::{
    line_break_opportunities, word_breaks, wrap_lines, LineRange, WrapOptions,
};

fn glyph(cluster: u32, x_advance: i32) -> Glyph {
    Glyph {
        glyph_id: 1,
        cluster,
        x_advance,
        y_advance: 0,
        x_offset: 0,
        y_offset: 0,
        unicode_props: 0,
        indic_position: 0,
    }
}

/// One glyph per char, with `cluster` set to the char's byte offset.
fn uniform_glyphs(text: &str, advance: i32) -> Vec<Glyph> {
    text.char_indices()
        .map(|(b, _)| glyph(b as u32, advance))
        .collect()
}

/// Asserts that the lines tile `text` in order and land on char
/// boundaries.
fn assert_tiles(text: &str, lines: &[LineRange]) {
    let mut expected_start = 0;
    for line in lines {
        assert_eq!(
            line.start_byte, expected_start,
            "gap or overlap in {lines:?}"
        );
        assert!(line.end_byte > line.start_byte, "empty line in {lines:?}");
        assert!(text.is_char_boundary(line.start_byte));
        assert!(text.is_char_boundary(line.end_byte));
        expected_start = line.end_byte;
    }
    assert_eq!(expected_start, text.len());
}

/// Deterministic mixed-script string that covers every class the
/// classifier knows about, plus unpaired CR, lone combining marks,
/// and multi-byte characters next to ASCII.
fn mixed_text(len: usize) -> String {
    const POOL: &[char] = &[
        'a',
        'Z',
        '0',
        ' ',
        '\t',
        '\n',
        '\r',
        '-',
        '(',
        ')',
        '[',
        ']',
        '"',
        '\'',
        ',',
        '.',
        '!',
        '?',
        '$',
        '%',
        '/',
        '\u{00A0}',
        '\u{00AD}',
        '\u{0301}',
        '\u{200B}',
        '\u{2060}',
        '\u{2014}',
        '\u{2026}',
        '\u{3000}',
        '\u{3001}',
        '\u{300C}',
        '\u{300D}',
        '\u{4E16}',
        '\u{AC00}',
        '\u{1F600}',
        '\u{1F3FB}',
        '\u{0085}',
        '\u{2028}',
        '\u{FEFF}',
        '\u{10FFFF}',
    ];
    let mut state: u32 = 0x1234_5678;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            POOL[(state >> 16) as usize % POOL.len()]
        })
        .collect()
}

#[test]
fn line_break_offsets_are_ordered_char_boundaries() {
    for len in [0, 1, 2, 3, 17, 500] {
        let text = mixed_text(len);
        let mut prev = 0;
        for (offset, _) in line_break_opportunities(&text) {
            assert!(offset >= prev, "offsets went backwards in {text:?}");
            assert!(offset <= text.len());
            assert!(text.is_char_boundary(offset));
            prev = offset;
        }
    }
}

#[test]
fn word_break_offsets_are_ordered_char_boundaries() {
    for len in [0, 1, 2, 3, 17, 500] {
        let text = mixed_text(len);
        let mut prev = 0;
        for offset in word_breaks(&text) {
            assert!(offset >= prev, "offsets went backwards in {text:?}");
            assert!(offset <= text.len());
            assert!(text.is_char_boundary(offset));
            prev = offset;
        }
    }
}

#[test]
fn wrap_lines_accepts_any_width_budget() {
    let text = mixed_text(300);
    let glyphs = uniform_glyphs(&text, 10);
    for max_width in [
        0.0,
        -1.0,
        -0.0,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MIN_POSITIVE,
        f32::MAX,
    ] {
        for break_at_word_boundaries in [true, false] {
            let lines = wrap_lines(
                &glyphs,
                &text,
                WrapOptions {
                    max_width,
                    break_at_word_boundaries,
                },
            );
            assert_tiles(&text, &lines);
        }
    }
}

#[test]
fn wrap_lines_ignores_glyphs_that_do_not_match_the_text() {
    // Clusters past the end, clusters inside a multi-byte char,
    // extreme advances, and more glyphs than chars.
    let text = "\u{4E16}a \u{1F600}b";
    let glyphs = [
        glyph(u32::MAX, i32::MAX),
        glyph(1, i32::MIN),
        glyph(2, 7),
        glyph(9, 3),
        glyph(0, i32::MAX),
        glyph(0, i32::MAX),
    ];
    for max_width in [0.0, 5.0, f32::NAN] {
        for break_at_word_boundaries in [true, false] {
            let lines = wrap_lines(
                &glyphs,
                text,
                WrapOptions {
                    max_width,
                    break_at_word_boundaries,
                },
            );
            assert_tiles(text, &lines);
        }
    }
    // No glyphs at all is also fine.
    assert_tiles(text, &wrap_lines(&[], text, WrapOptions::default()));
}

#[test]
fn wrap_lines_long_tab_run_finishes() {
    // Every tab is a break opportunity and every tab is trimmed as
    // trailing whitespace. Trimming used to walk back over the whole
    // run at each opportunity, which made this input quadratic.
    let text = "\t".repeat(200_000);
    let glyphs = uniform_glyphs(&text, 10);
    for break_at_word_boundaries in [true, false] {
        let lines = wrap_lines(
            &glyphs,
            &text,
            WrapOptions {
                max_width: 50.0,
                break_at_word_boundaries,
            },
        );
        assert_tiles(&text, &lines);
        assert!(lines.iter().all(|line| line.width.is_finite()));
    }
}

#[test]
fn wrap_lines_long_space_run_before_word_finishes() {
    // Spaces and tabs interleaved, then a word. Mixes LB7 and LB18.
    let mut text = " \t".repeat(100_000);
    text.push_str("word");
    let glyphs = uniform_glyphs(&text, 1);
    let lines = wrap_lines(
        &glyphs,
        &text,
        WrapOptions {
            max_width: 3.0,
            break_at_word_boundaries: true,
        },
    );
    assert_tiles(&text, &lines);
}
