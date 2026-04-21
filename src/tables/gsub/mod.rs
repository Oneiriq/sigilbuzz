//! `GSUB` — Glyph Substitution.
//!
//! Rewrites the glyph stream before positioning: ligature
//! substitution replaces `(f, i)` with `ﬁ`, contextual alternates
//! pick different glyph shapes based on neighbours, and so on.
//! sigilbuzz at M2 ships lookup type 4 (ligature substitution)
//! only — the scaffolding for more types lives here.
//!
//! The table header is identical to GPOS's: version + offsets to
//! `ScriptList`, `FeatureList`, and `LookupList`. Each lookup's
//! `lookupType` is GSUB-specific, enumerated in [`lookup_type`].

use crate::error::{Error, Result};
use crate::tables::layout::{FeatureList, LookupList, ScriptList};
use crate::tables::parse::Reader;

pub mod ligature;

pub use ligature::Ligature;

/// GSUB lookup type numbers. Entries land here as sigilbuzz acquires
/// the corresponding lookup parsers.
pub mod lookup_type {
    /// Single substitution. Deferred.
    pub const SINGLE: u16 = 1;
    /// Multiple substitution (one → many). Deferred.
    pub const MULTIPLE: u16 = 2;
    /// Alternate substitution (one → choice of alternates). Deferred.
    pub const ALTERNATE: u16 = 3;
    /// Ligature substitution (many → one) — implemented.
    pub const LIGATURE: u16 = 4;
    /// Contextual substitution. Deferred.
    pub const CONTEXT: u16 = 5;
    /// Chained contextual substitution. Deferred.
    pub const CHAINED_CONTEXT: u16 = 6;
    /// Extension substitution — forwards to another lookup type.
    pub const EXTENSION: u16 = 7;
    /// Reverse chained contextual substitution. Deferred.
    pub const REVERSE_CHAINED: u16 = 8;
}

/// Parsed `GSUB`.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_field_names)]
pub struct Gsub<'a> {
    script_list: ScriptList<'a>,
    feature_list: FeatureList<'a>,
    lookup_list: LookupList<'a>,
}

impl<'a> Gsub<'a> {
    /// Parses a `GSUB` table.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported GSUB major version",
            });
        }
        let script_list_off = r.read_u16()? as usize;
        let feature_list_off = r.read_u16()? as usize;
        let lookup_list_off = r.read_u16()? as usize;

        let script_list =
            ScriptList::parse(data.get(script_list_off..).ok_or(Error::Malformed {
                offset: script_list_off,
                context: "GSUB scriptList offset past end",
            })?)?;
        let feature_list =
            FeatureList::parse(data.get(feature_list_off..).ok_or(Error::Malformed {
                offset: feature_list_off,
                context: "GSUB featureList offset past end",
            })?)?;
        let lookup_list =
            LookupList::parse(data.get(lookup_list_off..).ok_or(Error::Malformed {
                offset: lookup_list_off,
                context: "GSUB lookupList offset past end",
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

    fn build_empty_gsub() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
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
    fn parses_empty_gsub_header() {
        let bytes = build_empty_gsub();
        let gsub = Gsub::parse(&bytes).unwrap();
        assert_eq!(gsub.script_list().len(), 0);
        assert_eq!(gsub.feature_list().len(), 0);
        assert_eq!(gsub.lookup_list().len(), 0);
    }

    #[test]
    fn rejects_unsupported_major_version() {
        let mut bytes = build_empty_gsub();
        bytes[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert!(matches!(Gsub::parse(&bytes), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(Gsub::parse(&[0u8; 5]).is_err());
    }
}
