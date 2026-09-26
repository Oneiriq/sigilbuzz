//! `COLR`: Layered color glyph table.
//!
//! Two generations coexist in a single `COLR` blob:
//!
//! - **v0** (2013 era): a base glyph maps to a flat list of
//!   *(glyph_id, palette_index)* layer records. The renderer draws
//!   each layer in order with the palette color.
//! - **v1** (2020 era): a base glyph maps to a tree of `Paint`
//!   operations (gradients, transforms, composites, nested clip
//!   glyphs), roughly matching SVG's native-paint model. Variable-font
//!   aware siblings (`PaintVar*`) carry `ItemVariationStore` deltas.
//!
//! sigilbuzz parses both. Evaluation (drawing pixels) is a renderer
//! concern; this module stops at structural traversal: enumerate the
//! v0 layers for a glyph, walk the v1 paint DAG for a glyph. The walk
//! returns byte-slice-backed views so callers do not pay to clone the
//! tree.
//!
//! The v1 paint enum is large. Every variant borrows `&'a [u8]` into
//! the original font data; sub-paints are represented by `Offset24`
//! (3-byte offsets relative to the start of the `LayerList` /
//! `BaseGlyphList` block) which consumers resolve through
//! [`Colr::paint_at`]. This keeps traversal zero-copy and avoids
//! constructing a recursive owned tree eagerly.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// Offset (absolute, within the COLR table) to a v1 `Paint` node.
/// Stored as a u24 in the font, widened to u32 here.
pub type PaintOffset = u32;

// =========================================================================
// Header
// =========================================================================

/// Parsed `COLR` table.
///
/// Holds enough offsets to service both the v0 layer list and, when
/// present, the v1 paint-tree traversal API. Cloning is cheap:
/// just a handful of slice references.
#[derive(Debug, Clone, Copy)]
pub struct Colr<'a> {
    data: &'a [u8],
    /// Version word. `0` for the layer-only table, `1` for the
    /// paint-tree extension.
    version: u16,

    /// v0: base-glyph-record array slice. Each record is 6 bytes.
    base_glyph_records: &'a [u8],
    /// v0: layer-record array slice. Each record is 4 bytes.
    layer_records: &'a [u8],
    num_layer_records: u16,

    // --- v1-only offsets (all zero / None when version == 0) ---
    /// Absolute offset to the v1 BaseGlyphList block, or 0 if absent.
    base_glyph_list_off: u32,
    /// Absolute offset to the v1 LayerList block, or 0 if absent.
    layer_list_off: u32,
    /// Absolute offset to the v1 ItemVariationStore, or 0 if absent.
    var_store_off: u32,
}

