//! GPOS lookup type 8: Chained Contextual Positioning.
//!
//! Same three-format shape as GSUB type 6. The shared
//! `tables::layout::context` helpers do the parsing and matching:
//! this file is just the GPOS-side enum so the positioning
//! dispatcher stays symmetric with the substitution dispatcher.

use crate::error::{Error, Result};
use crate::tables::layout::{ChainContext1, ChainContext2, ChainContext3};
use crate::tables::parse::Reader;

/// A parsed GPOS type-8 chained-context-positioning subtable.
#[derive(Debug, Clone)]
pub enum ChainContextPos<'a> {
    /// Format 1: glyph-based.
    Format1(ChainContext1<'a>),
    /// Format 2: class-based.
    Format2(ChainContext2<'a>),
    /// Format 3: coverage-based.
    Format3(ChainContext3<'a>),
}

impl<'a> ChainContextPos<'a> {
    /// Parses a chain-context-positioning subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        match format {
            1 => Ok(Self::Format1(ChainContext1::parse(data)?)),
            2 => Ok(Self::Format2(ChainContext2::parse(data)?)),
            3 => Ok(Self::Format3(ChainContext3::parse(data)?)),
            _ => Err(Error::Malformed {
                offset: 0,
                context: "unsupported GPOS chain-context positioning format",
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
        bytes.extend_from_slice(&0u16.to_be_bytes()); // backtrack count
        bytes.extend_from_slice(&0u16.to_be_bytes()); // input count
        bytes.extend_from_slice(&0u16.to_be_bytes()); // lookahead count
        bytes.extend_from_slice(&0u16.to_be_bytes()); // lookup count
        assert!(matches!(
            ChainContextPos::parse(&bytes).unwrap(),
            ChainContextPos::Format3(_)
        ));
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = alloc::vec::Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        assert!(ChainContextPos::parse(&bytes).is_err());
    }
}
