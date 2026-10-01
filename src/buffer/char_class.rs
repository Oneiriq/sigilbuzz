//! Bits packed into [`Glyph::char_class`](super::Glyph::char_class):
//! the parts of the source character's General_Category that shaping
//! reads after the cmap lookup (HarfBuzz keeps them in its
//! `unicode_props`).

/// The source character is a mark (General_Category Mn, Mc, or Me).
pub const MARK: u8 = 1 << 0;
/// The source character is a nonspacing mark (Mn).
pub const NONSPACING_MARK: u8 = 1 << 1;
/// Shift of the fallback space kind in the upper five bits: nonzero
/// when normalization drew a space character the font does not map
/// (U+2002 EN SPACE, U+202F NARROW NO-BREAK SPACE, ...) with the space
/// glyph, telling positioning which width to give it (HarfBuzz's space
/// fallback type).
pub const SPACE_SHIFT: u32 = 3;
/// The source character is a variation selector right after a base
/// character, and the font has no glyph for the pair. HarfBuzz gives it
/// General_Category Cf while it substitutes and positions, so it is no
/// mark then, and afterwards swaps in the buffer's not-found variation
/// selector glyph when one is set.
pub const UNRESOLVED_SELECTOR: u8 = 1 << 2;
