//! Byte-offset mapping between logical and visual text order.
//!
//! [`crate::Buffer::set_text_bidi`] reorders mixed-direction input
//! into visual order before shaping, which means every
//! [`crate::Glyph::cluster`] the shaper emits indexes the *reordered*
//! string. That is correct for rendering but useless on its own for
//! editing: a caret lives at a byte offset in the *logical* (source)
//! string, and hit-testing produces a visual position. [`BidiMap`] is
//! the bridge: it records, for every character, where it sits in
//! both orders, so consumers can translate cluster values back to
//! source offsets (`visual_to_logical`) and caret offsets forward to
//! visual positions (`logical_to_visual`).
//!
//! Offsets on both sides are UTF-8 **byte** offsets, matching the
//! cluster convention (`shape()` assigns `cluster` = byte offset of
//! the character's first byte). Both strings contain the same
//! characters, so total byte length is identical; only positions
//! permute.
//!
//! ```
//! use sigilbuzz::{BidiMap, Buffer};
//!
//! let mut buffer = Buffer::new();
//! buffer.set_text_bidi("abc \u{05D0}\u{05D1}");
//! let map = buffer.bidi_map().expect("set_text_bidi retains the map");
//!
//! // The Hebrew pair renders first-to-last swapped: the visually
//! // leftmost of the two (byte 4 in the visual string) is the
//! // logically *last* character (byte 6 in the source).
//! assert_eq!(map.visual_to_logical(4), Some(6));
//! assert_eq!(map.logical_to_visual(6), Some(4));
//! ```

use alloc::vec::Vec;

use crate::buffer::Direction;
use crate::unicode::bidi::BidiInfo;

/// Bidirectional reorder map: per-character byte offsets in visual
/// and logical order, plus resolved embedding levels.
///
/// Build one with [`BidiMap::new`] (or receive one from
/// [`crate::Buffer::bidi_map`] after
/// [`crate::Buffer::set_text_bidi`]). Lookups round byte offsets
/// down to the containing character, so any in-character byte (in
/// particular any `cluster` value) resolves to that character's
/// first byte.
///
/// The visual-side offsets match [`crate::Glyph::cluster`] values as
/// long as the shaper saw the buffer text unchanged. The opt-in NFC
/// pass ([`crate::Buffer::set_normalize_nfc`]) can shorten the text
/// it shapes, shifting cluster offsets after any composed pair:
/// feed precomposed input (or leave NFC off) when combining it with
/// bidi mapping.
#[derive(Debug, Clone)]
pub struct BidiMap {
    /// Byte start of each character in the visual string, in visual
    /// order. Strictly increasing.
    visual_starts: Vec<u32>,
    /// Byte start of the same character in the logical string,
    /// parallel to `visual_starts`.
    logical_starts: Vec<u32>,
    /// Embedding level (post L1) of the same character, parallel to
    /// `visual_starts`.
    levels: Vec<u8>,
    /// `(logical_start, parallel-array index)` sorted by
    /// `logical_start`, for the inverse lookup.
    logical_index: Vec<(u32, u32)>,
    /// Resolved paragraph direction.
    paragraph: Direction,
    /// Total byte length of the text (both orders).
    text_len: u32,
    /// True when no character moved (pure-LTR input).
    identity: bool,
}

impl BidiMap {
    /// Builds the map for `text` from a resolved [`BidiInfo`].
    ///
    /// Equivalent to `BidiMap::from_order(text, info.reorder(),
    /// info)`.
    ///
    /// # Panics
    ///
    /// Debug builds panic if `info` was computed for a different
    /// string (its [`BidiInfo::char_count`] must match `text`'s
    /// character count). See [`BidiMap::from_order`] for what release
    /// builds return instead.
    #[must_use]
    pub fn new(text: &str, info: &BidiInfo) -> Self {
        Self::from_order(text, &info.reorder(), info)
    }

