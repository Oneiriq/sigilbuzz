//! Unicode conformance tests for line breaking (UAX #14) and word
//! segmentation (UAX #29).
//!
//! The test files are not committed. Point `SIGILBUZZ_UCD_TEST_DIR` at
//! a directory holding `LineBreakTest.txt` (from
//! `https://www.unicode.org/Public/<version>/ucd/auxiliary/`) and
//! `WordBreakTest.txt` (same directory), then run:
//!
//! ```text
//! SIGILBUZZ_UCD_TEST_DIR=<dir> cargo test -p sigilbuzz-text-layout \
//!     --test conformance -- --ignored --nocapture
//! ```
//!
//! Each test line lists code points separated by `÷` (break) and `×`
//! (no break). The expected break set is the byte offset of every `÷`
//! after the first character, which includes the end of the text and
//! excludes the start, the way both iterators report boundaries.
//!
//! The non-ignored test at the bottom checks the line parser.

use std::collections::BTreeSet;
use std::path::PathBuf;

use sigilbuzz_text_layout::{line_break_opportunities, word_breaks};

/// One parsed test line: the text and the expected break offsets.
struct Case {
    line: usize,
    text: String,
    expected: BTreeSet<usize>,
}

/// Parses one test line. Returns `None` for blank and comment lines.
fn parse_line(raw: &str) -> Option<(String, BTreeSet<usize>)> {
    let data = raw.split('#').next().unwrap_or("").trim();
    if data.is_empty() {
        return None;
    }
    let mut text = String::new();
    let mut expected = BTreeSet::new();
    for token in data.split_whitespace() {
        match token {
            "\u{F7}" => {
                if !text.is_empty() {
                    expected.insert(text.len());
                }
            }
            "\u{D7}" => {}
            hex => {
                let cp = u32::from_str_radix(hex, 16)
                    .unwrap_or_else(|e| panic!("bad code point {hex:?}: {e}"));
                let ch = char::from_u32(cp).unwrap_or_else(|| panic!("not a char: {hex}"));
                text.push(ch);
            }
        }
    }
    Some((text, expected))
}

fn load(file: &str) -> Vec<Case> {
    let dir = std::env::var("SIGILBUZZ_UCD_TEST_DIR")
        .expect("set SIGILBUZZ_UCD_TEST_DIR to a directory holding the UCD test files");
    let path = PathBuf::from(dir).join(file);
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    raw.lines()
        .enumerate()
        .filter_map(|(i, line)| {
            parse_line(line).map(|(text, expected)| Case {
                line: i + 1,
                text,
                expected,
            })
        })
        .collect()
}

fn line_breaks(text: &str) -> BTreeSet<usize> {
    line_break_opportunities(text)
        .map(|(offset, _)| offset)
        .collect()
}

fn word_boundaries(text: &str) -> BTreeSet<usize> {
    word_breaks(text).collect()
}

/// Runs every case through `actual`, prints the pass count and the
/// first 20 failures, and returns the number of failures.
fn run(name: &str, cases: &[Case], actual: fn(&str) -> BTreeSet<usize>) -> usize {
    let mut failures = Vec::new();
    for case in cases {
        let got = actual(&case.text);
        if got != case.expected {
            failures.push((case, got));
        }
    }
    let passed = cases.len() - failures.len();
    println!("{name}: {passed}/{} cases pass", cases.len());
    for (case, got) in failures.iter().take(20) {
        let cps: Vec<String> = case
            .text
            .chars()
            .map(|c| format!("{:04X}", c as u32))
            .collect();
        println!(
            "  line {}: [{}] expected {:?}, got {:?}",
            case.line,
            cps.join(" "),
            case.expected,
            got
        );
    }
    failures.len()
}

#[test]
#[ignore = "reads LineBreakTest.txt from SIGILBUZZ_UCD_TEST_DIR"]
fn line_break_test_txt() {
    let cases = load("LineBreakTest.txt");
    assert!(!cases.is_empty(), "LineBreakTest.txt has no cases");
    let failures = run("LineBreakTest.txt", &cases, line_breaks);
    assert_eq!(failures, 0, "{failures} LineBreakTest.txt cases fail");
}

#[test]
#[ignore = "reads WordBreakTest.txt from SIGILBUZZ_UCD_TEST_DIR"]
fn word_break_test_txt() {
    let cases = load("WordBreakTest.txt");
    assert!(!cases.is_empty(), "WordBreakTest.txt has no cases");
    let failures = run("WordBreakTest.txt", &cases, word_boundaries);
    assert_eq!(failures, 0, "{failures} WordBreakTest.txt cases fail");
}

#[test]
fn parser_reads_breaks_and_code_points() {
    let (text, expected) =
        parse_line("\u{D7} 0041 \u{D7} 0020 \u{F7} AC00 \u{F7}\t# comment").expect("a test line");
    assert_eq!(text, "A \u{AC00}");
    assert_eq!(expected, BTreeSet::from([2, 5]));
    assert!(parse_line("# only a comment").is_none());
    assert!(parse_line("").is_none());
}
