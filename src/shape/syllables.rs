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

use crate::buffer::Glyph;

/// Numbers the syllables of `glyphs`, one glyph per code point: each
/// `(start, end, kind)` gives its glyphs `start..end` the next serial
/// and the type `kind`. Glyphs outside every syllable keep theirs.
pub(crate) fn number_syllables(
    glyphs: &mut [Glyph],
    syllables: impl IntoIterator<Item = (usize, usize, u8)>,
) {
    let mut serial: u8 = 1;
    for (start, end, kind) in syllables {
        let value = (serial << 4) | (kind & 0x0F);
        if let Some(run) = glyphs.get_mut(start..end.min(glyphs.len())) {
            for g in run {
                g.syllable = value;
            }
        }
        serial = if serial == 15 { 1 } else { serial + 1 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn serials_run_from_one_to_fifteen_and_wrap() {
        let mut glyphs: Vec<Glyph> = (0..17).map(|i| Glyph::new(1, i)).collect();
        number_syllables(&mut glyphs, (0..17).map(|i| (i, i + 1, 2)));
        assert_eq!(glyphs[0].syllable, 0x12);
        assert_eq!(glyphs[14].syllable, 0xF2);
        assert_eq!(glyphs[15].syllable, 0x12);
        // A range past the run is cut at its end; the glyphs keep
        // their syllable when a range misses them.
        number_syllables(&mut glyphs, [(16, 99, 3)]);
        assert_eq!(glyphs[16].syllable, 0x13);
        assert_eq!(glyphs[15].syllable, 0x12);
    }
}
