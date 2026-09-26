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

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::layout::skip_iter::MatchFilter;
use crate::tables::layout::{ClassDef, Coverage};
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

// ---------------------------------------------------------------------
// Contextual (non-chained) format 1: glyph-based
// ---------------------------------------------------------------------

/// Format 1: glyph-based contextual rules. The covered first glyph
/// selects a `RuleSet`; each rule inside carries the remaining input
/// glyph ids to match plus the nested lookup records.
#[derive(Debug, Clone)]
pub struct Context1<'a> {
    coverage: Coverage<'a>,
    /// Index into `sets` for each coverage index, `None` for a NULL
    /// rule set.
    set_slots: Vec<Option<usize>>,
    sets: Vec<RuleSet1>,
}

/// All rules keyed off a single covered first glyph.
#[derive(Debug, Clone)]
pub struct RuleSet1 {
    /// Rules tried in declaration order. The first match wins.
    pub rules: Vec<Rule1>,
}

/// One glyph-id sequence + its nested lookups. The first input glyph
/// is implicit in the coverage that steered us to this rule, so
/// `input_tail` stores glyphs `[1..]`.
#[derive(Debug, Clone)]
pub struct Rule1 {
    /// Remaining input glyph ids after the coverage-matched first.
    pub input_tail: Vec<u16>,
    /// Nested lookups to fire, relative to the matched input window.
    pub lookups: Vec<SequenceLookupRecord>,
}

impl<'a> Context1<'a> {
    /// Parses a format-1 contextual subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "context format mismatch (expected 1)",
            });
        }
        let coverage_off = r.read_u16()? as usize;
        let rule_set_offs = read_offset_array(&mut r)?;
        let coverage = Coverage::parse(data.get(coverage_off..).ok_or(Error::Malformed {
            offset: coverage_off,
            context: "context format 1 coverage offset past end",
        })?)?;

        let mut budget = RuleBudget::for_table(data);
        let (set_slots, sets) = parse_shared_sets(
            data,
            &rule_set_offs,
            "context format 1 ruleSet offset past end",
            &mut budget,
            parse_rule_set1,
        )?;
        Ok(Self {
            coverage,
            set_slots,
            sets,
        })
    }

    /// Coverage table. The caller uses it to gate matching and to
    /// look up the ruleset index for the first glyph.
    #[must_use]
    pub const fn coverage(&self) -> &Coverage<'a> {
        &self.coverage
    }

    /// Ruleset for the first glyph's coverage index, or `None` when
    /// the subtable carries a NULL slot.
    #[must_use]
    pub fn rule_set(&self, coverage_index: u16) -> Option<&RuleSet1> {
        let set = (*self.set_slots.get(coverage_index as usize)?)?;
        self.sets.get(set)
    }
}

fn parse_rule_set1(
    full: &[u8],
    set_off: usize,
    set_bytes: &[u8],
    budget: &mut RuleBudget,
) -> Result<RuleSet1> {
    let rule_offs = read_rule_offsets(set_off, set_bytes, budget)?;
    let mut rules = Vec::with_capacity(rule_offs.len());
    for off in rule_offs {
        let (abs, bytes) = rule_bytes(full, set_off, off, "context format 1 rule offset past end")?;
        rules.push(parse_rule1(bytes, abs, budget)?);
    }
    Ok(RuleSet1 { rules })
}

fn parse_rule1(data: &[u8], abs: usize, budget: &mut RuleBudget) -> Result<Rule1> {
    let mut r = Reader::new(data);
    let glyph_count = r.read_u16()? as usize;
    let lookup_count = r.read_u16()?;
    // A rule with zero input glyphs is meaningless. It parses with an
    // empty tail, the same as a one-glyph rule.
    let tail_len = glyph_count.saturating_sub(1);
    budget.charge(abs, tail_len + 2 * usize::from(lookup_count))?;
    let input_tail = read_u16_array(&mut r, tail_len)?;
    let lookups = parse_sequence_lookup_records(&mut r, lookup_count)?;
    Ok(Rule1 {
        input_tail,
        lookups,
    })
}

// ---------------------------------------------------------------------
// Contextual format 2: class-based
// ---------------------------------------------------------------------

/// Format 2: class-based contextual rules. One shared `ClassDef`
/// defines every glyph's class; the first input glyph's coverage
/// gates matching and its class index selects a `ClassSet`.
#[derive(Debug, Clone)]
pub struct Context2<'a> {
    coverage: Coverage<'a>,
    class_def: ClassDef<'a>,
    /// Index into `sets` for each class, `None` for a NULL class set.
    set_slots: Vec<Option<usize>>,
    sets: Vec<ClassSet2>,
}

/// All class-based rules keyed off a single input class.
#[derive(Debug, Clone)]
pub struct ClassSet2 {
    /// Rules tried in declaration order. The first match wins.
    pub rules: Vec<ClassRule2>,
}

/// One class-id sequence + its nested lookups. The first class is
/// implicit in the enclosing `ClassSet`'s index.
#[derive(Debug, Clone)]
pub struct ClassRule2 {
    /// Remaining input classes (for glyphs 1..N of the input).
    pub input_classes_tail: Vec<u16>,
    /// Nested lookups to fire, relative to the matched input window.
    pub lookups: Vec<SequenceLookupRecord>,
}

impl<'a> Context2<'a> {
    /// Parses a format-2 contextual subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 2 {
            return Err(Error::Malformed {
                offset: 0,
                context: "context format mismatch (expected 2)",
            });
        }
        let coverage_off = r.read_u16()? as usize;
        let class_def_off = r.read_u16()? as usize;
        let set_offs = read_offset_array(&mut r)?;

        let coverage = Coverage::parse(data.get(coverage_off..).ok_or(Error::Malformed {
            offset: coverage_off,
            context: "context format 2 coverage offset past end",
        })?)?;
        let class_def = ClassDef::parse_at(
            data,
            class_def_off,
            "context format 2 classDef offset past end",
        )?;

        let mut budget = RuleBudget::for_table(data);
        let (set_slots, sets) = parse_shared_sets(
            data,
            &set_offs,
            "context format 2 classSet offset past end",
            &mut budget,
            parse_class_set2,
        )?;
        Ok(Self {
            coverage,
            class_def,
            set_slots,
            sets,
        })
    }

    /// Coverage gating the first-glyph match.
    #[must_use]
    pub const fn coverage(&self) -> &Coverage<'a> {
        &self.coverage
    }

    /// The `ClassDef` used to classify every glyph in the input.
    #[must_use]
    pub const fn class_def(&self) -> &ClassDef<'a> {
        &self.class_def
    }

    /// ClassSet for a given class index, or `None` when absent.
    #[must_use]
    pub fn class_set(&self, class_index: u16) -> Option<&ClassSet2> {
        let set = (*self.set_slots.get(class_index as usize)?)?;
        self.sets.get(set)
    }
}

fn parse_class_set2(
    full: &[u8],
    set_off: usize,
    set_bytes: &[u8],
    budget: &mut RuleBudget,
) -> Result<ClassSet2> {
    let rule_offs = read_rule_offsets(set_off, set_bytes, budget)?;
    let mut rules = Vec::with_capacity(rule_offs.len());
    for off in rule_offs {
        let (abs, bytes) = rule_bytes(full, set_off, off, "context format 2 rule offset past end")?;
        rules.push(parse_class_rule2(bytes, abs, budget)?);
    }
    Ok(ClassSet2 { rules })
}

