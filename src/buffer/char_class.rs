//! Bits packed into [`Glyph::char_class`](super::Glyph::char_class):
//! the parts of the source character's General_Category that shaping
//! reads after the cmap lookup (HarfBuzz keeps them in its
//! `unicode_props`).

/// The source character is a mark (General_Category Mn, Mc, or Me).
pub const MARK: u8 = 1 << 0;
/// The source character is a nonspacing mark (Mn).
pub const NONSPACING_MARK: u8 = 1 << 1;
