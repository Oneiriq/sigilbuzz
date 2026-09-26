//! Integration coverage for UAX #9 visual ordering: the character-level
//! [`BidiInfo::reorder`] and the run-level
//! [`BidiParagraph::line_runs`] a layout engine uses per line.
//!
//! Mixed Latin / Hebrew / Arabic text exercises the full chain
//! (X1-X10 + W1-W7 + N0-N2 + I1-I2 + L1 + L2). Expected visual orderings
//! are derived by hand from the spec.

use sigilbuzz::{BidiInfo, BidiParagraph, BidiRun, Buffer, Direction};

/// The characters of `text` in the order `BidiInfo::reorder` gives.
fn visual_chars(text: &str) -> String {
    let info = BidiInfo::new(text, None);
    let chars: Vec<char> = text.chars().collect();
    info.reorder().iter().map(|&i| chars[i]).collect()
}

/// The characters of `line` in the order its runs give: each run in
/// logical order when left to right, reversed when right to left.
fn visual_chars_of_runs(text: &str, runs: &[BidiRun]) -> String {
    runs.iter()
        .flat_map(|run| {
            let chars: Vec<char> = text[run.range.clone()].chars().collect();
            if run.is_rtl() {
                chars.into_iter().rev().collect::<Vec<_>>()
            } else {
                chars
            }
        })
        .collect()
}

#[test]
fn pure_ascii_reorder_is_identity() {
    let info = BidiInfo::new("Hello, world!", None);
    assert_eq!(info.paragraph_direction(), Direction::Ltr);
    assert_eq!(visual_chars("Hello, world!"), "Hello, world!");
}

#[test]
fn latin_with_hebrew_reorders_only_hebrew_span() {
    // "Hello עברית world": Hebrew "עברית" (5 chars) is between two
    // Latin spans. L2 reverses the level-1 span only.
    let text = "Hello \u{05E2}\u{05D1}\u{05E8}\u{05D9}\u{05EA} world";
    assert_eq!(
        visual_chars(text),
        "Hello \u{05EA}\u{05D9}\u{05E8}\u{05D1}\u{05E2} world"
    );
}

#[test]
fn pure_rtl_paragraph_reverses_completely() {
    // Pure Hebrew run ("שלום") gets reversed end-to-end.
    let text = "\u{05E9}\u{05DC}\u{05D5}\u{05DD}";
    let info = BidiInfo::new(text, None);
    assert_eq!(info.paragraph_direction(), Direction::Rtl);
    assert_eq!(visual_chars(text), "\u{05DD}\u{05D5}\u{05DC}\u{05E9}");
}

#[test]
fn rtl_paragraph_with_embedded_latin_keeps_latin_logical_order() {
    // RTL paragraph: Hebrew + space + "abc" + space + Hebrew. The
    // Latin "abc" sits inside a level-2 span; L2 reverses level >= 1
    // around it, but the level-2 span itself is reversed back so the
    // Latin reads left-to-right within the visual run.
    let text = "\u{05D0} abc \u{05D1}";
    assert_eq!(visual_chars(text), "\u{05D1} abc \u{05D0}");
}

#[test]
fn run_order_agrees_with_character_order() {
    // On one line, expanding the visual runs gives the same characters
    // in the same order as reordering the characters themselves.
    let texts = [
        "Hello \u{05E2}\u{05D1}\u{05E8}\u{05D9}\u{05EA} world",
        "\u{05D0} abc 123 def \u{05D1}\u{05D2}",
        "abc \u{0627}\u{0644}\u{0639}\u{0631}\u{0628}\u{064A}\u{0629} \u{0661}\u{0662} xyz",
        "a\u{2067}\u{05D0} b\u{2069}c \u{202E}def\u{202C} (\u{05D3}\u{05D4})",
        "\u{05D0}\u{05D1} (abc [\u{05D2}]) 1-2 \u{05D3}.",
    ];
    for text in texts {
        let paragraph = BidiParagraph::new(text, None);
        assert_eq!(
            visual_chars_of_runs(text, &paragraph.visual_runs()),
            visual_chars(text),
            "{text:?}"
        );
    }
}

#[test]
fn each_line_reorders_its_own_byte_range() {
    // LTR paragraph with a Hebrew phrase that wraps: "abc ALEF BET
    // GIMEL DALET def". On one line the whole phrase reverses; split
    // after "BET ", each line reverses only its own Hebrew, which is
    // what UAX #9 asks (reordering the paragraph first and then cutting
    // it would put DALET on the first line).
    let text = "abc \u{05D0}\u{05D1} \u{05D2}\u{05D3} def";
    let paragraph = BidiParagraph::new(text, None);
    assert_eq!(
        visual_chars_of_runs(text, &paragraph.visual_runs()),
        "abc \u{05D3}\u{05D2} \u{05D1}\u{05D0} def"
    );
    let cut = text.find('\u{05D2}').expect("gimel");
    assert_eq!(
        visual_chars_of_runs(text, &paragraph.line_runs(0..cut)),
        "abc \u{05D1}\u{05D0} "
    );
    assert_eq!(
        visual_chars_of_runs(text, &paragraph.line_runs(cut..text.len())),
        "\u{05D3}\u{05D2} def"
    );
}

#[test]
fn trailing_whitespace_moves_to_the_paragraph_end_of_each_line() {
    // RTL paragraph "ALEF abc def BET" broken after "abc ": the space
    // ending the first line takes the paragraph level (L1), so it is
    // drawn at the left end, not between "abc" and ALEF.
    let text = "\u{05D0} abc def \u{05D1}";
    let paragraph = BidiParagraph::new(text, None);
    let cut = text.find('d').expect("d");
    let first = paragraph.line_runs(0..cut);
    assert_eq!(visual_chars_of_runs(text, &first), " abc \u{05D0}");
    assert_eq!(
        first[0],
        BidiRun {
            range: cut - 1..cut,
            level: 1
        }
    );
    assert_eq!(
        visual_chars_of_runs(text, &paragraph.line_runs(cut..text.len())),
        "\u{05D1} def"
    );
}

#[test]
fn line_ranges_inside_a_nested_embedding() {
    // An RLI isolate holding Hebrew, Latin and a number: levels 0, 1
    // and 2. Cut anywhere, the two lines cover exactly their own bytes
    // with non-empty runs.
    let text = "x \u{2067}\u{05D0} yz 12 \u{05D1}\u{2069} w";
    let paragraph = BidiParagraph::new(text, None);
    let full = visual_chars_of_runs(text, &paragraph.visual_runs());
    assert_eq!(full, visual_chars(text));
    for (cut, _) in text.char_indices().skip(1) {
        let first = paragraph.line_runs(0..cut);
        let second = paragraph.line_runs(cut..text.len());
        // Each line covers exactly its own bytes.
        let mut covered: Vec<usize> = first
            .iter()
            .chain(&second)
            .flat_map(|run| run.range.clone())
            .collect();
        covered.sort_unstable();
        assert_eq!(covered, (0..text.len()).collect::<Vec<_>>(), "cut {cut}");
        for run in first.iter().chain(&second) {
            assert!(!run.range.is_empty());
        }
    }
}

#[test]
fn plain_set_text_keeps_logical_order() {
    // A plain buffer never reorders: bidi goes through BidiParagraph.
    let text = "Hello \u{05E2}\u{05D1}\u{05E8}\u{05D9}\u{05EA}";
    let mut buf = Buffer::new();
    buf.set_text(text);
    assert_eq!(buf.text(), text);
    assert_eq!(buf.direction(), Direction::Ltr);
}
