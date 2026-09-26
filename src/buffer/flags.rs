//! Buffer flags and cluster levels, HarfBuzz's `hb_buffer_flags_t`
//! and `hb_buffer_cluster_level_t`.
//!
//! Both are buffer settings rather than content: like HarfBuzz's
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
/// buffer.set_flags(BufferFlags::BOT | BufferFlags::EOT);
/// assert!(buffer.flags().contains(BufferFlags::BOT));
/// assert_eq!(buffer.flags().bits(), 0x3);
/// ```
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferFlags(u32);

impl BufferFlags {
    /// No flags, HarfBuzz's `HB_BUFFER_FLAG_DEFAULT`.
    pub const DEFAULT: Self = Self(0);
    /// The text starts a paragraph (`HB_BUFFER_FLAG_BOT`). With it, and
    /// no pre-context, a combining mark at the very start of the text
    /// gets a U+25CC DOTTED CIRCLE to sit on, unless
    /// [`Self::DO_NOT_INSERT_DOTTED_CIRCLE`] is also set or the font has
    /// no glyph for U+25CC.
    pub const BOT: Self = Self(0x01);
    /// The text ends a paragraph (`HB_BUFFER_FLAG_EOT`). HarfBuzz's
    /// OpenType shaper reads no end-of-text state, so this flag is
    /// kept for callers but changes nothing, there as here.
    pub const EOT: Self = Self(0x02);
    /// Default-ignorable characters (ZWJ, variation selectors, bidi
    /// controls, ...) keep the font's glyph and its advance instead of
    /// being hidden (`HB_BUFFER_FLAG_PRESERVE_DEFAULT_IGNORABLES`).
    /// Takes precedence over [`Self::REMOVE_DEFAULT_IGNORABLES`].
    pub const PRESERVE_DEFAULT_IGNORABLES: Self = Self(0x04);
    /// Default-ignorable characters are deleted from the output, their
    /// clusters merged into a neighbor, instead of being drawn as an
    /// invisible zero-width space glyph
    /// (`HB_BUFFER_FLAG_REMOVE_DEFAULT_IGNORABLES`).
    pub const REMOVE_DEFAULT_IGNORABLES: Self = Self(0x08);
    /// Never insert U+25CC DOTTED CIRCLE, neither for a broken Indic,
    /// Khmer, Myanmar, or USE syllable nor at the start of the text
    /// (`HB_BUFFER_FLAG_DO_NOT_INSERT_DOTTED_CIRCLE`). Useful when the
    /// run continues text shaped earlier.
    pub const DO_NOT_INSERT_DOTTED_CIRCLE: Self = Self(0x10);

    /// Every flag sigilbuzz defines.
    const KNOWN: u32 = 0x1F;

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
    /// assert_eq!(BufferFlags::from_bits_truncate(0x21), BufferFlags::BOT);
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
    /// let mut flags = BufferFlags::BOT;
    /// flags.set(BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE, true);
    /// flags.set(BufferFlags::BOT, false);
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

/// How shaping groups the input characters into clusters,
/// HarfBuzz's `hb_buffer_cluster_level_t`.
///
/// Every glyph's [`crate::Glyph::cluster`] is the offset of an input
/// character. The level decides which characters share one:
///
/// - Grapheme levels ([`Self::MonotoneGraphemes`], [`Self::Graphemes`])
///   first merge every character into the cluster of the base it
///   continues: combining marks, ZWJ and an emoji after it, emoji
///   modifiers, the second regional indicator of a flag, tag
///   characters, the halfwidth katakana sound marks.
/// - Monotone levels ([`Self::MonotoneGraphemes`],
///   [`Self::MonotoneCharacters`]) merge clusters whenever shaping
///   would otherwise take them out of order: a ligature takes its
///   components' smallest cluster, a reordered vowel sign shares the
///   cluster of the consonants it moved across, a deleted glyph's
///   cluster goes to its neighbor, and so on.
/// - [`Self::Characters`] does neither: characters keep their own
///   clusters and a reordered glyph keeps its own offset, so clusters
///   can come out of order.
///
/// HarfBuzz defaults to [`Self::MonotoneGraphemes`] (the C API does
/// too). A Rust [`Buffer`] defaults to [`Self::MonotoneCharacters`],
/// the level closest to what sigilbuzz produced before it supported
/// cluster levels.
///
/// # Examples
///
/// ```
/// use sigilbuzz::{Buffer, ClusterLevel};
///
/// let mut buffer = Buffer::new();
/// assert_eq!(buffer.cluster_level(), ClusterLevel::MonotoneCharacters);
/// buffer.set_cluster_level(ClusterLevel::MonotoneGraphemes);
/// assert!(buffer.cluster_level().is_monotone());
/// assert!(buffer.cluster_level().is_graphemes());
/// ```
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClusterLevel {
    /// Characters merge into their grapheme, and clusters stay in
    /// order (`HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES`, HarfBuzz's
    /// default).
    MonotoneGraphemes,
    /// Every character starts with its own cluster, and clusters stay
    /// in order (`HB_BUFFER_CLUSTER_LEVEL_MONOTONE_CHARACTERS`).
    #[default]
    MonotoneCharacters,
    /// Every character keeps its own cluster, in whatever order
    /// shaping leaves them (`HB_BUFFER_CLUSTER_LEVEL_CHARACTERS`).
    Characters,
    /// Characters merge into their grapheme, without forcing clusters
    /// into order (`HB_BUFFER_CLUSTER_LEVEL_GRAPHEMES`).
    Graphemes,
}

impl ClusterLevel {
    /// True when clusters are merged to stay in order,
    /// HarfBuzz's `HB_BUFFER_CLUSTER_LEVEL_IS_MONOTONE`.
    #[must_use]
    pub const fn is_monotone(self) -> bool {
        matches!(self, Self::MonotoneGraphemes | Self::MonotoneCharacters)
    }

