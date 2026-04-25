//! Integration with [`sigilbuzz_text_layout`].
//!
//! Gated behind the `text-layout-integration` cargo feature. When
//! enabled, [`break_opportunities_with_hyphens`] walks `text` once,
//! merges UAX 14 line-break opportunities from
//! [`sigilbuzz_text_layout::line_break_opportunities`] with hyphen
//! opportunities discovered by Liang's algorithm, and produces a
//! single sorted iterator over `(byte_offset, HyphenatedBreak)`.
//!
//! The integration introduces a thin local enum, [`HyphenatedBreak`],
//! that wraps [`sigilbuzz_text_layout::BreakOpportunity`] and adds a
//! `Hyphen` variant — kept on this side of the boundary so the layout
//! crate's public API stays focused on UAX 14.

use alloc::vec::Vec;

use sigilbuzz_text_layout::{line_break_opportunities, BreakOpportunity};

use crate::algorithm::hyphenate;
use crate::pattern::Patterns;

/// A break opportunity emitted by
/// [`break_opportunities_with_hyphens`]. Wraps the UAX 14 enum from
/// `sigilbuzz-text-layout` and adds a [`HyphenatedBreak::Hyphen`]
/// variant for soft-hyphen breaks discovered by Liang's algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HyphenatedBreak {
    /// Mandatory UAX 14 break — `CR`, `LF`, `NL`, `BK`.
    Mandatory,
    /// Allowed UAX 14 break — typically a space or other class
    /// boundary.
    Allowed,
    /// Soft-hyphen break discovered by Liang's algorithm. Distinct
    /// from `Allowed` so callers can render a hyphen glyph at the
    /// break point.
    Hyphen,
}

impl From<BreakOpportunity> for HyphenatedBreak {
    fn from(b: BreakOpportunity) -> Self {
        match b {
            BreakOpportunity::Mandatory => Self::Mandatory,
            BreakOpportunity::Allowed => Self::Allowed,
            BreakOpportunity::Prohibited => Self::Allowed,
        }
    }
}

/// Walk `text`, find each ASCII-letter run, run Liang's hyphenation
/// over it, and merge the resulting hyphen opportunities with the UAX
/// 14 break stream from
/// [`sigilbuzz_text_layout::line_break_opportunities`].
///
/// The returned iterator yields `(byte_offset, HyphenatedBreak)` pairs
/// in ascending byte-offset order. Opportunities at the same offset
/// prefer mandatory > allowed > hyphen.
pub fn break_opportunities_with_hyphens(
    text: &str,
    patterns: &Patterns,
) -> impl Iterator<Item = (usize, HyphenatedBreak)> {
    // 1. Collect UAX 14 opportunities.
    let mut events: Vec<(usize, HyphenatedBreak)> = line_break_opportunities(text)
        .map(|(offset, opp)| (offset, HyphenatedBreak::from(opp)))
        .collect();

    // 2. For each contiguous run of ASCII letters, hyphenate it and
    //    convert the byte offsets back into absolute offsets in
    //    `text`. Skip runs whose neighbouring breaks already make the
    //    run trivially small.
    let bytes = text.as_bytes();
    let mut start: Option<usize> = None;
    let mut i = 0;
    while i <= bytes.len() {
        let in_letter = i < bytes.len() && bytes[i].is_ascii_alphabetic();
        match (start, in_letter) {
            (None, true) => start = Some(i),
            (Some(s), false) => {
                hyphenate_run(text, s, i, patterns, &mut events);
                start = None;
            }
            _ => {}
        }
        i += 1;
    }

    // 3. Merge: sort by offset, dedup keeping the strongest variant.
    events.sort_by_key(|(o, _)| *o);
    let mut merged: Vec<(usize, HyphenatedBreak)> = Vec::with_capacity(events.len());
    for (offset, kind) in events {
        if let Some(last) = merged.last_mut() {
            if last.0 == offset {
                last.1 = strongest(last.1, kind);
                continue;
            }
        }
        merged.push((offset, kind));
    }
    merged.into_iter()
}

fn hyphenate_run(
    text: &str,
    start: usize,
    end: usize,
    patterns: &Patterns,
    out: &mut Vec<(usize, HyphenatedBreak)>,
) {
    let word = &text[start..end];
    if word.len() < patterns.left_min + patterns.right_min {
        return;
    }
    for off in hyphenate(word, patterns) {
        out.push((start + off, HyphenatedBreak::Hyphen));
    }
}

fn strongest(a: HyphenatedBreak, b: HyphenatedBreak) -> HyphenatedBreak {
    use HyphenatedBreak::{Allowed, Hyphen, Mandatory};
    match (a, b) {
        (Mandatory, _) | (_, Mandatory) => Mandatory,
        (Allowed, _) | (_, Allowed) => Allowed,
        _ => Hyphen,
    }
}

#[cfg(all(test, feature = "patterns-en-us"))]
mod tests {
    use super::*;
    use crate::bundled::Language;

    #[test]
    fn merges_uax14_and_hyphens() {
        let text = "hyphenation works";
        let patterns = Patterns::for_language(Language::EnglishUs).unwrap();
        let events: Vec<(usize, HyphenatedBreak)> =
            break_opportunities_with_hyphens(text, patterns).collect();

        // UAX 14 must contribute a break around the space (offset
        // somewhere near 11..12). Liang must contribute hyphens at
        // 2 and 6 inside "hyphenation" (the canonical Liang split is
        // "hy-phen-ation" with the bundled `hyph-en-us.tex` patterns).
        assert!(events.iter().any(|(o, k)| *o == 2 && *k == HyphenatedBreak::Hyphen));
        assert!(events.iter().any(|(o, k)| *o == 6 && *k == HyphenatedBreak::Hyphen));
        assert!(events.iter().any(|(_, k)| *k == HyphenatedBreak::Allowed));
    }

    #[test]
    fn mandatory_beats_hyphen_at_same_offset() {
        // Construct a text where a forced break and a hyphen would
        // collide. Newline at end of "hyphenation\n" sits *after* the
        // word so this is more a smoke test that mandatory survives.
        let text = "hyphenation\n";
        let patterns = Patterns::for_language(Language::EnglishUs).unwrap();
        let events: Vec<(usize, HyphenatedBreak)> =
            break_opportunities_with_hyphens(text, patterns).collect();
        assert!(events.iter().any(|(_, k)| *k == HyphenatedBreak::Mandatory));
    }
}
