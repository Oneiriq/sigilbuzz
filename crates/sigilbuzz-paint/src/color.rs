//! Linear RGBA color with f32 channels.
//!
//! sigilbuzz parses CPAL palettes as 8-bit sRGB; this module is the
//! evaluator's float-channel companion. Channels are post-multiplied
//! during stop resolution: a stop's per-stop alpha is multiplied with
//! the palette entry's alpha before the color leaves the evaluator.
//!
//! Conversion stays linear-in-sRGB at this layer — color management
//! is the renderer's call, not ours.

use sigilbuzz::tables::cpal::Color as CpalColor;

/// Floating-point RGBA color, channels in `[0.0, 1.0]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    /// Red channel.
    pub r: f32,
    /// Green channel.
    pub g: f32,
    /// Blue channel.
    pub b: f32,
    /// Alpha channel.
    pub a: f32,
}

impl Color {
    /// Fully transparent black. Used as the fallback when a palette
    /// lookup fails (e.g. malformed `paletteIndex`).
    pub const TRANSPARENT: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    /// Constructs from raw f32 channels without clamping.
    #[must_use]
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// Promotes a CPAL entry to f32 RGBA in `[0, 1]`.
    #[must_use]
    pub fn from_cpal(c: CpalColor) -> Self {
        Self {
            r: c.r as f32 / 255.0,
            g: c.g as f32 / 255.0,
            b: c.b as f32 / 255.0,
            a: c.a as f32 / 255.0,
        }
    }

    /// Multiplies the alpha channel by `factor` (clamped to `[0, 1]`).
    /// Used to apply a `ColorStop`'s per-stop alpha or a `PaintSolid`'s
    /// alpha override on top of the palette entry.
    #[must_use]
    pub fn with_alpha_multiplied(self, factor: f32) -> Self {
        let f = factor.clamp(0.0, 1.0);
        Self {
            a: (self.a * f).clamp(0.0, 1.0),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpal_promotion_is_linear() {
        let c = CpalColor {
            r: 255,
            g: 128,
            b: 0,
            a: 255,
        };
        let f = Color::from_cpal(c);
        assert!((f.r - 1.0).abs() < 1e-6);
        assert!((f.g - 128.0 / 255.0).abs() < 1e-6);
        assert!((f.b).abs() < 1e-6);
        assert!((f.a - 1.0).abs() < 1e-6);
    }

    #[test]
    fn alpha_multiplied_clamps() {
        let c = Color::new(0.5, 0.5, 0.5, 1.0).with_alpha_multiplied(0.5);
        assert!((c.a - 0.5).abs() < 1e-6);
        let c2 = Color::new(0.5, 0.5, 0.5, 1.0).with_alpha_multiplied(2.0);
        assert!((c2.a - 1.0).abs() < 1e-6);
        let c3 = Color::new(0.5, 0.5, 0.5, 1.0).with_alpha_multiplied(-1.0);
        assert!((c3.a).abs() < 1e-6);
    }
}
