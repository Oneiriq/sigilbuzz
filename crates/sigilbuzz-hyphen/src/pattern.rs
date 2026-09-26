//! Pattern parsing and storage.
//!
//! A Liang pattern is a fragment like `hy3ph` or `.ach4` where:
//!
//! - Letters are matchee characters.
//! - A leading `.` anchors the pattern to a word start; a trailing
//!   `.` anchors it to a word end.
//! - Single ASCII digits between letters carry the break priority for
//!   that *gap* (0-9, where odd = encourage and even = discourage).
//!
//! Pattern files are newline-separated; lines starting with `%` (TeX
//! comment) or `#` are ignored, as are empty lines.
//!
//! Internally each pattern is split into:
//! - `letters`: the lowercase ASCII letters plus the optional `.`
//!   anchors, used for `starts_with` matching.
//! - `priorities`: one priority per *position* in `letters`, where
//!   the priority at index `i` is the digit (if any) that appeared
//!   *before* `letters[i]`. So `hy3ph` becomes
//!   `letters="hyph"`, `priorities=[0,0,3,0]`.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

/// A compiled hyphenation pattern.
#[derive(Debug, Clone)]
pub(crate) struct Pattern {
    /// Lowercase ASCII letters plus optional leading/trailing `.`
    /// anchors. Matched against the candidate word fragment.
    pub(crate) letters: String,
    /// One priority per character of `letters`. `priorities[i]` is the
    /// strength of the break opportunity *before* `letters[i]`.
    pub(crate) priorities: Vec<u8>,
    /// True if this pattern only applies at a word start (`.` prefix).
    pub(crate) anchored_start: bool,
    /// True if this pattern only applies at a word end (`.` suffix).
    pub(crate) anchored_end: bool,
}

/// A bundle of compiled hyphenation patterns plus the language's
/// recommended `left_min`/`right_min` thresholds.
#[derive(Debug, Clone)]
pub struct Patterns {
    /// Internal pattern table. Implementation-private.
    pub(crate) inner: Vec<Pattern>,
    /// Pattern indexes grouped by first byte, built once by `parse`.
    pub(crate) index: PatternIndex,
    /// Minimum letters before the first break (typically 2-3).
    pub left_min: usize,
    /// Minimum letters after the last break (typically 2-3).
    pub right_min: usize,
}

/// Groups patterns by the first byte of their letters.
///
/// A pattern can only match where its first byte matches, so `hyphenate`
/// checks one small group per position instead of every pattern. Without
/// this, a long run of letters cost word length times the full pattern
/// count (about 4,900 for US English).
#[derive(Debug, Clone)]
pub(crate) struct PatternIndex {
    /// `by_first[b]` holds the indexes of patterns whose letters start
    /// with byte `b`.
    by_first: Vec<Vec<usize>>,
    /// Patterns with no letters. They can match at every position.
    no_letters: Vec<usize>,
}

impl PatternIndex {
    fn build(patterns: &[Pattern]) -> Self {
        let mut by_first = vec![Vec::new(); 256];
        let mut no_letters = Vec::new();
        for (i, pattern) in patterns.iter().enumerate() {
            match pattern.letters.as_bytes().first() {
                Some(&b) => {
                    if let Some(group) = by_first.get_mut(usize::from(b.to_ascii_lowercase())) {
                        group.push(i);
                    }
                }
                None => no_letters.push(i),
            }
        }
        Self {
            by_first,
            no_letters,
        }
    }

    /// Indexes of the patterns that could match at a position whose byte
    /// is `b`.
    pub(crate) fn candidates(&self, b: u8) -> impl Iterator<Item = usize> + '_ {
        let group = self
            .by_first
            .get(usize::from(b.to_ascii_lowercase()))
            .map_or(&[][..], Vec::as_slice);
        self.no_letters.iter().chain(group).copied()
    }
}

