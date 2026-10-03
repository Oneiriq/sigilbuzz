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
//! Each test line lists code points separated by U+00F7 DIVISION SIGN
//! (a break) and U+00D7 MULTIPLICATION SIGN (no break). The expected
//! break set is the byte offset of every break sign after the first
//! character, which includes the end of the text and excludes the
//! start, the way both iterators report boundaries.
//!
//! The non-ignored tests at the bottom check the line parser and run a
//! few lines copied from the Unicode 17.0.0 files on every
//! `cargo test`: Korean, Japanese, quotation marks, and numbers. They
//! write the break sign as `/` and the no-break sign as `x`.

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

/// Checks test lines written with `/` for the break sign and `x` for
/// the no-break sign against `actual`.
fn check_samples(lines: &[&str], actual: fn(&str) -> BTreeSet<usize>) {
    for line in lines {
        let raw = line.replace('/', "\u{F7}").replace('x', "\u{D7}");
        let (text, expected) = parse_line(&raw).expect("a test line");
        assert_eq!(actual(&text), expected, "{line}");
    }
}

#[test]
fn line_break_samples() {
    check_samples(
        &[
            // LineBreakTest.txt lines 19208, 19230, 19239, and 19277:
            // Korean breaks between syllables (LB31) but not before
            // punctuation, and East Asian brackets.
            "x C5C6 / C5B4 / C694 x 0020 / 006F x 0072 x 0020 / BABB /",
            "x BD24 / C5B4 x 002E x 0020 / 0041 x 002E x 0032 x 0020 / BCFC /",
            "x 3066 / 300C x BD24 / C5B4 x 003F x 300D / 3068 /",
            "x 540D x 0029 / C740 x 0020 / C54C / C544 / C694 x 003F x 300D / 3068 /",
            // Line 19344: numbers with prefixes, postfixes, and signs
            // (LB25).
            "x 0024 x 002D x 0035 x 0020 / 002D x 002E x 0033 x 0020 / 00A3 x 0028 x 0031 x 0032 \
             x 0033 x 002E x 0034 x 0035 x 0036 x 0029 x 0020 / 0031 x 0032 x 0033 x 002E x 20AC \
             x 0020 / 002B x 002E x 0032 x 0035 x 0020 / 0031 x 002F x 0032 /",
            // Lines 19106 and 19161: quotation marks (LB15a, LB15b,
            // LB19).
            "x 0063 x 0061 x 006E x 2019 x 0074 /",
            "x 0061 x 006D x 0062 x 0069 x 0067 x 0075 x 00AB x 0020 / 0028 x 0020 x 0308 x 0020 \
             x 0029 x 0020 / 00BB x 0028 x 0065 x 0308 x 0029 /",
            // Conjoining jamo and a Hangul syllable with a trailing
            // jamo (LB26), and regional indicators (LB30a).
            "x 1112 x 1161 x 11AB / 1100 x 1161 /",
            "x AC00 x 11A8 / AC01 x 11A8 /",
            "x 1F1E6 x 1F1E7 / 1F1E8 /",
        ],
        line_breaks,
    );
}

#[test]
fn word_break_samples() {
    check_samples(
        &[
            // WordBreakTest.txt lines 1515, 1825, 1843, and 1847.
            "/ 0061 x 0027 x 2060 x 0061 / 0027 x 2060 /",
            "/ 0031 x 002E x 2060 x 0031 / 002E x 2060 /",
            "/ 0041 x 005F x 0030 x 005F x 3031 x 005F /",
            "/ 0061 / 1F1E6 x 1F1E7 x 200D / 1F1E8 / 0062 /",
            // The UAX #29 example "나는 Chicago에 산다." keeps each word
            // whole.
            "/ B098 x B294 / 0020 / 0043 x 0068 x 0069 x 0063 x 0061 x 0067 x 006F x C5D0 / 0020 \
             / C0B0 x B2E4 / 002E /",
        ],
        word_boundaries,
    );
}
