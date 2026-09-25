//! `VORG`: Vertical Origin table.
//!
//! Optional table; records the y-coordinate of the vertical origin for
//! specific glyphs, overriding the default origin the renderer would
//! pick (typically `os2.sTypoAscender` or a vmtx-derived value). Most
//! fonts that ship vertical metrics omit VORG because the default rule
//! is good enough; CFF CJK fonts and some ornate display fonts use it.
//!
//! Layout:
//!
//! ```text
//!   u16  majorVersion        (= 1)
//!   u16  minorVersion        (= 0)
//!   i16  defaultVertOriginY
//!   u16  numVertOriginYMetrics
//!   VertOriginYMetric[numVertOriginYMetrics] {
//!       u16 glyphIndex
//!       i16 vertOriginY
//!   }
//! ```
//!
//! The metric array is sorted by `glyphIndex`, so lookup is a binary
//! search; fallback is `defaultVertOriginY`.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// Parsed `VORG`.
#[derive(Debug, Clone, Copy)]
pub struct Vorg<'a> {
    /// Fallback origin Y when a glyph has no per-glyph record.
    default_vert_origin_y: i16,
    /// Sorted array of `(glyph_index u16, vert_origin_y i16)` pairs.
    metrics: &'a [u8],
    /// Number of metric entries.
    count: u16,
}

impl<'a> Vorg<'a> {
    /// Parses a `VORG` table.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported VORG major version",
            });
        }
        let default_vert_origin_y = r.read_i16()?;
        let count = r.read_u16()?;
        let needed = count as usize * 4;
        let start = r.position();
        let end = start.checked_add(needed).ok_or(Error::Malformed {
            offset: start,
            context: "VORG metrics count overflow",
        })?;
        if end > data.len() {
            return Err(Error::Truncated {
                offset: data.len(),
                context: "VORG metrics truncated",
            });
        }
        let metrics = &data[start..end];

        Ok(Self {
            default_vert_origin_y,
            metrics,
            count,
        })
    }

    /// Returns the default vertical origin Y, applied when a glyph
    /// has no per-glyph entry.
    #[must_use]
    pub const fn default_vert_origin_y(&self) -> i16 {
        self.default_vert_origin_y
    }

    /// Looks up the vertical origin Y for `glyph`. Falls back to the
    /// table's default when the glyph has no record.
    #[must_use]
    pub fn vert_origin_y(&self, glyph: u16) -> i16 {
        // The array is sorted by glyph index: binary search.
        let mut lo = 0usize;
        let mut hi = self.count as usize;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let off = mid * 4;
            let gid = u16::from_be_bytes([self.metrics[off], self.metrics[off + 1]]);
            match gid.cmp(&glyph) {
                core::cmp::Ordering::Equal => {
                    return i16::from_be_bytes([self.metrics[off + 2], self.metrics[off + 3]]);
                }
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
            }
        }
        self.default_vert_origin_y
    }

    /// Number of per-glyph overrides in the table (not counting the
    /// default).
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.count
    }

    /// True when the table carries no per-glyph overrides.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn build_vorg(default_y: i16, entries: &[(u16, i16)]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&1u16.to_be_bytes()); // major
        b.extend_from_slice(&0u16.to_be_bytes()); // minor
        b.extend_from_slice(&default_y.to_be_bytes());
        b.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        for (gid, y) in entries {
            b.extend_from_slice(&gid.to_be_bytes());
            b.extend_from_slice(&y.to_be_bytes());
        }
        b
    }

    #[test]
    fn returns_default_when_no_entries() {
        let b = build_vorg(880, &[]);
        let v = Vorg::parse(&b).unwrap();
        assert_eq!(v.default_vert_origin_y(), 880);
        assert_eq!(v.vert_origin_y(42), 880);
        assert!(v.is_empty());
    }

    #[test]
    fn looks_up_specific_overrides() {
        let b = build_vorg(880, &[(1, 900), (5, 950), (10, 1000)]);
        let v = Vorg::parse(&b).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v.vert_origin_y(1), 900);
        assert_eq!(v.vert_origin_y(5), 950);
        assert_eq!(v.vert_origin_y(10), 1000);
        // Non-overridden glyphs fall back.
        assert_eq!(v.vert_origin_y(2), 880);
        assert_eq!(v.vert_origin_y(99), 880);
    }

    #[test]
    fn rejects_bad_version() {
        let mut b = build_vorg(0, &[]);
        b[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert!(matches!(Vorg::parse(&b), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_truncated_metrics() {
        let mut b = build_vorg(0, &[(1, 2)]);
        b.truncate(b.len() - 1);
        assert!(matches!(Vorg::parse(&b), Err(Error::Truncated { .. })));
    }
}
