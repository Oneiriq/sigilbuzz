//! Level runs, lookups, and per-line ordering of [`BidiParagraph`].
//! Shaping is covered by `tests/bidi_paragraph.rs`, against real fonts.

use alloc::vec;
use alloc::vec::Vec;

use super::{BidiParagraph, BidiParagraphSpan, BidiRun};
use crate::buffer::Direction;

fn run(range: core::ops::Range<usize>, level: u8) -> BidiRun {
    BidiRun { range, level }
}

fn span(range: core::ops::Range<usize>, level: u8) -> BidiParagraphSpan {
    BidiParagraphSpan { range, level }
}

#[test]
fn empty_text_has_no_runs() {
    let paragraph = BidiParagraph::new("", None);
    assert!(paragraph.runs().is_empty());
    assert_eq!(paragraph.direction(), Direction::Ltr);
    assert_eq!(paragraph.level_at(0), None);
    assert_eq!(paragraph.run_at(0), None);
    assert!(paragraph.visual_runs().is_empty());
    assert!(paragraph.line_runs(0..0).is_empty());
}

#[test]
fn pure_ltr_is_one_level_zero_run() {
    let paragraph = BidiParagraph::new("hello world", None);
    assert_eq!(paragraph.runs(), [run(0..11, 0)]);
    assert_eq!(paragraph.visual_runs(), [run(0..11, 0)]);
    assert_eq!(paragraph.base_level(), 0);
}

#[test]
fn pure_rtl_is_one_level_one_run() {
    let text = "\u{05E9}\u{05DC}\u{05D5}\u{05DD}";
    let paragraph = BidiParagraph::new(text, None);
    assert_eq!(paragraph.direction(), Direction::Rtl);
    assert_eq!(paragraph.base_level(), 1);
    assert_eq!(paragraph.runs(), [run(0..8, 1)]);
}

#[test]
fn runs_cover_the_text_and_never_repeat_a_level() {
    let text = "Hello \u{05E2}\u{05D1}\u{05E8}\u{05D9}\u{05EA} 123 \u{0645}\u{0631}\u{062D}\u{0628}\u{0627}!";
    let paragraph = BidiParagraph::new(text, None);
    let runs = paragraph.runs();
    assert_eq!(runs.first().map(|r| r.range.start), Some(0));
    assert_eq!(runs.last().map(|r| r.range.end), Some(text.len()));
    for pair in runs.windows(2) {
        assert_eq!(pair[0].range.end, pair[1].range.start);
        assert_ne!(pair[0].level, pair[1].level);
    }
    for r in runs {
        assert!(text.is_char_boundary(r.range.start));
        assert!(text.is_char_boundary(r.range.end));
        for byte in r.range.clone() {
            assert_eq!(paragraph.level_at(byte), Some(r.level));
            assert_eq!(paragraph.run_at(byte), Some(r));
        }
    }
}

#[test]
fn forced_direction_overrides_first_strong() {
    let paragraph = BidiParagraph::new("abc", Some(Direction::Rtl));
    assert_eq!(paragraph.direction(), Direction::Rtl);
    assert_eq!(paragraph.runs(), [run(0..3, 2)]);
    // Vertical directions count as left to right.
    let paragraph = BidiParagraph::new("\u{05D0}", Some(Direction::Ttb));
    assert_eq!(paragraph.direction(), Direction::Ltr);
    assert_eq!(paragraph.runs(), [run(0..2, 1)]);
}

#[test]
fn zwnj_does_not_split_a_persian_word() {
    // "abc " + mi ZWNJ khaham: the ZWNJ stays in the RTL run.
    let text = "abc \u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}";
    let paragraph = BidiParagraph::new(text, None);
    assert_eq!(paragraph.runs(), [run(0..4, 0), run(4..text.len(), 1)]);
}

