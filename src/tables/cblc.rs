//! `CBLC` — Color Bitmap Location.
//!
//! Companion to [`Cbdt`](crate::tables::cbdt::Cbdt): `CBLC` indexes
//! into the data table by glyph id at one or more pixel-per-em sizes.
//! Same layout as the older monochrome `EBLC` / `bloc` tables — the
//! "C" prefix only changes the expected payload type in `CBDT`
//! (PNG / mask) versus the older 1-bit / 8-bit grayscale formats.
//!
//! sigilbuzz parses the directory structure (sizes, ranges, index
//! sub-tables formats 1-5) and exposes per-glyph lookups that yield
//! a byte offset and length into the matching `CBDT` table. Decoding
//! the bitmap payload is a renderer concern handled by
//! [`Cbdt::glyph_bitmap`](crate::tables::cbdt::Cbdt::glyph_bitmap).
//!
//! # Format
//!
//! ```text
//!   CBLC header
//!     0  u16  majorVersion        = 3
//!     2  u16  minorVersion        = 0
//!     4  u32  numSizes
//!     8  ...  BitmapSize[numSizes]
//!
//!   BitmapSize record (48 bytes)
//!     0   u32  indexSubTableArrayOffset    relative to start of CBLC
//!     4   u32  indexTablesSize             total length of the array
//!     8   u32  numberOfIndexSubTables
//!    12   u32  colorRef                    reserved, 0
//!    16   12B  hori SbitLineMetrics
//!    28   12B  vert SbitLineMetrics
//!    40   u16  startGlyphIndex
//!    42   u16  endGlyphIndex
//!    44   u8   ppemX
//!    45   u8   ppemY
//!    46   u8   bitDepth
//!    47   i8   flags
//!
//!   IndexSubTableArray entry (8 bytes, repeated numberOfIndexSubTables times)
//!     0   u16  firstGlyphIndex
//!     2   u16  lastGlyphIndex
//!     4   u32  additionalOffsetToIndexSubTable     relative to BitmapSize record
//!
//!   IndexSubTable header (8 bytes)
//!     0   u16  indexFormat         1..5
//!     2   u16  imageFormat         CBDT format id (17 / 18 / 19 etc.)
//!     4   u32  imageDataOffset     base offset into CBDT
//!
//!   Index sub-table formats:
//!     1: variable-metric, u32 offset[lastGid - firstGid + 2]
//!     2: constant-metric, u32 imageSize, BigGlyphMetrics (8 B)
//!     3: variable-metric, u16 offset[lastGid - firstGid + 2]
//!     4: variable-metric sparse: numGlyphs, (gid, u16 offset)[numGlyphs+1]
//!     5: constant-metric sparse: imageSize, BigGlyphMetrics, gid[]
//! ```
//!
//! Format 1 / 3 emit a "+1" sentinel offset whose only purpose is to
//! bound the last entry's length, mirroring the convention used by
//! `loca`. Formats 2 / 5 imply every glyph in the range has the same
//! pixel size and metrics, and the consumer derives each offset by
//! `imageDataOffset + index * imageSize`.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// Per-axis line metrics for one strike. Mirrors `hhea` / `vhea` but
/// expressed in pixels rather than design units. sigilbuzz exposes
/// the raw fields for downstream layout — none of them feed the
/// shaping pipeline today.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SbitLineMetrics {
    /// Distance from baseline to top of bitmap, in pixels.
    pub ascender: i8,
    /// Distance from baseline to bottom of bitmap, in pixels (negative).
    pub descender: i8,
    /// Maximum bitmap width in this strike, in pixels.
    pub width_max: u8,
    /// Caret-slope rise.
    pub caret_slope_numerator: i8,
    /// Caret-slope run.
    pub caret_slope_denominator: i8,
    /// Caret offset.
    pub caret_offset: i8,
    /// Minimum origin SB.
    pub min_origin_sb: i8,
    /// Minimum advance SB.
    pub min_advance_sb: i8,
    /// Maximum before BL.
    pub max_before_bl: i8,
    /// Minimum after BL.
    pub min_after_bl: i8,
}

