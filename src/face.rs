//! Parsed SFNT font directory.
//!
//! A [`Face`] is the result of reading the top-level header of an OTF or
//! TTF font file. It does **not** eagerly parse every table. Instead it
//! records where each table starts and how many bytes it occupies, so
//! downstream callers can slice into the blob on demand.
//!
//! # Format
//!
//! ```text
//!   offset  type   field               notes
//!     0     u32    sfntVersion         0x00010000 (TTF), 'OTTO' (OTF)
//!     4     u16    numTables
//!     6     u16    searchRange         unused, informational only
//!     8     u16    entrySelector       unused, informational only
//!    10     u16    rangeShift          unused, informational only
//!   12+    xN     TableRecord         numTables x 16 bytes
//! ```
//!
//! `TableRecord` is:
//!
//! ```text
//!    0  [u8;4]  tag
//!    4  u32     checksum        sigilbuzz does not verify this today
//!    8  u32     offset          absolute, from start of the font file
//!   12  u32     length
//! ```
//!
//! TrueType Collections (`ttcf`) are supported: `Face::parse` reads
//! the collection header and indexes into the requested member font
//! (member table offsets are absolute from the start of the file, so
//! table access is identical to a standalone font). Enumerate members
//! with [`crate::fonts_in_collection`].

mod color;
mod layout;
mod outlines;
mod variations;

use alloc::vec::Vec;

use crate::blob::Blob;
use crate::error::{Error, Result};
use crate::tables::parse::Reader;
use crate::tables::{tag, Cmap, Head, Hhea, Hmtx, Maxp, Name, Vhea, Vmtx, Vorg};

pub use color::GlyphBitmapEntry;

/// One entry in the SFNT table directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableRecord {
    /// Four-byte tag identifying the table (e.g. `b"cmap"`).
    pub tag: [u8; 4],
    /// Absolute offset from the start of the font data.
    pub offset: u32,
    /// Length of the table in bytes.
    pub length: u32,
}

/// A parsed SFNT header and table directory.
///
/// Holds a reference to the underlying bytes so table accessors can
/// return byte slices without copying. A `Face` is cheap to clone:
/// it carries a short `Vec<TableRecord>` and a borrowed slice.
#[derive(Debug, Clone)]
pub struct Face<'a> {
    data: &'a [u8],
    sfnt_version: u32,
    records: Vec<TableRecord>,
}

/// Rounds a float to the nearest `i16`, saturating at the type bounds.
/// A `no_std`-friendly replacement for `f32::round() as i16`, which
/// would otherwise drag in `libm`.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn round_f32_to_i16(v: f32) -> i16 {
    // Add-half trick: positive -> +0.5 floor, negative -> -0.5 ceil.
    // Clamp to i16 range before the `as` cast to dodge UB on overflow.
    let adj = if v >= 0.0 { v + 0.5 } else { v - 0.5 };
    let clamped = adj.max(i16::MIN as f32).min(i16::MAX as f32);
    clamped as i16
}

use crate::ttc::TTCF_MAGIC;

const SFNT_TRUETYPE: u32 = 0x0001_0000;
const SFNT_OTTO: u32 = 0x4F54_544F; // 'OTTO'
const SFNT_TRUE: u32 = 0x7472_7565; // 'true': legacy Apple TrueType

impl<'a> Face<'a> {
    /// Parses the SFNT directory at the start of `blob`.
    ///
    /// `index` selects a font in a TrueType Collection (`ttcf`);
    /// enumerate valid indices with [`crate::fonts_in_collection`].
    /// For plain TTF and OTF files it must be zero.
    pub fn parse(blob: &'a Blob<'a>, index: u32) -> Result<Self> {
        Self::parse_bytes(blob.as_bytes(), index)
    }

