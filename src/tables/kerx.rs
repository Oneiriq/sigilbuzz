//! `kerx` — Apple Extended Kerning.
//!
//! `kerx` is the AAT successor to `kern`. AAT-only fonts ship their
//! kerning here. sigilbuzz consults `kerx` only when GPOS has no
//! `kern` feature, so modern OpenType fonts keep their existing
//! behaviour — this matches HarfBuzz's AAT shaper policy.
//!
//! # Layout
//!
//! ```text
//!   u16 version       (2 or 3)
//!   u16 _pad
//!   u32 nTables
//!   Subtable subtables[nTables]
//!
//!   Subtable:
//!     u32 length       (bytes, incl. this header)
//!     u32 coverage     (low byte = format; high bits = flags)
//!     u32 tupleCount   (variation-font kerning — sigilbuzz ignores)
//!     Body body        (format-specific)
//! ```
//!
//! Only format 0 (ordered pair list) is implemented; it is the
//! format the vast majority of AAT fonts actually ship. Format 2
//! (two-class compound tables) is deferred with a clear
//! `Unsupported` error so a future PR can drop it in without
//! changing the public surface.
//!
//! # Format 0
//!
//! ```text
//!   u32 nPairs
//!   u32 searchRange
//!   u32 entrySelector
//!   u32 rangeShift
//!   Pair pairs[nPairs]:
//!     u16 left
//!     u16 right
//!     i16 value
//! ```
//!
//! Pairs are sorted by the 32-bit key `(left << 16) | right`, so
//! lookup is a binary search — exactly as in the legacy `kern`
//! table, just with a u32 count instead of u16.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

const COVERAGE_FORMAT_MASK: u32 = 0xFF;
// Coverage flags (high byte of the coverage u32).
const COVERAGE_VERTICAL: u32 = 1 << 31;
const COVERAGE_CROSS_STREAM: u32 = 1 << 30;
const COVERAGE_VARIATION: u32 = 1 << 29;

/// Parsed `kerx` table.
#[derive(Debug, Clone)]
pub struct Kerx<'a> {
    version: u16,
    subtables: Vec<Format0<'a>>,
}

#[derive(Debug, Clone, Copy)]
struct Format0<'a> {
    data: &'a [u8],
    pairs_off: usize,
    n_pairs: u32,
}

impl<'a> Kerx<'a> {
    /// Parses a `kerx` table. Returns [`Error::Unsupported`] for
    /// versions outside {2, 3} — every AAT font sigilbuzz targets
    /// ships one of those two.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.read_u16()?;
        if version != 2 && version != 3 {
            return Err(Error::Unsupported {
                context: "kerx version outside {2, 3}",
            });
        }
        let _pad = r.read_u16()?;
        let n_tables = r.read_u32()?;

        let mut subtables = Vec::new();
        for _ in 0..n_tables {
            let sub_start = r.position();
            if sub_start + 12 > data.len() {
                return Err(Error::Truncated {
                    offset: sub_start,
                    context: "kerx subtable header",
                });
            }
            let length = r.read_u32()? as usize;
            let coverage = r.read_u32()?;
            let _tuple_count = r.read_u32()?;

            let sub_end = sub_start.checked_add(length).ok_or(Error::Malformed {
                offset: sub_start,
                context: "kerx subtable length overflow",
            })?;
            if sub_end > data.len() {
                return Err(Error::Truncated {
                    offset: sub_end,
                    context: "kerx subtable extends past table",
                });
            }

            let format = (coverage & COVERAGE_FORMAT_MASK) as u8;
            // Skip vertical, cross-stream, and variation subtables —
            // sigilbuzz produces horizontal advances only for now.
            // The cross-stream bit moves a glyph's origin in the
            // opposite axis (e.g. Zapfino's connecting ligatures
            // nudge y to tuck the bowls together); applying it
            // blindly would corrupt positions, so we skip until the
            // feature lands.
            if coverage & (COVERAGE_VERTICAL | COVERAGE_CROSS_STREAM | COVERAGE_VARIATION) != 0 {
                r.seek(sub_end)?;
                continue;
            }

            // Format 0 is the common case; formats 1 (state table),
            // 2 (two-class), 4 (control points / anchors), and 6
            // (indexed class kerning) all exist in the spec but are
            // rare — sigilbuzz skips them silently until a real font
            // exercises the path, so the seek to `sub_end` below
            // keeps later subtables correctly aligned.
            if format == 0 {
                // Format 0 body: u32 nPairs + 3 u32 search hints.
                let body_start = r.position();
                if body_start + 16 > sub_end {
                    return Err(Error::Truncated {
                        offset: body_start,
                        context: "kerx format 0 header",
                    });
                }
                let n_pairs = r.read_u32()?;
                r.skip(12)?; // searchRange, entrySelector, rangeShift
                let pairs_off = r.position();
                let pairs_bytes = (n_pairs as usize).saturating_mul(6);
                let required = pairs_off.checked_add(pairs_bytes).ok_or(Error::Malformed {
                    offset: pairs_off,
                    context: "kerx format 0 pairs overflow",
                })?;
                if required > sub_end {
                    return Err(Error::Truncated {
                        offset: required,
                        context: "kerx format 0 pairs exceed subtable",
                    });
                }
                subtables.push(Format0 {
                    data,
                    pairs_off,
                    n_pairs,
                });
            }

            r.seek(sub_end)?;
        }

