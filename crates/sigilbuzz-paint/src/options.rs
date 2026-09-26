//! Evaluation options and palette resolution.
//!
//! [`EvalOptions`] carries the three inputs a COLRv1 renderer chooses
//! besides the glyph itself:
//!
//! - the normalized variation coordinates the `PaintVar*` deltas are
//!   evaluated at,
//! - the CPAL palette that ordinary palette entries resolve against,
//! - the foreground color that stands in for palette entry `0xFFFF`.
//!
//! COLR reserves entry `0xFFFF` for "the current text color". The
//! evaluator never collapses it into an ordinary color: every solid fill
//! and every gradient stop that used it reports `is_foreground == true`,
//! and its color is the configured foreground with the paint's alpha
//! applied. A consumer that knows its text color can either pass it in
//! through [`EvalOptions::with_foreground`] or keep the default and
//! substitute its own color wherever the flag is set.

use sigilbuzz::tables::cpal::Cpal;

use crate::color::Color;

/// COLR palette entry index that means "use the foreground color".
pub(crate) const FOREGROUND_PALETTE_ENTRY: u16 = 0xFFFF;

/// Options for [`crate::evaluate_with`].
///
/// The defaults match [`crate::evaluate`]: no variation coordinates,
/// palette 0, and [`EvalOptions::DEFAULT_FOREGROUND`] as the foreground
/// color.
///
/// ```
/// use sigilbuzz_paint::{Color, EvalOptions};
///
/// let coords = [0.5_f32];
/// let black = Color::new(0.0, 0.0, 0.0, 1.0);
/// let options = EvalOptions::new()
///     .with_coords(&coords)
///     .with_palette_index(1)
///     .with_foreground(black);
/// assert_eq!(options.coords(), &[0.5]);
/// assert_eq!(options.palette_index(), 1);
/// assert_eq!(options.foreground(), black);
///
/// let defaults = EvalOptions::default();
/// assert!(defaults.coords().is_empty());
/// assert_eq!(defaults.palette_index(), 0);
/// assert_eq!(defaults.foreground(), EvalOptions::DEFAULT_FOREGROUND);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EvalOptions<'a> {
    coords: &'a [f32],
    palette_index: u16,
    foreground: Color,
}

impl<'a> EvalOptions<'a> {
    /// Foreground color used when the caller does not pick one: opaque
    /// white. It is what earlier releases painted for palette entry
    /// `0xFFFF`, so output stays the same for callers that never set a
    /// foreground.
    pub const DEFAULT_FOREGROUND: Color = Color::WHITE;

    /// Default options: static (no variation deltas), palette 0, and
    /// [`EvalOptions::DEFAULT_FOREGROUND`].
    ///
    /// ```
    /// use sigilbuzz_paint::EvalOptions;
    ///
    /// assert_eq!(EvalOptions::new(), EvalOptions::default());
    /// ```
    #[must_use]
    pub const fn new() -> Self {
        Self {
            coords: &[],
            palette_index: 0,
            foreground: Self::DEFAULT_FOREGROUND,
        }
    }

    /// Evaluates `PaintVar*` deltas at `coords`, the normalized axis
    /// vector (the same shape
    /// [`Face::glyph_outline_at_coords`](sigilbuzz::Face::glyph_outline_at_coords)
    /// accepts). An empty slice is the static path.
    ///
    /// ```
    /// use sigilbuzz_paint::EvalOptions;
    ///
    /// let coords = [1.0_f32, -0.25];
    /// assert_eq!(EvalOptions::new().with_coords(&coords).coords(), &coords);
    /// ```
    #[must_use]
    pub const fn with_coords(mut self, coords: &'a [f32]) -> Self {
        self.coords = coords;
        self
    }

    /// Resolves palette entries against CPAL palette `palette_index`.
    ///
    /// A font with fewer palettes than `palette_index + 1` falls back
    /// to palette 0, the font's default palette, so an out-of-range
    /// choice still renders the glyph in its default colors.
    ///
    /// ```
    /// use sigilbuzz_paint::EvalOptions;
    ///
    /// assert_eq!(EvalOptions::new().with_palette_index(2).palette_index(), 2);
    /// ```
    #[must_use]
    pub const fn with_palette_index(mut self, palette_index: u16) -> Self {
        self.palette_index = palette_index;
        self
    }

    /// Uses `foreground` for palette entry `0xFFFF`. The paint's alpha
    /// multiplies `foreground.a`.
    ///
    /// ```
    /// use sigilbuzz_paint::{Color, EvalOptions};
    ///
    /// let red = Color::new(1.0, 0.0, 0.0, 1.0);
    /// assert_eq!(EvalOptions::new().with_foreground(red).foreground(), red);
    /// ```
    #[must_use]
    pub const fn with_foreground(mut self, foreground: Color) -> Self {
        self.foreground = foreground;
        self
    }

    /// The normalized variation coordinates.
    ///
    /// ```
    /// use sigilbuzz_paint::EvalOptions;
    ///
    /// assert!(EvalOptions::new().coords().is_empty());
    /// ```
    #[must_use]
    pub const fn coords(&self) -> &'a [f32] {
        self.coords
    }

    /// The requested CPAL palette index.
    ///
    /// ```
    /// use sigilbuzz_paint::EvalOptions;
    ///
    /// assert_eq!(EvalOptions::new().palette_index(), 0);
    /// ```
    #[must_use]
    pub const fn palette_index(&self) -> u16 {
        self.palette_index
    }

    /// The foreground color used for palette entry `0xFFFF`.
    ///
    /// ```
    /// use sigilbuzz_paint::{Color, EvalOptions};
    ///
    /// assert_eq!(EvalOptions::new().foreground(), Color::WHITE);
    /// ```
    #[must_use]
    pub const fn foreground(&self) -> Color {
        self.foreground
    }
}