    /// Builds the map from a precomputed visual-order permutation.
    ///
    /// `order` must be the value of [`BidiInfo::reorder`] for this
    /// exact `text` / `info` pair. Callers that already reordered
    /// the string (like [`crate::Buffer::set_text_bidi`]) pass it in
    /// so the L2 pass runs once.
    ///
    /// When the inputs do not fit together (`order` is not a
    /// permutation of `text`'s characters, `info` was computed for a
    /// different string, or `text` exceeds `u32::MAX` bytes) the
    /// result is an empty map: every lookup returns `None`.
    ///
    /// # Panics
    ///
    /// Debug builds panic if `order`'s length or `info`'s character
    /// count differs from `text`'s character count, to flag the
    /// caller bug early.
    #[must_use]
    pub fn from_order(text: &str, order: &[usize], info: &BidiInfo) -> Self {
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        debug_assert_eq!(
            order.len(),
            chars.len(),
            "order length must match text character count"
        );
        debug_assert_eq!(
            info.char_count(),
            chars.len(),
            "BidiInfo was computed for a different string"
        );
        let paragraph = info.paragraph_direction();
        let levels_logical = info.levels();
        if u32::try_from(text.len()).is_err()
            || order.len() != chars.len()
            || levels_logical.len() != chars.len()
        {
            return Self::empty(paragraph);
        }

        let n = chars.len();
        let mut visual_starts = Vec::with_capacity(n);
        let mut logical_starts = Vec::with_capacity(n);
        let mut levels = Vec::with_capacity(n);
        let mut identity = true;
        let mut visual_byte: u32 = 0;
        for (visual_idx, &logical_idx) in order.iter().enumerate() {
            let (Some(&(logical_byte, ch)), Some(&level)) =
                (chars.get(logical_idx), levels_logical.get(logical_idx))
            else {
                return Self::empty(paragraph);
            };
            visual_starts.push(visual_byte);
            // Both casts are lossless: `text.len()` fits in `u32`.
            logical_starts.push(logical_byte as u32);
            levels.push(level);
            identity &= visual_idx == logical_idx;
            visual_byte += ch.len_utf8() as u32;
        }

        let mut logical_index: Vec<(u32, u32)> = logical_starts
            .iter()
            .enumerate()
            .map(|(i, &l)| (l, i as u32))
            .collect();
        logical_index.sort_unstable_by_key(|&(l, _)| l);

        // A permutation lists every character start exactly once, so
        // the sorted logical starts must equal the text's own starts.
        // A repeated index breaks that and would make the inverse
        // lookups disagree with the forward ones.
        let is_permutation = logical_index
            .iter()
            .map(|&(l, _)| l as usize)
            .eq(chars.iter().map(|&(b, _)| b));
        if !is_permutation {
            return Self::empty(paragraph);
        }

        Self {
            visual_starts,
            logical_starts,
            levels,
            logical_index,
            paragraph,
            text_len: visual_byte,
            identity,
        }
    }

    /// A map with no characters. Every lookup returns `None`.
    fn empty(paragraph: Direction) -> Self {
        Self {
            visual_starts: Vec::new(),
            logical_starts: Vec::new(),
            levels: Vec::new(),
            logical_index: Vec::new(),
            paragraph,
            text_len: 0,
            identity: true,
        }
    }

    /// Resolved paragraph direction (P2 / P3).
    #[must_use]
    pub const fn paragraph_direction(&self) -> Direction {
        self.paragraph
    }

    /// Number of characters mapped.
    #[must_use]
    pub fn len(&self) -> usize {
        self.visual_starts.len()
    }

    /// True for empty text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.visual_starts.is_empty()
    }

    /// True when the visual order equals the logical order (no
    /// reordering happened, pure-LTR input). Consumers can skip
    /// mapping entirely in that case.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        self.identity
    }

    /// Maps a byte offset in the visual (reordered) string (e.g. a
    /// [`crate::Glyph::cluster`] value) to the byte offset of the
    /// same character in the logical (source) string.
    ///
    /// The offset is rounded down to the containing character's
    /// first byte. Returns `None` when `visual_byte` is at or past
    /// the end of the text (an end-of-text caret has no containing
    /// character; handle it before mapping).
    #[must_use]
    pub fn visual_to_logical(&self, visual_byte: usize) -> Option<usize> {
        let idx = round_down_index(&self.visual_starts, visual_byte, self.text_len)?;
        self.logical_starts.get(idx).map(|&l| l as usize)
    }

    /// Maps a byte offset in the logical (source) string to the byte
    /// offset of the same character in the visual (reordered)
    /// string.
    ///
    /// The offset is rounded down to the containing character's
    /// first byte. Returns `None` at or past the end of the text.
    #[must_use]
    pub fn logical_to_visual(&self, logical_byte: usize) -> Option<usize> {
        let idx = self.logical_index_at(logical_byte)?;
        self.visual_starts.get(idx).map(|&v| v as usize)
    }

    /// Embedding level (post L1) of the character containing the
    /// given byte offset in the visual string. Odd levels are
    /// right-to-left: the signal caret math needs to pick which
    /// side of a glyph a boundary caret sits on.
    #[must_use]
    pub fn level_at_visual(&self, visual_byte: usize) -> Option<u8> {
        let idx = round_down_index(&self.visual_starts, visual_byte, self.text_len)?;
        self.levels.get(idx).copied()
    }

    /// Embedding level (post L1) of the character containing the
    /// given byte offset in the logical string.
    #[must_use]
    pub fn level_at_logical(&self, logical_byte: usize) -> Option<u8> {
        let idx = self.logical_index_at(logical_byte)?;
        self.levels.get(idx).copied()
    }

    /// Index into the parallel arrays for the character containing
    /// `logical_byte`, via the sorted inverse index.
    fn logical_index_at(&self, logical_byte: usize) -> Option<usize> {
        if logical_byte >= self.text_len as usize {
            return None;
        }
        // Lossless: below `text_len`, which is a `u32`.
        let logical_byte = logical_byte as u32;
        let pos = self
            .logical_index
            .partition_point(|&(l, _)| l <= logical_byte);
        // pos > 0 for any non-empty map: offset 0 is a char start.
        let (_, idx) = self.logical_index.get(pos.checked_sub(1)?)?;
        Some(*idx as usize)
    }
}

