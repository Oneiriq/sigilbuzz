//! `MathVariants`: stretchy glyph variant lists and glyph assemblies.

use super::MathValue;
use crate::error::{Error, Result};
use crate::tables::layout::{Coverage, DeviceOrVariationIndex};
use crate::tables::parse::Reader;

// =========================================================================
// MathVariants
// =========================================================================

/// One entry in a stretchy operator's progressive-size variant list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MathGlyphVariant {
    /// Glyph id of the variant.
    pub variant_glyph: u16,
    /// Advance dimension (height for vertical, width for horizontal)
    /// in font design units.
    pub advance_measurement: u16,
}

/// Flag bit on a [`GlyphPart`] marking it as a repeatable extender
/// (the part that the assembler tiles to fill arbitrary lengths).
pub const PART_FLAG_EXTENDER: u16 = 0x0001;

/// One part of an extensible-glyph assembly.
///
/// The full glyph is stitched from these in order, with adjacent
/// parts overlapping by at least `start_connector_length` /
/// `end_connector_length` design units so the seam is invisible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphPart {
    /// Glyph id of the part.
    pub glyph_id: u16,
    /// Connector length on the leading edge (top for vertical, left
    /// for horizontal).
    pub start_connector_length: u16,
    /// Connector length on the trailing edge.
    pub end_connector_length: u16,
    /// Full advance of this part, in design units.
    pub full_advance: u16,
    /// Bit flags. `PART_FLAG_EXTENDER` (0x0001) marks the repeatable
    /// extender; other bits are reserved.
    pub part_flags: u16,
}

impl GlyphPart {
    /// True when this part is the repeatable extender of its assembly.
    #[must_use]
    pub const fn is_extender(&self) -> bool {
        self.part_flags & PART_FLAG_EXTENDER != 0
    }
}

/// `GlyphAssembly`: the parts list used to compose stretchy glyphs
/// taller / wider than every variant in [`GlyphConstruction::variants`].
#[derive(Debug, Clone, Copy)]
pub struct GlyphAssembly<'a> {
    /// The italics-correction MathValueRecord for the assembled glyph.
    pub italics_correction: MathValue<'a>,
    /// Slice of just the `GlyphPartRecord` array (10 bytes each).
    parts: &'a [u8],
    part_count: u16,
}

impl GlyphAssembly<'_> {
    /// Number of `GlyphPart`s in this assembly.
    #[must_use]
    pub const fn part_count(&self) -> u16 {
        self.part_count
    }

    /// Returns the i-th `GlyphPart`, or `None` for an out-of-range index.
    #[must_use]
    pub fn part(&self, i: u16) -> Option<GlyphPart> {
        if i >= self.part_count {
            return None;
        }
        let off = (i as usize) * 10;
        let b = self.parts.get(off..off + 10)?;
        Some(GlyphPart {
            glyph_id: u16::from_be_bytes([b[0], b[1]]),
            start_connector_length: u16::from_be_bytes([b[2], b[3]]),
            end_connector_length: u16::from_be_bytes([b[4], b[5]]),
            full_advance: u16::from_be_bytes([b[6], b[7]]),
            part_flags: u16::from_be_bytes([b[8], b[9]]),
        })
    }

    /// Iterator over every part in order.
    pub fn iter(&self) -> impl Iterator<Item = GlyphPart> + '_ {
        (0..self.part_count).filter_map(|i| self.part(i))
    }
}

/// `MathGlyphConstruction`: variants list plus optional assembly.
#[derive(Debug, Clone, Copy)]
pub struct GlyphConstruction<'a> {
    /// Slice of the MathGlyphConstruction subtable.
    data: &'a [u8],
    assembly_off: u16,
    variant_count: u16,
}

