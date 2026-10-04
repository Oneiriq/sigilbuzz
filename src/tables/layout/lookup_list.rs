//! OpenType `LookupList`: the catalog of lookups that features
//! reference.
//!
//! ```text
//!   LookupList
//!     u16      lookupCount
//!     Offset16 lookupOffsets[lookupCount]
//!
//!   Lookup
//!     u16       lookupType
//!     u16       lookupFlag
//!     u16       subtableCount
//!     Offset16  subtableOffsets[subtableCount]
//!     (u16     markFilteringSet)       -- only if lookupFlag & 0x10
//! ```
//!
//! This module exposes only the lookup header. Subtable bodies are
//! returned as raw byte slices so each lookup-type-specific parser
//! can handle them in its own module.

use crate::error::{Error, Result};
use crate::tables::layout::skip_iter::LOOKUP_FLAG_USE_MARK_FILTERING_SET;
use crate::tables::parse::Reader;

/// Parsed `LookupList`.
#[derive(Debug, Clone, Copy)]
pub struct LookupList<'a> {
    data: &'a [u8],
    offsets_off: usize,
    lookup_count: u16,
}

impl<'a> LookupList<'a> {
    /// Parses a `LookupList` from its raw bytes.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let lookup_count = r.read_u16()?;
        let offsets_off = r.position();
        let need = offsets_off + lookup_count as usize * 2;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: offsets_off,
                context: "lookupList offsets shorter than lookupCount",
            });
        }
        Ok(Self {
            data,
            offsets_off,
            lookup_count,
        })
    }

    /// Number of lookups in the list.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.lookup_count
    }

    /// True if the list has no lookups.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.lookup_count == 0
    }

    /// Returns the lookup at `index`, or `None` when out of range.
    #[must_use]
    pub fn get(&self, index: u16) -> Option<Lookup<'a>> {
        Lookup::parse_at(self.data, self.lookup_offset(index)?).ok()
    }

    /// The offset of lookup `index` from the start of the list, or
    /// `None` when out of range. Lookup indices with the same offset
    /// name the same lookup.
    #[must_use]
    pub(crate) fn lookup_offset(&self, index: u16) -> Option<u16> {
        if index >= self.lookup_count {
            return None;
        }
        let off = self.offsets_off + index as usize * 2;
        Some(u16::from_be_bytes([self.data[off], self.data[off + 1]]))
    }
}

/// A single lookup header. Subtable bodies are exposed via
/// [`Lookup::subtable_bytes`], which a lookup-type-specific parser
/// re-reads once it has identified what kind of lookup this is.
#[derive(Debug, Clone, Copy)]
pub struct Lookup<'a> {
    data: &'a [u8],
    base: usize,
    lookup_type: u16,
    lookup_flag: u16,
    subtable_offsets_off: usize,
    subtable_count: u16,
    /// Trailing markFilteringSet, if the flag said so.
    mark_filtering_set: Option<u16>,
}

