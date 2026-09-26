//! `cmap`: character to glyph index mapping.
//!
//! A `cmap` table is a wrapper around one or more *subtables*, each
//! declaring a `(platform, encoding)` pair and a format. sigilbuzz
//! scans the wrapper, scores each subtable against a fixed preference
//! list, and parses only the winner. Everything else is ignored.
//!
//! # Supported subtable formats
//!
//! - **Format 4**: segmented mapping of the Basic Multilingual Plane
//!   (U+0000..U+FFFF). The format every Latin / Cyrillic / Greek font
//!   in existence has.
//! - **Format 12**: sparse groups covering the full Unicode range,
//!   including supplementary planes. Preferred over format 4 when
//!   both are present because it can answer astral codepoints.
//!
//! # Subtable preference
//!
//! Scored low-is-best by `(format_tier, platform_tier)`:
//!
//! | format tier | platform tier | `(platform, encoding)` |
//! |-------------|---------------|------------------------|
//! | 0 (fmt 12)  | 0             | (0, any)               |
//! | 0 (fmt 12)  | 1             | (3, 10)                |
//! | 1 (fmt 4)   | 0             | (0, any)               |
//! | 1 (fmt 4)   | 2             | (3, 1)                 |
//!
//! Microsoft Symbol encoding `(3, 0)` is intentionally ignored: real
//! Symbol fonts need per-codepoint PUA remapping and the handful of
//! glyphs that matters will be reachable through another subtable in
//! any font sigilbuzz cares about for M1.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// Parsed `cmap` with its best available subtable pre-selected.
#[derive(Debug, Clone)]
pub struct Cmap<'a> {
    subtable: Subtable<'a>,
}

#[derive(Debug, Clone)]
enum Subtable<'a> {
    Format4(Format4<'a>),
    Format12(Format12<'a>),
}

impl<'a> Cmap<'a> {
    /// Parses a `cmap` table, selecting the best available subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);

        let version = r.read_u16()?;
        if version != 0 {
            return Err(Error::Malformed {
                offset: 0,
                context: "cmap version must be 0",
            });
        }

        let num_tables = r.read_u16()?;
        if num_tables == 0 {
            return Err(Error::Malformed {
                offset: 2,
                context: "cmap has zero subtables",
            });
        }

        // First pass: pick the highest-scoring encoding record.
        let mut best: Option<(u32, u32)> = None; // (score, subtable_offset)
        for _ in 0..num_tables {
            let platform_id = r.read_u16()?;
            let encoding_id = r.read_u16()?;
            let subtable_offset = r.read_u32()?;

            let Some(score) = encoding_score(platform_id, encoding_id, data, subtable_offset)
            else {
                continue;
            };

            if best.map_or(true, |(prev, _)| score < prev) {
                best = Some((score, subtable_offset));
            }
        }

        let (_score, subtable_offset) = best.ok_or(Error::Unsupported {
            context: "no supported cmap subtable (need format 4 or 12 on a known platform)",
        })?;

        let subtable_data = data
            .get(subtable_offset as usize..)
            .ok_or(Error::Malformed {
                offset: subtable_offset as usize,
                context: "cmap subtable offset points outside table",
            })?;
        let format = u16::from_be_bytes([
            *subtable_data.first().ok_or(Error::Truncated {
                offset: subtable_offset as usize,
                context: "cmap subtable header",
            })?,
            *subtable_data.get(1).ok_or(Error::Truncated {
                offset: subtable_offset as usize + 1,
                context: "cmap subtable header",
            })?,
        ]);

        let subtable = match format {
            4 => Subtable::Format4(Format4::parse(subtable_data)?),
            12 => Subtable::Format12(Format12::parse(subtable_data)?),
            _ => {
                return Err(Error::Unsupported {
                    context: "cmap subtable format selected but unsupported",
                });
            }
        };

        Ok(Self { subtable })
    }

    /// Resolves a character to its glyph index. Returns `None` when
    /// the font has no glyph for this codepoint.
    ///
    /// Glyph index `0` is the "missing glyph" by convention and is
    /// treated as "no match" here. Callers that want the .notdef
    /// glyph for unmappable text should fall back to 0 explicitly.
    #[must_use]
    pub fn glyph_id(&self, ch: char) -> Option<u16> {
        let gid = match &self.subtable {
            Subtable::Format4(f) => f.lookup(ch as u32),
            Subtable::Format12(f) => f.lookup(ch as u32),
        }?;
        if gid == 0 {
            None
        } else {
            Some(gid)
        }
    }
}