impl<'a> GlyphConstruction<'a> {
    /// Parses a `MathGlyphConstruction` subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let assembly_off = r.read_u16()?;
        let variant_count = r.read_u16()?;
        // Variant array follows the header: 4 bytes per record.
        let needed = 4 + (variant_count as usize) * 4;
        if data.len() < needed {
            return Err(Error::Truncated {
                offset: data.len(),
                context: "MathGlyphConstruction variant array truncated",
            });
        }
        if assembly_off != 0 && assembly_off as usize > data.len() {
            return Err(Error::Malformed {
                offset: assembly_off as usize,
                context: "GlyphAssembly offset past end",
            });
        }
        Ok(Self {
            data,
            assembly_off,
            variant_count,
        })
    }

    /// Number of progressive-size variants.
    #[must_use]
    pub const fn variant_count(&self) -> u16 {
        self.variant_count
    }

    /// Returns the i-th progressive variant.
    #[must_use]
    pub fn variant(&self, i: u16) -> Option<MathGlyphVariant> {
        if i >= self.variant_count {
            return None;
        }
        let off = 4 + (i as usize) * 4;
        let b = self.data.get(off..off + 4)?;
        Some(MathGlyphVariant {
            variant_glyph: u16::from_be_bytes([b[0], b[1]]),
            advance_measurement: u16::from_be_bytes([b[2], b[3]]),
        })
    }

    /// Iterator over every variant in order.
    pub fn variants(&self) -> impl Iterator<Item = MathGlyphVariant> + '_ {
        (0..self.variant_count).filter_map(|i| self.variant(i))
    }

    /// Returns the assembly-parts list used for sizes beyond the
    /// largest variant, or `None` when the font does not provide one.
    pub fn assembly(&self) -> Option<GlyphAssembly<'a>> {
        if self.assembly_off == 0 {
            return None;
        }
        let base = self.data.get(self.assembly_off as usize..)?;
        // GlyphAssembly:
        //   MathValueRecord italicsCorrection (4 bytes)
        //   uint16 partCount
        //   GlyphPartRecord parts[partCount]   (10 bytes each)
        let mut r = Reader::new(base);
        let value = r.read_i16().ok()?;
        let device_off = r.read_u16().ok()?;
        let part_count = r.read_u16().ok()?;
        let needed = 6 + (part_count as usize) * 10;
        let parts_block = base.get(6..needed)?;
        let device = DeviceOrVariationIndex::parse_from(base, device_off)
            .ok()
            .flatten();
        Some(GlyphAssembly {
            italics_correction: MathValue {
                value,
                device,
                _table: base,
            },
            parts: parts_block,
            part_count,
        })
    }
}

/// `MathVariants`: stretchy-operator construction tables.
#[derive(Debug, Clone, Copy)]
pub struct MathVariants<'a> {
    /// Slice of the MathVariants subtable.
    data: &'a [u8],
    /// Minimum overlap between adjacent assembly parts, in design
    /// units. Re-exposed as [`Self::min_connector_overlap`].
    min_connector_overlap: u16,
    vert_coverage_off: u16,
    horiz_coverage_off: u16,
    vert_count: u16,
    horiz_count: u16,
    /// Offset of the start of the vertical-construction-offset array
    /// (right after the header).
    vert_off_table_start: usize,
    /// Offset of the start of the horizontal-construction-offset array.
    horiz_off_table_start: usize,
}

impl<'a> MathVariants<'a> {
    /// Parses a MathVariants header and validates all child offsets.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let min_connector_overlap = r.read_u16()?;
        let vert_coverage_off = r.read_u16()?;
        let horiz_coverage_off = r.read_u16()?;
        let vert_count = r.read_u16()?;
        let horiz_count = r.read_u16()?;
        let vert_off_table_start = r.position();
        let horiz_off_table_start = vert_off_table_start + (vert_count as usize) * 2;
        let needed = horiz_off_table_start + (horiz_count as usize) * 2;
        if data.len() < needed {
            return Err(Error::Truncated {
                offset: data.len(),
                context: "MathVariants construction-offset arrays truncated",
            });
        }
        for (off, ctx) in [
            (vert_coverage_off, "MathVariants vertical Coverage past end"),
            (
                horiz_coverage_off,
                "MathVariants horizontal Coverage past end",
            ),
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
            min_connector_overlap,
            vert_coverage_off,
            horiz_coverage_off,
            vert_count,
            horiz_count,
            vert_off_table_start,
            horiz_off_table_start,
        })
    }

    /// Minimum overlap between adjacent assembly parts.
    #[must_use]
    pub const fn min_connector_overlap(&self) -> u16 {
        self.min_connector_overlap
    }

    /// Looks up the vertical [`GlyphConstruction`] for a stretchy
    /// glyph (e.g. a tall integral or matrix bracket).
    pub fn vertical_glyph_construction(&self, gid: u16) -> Option<GlyphConstruction<'a>> {
        self.lookup(
            gid,
            self.vert_coverage_off,
            self.vert_off_table_start,
            self.vert_count,
        )
    }

    /// Looks up the horizontal [`GlyphConstruction`] for a stretchy
    /// glyph (e.g. an extensible underbrace).
    pub fn horizontal_glyph_construction(&self, gid: u16) -> Option<GlyphConstruction<'a>> {
        self.lookup(
            gid,
            self.horiz_coverage_off,
            self.horiz_off_table_start,
            self.horiz_count,
        )
    }

    fn lookup(
        &self,
        gid: u16,
        cov_off: u16,
        off_table_start: usize,
        count: u16,
    ) -> Option<GlyphConstruction<'a>> {
        if cov_off == 0 || count == 0 {
            return None;
        }
        let cov_bytes = self.data.get(cov_off as usize..)?;
        let cov = Coverage::parse(cov_bytes).ok()?;
        let idx = cov.index_of(gid)? as usize;
        if idx >= count as usize {
            return None;
        }
        let off_pos = off_table_start + idx * 2;
        let b = self.data.get(off_pos..off_pos + 2)?;
        let cons_off = u16::from_be_bytes([b[0], b[1]]);
        if cons_off == 0 {
            return None;
        }
        let cons_bytes = self.data.get(cons_off as usize..)?;
        GlyphConstruction::parse(cons_bytes).ok()
    }
}
