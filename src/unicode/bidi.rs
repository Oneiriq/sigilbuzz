//! UAX #9 Unicode Bidirectional Algorithm.
//!
//! 0.1.0 shipped only the paragraph-direction first-strong rule (P2 /
//! P3). 0.10.0 fills in the rest:
//!
//! - **P1-P3**: paragraph-direction (already shipped, kept).
//! - **X1-X10**: explicit-embedding / override / isolate stack.
//! - **W1-W7**: weak-type resolution.
//! - **N1-N2**: neutral resolution. (N0 paired-bracket handling
//!   is intentionally deferred. See module note below.)
//! - **I1-I2**: implicit-level resolution.
//! - **L1-L4**: post-resolve normalization + reorder (rule L2).
//!
//! The algorithm is implemented as a sequence of array-mutation
//! passes against a single working buffer of (`BidiClass`, `level`)
//! pairs, mirroring the reference implementation. Output is exposed
//! through [`BidiInfo`].
//!
//! ## N0 paired-bracket handling
//!
//! UAX #9 §3.3.5: paired-bracket pass. After W1-W7 resolve the weak
//! types but before N1 / N2 sweep neutrals, brackets that pair across
//! the isolating-run sequence get a strong type assigned according to
//! the surrounding embedding context. The pair codepoint table lives
//! in [`crate::unicode::bidi_brackets`] (curated extract of
//! `BidiBrackets.txt`: ASCII + CJK + math families).
//!
//! Brackets that don't pair (unbalanced opener / closer, opener
//! without a matching closer) fall through unchanged and N1's
//! surrounding-strong fallback handles them, exactly the behavior
//! shipped before N0 landed.
//!
//! ## Public API
//!
//! [`BidiInfo::new`] runs the full algorithm against a paragraph and
//! exposes:
//!
//! - [`BidiInfo::paragraph_direction`]: resolved paragraph direction.
//! - [`BidiInfo::levels`]: per-character embedding level (L1-L4
//!   normalized).
//! - [`BidiInfo::reorder`]: visual-order character-index permutation
//!   (rule L2).
//!
//! Buffer integration uses [`crate::buffer::Buffer::set_text_bidi`],
//! which auto-runs the bidi pipeline before shaping. The plain
//! [`crate::buffer::Buffer::set_text`] is left untouched for backward
//! compat with 0.1.0 consumers (oniq, demos) that handle direction
//! themselves.

mod explicit;
mod neutral;
mod reorder;
mod weak;

use alloc::vec::Vec;

use explicit::{build_isolating_sequences, explicit_levels};
use reorder::apply_l1;
use weak::resolve_sequence;

use crate::buffer::Direction;
pub use crate::unicode::bidi_class::{bidi_class, BidiClass};

/// Applies UAX #9 rules P2 and P3 to `text` and returns the
/// paragraph-level direction. LTR when no strong character exists
/// in the run (whitespace-only, symbol-only, empty input).
///
/// Kept on the public surface so 0.1.0 callers don't break.
#[must_use]
pub fn paragraph_direction(text: &str) -> Direction {
    paragraph_direction_with_isolates(text)
}