// Cheaper-is-better score. Returns None for unsupported combinations.
// We peek at the subtable's format byte so Symbol fonts declared as
// format 4 under (3, 0) still score, but we give them the lowest
// priority.
fn encoding_score(platform: u16, encoding: u16, data: &[u8], subtable_offset: u32) -> Option<u32> {
    // Peek the first two bytes of the subtable to learn its format.
    let start = subtable_offset as usize;
    if start + 2 > data.len() {
        return None;
    }
    let format = u16::from_be_bytes([data[start], data[start + 1]]);

    let format_tier: u32 = match format {
        12 => 0,
        4 => 1,
        _ => return None,
    };
    let platform_tier: u32 = match (platform, encoding) {
        (0, _) => 0,  // Unicode platform, any encoding
        (3, 10) => 1, // Windows UCS-4
        (3, 1) => 2,  // Windows BMP
        _ => return None,
    };
    Some((format_tier << 16) | platform_tier)
}

// Apply the format 4 idDelta to a raw u16. The spec computes
// `(base + delta) mod 65536` with wrapping arithmetic, so we use
// `u16::wrapping_add` on the bitwise representation: adding a
// negative delta is equivalent to wrapping_add of its unsigned
// representation.
#[inline]
fn apply_delta(base: u16, delta: i16) -> u16 {
    base.wrapping_add(delta as u16)
}

// --------------------------------------------------------------------------
// Format 4
// --------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Format4<'a> {
    data: &'a [u8],
    seg_count: u16,
    end_codes_off: usize,
    start_codes_off: usize,
    id_deltas_off: usize,
    id_range_offsets_off: usize,
}

impl<'a> Format4<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 4 {
            return Err(Error::Malformed {
                offset: 0,
                context: "format 4 parser invoked on non-format-4 subtable",
            });
        }
        let length = r.read_u16()? as usize;
        if length > data.len() {
            return Err(Error::Truncated {
                offset: 2,
                context: "format 4 length exceeds available bytes",
            });
        }
        let _language = r.read_u16()?;
        let seg_count_x2 = r.read_u16()?;
        if seg_count_x2 == 0 || seg_count_x2 % 2 != 0 {
            return Err(Error::Malformed {
                offset: r.position() - 2,
                context: "format 4 segCountX2 must be positive and even",
            });
        }
        let seg_count = seg_count_x2 / 2;
        // searchRange / entrySelector / rangeShift: informational.
        r.skip(6)?;

        // Lay out the four arrays. Format 4 packs:
        //   endCode[segCount] u16
        //   reservedPad       u16 (0)
        //   startCode[segCount] u16
        //   idDelta[segCount] i16
        //   idRangeOffset[segCount] u16
        //   glyphIdArray[...]
        let seg_bytes = seg_count as usize * 2;
        let end_codes_off = r.position();
        let start_codes_off = end_codes_off + seg_bytes + 2; // +2 for reservedPad
        let id_deltas_off = start_codes_off + seg_bytes;
        let id_range_offsets_off = id_deltas_off + seg_bytes;
        let tail_needed = id_range_offsets_off + seg_bytes;
        if tail_needed > data.len() {
            return Err(Error::Truncated {
                offset: end_codes_off,
                context: "format 4 segment arrays do not fit",
            });
        }

        Ok(Self {
            data,
            seg_count,
            end_codes_off,
            start_codes_off,
            id_deltas_off,
            id_range_offsets_off,
        })
    }

    #[inline]
    fn read_u16(&self, off: usize) -> u16 {
        u16::from_be_bytes([self.data[off], self.data[off + 1]])
    }

    #[inline]
    fn read_i16(&self, off: usize) -> i16 {
        i16::from_be_bytes([self.data[off], self.data[off + 1]])
    }

    fn end_code(&self, i: u16) -> u16 {
        self.read_u16(self.end_codes_off + i as usize * 2)
    }

    fn start_code(&self, i: u16) -> u16 {
        self.read_u16(self.start_codes_off + i as usize * 2)
    }

    fn id_delta(&self, i: u16) -> i16 {
        self.read_i16(self.id_deltas_off + i as usize * 2)
    }

    fn id_range_offset(&self, i: u16) -> u16 {
        self.read_u16(self.id_range_offsets_off + i as usize * 2)
    }

    fn lookup(&self, ch: u32) -> Option<u16> {
        // Format 4 cannot describe astral-plane codepoints.
        if ch > 0xFFFF {
            return None;
        }
        let ch = ch as u16;

        // Binary search for the first segment whose endCode >= ch.
        // The final sentinel segment always ends at 0xFFFF, so this
        // terminates without special-casing.
        let mut lo: u16 = 0;
        let mut hi: u16 = self.seg_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.end_code(mid) < ch {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let seg = lo;
        if seg >= self.seg_count {
            return None;
        }

        let start = self.start_code(seg);
        if ch < start {
            return None;
        }

        let id_range_offset = self.id_range_offset(seg);
        let id_delta = self.id_delta(seg);

        if id_range_offset == 0 {
            return Some(apply_delta(ch, id_delta));
        }

        // Spec lookup:
        //   glyphIndexAddress = idRangeOffset[seg]
        //                     + 2 * (ch - startCode[seg])
        //                     + &idRangeOffset[seg]
        // Interpreted as a byte offset from the start of the
        // subtable, that is:
        let offset = self.id_range_offsets_off
            + seg as usize * 2
            + id_range_offset as usize
            + (ch - start) as usize * 2;
        if offset + 2 > self.data.len() {
            return None;
        }
        let raw = self.read_u16(offset);
        if raw == 0 {
            return None;
        }
        Some(apply_delta(raw, id_delta))
    }
}

