//! `loca`: glyph index to location.
//!
//! Maps glyph ids to byte offsets inside the sibling `glyf` table.
//! Ships in two shapes, selected by `head.indexToLocFormat`:
//!
//! - **Short**: `u16[numGlyphs + 1]`, each entry stores `offset / 2`.
//!   Used when the glyf table is at most 128 KB (the u16 * 2
//!   reach).
//! - **Long**: `u32[numGlyphs + 1]`, raw byte offsets.
//!
//! The array carries `numGlyphs + 1` entries. Glyph `i` lives in
//! `glyf[loca[i] .. loca[i+1]]`. A zero-length range (where the two
//! offsets are equal) means the glyph has no outline. Whitespace
//! glyphs commonly encode this way.

use crate::error::{Error, Result};
use crate::tables::head::IndexToLocFormat;

/// Parsed `loca` table.
#[derive(Debug, Clone, Copy)]
pub struct Loca<'a> {
    data: &'a [u8],
    format: IndexToLocFormat,
    num_glyphs: u16,
}

impl<'a> Loca<'a> {
    /// Parses a `loca` table. Callers pass the format from `head`
    /// and the glyph count from `maxp`; `loca` itself carries no
    /// self-describing header.
    pub fn parse(data: &'a [u8], format: IndexToLocFormat, num_glyphs: u16) -> Result<Self> {
        let entry_size = match format {
            IndexToLocFormat::Short => 2,
            IndexToLocFormat::Long => 4,
        };
        let need = (num_glyphs as usize + 1) * entry_size;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: data.len(),
                context: "loca shorter than numGlyphs + 1 entries",
            });
        }
        Ok(Self {
            data,
            format,
            num_glyphs,
        })
    }

    /// Number of glyphs this table indexes.
    #[must_use]
    pub const fn num_glyphs(&self) -> u16 {
        self.num_glyphs
    }

    /// Returns the `(start, end)` byte range inside `glyf` for
    /// `glyph_id`. Returns `None` when the id is out of range.
    ///
    /// An equal start/end means the glyph has no outline (e.g. a
    /// space glyph). The caller treats that as "no bounding box"
    /// (it is not an error).
    #[must_use]
    pub fn range(&self, glyph_id: u16) -> Option<(u32, u32)> {
        if glyph_id >= self.num_glyphs {
            return None;
        }
        let i = glyph_id as usize;
        Some(match self.format {
            IndexToLocFormat::Short => {
                let a = u16::from_be_bytes([self.data[i * 2], self.data[i * 2 + 1]]);
                let b = u16::from_be_bytes([self.data[(i + 1) * 2], self.data[(i + 1) * 2 + 1]]);
                (u32::from(a) * 2, u32::from(b) * 2)
            }
            IndexToLocFormat::Long => {
                let a = read_u32(self.data, i * 4);
                let b = read_u32(self.data, (i + 1) * 4);
                (a, b)
            }
        })
    }
}

fn read_u32(data: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn build_short(offsets: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        for o in offsets {
            out.extend_from_slice(&o.to_be_bytes());
        }
        out
    }

    fn build_long(offsets: &[u32]) -> Vec<u8> {
        let mut out = Vec::new();
        for o in offsets {
            out.extend_from_slice(&o.to_be_bytes());
        }
        out
    }

    #[test]
    fn short_format_doubles_stored_offsets() {
        // numGlyphs = 3 -> 4 offsets. Halved: 0, 50, 100, 200 -> actual
        // bytes 0, 100, 200, 400.
        let bytes = build_short(&[0, 50, 100, 200]);
        let loca = Loca::parse(&bytes, IndexToLocFormat::Short, 3).unwrap();
        assert_eq!(loca.num_glyphs(), 3);
        assert_eq!(loca.range(0), Some((0, 100)));
        assert_eq!(loca.range(1), Some((100, 200)));
        assert_eq!(loca.range(2), Some((200, 400)));
    }

    #[test]
    fn long_format_stores_raw_offsets() {
        let bytes = build_long(&[0, 256, 512, 1024]);
        let loca = Loca::parse(&bytes, IndexToLocFormat::Long, 3).unwrap();
        assert_eq!(loca.range(0), Some((0, 256)));
        assert_eq!(loca.range(1), Some((256, 512)));
        assert_eq!(loca.range(2), Some((512, 1024)));
    }

    #[test]
    fn equal_offsets_signal_an_empty_glyph() {
        // Glyph 1 has zero-length outline (e.g. a space).
        let bytes = build_short(&[0, 40, 40, 80]);
        let loca = Loca::parse(&bytes, IndexToLocFormat::Short, 3).unwrap();
        let (start, end) = loca.range(1).unwrap();
        assert_eq!(start, end);
    }

    #[test]
    fn out_of_range_glyph_returns_none() {
        let bytes = build_short(&[0, 10]);
        let loca = Loca::parse(&bytes, IndexToLocFormat::Short, 1).unwrap();
        assert!(loca.range(1).is_none());
    }

    #[test]
    fn rejects_truncated_table() {
        // Short format, numGlyphs = 3 needs 4 offsets * 2 bytes = 8,
        // but only 6 provided.
        let bytes = build_short(&[0, 10, 20]);
        assert!(matches!(
            Loca::parse(&bytes, IndexToLocFormat::Short, 3),
            Err(Error::Truncated { .. })
        ));
    }
}
