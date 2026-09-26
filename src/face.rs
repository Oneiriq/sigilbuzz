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

use alloc::vec::Vec;

use crate::blob::Blob;
use crate::error::{Error, Result};
use crate::tables::glyf::PhantomMetrics;
use crate::tables::parse::Reader;
use crate::tables::{
    tag, Ankr, Avar, Base, Cbdt, Cblc, Cff, Cff2, Cmap, Ebdt, EbdtBitmap, Eblc, Fvar, Gdef, Glyf,
    GlyphBitmap, GlyphBounds, Gpos, Gsub, Gvar, Head, Hhea, Hmtx, Hvar, KernTable, Kerx, Loca,
    Math, Maxp, Morx, Mvar, Name, Outline, PathOp, Sbix, Svg, SvgDocument, Varc, Vhea, Vmtx, Vorg,
    Vvar,
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
/// would otherwise drag in `libm`. NaN maps to `i16::MIN`.
fn round_f32_to_i16(v: f32) -> i16 {
    // Add-half trick: positive -> +0.5 then truncate, negative ->
    // -0.5 then truncate. The clamp makes the saturation explicit.
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

        // Each record takes 16 bytes. Size the allocation by the
        // records the data can hold, not by the claimed count.
        let mut records = Vec::with_capacity(num_tables.min(r.remaining() / 16));
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
        // `parse_bytes` has already validated that this range is
        // in-bounds. The checked form keeps the lookup total anyway.
        start
            .checked_add(record.length as usize)
            .and_then(|end| self.data.get(start..end))
            .ok_or(Error::Malformed {
                offset: start,
                context: "table extends past end of font",
            })
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
    /// subfamily / full name to end users. Returns `Ok(None)` for the
    /// rare fonts that omit `name` entirely.
    pub fn name(&self) -> Result<Option<Name<'a>>> {
        match self.table_bytes(tag::NAME) {
            Ok(bytes) => Ok(Some(Name::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `GDEF` table if the font carries one. Fonts without
    /// `GDEF` get `Ok(None)`. The shaper handles missing `GDEF` by
    /// treating every glyph as a base, which is the OpenType default.
    pub fn gdef(&self) -> Result<Option<Gdef<'a>>> {
        match self.table_bytes(tag::GDEF) {
            Ok(bytes) => Ok(Some(Gdef::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `GPOS` table if the font carries one. Returns
    /// `Ok(None)` when the font has no positioning features. Not
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
    /// since many older fonts (Open Sans among them) ship their
    /// kerning here rather than in GPOS.
    pub fn kern(&self) -> Result<Option<KernTable<'a>>> {
        match self.table_bytes(tag::KERN) {
            Ok(bytes) => Ok(Some(KernTable::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the AAT `morx` table if the font carries one.
    /// sigilbuzz reaches for `morx` only when the font has no GSUB,
    /// which is the same policy HarfBuzz uses; AAT-only fonts (some
    /// legacy macOS system fonts, most third-party AAT designs) are
    /// the common case.
    pub fn morx(&self) -> Result<Option<Morx<'a>>> {
        match self.table_bytes(tag::MORX) {
            Ok(bytes) => Ok(Some(Morx::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the AAT `kerx` table if the font carries one. Used
    /// only when the font has no GPOS kern feature, so mainstream
    /// fonts are unaffected. `kerx` format-2 needs `numGlyphs` to
    /// bound-check format-0 class lookups, so we plumb it through
    /// from `maxp`.
    pub fn kerx(&self) -> Result<Option<Kerx<'a>>> {
        match self.table_bytes(tag::KERX) {
            Ok(bytes) => {
                let num_glyphs = self.maxp()?.num_glyphs;
                Ok(Some(Kerx::parse(bytes, num_glyphs)?))
            }
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the AAT `ankr` (Anchor Point) table if the font carries
    /// one. Pairs with `kerx` format-4 action type 1: the kerx state
    /// machine emits `(mark_anchor_idx, current_anchor_idx)` pairs and
    /// the apply path resolves them through `ankr.anchor_for(gid, idx)`
    /// into concrete `(x, y)` design-unit coordinates.
    pub fn ankr(&self) -> Result<Option<Ankr<'a>>> {
        match self.table_bytes(tag::ANKR) {
            Ok(bytes) => Ok(Some(Ankr::parse(bytes)?)),
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

    /// Returns the glyph's raw points in glyf-natural order: contour
    /// points (on-curve + off-curve) followed by the four phantom
    /// points (pp1..pp4). Thin wrapper over [`Glyf::glyph_points`]:
    /// the heavy lifting (composite flattening, phantom synthesis)
    /// lives there; this method just plumbs `loca`, `hmtx`, and the
    /// optional `vmtx` through.
    ///
    /// Used by `kerx` format-4 action type 0, which references glyph
    /// points by index. Returns `Ok(None)` for glyphs without an
    /// outline (whitespace, missing) and for fonts that lack `glyf`
    /// entirely (CFF-only); the caller should treat the missing
    /// information as "drop the kern silently", the same conservative
    /// posture sigilbuzz uses for fmt-4 fall-through everywhere else.
    pub fn glyph_points(&self, glyph_id: u16) -> Result<Option<Vec<(i16, i16)>>> {
        if self.record(tag::GLYF).is_none() {
            return Ok(None);
        }
        let loca = self.loca()?;
        let glyf = self.glyf()?;
        let hmtx = self.hmtx()?;
        let vmtx = self.vmtx()?;
        glyf.glyph_points(&loca, glyph_id, &hmtx, vmtx.as_ref())
    }

    /// Returns the design-unit bounding box for `glyph_id`, or
    /// `Ok(None)` when the glyph has no outline (e.g. a space
    /// glyph). Requires both `loca` and `glyf`. Fonts that use CFF
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
    /// even in variable fonts: `avar` only appears when the
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

    /// Parses the `gvar` table if the font carries one. Present in
    /// every TrueType variable font with a `glyf` outline table;
    /// CFF2-based variable fonts carry their own `CFF2` deltas
    /// instead.
    pub fn gvar(&self) -> Result<Option<Gvar<'a>>> {
        match self.table_bytes(tag::GVAR) {
            Ok(bytes) => Ok(Some(Gvar::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `MVAR` table if the font carries one. MVAR varies
    /// font-wide instance metrics (typoAscender / Descender,
    /// x-height, sub/super offsets, strikeout, underline, ...) by
    /// axis coord; missing MVAR means those metrics stay constant
    /// across the design space. Most variable fonts ship MVAR.
    pub fn mvar(&self) -> Result<Option<Mvar<'a>>> {
        match self.table_bytes(tag::MVAR) {
            Ok(bytes) => Ok(Some(Mvar::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `VVAR` table if the font carries one. HVAR's
    /// vertical sibling: varies per-glyph advance height and
    /// top-side bearing. Only fonts that support vertical layout
    /// (CJK, vertical Latin) ship this; horizontal-only variable
    /// fonts omit it.
    pub fn vvar(&self) -> Result<Option<Vvar<'a>>> {
        match self.table_bytes(tag::VVAR) {
            Ok(bytes) => Ok(Some(Vvar::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `VARC` table if the font carries one. VARC adds
    /// per-axis variation deltas to composite-glyph component
    /// transforms and to each component's effective coord vector;
    /// shipped to date only by Chrome's experimental VARC fonts and
    /// a handful of demo files. Returns `Ok(None)` for the vast
    /// majority of fonts that don't carry it.
    pub fn varc(&self) -> Result<Option<Varc<'a>>> {
        match self.table_bytes(tag::VARC) {
            Ok(bytes) => Ok(Some(Varc::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Returns the design-unit bounding box for `glyph_id` at the
    /// given normalized axis coords. When `gvar` is present and any
    /// tuple contributes a delta, the static bounds from `glyf` are
    /// adjusted by the minimum / maximum `(dx, dy)` across every
    /// contour point touched by the glyph's variation data. Missing
    /// `gvar` (or coords that produce zero deltas) gives the same
    /// answer as [`Face::glyph_bounds`].
    pub fn glyph_bounds_at_coords(
        &self,
        glyph_id: u16,
        coords: &[f32],
    ) -> Result<Option<GlyphBounds>> {
        let Some(base) = self.glyph_bounds(glyph_id)? else {
            return Ok(None);
        };
        let Some(gvar) = self.gvar()? else {
            return Ok(Some(base));
        };
        // Composite glyphs and zero-contour glyphs return None here;
        // sigilbuzz doesn't apply gvar deltas to those yet.
        let loca = self.loca()?;
        let glyf = self.glyf()?;
        let Some(num_points) = glyf.point_count(&loca, glyph_id)? else {
            return Ok(Some(base));
        };
        let deltas = gvar.glyph_deltas(glyph_id, coords, num_points);
        if deltas.is_empty() {
            return Ok(Some(base));
        }
        // Simple glyphs carry points on contour edges; the bounding
        // box tracks those extrema. A full renderer would interpolate
        // composite glyphs and phantom points. sigilbuzz only needs
        // an approximate bbox, so "shift corners by min/max deltas
        // across touched points" is sufficient for layout work.
        let mut x_lo = f32::INFINITY;
        let mut x_hi = f32::NEG_INFINITY;
        let mut y_lo = f32::INFINITY;
        let mut y_hi = f32::NEG_INFINITY;
        for d in &deltas {
            if d.dx < x_lo {
                x_lo = d.dx;
            }
            if d.dx > x_hi {
                x_hi = d.dx;
            }
            if d.dy < y_lo {
                y_lo = d.dy;
            }
            if d.dy > y_hi {
                y_hi = d.dy;
            }
        }
        if !x_lo.is_finite() {
            return Ok(Some(base));
        }
        let adjusted = GlyphBounds {
            x_min: base.x_min.saturating_add(round_f32_to_i16(x_lo)),
            y_min: base.y_min.saturating_add(round_f32_to_i16(y_lo)),
            x_max: base.x_max.saturating_add(round_f32_to_i16(x_hi)),
            y_max: base.y_max.saturating_add(round_f32_to_i16(y_hi)),
            num_contours: base.num_contours,
        };
        Ok(Some(adjusted))
    }

    /// Parses the `CFF ` (Compact Font Format 1) table.
    pub fn cff(&self) -> Result<Cff<'a>> {
        Cff::parse(self.table_bytes(tag::CFF1)?)
    }

    /// Parses the `CFF2` table if the font carries one. CFF2 is the
    /// variable-font flavor of CFF; static OTF fonts use plain
    /// `CFF `.
    pub fn cff2(&self) -> Result<Cff2<'a>> {
        Cff2::parse(self.table_bytes(tag::CFF2)?)
    }

    /// Returns the full contour outline for `glyph_id` as a flat
    /// list of [`crate::tables::PathOp`]s. Works for both TrueType
    /// (`glyf`) and CFF / CFF2 fonts; the backend is inferred from
    /// the tables the font carries.
    ///
    /// Composite glyphs are flattened: the caller never sees
    /// component references. Returns `Ok(None)` for glyphs with no
    /// outline (whitespace) or for glyph ids past the end of the
    /// font's outline table.
    pub fn glyph_outline(&self, glyph_id: u16) -> Result<Option<Outline>> {
        self.glyph_outline_at_coords(glyph_id, &[])
    }

    /// Like [`Face::glyph_outline`] but applies variable-font deltas
    /// for the given normalized axis coords. For TrueType outlines
    /// the deltas come from `gvar`; for CFF2 they come from the
    /// table's own Variation Store via the `blend` charstring
    /// operator. An empty `coords` slice is equivalent to the static
    /// outline and is the cheap path taken by [`Face::glyph_outline`].
    pub fn glyph_outline_at_coords(
        &self,
        glyph_id: u16,
        coords: &[f32],
    ) -> Result<Option<Outline>> {
        let mut budget = VarcBudget {
            components_left: MAX_VARC_COMPONENTS,
            ops_left: MAX_VARC_OPS,
        };
        self.glyph_outline_at_coords_inner(glyph_id, coords, 0, &mut budget)
    }

    /// Recursive entry point used by VARC composite resolution.
    /// `depth` caps recursion through nested VARC composites the
    /// same way [`Glyf::flatten`] caps `glyf` composites. `budget`
    /// caps the total work: depth alone still lets a glyph whose
    /// components share children expand exponentially.
    fn glyph_outline_at_coords_inner(
        &self,
        glyph_id: u16,
        coords: &[f32],
        depth: u8,
        budget: &mut VarcBudget,
    ) -> Result<Option<Outline>> {
        const MAX_VARC_DEPTH: u8 = 64;
        if depth > MAX_VARC_DEPTH {
            return Err(Error::Malformed {
                offset: 0,
                context: "VARC composite recursion exceeded cap",
            });
        }

        // VARC routing: if the font ships a VARC table that covers
        // this gid, recurse through the resolved components and apply
        // each component's affine to the child outline. Children
        // outside VARC's coverage fall through to the regular glyf /
        // CFF path with the component's effective coord vector.
        if let Some(varc) = self.varc()? {
            if varc.covers(glyph_id) {
                if let Some(composite) = varc.composite(glyph_id, coords) {
                    let mut out = Outline::new();
                    for comp in &composite.components {
                        budget.components_left =
                            budget
                                .components_left
                                .checked_sub(1)
                                .ok_or(Error::Malformed {
                                    offset: 0,
                                    context: "VARC composite exceeds component budget",
                                })?;
                        let child = self.glyph_outline_at_coords_inner(
                            comp.gid,
                            &comp.coords,
                            depth + 1,
                            budget,
                        )?;
                        if let Some(child) = child {
                            budget.ops_left = budget
                                .ops_left
                                .checked_sub(child.ops().len())
                                .ok_or(Error::Malformed {
                                    offset: 0,
                                    context: "VARC composite exceeds outline budget",
                                })?;
                            for op in child.ops() {
                                out.push(transform_path_op(*op, comp.transform));
                            }
                        }
                    }
                    return Ok(Some(out));
                }
            }
        }

        // CFF / CFF2 path: presence of `CFF2` wins over `CFF ` since
        // variable fonts ship only CFF2. `CFF ` is used only when the
        // font has no `glyf`. A font carrying both takes the TrueType
        // path below.
        if self.record(tag::CFF2).is_some() {
            let cff2 = self.cff2()?;
            let mut out = Outline::new();
            let drew = cff2.outline(glyph_id, coords, &mut out)?;
            return Ok(drew.then_some(out));
        }
        if self.record(tag::CFF1).is_some() && self.record(tag::GLYF).is_none() {
            let cff = self.cff()?;
            let mut out = Outline::new();
            let drew = cff.outline(glyph_id, &mut out)?;
            return Ok(drew.then_some(out));
        }

        // TrueType path.
        let loca = self.loca()?;
        let glyf = self.glyf()?;
        let mut out = Outline::new();

        // Phantom metrics let composite anchor-mode resolve indices
        // past the contour-point count (lsb / advance-width / tsb /
        // advance-height). hmtx is required by every TrueType font;
        // vmtx is optional and only horizontal-only fonts skip it.
        let hmtx = self.hmtx()?;
        let vmtx = self.vmtx()?;
        let metrics = PhantomMetrics {
            hmtx: &hmtx,
            vmtx: vmtx.as_ref(),
        };

        // Apply gvar if present and the font is variable.
        if !coords.is_empty() {
            if let Some(gvar) = self.gvar()? {
                if let Some(num_points) = glyf.point_count(&loca, glyph_id)? {
                    let deltas_sparse = gvar.glyph_deltas(glyph_id, coords, num_points);
                    if !deltas_sparse.is_empty() {
                        // Dense deltas indexed by point id. Phantom
                        // points live at the end of the range but
                        // don't appear in the simple-glyph coord
                        // stream, so we only need the real-point
                        // portion; outline() bounds-checks by slice
                        // index.
                        let mut dense: Vec<(f32, f32)> =
                            alloc::vec![(0.0_f32, 0.0_f32); num_points as usize];
                        for d in &deltas_sparse {
                            if (d.point as usize) < dense.len() {
                                dense[d.point as usize] = (d.dx, d.dy);
                            }
                        }
                        let drew =
                            glyf.outline(&loca, glyph_id, Some(&dense), Some(&metrics), &mut out)?;
                        return Ok(drew.then_some(out));
                    }
                }
            }
        }

        let drew = glyf.outline(&loca, glyph_id, None, Some(&metrics), &mut out)?;
        Ok(drew.then_some(out))
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

    /// Parses the `COLR` table if the font carries one. Color fonts
    /// ship this alongside `CPAL`; monochrome outlines-only fonts
    /// omit both.
    pub fn colr(&self) -> Result<Option<crate::tables::colr::Colr<'a>>> {
        match self.table_bytes(tag::COLR) {
            Ok(bytes) => Ok(Some(crate::tables::colr::Colr::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `CPAL` table if the font carries one.
    pub fn cpal(&self) -> Result<Option<crate::tables::cpal::Cpal<'a>>> {
        match self.table_bytes(tag::CPAL) {
            Ok(bytes) => Ok(Some(crate::tables::cpal::Cpal::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Convenience: resolves the paint subtree for `glyph_id` through
    /// the `COLR` table, returning `Ok(None)` when either the font has
    /// no COLR or the glyph has no color record.
    pub fn colr_paint(&self, glyph_id: u16) -> Result<Option<crate::tables::colr::ColrPaint<'a>>> {
        match self.colr()? {
            Some(colr) => Ok(colr.paint(glyph_id)),
            None => Ok(None),
        }
    }

    /// Parses the `MATH` table if the font carries one. Math
    /// typography fonts (STIX 2 Math, Latin Modern Math, Cambria
    /// Math, Asana Math, XITS Math) ship this; everything else
    /// returns `Ok(None)`. sigilbuzz exposes the parsed structure;
    /// running an actual math layout pass is the consumer's job
    /// (LuaTeX, MathML renderers, ...).
    pub fn math(&self) -> Result<Option<Math<'a>>> {
        match self.table_bytes(tag::MATH) {
            Ok(bytes) => Ok(Some(Math::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `BASE` table if the font carries one. BASE
    /// provides per-script baseline metrics (`romn`, `ideo`,
    /// `hang`, `math`, ...) plus min/max clamps so a typesetting
    /// engine can align glyphs from different scripts on a common
    /// baseline. Most fonts omit BASE. Adobe's flagship faces,
    /// some Noto / SIL designs, and a handful of math fonts ship
    /// it. v1.1 BASE tables can carry IVS-varied baseline coords;
    /// the parsed [`Base`] exposes its variation store via
    /// [`Base::variation_store`].
    pub fn base(&self) -> Result<Option<Base<'a>>> {
        match self.table_bytes(tag::BASE) {
            Ok(bytes) => Ok(Some(Base::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `CBLC` table (color bitmap location) if the font
    /// carries one. Always paired with `CBDT` in the wild.
    pub fn cblc(&self) -> Result<Option<Cblc<'a>>> {
        match self.table_bytes(tag::CBLC) {
            Ok(bytes) => Ok(Some(Cblc::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `CBDT` table (color bitmap data) if the font carries
    /// one. Pairs with `CBLC`; consumers usually go through
    /// [`Face::glyph_bitmap`] instead of touching either directly.
    pub fn cbdt(&self) -> Result<Option<Cbdt<'a>>> {
        match self.table_bytes(tag::CBDT) {
            Ok(bytes) => Ok(Some(Cbdt::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `EBLC` table (Microsoft monochrome bitmap location)
    /// if the font carries one. Always paired with `EBDT` in the wild.
    /// Modern color-emoji fonts use CBDT/CBLC instead; EBLC/EBDT
    /// shows up in legacy Asian text fonts and a handful of bitmap-
    /// only display faces.
    pub fn eblc(&self) -> Result<Option<Eblc<'a>>> {
        match self.table_bytes(tag::EBLC) {
            Ok(bytes) => Ok(Some(Eblc::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `EBDT` table (Microsoft monochrome bitmap data) if
    /// the font carries one. Pairs with `EBLC`; consumers usually go
    /// through [`Face::glyph_bitmap`] instead of touching either
    /// directly.
    pub fn ebdt(&self) -> Result<Option<Ebdt<'a>>> {
        match self.table_bytes(tag::EBDT) {
            Ok(bytes) => Ok(Some(Ebdt::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `sbix` table (Apple Standard Bitmap Graphics) if
    /// the font carries one. The strike's per-glyph offsets array is
    /// sized off `maxp.numGlyphs`, so `maxp` must be present.
    pub fn sbix(&self) -> Result<Option<Sbix<'a>>> {
        match self.table_bytes(tag::SBIX) {
            Ok(bytes) => {
                let num_glyphs = self.maxp()?.num_glyphs;
                Ok(Some(Sbix::parse(bytes, num_glyphs)?))
            }
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `SVG ` table (OpenType SVG) if the font carries
    /// one. Returns `Ok(None)` for fonts without inline SVG glyphs:
    /// most fonts in the wild, including all COLR-only color fonts.
    pub fn svg(&self) -> Result<Option<Svg<'a>>> {
        match self.table_bytes(tag::SVG) {
            Ok(bytes) => Ok(Some(Svg::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Convenience: returns the inline SVG document for `glyph_id`,
    /// or `Ok(None)` when the font has no `SVG ` table or no record
    /// covers the glyph.
    ///
    /// The returned [`SvgDocument`] borrows directly into the font
    /// blob; `data` is either plain SVG XML or a gzip stream. See
    /// the `gzipped` flag. sigilbuzz never decompresses or parses the
    /// SVG itself.
    pub fn svg_document(&self, glyph_id: u16) -> Result<Option<SvgDocument<'a>>> {
        Ok(self.svg()?.and_then(|svg| svg.document_for(glyph_id)))
    }

    /// Returns the bitmap glyph for `glyph_id` at the requested
    /// `ppem`, picking the closest available strike.
    ///
    /// Resolution order:
    /// 1. `CBDT` / `CBLC`: Google color emoji.
    /// 2. `sbix`: Apple color emoji.
    /// 3. `EBDT` / `EBLC`: Microsoft monochrome bitmap embeds.
    ///
    /// CBDT outranks EBDT because a font carrying both (rare but
    /// permitted) is overwhelmingly built around the color table;
    /// the mono path is the legacy fallback.
    ///
    /// Returns `Ok(None)` when no bitmap table is present, or the
    /// available strikes don't cover the glyph (legitimate for glyphs
    /// that have only outline data, e.g. ASCII fallbacks in an emoji
    /// font).
    ///
    /// The returned [`GlyphBitmapEntry`] is a tagged union of CBDT,
    /// sbix, and EBDT payloads. All three ride a `&'a [u8]` that
    /// points back into the original font blob, so cloning is cheap
    /// and there is no allocation on the lookup path. PNG / JPEG /
    /// TIFF / mask decoding is the consumer's job: sigilbuzz exposes
    /// the bytes, not pixels.
    pub fn glyph_bitmap(&self, glyph_id: u16, ppem: u16) -> Result<Option<GlyphBitmapEntry<'a>>> {
        if let Some(cblc) = self.cblc()? {
            if let Some(cbdt) = self.cbdt()? {
                if let Some(size) = cblc.best_strike(glyph_id, ppem) {
                    if let Some(loc) = cblc.locate(&size, glyph_id)? {
                        let bm = cbdt.glyph_bitmap(&loc)?;
                        return Ok(Some(GlyphBitmapEntry::Cbdt {
                            ppem_x: size.ppem_x,
                            ppem_y: size.ppem_y,
                            bitmap: bm,
                        }));
                    }
                }
            }
        }
        if let Some(sbix) = self.sbix()? {
            if let Some(strike) = sbix.best_strike(ppem) {
                if let Some(g) = strike.glyph(glyph_id)? {
                    return Ok(Some(GlyphBitmapEntry::Sbix {
                        ppem: strike.ppem(),
                        ppi: strike.ppi(),
                        glyph: g,
                    }));
                }
            }
        }
        if let Some(eblc) = self.eblc()? {
            if let Some(ebdt) = self.ebdt()? {
                if let Some(size) = eblc.best_strike(glyph_id, ppem) {
                    if let Some(loc) = eblc.locate(&size, glyph_id)? {
                        let bm = ebdt.glyph_bitmap(&loc)?;
                        return Ok(Some(GlyphBitmapEntry::Ebdt {
                            ppem_x: size.ppem_x,
                            ppem_y: size.ppem_y,
                            bitmap: bm,
                        }));
                    }
                }
            }
        }
        Ok(None)
    }
}

/// Most VARC components one outline request may resolve, summed over
/// every nesting level. Mirrors HarfBuzz's graph edge cap. Real VARC
/// glyphs use a few dozen.
const MAX_VARC_COMPONENTS: usize = 2048;

/// Most path ops VARC composition may copy into composite outlines
/// for one outline request, summed over every nesting level.
const MAX_VARC_OPS: usize = 1 << 20;

/// Remaining work for one [`Face::glyph_outline_at_coords`] call.
/// Components that share children can make the resolved outline
/// grow exponentially with depth, so the whole request shares one
/// budget and fails with `Malformed` when it runs out.
struct VarcBudget {
    components_left: usize,
    ops_left: usize,
}

/// Applies a row-major `[xx, xy, yx, yy, tx, ty]` affine to a single
/// path op, transforming every point inside it. Control points and
/// endpoints alike receive the same affine, which is correct for
/// affine maps because they preserve the "control point ratio"
/// implied by Bezier evaluation.
fn transform_path_op(op: PathOp, m: [f32; 6]) -> PathOp {
    let xform =
        |x: f32, y: f32| -> (f32, f32) { (m[0] * x + m[1] * y + m[4], m[2] * x + m[3] * y + m[5]) };
    match op {
        PathOp::MoveTo { x, y } => {
            let (x, y) = xform(x, y);
            PathOp::MoveTo { x, y }
        }
        PathOp::LineTo { x, y } => {
            let (x, y) = xform(x, y);
            PathOp::LineTo { x, y }
        }
        PathOp::QuadTo { cx, cy, x, y } => {
            let (cx, cy) = xform(cx, cy);
            let (x, y) = xform(x, y);
            PathOp::QuadTo { cx, cy, x, y }
        }
        PathOp::CubicTo {
            c1x,
            c1y,
            c2x,
            c2y,
            x,
            y,
        } => {
            let (c1x, c1y) = xform(c1x, c1y);
            let (c2x, c2y) = xform(c2x, c2y);
            let (x, y) = xform(x, y);
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            }
        }
        PathOp::Close => PathOp::Close,
    }
}

/// Tagged result of [`Face::glyph_bitmap`]. Bitmap fonts come in two
/// flavors and the metrics shape differs enough that folding them
/// into one struct loses information; consumers match on the variant
/// they care about.
#[derive(Debug, Clone, Copy)]
pub enum GlyphBitmapEntry<'a> {
    /// Color Bitmap Data (Google CBDT/CBLC). `ppem_x` / `ppem_y` are
    /// the strike's design size; `bitmap.data` is raw payload bytes
    /// (PNG for formats 17/18/19).
    Cbdt {
        /// Strike X resolution (pixels-per-em).
        ppem_x: u8,
        /// Strike Y resolution (pixels-per-em).
        ppem_y: u8,
        /// Parsed CBDT entry (format, metrics, payload bytes).
        bitmap: GlyphBitmap<'a>,
    },
    /// Apple Standard Bitmap Graphics (sbix). `glyph.graphic_type`
    /// indicates the payload format (`'png '`, `'jpg '`, `'tiff'`,
    /// etc.).
    Sbix {
        /// Strike resolution (pixels-per-em).
        ppem: u16,
        /// Strike DPI.
        ppi: u16,
        /// Per-glyph entry (origin offsets, format tag, payload).
        glyph: crate::tables::SbixGlyph<'a>,
    },
    /// Microsoft Embedded Bitmap Data (EBDT/EBLC): monochrome 1bpp
    /// masks. Predecessor to CBDT/CBLC; same indexing model, mask
    /// payload instead of PNG.
    Ebdt {
        /// Strike X resolution (pixels-per-em).
        ppem_x: u8,
        /// Strike Y resolution (pixels-per-em).
        ppem_y: u8,
        /// Parsed EBDT entry (format, metrics, packing, mask bytes).
        bitmap: EbdtBitmap<'a>,
    },
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
