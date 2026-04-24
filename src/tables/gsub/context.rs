//! GSUB lookup type 5 — Contextual Substitution.
//!
//! Contextual substitution is the "non-chained" cousin of chained
//! context (type 6): a run of input glyphs triggers a list of nested
//! substitutions, but there is no backtrack or lookahead requirement.
//! It ships less often than type 6 in modern fonts but is still used
//! for simple script-specific rules (e.g. the Myanmar `blws` reorder).
//!
//! The three sub-formats use the same wire shape as GPOS type 7.
//! sigilbuzz keeps the parser in the shared `tables::layout::context`
//! module; this file is a thin typed enum that distinguishes GSUB
//! from GPOS at the call site, plus a format dispatcher so the shape
//! driver sees a single `Context::parse(bytes)` entry point.

use crate::error::{Error, Result};
use crate::tables::layout::{Context1, Context2, Context3};
use crate::tables::parse::Reader;

/// A parsed GSUB type-5 contextual-substitution subtable. The three
/// formats are distinct wire shapes that share semantics; the enum
/// preserves the parsed variant so the dispatcher can hand the
/// right matcher to the shape driver.
#[derive(Debug, Clone)]
pub enum Context<'a> {
    /// Format 1 — glyph-based.
    Format1(Context1<'a>),
    /// Format 2 — class-based.
    Format2(Context2<'a>),
    /// Format 3 — coverage-based.
    Format3(Context3<'a>),
}

impl<'a> Context<'a> {
    /// Parses a context-substitution subtable, dispatching on the
    /// leading `u16` format.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        // Peek the format without consuming.
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        match format {
            1 => Ok(Self::Format1(Context1::parse(data)?)),
            2 => Ok(Self::Format2(Context2::parse(data)?)),
            3 => Ok(Self::Format3(Context3::parse(data)?)),
            _ => Err(Error::Malformed {
                offset: 0,
                context: "unsupported GSUB context substitution format",
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = alloc::vec::Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        assert!(Context::parse(&bytes).is_err());
    }

    #[test]
    fn dispatches_on_format3() {
        // Minimal format-3 subtable: empty input, no lookups.
        let mut bytes = alloc::vec::Vec::new();
        bytes.extend_from_slice(&3u16.to_be_bytes()); // format
        bytes.extend_from_slice(&0u16.to_be_bytes()); // glyphCount
        bytes.extend_from_slice(&0u16.to_be_bytes()); // lookupCount
        assert!(matches!(
            Context::parse(&bytes).unwrap(),
            Context::Format3(_)
        ));
    }
}
