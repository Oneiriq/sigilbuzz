//! The per-glyph props the lookup matching rules read.
//!
//! They live in the low byte of [`Glyph::unicode_props`], laid out as
//! in [`match_prop`], next to the ligature bookkeeping of the `lig`
//! module in the high byte. A glyph starts with the props of the
//! character it was mapped from, the way HarfBuzz sets them up before
//! GSUB:
//!
//! - the default-ignorable and joiner bits (see the `ignorables`
//!   module);
//! - HarfBuzz's hidden bit (`_hb_glyph_info_set_unicode_props`) for
//!   the default ignorables GSUB must still see: COMBINING GRAPHEME
//!   JOINER, the Mongolian free variation selectors, and the tag
//!   characters;
//! - the synthesized glyph class (`hb_synthesize_glyph_classes`): a
//!   nonspacing mark that is not default ignorable is a mark, anything
//!   else a base glyph. HarfBuzz reads the General_Category it stores
//!   with the character, which a character a shaper inserted copies
//!   from the mark after it. The class only matters for fonts without
//!   a GDEF `GlyphClassDef`. GSUB updates it as it ligates and expands
//!   glyphs (see the `lig` module).
//!
//! Normalization (the `normalize` module) sets these props for the
//! characters it produces, and un-hides a COMBINING GRAPHEME JOINER
//! that did not block any mark reordering, as HarfBuzz does.

use super::ignorables;
use crate::buffer::{char_class, Glyph};
use crate::tables::layout::skip_iter::{match_prop, MatchGlyph};

impl From<&Glyph> for MatchGlyph {
    fn from(g: &Glyph) -> Self {
        MatchGlyph::with_props(g.glyph_id as u16, g.unicode_props)
    }
}

/// The matching props a glyph mapped from `ch` starts with, on top of
/// the default-ignorable and joiner bits. `class` holds the
/// `char_class` bits of the character's General_Category.
pub(super) fn initial(ch: char, class: u8) -> u16 {
    if ignorables::is_default_ignorable(ch) {
        // Never a mark, so that lookups skipping marks do not skip
        // them; some of these are hidden.
        if matches!(ch as u32, 0x034F | 0x180B..=0x180D | 0x180F | 0xE0020..=0xE007F) {
            match_prop::HIDDEN
        } else {
            0
        }
    } else if class & char_class::NONSPACING_MARK != 0 {
        match_prop::SYNTHESIZED_MARK
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(ch: char) -> u16 {
        initial(ch, crate::shape::normalize::mark_props(ch).0)
    }

    #[test]
    fn marks_hidden_characters_and_bases() {
        assert_eq!(props('\u{0301}'), match_prop::SYNTHESIZED_MARK);
        // Spacing marks are base glyphs to HarfBuzz's synthesis.
        assert_eq!(props('\u{0903}'), 0);
        assert_eq!(props('a'), 0);
        // Variation selectors are Mn but default ignorable.
        assert_eq!(props('\u{FE0F}'), 0);
        for hidden in ['\u{034F}', '\u{180B}', '\u{180F}', '\u{E0041}'] {
            assert_eq!(props(hidden), match_prop::HIDDEN, "{hidden:?}");
        }
        assert_eq!(props('\u{200D}'), 0);
    }

    #[test]
    fn inserted_characters_take_the_class_they_copy() {
        // A dotted circle with the General_Category of a nonspacing
        // vowel sign is a mark, as in `hb_synthesize_glyph_classes`.
        let nonspacing = char_class::MARK | char_class::NONSPACING_MARK;
        assert_eq!(
            initial('\u{25CC}', nonspacing),
            match_prop::SYNTHESIZED_MARK
        );
        assert_eq!(initial('\u{25CC}', char_class::MARK), 0);
        assert_eq!(props('\u{25CC}'), 0);
    }
}