impl<'a> Colr<'a> {
    /// Parses a `COLR` table header.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.read_u16()?;
        if version != 0 && version != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported COLR version",
            });
        }
        let num_base_glyph_records = r.read_u16()?;
        let base_glyph_records_off = r.read_u32()? as usize;
        let layer_records_off = r.read_u32()? as usize;
        let num_layer_records = r.read_u16()?;

        let base_bytes = num_base_glyph_records as usize * 6;
        let layer_bytes = num_layer_records as usize * 4;

        let base_end = base_glyph_records_off
            .checked_add(base_bytes)
            .ok_or(Error::Malformed {
                offset: base_glyph_records_off,
                context: "COLR base glyph records overflow",
            })?;
        let layer_end = layer_records_off
            .checked_add(layer_bytes)
            .ok_or(Error::Malformed {
                offset: layer_records_off,
                context: "COLR layer records overflow",
            })?;
        if base_end > data.len() || layer_end > data.len() {
            return Err(Error::Truncated {
                offset: data.len(),
                context: "COLR v0 record arrays truncated",
            });
        }

        let base_glyph_records = if num_base_glyph_records == 0 {
            &data[..0]
        } else {
            &data[base_glyph_records_off..base_end]
        };
        let layer_records = if num_layer_records == 0 {
            &data[..0]
        } else {
            &data[layer_records_off..layer_end]
        };

        let mut base_glyph_list_off = 0u32;
        let mut layer_list_off = 0u32;
        let mut var_store_off = 0u32;
        if version >= 1 {
            // v1 appends four Offset32 fields to the header. The
            // ClipList offset is read and dropped: nothing in
            // sigilbuzz consumes clip boxes.
            base_glyph_list_off = r.read_u32()?;
            layer_list_off = r.read_u32()?;
            let _clip_list_off = r.read_u32()?;
            var_store_off = r.read_u32()?;
        }

        Ok(Self {
            data,
            version,
            base_glyph_records,
            layer_records,
            num_layer_records,
            base_glyph_list_off,
            layer_list_off,
            var_store_off,
        })
    }

    /// Table version word. `0` for layer-only, `1` for paint-tree.
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Number of v0 base-glyph records.
    #[must_use]
    pub fn num_base_glyph_records(&self) -> u16 {
        (self.base_glyph_records.len() / 6) as u16
    }

    /// Number of v0 layer records in the pool.
    #[must_use]
    pub const fn num_layer_records(&self) -> u16 {
        self.num_layer_records
    }

    /// True when the font carries the v1 paint-tree extension.
    #[must_use]
    pub const fn has_v1(&self) -> bool {
        self.version >= 1 && self.base_glyph_list_off != 0
    }

    /// Returns the absolute offset of the v1 ItemVariationStore, or
    /// `None` when the font has no variation deltas on its paint
    /// tree.
    #[must_use]
    pub fn var_store_offset(&self) -> Option<u32> {
        if self.var_store_off == 0 {
            None
        } else {
            Some(self.var_store_off)
        }
    }

    /// Looks up v0 layers for `glyph_id`. Binary search on the sorted
    /// base-glyph-record array. Returns `None` when the glyph has no
    /// v0 record.
    #[must_use]
    pub fn v0_layers(&self, glyph_id: u16) -> Option<V0Layers<'a>> {
        let n = self.num_base_glyph_records() as usize;
        let mut lo = 0;
        let mut hi = n;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let off = mid * 6;
            let gid = u16::from_be_bytes([
                self.base_glyph_records[off],
                self.base_glyph_records[off + 1],
            ]);
            match gid.cmp(&glyph_id) {
                core::cmp::Ordering::Equal => {
                    let first = u16::from_be_bytes([
                        self.base_glyph_records[off + 2],
                        self.base_glyph_records[off + 3],
                    ]);
                    let count = u16::from_be_bytes([
                        self.base_glyph_records[off + 4],
                        self.base_glyph_records[off + 5],
                    ]);
                    return Some(V0Layers {
                        records: self.layer_records,
                        first,
                        count,
                    });
                }
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
            }
        }
        None
    }

    /// Returns the v1 paint for `glyph_id` if the font has the v1
    /// extension and the glyph has a BaseGlyphPaintRecord. Callers
    /// walk the returned [`ColrPaint`] with its child-resolution
    /// helpers to traverse the tree.
    #[must_use]
    pub fn paint(&self, glyph_id: u16) -> Option<ColrPaint<'a>> {
        if !self.has_v1() {
            return None;
        }
        let list_start = self.base_glyph_list_off as usize;
        let count = read_u32_at(self.data, list_start)? as usize;
        let recs_start = list_start + 4;
        // Checked: on 32-bit targets `count * 6` can overflow, and a
        // wrapped end would let the search below index out of bounds.
        let recs_end = recs_start.checked_add(count.checked_mul(6)?)?;
        if recs_end > self.data.len() {
            return None;
        }
        // BaseGlyphPaintRecord: { u16 glyphID; Offset32 paintOffset }.
        // Binary search on glyphID.
        let mut lo = 0usize;
        let mut hi = count;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let off = recs_start + mid * 6;
            let gid = u16::from_be_bytes([self.data[off], self.data[off + 1]]);
            match gid.cmp(&glyph_id) {
                core::cmp::Ordering::Equal => {
                    let paint_rel = u32::from_be_bytes([
                        self.data[off + 2],
                        self.data[off + 3],
                        self.data[off + 4],
                        self.data[off + 5],
                    ]);
                    // Paint offset is relative to the start of the
                    // BaseGlyphList subtable.
                    let abs = (list_start as u32).checked_add(paint_rel)?;
                    return self.paint_at(abs);
                }
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
            }
        }
        None
    }

    /// Resolves the `Paint` sitting at the absolute `offset` inside
    /// the COLR table. Returns `None` when the offset is out of range
    /// or the format byte is one sigilbuzz does not yet handle.
    #[must_use]
    pub fn paint_at(&self, offset: u32) -> Option<ColrPaint<'a>> {
        let start = offset as usize;
        if start >= self.data.len() {
            return None;
        }
        ColrPaint::parse(self.data, start).ok()
    }

    /// Resolves a layer in the v1 LayerList by index. Each layer is
    /// itself a Paint; callers iterate over them after reading a
    /// [`ColrPaint::ColrLayers`] header.
    #[must_use]
    pub fn layer_paint(&self, layer_index: u32) -> Option<ColrPaint<'a>> {
        if self.layer_list_off == 0 {
            return None;
        }
        let list_start = self.layer_list_off as usize;
        let count = read_u32_at(self.data, list_start)?;
        if layer_index >= count {
            return None;
        }
        let entry = (layer_index as usize)
            .checked_mul(4)?
            .checked_add(list_start + 4)?;
        let paint_rel = read_u32_at(self.data, entry)?;
        let abs = (list_start as u32).checked_add(paint_rel)?;
        self.paint_at(abs)
    }

    /// Returns the full backing slice. Useful for renderers walking
    /// child offsets that were resolved into absolute positions.
    #[must_use]
    pub const fn data(&self) -> &'a [u8] {
        self.data
    }
}

/// Reads a big-endian `u32` at `off`, or `None` when fewer than four
/// bytes remain.
fn read_u32_at(data: &[u8], off: usize) -> Option<u32> {
    let bytes = data.get(off..)?.get(..4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

// =========================================================================
// COLR v0: layer records
// =========================================================================

/// Iterator view over the v0 layer records for a single base glyph.
#[derive(Debug, Clone, Copy)]
pub struct V0Layers<'a> {
    records: &'a [u8],
    first: u16,
    count: u16,
}

impl V0Layers<'_> {
    /// Number of layers that make up this glyph.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.count
    }

    /// True when the glyph has no layers (spec-degenerate).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns the layer at `index`. Layers are drawn in order,
    /// bottom-up: index 0 is behind index 1.
    #[must_use]
    pub fn get(&self, index: u16) -> Option<V0Layer> {
        if index >= self.count {
            return None;
        }
        let rec = self.first as usize + index as usize;
        let off = rec * 4;
        if off + 4 > self.records.len() {
            return None;
        }
        let gid = u16::from_be_bytes([self.records[off], self.records[off + 1]]);
        let palette_index = u16::from_be_bytes([self.records[off + 2], self.records[off + 3]]);
        Some(V0Layer {
            glyph_id: gid,
            palette_index,
        })
    }

    /// Yields every layer in draw order.
    pub fn iter(&self) -> impl Iterator<Item = V0Layer> + '_ {
        (0..self.count).filter_map(move |i| self.get(i))
    }
}