/// One BitmapSize record — a single resolution at which the font
/// provides bitmap glyphs. A typical color-emoji font ships several
/// (e.g. 32, 64, 96, 128, 160 ppem) so renderers can pick the closest
/// match.
#[derive(Debug, Clone, Copy)]
pub struct BitmapSize {
    /// Offset (relative to the start of the CBLC table) to the
    /// `IndexSubTableArray` for this strike.
    pub index_sub_table_array_offset: u32,
    /// Total bytes occupied by the IndexSubTableArray + sub-tables.
    pub index_tables_size: u32,
    /// Number of `IndexSubTableArray` entries.
    pub number_of_index_sub_tables: u32,
    /// Horizontal line metrics.
    pub hori: SbitLineMetrics,
    /// Vertical line metrics.
    pub vert: SbitLineMetrics,
    /// Lowest glyph id covered by this strike.
    pub start_glyph_index: u16,
    /// Highest glyph id covered by this strike (inclusive).
    pub end_glyph_index: u16,
    /// Pixels-per-em on the x axis.
    pub ppem_x: u8,
    /// Pixels-per-em on the y axis.
    pub ppem_y: u8,
    /// Bit depth of the pixel data — 32 means RGBA8, 1 / 2 / 4 / 8
    /// indicate the older mask formats. CBDT today is always 32.
    pub bit_depth: u8,
    /// Flag byte: bit 0 = horizontal, bit 1 = vertical.
    pub flags: i8,
}

/// Big glyph metrics — the per-glyph metrics record stored either at
/// the head of an index sub-table (formats 2, 5) or inline with each
/// glyph in `CBDT` (formats 18, 19). All fields are in pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BigGlyphMetrics {
    /// Bitmap height in pixels.
    pub height: u8,
    /// Bitmap width in pixels.
    pub width: u8,
    /// Horizontal-mode side bearing.
    pub hori_bearing_x: i8,
    /// Horizontal-mode side bearing.
    pub hori_bearing_y: i8,
    /// Horizontal-mode advance.
    pub hori_advance: u8,
    /// Vertical-mode side bearing.
    pub vert_bearing_x: i8,
    /// Vertical-mode side bearing.
    pub vert_bearing_y: i8,
    /// Vertical-mode advance.
    pub vert_advance: u8,
}

impl BigGlyphMetrics {
    /// Reads an 8-byte BigGlyphMetrics record. Public-in-crate so the
    /// CBDT parser (formats 18 / 19) can share the same shape.
    #[allow(dead_code)]
    pub(crate) fn parse(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            height: r.read_u8()?,
            width: r.read_u8()?,
            hori_bearing_x: r.read_i8()?,
            hori_bearing_y: r.read_i8()?,
            hori_advance: r.read_u8()?,
            vert_bearing_x: r.read_i8()?,
            vert_bearing_y: r.read_i8()?,
            vert_advance: r.read_u8()?,
        })
    }
}

/// Small glyph metrics — the trimmed five-byte horizontal-only
/// metrics record used by CBDT formats 17 and the EBDT formats 1/2/6.
/// `vert*` fields default to zero when only the small variant is
/// present; consumers needing vertical metrics fall back to vmtx /
/// VORG.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SmallGlyphMetrics {
    /// Bitmap height in pixels.
    pub height: u8,
    /// Bitmap width in pixels.
    pub width: u8,
    /// Bearing X (horizontal mode).
    pub bearing_x: i8,
    /// Bearing Y (horizontal mode).
    pub bearing_y: i8,
    /// Advance (horizontal mode).
    pub advance: u8,
}

impl SmallGlyphMetrics {
    /// Reads a 5-byte SmallGlyphMetrics record. Public-in-crate so
    /// the CBDT parser (format 17) can share the same shape.
    #[allow(dead_code)]
    pub(crate) fn parse(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            height: r.read_u8()?,
            width: r.read_u8()?,
            bearing_x: r.read_i8()?,
            bearing_y: r.read_i8()?,
            advance: r.read_u8()?,
        })
    }
}