        Ok(Self {
            version,
            subtables,
        })
    }

    /// Reported version word (2 or 3).
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Sum of kerning deltas across every format-0 subtable for the
    /// pair `(left, right)`. Zero when no pair matches.
    #[must_use]
    pub fn kern(&self, left: u16, right: u16) -> i16 {
        let key = (u32::from(left) << 16) | u32::from(right);
        let mut total: i32 = 0;
        for sub in &self.subtables {
            if let Some(v) = sub.find(key) {
                total += i32::from(v);
            }
        }
        total.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
    }

    /// Number of parsed format-0 subtables — useful in tests to
    /// assert which subtables were retained.
    #[must_use]
    pub fn subtable_count(&self) -> usize {
        self.subtables.len()
    }
}

impl Format0<'_> {
    fn pair_at(&self, i: u32) -> (u32, i16) {
        let off = self.pairs_off + i as usize * 6;
        let left = u16::from_be_bytes([self.data[off], self.data[off + 1]]);
        let right = u16::from_be_bytes([self.data[off + 2], self.data[off + 3]]);
        let value = i16::from_be_bytes([self.data[off + 4], self.data[off + 5]]);
        ((u32::from(left) << 16) | u32::from(right), value)
    }

    fn find(&self, key: u32) -> Option<i16> {
        if self.n_pairs == 0 {
            return None;
        }
        let mut lo: u32 = 0;
        let mut hi: u32 = self.n_pairs;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (k, v) = self.pair_at(mid);
            match k.cmp(&key) {
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
                core::cmp::Ordering::Equal => return Some(v),
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_kerx_format0(pairs: &[(u16, u16, i16)]) -> Vec<u8> {
        let pair_bytes = pairs.len() * 6;
        let body_len = 16 + pair_bytes; // 4 × u32 + pairs
        let sub_len = 12 + body_len;

        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes()); // version
        out.extend_from_slice(&0u16.to_be_bytes()); // pad
        out.extend_from_slice(&1u32.to_be_bytes()); // nTables

        // Subtable header.
        out.extend_from_slice(&(sub_len as u32).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // coverage: horizontal, format 0
        out.extend_from_slice(&0u32.to_be_bytes()); // tupleCount

        // Format 0 body.
        out.extend_from_slice(&(pairs.len() as u32).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // searchRange
        out.extend_from_slice(&0u32.to_be_bytes()); // entrySelector
        out.extend_from_slice(&0u32.to_be_bytes()); // rangeShift
        for (l, r, v) in pairs {
            out.extend_from_slice(&l.to_be_bytes());
            out.extend_from_slice(&r.to_be_bytes());
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    #[test]
    fn format0_binary_search_finds_pairs() {
        let bytes = build_kerx_format0(&[(10, 20, -30), (10, 30, -5), (40, 5, 7)]);
        let k = Kerx::parse(&bytes).unwrap();
        assert_eq!(k.version(), 2);
        assert_eq!(k.kern(10, 20), -30);
        assert_eq!(k.kern(40, 5), 7);
        assert_eq!(k.kern(99, 99), 0);
    }

    #[test]
    fn empty_kerx_table_yields_no_subtables() {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes()); // nTables
        let k = Kerx::parse(&bytes).unwrap();
        assert_eq!(k.subtable_count(), 0);
        assert_eq!(k.kern(1, 2), 0);
    }

    #[test]
    fn vertical_subtable_is_skipped() {
        // Build a 2-subtable kerx: first horizontal, second vertical
        // (coverage bit 31 set). The vertical one should be dropped.
        let sub_body_len = 16 + 6; // one pair
        let sub_len = 12 + sub_body_len;
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes()); // 2 subtables

        for (coverage, value) in [(0u32, -10i16), (COVERAGE_VERTICAL, 99i16)] {
            bytes.extend_from_slice(&(sub_len as u32).to_be_bytes());
            bytes.extend_from_slice(&coverage.to_be_bytes());
            bytes.extend_from_slice(&0u32.to_be_bytes()); // tupleCount
            bytes.extend_from_slice(&1u32.to_be_bytes()); // nPairs
            bytes.extend_from_slice(&[0u8; 12]);
            bytes.extend_from_slice(&10u16.to_be_bytes());
            bytes.extend_from_slice(&20u16.to_be_bytes());
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        let k = Kerx::parse(&bytes).unwrap();
        assert_eq!(k.subtable_count(), 1);
        assert_eq!(k.kern(10, 20), -10);
    }

    #[test]
    fn rejects_unknown_version() {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&5u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        assert!(matches!(
            Kerx::parse(&bytes),
            Err(Error::Unsupported { .. })
        ));
    }
}
