//! COLR v1 paint model: the `ColrPaint` node enum plus the color
//! line, color stop, extend and composite mode types its variants
//! carry.

mod parse;

use super::PaintOffset;

// =========================================================================
// COLR v1: paint tree
// =========================================================================

/// An alpha factor clamped to `[0, 1]`. Stored as F2DOT14 in the font.
pub type F2Dot14 = f32;

/// A fixed-point 16.16 scalar, used for gradient stops and so on.
pub type Fixed = f32;

/// Signed 16-bit point coordinate in font design units.
pub type Fword = i16;

/// Var-index-base used to look up deltas in the ItemVariationStore
/// for a paint. Sentinel value `0xFFFFFFFF` means "no variation".
pub type VarIndexBase = u32;

/// Color stop in a `PaintColorLine`. Colors index into the active
/// `CPAL` palette; alpha is a post-multiplied factor on top of the
/// palette entry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorStop {
    /// Stop position along the line, as a fraction (typically
    /// `0.0..=1.0`, but may exceed).
    pub stop_offset: F2Dot14,
    /// Palette index. `0xFFFF` means "use the foreground color".
    pub palette_index: u16,
    /// Additional alpha on the stop color.
    pub alpha: F2Dot14,
}

/// Gradient extend modes (what happens outside `[0, 1]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extend {
    /// Repeat the first / last color outward.
    Pad,
    /// Repeat the entire gradient.
    Repeat,
    /// Repeat, reflecting every other copy.
    Reflect,
}

impl Extend {
    const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Repeat,
            2 => Self::Reflect,
            _ => Self::Pad,
        }
    }
}

/// Color line: extend + color stops. Parsed lazily; iteration
/// over [`ColorLine::stops`] walks the font bytes directly.
#[derive(Debug, Clone, Copy)]
pub struct ColorLine<'a> {
    /// Where to extend colors past `[0, 1]`.
    pub extend: Extend,
    /// Raw per-stop array. Each stop is 6 bytes (v0) or 10 bytes
    /// (v1, with var index base).
    stops: &'a [u8],
    /// Number of stops.
    stop_count: u16,
    /// True when each stop carries a `varIndexBase` (the
    /// `VarColorLine` subtable).
    variable: bool,
}

impl ColorLine<'_> {
    /// Number of stops in the line.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.stop_count
    }

    /// True when the line carries zero stops (spec-degenerate).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.stop_count == 0
    }

    /// Returns the stop at `index`, ignoring the variation index
    /// base even on `VarColorLine` subtables (consumers that want
    /// deltas walk [`Self::stops_variable`] instead).
    #[must_use]
    pub fn get(&self, index: u16) -> Option<ColorStop> {
        if index >= self.stop_count {
            return None;
        }
        let rec = if self.variable { 10 } else { 6 };
        let off = index as usize * rec;
        if off + 6 > self.stops.len() {
            return None;
        }
        let stop_offset =
            i16::from_be_bytes([self.stops[off], self.stops[off + 1]]) as f32 / 16384.0;
        let palette_index = u16::from_be_bytes([self.stops[off + 2], self.stops[off + 3]]);
        let alpha = i16::from_be_bytes([self.stops[off + 4], self.stops[off + 5]]) as f32 / 16384.0;
        Some(ColorStop {
            stop_offset,
            palette_index,
            alpha,
        })
    }

    /// Iterates over every stop in order.
    pub fn stops(&self) -> impl Iterator<Item = ColorStop> + '_ {
        (0..self.stop_count).filter_map(move |i| self.get(i))
    }

    /// For variable color lines, returns the `varIndexBase` alongside
    /// each stop so consumers can resolve deltas through the
    /// ItemVariationStore.
    pub fn stops_variable(&self) -> impl Iterator<Item = (ColorStop, VarIndexBase)> + '_ {
        (0..self.stop_count).filter_map(move |i| {
            let stop = self.get(i)?;
            if !self.variable {
                return Some((stop, u32::MAX));
            }
            let off = i as usize * 10 + 6;
            if off + 4 > self.stops.len() {
                return None;
            }
            let var = u32::from_be_bytes([
                self.stops[off],
                self.stops[off + 1],
                self.stops[off + 2],
                self.stops[off + 3],
            ]);
            Some((stop, var))
        })
    }
}

