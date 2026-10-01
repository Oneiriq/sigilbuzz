//! Glyph flags, HarfBuzz's `hb_glyph_flags_t`: what a shaped glyph
//! says about breaking or joining the text at its cluster.

use core::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign};

/// Flags shaping sets on a [`Glyph`](super::Glyph), HarfBuzz's
/// `hb_glyph_flags_t` as `hb_glyph_info_get_glyph_flags` reports them.
///
/// Every glyph of a cluster carries the same flags. The values are
/// HarfBuzz's, so [`Self::bits`] can be handed to or taken from
/// HarfBuzz code unchanged.
///
/// - [`Self::UNSAFE_TO_BREAK`]: breaking the text at the start of
///   this glyph's cluster and shaping the two sides separately gives
///   different glyphs or positions, so a line breaker must reshape.
/// - [`Self::UNSAFE_TO_CONCAT`]: the glyphs at the start of this
///   cluster depend on the text before it, so text shaped separately
///   may not be joined here without reshaping. Only produced when the
///   buffer has [`BufferFlags::PRODUCE_UNSAFE_TO_CONCAT`]. Every glyph
///   unsafe to break is also unsafe to concatenate.
/// - [`Self::SAFE_TO_INSERT_TATWEEL`]: a tatweel (U+0640) may be
///   inserted before this cluster to elongate the text. Only produced
///   when the buffer has
///   [`BufferFlags::PRODUCE_SAFE_TO_INSERT_TATWEEL`].
///
/// [`BufferFlags::PRODUCE_UNSAFE_TO_CONCAT`]: super::BufferFlags::PRODUCE_UNSAFE_TO_CONCAT
/// [`BufferFlags::PRODUCE_SAFE_TO_INSERT_TATWEEL`]: super::BufferFlags::PRODUCE_SAFE_TO_INSERT_TATWEEL
///
/// # Examples
///
/// ```
/// use sigilbuzz::GlyphFlags;
///
/// let flags = GlyphFlags::UNSAFE_TO_BREAK | GlyphFlags::UNSAFE_TO_CONCAT;
/// assert!(flags.contains(GlyphFlags::UNSAFE_TO_BREAK));
/// assert_eq!(flags.bits(), 0x3);
/// assert!(GlyphFlags::default().is_empty());
/// ```
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphFlags(u32);

impl GlyphFlags {
    /// `HB_GLYPH_FLAG_UNSAFE_TO_BREAK`.
    pub const UNSAFE_TO_BREAK: Self = Self(0x1);
    /// `HB_GLYPH_FLAG_UNSAFE_TO_CONCAT`.
    pub const UNSAFE_TO_CONCAT: Self = Self(0x2);
    /// `HB_GLYPH_FLAG_SAFE_TO_INSERT_TATWEEL`.
    pub const SAFE_TO_INSERT_TATWEEL: Self = Self(0x4);

    /// Every flag, `HB_GLYPH_FLAG_DEFINED`.
    const DEFINED: u32 = 0x7;

    /// No flags.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Every flag.
    #[must_use]
    pub const fn all() -> Self {
        Self(Self::DEFINED)
    }

    /// The raw bits, HarfBuzz's values.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// The flags for `bits`, dropping bits HarfBuzz does not define.
    ///
    /// ```
    /// use sigilbuzz::GlyphFlags;
    ///
    /// assert_eq!(GlyphFlags::from_bits_truncate(0x11), GlyphFlags::UNSAFE_TO_BREAK);
    /// ```
    #[must_use]
    pub const fn from_bits_truncate(bits: u32) -> Self {
        Self(bits & Self::DEFINED)
    }

    /// True when no flag is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// True when every flag of `other` is set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// True when `self` and `other` share at least one flag.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// The flags set in either.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The flags set in both.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// The flags of `self` that `other` does not set.
    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Sets the flags of `other`.
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// Clears the flags of `other`.
    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

impl BitOr for GlyphFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl BitOrAssign for GlyphFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.insert(rhs);
    }
}

impl BitAnd for GlyphFlags {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self {
        self.intersection(rhs)
    }
}

impl BitAndAssign for GlyphFlags {
    fn bitand_assign(&mut self, rhs: Self) {
        *self = self.intersection(rhs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_values_are_harfbuzz_values() {
        assert_eq!(GlyphFlags::UNSAFE_TO_BREAK.bits(), 1);
        assert_eq!(GlyphFlags::UNSAFE_TO_CONCAT.bits(), 2);
        assert_eq!(GlyphFlags::SAFE_TO_INSERT_TATWEEL.bits(), 4);
        assert_eq!(GlyphFlags::all().bits(), 7);
        assert_eq!(GlyphFlags::from_bits_truncate(u32::MAX), GlyphFlags::all());
    }

    #[test]
    fn set_operations_combine_flags() {
        let mut f = GlyphFlags::empty();
        f |= GlyphFlags::UNSAFE_TO_CONCAT;
        f.insert(GlyphFlags::UNSAFE_TO_BREAK);
        assert!(f.contains(GlyphFlags::UNSAFE_TO_BREAK | GlyphFlags::UNSAFE_TO_CONCAT));
        f.remove(GlyphFlags::UNSAFE_TO_BREAK);
        assert_eq!(f, GlyphFlags::UNSAFE_TO_CONCAT);
        f &= GlyphFlags::UNSAFE_TO_BREAK;
        assert!(f.is_empty());
        let both = GlyphFlags::UNSAFE_TO_BREAK | GlyphFlags::SAFE_TO_INSERT_TATWEEL;
        assert_eq!(
            both.difference(GlyphFlags::UNSAFE_TO_BREAK),
            GlyphFlags::SAFE_TO_INSERT_TATWEEL
        );
        assert!(both.intersects(GlyphFlags::SAFE_TO_INSERT_TATWEEL));
        assert_eq!(
            both & GlyphFlags::UNSAFE_TO_BREAK,
            GlyphFlags::UNSAFE_TO_BREAK
        );
    }
}
