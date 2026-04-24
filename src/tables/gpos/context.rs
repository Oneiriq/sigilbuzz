//! GPOS lookup type 7 — Contextual Positioning.
//!
//! Same three-format shape as GSUB type 5: a contextual match drives
//! a list of `(sequenceIndex, lookupListIndex)` nested positioning
//! lookups. The match logic is identical to GSUB because the shared
//! `tables::layout::context` module is GSUB/GPOS agnostic — the only
//! thing that changes is which dispatcher runs on a hit.

use crate::error::{Error, Result};
use crate::tables::layout::{Context1, Context2, Context3};
use crate::tables::parse::Reader;

/// A parsed GPOS type-7 contextual-positioning subtable.
#[derive(Debug, Clone)]
pub enum ContextPos<'a> {
    /// Format 1 — glyph-based.
    Format1(Context1<'a>),
    /// Format 2 — class-based.
    Format2(Context2<'a>),
    /// Format 3 — coverage-based.
    Format3(Context3<'a>),
}

impl<'a> ContextPos<'a> {
    /// Parses a context-positioning subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        match format {
            1 => Ok(Self::Format1(Context1::parse(data)?)),
            2 => Ok(Self::Format2(Context2::parse(data)?)),
            3 => Ok(Self::Format3(Context3::parse(data)?)),
            _ => Err(Error::Malformed {
                offset: 0,
                context: "unsupported GPOS context positioning format",
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatches_on_format3() {
        let mut bytes = alloc::vec::Vec::new();
        bytes.extend_from_slice(&3u16.to_be_bytes()); // format
        bytes.extend_from_slice(&0u16.to_be_bytes()); // glyphCount
        bytes.extend_from_slice(&0u16.to_be_bytes()); // lookupCount
        assert!(matches!(
            ContextPos::parse(&bytes).unwrap(),
            ContextPos::Format3(_)
        ));
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = alloc::vec::Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        assert!(ContextPos::parse(&bytes).is_err());
    }
}