/// Resolved CBDT slice for one glyph at one strike: where the bitmap
/// begins inside `CBDT` and the format / metrics needed to interpret
/// it. The `metrics` field is `None` for index formats 1, 3, and 4
/// (where every glyph carries its own metrics inline in `CBDT`) and
/// `Some` for formats 2 / 5 (where the metrics are shared and stored
/// in the index sub-table header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CbdtLocation {
    /// Byte offset, relative to the start of the CBDT table, where
    /// this glyph's record begins.
    pub offset: u32,
    /// Length of the record in `CBDT`. Formats 2 / 5 share a single
    /// length across the strike; 1 / 3 / 4 derive it from successive
    /// offsets.
    pub length: u32,
    /// CBDT image format (17 / 18 / 19 most commonly).
    pub image_format: u16,
    /// Out-of-band BigGlyphMetrics for constant-metric formats
    /// (CBLC index 2 / 5). When `None`, the glyph carries its own
    /// metrics inline at the start of its CBDT record.
    pub metrics: Option<BigGlyphMetrics>,
}

/// Parsed `CBLC` table — header plus the size record array.
#[derive(Debug, Clone, Copy)]
pub struct Cblc<'a> {
    data: &'a [u8],
    sizes: &'a [u8],
    num_sizes: u32,
}

