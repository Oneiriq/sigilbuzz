//! The `Bidi_Mirroring_Glyph` property.
//!
//! In a right-to-left run, HarfBuzz (`hb_ot_rotate_chars` in
//! `hb-ot-shape.cc`) replaces each character that has a mirroring glyph
//! with that glyph's character when the font maps it, so `(` draws as
//! `)`. The table in `mirroring_table.rs` is generated from Unicode
//! 17.0.0 `BidiMirroring.txt` (snapshot in `tests/tools/ucd/`;
//! regenerate with `cargo test --test unicode_table_gen -- --ignored`).

use super::mirroring_table::MIRRORING;

/// The character whose glyph mirrors `ch` (`Bidi_Mirroring_Glyph`),
/// or `None` when `ch` has none. Characters that are Bidi_Mirrored but
/// have no mirroring glyph in the UCD, such as U+2211 N-ARY
/// SUMMATION, also return `None`; fonts handle those through the
/// `rtlm` feature.
///
/// # Examples
///
/// ```
/// use sigilbuzz::unicode::mirroring::bidi_mirroring_glyph;
///
/// assert_eq!(bidi_mirroring_glyph('('), Some(')'));
/// assert_eq!(bidi_mirroring_glyph('\u{00AB}'), Some('\u{00BB}'));
/// assert_eq!(bidi_mirroring_glyph('a'), None);
/// assert_eq!(bidi_mirroring_glyph('\u{2211}'), None);
/// ```
#[must_use]
pub fn bidi_mirroring_glyph(ch: char) -> Option<char> {
    let cp = u32::from(ch);
    MIRRORING
        .binary_search_by_key(&cp, |&(from, _)| from)
        .ok()
        .and_then(|i| char::from_u32(MIRRORING[i].1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_mirror_both_ways() {
        for (a, b) in [
            ('(', ')'),
            ('[', ']'),
            ('{', '}'),
            ('<', '>'),
            ('\u{2208}', '\u{220B}'),
        ] {
            assert_eq!(bidi_mirroring_glyph(a), Some(b));
            assert_eq!(bidi_mirroring_glyph(b), Some(a));
        }
    }

    #[test]
    fn table_is_sorted_and_symmetric_for_brackets() {
        assert!(MIRRORING.windows(2).all(|w| w[0].0 < w[1].0));
        // Every bracket pair the bidi algorithm knows is a mirror pair.
        for &(cp, _) in MIRRORING {
            let ch = char::from_u32(cp).expect("scalar value");
            if let Some(entry) = super::super::bidi_brackets::bracket_of(ch as u32) {
                assert_eq!(bidi_mirroring_glyph(ch).map(u32::from), Some(entry.pair));
            }
        }
    }

    #[test]
    fn unmirrored_characters_have_none() {
        assert_eq!(bidi_mirroring_glyph('A'), None);
        assert_eq!(bidi_mirroring_glyph('\u{0627}'), None);
        // Bidi_Mirrored=Yes without a mirroring glyph.
        assert_eq!(bidi_mirroring_glyph('\u{221A}'), None);
    }
}
