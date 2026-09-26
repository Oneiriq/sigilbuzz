//! `MATH`: OpenType math typography table.
//!
//! Math fonts (STIX 2 Math, Latin Modern Math, Cambria Math, Asana Math,
//! XITS Math, ...) ship a `MATH` table that math-typesetting engines like
//! LuaTeX and MathML renderers consume to lay out equations. The table
//! is divided into five logically distinct subtables:
//!
//! - [`MathConstants`]: ~70 font-wide layout constants (script scale
//!   percentages, fraction-rule shifts, radical inset, etc.).
//! - [`MathGlyphInfo`]: per-glyph italic correction, top-accent
//!   attachment, an "is extended shape" bitmap, and per-corner kerning.
//! - [`MathKern`]: piecewise math kerning that varies with the
//!   secondary glyph's vertical position.
//! - [`MathVariants`]: stretchy-glyph variant lists and assembly
//!   parts for tall operators (∑ ∫ ⎰ ⎱ ⎛ ⎜ ⎝ ...).
//!
//! sigilbuzz parses the data; *evaluating* it (running a math layout
//! pass) is the consumer's job, just like `COLR` paint evaluation. All
//! views are zero-copy, borrowing `&'a [u8]` into the original `MATH`
//! table bytes.
//!
//! Reference: <https://learn.microsoft.com/en-us/typography/opentype/spec/math>.

mod constants;
mod glyph_info;
mod variants;

use crate::error::{Error, Result};
use crate::tables::layout::DeviceOrVariationIndex;
use crate::tables::parse::Reader;

pub use constants::MathConstants;
pub use glyph_info::{KernSide, MathGlyphInfo, MathKern};
pub use variants::{
    GlyphAssembly, GlyphConstruction, GlyphPart, MathGlyphVariant, MathVariants, PART_FLAG_EXTENDER,
};

// =========================================================================
// MathValueRecord
// =========================================================================

/// A `MathValueRecord`: every numeric field in the MATH table is one
/// of these. The `value` is a design-unit (`FWord`) scalar; the optional
/// `device` references a Device or VariationIndex table that adjusts
/// the value at runtime (per-ppem hinting deltas or variable-font
/// axis-driven deltas, respectively).
///
/// The Device offset is parsed lazily into [`DeviceOrVariationIndex`];
/// callers that only need the design-unit value can ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MathValue<'a> {
    /// Design-unit value of the record.
    pub value: i16,
    /// Resolved Device / VariationIndex sub-table, or `None` when the
    /// font emits the spec's "absent" sentinel (offset = 0).
    pub device: Option<DeviceOrVariationIndex>,
    /// Slice of the enclosing MATH table, kept so callers can later
    /// resolve any embedded VariationIndex against an
    /// `ItemVariationStore` if one is wired in.
    _table: &'a [u8],
}

// MathValueRecord is 4 bytes (i16 value + u16 Device/VariationIndex
// offset relative to the enclosing subtable). Each subtable inlines
// the read so it can use the right `data` slice as the device base.

// =========================================================================
// MATH header
// =========================================================================

/// Parsed `MATH` table.
///
/// The header is just three offsets: MathConstants, MathGlyphInfo,
/// MathVariants. Each is parsed lazily on the matching accessor so a
/// font missing one of them costs nothing.
#[derive(Debug, Clone, Copy)]
pub struct Math<'a> {
    data: &'a [u8],
    constants_off: u16,
    glyph_info_off: u16,
    variants_off: u16,
}

impl<'a> Math<'a> {
    /// Parses a `MATH` table header.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported MATH major version",
            });
        }
        let _ = minor;
        let constants_off = r.read_u16()?;
        let glyph_info_off = r.read_u16()?;
        let variants_off = r.read_u16()?;

        // Validate non-zero offsets fit inside the table.
        for (off, ctx) in [
            (constants_off, "MathConstants offset past end"),
            (glyph_info_off, "MathGlyphInfo offset past end"),
            (variants_off, "MathVariants offset past end"),
        ] {
            if off != 0 && off as usize > data.len() {
                return Err(Error::Malformed {
                    offset: off as usize,
                    context: ctx,
                });
            }
        }

        Ok(Self {
            data,
            constants_off,
            glyph_info_off,
            variants_off,
        })
    }

    /// Returns the [`MathConstants`] subtable, or `None` when the font
    /// does not provide one (rare: every real math font ships it).
    pub fn constants(&self) -> Result<Option<MathConstants<'a>>> {
        if self.constants_off == 0 {
            return Ok(None);
        }
        let bytes = self
            .data
            .get(self.constants_off as usize..)
            .ok_or(Error::Malformed {
                offset: self.constants_off as usize,
                context: "MathConstants offset past end",
            })?;
        MathConstants::parse(bytes).map(Some)
    }

    /// Returns the [`MathGlyphInfo`] subtable, or `None` when absent.
    pub fn glyph_info(&self) -> Result<Option<MathGlyphInfo<'a>>> {
        if self.glyph_info_off == 0 {
            return Ok(None);
        }
        let bytes = self
            .data
            .get(self.glyph_info_off as usize..)
            .ok_or(Error::Malformed {
                offset: self.glyph_info_off as usize,
                context: "MathGlyphInfo offset past end",
            })?;
        MathGlyphInfo::parse(bytes).map(Some)
    }

    /// Returns the [`MathVariants`] subtable, or `None` when absent.
    /// Fonts without stretchy operator support omit this.
    pub fn variants(&self) -> Result<Option<MathVariants<'a>>> {
        if self.variants_off == 0 {
            return Ok(None);
        }
        let bytes = self
            .data
            .get(self.variants_off as usize..)
            .ok_or(Error::Malformed {
                offset: self.variants_off as usize,
                context: "MathVariants offset past end",
            })?;
        MathVariants::parse(bytes).map(Some)
    }
}

#[cfg(test)]
mod tests;