/// One layer in the v0 list: a glyph id and the palette entry it
/// should be painted with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V0Layer {
    /// Child glyph id to draw.
    pub glyph_id: u16,
    /// Index into the active `CPAL` palette. `0xFFFF` means "use the
    /// foreground text color".
    pub palette_index: u16,
}

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
/// Modes 0 to 12 are the Porter-Duff operators. Modes 13 to 27 are
/// the separable and non-separable blend modes from the W3C
/// Compositing and Blending spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CompositeMode {
    /// Porter-Duff clear: the result is fully transparent.
    Clear = 0,
    /// Porter-Duff source: keep the source only.
    Src = 1,
    /// Porter-Duff destination: keep the backdrop only.
    Dest = 2,
    /// Porter-Duff source over: source drawn on top of the backdrop.
    SrcOver = 3,
    /// Porter-Duff destination over: backdrop drawn on top of the
    /// source.
    DestOver = 4,
    /// Porter-Duff source in: source where the backdrop is opaque.
    SrcIn = 5,
    /// Porter-Duff destination in: backdrop where the source is
    /// opaque.
    DestIn = 6,
    /// Porter-Duff source out: source where the backdrop is
    /// transparent.
    SrcOut = 7,
    /// Porter-Duff destination out: backdrop where the source is
    /// transparent.
    DestOut = 8,
    /// Porter-Duff source atop: source over the backdrop, clipped to
    /// the backdrop.
    SrcAtop = 9,
    /// Porter-Duff destination atop: backdrop over the source, clipped
    /// to the source.
    DestAtop = 10,
    /// Porter-Duff XOR: each shape only where the other is absent.
    Xor = 11,
    /// Porter-Duff plus: source and backdrop added and clamped.
    Plus = 12,
    /// Screen blend: inverted multiply, always at least as light.
    Screen = 13,
    /// Overlay blend: multiply or screen, chosen by the backdrop.
    Overlay = 14,
    /// Darken blend: the darker of source and backdrop per channel.
    Darken = 15,
    /// Lighten blend: the lighter of source and backdrop per channel.
    Lighten = 16,
    /// Color dodge blend: brightens the backdrop by the source.
    ColorDodge = 17,
    /// Color burn blend: darkens the backdrop by the source.
    ColorBurn = 18,
    /// Hard light blend: multiply or screen, chosen by the source.
    HardLight = 19,
    /// Soft light blend: a softer version of hard light.
    SoftLight = 20,
    /// Difference blend: absolute difference of the channels.
    Difference = 21,
    /// Exclusion blend: like difference with lower contrast.
    Exclusion = 22,
    /// Multiply blend: product of the channels, always at least as
    /// dark.
    Multiply = 23,
    /// Hue blend: source hue with backdrop saturation and luminosity.
    HslHue = 24,
    /// Saturation blend: source saturation with backdrop hue and
    /// luminosity.
    HslSaturation = 25,
    /// Color blend: source hue and saturation with backdrop
    /// luminosity.
    HslColor = 26,
    /// Luminosity blend: source luminosity with backdrop hue and
    /// saturation.
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
/// resolve child paints through [`Colr::paint_at`] without doing
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

