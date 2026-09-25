//! OpenType Anchor table: `(x, y)` attachment point.
//!
//! Anchors are the glue for three GPOS lookup types: mark-to-base
//! (type 4), mark-to-ligature (type 5), and mark-to-mark (type 6).
//! Each lookup holds a MarkArray and a BaseArray (or the
//! ligature/mark equivalents); every element is an Anchor giving
//! the `(x, y)` point the mark should snap to.
//!
//! # Formats
//!
//! All three formats collapse to the same `(x, y)` pair from
//! sigilbuzz's point of view:
//!
//! ```text
//!   format 1:  u16 format=1, i16 x, i16 y
//!   format 2:  format 1 + u16 anchorPoint              (hinting, ignored)
//!   format 3:  format 1 + u16 xDeviceOffset, u16 yDev  (hinting, ignored)
//! ```
//!
//! The hint-only fields are consumed to advance the cursor past
//! them when parsing a larger structure that contains anchors
//! inline, but they do not influence placement. Device tables are
//! a later milestone (same situation as in ValueRecord: they hold
//! per-ppem deltas that sigilbuzz does not yet apply).

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// A parsed anchor: a design-unit `(x, y)` attachment point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Anchor {
    /// X coordinate in font design units.
    pub x: i16,
    /// Y coordinate in font design units.
    pub y: i16,
}

impl Anchor {
    /// Parses an anchor from `data`. Accepts all three spec formats;
    /// the extra fields past `(x, y)` are read-and-discarded so
    /// callers that parse anchors inline land at the right offset
    /// afterwards.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        let x = r.read_i16()?;
        let y = r.read_i16()?;
        match format {
            1 => Ok(Self { x, y }),
            2 => {
                // u16 anchorPoint: contour index for hinting.
                let _ = r.read_u16()?;
                Ok(Self { x, y })
            }
            3 => {
                // Offset16 xDeviceOffset, Offset16 yDeviceOffset.
                let _ = r.read_u16()?;
                let _ = r.read_u16()?;
                Ok(Self { x, y })
            }
            _ => Err(Error::Malformed {
                offset: 0,
                context: "unsupported anchor format",
            }),
        }
    }

    /// Parses the anchor at `offset` inside `data`. Convenience for
    /// callers that hold offsets relative to some enclosing table.
    pub fn parse_at(data: &[u8], offset: usize) -> Result<Self> {
        let slice = data.get(offset..).ok_or(Error::Malformed {
            offset,
            context: "anchor offset past end",
        })?;
        Self::parse(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn build_format(format: u16, x: i16, y: i16, trailer: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&format.to_be_bytes());
        out.extend_from_slice(&x.to_be_bytes());
        out.extend_from_slice(&y.to_be_bytes());
        for t in trailer {
            out.extend_from_slice(&t.to_be_bytes());
        }
        out
    }

    #[test]
    fn format1_reads_xy() {
        let bytes = build_format(1, 200, -50, &[]);
        let a = Anchor::parse(&bytes).unwrap();
        assert_eq!(a.x, 200);
        assert_eq!(a.y, -50);
    }

    #[test]
    fn format2_ignores_contour_point() {
        // Contour point 42 must not change the anchor coordinates.
        let bytes = build_format(2, 10, 20, &[42]);
        let a = Anchor::parse(&bytes).unwrap();
        assert_eq!(a, Anchor { x: 10, y: 20 });
    }

    #[test]
    fn format3_ignores_device_offsets() {
        let bytes = build_format(3, -7, 8, &[0xDEAD, 0xBEEF]);
        let a = Anchor::parse(&bytes).unwrap();
        assert_eq!(a, Anchor { x: -7, y: 8 });
    }

    #[test]
    fn parse_at_slices_to_offset() {
        let mut bytes = alloc::vec![0u8; 8];
        bytes.extend_from_slice(&build_format(1, 100, 200, &[]));
        let a = Anchor::parse_at(&bytes, 8).unwrap();
        assert_eq!(a, Anchor { x: 100, y: 200 });
    }

    #[test]
    fn parse_at_rejects_offset_past_end() {
        let bytes = build_format(1, 1, 2, &[]);
        assert!(Anchor::parse_at(&bytes, 99).is_err());
    }

    #[test]
    fn rejects_unknown_format() {
        let bytes = build_format(9, 0, 0, &[]);
        assert!(matches!(
            Anchor::parse(&bytes),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(Anchor::parse(&[0u8; 3]).is_err());
    }
}
