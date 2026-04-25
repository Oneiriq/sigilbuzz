//! `sbix` — Standard Bitmap Graphics (Apple).
//!
//! Apple's bitmap-emoji table. Simpler than CBDT/CBLC: a flat list of
//! "strikes" (one per ppem), each carrying a per-glyph
//! `glyphDataOffsets[numGlyphs + 1]` array — exactly the same trick
//! `loca` uses. Per-glyph the payload is one of:
//!
//! - `'png '` — PNG bytes
//! - `'jpg '` — JPEG bytes
//! - `'tiff'` — TIFF bytes
//! - `'jp2 '` — JPEG 2000 bytes (rare)
//! - `'dupe'` — pointer to another glyph id (the encoded glyph id is
//!   the entire 2-byte payload)
//!
//! sigilbuzz returns the raw four-byte tag and the payload slice;
//! decoding is a renderer concern.
//!
//! # Format
//!
//! ```text
//!   sbix header
//!     0  u16  version           = 1
//!     2  u16  flags
//!     4  u32  numStrikes
//!     8  u32  strikeOffsets[numStrikes]   relative to start of sbix
//!
//!   Strike header (at strikeOffsets[i])
//!     0  u16  ppem
//!     2  u16  ppi
//!     4  u32  glyphDataOffsets[numGlyphs + 1]   relative to strike start
//!
//!   Glyph data (at strike + glyphDataOffsets[gid])
//!     0  i16  originOffsetX
//!     2  i16  originOffsetY
//!     4  Tag  graphicType    (e.g. b"png ")
//!     8  ...  payload bytes
//! ```
//!
//! `numGlyphs` is taken from `maxp` — sbix doesn't restate it.
//! Empty glyphs (length-zero entries) are valid: `glyphDataOffsets[gid] ==
//! glyphDataOffsets[gid + 1]` means "this strike has no bitmap for
//! this glyph". sigilbuzz surfaces those as `Ok(None)`.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// One sbix strike — a set of per-glyph bitmaps at a given resolution.
/// Borrows into the table; cheap to copy.
#[derive(Debug, Clone, Copy)]
pub struct SbixStrike<'a> {
    /// Pixels-per-em (the design size of this strike).
    pub ppem: u16,
    /// Pixels-per-inch (target rendering DPI).
    pub ppi: u16,
    /// Slice into the strike's `glyphDataOffsets` array. `4 *
    /// (numGlyphs + 1)` bytes long.
    offsets: &'a [u8],
    /// Number of glyphs the offsets array covers (= `maxp.numGlyphs`).
    num_glyphs: u16,
    /// Slice covering the strike's full payload, starting at the
    /// strike header. Per-glyph offsets index into this.
    strike_data: &'a [u8],
}

/// One sbix glyph entry — origin offset, graphic type tag, and the
/// raw payload.
#[derive(Debug, Clone, Copy)]
pub struct SbixGlyph<'a> {
    /// Pixel offset between the glyph's origin and the bitmap's
    /// bottom-left corner (positive = right).
    pub origin_offset_x: i16,
    /// Pixel offset on the y axis (positive = up).
    pub origin_offset_y: i16,
    /// Four-byte tag describing the payload format.
    pub graphic_type: [u8; 4],
    /// Raw payload bytes — a PNG/JPEG/TIFF blob, or for `'dupe'` the
    /// 2-byte big-endian glyph id of the glyph this one aliases.
    pub data: &'a [u8],
}

/// Tag for PNG payloads.
pub const TAG_PNG: [u8; 4] = *b"png ";
/// Tag for JPEG payloads.
pub const TAG_JPG: [u8; 4] = *b"jpg ";
/// Tag for TIFF payloads.
pub const TAG_TIFF: [u8; 4] = *b"tiff";
/// Tag for JPEG-2000 payloads.
pub const TAG_JP2: [u8; 4] = *b"jp2 ";
/// Tag indicating the payload is a 2-byte glyph id whose bitmap to use instead.
pub const TAG_DUPE: [u8; 4] = *b"dupe";

/// Parsed `sbix` table — header plus the strike-offset array.
#[derive(Debug, Clone, Copy)]
pub struct Sbix<'a> {
    data: &'a [u8],
    /// `flags` field from the header. Bit 0: draw outline behind
    /// bitmap. Bit 1: draw outline only. sigilbuzz exposes the raw
    /// value; consumers decide.
    pub flags: u16,
    num_strikes: u32,
    strike_offsets: &'a [u8],
    num_glyphs: u16,
}