// --------------------------------------------------------------------------
// Format 12
// --------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Format12<'a> {
    data: &'a [u8],
    groups_off: usize,
    num_groups: u32,
}

impl<'a> Format12<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 12 {
            return Err(Error::Malformed {
                offset: 0,
                context: "format 12 parser invoked on non-format-12 subtable",
            });
        }
        let _reserved = r.read_u16()?;
        let length = r.read_u32()? as usize;
        if length > data.len() {
            return Err(Error::Truncated {
                offset: 4,
                context: "format 12 length exceeds available bytes",
            });
        }
        let _language = r.read_u32()?;
        let num_groups = r.read_u32()?;
        let groups_off = r.position();
        let need = num_groups as usize * 12;
        if groups_off + need > data.len() {
            return Err(Error::Truncated {
                offset: groups_off,
                context: "format 12 group table does not fit",
            });
        }

        Ok(Self {
            data,
            groups_off,
            num_groups,
        })
    }

    fn group(&self, i: u32) -> (u32, u32, u32) {
        let off = self.groups_off + i as usize * 12;
        let start = u32::from_be_bytes([
            self.data[off],
            self.data[off + 1],
            self.data[off + 2],
            self.data[off + 3],
        ]);
        let end = u32::from_be_bytes([
            self.data[off + 4],
            self.data[off + 5],
            self.data[off + 6],
            self.data[off + 7],
        ]);
        let start_glyph = u32::from_be_bytes([
            self.data[off + 8],
            self.data[off + 9],
            self.data[off + 10],
            self.data[off + 11],
        ]);
        (start, end, start_glyph)
    }

    fn lookup(&self, ch: u32) -> Option<u16> {
        if self.num_groups == 0 {
            return None;
        }
        let mut lo: u32 = 0;
        let mut hi: u32 = self.num_groups;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (start, end, _) = self.group(mid);
            if ch < start {
                hi = mid;
            } else if ch > end {
                lo = mid + 1;
            } else {
                let (s, _, start_glyph) = self.group(mid);
                let gid = start_glyph.checked_add(ch - s)?;
                if gid > u16::MAX as u32 {
                    return None;
                }
                return Some(gid as u16);
            }
        }
        None
    }
}

// --------------------------------------------------------------------------
// Fixture helpers (tests only)
// --------------------------------------------------------------------------

#[cfg(test)]
use alloc::vec::Vec;

