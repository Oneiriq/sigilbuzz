//! Syllable numbers for the syllabic shapers, HarfBuzz's `syllable()`
//! glyph byte (`found_syllable` in the shapers' Ragel machines).
//!
//! The Indic, Khmer, Myanmar, and USE shapers find syllables before
//! any lookup runs and number them: a serial from 1 to 15 in the high
//! four bits (wrapping back to 1), the syllable's type in the low
//! four. The number rides on every glyph of the syllable through
//! GSUB, so a feature registered per syllable only matches glyphs of
//! the cursor's syllable (see
//! [`MatchContext::with_per_syllable`](crate::tables::layout::MatchContext::with_per_syllable)).
//!
//! As in the shapers' `setup_syllables` functions, each syllable is
//! also marked unsafe to break: its shape depends on all of it.

use super::glyph_flags;
use crate::buffer::{ClusterLevel, Glyph};

/// Numbers the syllables of `glyphs`, one glyph per code point: each
/// `(start, end, kind)` gives its glyphs `start..end` the next serial
/// and the type `kind`, and marks it unsafe to break at the cluster
/// `level`. Glyphs outside every syllable keep theirs.
pub(crate) fn number_syllables(
    glyphs: &mut [Glyph],
    syllables: impl IntoIterator<Item = (usize, usize, u8)>,
    level: ClusterLevel,
) {
    let mut serial: u8 = 1;
    for (start, end, kind) in syllables {
        let value = (serial << 4) | (kind & 0x0F);
        if let Some(run) = glyphs.get_mut(start..end.min(glyphs.len())) {
            for g in run {
                g.syllable = value;
            }
        }
        glyph_flags::unsafe_to_break(glyphs, start, end, level);
        serial = if serial == 15 { 1 } else { serial + 1 };
    }
}

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

    const MC: ClusterLevel = ClusterLevel::MonotoneCharacters;

    #[test]
    fn serials_run_from_one_to_fifteen_and_wrap() {
        let mut glyphs: Vec<Glyph> = (0..17).map(|i| Glyph::new(1, i)).collect();
        number_syllables(&mut glyphs, (0..17).map(|i| (i, i + 1, 2)), MC);
        assert_eq!(glyphs[0].syllable, 0x12);
        assert_eq!(glyphs[14].syllable, 0xF2);
        assert_eq!(glyphs[15].syllable, 0x12);
        // A range past the run is cut at its end; the glyphs keep
        // their syllable when a range misses them.
        number_syllables(&mut glyphs, [(16, 99, 3)], MC);
        assert_eq!(glyphs[16].syllable, 0x13);
        assert_eq!(glyphs[15].syllable, 0x12);
    }

    #[test]
    fn a_syllable_is_unsafe_to_break_inside() {
        let mut glyphs: Vec<Glyph> = (0..4).map(|i| Glyph::new(1, i)).collect();
        number_syllables(&mut glyphs, [(0, 3, 1), (3, 4, 1)], MC);
        let flags: Vec<u32> = glyphs.iter().map(|g| g.flags.bits()).collect();
        assert_eq!(flags, [0, 3, 3, 0]);
    }
}
