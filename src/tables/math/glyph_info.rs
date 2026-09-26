//! `MathGlyphInfo` and `MathKern`: per-glyph italic correction,
//! top-accent attachment, extended-shape coverage and corner kerning.

use super::MathValue;
use crate::error::{Error, Result};
use crate::tables::layout::{Coverage, DeviceOrVariationIndex};
use crate::tables::parse::Reader;

// =========================================================================
// MathGlyphInfo
// =========================================================================

/// Which corner a math kerning lookup applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernSide {
    /// Top-right corner of the glyph.
    TopRight,
    /// Top-left corner.
    TopLeft,
    /// Bottom-right corner.
    BottomRight,
    /// Bottom-left corner.
    BottomLeft,
}

/// Per-glyph math information: italic correction, top-accent attachment,
/// extended-shape membership, and the four-corner math kern table.
#[derive(Debug, Clone, Copy)]
pub struct MathGlyphInfo<'a> {
    /// Slice of the MathGlyphInfo subtable.
    data: &'a [u8],
    italic_correction_off: u16,
    top_accent_off: u16,
    extended_shape_off: u16,
    kern_info_off: u16,
}

impl<'a> MathGlyphInfo<'a> {
    /// Parses the four-offset MathGlyphInfo header. Each offset is
    /// relative to the start of the MathGlyphInfo subtable (i.e. `data`).
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let italic_correction_off = r.read_u16()?;
        let top_accent_off = r.read_u16()?;
        let extended_shape_off = r.read_u16()?;
        let kern_info_off = r.read_u16()?;
        for (off, ctx) in [
            (
                italic_correction_off,
                "MathItalicsCorrectionInfo offset past end",
            ),
            (top_accent_off, "MathTopAccentAttachment offset past end"),
            (extended_shape_off, "ExtendedShapeCoverage offset past end"),
            (kern_info_off, "MathKernInfo offset past end"),
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
            italic_correction_off,
            top_accent_off,
            extended_shape_off,
            kern_info_off,
        })
    }

    /// Italic correction (the kern that compensates for an italic
    /// glyph's tilt) for `gid`, or `None` when the font has no entry.
    pub fn italic_correction(&self, gid: u16) -> Option<MathValue<'a>> {
        Self::lookup_value(self.data, self.italic_correction_off, gid)
    }

    /// Top-accent attachment x-offset for `gid` (used to position
    /// combining accents above the glyph), or `None` when absent.
    pub fn top_accent_attachment(&self, gid: u16) -> Option<MathValue<'a>> {
        Self::lookup_value(self.data, self.top_accent_off, gid)
    }

    /// True when `gid` is in the "extended shape" coverage set:
    /// glyphs that already span the math axis and don't need
    /// accent-style superscript shifting.
    #[must_use]
    pub fn is_extended_shape(&self, gid: u16) -> bool {
        if self.extended_shape_off == 0 {
            return false;
        }
        let Some(bytes) = self.data.get(self.extended_shape_off as usize..) else {
            return false;
        };
        let Ok(cov) = Coverage::parse(bytes) else {
            return false;
        };
        cov.contains(gid)
    }

    /// Returns the `MathKernInfo` for `gid` on `side`, or `None`
    /// when no entry covers that corner. The same coverage table
    /// drives all four sides; an absent entry on one side does not
    /// prevent the other three from working.
    pub fn kern_info(&self, gid: u16, side: KernSide) -> Option<MathKern<'a>> {
        if self.kern_info_off == 0 {
            return None;
        }
        let base = self.data.get(self.kern_info_off as usize..)?;
        // MathKernInfo header:
        //   Offset16 mathKernCoverageOffset
        //   uint16   mathKernCount
        //   MathKernInfoRecord[mathKernCount]    (8 bytes each)
        let mut r = Reader::new(base);
        let cov_off = r.read_u16().ok()?;
        let count = r.read_u16().ok()?;
        if cov_off == 0 {
            return None;
        }
        let cov_bytes = base.get(cov_off as usize..)?;
        let cov = Coverage::parse(cov_bytes).ok()?;
        let idx = cov.index_of(gid)? as usize;
        if idx >= count as usize {
            return None;
        }
        let record_off = 4 + idx * 8;
        let record = base.get(record_off..record_off + 8)?;
        let side_off = match side {
            KernSide::TopRight => 0,
            KernSide::TopLeft => 2,
            KernSide::BottomRight => 4,
            KernSide::BottomLeft => 6,
        };
        let kern_off = u16::from_be_bytes([record[side_off], record[side_off + 1]]);
        if kern_off == 0 {
            return None;
        }
        let kern_bytes = base.get(kern_off as usize..)?;
        MathKern::parse(kern_bytes).ok()
    }

    /// Looks up a per-glyph MathValueRecord through a
    /// `MathItalicsCorrectionInfo` / `MathTopAccentAttachment` table
    /// (both share the same `Coverage + array` shape).
    fn lookup_value(data: &'a [u8], info_off: u16, gid: u16) -> Option<MathValue<'a>> {
        if info_off == 0 {
            return None;
        }
        let base = data.get(info_off as usize..)?;
        // Layout:
        //   Offset16 coverageOffset
        //   uint16   count
        //   MathValueRecord[count]   (4 bytes each)
        let mut r = Reader::new(base);
        let cov_off = r.read_u16().ok()?;
        let count = r.read_u16().ok()?;
        if cov_off == 0 {
            return None;
        }
        let cov_bytes = base.get(cov_off as usize..)?;
        let cov = Coverage::parse(cov_bytes).ok()?;
        let idx = cov.index_of(gid)? as usize;
        if idx >= count as usize {
            return None;
        }
        let record_off = 4 + idx * 4;
        let bytes = base.get(record_off..record_off + 4)?;
        let value = i16::from_be_bytes([bytes[0], bytes[1]]);
        let device_off = u16::from_be_bytes([bytes[2], bytes[3]]);
        let device = DeviceOrVariationIndex::parse_from(base, device_off)
            .ok()
            .flatten();
        Some(MathValue {
            value,
            device,
            _table: base,
        })
    }
}