#[cfg(test)]
pub(crate) fn build_cmap_wrapper(records: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
    // Assemble a cmap table where each record's (platform, encoding)
    // points at a subtable appended after the encoding-record list.
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&(records.len() as u16).to_be_bytes());

    // First pass: reserve record space, remember where each subtable
    // will start.
    let header_end = 4 + records.len() * 8;
    let mut cursor = header_end;
    let mut offsets = Vec::with_capacity(records.len());
    for (_plat, _enc, bytes) in records {
        offsets.push(cursor);
        cursor += bytes.len();
    }

    for ((plat, enc, _), off) in records.iter().zip(offsets.iter()) {
        out.extend_from_slice(&plat.to_be_bytes());
        out.extend_from_slice(&enc.to_be_bytes());
        out.extend_from_slice(&(*off as u32).to_be_bytes());
    }

    for (_, _, bytes) in records {
        out.extend_from_slice(bytes);
    }

    out
}

#[cfg(test)]
pub(crate) fn build_format4(
    segments: &[(u16, u16, i16)], // (startCode, endCode, idDelta)
) -> Vec<u8> {
    // Every segment uses idRangeOffset=0 so the delta path is taken.
    // Terminator segment at 0xFFFF..=0xFFFF is appended automatically.
    let mut segs: Vec<(u16, u16, i16)> = segments.to_vec();
    if segs.last().map_or(true, |s| s.1 != 0xFFFF) {
        segs.push((0xFFFF, 0xFFFF, 1)); // delta 1 maps 0xFFFF to glyph 0 (miss)
    }
    let seg_count = segs.len();
    let seg_bytes = seg_count * 2;

    let mut out = Vec::new();
    out.extend_from_slice(&4u16.to_be_bytes()); // format
                                                // length placeholder, patched later
    let length_idx = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // language
    out.extend_from_slice(&((seg_count * 2) as u16).to_be_bytes()); // segCountX2
    out.extend_from_slice(&0u16.to_be_bytes()); // searchRange
    out.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
    out.extend_from_slice(&0u16.to_be_bytes()); // rangeShift

    for &(_start, end, _delta) in &segs {
        out.extend_from_slice(&end.to_be_bytes());
    }
    out.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
    for &(start, _end, _delta) in &segs {
        out.extend_from_slice(&start.to_be_bytes());
    }
    for &(_start, _end, delta) in &segs {
        out.extend_from_slice(&delta.to_be_bytes());
    }
    for _ in 0..seg_count {
        out.extend_from_slice(&0u16.to_be_bytes()); // idRangeOffset = 0
    }

    // No glyphIdArray needed since every idRangeOffset is zero.
    let length = out.len();
    out[length_idx..length_idx + 2].copy_from_slice(&(length as u16).to_be_bytes());
    let _ = seg_bytes;
    out
}

