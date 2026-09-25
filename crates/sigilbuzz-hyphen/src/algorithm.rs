//! Liang's hyphenation algorithm.
//!
//! Given a word and a [`Patterns`] table:
//!
//! 1. Wrap the word with `.` boundary markers.
//! 2. For every position `i` in the wrapped word and every pattern,
//!    if the pattern matches starting at `i`, write `max(existing,
//!    pattern_priority)` into a per-position priority array.
//! 3. After all patterns are applied, *odd* priorities mark valid
//!    breaks; even (or zero) priorities mean "do not break here".
//! 4. Filter the breaks against the [`left_min`]/[`right_min`]
//!    thresholds.
//!
//! [`left_min`]: super::Patterns::left_min
//! [`right_min`]: super::Patterns::right_min

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::pattern::{Pattern, Patterns};

/// Returns the byte offsets within `word` where soft-hyphen breaks are
/// valid, applying the patterns in `patterns`.
///
/// The returned offsets are *byte indices* into `word`, lie in
/// `1..word.len()`, and always fall on a char boundary. Offsets are
/// filtered against the `left_min` / `right_min` thresholds carried by
/// `patterns`.
///
/// # ASCII only
///
/// Liang's algorithm and the bundled pattern files assume the word
/// consists of 7-bit ASCII letters. ASCII letters match
/// case-insensitively. Characters outside the ASCII letter range are
/// passed through verbatim and never match a pattern letter.
#[must_use]
pub fn hyphenate(word: &str, patterns: &Patterns) -> Vec<usize> {
    if word.len() < patterns.left_min.saturating_add(patterns.right_min) {
        return Vec::new();
    }

    // Build the lower-case wrapped word: ".word.".
    let mut wrapped = String::with_capacity(word.len() + 2);
    wrapped.push('.');
    for ch in word.chars() {
        wrapped.push(ch.to_ascii_lowercase());
    }
    wrapped.push('.');

    let bytes = wrapped.as_bytes();
    // priorities[i] is the strength of the break opportunity *before*
    // position `i` in the wrapped word.
    let mut priorities = vec![0u8; wrapped.len() + 1];
    fill_priorities(bytes, patterns, &mut priorities);

    // Translate priority indices back to byte offsets in the *original*
    // word. The wrapped word is `.word.`, so a priority at wrapped
    // index `k` corresponds to offset `k - 1` in `word`. Valid break
    // offsets are 1..word.len(). A pattern with no letters matches at
    // every byte, including bytes inside a multi-byte char, so offsets
    // that split a char are dropped.
    let mut breaks = Vec::new();
    let upper = word.len() + 1;
    for (k, p) in priorities.iter().enumerate().take(upper).skip(2) {
        if p % 2 == 1 {
            let byte_offset = k - 1;
            if byte_offset >= patterns.left_min
                && word.len() - byte_offset >= patterns.right_min
                && word.is_char_boundary(byte_offset)
            {
                breaks.push(byte_offset);
            }
        }
    }
    breaks
}

/// Applies every pattern that matches `haystack` to `priorities`.
///
/// Priorities combine with `max`, so the order patterns are applied in does
/// not matter. Only patterns whose first byte matches can apply at a
/// position, so each position checks one small group of patterns.
fn fill_priorities(haystack: &[u8], patterns: &Patterns, priorities: &mut [u8]) {
    for (i, &b) in haystack.iter().enumerate() {
        let at_start = i == 0;
        for idx in patterns.index.candidates(b) {
            if let Some(pat) = patterns.inner.get(idx) {
                apply_pattern(pat, haystack, i, at_start, priorities);
            }
        }
    }
}