fn parse_class_rule2(data: &[u8], abs: usize, budget: &mut RuleBudget) -> Result<ClassRule2> {
    let mut r = Reader::new(data);
    let glyph_count = r.read_u16()? as usize;
    let lookup_count = r.read_u16()?;
    let tail_len = glyph_count.saturating_sub(1);
    budget.charge(abs, tail_len + 2 * usize::from(lookup_count))?;
    let input_classes_tail = read_u16_array(&mut r, tail_len)?;
    let lookups = parse_sequence_lookup_records(&mut r, lookup_count)?;
    Ok(ClassRule2 {
        input_classes_tail,
        lookups,
    })
}

// ---------------------------------------------------------------------
// Contextual format 3: coverage-based
// ---------------------------------------------------------------------

/// Format 3: coverage-based contextual rules. A single sequence of
/// coverages describes the input window; each position carries one
/// coverage to match against.
#[derive(Debug, Clone)]
pub struct Context3<'a> {
    input: Vec<Coverage<'a>>,
    lookups: Vec<SequenceLookupRecord>,
}

impl<'a> Context3<'a> {
    /// Parses a format-3 contextual subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 3 {
            return Err(Error::Malformed {
                offset: 0,
                context: "context format mismatch (expected 3)",
            });
        }
        let glyph_count = r.read_u16()? as usize;
        let lookup_count = r.read_u16()?;
        let mut cov_offs = Vec::with_capacity(glyph_count.min(r.remaining() / 2));
        for _ in 0..glyph_count {
            cov_offs.push(r.read_u16()? as usize);
        }
        let lookups = parse_sequence_lookup_records(&mut r, lookup_count)?;
        let mut input = Vec::with_capacity(cov_offs.len());
        for off in cov_offs {
            let bytes = data.get(off..).ok_or(Error::Malformed {
                offset: off,
                context: "context format 3 coverage offset past end",
            })?;
            input.push(Coverage::parse(bytes)?);
        }
        Ok(Self { input, lookups })
    }

    /// Coverage tables for each input position.
    #[must_use]
    pub fn input(&self) -> &[Coverage<'a>] {
        &self.input
    }

    /// Nested lookups to fire on match.
    #[must_use]
    pub fn lookups(&self) -> &[SequenceLookupRecord] {
        &self.lookups
    }

    /// Tests whether the run matches starting at `i`. Pass-through
    /// filter shorthand.
    #[must_use]
    pub fn matches(&self, glyphs: &[u16], i: usize) -> bool {
        self.matches_filtered(glyphs, i, &MatchFilter::none())
            .is_some()
    }

    /// Filter-aware match: returns the raw span of the match (number
    /// of glyph positions between the first matched glyph and the
    /// last matched glyph, inclusive) or `None` when the input
    /// sequence does not align with `glyphs` starting at `i`.
    #[must_use]
    pub fn matches_filtered(
        &self,
        glyphs: &[u16],
        i: usize,
        filter: &MatchFilter<'_>,
    ) -> Option<usize> {
        let Some((first, rest)) = self.input.split_first() else {
            return Some(0);
        };
        // First input glyph must be at position `i` (coverage gate:
        // the caller positioned us here on purpose; we do not skip
        // the first glyph).
        if !first.contains(*glyphs.get(i)?) {
            return None;
        }
        let mut last = i;
        let mut cursor = i + 1;
        for cov in rest {
            let pos = filter.next_unskipped(glyphs, cursor)?;
            if !cov.contains(glyphs[pos]) {
                return None;
            }
            last = pos;
            cursor = pos + 1;
        }
        Some(last - i + 1)
    }
}

// ---------------------------------------------------------------------
// Chained-context format 1: glyph-based
// ---------------------------------------------------------------------

/// Format 1: glyph-based chained-context rules. The covered first
/// input glyph gates the match; each rule carries backtrack / input
/// tail / lookahead glyph-id sequences plus nested lookups.
#[derive(Debug, Clone)]
pub struct ChainContext1<'a> {
    coverage: Coverage<'a>,
    /// Index into `sets` for each coverage index, `None` for a NULL
    /// rule set.
    set_slots: Vec<Option<usize>>,
    sets: Vec<ChainRuleSet1>,
}

/// All chained rules keyed off a single covered first input glyph.
#[derive(Debug, Clone)]
pub struct ChainRuleSet1 {
    /// Rules tried in declaration order. The first match wins.
    pub rules: Vec<ChainRule1>,
}

/// One chained glyph-id rule.
#[derive(Debug, Clone)]
pub struct ChainRule1 {
    /// Backtrack glyph ids, listed in *reverse* match order per spec:
    /// index 0 is the glyph immediately before the input.
    pub backtrack: Vec<u16>,
    /// Remaining input glyph ids after the coverage-matched first.
    pub input_tail: Vec<u16>,
    /// Lookahead glyph ids, forward order. Index 0 is the glyph
    /// immediately after the input.
    pub lookahead: Vec<u16>,
    /// Nested lookups to fire, relative to the matched input window.
    pub lookups: Vec<SequenceLookupRecord>,
}

impl<'a> ChainContext1<'a> {
    /// Parses a format-1 chained-context subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "chain context format mismatch (expected 1)",
            });
        }
        let coverage_off = r.read_u16()? as usize;
        let set_offs = read_offset_array(&mut r)?;

        let coverage = Coverage::parse(data.get(coverage_off..).ok_or(Error::Malformed {
            offset: coverage_off,
            context: "chain context format 1 coverage offset past end",
        })?)?;

        let mut budget = RuleBudget::for_table(data);
        let (set_slots, sets) = parse_shared_sets(
            data,
            &set_offs,
            "chain context format 1 ruleSet offset past end",
            &mut budget,
            parse_chain_rule_set1,
        )?;
        Ok(Self {
            coverage,
            set_slots,
            sets,
        })
    }

    /// Coverage gating the first-glyph match.
    #[must_use]
    pub const fn coverage(&self) -> &Coverage<'a> {
        &self.coverage
    }

    /// Ruleset at a given coverage index.
    #[must_use]
    pub fn rule_set(&self, coverage_index: u16) -> Option<&ChainRuleSet1> {
        let set = (*self.set_slots.get(coverage_index as usize)?)?;
        self.sets.get(set)
    }
}

fn parse_chain_rule_set1(
    full: &[u8],
    set_off: usize,
    set_bytes: &[u8],
    budget: &mut RuleBudget,
) -> Result<ChainRuleSet1> {
    let rule_offs = read_rule_offsets(set_off, set_bytes, budget)?;
    let mut rules = Vec::with_capacity(rule_offs.len());
    for off in rule_offs {
        let (abs, bytes) = rule_bytes(
            full,
            set_off,
            off,
            "chain context format 1 rule offset past end",
        )?;
        rules.push(parse_chain_rule1(bytes, abs, budget)?);
    }
    Ok(ChainRuleSet1 { rules })
}

/// The four arrays every chained format 1 or format 2 rule carries.
struct ChainRuleArrays {
    backtrack: Vec<u16>,
    input_tail: Vec<u16>,
    lookahead: Vec<u16>,
    lookups: Vec<SequenceLookupRecord>,
}

