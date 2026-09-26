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
//!   else a base glyph. The class only matters for fonts without a
//!   GDEF `GlyphClassDef`; GSUB updates it as it ligates and expands
//!   glyphs (see the `lig` module).
//!
//! HarfBuzz also un-hides a COMBINING GRAPHEME JOINER when it did not
//! block any mark reordering during normalization; sigilbuzz does not
//! reorder marks yet, so a CGJ stays hidden.

use super::ignorables;
use crate::buffer::Glyph;
use crate::tables::layout::skip_iter::{match_prop, MatchGlyph};
use crate::unicode::general_category::is_nonspacing_mark;

impl From<&Glyph> for MatchGlyph {
    fn from(g: &Glyph) -> Self {
        MatchGlyph::with_props(g.glyph_id as u16, g.unicode_props)
    }
}

/// The matching props a glyph mapped from `ch` starts with, on top of
/// the default-ignorable and joiner bits.
pub(super) fn initial(ch: char) -> u16 {
    if ignorables::is_default_ignorable(ch) {
        // Never a mark, so that lookups skipping marks do not skip
        // them; some of these are hidden.
        if matches!(ch as u32, 0x034F | 0x180B..=0x180D | 0x180F | 0xE0020..=0xE007F) {
            match_prop::HIDDEN
        } else {
            0
        }
    } else if is_nonspacing_mark(ch) {
        match_prop::SYNTHESIZED_MARK
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_hidden_characters_and_bases() {
        assert_eq!(initial('\u{0301}'), match_prop::SYNTHESIZED_MARK);
        // Spacing marks are base glyphs to HarfBuzz's synthesis.
        assert_eq!(initial('\u{0903}'), 0);
        assert_eq!(initial('a'), 0);
        // Variation selectors are Mn but default ignorable.
        assert_eq!(initial('\u{FE0F}'), 0);
        for hidden in ['\u{034F}', '\u{180B}', '\u{180F}', '\u{E0041}'] {
            assert_eq!(initial(hidden), match_prop::HIDDEN, "{hidden:?}");
        }
        assert_eq!(initial('\u{200D}'), 0);
    }
}