impl<'a> Sbix<'a> {
    /// Parses an `sbix` table. `num_glyphs` comes from `maxp` — the
    /// sbix table itself doesn't carry one, since the `glyphDataOffsets`
    /// array length is `maxp.numGlyphs + 1`.
    pub fn parse(data: &'a [u8], num_glyphs: u16) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.read_u16()?;
        if version != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported sbix version",
            });
        }
        let flags = r.read_u16()?;
        let num_strikes = r.read_u32()?;
        let arr_start = r.position();
        let arr_bytes = (num_strikes as usize)
            .checked_mul(4)
            .ok_or(Error::Malformed {
                offset: arr_start,
                context: "sbix strike offset overflow",
            })?;
        let arr_end = arr_start.checked_add(arr_bytes).ok_or(Error::Malformed {
            offset: arr_start,
            context: "sbix strike offset overflow",
        })?;
        if arr_end > data.len() {
            return Err(Error::Truncated {
                offset: arr_end,
                context: "sbix strikeOffsets array",
            });
        }
        Ok(Self {
            data,
            flags,
            num_strikes,
            strike_offsets: &data[arr_start..arr_end],
            num_glyphs,
        })
    }

    /// Number of strikes in the table.
    #[must_use]
    pub fn num_strikes(&self) -> u32 {
        self.num_strikes
    }

    /// Returns strike `i`. Validates the strike header lies inside
    /// the table; the per-glyph offset array is bound-checked here so
    /// later glyph lookups don't need to re-verify.
    pub fn strike(&self, i: u32) -> Result<Option<SbixStrike<'a>>> {
        if i >= self.num_strikes {
            return Ok(None);
        }
        let entry_off = (i as usize) * 4;
        let mut r = Reader::at(self.strike_offsets, entry_off)?;
        let strike_off = r.read_u32()? as usize;
        if strike_off + 4 > self.data.len() {
            return Err(Error::Truncated {
                offset: strike_off,
                context: "sbix strike header",
            });
        }
        let strike_data = &self.data[strike_off..];
        let mut sr = Reader::new(strike_data);
        let ppem = sr.read_u16()?;
        let ppi = sr.read_u16()?;
        let arr_bytes = (self.num_glyphs as usize + 1)
            .checked_mul(4)
            .ok_or(Error::Malformed {
                offset: strike_off,
                context: "sbix glyphDataOffsets overflow",
            })?;
        let arr_start = sr.position();
        let arr_end = arr_start.checked_add(arr_bytes).ok_or(Error::Malformed {
            offset: strike_off,
            context: "sbix glyphDataOffsets overflow",
        })?;
        if arr_end > strike_data.len() {
            return Err(Error::Truncated {
                offset: strike_off + arr_end,
                context: "sbix glyphDataOffsets array",
            });
        }
        Ok(Some(SbixStrike {
            ppem,
            ppi,
            offsets: &strike_data[arr_start..arr_end],
            num_glyphs: self.num_glyphs,
            strike_data,
        }))
    }

    /// Iterates every strike in directory order, skipping any that
    /// fail to parse (so a malformed strike doesn't block reads of
    /// well-formed siblings).
    pub fn strikes(&self) -> impl Iterator<Item = SbixStrike<'a>> + '_ {
        (0..self.num_strikes).filter_map(|i| self.strike(i).ok().flatten())
    }

    /// Picks the strike whose `ppem` is closest to `target_ppem`.
    /// Ties prefer the larger size. Returns `None` if there are no
    /// strikes.
    #[must_use]
    pub fn best_strike(&self, target_ppem: u16) -> Option<SbixStrike<'a>> {
        let target = i32::from(target_ppem);
        let mut best: Option<(i32, u16, SbixStrike<'a>)> = None;
        for s in self.strikes() {
            let diff = (i32::from(s.ppem) - target).abs();
            match best {
                None => best = Some((diff, s.ppem, s)),
                Some((bdiff, bppem, _)) => {
                    let take = diff < bdiff || (diff == bdiff && s.ppem > bppem);
                    if take {
                        best = Some((diff, s.ppem, s));
                    }
                }
            }
        }
        best.map(|(_, _, s)| s)
    }
}

impl<'a> SbixStrike<'a> {
    /// Pixels-per-em.
    #[must_use]
    pub fn ppem(&self) -> u16 {
        self.ppem
    }

    /// Pixels-per-inch.
    #[must_use]
    pub fn ppi(&self) -> u16 {
        self.ppi
    }

