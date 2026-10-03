//! Robustness checks for arbitrary text and arbitrary glyph streams.
//!
//! Every public entry point must return without panicking or stalling
//! on any string, any glyph slice, and any width budget.

use sigilbuzz::Glyph;
use sigilbuzz_text_layout::{
    line_break_opportunities, line_break_opportunities_with, word_breaks, wrap_lines, LineRange,
    WordBreak, WrapOptions,
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
        char_class: 0,
        combining_class: 0,
        syllable: 0,
        flags: sigilbuzz::GlyphFlags::empty(),
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
        // Hangul jamo (JL, JV, JT) and an LVT syllable (H3).
        '\u{1100}',
        '\u{1161}',
        '\u{11A8}',
        '\u{D55C}',
        // Regional indicator, ZWJ, Hebrew (HL), Thai (SA letter and
        // mark), Balinese aksara and virama (AK, VI), dotted circle.
        '\u{1F1E6}',
        '\u{200D}',
        '\u{05D0}',
        '\u{0E01}',
        '\u{0E31}',
        '\u{1B05}',
        '\u{1B44}',
        '\u{25CC}',
        // BB, IN, CB, CJ, initial and final quotation marks, HH.
        '\u{00B4}',
        '\u{2024}',
        '\u{FFFC}',
        '\u{3041}',
        '\u{201C}',
        '\u{201D}',
        '\u{2010}',
    ];
    let mut state: u32 = 0x1234_5678;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            POOL[(state >> 16) as usize % POOL.len()]
        })
        .collect()
}

const WORD_BREAKS: [WordBreak; 3] = [WordBreak::Normal, WordBreak::KeepAll, WordBreak::BreakAll];

#[test]
fn line_break_offsets_are_ordered_char_boundaries() {
    for len in [0, 1, 2, 3, 17, 500] {
        let text = mixed_text(len);
        for word_break in WORD_BREAKS {
            let mut prev = 0;
            for (offset, _) in line_break_opportunities_with(&text, word_break) {
                assert!(offset > prev, "offsets not increasing in {text:?}");
                assert!(offset <= text.len());
                assert!(text.is_char_boundary(offset));
                prev = offset;
            }
            assert_eq!(prev, text.len(), "no end-of-text break in {text:?}");
        }
        assert!(line_break_opportunities(&text)
            .eq(line_break_opportunities_with(&text, WordBreak::Normal)));
    }
}

#[test]
fn context_and_look_ahead_rules_stay_linear() {
    // Long runs that the space context (LB8, LB14 to LB17), the number
    // state (LB25), the regional indicator parity (LB30a, WB15), and
    // the look-ahead past combining marks (LB15b, LB15c, LB19a, LB25,
    // LB28a, WB6, WB12) walk over.
    let marks = "\u{0301}".repeat(50_000);
    let cases = [
        format!("({}x", " ".repeat(200_000)),
        format!("\u{200B}{}x", " ".repeat(200_000)),
        format!("a\u{201D}{marks}b"),
        format!(" .{marks}1"),
        format!("$({marks}.{marks}1"),
        format!("1{}", ",".repeat(200_000)),
        format!("\u{1B05}{marks}\u{1B05}"),
        format!("a:{marks}b 1.{marks}2"),
        "\u{1F1E6}".repeat(100_000),
        "\u{1100}".repeat(100_000),
        "\u{201C}".repeat(100_000),
        "\u{0E31}".repeat(100_000),
        "\u{200D}".repeat(100_000),
    ];
    for text in &cases {
        for word_break in WORD_BREAKS {
            let last = line_break_opportunities_with(text, word_break).last();
            assert_eq!(last.map(|(offset, _)| offset), Some(text.len()));
        }
        assert_eq!(word_breaks(text).last(), Some(text.len()));
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
            for word_break in WORD_BREAKS {
                let lines = wrap_lines(
                    &glyphs,
                    &text,
                    WrapOptions {
                        max_width,
                        break_at_word_boundaries,
                        word_break,
                    },
                );
                assert_tiles(&text, &lines);
            }
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
                    ..WrapOptions::default()
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
                ..WrapOptions::default()
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
            ..WrapOptions::default()
        },
    );
    assert_tiles(&text, &lines);
}
