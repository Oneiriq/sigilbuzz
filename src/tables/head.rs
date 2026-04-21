//! `head` — font header.
//!
//! Only the fields sigilbuzz actually consumes are parsed today:
//! `unitsPerEm` (the grid that every metric is expressed in) and
//! `indexToLocFormat` (short vs long `loca` offsets). The rest of the
//! 54-byte table is skipped over without interpretation, but the
//! parser still validates the magic number and table length so a
//! malformed font fails at the boundary rather than mid-shape.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// Offset form used by the `loca` table. `Short` stores u16 offsets
/// divided by two; `Long` stores u32 offsets directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexToLocFormat {
    /// u16 offsets, halved. Multiply by 2 to get the true byte offset.
    Short,
    /// u32 offsets, stored directly.
    Long,
}

/// The parsed `head` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Head {
    /// Font design grid size (16..=16384).
    pub units_per_em: u16,
    /// Offset format used by `loca`.
    pub index_to_loc_format: IndexToLocFormat,
}

/// Magic number the spec requires at offset 12.
const HEAD_MAGIC: u32 = 0x5F0F_3CF5;

impl Head {
    /// Parses a `head` table from its raw bytes.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);

        let major = r.read_u16()?;
        let minor = r.read_u16()?;
        if major != 1 || minor != 0 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported head version",
            });
        }

        // fontRevision (Fixed, i32) — not consumed yet, skip.
        r.skip(4)?;
        // checksumAdjustment — skip.
        r.skip(4)?;

        // magicNumber — must match 0x5F0F3CF5 or the table is bogus.
        let magic_offset = r.position();
        let magic = r.read_u32()?;
        if magic != HEAD_MAGIC {
            return Err(Error::Malformed {
                offset: magic_offset,
                context: "head magicNumber mismatch",
            });
        }

        // flags — skip (16 bits of feature hints we do not consume).
        r.skip(2)?;

        let upem = r.read_u16()?;
        if !(16..=16384).contains(&upem) {
            return Err(Error::Malformed {
                offset: r.position() - 2,
                context: "unitsPerEm out of spec range",
            });
        }

        // created + modified (LONGDATETIME = i64 each) — skip.
        r.skip(16)?;
        // xMin/yMin/xMax/yMax (i16 × 4) — skip.
        r.skip(8)?;
        // macStyle + lowestRecPPEM (u16 × 2) — skip.
        r.skip(4)?;
        // fontDirectionHint — skip.
        r.skip(2)?;

        let itl_offset = r.position();
        let itl_raw = r.read_i16()?;
        let index_to_loc_format = match itl_raw {
            0 => IndexToLocFormat::Short,
            1 => IndexToLocFormat::Long,
            _ => {
                return Err(Error::Malformed {
                    offset: itl_offset,
                    context: "indexToLocFormat must be 0 or 1",
                });
            }
        };

        Ok(Self {
            units_per_em: upem,
            index_to_loc_format,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn valid_head(upem: u16, itl: i16) -> Vec<u8> {
        let mut b = Vec::with_capacity(54);
        b.extend_from_slice(&1u16.to_be_bytes()); // majorVersion
        b.extend_from_slice(&0u16.to_be_bytes()); // minorVersion
        b.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // fontRevision
        b.extend_from_slice(&0u32.to_be_bytes()); // checksumAdjustment
        b.extend_from_slice(&HEAD_MAGIC.to_be_bytes()); // magicNumber
        b.extend_from_slice(&0u16.to_be_bytes()); // flags
        b.extend_from_slice(&upem.to_be_bytes()); // unitsPerEm
        b.extend_from_slice(&[0; 8]); // created
        b.extend_from_slice(&[0; 8]); // modified
        b.extend_from_slice(&[0; 8]); // xMin/yMin/xMax/yMax
        b.extend_from_slice(&0u16.to_be_bytes()); // macStyle
        b.extend_from_slice(&0u16.to_be_bytes()); // lowestRecPPEM
        b.extend_from_slice(&0i16.to_be_bytes()); // fontDirectionHint
        b.extend_from_slice(&itl.to_be_bytes()); // indexToLocFormat
        b.extend_from_slice(&0i16.to_be_bytes()); // glyphDataFormat
        b
    }

    #[test]
    fn parses_a_valid_short_loca_head() {
        let b = valid_head(2048, 0);
        let head = Head::parse(&b).unwrap();
        assert_eq!(head.units_per_em, 2048);
        assert_eq!(head.index_to_loc_format, IndexToLocFormat::Short);
    }

    #[test]
    fn parses_a_valid_long_loca_head() {
        let b = valid_head(1000, 1);
        let head = Head::parse(&b).unwrap();
        assert_eq!(head.index_to_loc_format, IndexToLocFormat::Long);
    }

    #[test]
    fn rejects_bad_magic_number() {
        let mut b = valid_head(2048, 0);
        // magicNumber lives at bytes 12..16
        b[12..16].copy_from_slice(&0xDEAD_BEEFu32.to_be_bytes());
        let err = Head::parse(&b).unwrap_err();
        assert!(matches!(err, Error::Malformed { offset: 12, .. }));
    }

    #[test]
    fn rejects_out_of_range_units_per_em() {
        let b = valid_head(8, 0);
        assert!(matches!(Head::parse(&b), Err(Error::Malformed { .. })));
        let b = valid_head(20000, 0);
        assert!(matches!(Head::parse(&b), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_unknown_index_to_loc_format() {
        let b = valid_head(2048, 2);
        assert!(matches!(Head::parse(&b), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_truncated_table() {
        let b = valid_head(2048, 0);
        assert!(matches!(
            Head::parse(&b[..10]),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn rejects_wrong_version() {
        let mut b = valid_head(2048, 0);
        b[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert!(matches!(Head::parse(&b), Err(Error::Malformed { .. })));
    }
}
