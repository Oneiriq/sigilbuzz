//! Character mirroring for backward runs, HarfBuzz's
//! `hb_ot_rotate_chars` (`hb-ot-shape.cc`).
//!
//! In a right-to-left (or bottom-to-top) run, a character with a
//! `Bidi_Mirroring_Glyph` is replaced by that mirror character when the
//! font maps it, so `(` draws as `)`. Every other glyph of the run is
//! handed to the font's `rtlm` (right-to-left mirrored forms) feature
//! instead, which covers the Bidi_Mirrored characters that have no
//! mirror character (such as U+2211 N-ARY SUMMATION) and fonts that
//! draw mirrored forms themselves. HarfBuzz enables `rtlm` only on
//! those glyphs, through a feature mask; sigilbuzz runs it with a
//! per-glyph mask at the start of the segment's GSUB, right after any
//! required feature, which is the stage HarfBuzz gives it.

use alloc::vec::Vec;

use super::{apply_gsub_feature_masked, feature_disabled, Feature};
use crate::buffer::Glyph;
use crate::tables::cmap::Cmap;
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;
use crate::unicode::mirroring::bidi_mirroring_glyph;

/// `ch` as a backward run maps it: its mirror character when it has
/// one the font can draw, and whether it was replaced.
pub(super) fn mirror(ch: char, cmap: &Cmap<'_>) -> (char, bool) {
    match bidi_mirroring_glyph(ch) {
        Some(m) if cmap.glyph_id(m).is_some() => (m, true),
        _ => (ch, false),
    }
}

/// Applies `rtlm` to the glyphs of a segment whose code points were
/// not replaced by their mirror. `range` is the segment's range in the
/// run's code point list and `mirrored` the sorted indices, in that
/// list, of the replaced code points. Glyphs must still be one per
/// code point.
pub(super) fn apply_rtlm(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    script_priority: &[[u8; 4]],
    features: &[Feature],
    range: core::ops::Range<usize>,
    mirrored: &[usize],
) {
    if feature_disabled(features, *b"rtlm") {
        return;
    }
    let mask: Vec<bool> = range.map(|i| mirrored.binary_search(&i).is_err()).collect();
    apply_gsub_feature_masked(gsub, glyphs, gdef, *b"rtlm", script_priority, &mask);
}