impl<'a> Cblc<'a> {
    /// Parses a `CBLC` table header. Validates that the BitmapSize
    /// records and the index sub-table arrays they point to all fit
    /// inside the table; per-glyph lookups happen lazily.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 2 && major != 3 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported CBLC major version",
            });
        }
        let num_sizes = r.read_u32()?;
        let sizes_start = r.position();
        let sizes_bytes =
            (num_sizes as usize)
                .checked_mul(48)
                .ok_or(Error::Malformed {
                    offset: sizes_start,
                    context: "CBLC sizes overflow",
                })?;
        let sizes_end =
            sizes_start
                .checked_add(sizes_bytes)
                .ok_or(Error::Malformed {
                    offset: sizes_start,
                    context: "CBLC sizes overflow",
                })?;
        if sizes_end > data.len() {
            return Err(Error::Truncated {
                offset: sizes_end,
                context: "CBLC BitmapSize array",
            });
        }
        Ok(Self {
            data,
            sizes: &data[sizes_start..sizes_end],
            num_sizes,
        })
    }

    /// Total number of strikes (BitmapSize records).
    #[must_use]
    pub fn num_sizes(&self) -> u32 {
        self.num_sizes
    }

    /// Returns the BitmapSize at index `i`, or `None` if out of range.
    #[must_use]
    pub fn size(&self, i: u32) -> Option<BitmapSize> {
        if i >= self.num_sizes {
            return None;
        }
        let off = (i as usize) * 48;
        let bytes = self.sizes.get(off..off + 48)?;
        Some(parse_bitmap_size(bytes))
    }

    /// Iterates every BitmapSize in directory order.
    pub fn sizes(&self) -> impl Iterator<Item = BitmapSize> + '_ {
        (0..self.num_sizes).filter_map(|i| self.size(i))
    }

    /// Picks the strike whose `ppem_y` is closest to `target_ppem`.
    /// Ties prefer the larger size (sharper rasterisation when down-
    /// scaling); strikes that don't cover `glyph_id` are skipped.
    /// Returns `None` if no strike covers the glyph.
    #[must_use]
    pub fn best_strike(&self, glyph_id: u16, target_ppem: u16) -> Option<BitmapSize> {
        let target = i32::from(target_ppem);
        let mut best: Option<(i32, u8, BitmapSize)> = None;
        for size in self.sizes() {
            if glyph_id < size.start_glyph_index || glyph_id > size.end_glyph_index {
                continue;
            }
            let ppem = i32::from(size.ppem_y);
            let diff = (ppem - target).abs();
            match best {
                None => best = Some((diff, size.ppem_y, size)),
                Some((bdiff, bppem, _)) => {
                    let take = diff < bdiff || (diff == bdiff && size.ppem_y > bppem);
                    if take {
                        best = Some((diff, size.ppem_y, size));
                    }
                }
            }
        }
        best.map(|(_, _, s)| s)
    }

    /// Resolves `glyph_id` inside `size` to a CBDT location record.
    /// Returns `Ok(None)` if the strike doesn't cover the glyph or
    /// the glyph's index entry maps to an empty record (a valid way
    /// to say "the strike covers this range but no bitmap is
    /// supplied for that gid").
    pub fn locate(&self, size: &BitmapSize, glyph_id: u16) -> Result<Option<CbdtLocation>> {
        if glyph_id < size.start_glyph_index || glyph_id > size.end_glyph_index {
            return Ok(None);
        }
        let array_off = size.index_sub_table_array_offset as usize;
        if size.number_of_index_sub_tables == 0 {
            return Ok(None);
        }
        // Walk the IndexSubTableArray looking for the entry covering
        // `glyph_id`. Entries are sorted by firstGlyphIndex; a linear
        // scan is fine — color-emoji fonts rarely exceed a handful.
        for i in 0..size.number_of_index_sub_tables as usize {
            let entry_off = array_off
                .checked_add(i.checked_mul(8).ok_or(Error::Malformed {
                    offset: array_off,
                    context: "IndexSubTableArray overflow",
                })?)
                .ok_or(Error::Malformed {
                    offset: array_off,
                    context: "IndexSubTableArray overflow",
                })?;
            if entry_off + 8 > self.data.len() {
                return Err(Error::Truncated {
                    offset: entry_off,
                    context: "IndexSubTableArray entry",
                });
            }
            let mut er = Reader::at(self.data, entry_off)?;
            let first_gid = er.read_u16()?;
            let last_gid = er.read_u16()?;
            let additional_off = er.read_u32()? as usize;
            if glyph_id < first_gid || glyph_id > last_gid {
                continue;
            }
            // additionalOffsetToIndexSubTable is relative to the
            // BitmapSize record's IndexSubTableArray base offset.
            let sub_off = array_off
                .checked_add(additional_off)
                .ok_or(Error::Malformed {
                    offset: array_off,
                    context: "IndexSubTable offset overflow",
                })?;
            return self.locate_in_subtable(sub_off, first_gid, last_gid, glyph_id);
        }
        Ok(None)
    }

    fn locate_in_subtable(
        &self,
        sub_off: usize,
        first_gid: u16,
        last_gid: u16,
        glyph_id: u16,
    ) -> Result<Option<CbdtLocation>> {
        let mut r = Reader::at(self.data, sub_off)?;
        let index_format = r.read_u16()?;
        let image_format = r.read_u16()?;
        let image_data_offset = r.read_u32()?;
        let count = u32::from(last_gid - first_gid + 1);
        let local_idx = u32::from(glyph_id - first_gid);

        match index_format {
            1 => self.locate_fmt1(&r, image_format, image_data_offset, count, local_idx),
            2 => locate_fmt2(&mut r, image_format, image_data_offset, local_idx),
            3 => self.locate_fmt3(&r, image_format, image_data_offset, count, local_idx),
            4 => self.locate_fmt4(&mut r, image_format, image_data_offset, glyph_id),
            5 => self.locate_fmt5(&mut r, image_format, image_data_offset, glyph_id),
            _ => Err(Error::Unsupported {
                context: "CBLC index sub-table format",
            }),
        }
    }

    fn locate_fmt1(
        &self,
        r: &Reader<'_>,
        image_format: u16,
        image_data_offset: u32,
        count: u32,
        local_idx: u32,
    ) -> Result<Option<CbdtLocation>> {
        let arr_start = r.position();
        let bytes_needed = (count as usize + 1) * 4;
        if arr_start + bytes_needed > self.data.len() {
            return Err(Error::Truncated {
                offset: arr_start + bytes_needed,
                context: "CBLC index format 1 array",
            });
        }
        let mut ar = Reader::at(self.data, arr_start + (local_idx as usize) * 4)?;
        let off0 = ar.read_u32()?;
        let off1 = ar.read_u32()?;
        if off1 < off0 {
            return Err(Error::Malformed {
                offset: ar.position(),
                context: "CBLC fmt 1 offsets non-monotonic",
            });
        }
        if off0 == off1 {
            return Ok(None);
        }
        Ok(Some(CbdtLocation {
            offset: image_data_offset + off0,
            length: off1 - off0,
            image_format,
            metrics: None,
        }))
    }

    fn locate_fmt3(
        &self,
        r: &Reader<'_>,
        image_format: u16,
        image_data_offset: u32,
        count: u32,
        local_idx: u32,
    ) -> Result<Option<CbdtLocation>> {
        let arr_start = r.position();
        let bytes_needed = (count as usize + 1) * 2;
        if arr_start + bytes_needed > self.data.len() {
            return Err(Error::Truncated {
                offset: arr_start + bytes_needed,
                context: "CBLC index format 3 array",
            });
        }
        let mut ar = Reader::at(self.data, arr_start + (local_idx as usize) * 2)?;
        let off0 = u32::from(ar.read_u16()?);
        let off1 = u32::from(ar.read_u16()?);
        if off1 < off0 {
            return Err(Error::Malformed {
                offset: ar.position(),
                context: "CBLC fmt 3 offsets non-monotonic",
            });
        }
        if off0 == off1 {
            return Ok(None);
        }
        Ok(Some(CbdtLocation {
            offset: image_data_offset + off0,
            length: off1 - off0,
            image_format,
            metrics: None,
        }))
    }

    fn locate_fmt4(
        &self,
        r: &mut Reader<'_>,
        image_format: u16,
        image_data_offset: u32,
        glyph_id: u16,
    ) -> Result<Option<CbdtLocation>> {
        let num_glyphs = r.read_u32()?;
        let arr_start = r.position();
        let bytes_needed = (num_glyphs as usize + 1) * 4;
        if arr_start + bytes_needed > self.data.len() {
            return Err(Error::Truncated {
                offset: arr_start + bytes_needed,
                context: "CBLC index format 4 array",
            });
        }
        for i in 0..num_glyphs {
            let pair_off = arr_start + (i as usize) * 4;
            let mut pr = Reader::at(self.data, pair_off)?;
            let gid = pr.read_u16()?;
            let off0 = u32::from(pr.read_u16()?);
            if gid != glyph_id {
                continue;
            }
            let next_off = arr_start + (i as usize + 1) * 4;
            let mut nr = Reader::at(self.data, next_off + 2)?;
            let off1 = u32::from(nr.read_u16()?);
            if off1 < off0 {
                return Err(Error::Malformed {
                    offset: next_off,
                    context: "CBLC fmt 4 offsets non-monotonic",
                });
            }
            if off0 == off1 {
                return Ok(None);
            }
            return Ok(Some(CbdtLocation {
                offset: image_data_offset + off0,
                length: off1 - off0,
                image_format,
                metrics: None,
            }));
        }
        Ok(None)
    }

    fn locate_fmt5(
        &self,
        r: &mut Reader<'_>,
        image_format: u16,
        image_data_offset: u32,
        glyph_id: u16,
    ) -> Result<Option<CbdtLocation>> {
        let image_size = r.read_u32()?;
        let metrics = BigGlyphMetrics::parse(r)?;
        let num_glyphs = r.read_u32()?;
        let arr_start = r.position();
        let bytes_needed = (num_glyphs as usize) * 2;
        if arr_start + bytes_needed > self.data.len() {
            return Err(Error::Truncated {
                offset: arr_start + bytes_needed,
                context: "CBLC index format 5 array",
            });
        }
        for i in 0..num_glyphs {
            let off = arr_start + (i as usize) * 2;
            let mut pr = Reader::at(self.data, off)?;
            let gid = pr.read_u16()?;
            if gid == glyph_id {
                return Ok(Some(CbdtLocation {
                    offset: image_data_offset + i * image_size,
                    length: image_size,
                    image_format,
                    metrics: Some(metrics),
                }));
            }
        }
        Ok(None)
    }
}

