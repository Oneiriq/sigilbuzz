//! TrueType Collection (`ttcf`) header parsing.
//!
//! A collection wraps several fonts in one file so they can share
//! identical tables (CJK families and the classic Windows
//! `Cambria`/`MS Gothic` files ship this way). The layout is a small
//! header followed by ordinary SFNT table directories:
//!
//! ```text
//!   offset  type   field
//!     0     u32    ttcTag        'ttcf'
//!     4     u32    version       0x00010000 or 0x00020000
//!     8     u32    numFonts
//!    12    xN u32  tableDirectoryOffsets   absolute, from file start
//!   (v2 appends dsigTag / dsigLength / dsigOffset, ignored)
//! ```
//!
//! Member table directories record table offsets **absolute from the
//! start of the collection file**, so once [`member_offset`] locates a
//! directory, the regular [`crate::Face`] machinery works unchanged
//! against the full byte buffer.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// `'ttcf'`: the collection magic at byte 0.
pub(crate) const TTCF_MAGIC: u32 = 0x7474_6366;

/// Returns the number of fonts in a TrueType Collection, or `None`
/// when `data` is not a collection (plain TTF/OTF, or too short to
/// tell).
///
/// Use this to enumerate valid `index` arguments for
/// [`crate::Face::parse`] / [`crate::OwnedFace::parse`] before
/// picking a member.
#[must_use]
pub fn fonts_in_collection(data: &[u8]) -> Option<u32> {
    let mut r = Reader::new(data);
    if r.read_u32().ok()? != TTCF_MAGIC {
        return None;
    }
    let _version = r.read_u32().ok()?;
    r.read_u32().ok()
}

/// Byte offset of member `index`'s SFNT table directory.
///
/// # Errors
///
/// `Malformed` when the header is not a valid `ttcf` header, when
/// `index` is at or past `numFonts`, or when the recorded offset
/// points outside the file. `Truncated` when the header itself runs
/// out of bytes.
pub(crate) fn member_offset(data: &[u8], index: u32) -> Result<usize> {
    let mut r = Reader::new(data);
    if r.read_u32()? != TTCF_MAGIC {
        return Err(Error::Malformed {
            offset: 0,
            context: "not a TrueType Collection",
        });
    }
    let version = r.read_u32()?;
    if !matches!(version, 0x0001_0000 | 0x0002_0000) {
        return Err(Error::Malformed {
            offset: 4,
            context: "unrecognised ttcf version",
        });
    }
    let num_fonts = r.read_u32()?;
    if index >= num_fonts {
        return Err(Error::Malformed {
            offset: 8,
            context: "font index out of range for this collection",
        });
    }
    // `4 * index` overflows `usize` on 32-bit targets for large
    // indices. Such an index cannot fit in the data either.
    let entry_offset = (index as usize).checked_mul(4).ok_or(Error::Truncated {
        offset: 12,
        context: "ttcf offset table",
    })?;
    r.skip(entry_offset)?;
    let dir_offset = r.read_u32()? as usize;
    if dir_offset >= data.len() {
        return Err(Error::Malformed {
            offset: 12usize.saturating_add(entry_offset),
            context: "member table-directory offset past end of file",
        });
    }
    Ok(dir_offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn header(version: u32, offsets: &[u32]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&TTCF_MAGIC.to_be_bytes());
        d.extend_from_slice(&version.to_be_bytes());
        d.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
        for &o in offsets {
            d.extend_from_slice(&o.to_be_bytes());
        }
        // Padding so member offsets stay in-bounds for the tests.
        d.resize(64, 0);
        d
    }

    #[test]
    fn counts_fonts_and_resolves_offsets() {
        let d = header(0x0001_0000, &[20, 40]);
        assert_eq!(fonts_in_collection(&d), Some(2));
        assert_eq!(member_offset(&d, 0).unwrap(), 20);
        assert_eq!(member_offset(&d, 1).unwrap(), 40);
    }

    #[test]
    fn v2_header_is_accepted() {
        let d = header(0x0002_0000, &[20]);
        assert_eq!(member_offset(&d, 0).unwrap(), 20);
    }

    #[test]
    fn non_collection_returns_none_or_malformed() {
        let plain = 0x0001_0000u32.to_be_bytes();
        assert_eq!(fonts_in_collection(&plain), None);
        assert!(matches!(
            member_offset(&plain, 0),
            Err(Error::Malformed { offset: 0, .. })
        ));
    }

    #[test]
    fn out_of_range_index_is_rejected() {
        let d = header(0x0001_0000, &[20]);
        assert!(matches!(
            member_offset(&d, 1),
            Err(Error::Malformed { offset: 8, .. })
        ));
    }

    #[test]
    fn offset_past_eof_is_rejected() {
        let d = header(0x0001_0000, &[9999]);
        assert!(matches!(member_offset(&d, 0), Err(Error::Malformed { .. })));
    }

    #[test]
    fn truncated_header_errors() {
        let d = TTCF_MAGIC.to_be_bytes();
        assert!(member_offset(&d, 0).is_err());
    }
}