impl Default for EvalOptions<'_> {
    fn default() -> Self {
        Self::new()
    }
}

/// Palette state the walker resolves COLR palette entries through.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Palette<'a, 'b> {
    cpal: Option<&'b Cpal<'a>>,
    /// Effective palette index, already clamped into range.
    palette_index: u16,
    foreground: Color,
}

impl<'a, 'b> Palette<'a, 'b> {
    /// Selects the palette `options` asks for, or palette 0 when the
    /// font has no such palette.
    pub(crate) fn new(cpal: Option<&'b Cpal<'a>>, options: &EvalOptions<'_>) -> Self {
        let requested = options.palette_index();
        let palette_index = match cpal {
            Some(c) if requested < c.num_palettes() => requested,
            _ => 0,
        };
        Self {
            cpal,
            palette_index,
            foreground: options.foreground(),
        }
    }

    /// Resolves palette entry `entry` and folds `alpha` into the result.
    ///
    /// Returns the color and whether it came from the foreground entry
    /// (`0xFFFF`). An entry the palette does not have resolves to fully
    /// transparent. Never panics.
    pub(crate) fn resolve(&self, entry: u16, alpha: f32) -> (Color, bool) {
        if entry == FOREGROUND_PALETTE_ENTRY {
            return (self.foreground.with_alpha_multiplied(alpha), true);
        }
        let color = self
            .cpal
            .and_then(|cpal| cpal.color(self.palette_index, entry))
            .map_or(Color::TRANSPARENT, Color::from_cpal);
        (color.with_alpha_multiplied(alpha), false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CPAL v0 with two palettes of two entries each:
    /// palette 0 = [red, green], palette 1 = [blue, half-alpha white].
    fn two_palette_cpal() -> alloc::vec::Vec<u8> {
        let mut out = alloc::vec::Vec::new();
        out.extend_from_slice(&0u16.to_be_bytes()); // version
        out.extend_from_slice(&2u16.to_be_bytes()); // numPaletteEntries
        out.extend_from_slice(&2u16.to_be_bytes()); // numPalettes
        out.extend_from_slice(&4u16.to_be_bytes()); // numColorRecords
        out.extend_from_slice(&16u32.to_be_bytes()); // colorRecordsArrayOffset
        out.extend_from_slice(&0u16.to_be_bytes()); // palette 0 starts at record 0
        out.extend_from_slice(&2u16.to_be_bytes()); // palette 1 starts at record 2
        for (r, g, b, a) in [
            (255u8, 0u8, 0u8, 255u8),
            (0, 255, 0, 255),
            (0, 0, 255, 255),
            (255, 255, 255, 128),
        ] {
            out.extend_from_slice(&[b, g, r, a]);
        }
        out
    }

    #[test]
    fn default_options_match_legacy_behavior() {
        let o = EvalOptions::default();
        assert!(o.coords().is_empty());
        assert_eq!(o.palette_index(), 0);
        assert_eq!(o.foreground(), Color::new(1.0, 1.0, 1.0, 1.0));
    }

    #[test]
    fn builders_are_independent() {
        let coords = [0.25_f32];
        let fg = Color::new(0.1, 0.2, 0.3, 0.4);
        let o = EvalOptions::new()
            .with_foreground(fg)
            .with_palette_index(7)
            .with_coords(&coords);
        assert_eq!(o.coords(), &coords);
        assert_eq!(o.palette_index(), 7);
        assert_eq!(o.foreground(), fg);
    }

    #[test]
    fn foreground_entry_uses_foreground_with_alpha() {
        let fg = Color::new(0.2, 0.4, 0.6, 0.5);
        let o = EvalOptions::new().with_foreground(fg);
        let p = Palette::new(None, &o);
        let (c, is_fg) = p.resolve(0xFFFF, 0.5);
        assert!(is_fg);
        assert_eq!((c.r, c.g, c.b), (0.2, 0.4, 0.6));
        assert!((c.a - 0.25).abs() < 1e-6);
    }

    #[test]
    fn palette_selection_picks_requested_palette() {
        let bytes = two_palette_cpal();
        let cpal = Cpal::parse(&bytes).expect("cpal parses");
        let p1 = Palette::new(Some(&cpal), &EvalOptions::new().with_palette_index(1));
        let (c, is_fg) = p1.resolve(0, 1.0);
        assert!(!is_fg);
        assert_eq!(c, Color::new(0.0, 0.0, 1.0, 1.0));
        let (c, _) = p1.resolve(1, 1.0);
        assert!((c.a - 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn out_of_range_palette_falls_back_to_palette_zero() {
        let bytes = two_palette_cpal();
        let cpal = Cpal::parse(&bytes).expect("cpal parses");
        let p = Palette::new(Some(&cpal), &EvalOptions::new().with_palette_index(2));
        let (c, is_fg) = p.resolve(0, 1.0);
        assert!(!is_fg);
        assert_eq!(c, Color::new(1.0, 0.0, 0.0, 1.0));
    }

    #[test]
    fn missing_entry_or_cpal_is_transparent() {
        let bytes = two_palette_cpal();
        let cpal = Cpal::parse(&bytes).expect("cpal parses");
        let p = Palette::new(Some(&cpal), &EvalOptions::new());
        assert_eq!(p.resolve(9, 1.0), (Color::TRANSPARENT, false));
        let none = Palette::new(None, &EvalOptions::new());
        assert_eq!(none.resolve(0, 1.0), (Color::TRANSPARENT, false));
        // The foreground entry never needs a CPAL.
        assert_eq!(none.resolve(0xFFFF, 1.0), (Color::WHITE, true));
    }
}