/// Composite mode for `PaintComposite`. Values match the COLR spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
#[repr(u8)]
pub enum CompositeMode {
    Clear = 0,
    Src = 1,
    Dest = 2,
    SrcOver = 3,
    DestOver = 4,
    SrcIn = 5,
    DestIn = 6,
    SrcOut = 7,
    DestOut = 8,
    SrcAtop = 9,
    DestAtop = 10,
    Xor = 11,
    Plus = 12,
    Screen = 13,
    Overlay = 14,
    Darken = 15,
    Lighten = 16,
    ColorDodge = 17,
    ColorBurn = 18,
    HardLight = 19,
    SoftLight = 20,
    Difference = 21,
    Exclusion = 22,
    Multiply = 23,
    HslHue = 24,
    HslSaturation = 25,
    HslColor = 26,
    HslLuminosity = 27,
}

impl CompositeMode {
    const fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Clear,
            1 => Self::Src,
            2 => Self::Dest,
            4 => Self::DestOver,
            5 => Self::SrcIn,
            6 => Self::DestIn,
            7 => Self::SrcOut,
            8 => Self::DestOut,
            9 => Self::SrcAtop,
            10 => Self::DestAtop,
            11 => Self::Xor,
            12 => Self::Plus,
            13 => Self::Screen,
            14 => Self::Overlay,
            15 => Self::Darken,
            16 => Self::Lighten,
            17 => Self::ColorDodge,
            18 => Self::ColorBurn,
            19 => Self::HardLight,
            20 => Self::SoftLight,
            21 => Self::Difference,
            22 => Self::Exclusion,
            23 => Self::Multiply,
            24 => Self::HslHue,
            25 => Self::HslSaturation,
            26 => Self::HslColor,
            27 => Self::HslLuminosity,
            _ => Self::SrcOver,
        }
    }
}

