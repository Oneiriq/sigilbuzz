//! Tibetan script shaper.
//!
//! Tibetan (U+0F00..U+0FFF) is structurally a simplified Indic script:
//! the run reads phonetically, subjoined consonants (U+0F90..U+0FBC)
//! attach below the base, and vowel signs (U+0F71..U+0F84) attach
//! above or below. Unlike Devanagari, Tibetan has no reph reorder,
//! no pre-base matras to move, and no split matras. The shaper
//! reduces to a feature loop in the right order.
//!
//! # GSUB feature order
//!
//! Per the Microsoft Tibetan shaping doc, the order is:
//!
//! ```text
//!   ccmp → abvs → blws → calt → liga
//! ```
//!
//! - `ccmp` — glyph composition / decomposition (e.g. precomposed
//!   stacks vs. base + subjoined sequences).
//! - `abvs` — above-base substitution. Selects the form of
//!   above-base vowel signs / marks given the base they ride.
//! - `blws` — below-base substitution. Selects the subjoined
//!   consonant variant for U+0F90..U+0FBC. The bulk of Tibetan
//!   stacking happens here.
//! - `calt` — contextual alternates. Tibetan fonts use this for
//!   vowel-sign positioning shims and for some punctuation
//!   variants.
//! - `liga` — standard ligatures. Optional in Tibetan but kept in
//!   the chain so a font that ships `liga` lookups under `tibt`
//!   still gets them.
//!
//! After this pass the shaper hands the glyph run back to the
//! generic GPOS pipeline in [`crate::shape`] for mark attachment
//! and (rare) kerning.
//!
//! # Why no reordering
//!
//! Tibetan's logical order matches its visual order. A syllable
//! like ཀྱ (KA + subjoined YA) is encoded U+0F40 U+0FA1, the base
//! sits left and the subjoined letter directly under — same as
//! the encoding order. That means the feature pass runs straight
//! over the run with no pre-pass; a per-syllable splitter would
//! cost more than it buys.
//!
//! # Script tag
//!
//! Tibetan fonts register their features under the OpenType script
//! tag `tibt`. There is no Indic2-style new-tag variant for
//! Tibetan — `tibt` covers both old and new builds — so the
//! priority list is `[tibt, DFLT]`.

use alloc::vec::Vec;

use crate::buffer::Glyph;
use crate::shape::apply_gsub_feature_in_scripts;
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;

/// Script-tag priority for Tibetan GSUB / GPOS feature lookup.
///
/// Tibetan fonts register their lookups under `tibt` (no Indic2-era
/// renamed tag exists for Tibetan); DFLT is the universal fallback
/// for fonts that only carry features in the default LangSys.
pub const TIBT_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"tibt", *b"DFLT"];

/// Tibetan script-specific feature chain.
///
/// Only `abvs` and `blws` need the Tibetan shaper to drive them —
/// the above-base substitution runs first so vowel-sign forms
/// settle before the below-base subjoined consonant pass picks the
/// stacked variants.
///
/// `ccmp`, `calt`, and `liga` are intentionally not listed here —
/// the generic [`crate::shape`] default-GSUB pass applies them
/// (under the segment's `tibt`/DFLT priority) after this shaper
/// returns, matching how the Indic shaper interleaves with the
/// default pass. Listing them inside the shaper would double-apply
/// on fonts whose lookups are not idempotent.
pub const TIBT_FEATURES: &[&[u8; 4]] = &[b"abvs", b"blws"];

/// True for every codepoint that is part of the Tibetan block.
/// Mirrors [`crate::unicode::script_of`]'s Tibetan arm so the
/// dispatcher can use it as a fast pre-filter without going through
/// the full script classifier.
#[must_use]
pub const fn is_tibetan(ch: char) -> bool {
    matches!(ch as u32, 0x0F00..=0x0FFF)
}

/// Entry point — applies the Tibetan feature chain to one segment
/// of `glyphs`. `codepoints` and `glyphs` are 1:1 on entry; after
/// the call `glyphs` may have shrunk (ligature) or grown (multiple
/// substitution).
///
/// Caller (the segment dispatcher in [`crate::shape`]) is
/// responsible for slicing the buffer to a Tibetan-only segment so
/// non-Tibetan glyphs can never serve as context for these
/// lookups.
pub fn shape_tibetan(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    if codepoints.is_empty() || glyphs.is_empty() {
        return;
    }
    // No reordering pass — Tibetan's logical order matches visual.
    // Run the feature chain in the documented order. Each call is a
    // no-op when the font does not advertise that feature under
    // `tibt` / DFLT.
    let Some(gsub) = gsub else {
        return;
    };
    for tag in TIBT_FEATURES {
        apply_gsub_feature_in_scripts(gsub, glyphs, gdef, **tag, 0, TIBT_SCRIPT_PRIORITY);
    }
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
    fn feature_chain_order_matches_spec() {
        // Spec order from the Microsoft Tibetan shaping doc, minus
        // `ccmp`/`calt`/`liga` which the generic default-GSUB pass
        // already runs.
        let tags: Vec<[u8; 4]> = TIBT_FEATURES.iter().map(|t| **t).collect();
        assert_eq!(tags, alloc::vec![*b"abvs", *b"blws"]);
    }

    #[test]
    fn shape_with_no_gsub_is_a_noop() {
        // Without a GSUB table the shaper should leave glyphs alone.
        let cps: Vec<char> = "\u{0F40}\u{0F90}".chars().collect();
        let mut glyphs = alloc::vec![Glyph::new(1, 0), Glyph::new(2, 3)];
        let original = glyphs.clone();
        shape_tibetan(None, None, &cps, &mut glyphs);
        assert_eq!(glyphs, original);
    }

    #[test]
    fn empty_input_is_a_noop() {
        let mut glyphs: Vec<Glyph> = alloc::vec![];
        shape_tibetan(None, None, &[], &mut glyphs);
        assert!(glyphs.is_empty());
    }
}