/// Rounds `byte` down to the containing character and returns its
/// index in `starts` (a strictly increasing list of char byte
/// starts). `None` at or past `text_len`.
fn round_down_index(starts: &[u32], byte: usize, text_len: u32) -> Option<usize> {
    if byte >= text_len as usize {
        return None;
    }
    // Lossless: below `text_len`, which is a `u32`.
    let byte = byte as u32;
    let pos = starts.partition_point(|&s| s <= byte);
    // pos > 0 for any non-empty map: starts[0] == 0.
    pos.checked_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_for(text: &str) -> BidiMap {
        BidiMap::new(text, &BidiInfo::new(text, None))
    }

    #[test]
    fn ltr_text_is_identity() {
        let map = map_for("hello world");
        assert!(map.is_identity());
        assert_eq!(map.len(), 11);
        assert_eq!(map.visual_to_logical(0), Some(0));
        assert_eq!(map.visual_to_logical(10), Some(10));
        assert_eq!(map.logical_to_visual(4), Some(4));
        assert_eq!(map.visual_to_logical(11), None);
        assert_eq!(map.paragraph_direction(), Direction::Ltr);
    }

    #[test]
    fn empty_text_maps_nothing() {
        let map = map_for("");
        assert!(map.is_empty());
        assert!(map.is_identity());
        assert_eq!(map.visual_to_logical(0), None);
        assert_eq!(map.logical_to_visual(0), None);
        assert_eq!(map.level_at_visual(0), None);
    }

    #[test]
    fn pure_rtl_reverses_char_order() {
        // Three 2-byte Hebrew chars: logical starts 0, 2, 4.
        let text = "\u{05D0}\u{05D1}\u{05D2}";
        let map = map_for(text);
        assert!(!map.is_identity());
        assert_eq!(map.paragraph_direction(), Direction::Rtl);
        // Visually first char is logically last.
        assert_eq!(map.visual_to_logical(0), Some(4));
        assert_eq!(map.visual_to_logical(2), Some(2));
        assert_eq!(map.visual_to_logical(4), Some(0));
        // Inverse agrees.
        assert_eq!(map.logical_to_visual(0), Some(4));
        assert_eq!(map.logical_to_visual(4), Some(0));
        // RTL chars sit at odd embedding levels.
        assert_eq!(map.level_at_logical(0), Some(1));
    }

    #[test]
    fn lookups_round_down_to_char_starts() {
        let text = "\u{05D0}\u{05D1}"; // 2-byte chars at 0 and 2
        let map = map_for(text);
        // Mid-character bytes resolve to the containing char.
        assert_eq!(map.visual_to_logical(1), map.visual_to_logical(0));
        assert_eq!(map.visual_to_logical(3), map.visual_to_logical(2));
        assert_eq!(map.logical_to_visual(1), map.logical_to_visual(0));
    }

    #[test]
    fn mixed_run_round_trips_every_char() {
        let text = "abc \u{05D0}\u{05D1}\u{05D2} def 123";
        let map = map_for(text);
        assert!(!map.is_identity());
        for (logical_byte, _) in text.char_indices() {
            let visual = map
                .logical_to_visual(logical_byte)
                .expect("in-range logical offset maps");
            assert_eq!(
                map.visual_to_logical(visual),
                Some(logical_byte),
                "round trip failed for logical byte {logical_byte}"
            );
        }
    }

    #[test]
    fn mixed_run_maps_chars_to_equal_chars() {
        // The character AT the visual offset must be the character AT
        // the mapped logical offset. The map is a permutation of the
        // same characters.
        let text = "ab \u{05D0}\u{05D1} cd";
        let info = BidiInfo::new(text, None);
        let order = info.reorder();
        let map = BidiMap::from_order(text, &order, &info);

        // Rebuild the visual string the same way Buffer::set_text_bidi
        // does.
        let chars: Vec<char> = text.chars().collect();
        let visual: alloc::string::String = order.iter().map(|&i| chars[i]).collect();

        for (visual_byte, vch) in visual.char_indices() {
            let logical_byte = map
                .visual_to_logical(visual_byte)
                .expect("in-range visual offset maps");
            let lch = text[logical_byte..].chars().next().expect("char start");
            assert_eq!(vch, lch, "char mismatch at visual byte {visual_byte}");
        }
    }

    #[test]
    #[should_panic(expected = "order length must match")]
    fn from_order_rejects_mismatched_order() {
        let text = "abc";
        let info = BidiInfo::new(text, None);
        let _ = BidiMap::from_order(text, &[0, 1], &info);
    }
}
