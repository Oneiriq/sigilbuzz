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
use crate::buffer::{Direction, Glyph};
use crate::tables::cmap::Cmap;
use crate::tables::gdef::Gdef;
use crate::tables::layout::Joiners;
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

/// The direction features HarfBuzz enables for a run laid out in
/// `direction` (`hb_ot_shape_collect_features`): `ltra` and `ltrm` for
/// left to right, `rtla` for right to left (its `rtlm` only reaches
/// the glyphs [`apply_rtlm`] masks in), none for vertical runs. They
/// run in the stage right after any required feature, before every
/// shaper's own features.
pub(super) const fn direction_features(direction: Direction) -> &'static [[u8; 4]] {
    const LTR: &[[u8; 4]] = &[*b"ltra", *b"ltrm"];
    const RTL: &[[u8; 4]] = &[*b"rtla"];
    match direction {
        Direction::Ltr => LTR,
        Direction::Rtl => RTL,
        Direction::Ttb | Direction::Btt => &[],
    }
}

/// Applies `rtlm` to the glyphs of a segment whose code points were
/// not replaced by their mirror; `mirrored` says, per code point of
/// the segment, whether it was. Glyphs must still be one per code
/// point.
pub(super) fn apply_rtlm(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    script_priority: &[[u8; 4]],
    features: &[Feature],
    mirrored: &[bool],
) {
    if feature_disabled(features, *b"rtlm") {
        return;
    }
    let mask: Vec<bool> = mirrored.iter().map(|m| !m).collect();
    let joiners = Joiners::AUTO;
    apply_gsub_feature_masked(
        gsub,
        glyphs,
        gdef,
        *b"rtlm",
        script_priority,
        &mask,
        joiners,
    );
}