    /// Looks up `glyph_id` in this strike. Returns `Ok(None)` for an
    /// out-of-range gid, or for an empty entry (length-zero record —
    /// "this strike has no bitmap for this glyph").
    pub fn glyph(&self, glyph_id: u16) -> Result<Option<SbixGlyph<'a>>> {
        if glyph_id >= self.num_glyphs {
            return Ok(None);
        }
        let i = glyph_id as usize;
        let off0 = read_u32(self.offsets, i * 4)?;
        let off1 = read_u32(self.offsets, (i + 1) * 4)?;
        if off1 < off0 {
            return Err(Error::Malformed {
                offset: i * 4,
                context: "sbix glyphDataOffsets non-monotonic",
            });
        }
        if off0 == off1 {
            return Ok(None);
        }
        let start = off0 as usize;
        let end = off1 as usize;
        if end > self.strike_data.len() {
            return Err(Error::Truncated {
                offset: end,
                context: "sbix glyph payload",
            });
        }
        if end - start < 8 {
            return Err(Error::Malformed {
                offset: start,
                context: "sbix glyph entry shorter than 8-byte header",
            });
        }
        let entry = &self.strike_data[start..end];
        let mut r = Reader::new(entry);
        let origin_offset_x = r.read_i16()?;
        let origin_offset_y = r.read_i16()?;
        let graphic_type = r.read_tag()?;
        let payload_start = r.position();
        Ok(Some(SbixGlyph {
            origin_offset_x,
            origin_offset_y,
            graphic_type,
            data: &entry[payload_start..],
        }))
    }
}