/// Reads backtrack, input tail, lookahead, and nested lookups for one
/// chained rule. Each array is charged against `budget` before it is
/// allocated.
fn parse_chain_rule_arrays(
    data: &[u8],
    abs: usize,
    budget: &mut RuleBudget,
) -> Result<ChainRuleArrays> {
    let mut r = Reader::new(data);
    let bt_count = r.read_u16()? as usize;
    budget.charge(abs, bt_count)?;
    let backtrack = read_u16_array(&mut r, bt_count)?;
    let input_count = r.read_u16()? as usize;
    let tail_len = input_count.saturating_sub(1);
    budget.charge(abs, tail_len)?;
    let input_tail = read_u16_array(&mut r, tail_len)?;
    let la_count = r.read_u16()? as usize;
    budget.charge(abs, la_count)?;
    let lookahead = read_u16_array(&mut r, la_count)?;
    let lookup_count = r.read_u16()?;
    budget.charge(abs, 2 * usize::from(lookup_count))?;
    let lookups = parse_sequence_lookup_records(&mut r, lookup_count)?;
    Ok(ChainRuleArrays {
        backtrack,
        input_tail,
        lookahead,
        lookups,
    })
}

fn parse_chain_rule1(data: &[u8], abs: usize, budget: &mut RuleBudget) -> Result<ChainRule1> {
    let arrays = parse_chain_rule_arrays(data, abs, budget)?;
    Ok(ChainRule1 {
        backtrack: arrays.backtrack,
        input_tail: arrays.input_tail,
        lookahead: arrays.lookahead,
        lookups: arrays.lookups,
    })
}

// ---------------------------------------------------------------------
// Chained-context format 2: class-based
// ---------------------------------------------------------------------

/// Format 2: class-based chained-context rules. Three `ClassDef`s
/// cover backtrack / input / lookahead classes; a `ClassSet` is
/// selected by the first input glyph's class.
#[derive(Debug, Clone)]
pub struct ChainContext2<'a> {
    coverage: Coverage<'a>,
    backtrack_class: ClassDef<'a>,
    input_class: ClassDef<'a>,
    lookahead_class: ClassDef<'a>,
    /// Index into `sets` for each input class, `None` for a NULL
    /// class set.
    set_slots: Vec<Option<usize>>,
    sets: Vec<ChainClassSet2>,
}

/// All chained class-based rules keyed off a single input class.
#[derive(Debug, Clone)]
pub struct ChainClassSet2 {
    /// Rules tried in declaration order. The first match wins.
    pub rules: Vec<ChainClassRule2>,
}

/// One chained class-based rule.
#[derive(Debug, Clone)]
pub struct ChainClassRule2 {
    /// Backtrack classes (reverse-order per spec).
    pub backtrack: Vec<u16>,
    /// Remaining input classes (glyphs 1..N).
    pub input_classes_tail: Vec<u16>,
    /// Lookahead classes (forward order).
    pub lookahead: Vec<u16>,
    /// Nested lookups to fire on match.
    pub lookups: Vec<SequenceLookupRecord>,
}

impl<'a> ChainContext2<'a> {
    /// Parses a format-2 chained-context subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 2 {
            return Err(Error::Malformed {
                offset: 0,
                context: "chain context format mismatch (expected 2)",
            });
        }
        let coverage_off = r.read_u16()? as usize;
        let bt_cd_off = r.read_u16()? as usize;
        let in_cd_off = r.read_u16()? as usize;
        let la_cd_off = r.read_u16()? as usize;
        let set_offs = read_offset_array(&mut r)?;

        let coverage = Coverage::parse(data.get(coverage_off..).ok_or(Error::Malformed {
            offset: coverage_off,
            context: "chain context format 2 coverage offset past end",
        })?)?;
        let backtrack_class = ClassDef::parse_at(
            data,
            bt_cd_off,
            "chain context format 2 backtrackClassDef offset past end",
        )?;
        let input_class = ClassDef::parse_at(
            data,
            in_cd_off,
            "chain context format 2 inputClassDef offset past end",
        )?;
        let lookahead_class = ClassDef::parse_at(
            data,
            la_cd_off,
            "chain context format 2 lookaheadClassDef offset past end",
        )?;

        let mut budget = RuleBudget::for_table(data);
        let (set_slots, sets) = parse_shared_sets(
            data,
            &set_offs,
            "chain context format 2 classSet offset past end",
            &mut budget,
            parse_chain_class_set2,
        )?;
        Ok(Self {
            coverage,
            backtrack_class,
            input_class,
            lookahead_class,
            set_slots,
            sets,
        })
    }

    /// Coverage gating the first-glyph match.
    #[must_use]
    pub const fn coverage(&self) -> &Coverage<'a> {
        &self.coverage
    }

    /// Backtrack `ClassDef`.
    #[must_use]
    pub const fn backtrack_class(&self) -> &ClassDef<'a> {
        &self.backtrack_class
    }

    /// Input `ClassDef`.
    #[must_use]
    pub const fn input_class(&self) -> &ClassDef<'a> {
        &self.input_class
    }

    /// Lookahead `ClassDef`.
    #[must_use]
    pub const fn lookahead_class(&self) -> &ClassDef<'a> {
        &self.lookahead_class
    }

    /// ClassSet at a given class index.
    #[must_use]
    pub fn class_set(&self, class_index: u16) -> Option<&ChainClassSet2> {
        let set = (*self.set_slots.get(class_index as usize)?)?;
        self.sets.get(set)
    }
}

fn parse_chain_class_set2(
    full: &[u8],
    set_off: usize,
    set_bytes: &[u8],
    budget: &mut RuleBudget,
) -> Result<ChainClassSet2> {
    let rule_offs = read_rule_offsets(set_off, set_bytes, budget)?;
    let mut rules = Vec::with_capacity(rule_offs.len());
    for off in rule_offs {
        let (abs, bytes) = rule_bytes(
            full,
            set_off,
            off,
            "chain context format 2 rule offset past end",
        )?;
        rules.push(parse_chain_class_rule2(bytes, abs, budget)?);
    }
    Ok(ChainClassSet2 { rules })
}

fn parse_chain_class_rule2(
    data: &[u8],
    abs: usize,
    budget: &mut RuleBudget,
) -> Result<ChainClassRule2> {
    let arrays = parse_chain_rule_arrays(data, abs, budget)?;
    Ok(ChainClassRule2 {
        backtrack: arrays.backtrack,
        input_classes_tail: arrays.input_tail,
        lookahead: arrays.lookahead,
        lookups: arrays.lookups,
    })
}

// ---------------------------------------------------------------------
// Matchers: return (input_len, &lookups) on a successful match.
// ---------------------------------------------------------------------

impl Context1<'_> {
    /// Tries every rule in the ruleset for `glyphs[i]`. Pass-through
    /// filter shorthand, equivalent to [`Context1::matches_filtered`]
    /// with `MatchFilter::none()`.
    #[must_use]
    pub fn matches(&self, glyphs: &[u16], i: usize) -> Option<(usize, &[SequenceLookupRecord])> {
        self.matches_filtered(glyphs, i, &MatchFilter::none())
    }

    /// Filter-aware match: walks the input stream via the
    /// skip-iterator semantics baked into `filter`. Returns the span
    /// (from the first input glyph to the last, inclusive) in raw
    /// glyph positions. The dispatcher uses this to advance past
    /// the whole match region, skipped glyphs included.
    #[must_use]
    pub fn matches_filtered(
        &self,
        glyphs: &[u16],
        i: usize,
        filter: &MatchFilter<'_>,
    ) -> Option<(usize, &[SequenceLookupRecord])> {
        let first = *glyphs.get(i)?;
        let cov_i = self.coverage.index_of(first)?;
        let set = self.rule_set(cov_i)?;
        'rules: for rule in &set.rules {
            let mut last = i;
            let mut cursor = i + 1;
            for &g in &rule.input_tail {
                let Some(pos) = filter.next_unskipped(glyphs, cursor) else {
                    continue 'rules;
                };
                if glyphs[pos] != g {
                    continue 'rules;
                }
                last = pos;
                cursor = pos + 1;
            }
            return Some((last - i + 1, &rule.lookups));
        }
        None
    }
}

