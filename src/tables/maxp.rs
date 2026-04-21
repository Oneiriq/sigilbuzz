//! `maxp` — maximum profile.
//!
//! Two distinct layouts exist: version 0.5 (CFF / OpenType outlines)
//! which is six bytes and only carries `numGlyphs`, and version 1.0
//! (TrueType outlines) which adds a dozen more counters. sigilbuzz
//! needs `numGlyphs` for hmtx and loca indexing; anything else gets
//! skipped.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

const MAXP_VERSION_0_5: u32 = 0x0000_5000;
const MAXP_VERSION_1_0: u32 = 0x0001_0000;

/// The parsed `maxp` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Maxp {
    /// Total glyph count in the font.
    pub num_glyphs: u16,
}

impl Maxp {
    /// Parses a `maxp` table, tolerating either version shape.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.read_u32()?;
        if version != MAXP_VERSION_0_5 && version != MAXP_VERSION_1_0 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported maxp version",
            });
        }
        let num_glyphs = r.read_u16()?;
        Ok(Self { num_glyphs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn maxp_05(num_glyphs: u16) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&MAXP_VERSION_0_5.to_be_bytes());
        b.extend_from_slice(&num_glyphs.to_be_bytes());
        b
    }

    fn maxp_10(num_glyphs: u16) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&MAXP_VERSION_1_0.to_be_bytes());
        b.extend_from_slice(&num_glyphs.to_be_bytes());
        // 26 bytes of counters we don't care about.
        b.extend_from_slice(&[0; 26]);
        b
    }

    #[test]
    fn parses_cff_half_version() {
        let b = maxp_05(512);
        let maxp = Maxp::parse(&b).unwrap();
        assert_eq!(maxp.num_glyphs, 512);
    }

    #[test]
    fn parses_truetype_full_version() {
        let b = maxp_10(4095);
        let maxp = Maxp::parse(&b).unwrap();
        assert_eq!(maxp.num_glyphs, 4095);
    }

    #[test]
    fn rejects_unknown_version() {
        let mut b = Vec::new();
        b.extend_from_slice(&0x0002_0000u32.to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes());
        assert!(matches!(Maxp::parse(&b), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_truncated_input() {
        let b = maxp_05(1);
        assert!(matches!(Maxp::parse(&b[..4]), Err(Error::Truncated { .. })));
    }
}