fn locate_fmt2(
    r: &mut Reader<'_>,
    image_format: u16,
    image_data_offset: u32,
    local_idx: u32,
) -> Result<Option<CbdtLocation>> {
    let image_size = r.read_u32()?;
    let metrics = BigGlyphMetrics::parse(r)?;
    Ok(Some(CbdtLocation {
        offset: image_data_offset + local_idx * image_size,
        length: image_size,
        image_format,
        metrics: Some(metrics),
    }))
}

fn parse_bitmap_size(bytes: &[u8]) -> BitmapSize {
    // 48 bytes verified by caller; unwrap-free reader path keeps
    // panics out of the data path.
    let mut r = Reader::new(bytes);
    let index_sub_table_array_offset = r.read_u32().unwrap_or(0);
    let index_tables_size = r.read_u32().unwrap_or(0);
    let number_of_index_sub_tables = r.read_u32().unwrap_or(0);
    let _color_ref = r.read_u32().unwrap_or(0);
    let hori = parse_sbit_line_metrics(&mut r);
    let vert = parse_sbit_line_metrics(&mut r);
    let start_glyph_index = r.read_u16().unwrap_or(0);
    let end_glyph_index = r.read_u16().unwrap_or(0);
    let ppem_x = r.read_u8().unwrap_or(0);
    let ppem_y = r.read_u8().unwrap_or(0);
    let bit_depth = r.read_u8().unwrap_or(0);
    let flags = r.read_i8().unwrap_or(0);
    BitmapSize {
        index_sub_table_array_offset,
        index_tables_size,
        number_of_index_sub_tables,
        hori,
        vert,
        start_glyph_index,
        end_glyph_index,
        ppem_x,
        ppem_y,
        bit_depth,
        flags,
    }
}