impl Context2<'_> {
    /// Tries every class rule in the classset for `glyphs[i]`'s class.
    /// Pass-through filter shorthand.
    #[must_use]
    pub fn matches(&self, glyphs: &[u16], i: usize) -> Option<(usize, &[SequenceLookupRecord])> {
        self.matches_filtered(glyphs, i, &MatchFilter::none())
    }

    /// Filter-aware match. See [`Context1::matches_filtered`] for
    /// the input-span convention.
    #[must_use]
    pub fn matches_filtered(
        &self,
        glyphs: &[u16],
        i: usize,
        filter: &MatchFilter<'_>,
    ) -> Option<(usize, &[SequenceLookupRecord])> {
        let first = *glyphs.get(i)?;
        // Coverage gates matching. Format 2 stores the coverage for
        // every first glyph that appears in *any* class rule; a glyph
        // outside coverage cannot start a rule regardless of its class.
        self.coverage.index_of(first)?;
        let cls = self.class_def.class_of(first);
        let set = self.class_set(cls)?;
        'rules: for rule in &set.rules {
            let mut last = i;
            let mut cursor = i + 1;
            for &c in &rule.input_classes_tail {
                let Some(pos) = filter.next_unskipped(glyphs, cursor) else {
                    continue 'rules;
                };
                if self.class_def.class_of(glyphs[pos]) != c {
                    continue 'rules;
                }
                last = pos;
                cursor = pos + 1;
            }
            return Some((last - i + 1, &rule.lookups));
        }
        None
    }
}

impl ChainContext1<'_> {
    /// Tries every rule in the ruleset for `glyphs[i]`. Pass-through
    /// filter shorthand.
    #[must_use]
    pub fn matches(&self, glyphs: &[u16], i: usize) -> Option<(usize, &[SequenceLookupRecord])> {
        self.matches_filtered(glyphs, i, &MatchFilter::none())
    }

    /// Filter-aware match.
    #[must_use]
    pub fn matches_filtered(
        &self,
        glyphs: &[u16],
        i: usize,
        filter: &MatchFilter<'_>,
    ) -> Option<(usize, &[SequenceLookupRecord])> {
        let first = *glyphs.get(i)?;
        let cov_i = self.coverage.index_of(first)?;
        let set = self.rule_set(cov_i)?;
        'rules: for rule in &set.rules {
            // Backtrack: walk left from `i` via prev_unskipped, one
            // entry per backtrack step.
            let mut bt_cursor = i;
            for &g in &rule.backtrack {
                let Some(pos) = filter.prev_unskipped(glyphs, bt_cursor) else {
                    continue 'rules;
                };
                if glyphs[pos] != g {
                    continue 'rules;
                }
                bt_cursor = pos;
            }
            // Input tail.
            let mut last = i;
            let mut cursor = i + 1;
            for &g in &rule.input_tail {
                let Some(pos) = filter.next_unskipped(glyphs, cursor) else {
                    continue 'rules;
                };
                if glyphs[pos] != g {
                    continue 'rules;
                }
                last = pos;
                cursor = pos + 1;
            }
            // Lookahead: walk right from `last+1`.
            let mut la_cursor = last + 1;
            for &g in &rule.lookahead {
                let Some(pos) = filter.next_unskipped(glyphs, la_cursor) else {
                    continue 'rules;
                };
                if glyphs[pos] != g {
                    continue 'rules;
                }
                la_cursor = pos + 1;
            }
            return Some((last - i + 1, &rule.lookups));
        }
        None
    }
}

impl ChainContext2<'_> {
    /// Tries every class rule in the classset for `glyphs[i]`'s input class.
    /// Pass-through filter shorthand.
    #[must_use]
    pub fn matches(&self, glyphs: &[u16], i: usize) -> Option<(usize, &[SequenceLookupRecord])> {
        self.matches_filtered(glyphs, i, &MatchFilter::none())
    }

    /// Filter-aware match.
    #[must_use]
    pub fn matches_filtered(
        &self,
        glyphs: &[u16],
        i: usize,
        filter: &MatchFilter<'_>,
    ) -> Option<(usize, &[SequenceLookupRecord])> {
        let first = *glyphs.get(i)?;
        self.coverage.index_of(first)?;
        let cls = self.input_class.class_of(first);
        let set = self.class_set(cls)?;
        'rules: for rule in &set.rules {
            // Backtrack classes.
            let mut bt_cursor = i;
            for &c in &rule.backtrack {
                let Some(pos) = filter.prev_unskipped(glyphs, bt_cursor) else {
                    continue 'rules;
                };
                if self.backtrack_class.class_of(glyphs[pos]) != c {
                    continue 'rules;
                }
                bt_cursor = pos;
            }
            let mut last = i;
            let mut cursor = i + 1;
            for &c in &rule.input_classes_tail {
                let Some(pos) = filter.next_unskipped(glyphs, cursor) else {
                    continue 'rules;
                };
                if self.input_class.class_of(glyphs[pos]) != c {
                    continue 'rules;
                }
                last = pos;
                cursor = pos + 1;
            }
            let mut la_cursor = last + 1;
            for &c in &rule.lookahead {
                let Some(pos) = filter.next_unskipped(glyphs, la_cursor) else {
                    continue 'rules;
                };
                if self.lookahead_class.class_of(glyphs[pos]) != c {
                    continue 'rules;
                }
                la_cursor = pos + 1;
            }
            return Some((last - i + 1, &rule.lookups));
        }
        None
    }
}

// ---------------------------------------------------------------------
// Chained-context format 3: coverage-based, GSUB/GPOS agnostic
// ---------------------------------------------------------------------

/// Format 3: coverage-based chained-context rules. Three parallel
/// coverage arrays cover backtrack / input / lookahead.
#[derive(Debug, Clone)]
pub struct ChainContext3<'a> {
    backtrack: Vec<Coverage<'a>>,
    input: Vec<Coverage<'a>>,
    lookahead: Vec<Coverage<'a>>,
    lookups: Vec<SequenceLookupRecord>,
}