/// P2 / P3 with proper isolate-skipping. Characters between an
/// isolate-initiator (LRI / RLI / FSI) and its matching PDI do not
/// participate in paragraph-direction resolution.
fn paragraph_direction_with_isolates(text: &str) -> Direction {
    let mut depth: u32 = 0;
    for ch in text.chars() {
        let cls = bidi_class(ch);
        if cls.is_isolate_initiator() {
            depth = depth.saturating_add(1);
            continue;
        }
        if cls == BidiClass::Pdi {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth > 0 {
            continue;
        }
        match cls {
            BidiClass::L => return Direction::Ltr,
            BidiClass::R | BidiClass::Al => return Direction::Rtl,
            _ => {}
        }
    }
    Direction::Ltr
}

/// Per-character bidi state: the working pair the algorithm mutates.
#[derive(Debug, Clone, Copy)]
struct BidiCell {
    /// Resolved Bidi_Class. Mutated by W1-W7 / N1-N2 / I1-I2.
    cls: BidiClass,
    /// Resolved embedding level. Set by X1-X10, mutated by I1-I2 /
    /// L1.
    level: u8,
}

/// Result of running the algorithm against a paragraph.
///
/// Levels are stored per *character* in the input string (not per
/// byte). [`BidiInfo::reorder`] returns a character-index
/// permutation; map back to byte ranges via `char_indices()`.
#[derive(Debug, Clone)]
pub struct BidiInfo {
    /// Paragraph direction resolved by P2 / P3.
    paragraph: Direction,
    /// Per-character embedding levels post L1.
    levels: Vec<u8>,
    /// Character count of the original text. Always == `levels.len()`.
    char_count: usize,
}

impl BidiInfo {
    /// Runs the algorithm against `text`. If `paragraph_dir` is
    /// `None`, P2 / P3 resolves it from the first strong character.
    /// Otherwise the override is honored (matches the
    /// `unicode-bidi` API).
    #[must_use]
    pub fn new(text: &str, paragraph_dir: Option<Direction>) -> Self {
        let paragraph = paragraph_dir.unwrap_or_else(|| paragraph_direction_with_isolates(text));
        let chars: Vec<char> = text.chars().collect();
        let mut cells: Vec<BidiCell> = chars
            .iter()
            .map(|&ch| BidiCell {
                cls: bidi_class(ch),
                level: 0,
            })
            .collect();
        let char_count = cells.len();
        if char_count == 0 {
            return BidiInfo {
                paragraph,
                levels: Vec::new(),
                char_count: 0,
            };
        }
        let para_level: u8 = match paragraph {
            Direction::Rtl => 1,
            _ => 0,
        };

        // X1-X10: explicit-level resolution.
        explicit_levels(&mut cells, para_level);

        // Partition into level runs and isolating run sequences,
        // then run W1-W7 + N0 + N1-N2 + I1-I2 per sequence.
        let isolating_sequences = build_isolating_sequences(&cells, para_level);
        for seq in isolating_sequences {
            resolve_sequence(&mut cells, &chars, &seq, para_level);
        }

        // L1: reset trailing whitespace, segment separators, and
        // paragraph separators back to the paragraph level.
        apply_l1(&mut cells, para_level, text);

        let levels: Vec<u8> = cells.iter().map(|c| c.level).collect();
        BidiInfo {
            paragraph,
            levels,
            char_count,
        }
    }

    /// Resolved paragraph direction.
    #[must_use]
    pub const fn paragraph_direction(&self) -> Direction {
        self.paragraph
    }

    /// Per-character embedding level (post L1).
    #[must_use]
    pub fn levels(&self) -> &[u8] {
        &self.levels
    }

    /// Character count of the input.
    #[must_use]
    pub const fn char_count(&self) -> usize {
        self.char_count
    }

    /// Returns the visual-order character-index permutation (rule
    /// L2). Indices are into the original character sequence
    /// (`text.chars().nth(i)`); the returned `Vec` always has
    /// length [`Self::char_count`].
    #[must_use]
    pub fn reorder(&self) -> Vec<usize> {
        let n = self.char_count;
        let mut order: Vec<usize> = (0..n).collect();
        if n <= 1 {
            return order;
        }
        // L2: from the highest level down to the lowest odd level,
        // reverse the contiguous span at or above that level.
        let max_level = self.levels.iter().copied().max().unwrap_or(0);
        let min_level = self.levels.iter().copied().min().unwrap_or(0);
        // Lowest odd level: anything below it is purely-LTR and
        // never gets reversed.
        let lowest_odd = if min_level % 2 == 1 {
            min_level
        } else {
            min_level + 1
        };
        let mut level = max_level;
        while level >= lowest_odd {
            // Walk through and reverse every contiguous run whose
            // level is >= `level`.
            let mut i = 0;
            while i < n {
                if self.levels[i] >= level {
                    let mut j = i;
                    while j < n && self.levels[j] >= level {
                        j += 1;
                    }
                    order[i..j].reverse();
                    i = j;
                } else {
                    i += 1;
                }
            }
            if level == 0 {
                break;
            }
            level -= 1;
        }
        order
    }
}

// ---------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests;