fn parse_sbit_line_metrics(r: &mut Reader<'_>) -> SbitLineMetrics {
    let m = SbitLineMetrics {
        ascender: r.read_i8().unwrap_or(0),
        descender: r.read_i8().unwrap_or(0),
        width_max: r.read_u8().unwrap_or(0),
        caret_slope_numerator: r.read_i8().unwrap_or(0),
        caret_slope_denominator: r.read_i8().unwrap_or(0),
        caret_offset: r.read_i8().unwrap_or(0),
        min_origin_sb: r.read_i8().unwrap_or(0),
        min_advance_sb: r.read_i8().unwrap_or(0),
        max_before_bl: r.read_i8().unwrap_or(0),
        min_after_bl: r.read_i8().unwrap_or(0),
    };
    // pad1, pad2 — keep the 12-byte stride.
    let _ = r.skip(2);
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Builds a CBLC blob with one strike covering glyph ids 1..=2 at
    /// 32 ppem, exposing `index_format` so each test can pick its
    /// flavour. The IndexSubTable's `image_data_offset` is whatever
    /// the test passes in.
    struct CblcBuilder {
        ppem: u8,
        index_format: u16,
        image_format: u16,
        image_data_offset: u32,
        sub_payload: Vec<u8>,
    }

    impl CblcBuilder {
        fn build(&self) -> Vec<u8> {
            // header (8) + size (48) + array entry (8) + subtable header (8) + payload
            let header = 8u32;
            let bitmap_size = 48u32;
            let index_array_off = header + bitmap_size;
            let index_array_bytes = 8u32; // one entry
            let sub_off_relative = index_array_bytes;
            let sub_header_bytes = 8u32;

            let mut out = Vec::new();
            // CBLC header
            out.extend_from_slice(&3u16.to_be_bytes()); // major
            out.extend_from_slice(&0u16.to_be_bytes()); // minor
            out.extend_from_slice(&1u32.to_be_bytes()); // numSizes

            // BitmapSize record
            out.extend_from_slice(&index_array_off.to_be_bytes());
            out.extend_from_slice(
                &(index_array_bytes + sub_header_bytes + self.sub_payload.len() as u32)
                    .to_be_bytes(),
            );
            out.extend_from_slice(&1u32.to_be_bytes()); // numberOfIndexSubTables
            out.extend_from_slice(&0u32.to_be_bytes()); // colorRef
            out.extend_from_slice(&[0u8; 12]); // hori line metrics
            out.extend_from_slice(&[0u8; 12]); // vert line metrics
            out.extend_from_slice(&1u16.to_be_bytes()); // startGlyphIndex
            out.extend_from_slice(&2u16.to_be_bytes()); // endGlyphIndex
            out.push(self.ppem); // ppemX
            out.push(self.ppem); // ppemY
            out.push(32); // bit depth
            out.push(0x01); // flags = horizontal

            // IndexSubTableArray entry (1 entry)
            out.extend_from_slice(&1u16.to_be_bytes()); // firstGlyphIndex
            out.extend_from_slice(&2u16.to_be_bytes()); // lastGlyphIndex
            out.extend_from_slice(&sub_off_relative.to_be_bytes());

            // IndexSubTable header
            out.extend_from_slice(&self.index_format.to_be_bytes());
            out.extend_from_slice(&self.image_format.to_be_bytes());
            out.extend_from_slice(&self.image_data_offset.to_be_bytes());

            // Payload (per format)
            out.extend_from_slice(&self.sub_payload);
            out
        }
    }

    #[test]
    fn parses_header_and_strike_record() {
        let blob = CblcBuilder {
            ppem: 32,
            index_format: 1,
            image_format: 17,
            image_data_offset: 0,
            sub_payload: {
                // Three u32 offsets covering 2 glyphs.
                let mut p = Vec::new();
                p.extend_from_slice(&0u32.to_be_bytes());
                p.extend_from_slice(&100u32.to_be_bytes());
                p.extend_from_slice(&250u32.to_be_bytes());
                p
            },
        }
        .build();
        let cblc = Cblc::parse(&blob).unwrap();
        assert_eq!(cblc.num_sizes(), 1);
        let s = cblc.size(0).unwrap();
        assert_eq!(s.ppem_x, 32);
        assert_eq!(s.ppem_y, 32);
        assert_eq!(s.start_glyph_index, 1);
        assert_eq!(s.end_glyph_index, 2);
        assert_eq!(s.number_of_index_sub_tables, 1);
    }

    #[test]
    fn locate_format1_returns_offset_and_length() {
        let blob = CblcBuilder {
            ppem: 32,
            index_format: 1,
            image_format: 17,
            image_data_offset: 1000,
            sub_payload: {
                let mut p = Vec::new();
                p.extend_from_slice(&0u32.to_be_bytes()); // gid 1 starts here
                p.extend_from_slice(&100u32.to_be_bytes()); // gid 2 starts here
                p.extend_from_slice(&250u32.to_be_bytes()); // sentinel
                p
            },
        }
        .build();
        let cblc = Cblc::parse(&blob).unwrap();
        let size = cblc.size(0).unwrap();
        let loc1 = cblc.locate(&size, 1).unwrap().unwrap();
        assert_eq!(loc1.offset, 1000);
        assert_eq!(loc1.length, 100);
        assert_eq!(loc1.image_format, 17);
        assert!(loc1.metrics.is_none());

        let loc2 = cblc.locate(&size, 2).unwrap().unwrap();
        assert_eq!(loc2.offset, 1100);
        assert_eq!(loc2.length, 150);
    }

    #[test]
    fn locate_format2_uses_constant_metrics() {
        let blob = CblcBuilder {
            ppem: 32,
            index_format: 2,
            image_format: 19,
            image_data_offset: 500,
            sub_payload: {
                let mut p = Vec::new();
                p.extend_from_slice(&80u32.to_be_bytes()); // imageSize
                // BigGlyphMetrics: h=10, w=12, hbx=1, hby=2, hadv=15, vbx=0, vby=0, vadv=0
                p.extend_from_slice(&[10, 12, 1, 2, 15, 0, 0, 0]);
                p
            },
        }
        .build();
        let cblc = Cblc::parse(&blob).unwrap();
        let size = cblc.size(0).unwrap();
        let loc1 = cblc.locate(&size, 1).unwrap().unwrap();
        assert_eq!(loc1.offset, 500);
        assert_eq!(loc1.length, 80);
        assert_eq!(loc1.metrics.unwrap().height, 10);
        let loc2 = cblc.locate(&size, 2).unwrap().unwrap();
        assert_eq!(loc2.offset, 580);
        assert_eq!(loc2.length, 80);
    }

    #[test]
    fn locate_format3_u16_offsets() {
        let blob = CblcBuilder {
            ppem: 32,
            index_format: 3,
            image_format: 17,
            image_data_offset: 200,
            sub_payload: {
                let mut p = Vec::new();
                p.extend_from_slice(&0u16.to_be_bytes());
                p.extend_from_slice(&30u16.to_be_bytes());
                p.extend_from_slice(&80u16.to_be_bytes());
                p
            },
        }
        .build();
        let cblc = Cblc::parse(&blob).unwrap();
        let size = cblc.size(0).unwrap();
        let loc1 = cblc.locate(&size, 1).unwrap().unwrap();
        assert_eq!(loc1.offset, 200);
        assert_eq!(loc1.length, 30);
        let loc2 = cblc.locate(&size, 2).unwrap().unwrap();
        assert_eq!(loc2.offset, 230);
        assert_eq!(loc2.length, 50);
    }

    #[test]
    fn locate_format4_sparse_returns_none_for_missing() {
        let blob = CblcBuilder {
            ppem: 32,
            index_format: 4,
            image_format: 17,
            image_data_offset: 0,
            sub_payload: {
                let mut p = Vec::new();
                p.extend_from_slice(&1u32.to_be_bytes()); // numGlyphs (just gid 2)
                p.extend_from_slice(&2u16.to_be_bytes()); // gid
                p.extend_from_slice(&0u16.to_be_bytes()); // offset
                p.extend_from_slice(&0u16.to_be_bytes()); // sentinel gid
                p.extend_from_slice(&50u16.to_be_bytes()); // sentinel offset
                p
            },
        }
        .build();
        let cblc = Cblc::parse(&blob).unwrap();
        let size = cblc.size(0).unwrap();
        // gid 1: not in sparse list → None
        assert!(cblc.locate(&size, 1).unwrap().is_none());
        let loc2 = cblc.locate(&size, 2).unwrap().unwrap();
        assert_eq!(loc2.offset, 0);
        assert_eq!(loc2.length, 50);
    }

    #[test]
    fn locate_format5_constant_metric_sparse() {
        let blob = CblcBuilder {
            ppem: 32,
            index_format: 5,
            image_format: 19,
            image_data_offset: 1000,
            sub_payload: {
                let mut p = Vec::new();
                p.extend_from_slice(&60u32.to_be_bytes()); // imageSize
                p.extend_from_slice(&[8, 8, 0, 8, 9, 0, 0, 0]); // metrics
                p.extend_from_slice(&2u32.to_be_bytes()); // numGlyphs
                p.extend_from_slice(&1u16.to_be_bytes()); // gid 1
                p.extend_from_slice(&2u16.to_be_bytes()); // gid 2
                p
            },
        }
        .build();
        let cblc = Cblc::parse(&blob).unwrap();
        let size = cblc.size(0).unwrap();
        let loc1 = cblc.locate(&size, 1).unwrap().unwrap();
        assert_eq!(loc1.offset, 1000);
        assert_eq!(loc1.length, 60);
        assert_eq!(loc1.metrics.unwrap().width, 8);
        let loc2 = cblc.locate(&size, 2).unwrap().unwrap();
        assert_eq!(loc2.offset, 1060);
    }

    #[test]
    fn best_strike_picks_closest_ppem() {
        // Build a multi-strike CBLC by assembling two consecutive
        // BitmapSize records with no IndexSubTable bodies (best_strike
        // doesn't read those).
        let mut out = Vec::new();
        out.extend_from_slice(&3u16.to_be_bytes()); // major
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
        out.extend_from_slice(&2u32.to_be_bytes()); // numSizes
        for ppem in [16u8, 64u8] {
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&[0u8; 12]);
            out.extend_from_slice(&[0u8; 12]);
            out.extend_from_slice(&1u16.to_be_bytes());
            out.extend_from_slice(&5u16.to_be_bytes());
            out.push(ppem);
            out.push(ppem);
            out.push(32);
            out.push(1);
        }
        let cblc = Cblc::parse(&out).unwrap();
        // 16 vs 64 → distance 24 vs 24 from ppem 40; tie prefers larger.
        let strike40 = cblc.best_strike(3, 40).unwrap();
        assert_eq!(strike40.ppem_y, 64);
        // ppem 20: distance 4 (16) vs 44 (64) → 16 wins.
        let strike20 = cblc.best_strike(3, 20).unwrap();
        assert_eq!(strike20.ppem_y, 16);
        // ppem 100: 64 wins.
        let strike100 = cblc.best_strike(3, 100).unwrap();
        assert_eq!(strike100.ppem_y, 64);
    }

    #[test]
    fn rejects_unknown_version() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&5u16.to_be_bytes());
        blob.extend_from_slice(&0u16.to_be_bytes());
        blob.extend_from_slice(&0u32.to_be_bytes());
        let err = Cblc::parse(&blob).unwrap_err();
        assert!(matches!(err, Error::Malformed { .. }));
    }
}