impl<'a> ChainContext3<'a> {
    /// Parses a format-3 chained-context subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 3 {
            return Err(Error::Malformed {
                offset: 0,
                context: "chain context format mismatch (expected 3)",
            });
        }
        let backtrack = parse_coverage_array(data, &mut r)?;
        let input = parse_coverage_array(data, &mut r)?;
        let lookahead = parse_coverage_array(data, &mut r)?;
        let lookup_count = r.read_u16()?;
        let lookups = parse_sequence_lookup_records(&mut r, lookup_count)?;
        Ok(Self {
            backtrack,
            input,
            lookahead,
            lookups,
        })
    }

    /// Window widths (backtrack, input, lookahead).
    #[must_use]
    pub fn context_len(&self) -> (usize, usize, usize) {
        (self.backtrack.len(), self.input.len(), self.lookahead.len())
    }

    /// Nested lookups to fire on match.
    #[must_use]
    pub fn lookups(&self) -> &[SequenceLookupRecord] {
        &self.lookups
    }

    /// Tests whether the run matches starting at `i`. Pass-through
    /// filter shorthand.
    #[must_use]
    pub fn matches(&self, glyphs: &[u16], i: usize) -> bool {
        self.matches_filtered(glyphs, i, &MatchFilter::none())
            .is_some()
    }

    /// Filter-aware match: returns the raw span of the input match
    /// (first to last matched glyph, inclusive) or `None` when any
    /// of the backtrack / input / lookahead coverages fail.
    #[must_use]
    pub fn matches_filtered(
        &self,
        glyphs: &[u16],
        i: usize,
        filter: &MatchFilter<'_>,
    ) -> Option<usize> {
        // Cheapest test first: input[0] must match the cursor glyph.
        // Most cursor positions fail here, so checking before walking
        // backtrack avoids `prev_unskipped` work we'd throw away.
        if self.input.is_empty() {
            // Spec-wise empty input is degenerate; report a zero-span
            // match so the caller can still advance by one.
            return Some(0);
        }
        if !self.input[0].contains(*glyphs.get(i)?) {
            return None;
        }
        let mut last = i;
        let mut cursor = i + 1;
        for cov in &self.input[1..] {
            let pos = filter.next_unskipped(glyphs, cursor)?;
            if !cov.contains(glyphs[pos]) {
                return None;
            }
            last = pos;
            cursor = pos + 1;
        }
        // Backtrack: walk left from `i`.
        let mut bt_cursor = i;
        for cov in &self.backtrack {
            let pos = filter.prev_unskipped(glyphs, bt_cursor)?;
            if !cov.contains(glyphs[pos]) {
                return None;
            }
            bt_cursor = pos;
        }
        // Lookahead: walk right from last+1.
        let mut la_cursor = last + 1;
        for cov in &self.lookahead {
            let pos = filter.next_unskipped(glyphs, la_cursor)?;
            if !cov.contains(glyphs[pos]) {
                return None;
            }
            la_cursor = pos + 1;
        }
        Some(last - i + 1)
    }
}

