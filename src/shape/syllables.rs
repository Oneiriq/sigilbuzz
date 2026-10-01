//! Unsafe-to-break ranges for the syllabic shapers.
//!
//! The Indic, Khmer, Myanmar, and USE shapers find syllables before
//! any lookup runs and keep each syllable's number on its glyphs (see
//! `crate::ot::syllabic`), so a feature registered per syllable only
//! matches glyphs of the cursor's syllable (see
//! [`MatchContext::with_per_syllable`](crate::tables::layout::MatchContext::with_per_syllable)).
//! As in the shapers' `setup_syllables` functions, each syllable is
//! also marked unsafe to break: its shape depends on all of it.

use super::glyph_flags;
use crate::buffer::{ClusterLevel, Glyph};

/// HarfBuzz's `unsafe_to_break(start, end)` for the shapers that keep
/// their syllables beside the glyphs: the glyphs of
/// `glyphs[start..end]` outside its smallest cluster become unsafe to
/// break at the cluster `level`.
pub(crate) fn unsafe_to_break(glyphs: &mut [Glyph], start: usize, end: usize, level: ClusterLevel) {
    glyph_flags::unsafe_to_break(glyphs, start, end, level);
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn a_range_is_unsafe_to_break_inside() {
        let mut glyphs: Vec<Glyph> = (0..4).map(|i| Glyph::new(1, i)).collect();
        unsafe_to_break(&mut glyphs, 0, 3, ClusterLevel::MonotoneCharacters);
        let flags: Vec<u32> = glyphs.iter().map(|g| g.flags.bits()).collect();
        assert_eq!(flags, [0, 3, 3, 0]);
    }
}
