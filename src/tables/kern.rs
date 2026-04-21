//! Legacy `kern` table.
//!
//! Before OpenType standardised GPOS pair adjustment, fonts carried
//! kerning in a dedicated `kern` table. Many widely-deployed fonts
//! (Open Sans is a popular one) still do — their GPOS is either
//! absent or present-but-empty. The shaper falls back to this table
//! whenever the GPOS kern feature yields no lookups.
//!
//! # Format
//!
//! The Microsoft / OpenType flavour of version 0:
//!
//! ```text
//!   u16 version      = 0
//!   u16 nTables
//!   Subtable[nTables]:
//!     u16 version    = 0
//!     u16 length     (incl. header)
//!     u16 coverage:
//!       bit 0: horizontal (1) / vertical (0)
//!       bit 1: minimum values (1) / kerning values (0)
//!       bit 2: cross-stream
//!       bit 3: override (replace) vs additive (accumulate)
//!       bits 8-15: format
//!     // For format 0:
//!     u16 nPairs, u16 searchRange, u16 entrySelector, u16 rangeShift
//!     KernPair[nPairs]:
//!       u16 left, u16 right, i16 value
//! ```
//!
//! Pairs are sorted by the 32-bit key `(left << 16) | right`, so
//! lookup is a binary search.
//!
//! sigilbuzz today consumes only:
//!
//! - Subtable version 0
//! - Horizontal kerning (bit 0 set)
//! - Kerning values (bit 1 clear)
//! - Format 0 (individual pairs)
//! - Additive mode (bit 3 clear) — each matching subtable adds
//!
//! Vertical kerning, minimum-value subtables, cross-stream, and
//! other formats are recognised and silently skipped. Override
//! subtables are treated as additive because sigilbuzz does not
//! keep per-subtable state — this is the same shortcut HarfBuzz
//! takes for legacy kern.
//!
//! # Apple version 1.0
//!
//! Apple's alternative format starts with a `u32 version =
//! 0x00010000`. It is extremely rare in the wild; sigilbuzz skips
//! it with an explicit error so a malformed font can be
//! distinguished from a genuinely unsupported one.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

const COVERAGE_HORIZONTAL: u16 = 0x0001;
const COVERAGE_MINIMUM: u16 = 0x0002;
const COVERAGE_FORMAT_MASK: u16 = 0xFF00;
const COVERAGE_FORMAT_SHIFT: u32 = 8;

/// Parsed legacy `kern` table. Collects every subtable sigilbuzz can
/// interpret; everything else is dropped silently.
#[derive(Debug, Clone)]
pub struct KernTable<'a> {
    subtables: Vec<Format0<'a>>,
}

#[derive(Debug, Clone, Copy)]
struct Format0<'a> {
    data: &'a [u8],
    pairs_off: usize,
    n_pairs: u16,
}

impl<'a> KernTable<'a> {
    /// Parses a `kern` table. Fonts whose `kern` is absent should
    /// not reach this function — the caller checks `Face::table_bytes`
    /// first.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.read_u16()?;
        if version != 0 {
            // Apple's 0x00010000 lives here; we reject it with a
            // specific error rather than pretending it's malformed.
            return Err(Error::Unsupported {
                context: "legacy kern table version 1.0 (Apple format) not yet implemented",
            });
        }
        let n_tables = r.read_u16()?;

