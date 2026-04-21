//! Shaping input and output types.
//!
//! A [`Buffer`] is fed one text run at a time, then passed to
//! [`crate::shape`] along with a [`crate::Font`]. On a successful
//! shape it yields a vector of [`Glyph`]s — each carrying the glyph
//! index the renderer should emit plus the position of that glyph
//! relative to the pen.
//!
//! The API mirrors `HarfBuzz`'s `hb_buffer_t` deliberately, so a
//! consumer who already knows `HarfBuzz` can reach for sigilbuzz without
//! relearning concepts.

use alloc::string::String;
use alloc::vec::Vec;

/// Writing direction of a text run.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Horizontal, left-to-right. Default for Latin, CJK in modern use.
    #[default]
    Ltr,
    /// Horizontal, right-to-left. Arabic, Hebrew.
    Rtl,
    /// Vertical, top-to-bottom. Traditional CJK.
    Ttb,
    /// Vertical, bottom-to-top. Rare, used for some Mongolian display.
    Btt,
}

impl Direction {
    /// True for horizontal directions.
    #[must_use]
    pub const fn is_horizontal(self) -> bool {
        matches!(self, Self::Ltr | Self::Rtl)
    }

    /// True for directions that advance "forward" in natural order.
    #[must_use]
    pub const fn is_forward(self) -> bool {
        matches!(self, Self::Ltr | Self::Ttb)
    }
}

/// One positioned glyph in the shaped output.
///
/// Positions are in font-design units that have been scaled by the
/// font's size. Advances and offsets are signed because shaping can
/// produce negative displacements (contextual kerning, backtracking
/// combining marks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    /// Glyph index within the font. After shaping, this is the index
    /// in the SFNT glyph table, *not* a Unicode codepoint.
    pub glyph_id: u32,
    /// Cluster tag linking this glyph back to the input codepoints.
    /// Multiple glyphs with the same cluster came from the same input
    /// grapheme (e.g. a ligature, or a base + combining mark).
    pub cluster: u32,
    /// Horizontal advance applied after drawing this glyph.
    pub x_advance: i32,
    /// Vertical advance applied after drawing this glyph.
    pub y_advance: i32,
    /// Horizontal offset applied to the glyph origin before drawing.
    pub x_offset: i32,
    /// Vertical offset applied to the glyph origin before drawing.
    pub y_offset: i32,
}

/// Shaping input: the text run, plus state flags the shaper consults.
///
/// Buffers are reusable. After calling [`crate::shape`] and consuming
/// the output, call [`Buffer::clear`] and push the next run.
#[derive(Debug, Default, Clone)]
pub struct Buffer {
    /// The text being shaped. Stored as `String` so the shaper sees
    /// validated UTF-8 without re-checking.
    pub(crate) text: String,
    /// Writing direction. Defaults to [`Direction::Ltr`].
    pub(crate) direction: Direction,
    /// When `true`, `shape()` composes the input text via
    /// [`crate::unicode::normalize::compose_str`] before glyph
    /// lookup. Matches HarfBuzz's implicit NFC pass for the
    /// ranges sigilbuzz has curated tables for.
    pub(crate) normalize_nfc: bool,
}

impl Buffer {
    /// Creates an empty buffer with default direction.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `text` to the buffer.
    pub fn push_str(&mut self, text: &str) {
        self.text.push_str(text);
    }

    /// Replaces the buffer contents with `text`.
    pub fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
    }

    /// Current text view.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Current writing direction.
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    /// Sets the writing direction for the next shaping call.
    pub fn set_direction(&mut self, direction: Direction) {
        self.direction = direction;
    }

    /// True when [`Buffer::set_normalize_nfc`] has been enabled.
    #[must_use]
    pub const fn normalize_nfc(&self) -> bool {
        self.normalize_nfc
    }

    /// Enables or disables the implicit NFC composition pass that
    /// runs before glyph lookup. Off by default. Turn this on to
    /// match HarfBuzz's behaviour, where `e + U+0301` renders the
    /// same as the precomposed `é`.
    pub fn set_normalize_nfc(&mut self, enabled: bool) {
        self.normalize_nfc = enabled;
    }

    /// Clears the text and resets direction to LTR. Other future
    /// state (script, language, user data) will reset here too.
    pub fn clear(&mut self) {
        self.text.clear();
        self.direction = Direction::Ltr;
        self.normalize_nfc = false;
    }

    /// True when no text has been pushed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// The result of a shaping call: the glyphs, in visual order.
#[derive(Debug, Default, Clone)]
pub struct ShapedRun {
    /// Positioned glyphs, ready to draw.
    pub glyphs: Vec<Glyph>,
}

impl ShapedRun {
    /// Number of glyphs produced.
    #[must_use]
    pub fn len(&self) -> usize {
        self.glyphs.len()
    }

    /// True if shaping produced no glyphs (empty input, or pre-shape).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_round_trips_text_and_direction() {
        let mut b = Buffer::new();
        b.push_str("hello");
        b.set_direction(Direction::Rtl);
        assert_eq!(b.text(), "hello");
        assert_eq!(b.direction(), Direction::Rtl);
        assert!(!b.is_empty());
    }

    #[test]
    fn clear_resets_everything() {
        let mut b = Buffer::new();
        b.push_str("x");
        b.set_direction(Direction::Ttb);
        b.clear();
        assert!(b.is_empty());
        assert_eq!(b.direction(), Direction::Ltr);
    }

    #[test]
    fn set_text_replaces_rather_than_appends() {
        let mut b = Buffer::new();
        b.set_text("one");
        b.set_text("two");
        assert_eq!(b.text(), "two");
    }

    #[test]
    fn direction_classifies_axes_and_order() {
        assert!(Direction::Ltr.is_horizontal());
        assert!(Direction::Rtl.is_horizontal());
        assert!(!Direction::Ttb.is_horizontal());
        assert!(Direction::Ltr.is_forward());
        assert!(!Direction::Rtl.is_forward());
    }
}