fn read_u32(slice: &[u8], off: usize) -> Result<u32> {
    if off + 4 > slice.len() {
        return Err(Error::Truncated {
            offset: off + 4,
            context: "sbix u32 read",
        });
    }
    Ok(u32::from_be_bytes([
        slice[off],
        slice[off + 1],
        slice[off + 2],
        slice[off + 3],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Builds an sbix table with one strike covering `num_glyphs`
    /// glyphs, each carrying a small PNG-tagged payload.
    fn build(num_glyphs: u16, ppem: u16, payloads: &[(i16, i16, [u8; 4], &[u8])]) -> Vec<u8> {
        assert_eq!(payloads.len(), num_glyphs as usize);
        // Header: 8 bytes + numStrikes * 4 = 12 for one strike.
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // version
        out.extend_from_slice(&0u16.to_be_bytes()); // flags
        out.extend_from_slice(&1u32.to_be_bytes()); // numStrikes
        let strike_off_pos = out.len();
        out.extend_from_slice(&0u32.to_be_bytes()); // placeholder strikeOffsets[0]

        let strike_start = out.len() as u32;
        // Patch in strike offset.
        out[strike_off_pos..strike_off_pos + 4].copy_from_slice(&strike_start.to_be_bytes());

        // Strike header.
        out.extend_from_slice(&ppem.to_be_bytes());
        out.extend_from_slice(&72u16.to_be_bytes()); // ppi

        // Reserve glyphDataOffsets.
        let offsets_pos = out.len();
        let arr_bytes = (num_glyphs as usize + 1) * 4;
        out.resize(offsets_pos + arr_bytes, 0);

        // Now append per-glyph entries; record their offsets relative
        // to strike_start.
        let mut cur_off = (out.len() as u32) - strike_start;
        let mut offs = Vec::with_capacity(num_glyphs as usize + 1);
        for (ox, oy, tag, payload) in payloads {
            offs.push(cur_off);
            out.extend_from_slice(&ox.to_be_bytes());
            out.extend_from_slice(&oy.to_be_bytes());
            out.extend_from_slice(tag);
            out.extend_from_slice(payload);
            cur_off = (out.len() as u32) - strike_start;
        }
        offs.push(cur_off);

        for (i, off) in offs.iter().enumerate() {
            let dst = offsets_pos + i * 4;
            out[dst..dst + 4].copy_from_slice(&off.to_be_bytes());
        }
        out
    }

    #[test]
    fn parses_header_and_strike() {
        let blob = build(1, 32, &[(0, 0, TAG_PNG, &[0x89, b'P', b'N', b'G'])]);
        let sbix = Sbix::parse(&blob, 1).unwrap();
        assert_eq!(sbix.num_strikes(), 1);
        let strike = sbix.strike(0).unwrap().unwrap();
        assert_eq!(strike.ppem(), 32);
        assert_eq!(strike.ppi(), 72);
    }

    #[test]
    fn glyph_returns_payload_and_origin() {
        let png = [0x89, b'P', b'N', b'G', 0xde, 0xad];
        let blob = build(2, 32, &[(1, 2, TAG_PNG, &png), (0, 0, TAG_PNG, &[])]);
        let sbix = Sbix::parse(&blob, 2).unwrap();
        let strike = sbix.strike(0).unwrap().unwrap();
        let g0 = strike.glyph(0).unwrap().unwrap();
        assert_eq!(g0.origin_offset_x, 1);
        assert_eq!(g0.origin_offset_y, 2);
        assert_eq!(g0.graphic_type, TAG_PNG);
        assert_eq!(g0.data, &png);
        // gid 1 has a 0-byte payload but nonzero offsets.
        let g1 = strike.glyph(1).unwrap().unwrap();
        assert_eq!(g1.data.len(), 0);
    }

    #[test]
    fn empty_offset_pair_means_no_bitmap() {
        // Manually build a strike whose offsets[0] == offsets[1].
        let mut blob = Vec::new();
        blob.extend_from_slice(&1u16.to_be_bytes()); // version
        blob.extend_from_slice(&0u16.to_be_bytes()); // flags
        blob.extend_from_slice(&1u32.to_be_bytes()); // numStrikes
        blob.extend_from_slice(&12u32.to_be_bytes()); // strikeOffsets[0] = 12
                                                      // Strike header at 12: ppem, ppi, then offsets[0..=1].
        blob.extend_from_slice(&32u16.to_be_bytes());
        blob.extend_from_slice(&72u16.to_be_bytes());
        blob.extend_from_slice(&8u32.to_be_bytes()); // offsets[0] = 8 (right after the array itself)
        blob.extend_from_slice(&8u32.to_be_bytes()); // offsets[1] = 8 (no payload)
        let sbix = Sbix::parse(&blob, 1).unwrap();
        let strike = sbix.strike(0).unwrap().unwrap();
        assert!(strike.glyph(0).unwrap().is_none());
    }

    #[test]
    fn jpg_and_tiff_payloads_pass_through() {
        let payload = [0xff, 0xd8, 0xff, 0xe0, 0x00];
        let blob = build(1, 64, &[(0, 0, TAG_JPG, &payload)]);
        let sbix = Sbix::parse(&blob, 1).unwrap();
        let g = sbix.strike(0).unwrap().unwrap().glyph(0).unwrap().unwrap();
        assert_eq!(g.graphic_type, TAG_JPG);
        assert_eq!(g.data, &payload);

        let blob = build(1, 64, &[(0, 0, TAG_TIFF, &payload)]);
        let sbix = Sbix::parse(&blob, 1).unwrap();
        let g = sbix.strike(0).unwrap().unwrap().glyph(0).unwrap().unwrap();
        assert_eq!(g.graphic_type, TAG_TIFF);
    }

    #[test]
    fn dupe_payload_is_returned_raw() {
        // 'dupe' carries a 2-byte gid alias; sigilbuzz returns the bytes.
        let blob = build(1, 32, &[(0, 0, TAG_DUPE, &[0x00, 0x07])]);
        let sbix = Sbix::parse(&blob, 1).unwrap();
        let g = sbix.strike(0).unwrap().unwrap().glyph(0).unwrap().unwrap();
        assert_eq!(g.graphic_type, TAG_DUPE);
        assert_eq!(g.data, &[0x00, 0x07]);
    }

    #[test]
    fn best_strike_picks_closest() {
        // Two strikes at 16 and 64 ppem.
        let mut blob = Vec::new();
        blob.extend_from_slice(&1u16.to_be_bytes());
        blob.extend_from_slice(&0u16.to_be_bytes());
        blob.extend_from_slice(&2u32.to_be_bytes());
        // Reserve two strike offsets.
        let off0_pos = blob.len();
        blob.extend_from_slice(&0u32.to_be_bytes());
        let off1_pos = blob.len();
        blob.extend_from_slice(&0u32.to_be_bytes());

        // Strike 0 at 16 ppem.
        let s0 = blob.len() as u32;
        blob.extend_from_slice(&16u16.to_be_bytes());
        blob.extend_from_slice(&72u16.to_be_bytes());
        blob.extend_from_slice(&8u32.to_be_bytes());
        blob.extend_from_slice(&8u32.to_be_bytes()); // empty
        let s1 = blob.len() as u32;
        blob.extend_from_slice(&64u16.to_be_bytes());
        blob.extend_from_slice(&72u16.to_be_bytes());
        blob.extend_from_slice(&8u32.to_be_bytes());
        blob.extend_from_slice(&8u32.to_be_bytes());

        blob[off0_pos..off0_pos + 4].copy_from_slice(&s0.to_be_bytes());
        blob[off1_pos..off1_pos + 4].copy_from_slice(&s1.to_be_bytes());

        let sbix = Sbix::parse(&blob, 1).unwrap();
        assert_eq!(sbix.best_strike(40).unwrap().ppem(), 64);
        assert_eq!(sbix.best_strike(20).unwrap().ppem(), 16);
        assert_eq!(sbix.best_strike(0).unwrap().ppem(), 16);
    }

    #[test]
    fn rejects_bad_version() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&7u16.to_be_bytes());
        blob.extend_from_slice(&0u16.to_be_bytes());
        blob.extend_from_slice(&0u32.to_be_bytes());
        let err = Sbix::parse(&blob, 0).unwrap_err();
        assert!(matches!(err, Error::Malformed { .. }));
    }
}