impl<'a> ColrPaint<'a> {
    /// Parses the `Paint` at `offset` inside `data` (the full COLR
    /// blob). The `offset` is absolute; sub-paints stored as 24-bit
    /// offsets inside a parent format are translated to absolute
    /// values before the variant is returned, so traversal is a
    /// straight chain of [`Colr::paint_at`] calls.
    //
    // The paint tree spans 32 distinct formats so the match is
    // necessarily long. Splitting per-format would hurt locality more
    // than it would help readability, so keep the big `match` intact.
    pub fn parse(data: &'a [u8], offset: usize) -> Result<Self> {
        if offset >= data.len() {
            return Err(Error::Truncated {
                offset,
                context: "Paint offset past end",
            });
        }
        let format = data[offset];
        let base = offset;
        // Helper closure: read a u24 child-paint offset at (base + k)
        // and return the absolute offset.
        let read_off24 = |k: usize| -> Result<u32> {
            if base + k + 3 > data.len() {
                return Err(Error::Truncated {
                    offset: base + k,
                    context: "Paint sub-offset truncated",
                });
            }
            let rel =
                u32::from_be_bytes([0, data[base + k], data[base + k + 1], data[base + k + 2]]);
            (base as u32).checked_add(rel).ok_or(Error::Malformed {
                offset: base + k,
                context: "Paint sub-offset overflow",
            })
        };
        let read_u8 = |k: usize| -> Result<u8> {
            data.get(base + k).copied().ok_or(Error::Truncated {
                offset: base + k,
                context: "u8",
            })
        };
        let read_u16 = |k: usize| -> Result<u16> {
            if base + k + 2 > data.len() {
                return Err(Error::Truncated {
                    offset: base + k,
                    context: "u16",
                });
            }
            Ok(u16::from_be_bytes([data[base + k], data[base + k + 1]]))
        };
        let read_i16 = |k: usize| -> Result<i16> {
            if base + k + 2 > data.len() {
                return Err(Error::Truncated {
                    offset: base + k,
                    context: "i16",
                });
            }
            Ok(i16::from_be_bytes([data[base + k], data[base + k + 1]]))
        };
        let read_u32 = |k: usize| -> Result<u32> {
            if base + k + 4 > data.len() {
                return Err(Error::Truncated {
                    offset: base + k,
                    context: "u32",
                });
            }
            Ok(u32::from_be_bytes([
                data[base + k],
                data[base + k + 1],
                data[base + k + 2],
                data[base + k + 3],
            ]))
        };
        let read_f2dot14 = |k: usize| -> Result<F2Dot14> { Ok(read_i16(k)? as f32 / 16384.0) };
        // ColorLine / VarColorLine layout:
        //   u8 extend
        //   u16 numStops
        //   ColorStop[numStops]  (6 bytes each for ColorLine,
        //                         10 bytes for VarColorLine)
        let read_color_line = |k: usize, variable: bool| -> Result<(ColorLine<'a>, usize)> {
            // The gradient formats store the color line as a
            // u24 sub-offset (relative to the paint record); we
            // resolve it here so callers see a straight
            // `ColorLine<'a>`.
            if base + k + 3 > data.len() {
                return Err(Error::Truncated {
                    offset: base + k,
                    context: "ColorLine offset truncated",
                });
            }
            let rel =
                u32::from_be_bytes([0, data[base + k], data[base + k + 1], data[base + k + 2]])
                    as usize;
            let cl_start = base + rel;
            if cl_start + 3 > data.len() {
                return Err(Error::Truncated {
                    offset: cl_start,
                    context: "ColorLine body truncated",
                });
            }
            let extend = Extend::from_u8(data[cl_start]);
            let num_stops = u16::from_be_bytes([data[cl_start + 1], data[cl_start + 2]]);
            let stop_size = if variable { 10 } else { 6 };
            let stops_start = cl_start + 3;
            let stops_end = stops_start + num_stops as usize * stop_size;
            if stops_end > data.len() {
                return Err(Error::Truncated {
                    offset: stops_end,
                    context: "ColorLine stops truncated",
                });
            }
            Ok((
                ColorLine {
                    extend,
                    stops: &data[stops_start..stops_end],
                    stop_count: num_stops,
                    variable,
                },
                3, // consumed bytes at `k`
            ))
        };

        Ok(match format {
            // 1. PaintColrLayers: { u8 format; u8 numLayers; u32 firstLayerIndex }
            1 => Self::ColrLayers {
                num_layers: read_u8(1)?,
                first_layer_index: read_u32(2)?,
            },
            // 2. PaintSolid: { u8 format; u16 paletteIndex; F2Dot14 alpha }
            2 => Self::Solid {
                palette_index: read_u16(1)?,
                alpha: read_f2dot14(3)?,
            },
            // 3. PaintVarSolid: + u32 varIndexBase
            3 => Self::VarSolid {
                palette_index: read_u16(1)?,
                alpha: read_f2dot14(3)?,
                var_index_base: read_u32(5)?,
            },
            // 4. PaintLinearGradient:
            //    { u8 format; Offset24 colorLine; FWORD x0..y2 (6 words) }
            4 => {
                let (color_line, _) = read_color_line(1, false)?;
                Self::LinearGradient {
                    color_line,
                    x0: read_i16(4)?,
                    y0: read_i16(6)?,
                    x1: read_i16(8)?,
                    y1: read_i16(10)?,
                    x2: read_i16(12)?,
                    y2: read_i16(14)?,
                }
            }
            // 5. PaintVarLinearGradient: + varIndexBase
            5 => {
                let (color_line, _) = read_color_line(1, true)?;
                Self::VarLinearGradient {
                    color_line,
                    x0: read_i16(4)?,
                    y0: read_i16(6)?,
                    x1: read_i16(8)?,
                    y1: read_i16(10)?,
                    x2: read_i16(12)?,
                    y2: read_i16(14)?,
                    var_index_base: read_u32(16)?,
                }
            }
            // 6. PaintRadialGradient: colorLine + (x0,y0,r0,x1,y1,r1) all i16/u16
            6 => {
                let (color_line, _) = read_color_line(1, false)?;
                Self::RadialGradient {
                    color_line,
                    x0: read_i16(4)?,
                    y0: read_i16(6)?,
                    r0: read_u16(8)?,
                    x1: read_i16(10)?,
                    y1: read_i16(12)?,
                    r1: read_u16(14)?,
                }
            }
            // 7. PaintVarRadialGradient
            7 => {
                let (color_line, _) = read_color_line(1, true)?;
                Self::VarRadialGradient {
                    color_line,
                    x0: read_i16(4)?,
                    y0: read_i16(6)?,
                    r0: read_u16(8)?,
                    x1: read_i16(10)?,
                    y1: read_i16(12)?,
                    r1: read_u16(14)?,
                    var_index_base: read_u32(16)?,
                }
            }
            // 8. PaintSweepGradient: colorLine + centerX + centerY + startAngle + endAngle
            8 => {
                let (color_line, _) = read_color_line(1, false)?;
                Self::SweepGradient {
                    color_line,
                    center_x: read_i16(4)?,
                    center_y: read_i16(6)?,
                    start_angle: read_f2dot14(8)?,
                    end_angle: read_f2dot14(10)?,
                }
            }
            // 9. PaintVarSweepGradient
            9 => {
                let (color_line, _) = read_color_line(1, true)?;
                Self::VarSweepGradient {
                    color_line,
                    center_x: read_i16(4)?,
                    center_y: read_i16(6)?,
                    start_angle: read_f2dot14(8)?,
                    end_angle: read_f2dot14(10)?,
                    var_index_base: read_u32(12)?,
                }
            }
            // 10. PaintGlyph: { u8 format; Offset24 paint; u16 glyphID }
            10 => Self::Glyph {
                paint_offset: read_off24(1)?,
                glyph_id: read_u16(4)?,
            },
            // 11. PaintColrGlyph: { u8 format; u16 glyphID }
            11 => Self::ColrGlyph {
                glyph_id: read_u16(1)?,
            },
            // 12. PaintTransform: Offset24 paint + Offset24 affine
            12 => {
                // The 2x3 Affine2x3 table referenced by the sub-offset
                // has layout: Fixed xx, yx, xy, yy, dx, dy (6 * 4 bytes).
                let paint_offset = read_off24(1)?;
                if base + 4 + 3 > data.len() {
                    return Err(Error::Truncated {
                        offset: base + 4,
                        context: "PaintTransform affine offset truncated",
                    });
                }
                let affine_rel =
                    u32::from_be_bytes([0, data[base + 4], data[base + 5], data[base + 6]])
                        as usize;
                let a_start = base + affine_rel;
                if a_start + 24 > data.len() {
                    return Err(Error::Truncated {
                        offset: a_start,
                        context: "Affine2x3 body truncated",
                    });
                }
                let read_aff = |o: usize| -> f32 {
                    let raw = i32::from_be_bytes([
                        data[a_start + o],
                        data[a_start + o + 1],
                        data[a_start + o + 2],
                        data[a_start + o + 3],
                    ]);
                    raw as f32 / 65536.0
                };
                Self::Transform {
                    paint_offset,
                    xx: read_aff(0),
                    yx: read_aff(4),
                    xy: read_aff(8),
                    yy: read_aff(12),
                    dx: read_aff(16),
                    dy: read_aff(20),
                }
            }
            // 13. PaintVarTransform: same, VarAffine2x3 includes varIndexBase
            13 => {
                let paint_offset = read_off24(1)?;
                if base + 7 > data.len() {
                    return Err(Error::Truncated {
                        offset: base + 4,
                        context: "PaintVarTransform affine offset truncated",
                    });
                }
                let affine_rel =
                    u32::from_be_bytes([0, data[base + 4], data[base + 5], data[base + 6]])
                        as usize;
                let a_start = base + affine_rel;
                if a_start + 28 > data.len() {
                    return Err(Error::Truncated {
                        offset: a_start,
                        context: "VarAffine2x3 body truncated",
                    });
                }
                let read_aff = |o: usize| -> f32 {
                    let raw = i32::from_be_bytes([
                        data[a_start + o],
                        data[a_start + o + 1],
                        data[a_start + o + 2],
                        data[a_start + o + 3],
                    ]);
                    raw as f32 / 65536.0
                };
                let var_index_base = u32::from_be_bytes([
                    data[a_start + 24],
                    data[a_start + 25],
                    data[a_start + 26],
                    data[a_start + 27],
                ]);
                Self::VarTransform {
                    paint_offset,
                    xx: read_aff(0),
                    yx: read_aff(4),
                    xy: read_aff(8),
                    yy: read_aff(12),
                    dx: read_aff(16),
                    dy: read_aff(20),
                    var_index_base,
                }
            }
            // 14. PaintTranslate: Offset24 paint + dx + dy (i16)
            14 => Self::Translate {
                paint_offset: read_off24(1)?,
                dx: read_i16(4)?,
                dy: read_i16(6)?,
            },
            15 => Self::VarTranslate {
                paint_offset: read_off24(1)?,
                dx: read_i16(4)?,
                dy: read_i16(6)?,
                var_index_base: read_u32(8)?,
            },
            16 => Self::Scale {
                paint_offset: read_off24(1)?,
                scale_x: read_f2dot14(4)?,
                scale_y: read_f2dot14(6)?,
            },
            17 => Self::VarScale {
                paint_offset: read_off24(1)?,
                scale_x: read_f2dot14(4)?,
                scale_y: read_f2dot14(6)?,
                var_index_base: read_u32(8)?,
            },
            18 => Self::ScaleAroundCenter {
                paint_offset: read_off24(1)?,
                scale_x: read_f2dot14(4)?,
                scale_y: read_f2dot14(6)?,
                center_x: read_i16(8)?,
                center_y: read_i16(10)?,
            },
            19 => Self::VarScaleAroundCenter {
                paint_offset: read_off24(1)?,
                scale_x: read_f2dot14(4)?,
                scale_y: read_f2dot14(6)?,
                center_x: read_i16(8)?,
                center_y: read_i16(10)?,
                var_index_base: read_u32(12)?,
            },
            20 => Self::ScaleUniform {
                paint_offset: read_off24(1)?,
                scale: read_f2dot14(4)?,
            },
            21 => Self::VarScaleUniform {
                paint_offset: read_off24(1)?,
                scale: read_f2dot14(4)?,
                var_index_base: read_u32(6)?,
            },
            22 => Self::ScaleUniformAroundCenter {
                paint_offset: read_off24(1)?,
                scale: read_f2dot14(4)?,
                center_x: read_i16(6)?,
                center_y: read_i16(8)?,
            },
            23 => Self::VarScaleUniformAroundCenter {
                paint_offset: read_off24(1)?,
                scale: read_f2dot14(4)?,
                center_x: read_i16(6)?,
                center_y: read_i16(8)?,
                var_index_base: read_u32(10)?,
            },
            24 => Self::Rotate {
                paint_offset: read_off24(1)?,
                angle: read_f2dot14(4)?,
            },
            25 => Self::VarRotate {
                paint_offset: read_off24(1)?,
                angle: read_f2dot14(4)?,
                var_index_base: read_u32(6)?,
            },
            26 => Self::RotateAroundCenter {
                paint_offset: read_off24(1)?,
                angle: read_f2dot14(4)?,
                center_x: read_i16(6)?,
                center_y: read_i16(8)?,
            },
            27 => Self::VarRotateAroundCenter {
                paint_offset: read_off24(1)?,
                angle: read_f2dot14(4)?,
                center_x: read_i16(6)?,
                center_y: read_i16(8)?,
                var_index_base: read_u32(10)?,
            },
            28 => Self::Skew {
                paint_offset: read_off24(1)?,
                x_skew_angle: read_f2dot14(4)?,
                y_skew_angle: read_f2dot14(6)?,
            },
            29 => Self::VarSkew {
                paint_offset: read_off24(1)?,
                x_skew_angle: read_f2dot14(4)?,
                y_skew_angle: read_f2dot14(6)?,
                var_index_base: read_u32(8)?,
            },
            30 => Self::SkewAroundCenter {
                paint_offset: read_off24(1)?,
                x_skew_angle: read_f2dot14(4)?,
                y_skew_angle: read_f2dot14(6)?,
                center_x: read_i16(8)?,
                center_y: read_i16(10)?,
            },
            31 => Self::VarSkewAroundCenter {
                paint_offset: read_off24(1)?,
                x_skew_angle: read_f2dot14(4)?,
                y_skew_angle: read_f2dot14(6)?,
                center_x: read_i16(8)?,
                center_y: read_i16(10)?,
                var_index_base: read_u32(12)?,
            },
            32 => Self::Composite {
                source_paint_offset: read_off24(1)?,
                composite_mode: CompositeMode::from_u8(read_u8(4)?),
                backdrop_paint_offset: read_off24(5)?,
            },
            _ => {
                return Err(Error::Unsupported {
                    context: "unknown COLR v1 paint format",
                });
            }
        })
    }

