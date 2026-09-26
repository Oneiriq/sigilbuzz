//! Robustness checks for arbitrary words, arbitrary pattern files,
//! and arbitrary `left_min` / `right_min` thresholds.

use sigilbuzz_hyphen::{hyphenate, Patterns};

/// Asserts every returned offset can be used to split `word`.
fn assert_usable(word: &str, breaks: &[usize]) {
    let mut prev = 0;
    for &offset in breaks {
        assert!(offset > prev, "offsets not strictly increasing: {breaks:?}");
        assert!(
            offset < word.len(),
            "offset {offset} out of range in {word:?}"
        );
        assert!(
            word.is_char_boundary(offset),
            "offset {offset} splits a char in {word:?}"
        );
        prev = offset;
    }
}

#[test]
fn huge_min_thresholds_do_not_overflow() {
    let mut patterns = Patterns::parse("1a1\n").expect("parse");
    for (left_min, right_min) in [
        (usize::MAX, usize::MAX),
        (usize::MAX, 1),
        (1, usize::MAX),
        (usize::MAX / 2 + 1, usize::MAX / 2 + 1),
    ] {
        patterns.left_min = left_min;
        patterns.right_min = right_min;
        assert!(hyphenate("banana", &patterns).is_empty());
    }
}

#[test]
fn zero_min_thresholds_are_accepted() {
    let mut patterns = Patterns::parse("1a1\n").expect("parse");
    patterns.left_min = 0;
    patterns.right_min = 0;
    for word in ["", "a", "aa", "banana", "\u{00E9}a\u{00E9}"] {
        let breaks = hyphenate(word, &patterns);
        assert_usable(word, &breaks);
    }
    assert_eq!(hyphenate("banana", &patterns), vec![1, 2, 3, 4, 5]);
}

#[test]
fn digit_only_pattern_never_splits_a_char() {
    // A pattern with no letters matches at every byte of the wrapped
    // word, including bytes inside a multi-byte char.
    let mut patterns = Patterns::parse("1\n").expect("parse");
    patterns.left_min = 1;
    patterns.right_min = 1;
    for word in [
        "\u{00E9}\u{00E9}\u{00E9}\u{00E9}",
        "a\u{4E16}b\u{1F600}c",
        "stra\u{00DF}e",
    ] {
        let breaks = hyphenate(word, &patterns);
        assert_usable(word, &breaks);
        assert!(!breaks.is_empty(), "expected breaks in {word:?}");
    }
    // ASCII words keep every gap.
    assert_eq!(hyphenate("abcd", &patterns), vec![1, 2, 3]);
}

#[test]
fn odd_pattern_files_parse_or_fail_cleanly() {
    let inputs = [
        "",
        "\n\n\n",
        ".",
        "..",
        "...",
        "9",
        "0",
        "00000",
        "a0b0c0",
        ".9.",
        "12",
        "a.b",
        "caf\u{00E9}",
        "\u{0000}",
        "a b",
        "% only a comment",
        "#",
        "\r\nab1c\r\n",
        "ab1c\nab1c\nab1c",
        "HY3PH",
    ];
    for text in inputs {
        let Ok(mut patterns) = Patterns::parse(text) else {
            continue;
        };
        for (left_min, right_min) in [(0, 0), (1, 1), (2, 3)] {
            patterns.left_min = left_min;
            patterns.right_min = right_min;
            for word in ["", "a", "abc", "hyphenation", "\u{00E9}t\u{00E9}", "a.b"] {
                let breaks = hyphenate(word, &patterns);
                assert_usable(word, &breaks);
            }
        }
    }
}

#[test]
fn long_pattern_and_long_word_finish() {
    let long_pattern = format!("{}\n", "a1".repeat(10_000));
    let mut patterns = Patterns::parse(&long_pattern).expect("parse");
    patterns.left_min = 1;
    patterns.right_min = 1;
    let word = "a".repeat(20_000);
    let breaks = hyphenate(&word, &patterns);
    assert_usable(&word, &breaks);
    assert_eq!(breaks.len(), word.len() - 1);
}

#[cfg(feature = "text-layout-integration")]
#[test]
fn integration_huge_min_thresholds_do_not_overflow() {
    use sigilbuzz_hyphen::{break_opportunities_with_hyphens, HyphenatedBreak};

    let mut patterns = Patterns::parse("1a1\n").expect("parse");
    patterns.left_min = usize::MAX;
    patterns.right_min = usize::MAX;
    let events: Vec<(usize, HyphenatedBreak)> =
        break_opportunities_with_hyphens("banana split", &patterns).collect();
    assert!(events.iter().all(|(_, k)| *k != HyphenatedBreak::Hyphen));
}

#[cfg(feature = "text-layout-integration")]
#[test]
fn integration_offsets_are_char_boundaries() {
    use sigilbuzz_hyphen::break_opportunities_with_hyphens;

    let mut patterns = Patterns::parse("1\n").expect("parse");
    patterns.left_min = 0;
    patterns.right_min = 0;
    let text = "caf\u{00E9} na\u{00EF}ve \u{4E16}\u{754C}ab\r\ncd\u{1F600}";
    let mut prev = 0;
    for (offset, _) in break_opportunities_with_hyphens(text, &patterns) {
        assert!(offset >= prev);
        assert!(
            text.is_char_boundary(offset),
            "offset {offset} splits a char"
        );
        prev = offset;
    }
}