    /// True when characters merge into their grapheme,
    /// HarfBuzz's `HB_BUFFER_CLUSTER_LEVEL_IS_GRAPHEMES`.
    #[must_use]
    pub const fn is_graphemes(self) -> bool {
        matches!(self, Self::MonotoneGraphemes | Self::Graphemes)
    }

    /// True when characters keep their own clusters,
    /// HarfBuzz's `HB_BUFFER_CLUSTER_LEVEL_IS_CHARACTERS`.
    #[must_use]
    pub const fn is_characters(self) -> bool {
        matches!(self, Self::MonotoneCharacters | Self::Characters)
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

    /// The cluster level set with [`Self::set_cluster_level`];
    /// [`ClusterLevel::MonotoneCharacters`] until then.
    #[must_use]
    pub const fn cluster_level(&self) -> ClusterLevel {
        self.cluster_level
    }

    /// Sets how shaping forms and merges clusters, HarfBuzz's
    /// `hb_buffer_set_cluster_level`; see [`ClusterLevel`]. Like the
    /// flags, the level survives [`Self::clear`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::{shape, Blob, Buffer, ClusterLevel, Face, Font};
    ///
    /// # let data = include_bytes!("../../tests/fixtures/opensans_regular.ttf");
    /// let blob = Blob::new(data);
    /// let font = Font::new(Face::parse(&blob, 0)?, 1000.0);
    /// let mut buffer = Buffer::new();
    /// buffer.push_str("x\u{0301}");
    ///
    /// // The combining acute keeps its own cluster ...
    /// let clusters = |b: &Buffer| -> Vec<u32> {
    ///     shape(&font, b, &[]).unwrap().glyphs.iter().map(|g| g.cluster).collect()
    /// };
    /// assert_eq!(clusters(&buffer), [0, 1]);
    /// // ... until the grapheme levels merge it into its base.
    /// buffer.set_cluster_level(ClusterLevel::MonotoneGraphemes);
    /// assert_eq!(clusters(&buffer), [0, 0]);
    /// # Ok::<(), sigilbuzz::Error>(())
    /// ```
    pub fn set_cluster_level(&mut self, level: ClusterLevel) {
        self.cluster_level = level;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_values_match_harfbuzz() {
        assert_eq!(BufferFlags::DEFAULT.bits(), 0x00);
        assert_eq!(BufferFlags::BOT.bits(), 0x01);
        assert_eq!(BufferFlags::EOT.bits(), 0x02);
        assert_eq!(BufferFlags::PRESERVE_DEFAULT_IGNORABLES.bits(), 0x04);
        assert_eq!(BufferFlags::REMOVE_DEFAULT_IGNORABLES.bits(), 0x08);
        assert_eq!(BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE.bits(), 0x10);
        assert_eq!(BufferFlags::all().bits(), 0x1F);
    }

    #[test]
    fn flag_set_operations() {
        let mut f = BufferFlags::empty();
        assert!(f.is_empty());
        f |= BufferFlags::BOT;
        f.insert(BufferFlags::EOT);
        assert!(f.contains(BufferFlags::BOT | BufferFlags::EOT));
        assert!(!f.contains(BufferFlags::BOT | BufferFlags::REMOVE_DEFAULT_IGNORABLES));
        assert!(f.intersects(BufferFlags::EOT | BufferFlags::REMOVE_DEFAULT_IGNORABLES));
        assert_eq!(f - BufferFlags::BOT, BufferFlags::EOT);
        assert_eq!(f & BufferFlags::BOT, BufferFlags::BOT);
        f -= BufferFlags::EOT;
        assert_eq!(f, BufferFlags::BOT);
        f &= BufferFlags::EOT;
        assert!(f.is_empty());
        assert_eq!(BufferFlags::from_bits(0x1F), Some(BufferFlags::all()));
        assert_eq!(BufferFlags::from_bits(0x40), None);
        assert_eq!(BufferFlags::from_bits_truncate(0xFF), BufferFlags::all());
    }

    #[test]
    fn cluster_level_predicates_match_harfbuzz_macros() {
        use ClusterLevel::*;
        let rows = [
            (MonotoneGraphemes, true, true, false),
            (MonotoneCharacters, true, false, true),
            (Characters, false, false, true),
            (Graphemes, false, true, false),
        ];
        for (level, monotone, graphemes, characters) in rows {
            assert_eq!(level.is_monotone(), monotone, "{level:?}");
            assert_eq!(level.is_graphemes(), graphemes, "{level:?}");
            assert_eq!(level.is_characters(), characters, "{level:?}");
        }
    }

    #[test]
    fn flags_and_level_survive_clear() {
        let mut b = Buffer::new();
        assert_eq!(b.flags(), BufferFlags::DEFAULT);
        assert_eq!(b.cluster_level(), ClusterLevel::MonotoneCharacters);
        b.set_flags(BufferFlags::BOT | BufferFlags::REMOVE_DEFAULT_IGNORABLES);
        b.set_cluster_level(ClusterLevel::Characters);
        b.push_str("abc");
        b.clear();
        assert_eq!(
            b.flags(),
            BufferFlags::BOT | BufferFlags::REMOVE_DEFAULT_IGNORABLES
        );
        assert_eq!(b.cluster_level(), ClusterLevel::Characters);
    }
}
