//! Tibetan entry point.
//!
//! Tibetan (U+0F00..U+0FFF) stacks subjoined consonants
//! (U+0F90..U+0FBC) below the base and attaches vowel signs
//! (U+0F71..U+0F84) above or below. HarfBuzz shapes it with the
//! Universal Shaping Engine, and so does sigilbuzz
//! ([`crate::ot::use_shaper`]): the USE category table gives the
//! subjoined consonants and vowel signs their positions, the USE
//! syllable machine finds the clusters, and the USE feature stages run
//! `ccmp`, `abvs`, `blws`, and the default features.
//!
//! # Script tag
//!
//! Tibetan fonts register their features under the OpenType script
//! tag `tibt`. There is no Indic2-style new-tag variant for
//! Tibetan (`tibt` covers both old and new builds), so the
//! priority list is `[tibt, DFLT]`.

use alloc::vec::Vec;

use crate::buffer::{ClusterLevel, Glyph};
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;

/// Script-tag priority for Tibetan GSUB / GPOS feature lookup.
///
/// Tibetan fonts register their lookups under `tibt` (no Indic2-era
/// renamed tag exists for Tibetan). DFLT is the universal fallback
/// for fonts that only carry features in the default LangSys.
pub const TIBT_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"tibt", *b"DFLT"];

/// True for every codepoint that is part of the Tibetan block.
/// Mirrors [`crate::unicode::script_of`]'s Tibetan arm so the
/// dispatcher can use it as a fast pre-filter without going through
/// the full script classifier.
#[must_use]
pub const fn is_tibetan(ch: char) -> bool {
    matches!(ch as u32, 0x0F00..=0x0FFF)
}

/// Entry point: shapes one Tibetan run with the Universal Shaping
/// Engine, as HarfBuzz does, every GSUB feature included.
/// `codepoints` and `glyphs` are 1:1 on entry. After the call `glyphs`
/// may have shrunk (ligature) or grown (multiple substitution).
/// Clusters merge at the monotone characters level.
pub fn shape_tibetan(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    crate::ot::use_shaper::shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        TIBT_SCRIPT_PRIORITY,
        ClusterLevel::MonotoneCharacters,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_tibetan_covers_block() {
        assert!(is_tibetan('\u{0F00}')); // OM (block start)
        assert!(is_tibetan('\u{0F40}')); // KA (consonant)
        assert!(is_tibetan('\u{0F90}')); // subjoined KA
        assert!(is_tibetan('\u{0FBC}')); // subjoined RANGS
        assert!(is_tibetan('\u{0FFF}')); // block end
        assert!(!is_tibetan('\u{0EFF}')); // just below block
        assert!(!is_tibetan('\u{1000}')); // just above block (Myanmar)
        assert!(!is_tibetan('A'));
    }

    #[test]
    fn script_priority_is_tibt_then_dflt() {
        assert_eq!(TIBT_SCRIPT_PRIORITY, &[*b"tibt", *b"DFLT"]);
    }

    #[test]
    fn shape_with_no_gsub_keeps_the_glyphs() {
        // Without a GSUB table the glyphs stay as they are.
        let cps: Vec<char> = "\u{0F40}\u{0F90}".chars().collect();
        let mut glyphs = alloc::vec![Glyph::new(1, 0), Glyph::new(2, 3)];
        shape_tibetan(None, None, &cps, &mut glyphs);
        let ids: Vec<u32> = glyphs.iter().map(|g| g.glyph_id).collect();
        assert_eq!(ids, [1, 2]);
    }

    #[test]
    fn empty_input_is_a_noop() {
        let mut glyphs: Vec<Glyph> = alloc::vec![];
        shape_tibetan(None, None, &[], &mut glyphs);
        assert!(glyphs.is_empty());
    }
}