fn parse_coverage_array<'a>(data: &'a [u8], r: &mut Reader<'_>) -> Result<Vec<Coverage<'a>>> {
    let count = r.read_u16()? as usize;
    let mut out = Vec::with_capacity(count.min(r.remaining() / 2));
    for _ in 0..count {
        let off = r.read_u16()? as usize;
        let bytes = data.get(off..).ok_or(Error::Malformed {
            offset: off,
            context: "chain context coverage offset past end",
        })?;
        out.push(Coverage::parse(bytes)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_coverage_format1(glyphs: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
        for g in glyphs {
            out.extend_from_slice(&g.to_be_bytes());
        }
        out
    }

    fn build_classdef_format2(ranges: &[(u16, u16, u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes());
        out.extend_from_slice(&(ranges.len() as u16).to_be_bytes());
        for (s, e, c) in ranges {
            out.extend_from_slice(&s.to_be_bytes());
            out.extend_from_slice(&e.to_be_bytes());
            out.extend_from_slice(&c.to_be_bytes());
        }
        out
    }

    // ------------------------- Context format 1 -------------------------
    //
    // Layout after the format/coverage/ruleSetCount header (offsets are
    // absolute from subtable start):
    //   coverage at C
    //   ruleSet[i] at S_i; each ruleSet has ruleCount + rule offsets
    //     relative to the ruleSet
    //   each Rule is { glyphCount, lookupCount, input_tail..., records... }

    #[test]
    fn context1_parses_and_matches_a_rule() {
        // Cover glyph 10. One ruleset with one rule: input [10, 20, 30],
        // lookup (seq=1, lk=7).
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        out.extend_from_slice(&0u16.to_be_bytes()); // coverage offset slot
        out.extend_from_slice(&1u16.to_be_bytes()); // rule set count
        out.extend_from_slice(&0u16.to_be_bytes()); // rule set offset slot

        let coverage_off_slot = 2;
        let rule_set_off_slot = 6;

        // Append the ruleset at current length.
        let rule_set_off = out.len();
        out.extend_from_slice(&1u16.to_be_bytes()); // rule count
        out.extend_from_slice(&0u16.to_be_bytes()); // rule offset slot
        let rule_off_slot = rule_set_off + 2;

        // Rule lives immediately after the rule table.
        let rule_off_rel = out.len() - rule_set_off;
        out.extend_from_slice(&3u16.to_be_bytes()); // glyphCount
        out.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
        out.extend_from_slice(&20u16.to_be_bytes()); // tail[0]
        out.extend_from_slice(&30u16.to_be_bytes()); // tail[1]
        out.extend_from_slice(&1u16.to_be_bytes()); // seq
        out.extend_from_slice(&7u16.to_be_bytes()); // lookup index

        // Append coverage and patch slots.
        let cov_off = out.len();
        out.extend_from_slice(&build_coverage_format1(&[10]));
        out[coverage_off_slot..coverage_off_slot + 2]
            .copy_from_slice(&(cov_off as u16).to_be_bytes());
        out[rule_set_off_slot..rule_set_off_slot + 2]
            .copy_from_slice(&(rule_set_off as u16).to_be_bytes());
        out[rule_off_slot..rule_off_slot + 2].copy_from_slice(&(rule_off_rel as u16).to_be_bytes());

        let ctx = Context1::parse(&out).unwrap();
        let (n, lookups) = ctx.matches(&[10, 20, 30, 99], 0).unwrap();
        assert_eq!(n, 3);
        assert_eq!(lookups.len(), 1);
        assert_eq!(lookups[0].sequence_index, 1);
        assert_eq!(lookups[0].lookup_list_index, 7);

        // First glyph uncovered: no match.
        assert!(ctx.matches(&[11, 20, 30], 0).is_none());
        // Input tail mismatch.
        assert!(ctx.matches(&[10, 21, 30], 0).is_none());
    }

    #[test]
    fn context2_class_based_matches_and_rejects() {
        // Coverage: glyphs 10..=11 (both class 1).
        // Class def: 10->1, 11->1, 20..=29 -> 2.
        // One ClassSet at index 1 with one rule: input classes [1, 2],
        //   lookup (seq=0, lk=5).
        //
        // Manual layout:
        //   u16 format=2
        //   u16 coverageOffset
        //   u16 classDefOffset
        //   u16 classSetCount
        //   u16 classSetOffsets[count]
        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes()); // format
        out.extend_from_slice(&0u16.to_be_bytes()); // cov slot
        out.extend_from_slice(&0u16.to_be_bytes()); // classDef slot
        out.extend_from_slice(&2u16.to_be_bytes()); // classSetCount (index 0 NULL, index 1 real)
        out.extend_from_slice(&0u16.to_be_bytes()); // set[0] (NULL)
        out.extend_from_slice(&0u16.to_be_bytes()); // set[1] slot

        let cov_slot = 2;
        let cd_slot = 4;
        let set1_slot = 10;

        // ClassSet 1:
        let set_off = out.len();
        out.extend_from_slice(&1u16.to_be_bytes()); // ruleCount
        out.extend_from_slice(&0u16.to_be_bytes()); // rule slot

        // Rule:
        let rule_off_rel = out.len() - set_off;
        out.extend_from_slice(&2u16.to_be_bytes()); // glyphCount
        out.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
        out.extend_from_slice(&2u16.to_be_bytes()); // class tail
        out.extend_from_slice(&0u16.to_be_bytes()); // seq
        out.extend_from_slice(&5u16.to_be_bytes()); // lookup idx

        // Patch the rule offset inside the classset.
        out[set_off + 2..set_off + 4].copy_from_slice(&(rule_off_rel as u16).to_be_bytes());
        out[set1_slot..set1_slot + 2].copy_from_slice(&(set_off as u16).to_be_bytes());

        // Coverage.
        let cov_off = out.len();
        out.extend_from_slice(&build_coverage_format1(&[10, 11]));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());

        // ClassDef (format 2).
        let cd_off = out.len();
        out.extend_from_slice(&build_classdef_format2(&[(10, 11, 1), (20, 29, 2)]));
        out[cd_slot..cd_slot + 2].copy_from_slice(&(cd_off as u16).to_be_bytes());

        let ctx = Context2::parse(&out).unwrap();
        let (n, lookups) = ctx.matches(&[10, 25], 0).unwrap();
        assert_eq!(n, 2);
        assert_eq!(lookups[0].lookup_list_index, 5);
        // Mismatched class for second slot.
        assert!(ctx.matches(&[10, 40], 0).is_none());
        // Uncovered first glyph.
        assert!(ctx.matches(&[12, 25], 0).is_none());
    }

    #[test]
    fn context3_matches_coverage_input() {
        // Input: [cov{5,6}, cov{7}]. On match fire (seq=0, lk=3).
        let mut out = Vec::new();
        out.extend_from_slice(&3u16.to_be_bytes()); // format
        out.extend_from_slice(&2u16.to_be_bytes()); // glyphCount
        out.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
        out.extend_from_slice(&0u16.to_be_bytes()); // cov[0] slot
        out.extend_from_slice(&0u16.to_be_bytes()); // cov[1] slot
        out.extend_from_slice(&0u16.to_be_bytes()); // seq
        out.extend_from_slice(&3u16.to_be_bytes()); // lookup idx
        let cov0_slot = 6;
        let cov1_slot = 8;
        let cov0_off = out.len();
        out.extend_from_slice(&build_coverage_format1(&[5, 6]));
        let cov1_off = out.len();
        out.extend_from_slice(&build_coverage_format1(&[7]));
        out[cov0_slot..cov0_slot + 2].copy_from_slice(&(cov0_off as u16).to_be_bytes());
        out[cov1_slot..cov1_slot + 2].copy_from_slice(&(cov1_off as u16).to_be_bytes());

        let ctx = Context3::parse(&out).unwrap();
        assert!(ctx.matches(&[5, 7], 0));
        assert!(ctx.matches(&[6, 7], 0));
        assert!(!ctx.matches(&[5, 8], 0));
        assert_eq!(ctx.lookups()[0].lookup_list_index, 3);
    }

    #[test]
    fn chain_context3_walks_backtrack_input_lookahead() {
        // Backtrack: [cov{10}], input: [cov{20}], lookahead: [cov{30}].
        let mut out = Vec::new();
        out.extend_from_slice(&3u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // bt count
        out.extend_from_slice(&0u16.to_be_bytes()); // bt slot
        out.extend_from_slice(&1u16.to_be_bytes()); // in count
        out.extend_from_slice(&0u16.to_be_bytes()); // in slot
        out.extend_from_slice(&1u16.to_be_bytes()); // la count
        out.extend_from_slice(&0u16.to_be_bytes()); // la slot
        out.extend_from_slice(&1u16.to_be_bytes()); // lookup count
        out.extend_from_slice(&0u16.to_be_bytes()); // seq
        out.extend_from_slice(&1u16.to_be_bytes()); // lookup idx
        let bt_slot = 4;
        let in_slot = 8;
        let la_slot = 12;
        let bt_off = out.len();
        out.extend_from_slice(&build_coverage_format1(&[10]));
        let in_off = out.len();
        out.extend_from_slice(&build_coverage_format1(&[20]));
        let la_off = out.len();
        out.extend_from_slice(&build_coverage_format1(&[30]));
        out[bt_slot..bt_slot + 2].copy_from_slice(&(bt_off as u16).to_be_bytes());
        out[in_slot..in_slot + 2].copy_from_slice(&(in_off as u16).to_be_bytes());
        out[la_slot..la_slot + 2].copy_from_slice(&(la_off as u16).to_be_bytes());

        let ctx = ChainContext3::parse(&out).unwrap();
        assert_eq!(ctx.context_len(), (1, 1, 1));
        assert!(ctx.matches(&[10, 20, 30], 1));
        assert!(!ctx.matches(&[11, 20, 30], 1));
        assert!(!ctx.matches(&[10, 21, 30], 1));
        assert!(!ctx.matches(&[10, 20, 31], 1));
    }

    #[test]
    fn chain_context1_glyph_based_matches() {
        // Coverage: {10}. Ruleset with one rule:
        //   backtrack=[5], input_tail=[20], lookahead=[30], lookup=(0,4).
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        out.extend_from_slice(&0u16.to_be_bytes()); // cov slot
        out.extend_from_slice(&1u16.to_be_bytes()); // ruleset count
        out.extend_from_slice(&0u16.to_be_bytes()); // set slot

        let cov_slot = 2;
        let set_slot = 6;

        let set_off = out.len();
        out.extend_from_slice(&1u16.to_be_bytes()); // rule count
        out.extend_from_slice(&0u16.to_be_bytes()); // rule slot

        let rule_off_rel = out.len() - set_off;
        out.extend_from_slice(&1u16.to_be_bytes()); // bt count
        out.extend_from_slice(&5u16.to_be_bytes()); // bt[0]
        out.extend_from_slice(&2u16.to_be_bytes()); // input count (2 => tail of 1)
        out.extend_from_slice(&20u16.to_be_bytes()); // input tail
        out.extend_from_slice(&1u16.to_be_bytes()); // la count
        out.extend_from_slice(&30u16.to_be_bytes()); // la[0]
        out.extend_from_slice(&1u16.to_be_bytes()); // lookup count
        out.extend_from_slice(&0u16.to_be_bytes()); // seq
        out.extend_from_slice(&4u16.to_be_bytes()); // lookup idx

        out[set_off + 2..set_off + 4].copy_from_slice(&(rule_off_rel as u16).to_be_bytes());
        out[set_slot..set_slot + 2].copy_from_slice(&(set_off as u16).to_be_bytes());

        let cov_off = out.len();
        out.extend_from_slice(&build_coverage_format1(&[10]));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());

        let ctx = ChainContext1::parse(&out).unwrap();
        let (n, lookups) = ctx.matches(&[5, 10, 20, 30], 1).unwrap();
        assert_eq!(n, 2);
        assert_eq!(lookups[0].lookup_list_index, 4);
        assert!(ctx.matches(&[99, 10, 20, 30], 1).is_none()); // bt mismatch
        assert!(ctx.matches(&[5, 10, 21, 30], 1).is_none()); // input mismatch
        assert!(ctx.matches(&[5, 10, 20, 31], 1).is_none()); // la mismatch
    }

    #[test]
    fn chain_context2_class_based_matches() {
        // Coverage: {10, 11}. bt/input/la all share one class def for
        // simplicity.
        //   cd: 5->1, 10->2, 11->2, 20->3, 30->4.
        // Ruleset for class 2 (input class of 10) contains:
        //   bt=[1], input_tail=[3], la=[4], lookup=(0,9).
        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes()); // format
        out.extend_from_slice(&0u16.to_be_bytes()); // cov slot
        out.extend_from_slice(&0u16.to_be_bytes()); // bt cd slot
        out.extend_from_slice(&0u16.to_be_bytes()); // in cd slot
        out.extend_from_slice(&0u16.to_be_bytes()); // la cd slot
        out.extend_from_slice(&3u16.to_be_bytes()); // set count
        out.extend_from_slice(&0u16.to_be_bytes()); // set[0]
        out.extend_from_slice(&0u16.to_be_bytes()); // set[1]
        out.extend_from_slice(&0u16.to_be_bytes()); // set[2]
        let cov_slot = 2;
        let bt_cd_slot = 4;
        let in_cd_slot = 6;
        let la_cd_slot = 8;
        // Header is 12 bytes (format..setCount), set[0] at 12, set[1] at 14, set[2] at 16.
        let set2_slot = 16;

        // ClassSet at input-class=2.
        let set_off = out.len();
        out.extend_from_slice(&1u16.to_be_bytes()); // rule count
        out.extend_from_slice(&0u16.to_be_bytes()); // rule slot
        let rule_off_rel = out.len() - set_off;
        out.extend_from_slice(&1u16.to_be_bytes()); // bt count
        out.extend_from_slice(&1u16.to_be_bytes()); // bt class
        out.extend_from_slice(&2u16.to_be_bytes()); // input count
        out.extend_from_slice(&3u16.to_be_bytes()); // input tail class
        out.extend_from_slice(&1u16.to_be_bytes()); // la count
        out.extend_from_slice(&4u16.to_be_bytes()); // la class
        out.extend_from_slice(&1u16.to_be_bytes()); // lookup count
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&9u16.to_be_bytes());
        out[set_off + 2..set_off + 4].copy_from_slice(&(rule_off_rel as u16).to_be_bytes());
        out[set2_slot..set2_slot + 2].copy_from_slice(&(set_off as u16).to_be_bytes());

        // Coverage.
        let cov_off = out.len();
        out.extend_from_slice(&build_coverage_format1(&[10, 11]));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());

        // Shared classdef.
        let cd_off = out.len();
        let cd = build_classdef_format2(&[(5, 5, 1), (10, 11, 2), (20, 20, 3), (30, 30, 4)]);
        out.extend_from_slice(&cd);
        for slot in [bt_cd_slot, in_cd_slot, la_cd_slot] {
            out[slot..slot + 2].copy_from_slice(&(cd_off as u16).to_be_bytes());
        }

        let ctx = ChainContext2::parse(&out).unwrap();
        let (n, lookups) = ctx.matches(&[5, 10, 20, 30], 1).unwrap();
        assert_eq!(n, 2);
        assert_eq!(lookups[0].lookup_list_index, 9);
        assert!(ctx.matches(&[99, 10, 20, 30], 1).is_none());
        assert!(ctx.matches(&[5, 10, 99, 30], 1).is_none());
        assert!(ctx.matches(&[5, 10, 20, 99], 1).is_none());
    }

    #[test]
    fn chain_context2_null_class_defs_put_every_glyph_in_class_zero() {
        // fontmake leaves the backtrack (and often lookahead) ClassDef
        // offset null. A null ClassDef means every glyph is class 0, as
        // in HarfBuzz; reading the subtable header as a ClassDef instead
        // either fails to parse or invents classes.
        //   input cd: 10..=11 -> 1. Rule for input class 1:
        //   bt=[0], input_tail=[], la=[0].
        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes()); // format
        out.extend_from_slice(&0u16.to_be_bytes()); // cov slot
        out.extend_from_slice(&0u16.to_be_bytes()); // bt cd: null
        out.extend_from_slice(&0u16.to_be_bytes()); // in cd slot
        out.extend_from_slice(&0u16.to_be_bytes()); // la cd: null
        out.extend_from_slice(&2u16.to_be_bytes()); // set count
        out.extend_from_slice(&0u16.to_be_bytes()); // set[0]
        out.extend_from_slice(&0u16.to_be_bytes()); // set[1]
        let (cov_slot, in_cd_slot, set1_slot) = (2, 6, 14);

        let set_off = out.len();
        out.extend_from_slice(&1u16.to_be_bytes()); // rule count
        out.extend_from_slice(&4u16.to_be_bytes()); // rule offset (from the set)
        out.extend_from_slice(&1u16.to_be_bytes()); // bt count
        out.extend_from_slice(&0u16.to_be_bytes()); // bt class 0
        out.extend_from_slice(&1u16.to_be_bytes()); // input count
        out.extend_from_slice(&1u16.to_be_bytes()); // la count
        out.extend_from_slice(&0u16.to_be_bytes()); // la class 0
        out.extend_from_slice(&0u16.to_be_bytes()); // lookup count
        out[set1_slot..set1_slot + 2].copy_from_slice(&(set_off as u16).to_be_bytes());

        let cov_off = out.len();
        out.extend_from_slice(&build_coverage_format1(&[10, 11]));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());
        let cd_off = out.len();
        out.extend_from_slice(&build_classdef_format2(&[(10, 11, 1)]));
        out[in_cd_slot..in_cd_slot + 2].copy_from_slice(&(cd_off as u16).to_be_bytes());

        let ctx = ChainContext2::parse(&out).expect("null ClassDef offsets are valid");
        assert_eq!(ctx.backtrack_class().class_of(5), 0);
        assert_eq!(ctx.lookahead_class().class_of(11), 0);
        // Any glyph satisfies a class-0 backtrack or lookahead slot,
        // including ones the input ClassDef puts in another class.
        assert!(ctx.matches(&[99, 10, 77], 1).is_some());
        assert!(ctx.matches(&[11, 10, 11], 1).is_some());
        // The context still needs a glyph on each side.
        assert!(ctx.matches(&[10, 77], 0).is_none());
    }

    /// Build a minimal GDEF where each listed glyph has the given class.
    /// ClassDef format 2 requires sorted ranges. The helper sorts
    /// the caller's (gid, class) pairs to avoid silent binary-search
    /// misses.
    fn build_gdef_with_classes(classes: &[(u16, u16)]) -> alloc::vec::Vec<u8> {
        let mut sorted: alloc::vec::Vec<(u16, u16)> = classes.to_vec();
        sorted.sort_by_key(|&(gid, _)| gid);
        let mut cd = alloc::vec::Vec::new();
        cd.extend_from_slice(&2u16.to_be_bytes());
        cd.extend_from_slice(&(sorted.len() as u16).to_be_bytes());
        for (gid, cls) in &sorted {
            cd.extend_from_slice(&gid.to_be_bytes());
            cd.extend_from_slice(&gid.to_be_bytes());
            cd.extend_from_slice(&cls.to_be_bytes());
        }
        let mut gdef = alloc::vec::Vec::new();
        gdef.extend_from_slice(&1u16.to_be_bytes()); // major
        gdef.extend_from_slice(&0u16.to_be_bytes()); // minor
        gdef.extend_from_slice(&12u16.to_be_bytes()); // glyphClassDefOff
        gdef.extend_from_slice(&[0u8; 6]);
        gdef.extend_from_slice(&cd);
        gdef
    }

    #[test]
    fn context3_filtered_matches_across_marks() {
        use crate::tables::gdef::Gdef;
        use crate::tables::layout::skip_iter::{MatchFilter, LOOKUP_FLAG_IGNORE_MARKS};

        // Input: [cov{5,6}, cov{7}]. Same encoding as the pass-through test.
        let mut out = Vec::new();
        out.extend_from_slice(&3u16.to_be_bytes()); // format
        out.extend_from_slice(&2u16.to_be_bytes()); // glyphCount
        out.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&3u16.to_be_bytes());
        let cov0_slot = 6;
        let cov1_slot = 8;
        let cov0_off = out.len();
        out.extend_from_slice(&build_coverage_format1(&[5, 6]));
        let cov1_off = out.len();
        out.extend_from_slice(&build_coverage_format1(&[7]));
        out[cov0_slot..cov0_slot + 2].copy_from_slice(&(cov0_off as u16).to_be_bytes());
        out[cov1_slot..cov1_slot + 2].copy_from_slice(&(cov1_off as u16).to_be_bytes());

        let ctx = Context3::parse(&out).unwrap();
        // 5 = base, 99 = mark, 7 = base.
        let gdef_bytes = build_gdef_with_classes(&[(5, 1), (99, 3), (7, 1)]);
        let gdef = Gdef::parse(&gdef_bytes).unwrap();
        let filter = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_MARKS, Some(&gdef), None);

        // With the filter the mark is hopped over; span covers index 0..=2.
        assert_eq!(ctx.matches_filtered(&[5, 99, 7], 0, &filter), Some(3));
        // Without the filter the plain .matches fails at the mark.
        assert!(!ctx.matches(&[5, 99, 7], 0));
    }

    #[test]
    fn chain_context3_filtered_backtrack_skips_marks() {
        use crate::tables::gdef::Gdef;
        use crate::tables::layout::skip_iter::{MatchFilter, LOOKUP_FLAG_IGNORE_MARKS};

        // bt=[cov{10}], input=[cov{20}], lookahead=[cov{30}].
        let mut out = Vec::new();
        out.extend_from_slice(&3u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // bt count
        out.extend_from_slice(&0u16.to_be_bytes()); // bt slot
        out.extend_from_slice(&1u16.to_be_bytes()); // in count
        out.extend_from_slice(&0u16.to_be_bytes()); // in slot
        out.extend_from_slice(&1u16.to_be_bytes()); // la count
        out.extend_from_slice(&0u16.to_be_bytes()); // la slot
        out.extend_from_slice(&1u16.to_be_bytes()); // lookup count
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        let bt_slot = 4;
        let in_slot = 8;
        let la_slot = 12;
        let bt_off = out.len();
        out.extend_from_slice(&build_coverage_format1(&[10]));
        let in_off = out.len();
        out.extend_from_slice(&build_coverage_format1(&[20]));
        let la_off = out.len();
        out.extend_from_slice(&build_coverage_format1(&[30]));
        out[bt_slot..bt_slot + 2].copy_from_slice(&(bt_off as u16).to_be_bytes());
        out[in_slot..in_slot + 2].copy_from_slice(&(in_off as u16).to_be_bytes());
        out[la_slot..la_slot + 2].copy_from_slice(&(la_off as u16).to_be_bytes());

        let ctx = ChainContext3::parse(&out).unwrap();
        // GDEF: 10=base, 99=mark, 20=base, 30=base.
        let gdef_bytes = build_gdef_with_classes(&[(10, 1), (99, 3), (20, 1), (30, 1)]);
        let gdef = Gdef::parse(&gdef_bytes).unwrap();
        let filter = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_MARKS, Some(&gdef), None);

        // [10, 99, 20, 30]: plain matcher fails on the mark in backtrack (i=2).
        assert!(!ctx.matches(&[10, 99, 20, 30], 2));
        // With the filter, the mark is skipped and the match fires.
        assert_eq!(ctx.matches_filtered(&[10, 99, 20, 30], 2, &filter), Some(1));
    }

    #[test]
    fn rejects_format_mismatch() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        assert!(Context1::parse(&bytes).is_err());
        assert!(Context2::parse(&bytes).is_err());
        assert!(Context3::parse(&bytes).is_err());
        assert!(ChainContext1::parse(&bytes).is_err());
        assert!(ChainContext2::parse(&bytes).is_err());
        assert!(ChainContext3::parse(&bytes).is_err());
    }

    // ------------------------- Parse budget -------------------------

    fn push_repeated_u16(out: &mut Vec<u8>, value: u16, count: usize) {
        for _ in 0..count {
            out.extend_from_slice(&value.to_be_bytes());
        }
    }

    #[test]
    fn chain_context1_many_offsets_to_one_large_rule_hit_the_budget() {
        // One rule set with 65535 rule offsets that all point at the
        // same rule. The rule sits inside the offset array, so every
        // count it reads is R and it stores about 40000 values.
        // Copying that rule 65535 times would need gigabytes.
        const R: u16 = 10_000;
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        out.extend_from_slice(&8u16.to_be_bytes()); // coverage offset
        out.extend_from_slice(&1u16.to_be_bytes()); // rule set count
        out.extend_from_slice(&14u16.to_be_bytes()); // rule set offset
        out.extend_from_slice(&build_coverage_format1(&[5]));
        assert_eq!(out.len(), 14);
        out.extend_from_slice(&u16::MAX.to_be_bytes()); // rule count
        push_repeated_u16(&mut out, R, usize::from(u16::MAX));
        assert!(matches!(
            ChainContext1::parse(&out),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn context1_overlapping_rule_sets_hit_the_budget() {
        // 16000 rule-set offsets two bytes apart, all inside one run of
        // the u16 value V. Every set reads V rules of V glyphs and V
        // lookups. The offsets differ, so the sets cannot share one
        // parsed copy, and parsing all of them would need about 25 GB.
        const V: u16 = 512;
        const SETS: usize = 16_000;
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        let cov_off = 6 + 2 * SETS;
        let region = cov_off + 6;
        out.extend_from_slice(&(cov_off as u16).to_be_bytes());
        out.extend_from_slice(&(SETS as u16).to_be_bytes());
        for j in 0..SETS {
            out.extend_from_slice(&((region + 2 * j) as u16).to_be_bytes());
        }
        out.extend_from_slice(&build_coverage_format1(&[5]));
        assert_eq!(out.len(), region);
        push_repeated_u16(&mut out, V, SETS + 4 * usize::from(V));
        assert!(matches!(
            Context1::parse(&out),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn context1_slots_sharing_a_rule_set_offset_all_match() {
        // Coverage [10, 11, 12]. All three rule-set offsets point at the
        // same set, whose one rule is input [first, 20] with lookup
        // (0, 3). The shared set must answer for every coverage index.
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        out.extend_from_slice(&12u16.to_be_bytes()); // coverage offset
        out.extend_from_slice(&3u16.to_be_bytes()); // rule set count
        for _ in 0..3 {
            out.extend_from_slice(&22u16.to_be_bytes()); // shared rule set
        }
        out.extend_from_slice(&build_coverage_format1(&[10, 11, 12]));
        assert_eq!(out.len(), 22);
        out.extend_from_slice(&1u16.to_be_bytes()); // rule count
        out.extend_from_slice(&4u16.to_be_bytes()); // rule offset
        out.extend_from_slice(&2u16.to_be_bytes()); // glyphCount
        out.extend_from_slice(&1u16.to_be_bytes()); // lookupCount
        out.extend_from_slice(&20u16.to_be_bytes()); // tail[0]
        out.extend_from_slice(&0u16.to_be_bytes()); // seq
        out.extend_from_slice(&3u16.to_be_bytes()); // lookup index

        let ctx = Context1::parse(&out).unwrap();
        for first in [10u16, 11, 12] {
            let (n, lookups) = ctx.matches(&[first, 20], 0).unwrap();
            assert_eq!(n, 2);
            assert_eq!(lookups[0].lookup_list_index, 3);
        }
        assert!(ctx.matches(&[10, 21], 0).is_none());
        assert_eq!(ctx.rule_set(2).unwrap().rules.len(), 1);
        assert!(ctx.rule_set(3).is_none());
    }
}
