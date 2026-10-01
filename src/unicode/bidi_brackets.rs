//! UAX #9 Bidi_Paired_Bracket / Bidi_Paired_Bracket_Type data.
//!
//! The table in `bidi_brackets_table.rs` is generated from Unicode
//! 17.0.0 `BidiBrackets.txt` by `tests/unicode_table_gen.rs`. Each row
//! maps a bracket code point to its [`BracketType`] (open / close) and
//! the code point of the matching half.

use super::bidi_brackets_table::BRACKETS;

/// Whether a bracket codepoint opens or closes its pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BracketType {
    /// Opening half of the pair: `(`, `[`, `「`, ...
    Open,
    /// Closing half of the pair: `)`, `]`, `」`, ...
    Close,
}

/// One bracket-pair entry. The `pair` field holds the codepoint of the
/// matching bracket, i.e. the open's close, or the close's open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BracketEntry {
    /// Open or close.
    pub kind: BracketType,
    /// Codepoint of the matching half of this pair.
    pub pair: u32,
}

/// Returns the bracket entry for `cp`, or `None` if the code point is
/// not a paired bracket.
///
/// ```
/// use sigilbuzz::unicode::bidi_brackets::{bracket_of, BracketType};
///
/// let open = bracket_of(0x0F3A).map(|e| (e.kind, e.pair));
/// assert_eq!(open, Some((BracketType::Open, 0x0F3B)));
/// assert_eq!(bracket_of(u32::from('a')), None);
/// ```
#[must_use]
pub const fn bracket_of(cp: u32) -> Option<BracketEntry> {
    let table = BRACKETS;
    let (mut lo, mut hi) = (0, table.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        // `mid < hi <= table.len()`, so the index is in range.
        let (bracket, pair, kind) = table[mid];
        if cp < bracket {
            hi = mid;
        } else if cp > bracket {
            lo = mid + 1;
        } else {
            return Some(BracketEntry { kind, pair });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_paren_pairs_resolve() {
        assert_eq!(
            bracket_of('(' as u32),
            Some(BracketEntry {
                kind: BracketType::Open,
                pair: ')' as u32
            }),
        );
        assert_eq!(
            bracket_of(')' as u32),
            Some(BracketEntry {
                kind: BracketType::Close,
                pair: '(' as u32
            }),
        );
    }

    #[test]
    fn ascii_square_and_curly_pairs_resolve() {
        assert_eq!(
            bracket_of('[' as u32),
            Some(BracketEntry {
                kind: BracketType::Open,
                pair: ']' as u32
            }),
        );
        assert_eq!(
            bracket_of('{' as u32),
            Some(BracketEntry {
                kind: BracketType::Open,
                pair: '}' as u32
            }),
        );
    }

    #[test]
    fn cjk_corner_brackets_resolve() {
        // U+300C 「 <-> U+300D 」.
        assert_eq!(
            bracket_of(0x300C),
            Some(BracketEntry {
                kind: BracketType::Open,
                pair: 0x300D
            }),
        );
        assert_eq!(
            bracket_of(0x300D),
            Some(BracketEntry {
                kind: BracketType::Close,
                pair: 0x300C
            }),
        );
    }

    #[test]
    fn math_angle_brackets_resolve() {
        // U+27E8 ⟨ <-> U+27E9 ⟩.
        assert_eq!(
            bracket_of(0x27E8),
            Some(BracketEntry {
                kind: BracketType::Open,
                pair: 0x27E9
            }),
        );
        assert_eq!(
            bracket_of(0x27E9),
            Some(BracketEntry {
                kind: BracketType::Close,
                pair: 0x27E8
            }),
        );
    }

    #[test]
    fn fullwidth_paren_resolves() {
        assert_eq!(
            bracket_of(0xFF08),
            Some(BracketEntry {
                kind: BracketType::Open,
                pair: 0xFF09
            }),
        );
    }

    #[test]
    fn non_brackets_return_none() {
        assert_eq!(bracket_of('A' as u32), None);
        assert_eq!(bracket_of(' ' as u32), None);
        assert_eq!(bracket_of(0x05D0), None); // Hebrew alef
    }

    #[test]
    fn open_close_relation_is_symmetric() {
        // Pick a few pairs and verify the closing half points back to
        // the opening half and vice versa.
        for &cp in &[
            '(' as u32, '[' as u32, '{' as u32, 0x300C, 0x27E8, 0xFF08, 0xFE59,
        ] {
            let open = bracket_of(cp).expect("known opener");
            assert_eq!(open.kind, BracketType::Open);
            let close = bracket_of(open.pair).expect("paired closer exists");
            assert_eq!(close.kind, BracketType::Close);
            assert_eq!(close.pair, cp, "round-trip pair lookup");
        }
    }

    #[test]
    fn tick_brackets_pair_crosswise_as_in_bidi_brackets_txt() {
        let pair = |cp| bracket_of(cp).map(|e| (e.kind, e.pair));
        assert_eq!(pair(0x298D), Some((BracketType::Open, 0x2990)));
        assert_eq!(pair(0x2990), Some((BracketType::Close, 0x298D)));
        assert_eq!(pair(0x298F), Some((BracketType::Open, 0x298E)));
        assert_eq!(pair(0x298E), Some((BracketType::Close, 0x298F)));
    }
}
