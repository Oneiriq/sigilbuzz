//! Shared primitives for the OpenType contextual / chained-contextual
//! lookup families (GSUB type 5 & 6, GPOS type 7 & 8).
//!
//! GSUB and GPOS disagree on what the nested lookups *do* (GSUB
//! rewrites glyph ids, GPOS adjusts advances and offsets), but the
//! outer shape is identical: match a window of the glyph stream
//! against a Coverage / ClassDef / explicit-glyph pattern, and fire
//! a list of nested `(sequenceIndex, lookupListIndex)` records at
//! their declared positions inside that window.
//!
//! This module owns the parsing of the three subtable formats used
//! by both families:
//!
//! - Format 1: glyph-based. The first input glyph drives a coverage
//!   index; each coverage entry points at a `RuleSet` of explicit
//!   glyph-id sequences.
//! - Format 2: class-based. The first input glyph's coverage gate
//!   picks a `ClassSet`; each class set stores class-id sequences
//!   that are matched via one shared `ClassDef` (contextual) or
//!   three (backtrack / input / lookahead) for chained-context.
//! - Format 3: explicit-coverage arrays for every window position.
//!
//! Every lookup type that uses these formats shares the
//! `SequenceLookupRecord` payload, a `(sequenceIndex, lookupListIndex)`
//! pair the caller recursively dispatches.

mod chained;
mod contextual;
mod matchers;

use alloc::vec::Vec;

pub use chained::{
    ChainClassRule2, ChainClassSet2, ChainContext1, ChainContext2, ChainContext3, ChainRule1,
    ChainRuleSet1,
};
pub use contextual::{ClassRule2, ClassSet2, Context1, Context2, Context3, Rule1, RuleSet1};

use crate::error::Result;
use crate::tables::parse::Reader;

/// `(sequenceIndex, lookupListIndex)`: the position inside the
/// matched input window where the nested lookup fires, and which
/// entry of the enclosing `LookupList` it dispatches to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceLookupRecord {
    /// Zero-based offset into the matched input sequence.
    pub sequence_index: u16,
    /// Index into the enclosing `LookupList`.
    pub lookup_list_index: u16,
}

impl SequenceLookupRecord {
    /// Parses a single record (4 bytes).
    pub fn parse(r: &mut Reader<'_>) -> Result<Self> {
        let sequence_index = r.read_u16()?;
        let lookup_list_index = r.read_u16()?;
        Ok(Self {
            sequence_index,
            lookup_list_index,
        })
    }
}

/// Parses `count` sequence-lookup records in order.
pub fn parse_sequence_lookup_records(
    r: &mut Reader<'_>,
    count: u16,
) -> Result<Vec<SequenceLookupRecord>> {
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        out.push(SequenceLookupRecord::parse(r)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