/// A v1 paint node. Every variant borrows the original COLR bytes
/// and carries absolute offsets (not raw font offsets) so callers can
/// resolve child paints through [`Colr::paint_at`](super::Colr::paint_at) without doing
/// arithmetic themselves.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum ColrPaint<'a> {
    /// Format 1: Paint reference to a range of layers in the LayerList.
    ColrLayers {
        /// Number of consecutive layers to paint.
        num_layers: u8,
        /// Index into the LayerList for the first layer.
        first_layer_index: u32,
    },
    /// Format 2: Solid fill.
    Solid {
        /// Palette entry. `0xFFFF` = foreground.
        palette_index: u16,
        /// Additional alpha factor.
        alpha: F2Dot14,
    },
    /// Format 3: Variable solid fill.
    VarSolid {
        /// Palette entry. `0xFFFF` = foreground.
        palette_index: u16,
        /// Additional alpha factor.
        alpha: F2Dot14,
        /// Variation index base for the alpha.
        var_index_base: VarIndexBase,
    },
    /// Format 4: Linear gradient.
    LinearGradient {
        /// Color line. Resolve stops via [`ColorLine::stops`].
        color_line: ColorLine<'a>,
        /// Gradient endpoint p0 (start).
        x0: Fword,
        /// Gradient endpoint p0 (start).
        y0: Fword,
        /// Gradient endpoint p1 (end).
        x1: Fword,
        /// Gradient endpoint p1 (end).
        y1: Fword,
        /// Rotation anchor p2.
        x2: Fword,
        /// Rotation anchor p2.
        y2: Fword,
    },
    /// Format 5: Variable linear gradient.
    VarLinearGradient {
        /// Color line.
        color_line: ColorLine<'a>,
        /// Gradient endpoint p0.
        x0: Fword,
        /// Gradient endpoint p0.
        y0: Fword,
        /// Gradient endpoint p1.
        x1: Fword,
        /// Gradient endpoint p1.
        y1: Fword,
        /// Rotation anchor p2.
        x2: Fword,
        /// Rotation anchor p2.
        y2: Fword,
        /// Variation index base for the six coordinates.
        var_index_base: VarIndexBase,
    },
    /// Format 6: Radial gradient.
    RadialGradient {
        /// Color line.
        color_line: ColorLine<'a>,
        /// Inner circle center x.
        x0: Fword,
        /// Inner circle center y.
        y0: Fword,
        /// Inner radius.
        r0: u16,
        /// Outer circle center x.
        x1: Fword,
        /// Outer circle center y.
        y1: Fword,
        /// Outer radius.
        r1: u16,
    },
    /// Format 7: Variable radial gradient.
    VarRadialGradient {
        /// Color line.
        color_line: ColorLine<'a>,
        /// Inner center x.
        x0: Fword,
        /// Inner center y.
        y0: Fword,
        /// Inner radius.
        r0: u16,
        /// Outer center x.
        x1: Fword,
        /// Outer center y.
        y1: Fword,
        /// Outer radius.
        r1: u16,
        /// Variation index base.
        var_index_base: VarIndexBase,
    },
    /// Format 8: Sweep gradient.
    SweepGradient {
        /// Color line.
        color_line: ColorLine<'a>,
        /// Center x.
        center_x: Fword,
        /// Center y.
        center_y: Fword,
        /// Start angle in degrees * (180/128) per the spec's F2Dot14.
        start_angle: F2Dot14,
        /// End angle.
        end_angle: F2Dot14,
    },
    /// Format 9: Variable sweep gradient.
    VarSweepGradient {
        /// Color line.
        color_line: ColorLine<'a>,
        /// Center x.
        center_x: Fword,
        /// Center y.
        center_y: Fword,
        /// Start angle.
        start_angle: F2Dot14,
        /// End angle.
        end_angle: F2Dot14,
        /// Variation index base.
        var_index_base: VarIndexBase,
    },
    /// Format 10: Paint a child paint through the outline of `glyph`.
    Glyph {
        /// Child paint offset (absolute).
        paint_offset: PaintOffset,
        /// Outline glyph id that clips the child paint.
        glyph_id: u16,
    },
    /// Format 11: Reference another base-glyph's paint tree.
    ColrGlyph {
        /// Glyph id whose BaseGlyphPaintRecord we should substitute.
        glyph_id: u16,
    },
    /// Format 12: Apply an affine transform to a child paint.
    Transform {
        /// Child paint.
        paint_offset: PaintOffset,
        /// 3x2 affine matrix component xx.
        xx: Fixed,
        /// 3x2 affine matrix component yx.
        yx: Fixed,
        /// 3x2 affine matrix component xy.
        xy: Fixed,
        /// 3x2 affine matrix component yy.
        yy: Fixed,
        /// 3x2 affine matrix translation dx.
        dx: Fixed,
        /// 3x2 affine matrix translation dy.
        dy: Fixed,
    },
    /// Format 13: Variable affine transform.
    VarTransform {
        /// Child paint.
        paint_offset: PaintOffset,
        /// Affine xx.
        xx: Fixed,
        /// Affine yx.
        yx: Fixed,
        /// Affine xy.
        xy: Fixed,
        /// Affine yy.
        yy: Fixed,
        /// Affine dx.
        dx: Fixed,
        /// Affine dy.
        dy: Fixed,
        /// Variation index base.
        var_index_base: VarIndexBase,
    },
    /// Format 14: Pure translate.
    Translate {
        /// Child paint.
        paint_offset: PaintOffset,
        /// Translation x.
        dx: Fword,
        /// Translation y.
        dy: Fword,
    },
    /// Format 15: Variable translate.
    VarTranslate {
        /// Child paint.
        paint_offset: PaintOffset,
        /// Translation x.
        dx: Fword,
        /// Translation y.
        dy: Fword,
        /// Variation index base.
        var_index_base: VarIndexBase,
    },
    /// Format 16: Scale around origin.
    Scale {
        /// Child paint.
        paint_offset: PaintOffset,
        /// Scale x.
        scale_x: F2Dot14,
        /// Scale y.
        scale_y: F2Dot14,
    },
    /// Format 17: Variable scale around origin.
    VarScale {
        /// Child paint.
        paint_offset: PaintOffset,
        /// Scale x.
        scale_x: F2Dot14,
        /// Scale y.
        scale_y: F2Dot14,
        /// Variation index base.
        var_index_base: VarIndexBase,
    },
    /// Format 18: Scale around a specified center point.
    ScaleAroundCenter {
        /// Child paint.
        paint_offset: PaintOffset,
        /// Scale x.
        scale_x: F2Dot14,
        /// Scale y.
        scale_y: F2Dot14,
        /// Center x.
        center_x: Fword,
        /// Center y.
        center_y: Fword,
    },
    /// Format 19: Variable scale around center.
    VarScaleAroundCenter {
        /// Child paint.
        paint_offset: PaintOffset,
        /// Scale x.
        scale_x: F2Dot14,
        /// Scale y.
        scale_y: F2Dot14,
        /// Center x.
        center_x: Fword,
        /// Center y.
        center_y: Fword,
        /// Variation index base.
        var_index_base: VarIndexBase,
    },
    /// Format 20: Uniform scale around origin.
    ScaleUniform {
        /// Child paint.
        paint_offset: PaintOffset,
        /// Uniform scale.
        scale: F2Dot14,
    },
    /// Format 21: Variable uniform scale around origin.
    VarScaleUniform {
        /// Child paint.
        paint_offset: PaintOffset,
        /// Uniform scale.
        scale: F2Dot14,
        /// Variation index base.
        var_index_base: VarIndexBase,
    },
    /// Format 22: Uniform scale around specified center.
    ScaleUniformAroundCenter {
        /// Child paint.
        paint_offset: PaintOffset,
        /// Uniform scale.
        scale: F2Dot14,
        /// Center x.
        center_x: Fword,
        /// Center y.
        center_y: Fword,
    },
    /// Format 23: Variable uniform scale around center.
    VarScaleUniformAroundCenter {
        /// Child paint.
        paint_offset: PaintOffset,
        /// Uniform scale.
        scale: F2Dot14,
        /// Center x.
        center_x: Fword,
        /// Center y.
        center_y: Fword,
        /// Variation index base.
        var_index_base: VarIndexBase,
    },
    /// Format 24: Rotate around origin.
    Rotate {
        /// Child paint.
        paint_offset: PaintOffset,
        /// Angle in degrees, F2DOT14 * 180 scaling.
        angle: F2Dot14,
    },
    /// Format 25: Variable rotate.
    VarRotate {
        /// Child paint.
        paint_offset: PaintOffset,
        /// Angle.
        angle: F2Dot14,
        /// Variation index base.
        var_index_base: VarIndexBase,
    },
    /// Format 26: Rotate around center.
    RotateAroundCenter {
        /// Child paint.
        paint_offset: PaintOffset,
        /// Angle.
        angle: F2Dot14,
        /// Center x.
        center_x: Fword,
        /// Center y.
        center_y: Fword,
    },
    /// Format 27: Variable rotate around center.
    VarRotateAroundCenter {
        /// Child paint.
        paint_offset: PaintOffset,
        /// Angle.
        angle: F2Dot14,
        /// Center x.
        center_x: Fword,
        /// Center y.
        center_y: Fword,
        /// Variation index base.
        var_index_base: VarIndexBase,
    },
    /// Format 28: Skew around origin.
    Skew {
        /// Child paint.
        paint_offset: PaintOffset,
        /// X skew angle.
        x_skew_angle: F2Dot14,
        /// Y skew angle.
        y_skew_angle: F2Dot14,
    },
    /// Format 29: Variable skew.
    VarSkew {
        /// Child paint.
        paint_offset: PaintOffset,
        /// X skew angle.
        x_skew_angle: F2Dot14,
        /// Y skew angle.
        y_skew_angle: F2Dot14,
        /// Variation index base.
        var_index_base: VarIndexBase,
    },
    /// Format 30: Skew around center.
    SkewAroundCenter {
        /// Child paint.
        paint_offset: PaintOffset,
        /// X skew angle.
        x_skew_angle: F2Dot14,
        /// Y skew angle.
        y_skew_angle: F2Dot14,
        /// Center x.
        center_x: Fword,
        /// Center y.
        center_y: Fword,
    },
    /// Format 31: Variable skew around center.
    VarSkewAroundCenter {
        /// Child paint.
        paint_offset: PaintOffset,
        /// X skew angle.
        x_skew_angle: F2Dot14,
        /// Y skew angle.
        y_skew_angle: F2Dot14,
        /// Center x.
        center_x: Fword,
        /// Center y.
        center_y: Fword,
        /// Variation index base.
        var_index_base: VarIndexBase,
    },
    /// Format 32: Composite source over / under / various modes.
    Composite {
        /// Source paint.
        source_paint_offset: PaintOffset,
        /// Composite mode.
        composite_mode: CompositeMode,
        /// Backdrop paint.
        backdrop_paint_offset: PaintOffset,
    },
}