#[test]
fn isolates_and_embeddings_nest_levels() {
    // LTR paragraph: "a", RLI, Hebrew alef, space, "b", PDI, "c".
    // The isolate is level 1 inside, "b" inside it level 2; the
    // isolate controls themselves sit at the paragraph level.
    let text = "a\u{2067}\u{05D0} b\u{2069}c";
    let paragraph = BidiParagraph::new(text, None);
    let levels: Vec<u8> = text
        .char_indices()
        .map(|(i, _)| paragraph.level_at(i).unwrap_or(u8::MAX))
        .collect();
    assert_eq!(levels, [0, 0, 1, 1, 2, 0, 0]);
    // Visual: a, RLI, then the isolate reversed (b before the space
    // and alef), then PDI and c.
    assert_eq!(
        paragraph.visual_runs(),
        [run(0..4, 0), run(7..8, 2), run(4..7, 1), run(8..12, 0)]
    );
}

#[test]
fn line_runs_reset_trailing_whitespace_per_line() {
    // RTL paragraph: Hebrew, space, "abc", space, "def", Hebrew.
    // "abc def" is one level-2 run; broken after "abc ", the first
    // line's trailing space drops to the paragraph level 1.
    let text = "\u{05D0} abc def \u{05D1}";
    let paragraph = BidiParagraph::new(text, None);
    assert_eq!(
        paragraph.runs(),
        [run(0..3, 1), run(3..10, 2), run(10..13, 1)]
    );
    // Visual order of the first line, right to left in logical terms:
    // the alef run at the right, "abc" left of it, and the trailing
    // space (now level 1, apart from the alef run because "abc" sits
    // between them) at the line's left end.
    let first = paragraph.line_runs(0..7);
    assert_eq!(first, [run(6..7, 1), run(3..6, 2), run(0..3, 1)]);
    let second = paragraph.line_runs(7..13);
    assert_eq!(second, [run(10..13, 1), run(7..10, 2)]);
}

#[test]
fn line_runs_keep_inner_whitespace() {
    // A space inside the line keeps its resolved level.
    let text = "\u{05D0} \u{05D1}";
    let paragraph = BidiParagraph::new(text, Some(Direction::Ltr));
    assert_eq!(paragraph.line_runs(0..text.len()), [run(0..5, 1)]);
    // Cut right after the space: the space trails the line and takes
    // the paragraph level 0, so it sits right of the alef.
    assert_eq!(paragraph.line_runs(0..3), [run(0..2, 1), run(2..3, 0)]);
}

#[test]
fn line_of_only_whitespace_is_one_base_level_run() {
    let text = "\u{05D0}   \u{05D1}";
    let paragraph = BidiParagraph::new(text, Some(Direction::Ltr));
    assert_eq!(paragraph.line_runs(2..5), [run(2..5, 0)]);
    assert!(paragraph.line_runs(2..2).is_empty());
}

#[test]
fn reorder_visual_handles_edge_cases() {
    assert!(BidiParagraph::reorder_visual(&[]).is_empty());
    assert_eq!(BidiParagraph::reorder_visual(&[0]), [0]);
    assert_eq!(BidiParagraph::reorder_visual(&[1, 1, 1]), [2, 1, 0]);
    assert_eq!(BidiParagraph::reorder_visual(&[1, 2, 2, 1]), [3, 1, 2, 0]);
    assert_eq!(BidiParagraph::reorder_visual(&[2, 2, 0]), [0, 1, 2]);
    assert_eq!(
        BidiParagraph::reorder_visual(&[0, 1, 2, 3, 2, 1, 0]),
        vec![0, 5, 2, 3, 4, 1, 6]
    );
}

#[test]
#[should_panic(expected = "not a range of character boundaries")]
fn line_runs_reject_a_range_inside_a_character() {
    let paragraph = BidiParagraph::new("\u{05D0}\u{05D1}", None);
    let _ = paragraph.line_runs(1..4);
}

