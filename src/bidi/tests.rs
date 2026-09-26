//! Level runs, lookups, and per-line ordering of [`BidiParagraph`].
//! Shaping is covered by `tests/bidi_paragraph.rs`, against real fonts.

use alloc::vec;
use alloc::vec::Vec;

use super::{BidiParagraph, BidiRun};
use crate::buffer::Direction;

fn run(range: core::ops::Range<usize>, level: u8) -> BidiRun {
    BidiRun { range, level }
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