        let mut subtables = Vec::new();
        for _ in 0..n_tables {
            let subtable_start = r.position();
            if subtable_start + 6 > data.len() {
                return Err(Error::Truncated {
                    offset: subtable_start,
                    context: "kern subtable header",
                });
            }
            let sub_version = r.read_u16()?;
            let declared_length = r.read_u16()? as usize;
            let coverage = r.read_u16()?;

            // The `length` field is only u16, which means subtables
            // larger than ~64 KiB — rare but real, Open Sans is one
            // — cannot represent their true size. Treat an out-of-
            // bounds declared length as evidence the u16 overflowed
            // and fall back to the remainder of the table. FreeType
            // and HarfBuzz use the same workaround.
            let declared_end = subtable_start + declared_length;
            let subtable_end = if declared_length < 6 || declared_end > data.len() {
                data.len()
            } else {
                declared_end
            };

            let format = ((coverage & COVERAGE_FORMAT_MASK) >> COVERAGE_FORMAT_SHIFT) as u8;
            let horizontal = coverage & COVERAGE_HORIZONTAL != 0;
            let minimum = coverage & COVERAGE_MINIMUM != 0;

            // Skip subtables we do not understand — but consume
            // their bytes so the next iteration is positioned
            // correctly.
            if sub_version != 0 || format != 0 || !horizontal || minimum {
                r.seek(subtable_end)?;
                continue;
            }

            // Format 0 body: nPairs + 3 search hints + pairs.
            let n_pairs = r.read_u16()?;
            r.skip(6)?; // searchRange, entrySelector, rangeShift

            let pairs_off = r.position();
            let pairs_bytes = n_pairs as usize * 6;
            // Prefer the true payload extent: if the declared
            // subtable end does not fit the pairs but the overall
            // table does, trust the pair count — this is the u16
            // overflow case.
            let effective_end = if pairs_off + pairs_bytes > subtable_end
                && pairs_off + pairs_bytes <= data.len()
            {
                pairs_off + pairs_bytes
            } else {
                subtable_end
            };
            if pairs_off + pairs_bytes > effective_end {
                return Err(Error::Truncated {
                    offset: pairs_off,
                    context: "kern format 0 pairs exceed subtable length",
                });
            }

            subtables.push(Format0 {
                data,
                pairs_off,
                n_pairs,
            });

            // Advance the outer cursor past whichever end we
            // actually used so trailing subtables align correctly.
            r.seek(effective_end)?;
        }

        Ok(Self { subtables })
    }

    /// Sum of kerning deltas for the pair `(left, right)` across
    /// every usable subtable. Returns 0 when no subtable contains
    /// the pair — this is also the "no adjustment" value, and
    /// consumers wanting to distinguish "missing" from "zero" should
    /// reach for a richer API later.
    #[must_use]
    pub fn kern(&self, left: u16, right: u16) -> i16 {
        let mut total: i32 = 0;
        let key = (u32::from(left) << 16) | u32::from(right);
        for sub in &self.subtables {
            if let Some(v) = sub.find(key) {
                total += i32::from(v);
            }
        }
        // Saturating so fonts with pathological kerning cannot panic.
        total.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
    }

    /// Number of format-0 horizontal-kerning subtables that actually
    /// contributed to this parsed view.
    #[must_use]
    pub fn subtable_count(&self) -> usize {
        self.subtables.len()
    }
}