/// Errors raised by [`Patterns::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// A pattern contains a non-ASCII character. Liang patterns assume
    /// 7-bit ASCII; UTF-8 patterns require a Unicode-aware variant
    /// that is not yet implemented.
    NonAscii {
        /// 1-based line number of the offending pattern.
        line: usize,
    },
    /// A pattern contains an unexpected character (anything other than
    /// `[a-z0-9.]`).
    InvalidCharacter {
        /// 1-based line number of the offending pattern.
        line: usize,
        /// The offending character.
        ch: char,
    },
    /// Two priority digits appeared back-to-back in a pattern. Liang
    /// patterns allow at most one digit per gap.
    AdjacentDigits {
        /// 1-based line number of the offending pattern.
        line: usize,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonAscii { line } => {
                write!(f, "non-ASCII character on pattern line {line}")
            }
            Self::InvalidCharacter { line, ch } => {
                write!(f, "invalid character {ch:?} in pattern on line {line}")
            }
            Self::AdjacentDigits { line } => {
                write!(f, "adjacent priority digits on pattern line {line}")
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ParseError {}

impl Patterns {
    /// Load custom patterns from a string of newline-separated entries.
    ///
    /// Lines beginning with `%` (TeX comment) or `#` are skipped, as
    /// are blank lines. Each remaining line is one pattern in the
    /// classic Liang form (e.g. `hy3ph`, `.ach4`, `z3o1phr`).
    ///
    /// `left_min` and `right_min` default to 2 and 3, the canonical
    /// values for English; callers may overwrite the fields after
    /// parsing if their language prefers different thresholds.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let mut inner = Vec::new();
        for (idx, raw) in text.lines().enumerate() {
            let line_no = idx + 1;
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('%') || trimmed.starts_with('#') {
                continue;
            }
            inner.push(parse_one(trimmed, line_no)?);
        }
        let index = PatternIndex::build(&inner);
        Ok(Self {
            inner,
            index,
            left_min: 2,
            right_min: 3,
        })
    }

    /// Return the number of compiled patterns.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// True if no patterns are loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

fn parse_one(raw: &str, line_no: usize) -> Result<Pattern, ParseError> {
    if !raw.is_ascii() {
        return Err(ParseError::NonAscii { line: line_no });
    }

    let mut letters = String::with_capacity(raw.len());
    let mut priorities = Vec::with_capacity(raw.len() + 1);
    let mut pending: u8 = 0;
    let mut anchored_start = false;
    let mut anchored_end = false;

    for (i, b) in raw.bytes().enumerate() {
        match b {
            b'.' => {
                if i == 0 {
                    anchored_start = true;
                    letters.push('.');
                    priorities.push(pending);
                    pending = 0;
                } else if i == raw.len() - 1 {
                    anchored_end = true;
                    letters.push('.');
                    priorities.push(pending);
                    pending = 0;
                } else {
                    return Err(ParseError::InvalidCharacter {
                        line: line_no,
                        ch: '.',
                    });
                }
            }
            b'0'..=b'9' => {
                if pending != 0 {
                    return Err(ParseError::AdjacentDigits { line: line_no });
                }
                pending = b - b'0';
            }
            b'a'..=b'z' | b'A'..=b'Z' => {
                letters.push(b.to_ascii_lowercase() as char);
                priorities.push(pending);
                pending = 0;
            }
            _ => {
                return Err(ParseError::InvalidCharacter {
                    line: line_no,
                    ch: b as char,
                });
            }
        }
    }

    // A trailing digit (no character after it) carries a priority for
    // the position *after* the last letter. Push a final priority slot.
    if pending != 0 {
        priorities.push(pending);
    }

    Ok(Pattern {
        letters,
        priorities,
        anchored_start,
        anchored_end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn parses_simple_pattern() {
        let p = parse_one("hy3ph", 1).unwrap();
        assert_eq!(p.letters, "hyph");
        assert_eq!(p.priorities, vec![0, 0, 3, 0]);
        assert!(!p.anchored_start);
        assert!(!p.anchored_end);
    }

    #[test]
    fn parses_anchored_start() {
        let p = parse_one(".ach4", 1).unwrap();
        assert_eq!(p.letters, ".ach");
        assert_eq!(p.priorities, vec![0, 0, 0, 0, 4]);
        assert!(p.anchored_start);
        assert!(!p.anchored_end);
    }

    #[test]
    fn parses_anchored_end() {
        let p = parse_one("z3ian.", 1).unwrap();
        assert_eq!(p.letters, "zian.");
        assert_eq!(p.priorities, vec![0, 3, 0, 0, 0]);
        assert!(!p.anchored_start);
        assert!(p.anchored_end);
    }

    #[test]
    fn parses_leading_digit() {
        let p = parse_one("2tion", 1).unwrap();
        assert_eq!(p.letters, "tion");
        assert_eq!(p.priorities, vec![2, 0, 0, 0]);
    }

    #[test]
    fn rejects_adjacent_digits() {
        match parse_one("ab23cd", 7) {
            Err(ParseError::AdjacentDigits { line: 7 }) => {}
            other => panic!("expected AdjacentDigits on line 7, got {other:?}"),
        }
    }

    #[test]
    fn rejects_non_ascii() {
        match parse_one("café3", 9) {
            Err(ParseError::NonAscii { line: 9 }) => {}
            other => panic!("expected NonAscii on line 9, got {other:?}"),
        }
    }

    #[test]
    fn parse_skips_comments_and_blanks() {
        let text = "\
% comment\n\
\n\
hy3ph\n\
# another comment\n\
.ach4\n\
";
        let parsed = Patterns::parse(text).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed.inner[0].letters, "hyph");
        assert_eq!(parsed.inner[1].letters, ".ach");
        assert_eq!(parsed.left_min, 2);
        assert_eq!(parsed.right_min, 3);
    }
}