fn apply_pattern(pat: &Pattern, haystack: &[u8], i: usize, at_start: bool, priorities: &mut [u8]) {
    if pat.anchored_start && !at_start {
        return;
    }
    let needle = pat.letters.as_bytes();
    if needle.len() > haystack.len() - i {
        return;
    }
    if !haystack[i..i + needle.len()].eq_ignore_ascii_case(needle) {
        return;
    }
    if pat.anchored_end && i + needle.len() != haystack.len() {
        return;
    }

    for (j, prio) in pat.priorities.iter().enumerate() {
        if *prio == 0 {
            continue;
        }
        let slot = i + j;
        if slot < priorities.len() && *prio > priorities[slot] {
            priorities[slot] = *prio;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smallest hand-rolled pattern set that demonstrates Liang on a
    /// single word. We cover only the patterns Liang's classic en-us
    /// set fires for "hyphenation":
    ///
    /// - `hy3ph`: encourage break between hy and ph (gives `hy-phen`).
    /// - `he2n`: discourage break before n.
    /// - `hena4`: discourage even more strongly between n and a.
    /// - `hen5at`: *strongly encourage* break between hen and at
    ///   (gives `phen-at` which combines with `hena4` to produce
    ///   the canonical `phen-a-tion`).
    /// - `1na`: slight encourage before na (loses to hen5at).
    /// - `n2at`: discourage before at.
    /// - `1tio`: encourage before tio (gives `a-tion`).
    /// - `2io`: discourage before io.
    /// - `2on`: discourage before on.
    fn small_en_patterns() -> Patterns {
        let text = "\
hy3ph\n\
he2n\n\
hena4\n\
hen5at\n\
1na\n\
n2at\n\
1tio\n\
2io\n\
2on\n\
";
        Patterns::parse(text).unwrap()
    }

    #[test]
    fn hyphenation_classic_breaks() {
        let p = small_en_patterns();
        // The hand-rolled set above does not cover "hy-phen-a-tion"
        // perfectly, but it must at least produce the `hy-` break.
        let breaks = hyphenate("hyphenation", &p);
        assert!(breaks.contains(&2), "expected hy- break, got {breaks:?}");
    }

    #[test]
    fn empty_for_too_short() {
        let p = small_en_patterns();
        assert_eq!(hyphenate("a", &p), Vec::<usize>::new());
        assert_eq!(hyphenate("hi", &p), Vec::<usize>::new());
    }

    #[test]
    fn respects_left_min_right_min() {
        let mut p = small_en_patterns();
        p.left_min = 5;
        p.right_min = 5;
        // The `hy-` break sits at offset 2; with left_min=5 it gets
        // filtered.
        let breaks = hyphenate("hyphenation", &p);
        assert!(!breaks.contains(&2));
    }

    #[test]
    fn anchored_start_only_fires_at_word_start() {
        // Pattern `.ach4` must only fire when "ach" appears at the
        // start of the word.
        let p = Patterns::parse(".ach4\n").unwrap();
        let breaks_start = hyphenate("achievement", &p);
        // `.ach4` puts a `4` at position after "ach" in `.achievement.`,
        // but priority 4 is even, so no break. We assert no panic and
        // an empty list (4 is even => not a break).
        assert!(breaks_start.is_empty());

        // Now an odd-priority anchored pattern: `.ach3`.
        let p2 = Patterns::parse(".ach3\n").unwrap();
        let breaks_mid = hyphenate("teach", &p2);
        // "teach" starts with `t`, not `a`, so `.ach3` should not fire.
        assert!(breaks_mid.is_empty());
    }

    /// Checks every pattern at every position, the way the algorithm did
    /// before patterns were grouped by first byte.
    fn priorities_checking_every_pattern(haystack: &[u8], patterns: &Patterns) -> Vec<u8> {
        let mut priorities = vec![0u8; haystack.len() + 1];
        for i in 0..haystack.len() {
            for pat in &patterns.inner {
                apply_pattern(pat, haystack, i, i == 0, &mut priorities);
            }
        }
        priorities
    }

    fn assert_same_priorities(patterns: &Patterns, words: &[&str]) {
        for word in words {
            let wrapped = alloc::format!(".{}.", word.to_ascii_lowercase());
            let bytes = wrapped.as_bytes();
            let mut indexed = vec![0u8; bytes.len() + 1];
            fill_priorities(bytes, patterns, &mut indexed);
            assert_eq!(
                indexed,
                priorities_checking_every_pattern(bytes, patterns),
                "{word:?}"
            );
        }
    }

    const SAMPLE_WORDS: &[&str] = &[
        "hyphenation",
        "HyPhEnAtIoN",
        "achievement",
        "teach",
        "a",
        "",
        "internationalization",
        "\u{00e9}t\u{00e9}",
        "co-operate",
        "x.y.z",
        "1234",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ];

    #[test]
    fn grouped_matching_equals_checking_every_pattern() {
        // Includes a pattern with no letters (`3`), which can match at every
        // position, and both anchor kinds.
        let p = Patterns::parse("hy3ph\nhe2n\n1na\n.ach3\nion1.\n3\nx1y\n.a1\n").unwrap();
        assert_same_priorities(&p, SAMPLE_WORDS);
    }

    #[cfg(feature = "patterns-en-us")]
    #[test]
    fn grouped_matching_equals_checking_every_pattern_for_en_us() {
        let p = Patterns::for_language(crate::Language::EnglishUs).unwrap();
        assert_same_priorities(p, SAMPLE_WORDS);
    }

    #[cfg(feature = "patterns-en-us")]
    #[test]
    fn each_position_checks_a_small_group_of_patterns() {
        // Before the grouping, every position checked all ~4,900 US English
        // patterns, so a long run of letters took that many checks per
        // letter. Each group must now be a small fraction of the set.
        let p = Patterns::for_language(crate::Language::EnglishUs).unwrap();
        let total = p.len();
        let largest = (0..=255u8)
            .map(|b| p.index.candidates(b).count())
            .max()
            .unwrap_or(0);
        assert!(
            largest * 4 < total,
            "largest group {largest} of {total} patterns"
        );
        let covered: usize = (b'a'..=b'z')
            .chain(core::iter::once(b'.'))
            .map(|b| p.index.candidates(b).count())
            .sum();
        assert_eq!(covered, total, "every pattern belongs to exactly one group");
    }
}