    /// Returns every absolute child-paint offset this node carries.
    /// Callers drive a depth-first traversal by feeding these back
    /// into [`Colr::paint_at`].
    #[must_use]
    pub fn child_paint_offsets(&self) -> Vec<PaintOffset> {
        match *self {
            Self::Glyph { paint_offset, .. }
            | Self::Transform { paint_offset, .. }
            | Self::VarTransform { paint_offset, .. }
            | Self::Translate { paint_offset, .. }
            | Self::VarTranslate { paint_offset, .. }
            | Self::Scale { paint_offset, .. }
            | Self::VarScale { paint_offset, .. }
            | Self::ScaleAroundCenter { paint_offset, .. }
            | Self::VarScaleAroundCenter { paint_offset, .. }
            | Self::ScaleUniform { paint_offset, .. }
            | Self::VarScaleUniform { paint_offset, .. }
            | Self::ScaleUniformAroundCenter { paint_offset, .. }
            | Self::VarScaleUniformAroundCenter { paint_offset, .. }
            | Self::Rotate { paint_offset, .. }
            | Self::VarRotate { paint_offset, .. }
            | Self::RotateAroundCenter { paint_offset, .. }
            | Self::VarRotateAroundCenter { paint_offset, .. }
            | Self::Skew { paint_offset, .. }
            | Self::VarSkew { paint_offset, .. }
            | Self::SkewAroundCenter { paint_offset, .. }
            | Self::VarSkewAroundCenter { paint_offset, .. } => alloc::vec![paint_offset],
            Self::Composite {
                source_paint_offset,
                backdrop_paint_offset,
                ..
            } => alloc::vec![source_paint_offset, backdrop_paint_offset],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn build_colr_v0(base: &[(u16, u16, u16)], layers: &[(u16, u16)]) -> Vec<u8> {
        // Header is 14 bytes (v0). Then base records, then layer records.
        let base_records_off: u32 = 14;
        let base_bytes = (base.len() * 6) as u32;
        let layer_records_off = base_records_off + base_bytes;
        let mut b = Vec::new();
        b.extend_from_slice(&0u16.to_be_bytes()); // version
        b.extend_from_slice(&(base.len() as u16).to_be_bytes());
        b.extend_from_slice(&base_records_off.to_be_bytes());
        b.extend_from_slice(&layer_records_off.to_be_bytes());
        b.extend_from_slice(&(layers.len() as u16).to_be_bytes());
        for (gid, first, count) in base {
            b.extend_from_slice(&gid.to_be_bytes());
            b.extend_from_slice(&first.to_be_bytes());
            b.extend_from_slice(&count.to_be_bytes());
        }
        for (gid, pal) in layers {
            b.extend_from_slice(&gid.to_be_bytes());
            b.extend_from_slice(&pal.to_be_bytes());
        }
        b
    }

    #[test]
    fn v0_layer_lookup_round_trip() {
        let bytes = build_colr_v0(
            &[(3, 0, 2), (7, 2, 3)],
            &[(10, 0), (11, 1), (20, 0), (21, 1), (22, 2)],
        );
        let colr = Colr::parse(&bytes).unwrap();
        assert_eq!(colr.version(), 0);
        assert_eq!(colr.num_base_glyph_records(), 2);
        assert_eq!(colr.num_layer_records(), 5);

        let layers = colr.v0_layers(3).unwrap();
        assert_eq!(layers.len(), 2);
        let collected: Vec<V0Layer> = layers.iter().collect();
        assert_eq!(
            collected,
            alloc::vec![
                V0Layer {
                    glyph_id: 10,
                    palette_index: 0
                },
                V0Layer {
                    glyph_id: 11,
                    palette_index: 1
                },
            ]
        );

        let l7 = colr.v0_layers(7).unwrap();
        assert_eq!(l7.len(), 3);
        assert_eq!(l7.get(2).unwrap().glyph_id, 22);

        assert!(colr.v0_layers(99).is_none());
    }

    #[test]
    fn rejects_unknown_colr_version() {
        let mut bytes = build_colr_v0(&[], &[]);
        bytes[0..2].copy_from_slice(&9u16.to_be_bytes());
        assert!(matches!(Colr::parse(&bytes), Err(Error::Malformed { .. })));
    }

    // ---------------- v1 paint-tree tests ----------------

    /// Build a minimal v1 COLR with a single base glyph whose paint
    /// tree is a `PaintSolid`. Returns the full table bytes.
    fn build_colr_v1_solid(palette_index: u16, alpha: f32) -> Vec<u8> {
        let mut out = Vec::new();
        // v1 header = 14 bytes (v0) + 16 bytes (4x u32).
        let header_len = 14 + 16;
        out.extend_from_slice(&1u16.to_be_bytes()); // version
        out.extend_from_slice(&0u16.to_be_bytes()); // numBaseGlyphRecords
        out.extend_from_slice(&(header_len as u32).to_be_bytes()); // baseGlyphRecordsOffset (empty body so anywhere works)
        out.extend_from_slice(&(header_len as u32).to_be_bytes()); // layerRecordsOffset
        out.extend_from_slice(&0u16.to_be_bytes()); // numLayerRecords
                                                    // v1 appendix: baseGlyphListOffset, layerListOffset,
                                                    // clipListOffset, varStoreOffset.
        let base_glyph_list_off = header_len as u32;
        out.extend_from_slice(&base_glyph_list_off.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // layerListOffset
        out.extend_from_slice(&0u32.to_be_bytes()); // clipListOffset
        out.extend_from_slice(&0u32.to_be_bytes()); // varStoreOffset

        // BaseGlyphList: { u32 numRecords; BaseGlyphPaintRecord[...] }.
        // BaseGlyphPaintRecord = { u16 glyphID; Offset32 paintOffset (relative to BaseGlyphList) }.
        out.extend_from_slice(&1u32.to_be_bytes()); // numRecords
        out.extend_from_slice(&42u16.to_be_bytes()); // glyphID
                                                     // paintOffset relative to BaseGlyphList start. The list header
                                                     // is 4 bytes + 6 for the single record = 10 bytes.
        out.extend_from_slice(&10u32.to_be_bytes());

        // Paint body: format=2 (PaintSolid) + u16 palette + F2Dot14 alpha.
        out.push(2);
        out.extend_from_slice(&palette_index.to_be_bytes());
        let alpha_raw = (alpha * 16384.0) as i16;
        out.extend_from_slice(&alpha_raw.to_be_bytes());
        out
    }

    #[test]
    fn v1_solid_paint_round_trip() {
        let bytes = build_colr_v1_solid(5, 0.5);
        let colr = Colr::parse(&bytes).unwrap();
        assert_eq!(colr.version(), 1);
        assert!(colr.has_v1());

        let paint = colr.paint(42).expect("glyph 42 has a paint");
        match paint {
            ColrPaint::Solid {
                palette_index,
                alpha,
            } => {
                assert_eq!(palette_index, 5);
                assert!((alpha - 0.5).abs() < 1e-3);
            }
            _ => panic!("expected Solid, got {paint:?}"),
        }
    }

    /// PaintColrLayers at the root of a base glyph.
    #[test]
    fn v1_colr_layers_round_trip() {
        let mut out = Vec::new();
        let header_len = 30;
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&(header_len as u32).to_be_bytes());
        out.extend_from_slice(&(header_len as u32).to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        let base_glyph_list_off = header_len as u32;
        out.extend_from_slice(&base_glyph_list_off.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());

        // BaseGlyphList.
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&9u16.to_be_bytes());
        out.extend_from_slice(&10u32.to_be_bytes());

        // Paint body: format=1 (ColrLayers), numLayers=3, firstLayerIndex=7.
        out.push(1);
        out.push(3);
        out.extend_from_slice(&7u32.to_be_bytes());

        let colr = Colr::parse(&out).unwrap();
        let paint = colr.paint(9).unwrap();
        match paint {
            ColrPaint::ColrLayers {
                num_layers,
                first_layer_index,
            } => {
                assert_eq!(num_layers, 3);
                assert_eq!(first_layer_index, 7);
            }
            _ => panic!("expected ColrLayers"),
        }
    }

    /// PaintGlyph wraps a PaintSolid, exercising Offset24 child
    /// resolution + depth-one traversal via `paint_at`.
    #[test]
    fn v1_glyph_paint_chains_to_solid() {
        let mut out = Vec::new();
        let header_len = 30;
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&(header_len as u32).to_be_bytes());
        out.extend_from_slice(&(header_len as u32).to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        let base_glyph_list_off = header_len as u32;
        out.extend_from_slice(&base_glyph_list_off.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());

        // BaseGlyphList: one record pointing at paint at offset 10
        // (relative to list start).
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&100u16.to_be_bytes());
        out.extend_from_slice(&10u32.to_be_bytes());

        // PaintGlyph: format=10, Offset24 to child, u16 glyph.
        let paint_glyph_start = out.len();
        out.push(10);
        // Reserve 3 bytes for the Offset24, fill later.
        out.extend_from_slice(&[0, 0, 0]);
        out.extend_from_slice(&200u16.to_be_bytes()); // child glyph id

        // Pad so the next paint starts aligned.
        let solid_start = out.len();
        let offset24 = (solid_start - paint_glyph_start) as u32;
        out[paint_glyph_start + 1] = ((offset24 >> 16) & 0xff) as u8;
        out[paint_glyph_start + 2] = ((offset24 >> 8) & 0xff) as u8;
        out[paint_glyph_start + 3] = (offset24 & 0xff) as u8;

        // Child PaintSolid.
        out.push(2);
        out.extend_from_slice(&8u16.to_be_bytes());
        out.extend_from_slice(&16384i16.to_be_bytes()); // 1.0

        let colr = Colr::parse(&out).unwrap();
        let root = colr.paint(100).unwrap();
        let child_offset = match root {
            ColrPaint::Glyph {
                paint_offset,
                glyph_id,
            } => {
                assert_eq!(glyph_id, 200);
                paint_offset
            }
            _ => panic!("expected Glyph"),
        };
        let child = colr.paint_at(child_offset).unwrap();
        match child {
            ColrPaint::Solid {
                palette_index,
                alpha,
            } => {
                assert_eq!(palette_index, 8);
                assert!((alpha - 1.0).abs() < 1e-3);
            }
            _ => panic!("expected Solid child"),
        }

        // Verify the child-offset iteration helper sees exactly
        // the one child.
        let kids = root.child_paint_offsets();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0], child_offset);
    }

    /// LinearGradient exercises the ColorLine sub-offset + FWORD
    /// coordinate parsing. Coordinates and stop values are handed
    /// back byte-for-byte.
    #[test]
    fn v1_linear_gradient_parses_colorline_and_coords() {
        let mut out = Vec::new();
        let header_len = 30;
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&(header_len as u32).to_be_bytes());
        out.extend_from_slice(&(header_len as u32).to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        let base_glyph_list_off = header_len as u32;
        out.extend_from_slice(&base_glyph_list_off.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());

        out.extend_from_slice(&1u32.to_be_bytes()); // base-glyph count
        out.extend_from_slice(&33u16.to_be_bytes());
        out.extend_from_slice(&10u32.to_be_bytes()); // paint offset rel to list

        // PaintLinearGradient body:
        //   u8 format=4
        //   Offset24 colorLineOffset (relative to this paint)
        //   6x i16 (x0..y2)
        let paint_start = out.len();
        out.push(4);
        // Offset24 placeholder, patched after we know ColorLine offset.
        out.extend_from_slice(&[0, 0, 0]);
        out.extend_from_slice(&10i16.to_be_bytes()); // x0
        out.extend_from_slice(&20i16.to_be_bytes()); // y0
        out.extend_from_slice(&30i16.to_be_bytes()); // x1
        out.extend_from_slice(&40i16.to_be_bytes()); // y1
        out.extend_from_slice(&50i16.to_be_bytes()); // x2
        out.extend_from_slice(&60i16.to_be_bytes()); // y2

        let cl_start = out.len();
        let cl_rel = (cl_start - paint_start) as u32;
        out[paint_start + 1] = ((cl_rel >> 16) & 0xff) as u8;
        out[paint_start + 2] = ((cl_rel >> 8) & 0xff) as u8;
        out[paint_start + 3] = (cl_rel & 0xff) as u8;

        // ColorLine: u8 extend=0 (pad), u16 numStops=2, 2 stops (6 bytes each).
        out.push(0);
        out.extend_from_slice(&2u16.to_be_bytes());
        // Stop 0: offset=0.0, palette=1, alpha=1.0.
        out.extend_from_slice(&0i16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&16384i16.to_be_bytes());
        // Stop 1: offset=1.0, palette=2, alpha=1.0.
        out.extend_from_slice(&16384i16.to_be_bytes());
        out.extend_from_slice(&2u16.to_be_bytes());
        out.extend_from_slice(&16384i16.to_be_bytes());

        let colr = Colr::parse(&out).unwrap();
        let paint = colr.paint(33).unwrap();
        match paint {
            ColrPaint::LinearGradient {
                color_line,
                x0,
                y0,
                x1,
                y1,
                x2,
                y2,
            } => {
                assert_eq!(x0, 10);
                assert_eq!(y0, 20);
                assert_eq!(x1, 30);
                assert_eq!(y1, 40);
                assert_eq!(x2, 50);
                assert_eq!(y2, 60);
                assert_eq!(color_line.extend, Extend::Pad);
                assert_eq!(color_line.len(), 2);
                let stops: Vec<_> = color_line.stops().collect();
                assert_eq!(stops.len(), 2);
                assert_eq!(stops[0].palette_index, 1);
                assert_eq!(stops[1].palette_index, 2);
            }
            _ => panic!("expected LinearGradient, got {paint:?}"),
        }
    }

    /// PaintComposite carries two child offsets; make sure both land
    /// in `child_paint_offsets()`.
    #[test]
    fn v1_composite_exposes_both_children() {
        // Build a minimal header pointing at a composite at the end.
        let mut out = Vec::new();
        let header_len = 30;
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&(header_len as u32).to_be_bytes());
        out.extend_from_slice(&(header_len as u32).to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        let base_glyph_list_off = header_len as u32;
        out.extend_from_slice(&base_glyph_list_off.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());

        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&7u16.to_be_bytes());
        out.extend_from_slice(&10u32.to_be_bytes());

        // PaintComposite:
        //   u8 format=32, Offset24 source, u8 mode, Offset24 backdrop.
        let composite_start = out.len();
        out.push(32);
        out.extend_from_slice(&[0, 0, 0]); // source
        out.push(3); // mode = SrcOver
        out.extend_from_slice(&[0, 0, 0]); // backdrop

        // Child source paint: PaintSolid.
        let src_start = out.len();
        let src_rel = (src_start - composite_start) as u32;
        out[composite_start + 1] = ((src_rel >> 16) & 0xff) as u8;
        out[composite_start + 2] = ((src_rel >> 8) & 0xff) as u8;
        out[composite_start + 3] = (src_rel & 0xff) as u8;
        out.push(2);
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&16384i16.to_be_bytes());

        // Child backdrop paint: PaintSolid.
        let bd_start = out.len();
        let bd_rel = (bd_start - composite_start) as u32;
        out[composite_start + 5] = ((bd_rel >> 16) & 0xff) as u8;
        out[composite_start + 6] = ((bd_rel >> 8) & 0xff) as u8;
        out[composite_start + 7] = (bd_rel & 0xff) as u8;
        out.push(2);
        out.extend_from_slice(&2u16.to_be_bytes());
        out.extend_from_slice(&16384i16.to_be_bytes());

        let colr = Colr::parse(&out).unwrap();
        let root = colr.paint(7).unwrap();
        let kids = root.child_paint_offsets();
        assert_eq!(kids.len(), 2);
        match root {
            ColrPaint::Composite { composite_mode, .. } => {
                assert_eq!(composite_mode, CompositeMode::SrcOver);
            }
            _ => panic!("expected Composite"),
        }
        // Both children resolve to Solid.
        for off in kids {
            let child = colr.paint_at(off).unwrap();
            assert!(matches!(child, ColrPaint::Solid { .. }));
        }
    }

    /// Spot-check translate, scale, rotate, skew. Each should
    /// round-trip its single transform argument.
    #[test]
    fn v1_simple_transform_variants_round_trip() {
        fn build_single_transform(format: u8, payload: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            let header_len = 30;
            out.extend_from_slice(&1u16.to_be_bytes());
            out.extend_from_slice(&0u16.to_be_bytes());
            out.extend_from_slice(&(header_len as u32).to_be_bytes());
            out.extend_from_slice(&(header_len as u32).to_be_bytes());
            out.extend_from_slice(&0u16.to_be_bytes());
            let bgl = header_len as u32;
            out.extend_from_slice(&bgl.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&1u32.to_be_bytes());
            out.extend_from_slice(&1u16.to_be_bytes());
            out.extend_from_slice(&10u32.to_be_bytes());
            let pstart = out.len();
            out.push(format);
            out.extend_from_slice(&[0, 0, 0]); // Offset24 child (patched below)
            out.extend_from_slice(payload);
            let cstart = out.len();
            let rel = (cstart - pstart) as u32;
            out[pstart + 1] = ((rel >> 16) & 0xff) as u8;
            out[pstart + 2] = ((rel >> 8) & 0xff) as u8;
            out[pstart + 3] = (rel & 0xff) as u8;
            // Child Solid so the tree is well-formed.
            out.push(2);
            out.extend_from_slice(&0u16.to_be_bytes());
            out.extend_from_slice(&16384i16.to_be_bytes());
            out
        }

        // Format 14 (Translate): dx=5, dy=-7 (i16 each).
        let bytes = build_single_transform(14, &[0, 5, 0xff, 0xf9]);
        let colr = Colr::parse(&bytes).unwrap();
        match colr.paint(1).unwrap() {
            ColrPaint::Translate { dx, dy, .. } => {
                assert_eq!(dx, 5);
                assert_eq!(dy, -7);
            }
            p => panic!("expected Translate, got {p:?}"),
        }

        // Format 20 (ScaleUniform): scale=0.5 (F2Dot14 = 8192).
        let bytes = build_single_transform(20, &[0x20, 0x00]);
        let colr = Colr::parse(&bytes).unwrap();
        match colr.paint(1).unwrap() {
            ColrPaint::ScaleUniform { scale, .. } => {
                assert!((scale - 0.5).abs() < 1e-3);
            }
            p => panic!("expected ScaleUniform, got {p:?}"),
        }

        // Format 24 (Rotate): angle=0.25 (= F2Dot14 4096).
        let bytes = build_single_transform(24, &[0x10, 0x00]);
        let colr = Colr::parse(&bytes).unwrap();
        match colr.paint(1).unwrap() {
            ColrPaint::Rotate { angle, .. } => {
                assert!((angle - 0.25).abs() < 1e-3);
            }
            p => panic!("expected Rotate, got {p:?}"),
        }
    }
}