#[test]
#[should_panic(expected = "not a range of character boundaries")]
fn line_runs_reject_a_range_past_the_end() {
    let paragraph = BidiParagraph::new("abc", None);
    let _ = paragraph.line_runs(0..4);
}

/// The level of every character of `text`, in order.
fn char_levels(paragraph: &BidiParagraph) -> Vec<u8> {
    paragraph
        .text()
        .char_indices()
        .map(|(i, _)| paragraph.level_at(i).unwrap_or(u8::MAX))
        .collect()
}

#[test]
fn a_newline_starts_a_paragraph_with_its_own_direction() {
    // A Hebrew paragraph, then a Latin one ending in a Hebrew letter.
    let text = "\u{05D0}\u{05D1} ab\nab \u{05D0}";
    let paragraph = BidiParagraph::new(text, None);
    assert_eq!(char_levels(&paragraph), [1, 1, 1, 2, 2, 1, 0, 0, 0, 1]);
    assert_eq!(paragraph.direction(), Direction::Rtl);
    assert_eq!(
        paragraph.runs(),
        [
            run(0..5, 1),
            run(5..7, 2),
            run(7..8, 1),
            run(8..11, 0),
            run(11..13, 1)
        ]
    );
}

#[test]
fn every_paragraph_separator_splits_the_text() {
    // LF, CR, the information separators, NEL, PARAGRAPH SEPARATOR.
    for sep in [
        '\n', '\r', '\u{1C}', '\u{1D}', '\u{1E}', '\u{85}', '\u{2029}',
    ] {
        let text = alloc::format!("\u{05D0}{sep}a");
        let paragraph = BidiParagraph::new(&text, None);
        let split = 2 + sep.len_utf8();
        assert_eq!(
            paragraph.paragraphs(),
            [span(0..split, 1), span(split..split + 1, 0)],
            "{sep:?}"
        );
        // The separator takes its paragraph's level.
        assert_eq!(char_levels(&paragraph), [1, 1, 0], "{sep:?}");
    }
    // Other controls and line separators do not split.
    for other in ['\t', '\u{0B}', '\u{1F}', '\u{2028}'] {
        let text = alloc::format!("\u{05D0}{other}a");
        assert_eq!(BidiParagraph::new(&text, None).paragraphs().len(), 1);
    }
}

#[test]
fn cr_lf_is_one_separator() {
    let paragraph = BidiParagraph::new("\u{05D0}\r\na", None);
    assert_eq!(paragraph.paragraphs(), [span(0..4, 1), span(4..5, 0)]);
    assert_eq!(char_levels(&paragraph), [1, 1, 1, 0]);
    // LF then CR is two separators. The CR alone is a paragraph with
    // no strong character, so left to right.
    let paragraph = BidiParagraph::new("\u{05D0}\n\ra", None);
    assert_eq!(
        paragraph.paragraphs(),
        [span(0..3, 1), span(3..4, 0), span(4..5, 0)]
    );
    assert_eq!(char_levels(&paragraph), [1, 1, 0, 0]);
}

#[test]
fn a_final_separator_starts_no_paragraph() {
    assert!(BidiParagraph::new("", None).paragraphs().is_empty());
    assert_eq!(
        BidiParagraph::new("abc\n", None).paragraphs(),
        [span(0..4, 0)]
    );
    assert_eq!(BidiParagraph::new("\n", None).paragraphs(), [span(0..1, 0)]);
    assert_eq!(
        BidiParagraph::new("\n\n", Some(Direction::Rtl)).paragraphs(),
        [span(0..1, 1), span(1..2, 1)]
    );
    // Empty text keeps the forced direction.
    assert_eq!(
        BidiParagraph::new("", Some(Direction::Rtl)).direction(),
        Direction::Rtl
    );
}

