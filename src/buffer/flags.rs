//! Buffer flags, HarfBuzz's `hb_buffer_flags_t`.
//!
//! Flags are a buffer setting rather than content: like HarfBuzz's
//! `hb_buffer_clear_contents`, [`Buffer::clear`] keeps them.

use core::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Sub, SubAssign};

use super::Buffer;

/// Shaping flags for a [`Buffer`], HarfBuzz's `hb_buffer_flags_t`.
///
/// A set of bits combined with `|`. The values are HarfBuzz's, so
/// [`Self::bits`] can be handed to or taken from HarfBuzz code
/// unchanged. Only the flags sigilbuzz honors are defined; HarfBuzz's
/// `VERIFY`, `PRODUCE_UNSAFE_TO_CONCAT`, and
/// `PRODUCE_SAFE_TO_INSERT_TATWEEL` have no counterpart because
/// sigilbuzz produces no glyph flags.
///
/// # Examples
///
/// ```
/// use sigilbuzz::{Buffer, BufferFlags};
///
/// let mut buffer = Buffer::new();
/// assert_eq!(buffer.flags(), BufferFlags::DEFAULT);
/// buffer.set_flags(BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE | BufferFlags::EOT);
/// assert!(buffer.flags().contains(BufferFlags::EOT));
/// assert_eq!(buffer.flags().bits(), 0x12);
/// ```
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferFlags(u32);

impl BufferFlags {
    /// No flags, HarfBuzz's `HB_BUFFER_FLAG_DEFAULT`.
    pub const DEFAULT: Self = Self(0);
    /// The text ends a paragraph (`HB_BUFFER_FLAG_EOT`). HarfBuzz's
    /// OpenType shaper reads no end-of-text state, so this flag is
    /// kept for callers but changes nothing, there as here.
    pub const EOT: Self = Self(0x02);
    /// Never insert U+25CC DOTTED CIRCLE for a broken Indic, Khmer,
    /// Myanmar, or USE syllable
    /// (`HB_BUFFER_FLAG_DO_NOT_INSERT_DOTTED_CIRCLE`). Useful when the
    /// run continues text shaped earlier.
    pub const DO_NOT_INSERT_DOTTED_CIRCLE: Self = Self(0x10);

    /// Every flag sigilbuzz defines.
    const KNOWN: u32 = 0x12;

    /// The empty set, same as [`Self::DEFAULT`].
    #[must_use]
    pub const fn empty() -> Self {
        Self::DEFAULT
    }

    /// Every flag sigilbuzz defines.
    #[must_use]
    pub const fn all() -> Self {
        Self(Self::KNOWN)
    }

    /// The raw bits, HarfBuzz's values.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// The flags for `bits`, or `None` when `bits` holds a bit
    /// sigilbuzz does not define.
    ///
    /// ```
    /// use sigilbuzz::BufferFlags;
    ///
    /// assert_eq!(BufferFlags::from_bits(0x10), Some(BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE));
    /// assert_eq!(BufferFlags::from_bits(0x20), None);
    /// ```
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits & !Self::KNOWN == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    /// The flags for `bits`, dropping the bits sigilbuzz does not
    /// define (HarfBuzz's `VERIFY` and glyph-flag requests among them).
    ///
    /// ```
    /// use sigilbuzz::BufferFlags;
    ///
    /// assert_eq!(BufferFlags::from_bits_truncate(0x22), BufferFlags::EOT);
    /// ```
    #[must_use]
    pub const fn from_bits_truncate(bits: u32) -> Self {
        Self(bits & Self::KNOWN)
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

    /// Sets (`value` true) or clears the flags of `other`.
    ///
    /// ```
    /// use sigilbuzz::BufferFlags;
    ///
    /// let mut flags = BufferFlags::EOT;
    /// flags.set(BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE, true);
    /// flags.set(BufferFlags::EOT, false);
    /// assert_eq!(flags, BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE);
    /// ```
    pub fn set(&mut self, other: Self, value: bool) {
        if value {
            self.insert(other);
        } else {
            self.remove(other);
        }
    }
}

impl BitOr for BufferFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl BitOrAssign for BufferFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.insert(rhs);
    }
}

impl BitAnd for BufferFlags {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self {
        self.intersection(rhs)
    }
}

impl BitAndAssign for BufferFlags {
    fn bitand_assign(&mut self, rhs: Self) {
        *self = self.intersection(rhs);
    }
}

impl Sub for BufferFlags {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        self.difference(rhs)
    }
}

impl SubAssign for BufferFlags {
    fn sub_assign(&mut self, rhs: Self) {
        self.remove(rhs);
    }
}

impl Buffer {
    /// The shaping flags set with [`Self::set_flags`];
    /// [`BufferFlags::DEFAULT`] until then.
    #[must_use]
    pub const fn flags(&self) -> BufferFlags {
        self.flags
    }

    /// Sets the shaping flags, HarfBuzz's `hb_buffer_set_flags`.
    ///
    /// The flags are a setting, not content: they survive
    /// [`Self::clear`], as HarfBuzz's survive `hb_buffer_clear_contents`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::{Buffer, BufferFlags};
    ///
    /// let mut buffer = Buffer::new();
    /// buffer.push_str("\u{093F}");
    /// buffer.set_flags(BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE);
    /// buffer.clear();
    /// assert_eq!(buffer.flags(), BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE);
    /// ```
    pub fn set_flags(&mut self, flags: BufferFlags) {
        self.flags = flags;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_values_match_harfbuzz() {
        assert_eq!(BufferFlags::DEFAULT.bits(), 0x00);
        assert_eq!(BufferFlags::EOT.bits(), 0x02);
        assert_eq!(BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE.bits(), 0x10);
        assert_eq!(BufferFlags::all().bits(), 0x12);
    }

    #[test]
    fn flag_set_operations() {
        let circle = BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE;
        let mut f = BufferFlags::empty();
        assert!(f.is_empty());
        f |= circle;
        f.insert(BufferFlags::EOT);
        assert!(f.contains(circle | BufferFlags::EOT));
        assert!(f.intersects(BufferFlags::EOT));
        assert_eq!(f - circle, BufferFlags::EOT);
        assert_eq!(f & circle, circle);
        f -= BufferFlags::EOT;
        assert_eq!(f, circle);
        f &= BufferFlags::EOT;
        assert!(f.is_empty());
        assert_eq!(BufferFlags::from_bits(0x12), Some(BufferFlags::all()));
        assert_eq!(BufferFlags::from_bits(0x40), None);
        assert_eq!(BufferFlags::from_bits_truncate(0xFF), BufferFlags::all());
    }

    #[test]
    fn flags_survive_clear() {
        let mut b = Buffer::new();
        assert_eq!(b.flags(), BufferFlags::DEFAULT);
        b.set_flags(BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE | BufferFlags::EOT);
        b.push_str("abc");
        b.clear();
        assert_eq!(
            b.flags(),
            BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE | BufferFlags::EOT
        );
    }
}
