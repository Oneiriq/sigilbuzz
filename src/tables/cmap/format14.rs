//! `cmap` subtable format 14: Unicode Variation Sequences.
//!
//! A variation sequence is a base character followed by a variation
//! selector (U+FE00..U+FE0F, U+E0100..U+E01EF, or any other selector
//! the font lists). Format 14 has one record per selector, sorted by
//! selector. Each record has two optional tables:
//!
//! - Default UVS: ranges of base characters whose sequence uses the
//!   base character's usual glyph (the one the Unicode subtable maps).
//! - Non-default UVS: base characters mapped to their own glyph.
//!
//! ```text
//!   Format14:            u16 format (14), u32 length, u32 numRecords,
//!                        VariationSelectorRecord[numRecords]
//!   VariationSelector-   u24 varSelector, Offset32 defaultUVS,
//!   Record (11 bytes):   Offset32 nonDefaultUVS (0 means none)
//!   DefaultUVS:          u32 numRanges, { u24 start, u8 additionalCount }[]
//!   NonDefaultUVS:       u32 numMappings, { u24 unicode, u16 glyph }[]
//! ```
//!
//! Offsets count from the start of the format 14 subtable. The lookup
//! follows HarfBuzz's `CmapSubtableFormat14::get_glyph_variant`
//! (`hb-ot-cmap-table.hh`, HarfBuzz 14.5.0), including its binary
//! search. Where HarfBuzz's sanitizer would neuter an offset whose
//! table does not fit, the table reads as empty here.

use alloc::vec::Vec;
use core::cmp::Ordering;

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// Bytes before the first variation selector record.
const HEADER_SIZE: usize = 10;
/// Bytes per `VariationSelectorRecord`.
const RECORD_SIZE: usize = 11;
/// Bytes per `UnicodeRange` in a Default UVS table.
const RANGE_SIZE: usize = 4;
/// Bytes per `UVSMapping` in a Non-default UVS table.
const MAPPING_SIZE: usize = 5;
/// The largest Unicode code point (`HB_UNICODE_MAX`).
const UNICODE_MAX: u32 = 0x10_FFFF;

/// What a variation sequence resolves to (HarfBuzz's
/// `glyph_variant_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GlyphVariant {
    /// The font does not list the sequence.
    NotFound,
    /// The sequence has its own glyph.
    Found(u16),
    /// The sequence uses the base character's usual glyph.
    UseDefault,
}

/// A parsed format 14 subtable.
#[derive(Debug, Clone, Copy)]
pub(super) struct Format14<'a> {
    /// The subtable, from its first byte to the end of the `cmap`.
    data: &'a [u8],
    /// Number of variation selector records. All of them fit in
    /// `data`.
    num_records: usize,
}