#[test]
fn a_forced_direction_applies_to_every_paragraph() {
    let paragraph = BidiParagraph::new("abc\n\u{05D0}", Some(Direction::Rtl));
    assert_eq!(paragraph.paragraphs(), [span(0..4, 1), span(4..6, 1)]);
    // The last run of the first paragraph and the only run of the
    // second share level 1 but stay apart.
    assert_eq!(paragraph.runs(), [run(0..3, 2), run(3..4, 1), run(4..6, 1)]);
    let paragraph = BidiParagraph::new("\u{05D0}\n\u{05D1}", Some(Direction::Ltr));
    assert_eq!(paragraph.paragraphs(), [span(0..3, 0), span(3..5, 0)]);
    assert_eq!(char_levels(&paragraph), [1, 0, 1]);
}

#[test]
fn embeddings_and_isolates_end_with_their_paragraph() {
    // Rule X8: an override, an embedding, or an isolate left open at a
    // paragraph separator does not reach the next paragraph.
    for opener in ['\u{202E}', '\u{202B}', '\u{2067}', '\u{2068}'] {
        let text = alloc::format!("{opener}\u{05D0}b\ncd");
        let paragraph = BidiParagraph::new(&text, None);
        let levels = char_levels(&paragraph);
        assert_eq!(levels[levels.len() - 2..], [0, 0], "{opener:?}");
        assert_eq!(paragraph.paragraphs().len(), 2);
    }
}

#[test]
fn runs_and_lines_stop_at_paragraph_boundaries() {
    let paragraph = BidiParagraph::new("ab\ncd", None);
    assert_eq!(paragraph.runs(), [run(0..3, 0), run(3..5, 0)]);
    assert_eq!(paragraph.visual_runs(), [run(0..3, 0), run(3..5, 0)]);
    assert_eq!(paragraph.line_runs(1..4), [run(1..3, 0), run(3..4, 0)]);
    assert_eq!(paragraph.run_at(3), Some(&run(3..5, 0)));
}

#[test]
fn a_line_across_paragraphs_orders_each_part_on_its_own() {
    // Two right-to-left paragraphs, each with a Latin letter.
    let text = "\u{05D0} b\n\u{05D1} c";
    let paragraph = BidiParagraph::new(text, None);
    assert_eq!(
        paragraph.visual_runs(),
        [
            run(4..5, 1),
            run(3..4, 2),
            run(0..3, 1),
            run(8..9, 2),
            run(5..8, 1)
        ]
    );
    // Rule L1 applies at the end of each part: the space trailing the
    // first part drops to that paragraph's level.
    let text = "a \u{05D0} \n\u{05D1}";
    let paragraph = BidiParagraph::new(text, None);
    assert_eq!(char_levels(&paragraph), [0, 0, 1, 0, 0, 1]);
    assert_eq!(
        paragraph.line_runs(0..text.len()),
        [run(0..2, 0), run(2..4, 1), run(4..6, 0), run(6..8, 1)]
    );
}

#[test]
fn paragraph_at_covers_the_text() {
    let text = "ab\r\n\u{05D0}\u{2029}c";
    let paragraph = BidiParagraph::new(text, None);
    let spans = [span(0..4, 0), span(4..9, 1), span(9..10, 0)];
    assert_eq!(paragraph.paragraphs(), spans);
    for offset in 0..text.len() {
        let want = spans.iter().find(|s| s.range.contains(&offset));
        assert_eq!(paragraph.paragraph_at(offset), want, "{offset}");
    }
    assert_eq!(paragraph.paragraph_at(text.len()), None);
    assert_eq!(paragraph.paragraphs()[1].direction(), Direction::Rtl);
}

#[test]
fn many_paragraphs_stay_linear() {
    // A long run of separators is one paragraph each.
    let text = "\n".repeat(50_000);
    let paragraph = BidiParagraph::new(&text, Some(Direction::Rtl));
    assert_eq!(paragraph.paragraphs().len(), 50_000);
    assert_eq!(paragraph.runs().len(), 50_000);
    assert_eq!(paragraph.visual_runs().len(), 50_000);
}