    /// Parses directly from a byte slice. Useful for tests and for
    /// callers that have not wrapped their data in a [`Blob`] yet.
    pub fn parse_bytes(data: &'a [u8], index: u32) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.read_u32()?;

        let dir_offset = if version == TTCF_MAGIC {
            // Collection: locate the member's table directory. Member
            // table offsets are absolute from the start of the file,
            // so everything downstream keeps slicing `data` directly.
            crate::ttc::member_offset(data, index)?
        } else {
            if index != 0 {
                return Err(Error::Unsupported {
                    context: "non-zero font index outside a TTC is meaningless",
                });
            }
            0
        };

        Self::parse_directory(data, dir_offset)
    }

    /// Parses the SFNT table directory found at `dir_offset` within
    /// `data`. Table record offsets are absolute from the start of
    /// `data` (true for both standalone fonts and TTC members).
    fn parse_directory(data: &'a [u8], dir_offset: usize) -> Result<Self> {
        let dir = data.get(dir_offset..).ok_or(Error::Malformed {
            offset: dir_offset,
            context: "table directory offset past end of font",
        })?;
        let mut r = Reader::new(dir);
        let version = r.read_u32()?;

        match version {
            SFNT_TRUETYPE | SFNT_OTTO | SFNT_TRUE => {}
            _ => {
                return Err(Error::Malformed {
                    offset: dir_offset,
                    context: "unrecognised sfnt version",
                });
            }
        }

        let num_tables = r.read_u16()? as usize;
        // Skip searchRange / entrySelector / rangeShift, all derivable
        // from num_tables and not trusted by any sigilbuzz consumer.
        r.skip(6)?;

        let mut records = Vec::with_capacity(num_tables);
        for _ in 0..num_tables {
            let tag = r.read_tag()?;
            let _checksum = r.read_u32()?;
            let offset = r.read_u32()?;
            let length = r.read_u32()?;

            let end = (offset as usize)
                .checked_add(length as usize)
                .ok_or(Error::Malformed {
                    offset: dir_offset + r.position() - 8,
                    context: "table offset + length overflows",
                })?;
            if end > data.len() {
                return Err(Error::Malformed {
                    offset: dir_offset + r.position() - 8,
                    context: "table extends past end of font",
                });
            }

            records.push(TableRecord {
                tag,
                offset,
                length,
            });
        }

        Ok(Self {
            data,
            sfnt_version: version,
            records,
        })
    }

    /// Rebuilds a `Face` from a previously parsed directory without
    /// re-reading the header.
    ///
    /// Invariant (upheld by the only caller, [`crate::OwnedFace`]):
    /// `records` must come from a successful [`Face::parse_bytes`] over
    /// exactly this `data`, so every record range is already validated.
    pub(crate) fn from_raw_parts(
        data: &'a [u8],
        sfnt_version: u32,
        records: Vec<TableRecord>,
    ) -> Self {
        Self {
            data,
            sfnt_version,
            records,
        }
    }

    /// Raw SFNT version word. `0x00010000` is TrueType, `OTTO` is CFF,
    /// `true` is legacy Apple TrueType.
    #[must_use]
    pub fn sfnt_version(&self) -> u32 {
        self.sfnt_version
    }

    /// Number of tables in the directory.
    #[must_use]
    pub fn num_tables(&self) -> usize {
        self.records.len()
    }

    /// All parsed table records, in file order.
    #[must_use]
    pub fn records(&self) -> &[TableRecord] {
        &self.records
    }

    /// Looks up a table by tag. Returns the record if present.
    #[must_use]
    pub fn record(&self, tag: [u8; 4]) -> Option<&TableRecord> {
        self.records.iter().find(|r| r.tag == tag)
    }

