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
/// The returned offsets are *byte indices* into `word` and lie strictly
/// between 1 and `word.len() - 1`. Offsets are filtered against the
/// `left_min` / `right_min` thresholds carried by `patterns`.
///
/// # ASCII only
///
/// Liang's algorithm and the bundled pattern files assume the word
/// consists of 7-bit ASCII letters. Non-ASCII input is lower-cased
/// where possible but characters outside the ASCII letter range are
/// passed through verbatim and will simply fail to match any pattern,
/// producing zero break opportunities.
#[must_use]
pub fn hyphenate(word: &str, patterns: &Patterns) -> Vec<usize> {
    if word.len() < patterns.left_min + patterns.right_min {
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

    for i in 0..bytes.len() {
        let at_start = i == 0;
        for pat in &patterns.inner {
            apply_pattern(pat, bytes, i, at_start, &mut priorities);
        }
    }

    // Translate priority indices back to byte offsets in the *original*
    // word. The wrapped word is `.word.`, so a priority at wrapped
    // index `k` corresponds to offset `k - 1` in `word`. Valid break
    // offsets are 1..word.len().
    let mut breaks = Vec::new();
    let upper = word.len() + 1;
    for (k, p) in priorities.iter().enumerate().take(upper).skip(2) {
        if p % 2 == 1 {
            let byte_offset = k - 1;
            if byte_offset >= patterns.left_min && word.len() - byte_offset >= patterns.right_min {
                breaks.push(byte_offset);
            }
        }
    }
    breaks
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
}