impl Format0<'_> {
    fn pair_at(&self, i: u16) -> (u32, i16) {
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
        let mut lo: u16 = 0;
        let mut hi: u16 = self.n_pairs;
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

    /// Builds a `kern` table with one format-0 horizontal subtable
    /// carrying the given (left, right, value) pairs.
    fn build_single_subtable(pairs: &[(u16, u16, i16)]) -> Vec<u8> {
        let pair_bytes = pairs.len() * 6;
        let subtable_len = 6 + 8 + pair_bytes; // header + format-0 fixed + pairs

        let mut out = Vec::new();
        out.extend_from_slice(&0u16.to_be_bytes()); // version
        out.extend_from_slice(&1u16.to_be_bytes()); // nTables

        // Subtable header.
        out.extend_from_slice(&0u16.to_be_bytes()); // subtable version
        out.extend_from_slice(&(subtable_len as u16).to_be_bytes());
        out.extend_from_slice(&COVERAGE_HORIZONTAL.to_be_bytes()); // horizontal, format 0, kern values
                                                                   // Format 0 body.
        out.extend_from_slice(&(pairs.len() as u16).to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // searchRange
        out.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
        out.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
        for (l, r, v) in pairs {
            out.extend_from_slice(&l.to_be_bytes());
            out.extend_from_slice(&r.to_be_bytes());
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    /// Builds a `kern` table with two subtables: first horizontal
    /// kerning values, second a vertical subtable that should be
    /// silently skipped.
    fn build_mixed_subtables(horiz: &[(u16, u16, i16)], vert: &[(u16, u16, i16)]) -> Vec<u8> {
        let horiz_len = 6 + 8 + horiz.len() * 6;
        let vert_len = 6 + 8 + vert.len() * 6;

        let mut out = Vec::new();
        out.extend_from_slice(&0u16.to_be_bytes()); // version
        out.extend_from_slice(&2u16.to_be_bytes()); // nTables

        // Horizontal.
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&(horiz_len as u16).to_be_bytes());
        out.extend_from_slice(&COVERAGE_HORIZONTAL.to_be_bytes());
        out.extend_from_slice(&(horiz.len() as u16).to_be_bytes());
        out.extend_from_slice(&[0u8; 6]);
        for (l, r, v) in horiz {
            out.extend_from_slice(&l.to_be_bytes());
            out.extend_from_slice(&r.to_be_bytes());
            out.extend_from_slice(&v.to_be_bytes());
        }

        // Vertical (coverage bit 0 clear): should be skipped.
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&(vert_len as u16).to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // bit 0 unset -> vertical
        out.extend_from_slice(&(vert.len() as u16).to_be_bytes());
        out.extend_from_slice(&[0u8; 6]);
        for (l, r, v) in vert {
            out.extend_from_slice(&l.to_be_bytes());
            out.extend_from_slice(&r.to_be_bytes());
            out.extend_from_slice(&v.to_be_bytes());
        }

        out
    }

    #[test]
    fn empty_kern_table_parses_cleanly() {
        let bytes = build_single_subtable(&[]);
        let k = KernTable::parse(&bytes).unwrap();
        assert_eq!(k.subtable_count(), 1);
        assert_eq!(k.kern(1, 2), 0);
    }

    #[test]
    fn pairs_resolve_via_binary_search() {
        // Pairs sorted by (left << 16 | right).
        let bytes = build_single_subtable(&[(10, 20, -30), (10, 30, -5), (40, 5, 7)]);
        let k = KernTable::parse(&bytes).unwrap();
        assert_eq!(k.kern(10, 20), -30);
        assert_eq!(k.kern(10, 30), -5);
        assert_eq!(k.kern(40, 5), 7);
        assert_eq!(k.kern(10, 25), 0);
        assert_eq!(k.kern(99, 99), 0);
    }

    #[test]
    fn vertical_subtables_are_silently_skipped() {
        let horiz = &[(10, 20, -30)];
        let vert = &[(10, 20, 99)]; // would corrupt the answer if not skipped
        let bytes = build_mixed_subtables(horiz, vert);
        let k = KernTable::parse(&bytes).unwrap();
        assert_eq!(k.subtable_count(), 1);
        assert_eq!(k.kern(10, 20), -30);
    }

    #[test]
    fn values_from_multiple_horizontal_subtables_add() {
        // Two horizontal subtables, each contributing to the same
        // (10, 20) pair.
        let subtable_len = 6 + 8 + 6;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&2u16.to_be_bytes()); // two subtables
        for value in [-10i16, -5i16] {
            bytes.extend_from_slice(&0u16.to_be_bytes());
            bytes.extend_from_slice(&(subtable_len as u16).to_be_bytes());
            bytes.extend_from_slice(&COVERAGE_HORIZONTAL.to_be_bytes());
            bytes.extend_from_slice(&1u16.to_be_bytes()); // nPairs
            bytes.extend_from_slice(&[0u8; 6]);
            bytes.extend_from_slice(&10u16.to_be_bytes());
            bytes.extend_from_slice(&20u16.to_be_bytes());
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        let k = KernTable::parse(&bytes).unwrap();
        assert_eq!(k.kern(10, 20), -15);
    }

    #[test]
    fn minimum_values_subtable_is_skipped() {
        // Coverage bit 1 set = minimum values (not kerning values).
        let subtable_len = 6 + 8 + 6;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&(subtable_len as u16).to_be_bytes());
        bytes.extend_from_slice(&(COVERAGE_HORIZONTAL | COVERAGE_MINIMUM).to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 6]);
        bytes.extend_from_slice(&10u16.to_be_bytes());
        bytes.extend_from_slice(&20u16.to_be_bytes());
        bytes.extend_from_slice(&(-100i16).to_be_bytes());
        let k = KernTable::parse(&bytes).unwrap();
        assert_eq!(k.subtable_count(), 0);
        assert_eq!(k.kern(10, 20), 0);
    }

    #[test]
    fn rejects_apple_version_header() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x0001u16.to_be_bytes()); // high half of 0x00010000
        assert!(matches!(
            KernTable::parse(&bytes),
            Err(Error::Unsupported { .. })
        ));
    }

    #[test]
    fn rejects_truncated_subtable_header() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes()); // claim 1 subtable
        bytes.extend_from_slice(&[0u8; 3]); // not enough bytes for a header
        assert!(matches!(
            KernTable::parse(&bytes),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn rejects_subtable_length_exceeding_available_bytes() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes()); // sub version
        bytes.extend_from_slice(&9999u16.to_be_bytes()); // absurd length
        bytes.extend_from_slice(&COVERAGE_HORIZONTAL.to_be_bytes());
        assert!(matches!(
            KernTable::parse(&bytes),
            Err(Error::Truncated { .. })
        ));
    }
}
