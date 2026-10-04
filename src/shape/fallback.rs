//! HarfBuzz's fallback positioning (`hb-ot-shape-fallback.cc`,
//! HarfBuzz 14.5.0).
//!
//! Space characters the font does not map are drawn with the space
//! glyph by normalization (`_hb_ot_shape_normalize`), which records
//! what kind of space each was; [`adjust_spaces`] then gives each its
//! own width (`_hb_ot_shape_fallback_spaces`): a fraction of the em for
//! the em-based spaces, the width of a digit for U+2007 FIGURE SPACE,
//! of a period for U+2008 PUNCTUATION SPACE, and half the space for
//! U+202F NARROW NO-BREAK SPACE.
//!
//! When nothing in the font positions a run's marks, the `marks` child
//! places them from their combining classes and the glyphs' ink
//! extents (`_hb_ot_shape_fallback_mark_position`).

mod extents;
mod marks;

pub(super) use extents::{Extents, ExtentsTables};
pub(super) use marks::{recategorize_combining_class, MarkPositioner};

use super::position::FontAdvances;
use crate::buffer::{char_class, Glyph};
use crate::error::Result;
use crate::face::Face;

/// HarfBuzz's `hb_unicode_funcs_t::space_t`: what width a space
/// character drawn with the space glyph gets. The em-based kinds are
/// the em divisor.
pub(super) mod space {
    /// Not a space character with a fallback.
    pub(crate) const NOT_SPACE: u8 = 0;
    /// A full em (U+2001, U+2003, U+3000).
    pub(crate) const EM: u8 = 1;
    /// Half an em (U+2000, U+2002).
    pub(crate) const EM_2: u8 = 2;
    /// A third of an em (U+2004).
    pub(crate) const EM_3: u8 = 3;
    /// A quarter of an em (U+2005).
    pub(crate) const EM_4: u8 = 4;
    /// A fifth of an em (U+2009).
    pub(crate) const EM_5: u8 = 5;
    /// A sixth of an em (U+2006).
    pub(crate) const EM_6: u8 = 6;
    /// A sixteenth of an em (U+200A).
    pub(crate) const EM_16: u8 = 16;
    /// Four eighteenths of an em (U+205F).
    pub(crate) const EM_4_18: u8 = 17;
    /// The space itself (U+0020, U+00A0): keeps the space glyph's width.
    pub(crate) const SPACE: u8 = 18;
    /// The width of a digit (U+2007).
    pub(crate) const FIGURE: u8 = 19;
    /// The width of a period or comma (U+2008).
    pub(crate) const PUNCTUATION: u8 = 20;
    /// Half the space glyph's width (U+202F).
    pub(crate) const NARROW: u8 = 21;
}

/// The fallback kind of space character `ch`
/// (`hb_unicode_funcs_t::space_fallback_type`): every General_Category
/// Zs character except U+1680 OGHAM SPACE MARK, which draws ink.
pub(super) const fn space_fallback(ch: char) -> u8 {
    match ch {
        '\u{0020}' | '\u{00A0}' => space::SPACE,
        '\u{2000}' | '\u{2002}' => space::EM_2,
        '\u{2001}' | '\u{2003}' | '\u{3000}' => space::EM,
        '\u{2004}' => space::EM_3,
        '\u{2005}' => space::EM_4,
        '\u{2006}' => space::EM_6,
        '\u{2007}' => space::FIGURE,
        '\u{2008}' => space::PUNCTUATION,
        '\u{2009}' => space::EM_5,
        '\u{200A}' => space::EM_16,
        '\u{202F}' => space::NARROW,
        '\u{205F}' => space::EM_4_18,
        _ => space::NOT_SPACE,
    }
}

/// The fallback space kind normalization recorded on `g`.
pub(super) const fn space_kind(g: &Glyph) -> u8 {
    g.char_class >> char_class::SPACE_SHIFT
}

/// `_hb_ot_shape_fallback_spaces`: gives each space character drawn
/// with the space glyph the width of its kind. Runs on the default
/// advances, before GPOS; a space glyph that ligated keeps its advance.
/// `advances` are the shaping call's.
pub(super) fn adjust_spaces(
    face: &Face<'_>,
    advances: &FontAdvances<'_, '_>,
    glyphs: &mut [Glyph],
    horizontal: bool,
) -> Result<()> {
    if glyphs.iter().all(|g| space_kind(g) == space::NOT_SPACE) {
        return Ok(());
    }
    let upem = i32::from(face.head()?.units_per_em);
    let cmap = face.cmap()?;
    let advance = |gid: u16| advance(advances, gid, horizontal);
    // The digit and punctuation widths, looked up once.
    let figure = ('0'..='9').find_map(|c| cmap.glyph_id(c));
    let punctuation = cmap.glyph_id('.').or_else(|| cmap.glyph_id(','));
    for g in glyphs {
        let kind = space_kind(g);
        if kind == space::NOT_SPACE || super::lig::is_ligated(g) {
            continue;
        }
        let length = match kind {
            space::EM
            | space::EM_2
            | space::EM_3
            | space::EM_4
            | space::EM_5
            | space::EM_6
            | space::EM_16 => {
                let kind = i32::from(kind);
                Some((upem + kind / 2) / kind)
            }
            space::EM_4_18 => Some(upem * 4 / 18),
            space::FIGURE => figure.map(advance),
            space::PUNCTUATION => punctuation.map(advance),
            space::NARROW => {
                if horizontal {
                    g.x_advance /= 2;
                } else {
                    g.y_advance /= 2;
                }
                None
            }
            _ => None,
        };
        if let Some(length) = length {
            if horizontal {
                g.x_advance = length;
            } else {
                g.y_advance = -length;
            }
        }
    }
    Ok(())
}

/// The advance of glyph `gid` along the run, as the default positioning
/// computes it: `hmtx` (plus `HVAR`) horizontally, `vmtx` (plus `VVAR`)
/// or the ascender-to-descender height vertically, with the varied
/// phantom points of a `glyf` font standing in for a missing `HVAR` or
/// `VVAR` (see [`FontAdvances`]). Never negative.
fn advance(advances: &FontAdvances<'_, '_>, gid: u16, horizontal: bool) -> i32 {
    if horizontal {
        advances.h_advance(u32::from(gid))
    } else {
        advances.v_advance(u32::from(gid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fallback_space_is_a_space_separator() {
        use crate::unicode::general_category::general_category_class;
        for cp in 0..=0x3000u32 {
            let Some(ch) = char::from_u32(cp) else {
                continue;
            };
            if space_fallback(ch) != space::NOT_SPACE {
                // Zs characters have no General_Category class the
                // coarse table keeps.
                assert_eq!(general_category_class(ch), None, "{ch:?}");
            }
        }
        assert_eq!(space_fallback('\u{1680}'), space::NOT_SPACE);
        assert_eq!(space_fallback('\u{2003}'), space::EM);
        assert_eq!(space_fallback('\u{202F}'), space::NARROW);
    }

    #[test]
    fn the_kind_rides_in_the_char_class_bits() {
        let mut g = Glyph::new(3, 0);
        assert_eq!(space_kind(&g), space::NOT_SPACE);
        g.char_class = space::FIGURE << char_class::SPACE_SHIFT;
        assert_eq!(space_kind(&g), space::FIGURE);
    }
}