    /// Returns the byte range for a table, or `Error::MissingTable` if
    /// the font does not provide one.
    pub fn table_bytes(&self, tag: [u8; 4]) -> Result<&'a [u8]> {
        let record = self.record(tag).ok_or(Error::MissingTable { tag })?;
        let start = record.offset as usize;
        let end = start + record.length as usize;
        // `parse_bytes` has already validated that this range is
        // in-bounds, so the slice is safe.
        Ok(&self.data[start..end])
    }

    /// Parses the `head` table.
    pub fn head(&self) -> Result<Head> {
        Head::parse(self.table_bytes(tag::HEAD)?)
    }

    /// Parses the `maxp` table.
    pub fn maxp(&self) -> Result<Maxp> {
        Maxp::parse(self.table_bytes(tag::MAXP)?)
    }

    /// Parses the `hhea` table.
    pub fn hhea(&self) -> Result<Hhea> {
        Hhea::parse(self.table_bytes(tag::HHEA)?)
    }

    /// Parses the `hmtx` table. Requires `maxp` and `hhea` to be
    /// present because the hmtx layout depends on their counts; either
    /// missing surfaces as [`Error::MissingTable`].
    pub fn hmtx(&self) -> Result<Hmtx<'a>> {
        let maxp = self.maxp()?;
        let hhea = self.hhea()?;
        Hmtx::parse(
            self.table_bytes(tag::HMTX)?,
            maxp.num_glyphs,
            hhea.number_of_h_metrics,
        )
    }

    /// Parses the `cmap` table.
    pub fn cmap(&self) -> Result<Cmap<'a>> {
        Cmap::parse(self.table_bytes(tag::CMAP)?)
    }

    /// Parses the `name` table if the font carries one. Used by font
    /// browsers and rendering frontends that surface the family /
    /// subfamily / full name to end users; before this accessor
    /// landed the only way out of the crate was a placeholder string
    /// in the consumer (oniq #210). Returns `Ok(None)` for the rare
    /// fonts that omit `name` entirely.
    pub fn name(&self) -> Result<Option<Name<'a>>> {
        match self.table_bytes(tag::NAME) {
            Ok(bytes) => Ok(Some(Name::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `vhea` table if the font carries one. Fonts that
    /// support vertical writing ship this alongside `vmtx`; purely
    /// horizontal fonts omit both.
    pub fn vhea(&self) -> Result<Option<Vhea>> {
        match self.table_bytes(tag::VHEA) {
            Ok(bytes) => Ok(Some(Vhea::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `vmtx` table if the font carries one. Requires
    /// `maxp` and `vhea` to be present because the vmtx layout depends
    /// on their counts; missing either surfaces as
    /// [`Error::MissingTable`]. Returns `Ok(None)` when the font has
    /// no `vmtx` at all (i.e. horizontal-only).
    pub fn vmtx(&self) -> Result<Option<Vmtx<'a>>> {
        let Some(vhea) = self.vhea()? else {
            return Ok(None);
        };
        let maxp = self.maxp()?;
        match self.table_bytes(tag::VMTX) {
            Ok(bytes) => Ok(Some(Vmtx::parse(
                bytes,
                maxp.num_glyphs,
                vhea.number_of_long_ver_metrics,
            )?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `VORG` table if the font carries one. Most fonts
    /// with vertical metrics omit this. The renderer's default
    /// origin rule is usually good enough; CFF CJK fonts use it.
    pub fn vorg(&self) -> Result<Option<Vorg<'a>>> {
        match self.table_bytes(tag::VORG) {
            Ok(bytes) => Ok(Some(Vorg::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    // Build a minimal synthetic SFNT: header + N table records, no
    // actual table payloads. Useful for directory-level tests without
    // needing a real font.
    fn build_sfnt(version: u32, records: &[(u32, [u8; 4], u32)]) -> Vec<u8> {
        // Calculate where table payloads start: header (12 bytes) +
        // numTables * 16-byte record.
        let header_len = 12 + records.len() * 16;
        let total_payload: u32 = records.iter().map(|(len, _, _)| *len).sum();
        let mut out = Vec::with_capacity(header_len + total_payload as usize);

        out.extend_from_slice(&version.to_be_bytes());
        out.extend_from_slice(&(records.len() as u16).to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // searchRange
        out.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
        out.extend_from_slice(&0u16.to_be_bytes()); // rangeShift

        let mut cursor: u32 = header_len as u32;
        for (length, tag, _fill) in records {
            out.extend_from_slice(tag);
            out.extend_from_slice(&0u32.to_be_bytes()); // checksum
            out.extend_from_slice(&cursor.to_be_bytes()); // offset
            out.extend_from_slice(&length.to_be_bytes());
            cursor += length;
        }

        for (length, _tag, fill) in records {
            for _ in 0..*length {
                out.push((*fill & 0xff) as u8);
            }
        }
        out
    }

    #[test]
    fn parses_minimal_truetype_directory() {
        let bytes = build_sfnt(SFNT_TRUETYPE, &[(4, *b"head", 0xAA), (8, *b"cmap", 0xBB)]);
        let face = Face::parse_bytes(&bytes, 0).unwrap();
        assert_eq!(face.num_tables(), 2);
        assert_eq!(face.sfnt_version(), SFNT_TRUETYPE);

        let head = face.table_bytes(*b"head").unwrap();
        assert_eq!(head, &[0xAA, 0xAA, 0xAA, 0xAA]);
        let cmap = face.table_bytes(*b"cmap").unwrap();
        assert_eq!(cmap, &[0xBB; 8]);
    }

    #[test]
    fn parses_cff_otto_header() {
        let bytes = build_sfnt(SFNT_OTTO, &[(1, *b"name", 0x01)]);
        let face = Face::parse_bytes(&bytes, 0).unwrap();
        assert_eq!(face.sfnt_version(), SFNT_OTTO);
    }

    #[test]
    fn missing_table_yields_specific_error() {
        let bytes = build_sfnt(SFNT_TRUETYPE, &[(2, *b"head", 0x00)]);
        let face = Face::parse_bytes(&bytes, 0).unwrap();
        let err = face.table_bytes(*b"glyf").unwrap_err();
        assert!(matches!(err, Error::MissingTable { tag } if tag == *b"glyf"));
    }

    #[test]
    fn rejects_truncated_ttc_header() {
        // TTC files are supported now, but a header cut off right
        // after the magic still has to error instead of panicking.
        let mut bytes = vec![];
        bytes.extend_from_slice(&TTCF_MAGIC.to_be_bytes());
        let err = Face::parse_bytes(&bytes, 0).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn rejects_non_zero_index_outside_ttc() {
        let bytes = build_sfnt(SFNT_TRUETYPE, &[]);
        let err = Face::parse_bytes(&bytes, 1).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }));
    }

    #[test]
    fn rejects_unknown_sfnt_version() {
        let mut bytes = vec![];
        bytes.extend_from_slice(&0xDEAD_BEEFu32.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes()); // numTables
        bytes.extend_from_slice(&[0; 6]);
        let err = Face::parse_bytes(&bytes, 0).unwrap_err();
        assert!(matches!(err, Error::Malformed { .. }));
    }

    #[test]
    fn rejects_table_extending_past_end() {
        // Craft a header that claims a huge length for one table.
        let mut bytes = vec![];
        bytes.extend_from_slice(&SFNT_TRUETYPE.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes()); // numTables
        bytes.extend_from_slice(&[0; 6]);
        bytes.extend_from_slice(b"cmap");
        bytes.extend_from_slice(&0u32.to_be_bytes()); // checksum
        bytes.extend_from_slice(&(12u32 + 16).to_be_bytes()); // offset past header + record
        bytes.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes()); // bogus length
        let err = Face::parse_bytes(&bytes, 0).unwrap_err();
        assert!(matches!(err, Error::Malformed { .. }));
    }

    #[test]
    fn truncated_header_surfaces_parse_error() {
        let bytes = [0x00, 0x01];
        let err = Face::parse_bytes(&bytes, 0).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }
}
