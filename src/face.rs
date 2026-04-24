//! Parsed SFNT font directory.
//!
//! A [`Face`] is the result of reading the top-level header of an OTF or
//! TTF font file. It does **not** eagerly parse every table — instead it
//! records where each table starts and how many bytes it occupies, so
//! downstream callers can slice into the blob on demand.
//!
//! # Format
//!
//! ```text
//!   offset  type   field               notes
//!     0     u32    sfntVersion         0x00010000 (TTF), 'OTTO' (OTF)
//!     4     u16    numTables
//!     6     u16    searchRange         unused — informational only
//!     8     u16    entrySelector       unused — informational only
//!    10     u16    rangeShift          unused — informational only
//!   12+    ×N     TableRecord         numTables × 16 bytes
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
//! TrueType Collections (`ttcf`) are not handled yet; `Face::parse`
//! rejects them with [`crate::Error::Unsupported`].

use alloc::vec::Vec;

use crate::blob::Blob;
use crate::error::{Error, Result};
use crate::tables::parse::Reader;
use crate::tables::{
    tag, Avar, Cmap, Fvar, Gdef, Glyf, GlyphBounds, Gpos, Gsub, Head, Hhea, Hmtx, Hvar, KernTable,
    Loca, Maxp, Vhea, Vmtx, Vorg,
};

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
/// return byte slices without copying. A `Face` is cheap to clone —
/// it carries a short `Vec<TableRecord>` and a borrowed slice.
#[derive(Debug, Clone)]
pub struct Face<'a> {
    data: &'a [u8],
    sfnt_version: u32,
    records: Vec<TableRecord>,
}

const SFNT_TRUETYPE: u32 = 0x0001_0000;
const SFNT_OTTO: u32 = 0x4F54_544F; // 'OTTO'
const SFNT_TRUE: u32 = 0x7472_7565; // 'true' — legacy Apple TrueType
const TTCF_MAGIC: u32 = 0x7474_6366; // 'ttcf' — TrueType collection

impl<'a> Face<'a> {
    /// Parses the SFNT directory at the start of `blob`.
    ///
    /// `index` selects a font in a TrueType Collection; for plain TTF
    /// and OTF files it must be zero. Non-zero indices currently return
    /// [`Error::Unsupported`].
    pub fn parse(blob: &'a Blob<'a>, index: u32) -> Result<Self> {
        Self::parse_bytes(blob.as_bytes(), index)
    }

    /// Parses directly from a byte slice. Useful for tests and for
    /// callers that have not wrapped their data in a [`Blob`] yet.
    pub fn parse_bytes(data: &'a [u8], index: u32) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.read_u32()?;

        if version == TTCF_MAGIC {
            return Err(Error::Unsupported {
                context: "TrueType Collections (ttcf) not yet implemented",
            });
        }

        if index != 0 {
            return Err(Error::Unsupported {
                context: "non-zero font index outside a TTC is meaningless",
            });
        }

        match version {
            SFNT_TRUETYPE | SFNT_OTTO | SFNT_TRUE => {}
            _ => {
                return Err(Error::Malformed {
                    offset: 0,
                    context: "unrecognised sfnt version",
                });
            }
        }

        let num_tables = r.read_u16()? as usize;
        // Skip searchRange / entrySelector / rangeShift — all derivable
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
                    offset: r.position() - 8,
                    context: "table offset + length overflows",
                })?;
            if end > data.len() {
                return Err(Error::Malformed {
                    offset: r.position() - 8,
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

    /// Parses the `GDEF` table if the font carries one. Fonts without
    /// `GDEF` get `Ok(None)` — the shaper handles missing `GDEF` by
    /// treating every glyph as a base, which is the OpenType default.
    pub fn gdef(&self) -> Result<Option<Gdef<'a>>> {
        match self.table_bytes(tag::GDEF) {
            Ok(bytes) => Ok(Some(Gdef::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `GPOS` table if the font carries one. Returns
    /// `Ok(None)` when the font has no positioning features — not
    /// every font does, and kerning-less output is still valid.
    pub fn gpos(&self) -> Result<Option<Gpos<'a>>> {
        match self.table_bytes(tag::GPOS) {
            Ok(bytes) => Ok(Some(Gpos::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the legacy `kern` table if the font carries one. Used
    /// as a fallback when the GPOS `kern` feature yields no lookups
    /// — many older fonts (Open Sans among them) ship their
    /// kerning here rather than in GPOS.
    pub fn kern(&self) -> Result<Option<KernTable<'a>>> {
        match self.table_bytes(tag::KERN) {
            Ok(bytes) => Ok(Some(KernTable::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `GSUB` table if the font carries one. Returns
    /// `Ok(None)` when the font has no glyph substitution features.
    pub fn gsub(&self) -> Result<Option<Gsub<'a>>> {
        match self.table_bytes(tag::GSUB) {
            Ok(bytes) => Ok(Some(Gsub::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `loca` table. Pulls the offset format from `head`
    /// and the glyph count from `maxp`; both must be present.
    pub fn loca(&self) -> Result<Loca<'a>> {
        let head = self.head()?;
        let maxp = self.maxp()?;
        Loca::parse(
            self.table_bytes(tag::LOCA)?,
            head.index_to_loc_format,
            maxp.num_glyphs,
        )
    }

    /// Wraps the `glyf` table.
    pub fn glyf(&self) -> Result<Glyf<'a>> {
        Ok(Glyf::new(self.table_bytes(tag::GLYF)?))
    }

    /// Returns the design-unit bounding box for `glyph_id`, or
    /// `Ok(None)` when the glyph has no outline (e.g. a space
    /// glyph). Requires both `loca` and `glyf` — fonts that use CFF
    /// outlines instead will yield [`Error::MissingTable`] for
    /// `glyf`.
    pub fn glyph_bounds(&self, glyph_id: u16) -> Result<Option<GlyphBounds>> {
        let loca = self.loca()?;
        let glyf = self.glyf()?;
        glyf.bounds(&loca, glyph_id)
    }

    /// Parses the `fvar` table if the font carries one. Presence
    /// of `fvar` is the definitive signal that the font is a
    /// variable font; static fonts yield `Ok(None)`.
    pub fn fvar(&self) -> Result<Option<Fvar>> {
        match self.table_bytes(tag::FVAR) {
            Ok(bytes) => Ok(Some(Fvar::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `avar` table if the font carries one. Optional
    /// even in variable fonts — `avar` only appears when the
    /// designer provides non-linear axis remapping.
    pub fn avar(&self) -> Result<Option<Avar>> {
        match self.table_bytes(tag::AVAR) {
            Ok(bytes) => Ok(Some(Avar::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `HVAR` table if the font carries one. Variable
    /// fonts with varying advances include this; fixed-metric
    /// variable fonts omit it (their advances don't change across
    /// the design space).
    pub fn hvar(&self) -> Result<Option<Hvar<'a>>> {
        match self.table_bytes(tag::HVAR) {
            Ok(bytes) => Ok(Some(Hvar::parse(bytes)?)),
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
    /// with vertical metrics omit this — the renderer's default
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
    fn rejects_ttc_header() {
        let mut bytes = vec![];
        bytes.extend_from_slice(&TTCF_MAGIC.to_be_bytes());
        let err = Face::parse_bytes(&bytes, 0).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }));
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
