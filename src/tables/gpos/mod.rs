//! `GPOS` — Glyph Positioning.
//!
//! Refines the advances and placements produced by cmap+hmtx with
//! feature-driven deltas: kerning (lookup type 2), cursive
//! attachment (type 3), mark-to-base attachment (type 4), and so
//! on. sigilbuzz at M2 implements type 2 (pair adjustment) only —
//! enough to make kerning actually apply — and exposes the
//! machinery for later lookup types to slot in.
//!
//! The table header is shared with GSUB: version + offsets to
//! `ScriptList`, `FeatureList`, and `LookupList`. Each lookup in the
//! `LookupList` has a `lookupType` — see
//! [`LookupType`] — that determines how its subtables are parsed.

use crate::error::{Error, Result};
use crate::tables::layout::{FeatureList, LookupList, ScriptList};
use crate::tables::parse::Reader;

pub mod anchor;
pub mod chain_context;
pub mod context;
pub mod mark_base;
pub mod mark_liga;
pub mod mark_mark;
pub mod pair_pos;
pub mod single_adj;
pub mod value_record;

pub use anchor::Anchor;
pub use chain_context::ChainContextPos;
pub use context::ContextPos;
pub use mark_base::{MarkAttachment, MarkBasePos};
pub use mark_liga::MarkLigaPos;
pub use mark_mark::MarkMarkPos;
pub use pair_pos::{PairPos, PairPosFormat1, PairPosFormat2};
pub use single_adj::SinglePos;
pub use value_record::ValueRecord;

/// Canonical GPOS lookup type numbers. Not exhaustive today; entries
/// land here as sigilbuzz acquires the corresponding lookup parsers.
pub mod lookup_type {
    /// Single adjustment. Deferred.
    pub const SINGLE_ADJUSTMENT: u16 = 1;
    /// Pair adjustment — the one sigilbuzz currently implements.
    pub const PAIR_ADJUSTMENT: u16 = 2;
    /// Cursive attachment. Deferred.
    pub const CURSIVE_ATTACHMENT: u16 = 3;
    /// Mark-to-base attachment. Deferred.
    pub const MARK_TO_BASE: u16 = 4;
    /// Mark-to-ligature attachment. Deferred.
    pub const MARK_TO_LIGATURE: u16 = 5;
    /// Mark-to-mark attachment. Deferred.
    pub const MARK_TO_MARK: u16 = 6;
    /// Context positioning — implemented for formats 1, 2, 3.
    pub const CONTEXT: u16 = 7;
    /// Chained context positioning — implemented for formats 1, 2, 3.
    pub const CHAINED_CONTEXT: u16 = 8;
    /// Extension positioning — forwards to another lookup type.
    pub const EXTENSION: u16 = 9;
}

/// Parsed `GPOS`.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_field_names)]
pub struct Gpos<'a> {
    script_list: ScriptList<'a>,
    feature_list: FeatureList<'a>,
    lookup_list: LookupList<'a>,
}

impl<'a> Gpos<'a> {
    /// Parses a `GPOS` table.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported GPOS major version",
            });
        }
        let script_list_off = r.read_u16()? as usize;
        let feature_list_off = r.read_u16()? as usize;
        let lookup_list_off = r.read_u16()? as usize;

        let script_list =
            ScriptList::parse(data.get(script_list_off..).ok_or(Error::Malformed {
                offset: script_list_off,
                context: "GPOS scriptList offset past end",
            })?)?;
        let feature_list =
            FeatureList::parse(data.get(feature_list_off..).ok_or(Error::Malformed {
                offset: feature_list_off,
                context: "GPOS featureList offset past end",
            })?)?;
        let lookup_list =
            LookupList::parse(data.get(lookup_list_off..).ok_or(Error::Malformed {
                offset: lookup_list_off,
                context: "GPOS lookupList offset past end",
            })?)?;

        Ok(Self {
            script_list,
            feature_list,
            lookup_list,
        })
    }

    /// Returns the parsed `ScriptList`.
    #[must_use]
    pub const fn script_list(&self) -> &ScriptList<'a> {
        &self.script_list
    }

    /// Returns the parsed `FeatureList`.
    #[must_use]
    pub const fn feature_list(&self) -> &FeatureList<'a> {
        &self.feature_list
    }

    /// Returns the parsed `LookupList`.
    #[must_use]
    pub const fn lookup_list(&self) -> &LookupList<'a> {
        &self.lookup_list
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Builds a minimal GPOS table where ScriptList, FeatureList,
    /// and LookupList are all empty. Useful for smoke-testing the
    /// top-level header parser without also having to construct the
    /// child tables.
    fn build_empty_gpos() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
                                                    // Layout: header (10 bytes) + 3 empty tables each starting
                                                    // with a u16 count = 0.
        let header_len = 10u16;
        let sl_off = header_len;
        let fl_off = sl_off + 2;
        let ll_off = fl_off + 2;
        out.extend_from_slice(&sl_off.to_be_bytes());
        out.extend_from_slice(&fl_off.to_be_bytes());
        out.extend_from_slice(&ll_off.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // scriptCount
        out.extend_from_slice(&0u16.to_be_bytes()); // featureCount
        out.extend_from_slice(&0u16.to_be_bytes()); // lookupCount
        out
    }

    #[test]
    fn parses_empty_gpos_header() {
        let bytes = build_empty_gpos();
        let gpos = Gpos::parse(&bytes).unwrap();
        assert_eq!(gpos.script_list().len(), 0);
        assert_eq!(gpos.feature_list().len(), 0);
        assert_eq!(gpos.lookup_list().len(), 0);
    }

    #[test]
    fn rejects_unsupported_major_version() {
        let mut bytes = build_empty_gpos();
        bytes[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert!(matches!(Gpos::parse(&bytes), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(Gpos::parse(&[0u8; 5]).is_err());
    }
}