impl<'a> Format14<'a> {
    /// Parses the subtable header. Fails when the record array does
    /// not fit, which HarfBuzz treats as a missing subtable.
    pub(super) fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        if r.read_u16()? != 14 {
            return Err(Error::Malformed {
                offset: 0,
                context: "format 14 parser invoked on non-format-14 subtable",
            });
        }
        // HarfBuzz does not check the length field against the data,
        // so neither does this parser.
        let _length = r.read_u32()?;
        let num_records = r.read_u32()? as usize;
        let need = num_records.saturating_mul(RECORD_SIZE);
        if HEADER_SIZE.saturating_add(need) > data.len() {
            return Err(Error::Truncated {
                offset: HEADER_SIZE,
                context: "format 14 variation selector records do not fit",
            });
        }
        Ok(Self { data, num_records })
    }

    /// Record `i`: `(varSelector, defaultUVS, nonDefaultUVS)`.
    fn record(&self, i: usize) -> Option<(u32, u32, u32)> {
        let off = HEADER_SIZE.checked_add(i.checked_mul(RECORD_SIZE)?)?;
        let bytes = self.data.get(off..off.checked_add(RECORD_SIZE)?)?;
        Some((u24(bytes, 0)?, u32_at(bytes, 3)?, u32_at(bytes, 7)?))
    }

    /// The record for `selector`, found the way HarfBuzz finds it.
    fn find_record(&self, selector: u32) -> Option<(u32, u32, u32)> {
        let index = bsearch(self.num_records, |i| {
            self.record(i)
                .map_or(Ordering::Equal, |(vs, _, _)| selector.cmp(&vs))
        })?;
        self.record(index)
    }

    /// The elements of the array at `offset` (a `u32` count and then
    /// `count` elements of `size` bytes). Empty when the offset is zero
    /// or the array does not fit.
    fn array(&self, offset: u32, size: usize) -> &'a [u8] {
        let data: &'a [u8] = self.data;
        let offset = offset as usize;
        if offset == 0 {
            return &[];
        }
        let Some(count) = offset.checked_add(4).and_then(|end| {
            let bytes = data.get(offset..end)?;
            u32_at(bytes, 0)
        }) else {
            return &[];
        };
        let start = offset.saturating_add(4);
        let len = (count as usize).saturating_mul(size);
        start
            .checked_add(len)
            .and_then(|end| data.get(start..end))
            .unwrap_or(&[])
    }

    /// `glyph_variant_t get_glyph_variant (codepoint, selector)`.
    pub(super) fn glyph_variant(&self, ch: u32, selector: u32) -> GlyphVariant {
        let Some((_, default_uvs, non_default_uvs)) = self.find_record(selector) else {
            return GlyphVariant::NotFound;
        };
        let ranges = self.array(default_uvs, RANGE_SIZE);
        let in_default = bsearch(ranges.len() / RANGE_SIZE, |i| {
            range_at(ranges, i).map_or(Ordering::Equal, |(start, count)| {
                if ch < start {
                    Ordering::Less
                } else if ch > start.saturating_add(count) {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                }
            })
        });
        if in_default.is_some() {
            return GlyphVariant::UseDefault;
        }
        let mappings = self.array(non_default_uvs, MAPPING_SIZE);
        let glyph = bsearch(mappings.len() / MAPPING_SIZE, |i| {
            mapping_at(mappings, i).map_or(Ordering::Equal, |(unicode, _)| ch.cmp(&unicode))
        })
        .and_then(|i| mapping_at(mappings, i))
        .map_or(0, |(_, glyph)| glyph);
        if glyph == 0 {
            GlyphVariant::NotFound
        } else {
            GlyphVariant::Found(glyph)
        }
    }

    /// Every selector the subtable has a record for, sorted and
    /// without repeats (`collect_variation_selectors`).
    pub(super) fn selectors(&self) -> Vec<u32> {
        let mut out: Vec<u32> = (0..self.num_records)
            .filter_map(|i| self.record(i).map(|(vs, _, _)| vs))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Every base character the record for `selector` lists, in its
    /// Default and Non-default UVS tables, sorted and without repeats
    /// (`collect_variation_unicodes`). Default ranges stop at
    /// U+10FFFF, as in HarfBuzz.
    pub(super) fn unicodes(&self, selector: u32) -> Vec<u32> {
        let Some((_, default_uvs, non_default_uvs)) = self.find_record(selector) else {
            return Vec::new();
        };
        let ranges_bytes = self.array(default_uvs, RANGE_SIZE);
        let mut ranges: Vec<(u32, u32)> = (0..ranges_bytes.len() / RANGE_SIZE)
            .filter_map(|i| range_at(ranges_bytes, i))
            .filter(|&(start, _)| start <= UNICODE_MAX)
            .map(|(start, count)| (start, start.saturating_add(count).min(UNICODE_MAX)))
            .collect();
        // A malformed table can list overlapping ranges. Merging them
        // first keeps the output at most one entry per code point.
        ranges.sort_unstable();
        let mut out: Vec<u32> = Vec::new();
        let mut next = 0u32;
        for (start, end) in ranges {
            let from = start.max(next);
            if from <= end {
                out.extend(from..=end);
                next = end.saturating_add(1);
            }
        }
        let mappings = self.array(non_default_uvs, MAPPING_SIZE);
        out.extend(
            (0..mappings.len() / MAPPING_SIZE)
                .filter_map(|i| mapping_at(mappings, i).map(|(unicode, _)| unicode)),
        );
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// HarfBuzz's `hb_bsearch_impl` over `len` elements: `cmp(i)` orders
/// the key against element `i`. It probes the same elements as
/// HarfBuzz, so an unsorted (malformed) array gives the same answer.
fn bsearch(len: usize, cmp: impl Fn(usize) -> Ordering) -> Option<usize> {
    let (mut lo, mut hi) = (0usize, len);
    while lo < hi {
        // HarfBuzz's `(min + max) / 2` with an inclusive `max`.
        let mid = lo + (hi - 1 - lo) / 2;
        match cmp(mid) {
            Ordering::Less => hi = mid,
            Ordering::Greater => lo = mid + 1,
            Ordering::Equal => return Some(mid),
        }
    }
    None
}

/// Default UVS range `i`: `(startUnicodeValue, additionalCount)`.
fn range_at(bytes: &[u8], i: usize) -> Option<(u32, u32)> {
    let off = i.checked_mul(RANGE_SIZE)?;
    Some((
        u24(bytes, off)?,
        u32::from(*bytes.get(off.checked_add(3)?)?),
    ))
}

/// Non-default UVS mapping `i`: `(unicodeValue, glyphID)`.
fn mapping_at(bytes: &[u8], i: usize) -> Option<(u32, u16)> {
    let off = i.checked_mul(MAPPING_SIZE)?;
    let glyph = u16_at(bytes, off.checked_add(3)?)?;
    Some((u24(bytes, off)?, glyph))
}

/// The `N` bytes at `off`.
fn bytes_at<const N: usize>(bytes: &[u8], off: usize) -> Option<[u8; N]> {
    bytes.get(off..off.checked_add(N)?)?.try_into().ok()
}

/// Big-endian 16-bit value at `off`.
fn u16_at(bytes: &[u8], off: usize) -> Option<u16> {
    bytes_at(bytes, off).map(u16::from_be_bytes)
}

/// Big-endian 24-bit value at `off`.
fn u24(bytes: &[u8], off: usize) -> Option<u32> {
    let [a, b, c] = bytes_at(bytes, off)?;
    Some(u32::from_be_bytes([0, a, b, c]))
}

/// Big-endian 32-bit value at `off`.
fn u32_at(bytes: &[u8], off: usize) -> Option<u32> {
    bytes_at(bytes, off).map(u32::from_be_bytes)
}

/// One selector for [`build_format14`]: the selector, its Default UVS
/// ranges, and its Non-default UVS mappings.
#[cfg(test)]
pub(crate) type UvsRecord<'a> = (u32, &'a [(u32, u8)], &'a [(u32, u16)]);

/// Builds a format 14 subtable: for each selector, its Default UVS
/// ranges `(start, additionalCount)` and Non-default UVS mappings
/// `(unicode, glyph)`. An empty list gets a zero offset.
#[cfg(test)]
pub(crate) fn build_format14(records: &[UvsRecord<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&14u16.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // length, patched below
    out.extend_from_slice(&(records.len() as u32).to_be_bytes());
    let mut tables: Vec<u8> = Vec::new();
    let tables_start = HEADER_SIZE + records.len() * RECORD_SIZE;
    for &(selector, ranges, mappings) in records {
        out.extend_from_slice(&selector.to_be_bytes()[1..]);
        if ranges.is_empty() {
            out.extend_from_slice(&0u32.to_be_bytes());
        } else {
            out.extend_from_slice(&((tables_start + tables.len()) as u32).to_be_bytes());
            tables.extend_from_slice(&(ranges.len() as u32).to_be_bytes());
            for &(start, count) in ranges {
                tables.extend_from_slice(&start.to_be_bytes()[1..]);
                tables.push(count);
            }
        }
        if mappings.is_empty() {
            out.extend_from_slice(&0u32.to_be_bytes());
        } else {
            out.extend_from_slice(&((tables_start + tables.len()) as u32).to_be_bytes());
            tables.extend_from_slice(&(mappings.len() as u32).to_be_bytes());
            for &(unicode, glyph) in mappings {
                tables.extend_from_slice(&unicode.to_be_bytes()[1..]);
                tables.extend_from_slice(&glyph.to_be_bytes());
            }
        }
    }
    out.extend_from_slice(&tables);
    let length = out.len() as u32;
    out[2..6].copy_from_slice(&length.to_be_bytes());
    out
}

#[cfg(test)]
mod tests;