#[cfg(test)]
pub(crate) fn build_format12(
    groups: &[(u32, u32, u32)], // (startCharCode, endCharCode, startGlyphID)
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&12u16.to_be_bytes()); // format
    out.extend_from_slice(&0u16.to_be_bytes()); // reserved
    let length_idx = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // length placeholder
    out.extend_from_slice(&0u32.to_be_bytes()); // language
    out.extend_from_slice(&(groups.len() as u32).to_be_bytes());

    for &(start, end, start_glyph) in groups {
        out.extend_from_slice(&start.to_be_bytes());
        out.extend_from_slice(&end.to_be_bytes());
        out.extend_from_slice(&start_glyph.to_be_bytes());
    }

    let length = out.len() as u32;
    out[length_idx..length_idx + 4].copy_from_slice(&length.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format4_maps_segments_via_delta() {
        // Segment U+0041..=U+005A ('A'..='Z') maps to glyphs 2..=27
        // (delta = -63).
        let subtable = build_format4(&[(b'A' as u16, b'Z' as u16, -63)]);
        let cmap_bytes = build_cmap_wrapper(&[(3, 1, subtable)]);
        let cmap = Cmap::parse(&cmap_bytes).unwrap();
        assert_eq!(cmap.glyph_id('A'), Some(2));
        assert_eq!(cmap.glyph_id('B'), Some(3));
        assert_eq!(cmap.glyph_id('Z'), Some(27));
        // Space is outside the segment.
        assert_eq!(cmap.glyph_id(' '), None);
    }

    #[test]
    fn format4_handles_multiple_segments() {
        let subtable = build_format4(&[
            (b'0' as u16, b'9' as u16, -47), // '0' -> 1
            (b'A' as u16, b'Z' as u16, -54), // 'A' -> 11
        ]);
        let cmap_bytes = build_cmap_wrapper(&[(3, 1, subtable)]);
        let cmap = Cmap::parse(&cmap_bytes).unwrap();
        assert_eq!(cmap.glyph_id('0'), Some(1));
        assert_eq!(cmap.glyph_id('9'), Some(10));
        assert_eq!(cmap.glyph_id('A'), Some(11));
        assert_eq!(cmap.glyph_id(';'), None); // gap between segments
    }

    #[test]
    fn format12_maps_astral_plane_codepoints() {
        // U+1F600..=U+1F64F emoji range maps to glyphs 1000..=1079.
        let subtable = build_format12(&[(0x1F600, 0x1F64F, 1000)]);
        let cmap_bytes = build_cmap_wrapper(&[(3, 10, subtable)]);
        let cmap = Cmap::parse(&cmap_bytes).unwrap();
        assert_eq!(cmap.glyph_id('\u{1F600}'), Some(1000));
        assert_eq!(cmap.glyph_id('\u{1F64F}'), Some(1079));
        assert_eq!(cmap.glyph_id('\u{1F650}'), None);
    }

    #[test]
    fn format12_preferred_over_format4_when_both_present() {
        // Format 4 maps 'A' to 5. Format 12 maps 'A' to 42. Format 12
        // wins.
        let f4 = build_format4(&[(b'A' as u16, b'A' as u16, 4)]);
        let f12 = build_format12(&[(b'A' as u32, b'A' as u32, 42)]);
        let cmap_bytes = build_cmap_wrapper(&[(3, 1, f4), (3, 10, f12)]);
        let cmap = Cmap::parse(&cmap_bytes).unwrap();
        assert_eq!(cmap.glyph_id('A'), Some(42));
    }

    #[test]
    fn unicode_platform_preferred_over_microsoft_at_same_format() {
        // Same format 4 table under two platforms with different
        // deltas. Unicode (platform 0) wins.
        let f4_micro = build_format4(&[(b'A' as u16, b'A' as u16, 4)]);
        let f4_uni = build_format4(&[(b'A' as u16, b'A' as u16, 9)]);
        let cmap_bytes = build_cmap_wrapper(&[(3, 1, f4_micro), (0, 3, f4_uni)]);
        let cmap = Cmap::parse(&cmap_bytes).unwrap();
        assert_eq!(cmap.glyph_id('A'), Some(b'A' as u16 + 9));
    }

    #[test]
    fn symbol_subtable_is_ignored() {
        // A cmap with only (3, 0) Symbol encoding is currently
        // unsupported.
        let f4 = build_format4(&[(b'A' as u16, b'A' as u16, 4)]);
        let cmap_bytes = build_cmap_wrapper(&[(3, 0, f4)]);
        let err = Cmap::parse(&cmap_bytes).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }));
    }

    #[test]
    fn missing_glyph_zero_translates_to_none() {
        // Segment 0x20..=0x20 with delta -32 maps ' ' to glyph 0.
        let f4 = build_format4(&[(0x20, 0x20, -32)]);
        let cmap_bytes = build_cmap_wrapper(&[(3, 1, f4)]);
        let cmap = Cmap::parse(&cmap_bytes).unwrap();
        assert_eq!(cmap.glyph_id(' '), None);
    }

    #[test]
    fn rejects_cmap_with_unknown_version() {
        let mut bytes =
            build_cmap_wrapper(&[(3, 1, build_format4(&[(b'A' as u16, b'A' as u16, 4)]))]);
        bytes[0..2].copy_from_slice(&1u16.to_be_bytes());
        assert!(matches!(Cmap::parse(&bytes), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_cmap_with_zero_subtables() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        assert!(matches!(Cmap::parse(&bytes), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_cmap_with_no_supported_subtable() {
        // Only Apple platform (1) which we don't recognize.
        let f4 = build_format4(&[(b'A' as u16, b'A' as u16, 4)]);
        let cmap_bytes = build_cmap_wrapper(&[(1, 0, f4)]);
        assert!(matches!(
            Cmap::parse(&cmap_bytes),
            Err(Error::Unsupported { .. })
        ));
    }
}