impl<'a> Lookup<'a> {
    fn parse_at(data: &'a [u8], offset: u16) -> Result<Self> {
        let base = offset as usize;
        if base >= data.len() {
            return Err(Error::Malformed {
                offset: base,
                context: "lookup offset past end of LookupList",
            });
        }
        let mut r = Reader::at(data, base)?;
        let lookup_type = r.read_u16()?;
        let lookup_flag = r.read_u16()?;
        let subtable_count = r.read_u16()?;
        let subtable_offsets_off = r.position();

        let offsets_bytes = subtable_count as usize * 2;
        let mark_extra = if lookup_flag & LOOKUP_FLAG_USE_MARK_FILTERING_SET != 0 {
            2
        } else {
            0
        };
        let need = subtable_offsets_off + offsets_bytes + mark_extra;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: subtable_offsets_off,
                context: "lookup header shorter than declared",
            });
        }

        let mark_filtering_set = if mark_extra == 2 {
            let at = subtable_offsets_off + offsets_bytes;
            Some(u16::from_be_bytes([data[at], data[at + 1]]))
        } else {
            None
        };

        Ok(Self {
            data,
            base,
            lookup_type,
            lookup_flag,
            subtable_offsets_off,
            subtable_count,
            mark_filtering_set,
        })
    }

    /// Lookup type (1-9 in GSUB, 1-9 in GPOS, meanings differ).
    #[must_use]
    pub const fn lookup_type(&self) -> u16 {
        self.lookup_type
    }

    /// `lookupFlag` bit field.
    #[must_use]
    pub const fn flag(&self) -> u16 {
        self.lookup_flag
    }

    /// Mark filtering set index, present only when
    /// `lookupFlag & USE_MARK_FILTERING_SET` is set.
    #[must_use]
    pub const fn mark_filtering_set(&self) -> Option<u16> {
        self.mark_filtering_set
    }

    /// Number of subtables under this lookup.
    #[must_use]
    pub const fn subtable_count(&self) -> u16 {
        self.subtable_count
    }

    /// Returns the raw subtable byte slice starting at subtable
    /// `index`, extending to the end of the container table. The
    /// caller (a type-specific parser) decides how many bytes it
    /// needs.
    #[must_use]
    pub fn subtable_bytes(&self, index: u16) -> Option<&'a [u8]> {
        if index >= self.subtable_count {
            return None;
        }
        let off = self.subtable_offsets_off + index as usize * 2;
        let rel = u16::from_be_bytes([self.data[off], self.data[off + 1]]);
        let abs = self.base + rel as usize;
        self.data.get(abs..)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Assembles a `LookupList` where each lookup is declared by
    /// `(lookup_type, lookup_flag, subtables_as_byte_slices)`.
    fn build_lookup_list(lookups: &[(u16, u16, &[&[u8]])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(lookups.len() as u16).to_be_bytes());
        let offsets_start = out.len();
        for _ in 0..lookups.len() {
            out.extend_from_slice(&[0u8; 2]); // placeholder
        }
        for (i, (lt, lf, subs)) in lookups.iter().enumerate() {
            let lookup_start = out.len();
            out.extend_from_slice(&lt.to_be_bytes());
            out.extend_from_slice(&lf.to_be_bytes());
            out.extend_from_slice(&(subs.len() as u16).to_be_bytes());
            let sub_offsets_start = out.len();
            for _ in 0..subs.len() {
                out.extend_from_slice(&[0u8; 2]); // subtable offset placeholder
            }
            if lf & LOOKUP_FLAG_USE_MARK_FILTERING_SET != 0 {
                out.extend_from_slice(&0u16.to_be_bytes()); // markFilteringSet
            }
            for (j, body) in subs.iter().enumerate() {
                let body_start = out.len();
                out.extend_from_slice(body);
                let rel = (body_start - lookup_start) as u16;
                let slot = sub_offsets_start + j * 2;
                out[slot..slot + 2].copy_from_slice(&rel.to_be_bytes());
            }
            let slot = offsets_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&(lookup_start as u16).to_be_bytes());
        }
        out
    }

    #[test]
    fn empty_lookup_list_parses() {
        let bytes = build_lookup_list(&[]);
        let ll = LookupList::parse(&bytes).unwrap();
        assert!(ll.is_empty());
        assert_eq!(ll.len(), 0);
        assert!(ll.get(0).is_none());
    }

    #[test]
    fn lookup_header_fields_round_trip() {
        let sub = [0xAA, 0xBB, 0xCC];
        let bytes = build_lookup_list(&[(2, 0, &[&sub])]);
        let ll = LookupList::parse(&bytes).unwrap();
        let lookup = ll.get(0).unwrap();
        assert_eq!(lookup.lookup_type(), 2);
        assert_eq!(lookup.flag(), 0);
        assert_eq!(lookup.subtable_count(), 1);
        assert!(lookup.mark_filtering_set().is_none());
        let subtable = lookup.subtable_bytes(0).unwrap();
        // `subtable_bytes` returns a slice that *starts at* the
        // subtable. The caller reads what it needs.
        assert_eq!(&subtable[..3], &sub);
    }

    #[test]
    fn subtable_bytes_out_of_range_returns_none() {
        let sub = [0u8; 2];
        let bytes = build_lookup_list(&[(4, 0, &[&sub])]);
        let ll = LookupList::parse(&bytes).unwrap();
        let lookup = ll.get(0).unwrap();
        assert!(lookup.subtable_bytes(1).is_none());
    }

    #[test]
    fn mark_filtering_set_read_when_flag_set() {
        let sub = [0u8; 2];
        // Flag 0x0010 -> USE_MARK_FILTERING_SET.
        let bytes = build_lookup_list(&[(4, 0x0010, &[&sub])]);
        let ll = LookupList::parse(&bytes).unwrap();
        let lookup = ll.get(0).unwrap();
        assert_eq!(lookup.mark_filtering_set(), Some(0));
    }

    #[test]
    fn get_out_of_range_returns_none() {
        let sub = [0u8; 2];
        let bytes = build_lookup_list(&[(1, 0, &[&sub])]);
        let ll = LookupList::parse(&bytes).unwrap();
        assert!(ll.get(1).is_none());
    }

    #[test]
    fn multiple_subtables_yield_distinct_bytes() {
        let a = [0x01, 0x02];
        let b = [0x03, 0x04];
        let bytes = build_lookup_list(&[(1, 0, &[&a, &b])]);
        let ll = LookupList::parse(&bytes).unwrap();
        let lookup = ll.get(0).unwrap();
        assert_eq!(lookup.subtable_count(), 2);
        assert_eq!(&lookup.subtable_bytes(0).unwrap()[..2], &a);
        assert_eq!(&lookup.subtable_bytes(1).unwrap()[..2], &b);
    }

    #[test]
    fn rejects_truncated_offsets() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&3u16.to_be_bytes()); // claim 3 lookups
        bytes.extend_from_slice(&[0u8; 2]); // only one offset
        assert!(matches!(
            LookupList::parse(&bytes),
            Err(Error::Truncated { .. })
        ));
    }
}
