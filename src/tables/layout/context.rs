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
//!
//! Formats 1 and 2 copy their rules into owned vectors at parse time.
//! Rule sets and rules are reached through 16-bit offsets, and a font
//! may point many offsets at the same bytes. Slots that share one
//! rule-set offset share one parsed copy, and every copied rule is
//! charged against a parse budget proportional to the table length.
//! A subtable that would expand past the budget fails to parse with
//! [`Error::Malformed`], so the parsed form stays linear in the size
//! of the table.

mod chained;
mod contextual;
mod fast_path;
mod matchers;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

pub use chained::{
    ChainClassRule2, ChainClassSet2, ChainContext1, ChainContext2, ChainContext3, ChainRule1,
    ChainRuleSet1,
};
pub use contextual::{ClassRule2, ClassSet2, Context1, Context2, Context3, Rule1, RuleSet1};
pub(crate) use matchers::{chain_rule, ChainTests};

use crate::error::{Error, Result};
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
    // Each record is 4 bytes. Never reserve more than the reader can
    // back, so a large count in a short table fails before it
    // allocates.
    let mut out = Vec::with_capacity(usize::from(count).min(r.remaining() / 4));
    for _ in 0..count {
        out.push(SequenceLookupRecord::parse(r)?);
    }
    Ok(out)
}

/// Reads `count` big-endian u16 values.
fn read_u16_array(r: &mut Reader<'_>, count: usize) -> Result<Vec<u16>> {
    let mut out = Vec::with_capacity(count.min(r.remaining() / 2));
    for _ in 0..count {
        out.push(r.read_u16()?);
    }
    Ok(out)
}

/// Reads a u16 count followed by that many Offset16 values.
fn read_offset_array(r: &mut Reader<'_>) -> Result<Vec<usize>> {
    let count = usize::from(r.read_u16()?);
    let mut out = Vec::with_capacity(count.min(r.remaining() / 2));
    for _ in 0..count {
        out.push(usize::from(r.read_u16()?));
    }
    Ok(out)
}

/// Cost, in u16 slots, charged for each parsed rule on top of its
/// payload. It approximates the rule struct and its vector headers.
const RULE_SLOTS: usize = 48;

/// Budget granted per byte of table data, in u16 slots.
const BUDGET_SLOTS_PER_BYTE: usize = 64;

/// Budget floor, in u16 slots, so small tables near the end of a
/// GSUB or GPOS table still get room for real-world rule sharing.
const MIN_BUDGET_SLOTS: usize = 1 << 22;

/// Caps how much rule data one format 1 or format 2 subtable may
/// expand into when it is parsed.
///
/// The budget is counted in u16 slots. Each rule costs [`RULE_SLOTS`]
/// plus one slot per stored glyph id or class and two per nested
/// lookup record. The total allowance is proportional to the length
/// of the table slice the subtable was parsed from, with a floor of
/// [`MIN_BUDGET_SLOTS`].
#[derive(Debug)]
struct RuleBudget {
    remaining: usize,
}

impl RuleBudget {
    fn for_table(data: &[u8]) -> Self {
        Self {
            remaining: data
                .len()
                .saturating_mul(BUDGET_SLOTS_PER_BYTE)
                .max(MIN_BUDGET_SLOTS),
        }
    }

    /// Takes `slots` from the budget, or fails when it runs dry.
    fn charge(&mut self, offset: usize, slots: usize) -> Result<()> {
        self.remaining = self.remaining.checked_sub(slots).ok_or(Error::Malformed {
            offset,
            context: "contextual rules exceed the parse budget",
        })?;
        Ok(())
    }
}

/// Parses the rule sets named by `offsets`. A zero offset is a NULL
/// set, which the spec permits. Slots that repeat an offset share the
/// set parsed the first time that offset was seen.
///
/// Returns one entry per offset (an index into the returned set list,
/// or `None` for a NULL set) plus the list of distinct parsed sets.
fn parse_shared_sets<S>(
    data: &[u8],
    offsets: &[usize],
    context: &'static str,
    budget: &mut RuleBudget,
    parse_set: fn(&[u8], usize, &[u8], &mut RuleBudget) -> Result<S>,
) -> Result<(Vec<Option<usize>>, Vec<S>)> {
    let mut slots = Vec::with_capacity(offsets.len());
    let mut sets = Vec::new();
    let mut index_by_offset: BTreeMap<usize, usize> = BTreeMap::new();
    for &off in offsets {
        if off == 0 {
            slots.push(None);
            continue;
        }
        let index = if let Some(&index) = index_by_offset.get(&off) {
            index
        } else {
            let set_bytes = data.get(off..).ok_or(Error::Malformed {
                offset: off,
                context,
            })?;
            let set = parse_set(data, off, set_bytes, budget)?;
            let index = sets.len();
            sets.push(set);
            index_by_offset.insert(off, index);
            index
        };
        slots.push(Some(index));
    }
    Ok((slots, sets))
}

/// Reads a rule set's offset array and charges the fixed per-rule
/// cost for every rule it names, before any rule is parsed.
fn read_rule_offsets(
    set_off: usize,
    set_bytes: &[u8],
    budget: &mut RuleBudget,
) -> Result<Vec<usize>> {
    let mut r = Reader::new(set_bytes);
    let offs = read_offset_array(&mut r)?;
    budget.charge(set_off, offs.len().saturating_mul(RULE_SLOTS))?;
    Ok(offs)
}

/// Resolves a rule offset that is relative to its rule set.
fn rule_bytes<'d>(
    full: &'d [u8],
    set_off: usize,
    rule_off: usize,
    context: &'static str,
) -> Result<(usize, &'d [u8])> {
    let abs = set_off.saturating_add(rule_off);
    let bytes = full.get(abs..).ok_or(Error::Malformed {
        offset: abs,
        context,
    })?;
    Ok((abs, bytes))
}

#[cfg(test)]
mod tests;