// =========================================================================
// MathKern
// =========================================================================

/// Per-corner math kerning table. Provides a piecewise step function
/// keyed on the *secondary* glyph's vertical position relative to the
/// kerning glyph's baseline.
///
/// The shape is two parallel arrays: `correction_height[i]` (n entries)
/// and `kern_value[i]` (n + 1 entries). For a query height *h*, walk
/// `correction_height` and pick the first `i` where *h <=
/// correction_height\[i\]*; the returned kern is `kern_value[i]`. If *h*
/// exceeds every height, the answer is `kern_value[n]`.
#[derive(Debug, Clone, Copy)]
pub struct MathKern<'a> {
    /// Slice covering exactly the MathKern subtable.
    data: &'a [u8],
    height_count: u16,
}

impl<'a> MathKern<'a> {
    /// Parses a MathKern subtable header, validating array lengths.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let height_count = r.read_u16()?;
        let needed = 2 + (height_count as usize) * 4 + (height_count as usize + 1) * 4;
        if data.len() < needed {
            return Err(Error::Truncated {
                offset: data.len(),
                context: "MathKern arrays truncated",
            });
        }
        Ok(Self { data, height_count })
    }

    /// Number of correction-height steps. The kern value array has
    /// `height_count + 1` entries.
    #[must_use]
    pub const fn height_count(&self) -> u16 {
        self.height_count
    }

    /// Returns the i-th correction-height MathValueRecord, or `None`
    /// for an out-of-range index.
    pub fn correction_height(&self, i: u16) -> Option<MathValue<'a>> {
        if i >= self.height_count {
            return None;
        }
        let off = 2 + (i as usize) * 4;
        let bytes = self.data.get(off..off + 4)?;
        let value = i16::from_be_bytes([bytes[0], bytes[1]]);
        let device_off = u16::from_be_bytes([bytes[2], bytes[3]]);
        let device = DeviceOrVariationIndex::parse_from(self.data, device_off)
            .ok()
            .flatten();
        Some(MathValue {
            value,
            device,
            _table: self.data,
        })
    }

    /// Returns the i-th kern MathValueRecord. Valid `i` is
    /// `0..=height_count`.
    pub fn kern_value(&self, i: u16) -> Option<MathValue<'a>> {
        if i > self.height_count {
            return None;
        }
        let off = 2 + (self.height_count as usize) * 4 + (i as usize) * 4;
        let bytes = self.data.get(off..off + 4)?;
        let value = i16::from_be_bytes([bytes[0], bytes[1]]);
        let device_off = u16::from_be_bytes([bytes[2], bytes[3]]);
        let device = DeviceOrVariationIndex::parse_from(self.data, device_off)
            .ok()
            .flatten();
        Some(MathValue {
            value,
            device,
            _table: self.data,
        })
    }
}
