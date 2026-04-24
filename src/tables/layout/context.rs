//! Shared primitives for the OpenType contextual / chained-contextual
//! lookup families (GSUB type 5 & 6, GPOS type 7 & 8).
//!
//! GSUB and GPOS disagree on what the nested lookups *do* — GSUB
//! rewrites glyph ids, GPOS adjusts advances and offsets — but the
//! outer shape is identical: match a window of the glyph stream
//! against a Coverage / ClassDef / explicit-glyph pattern, and fire
//! a list of nested `(sequenceIndex, lookupListIndex)` records at
//! their declared positions inside that window.
//!
//! This module owns the parsing of the three subtable formats used
//! by both families:
//!
//! - Format 1 — glyph-based. The first input glyph drives a coverage
//!   index; each coverage entry points at a `RuleSet` of explicit
//!   glyph-id sequences.
//! - Format 2 — class-based. The first input glyph's coverage gate
//!   picks a `ClassSet`; each class set stores class-id sequences
//!   that are matched via one shared `ClassDef` (contextual) or
//!   three (backtrack / input / lookahead) for chained-context.
//! - Format 3 — explicit-coverage arrays for every window position.
//!
//! Every lookup type that uses these formats shares the
//! `SequenceLookupRecord` payload, a `(sequenceIndex, lookupListIndex)`
//! pair the caller recursively dispatches.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::layout::skip_iter::MatchFilter;
use crate::tables::layout::{ClassDef, Coverage};
use crate::tables::parse::Reader;

/// `(sequenceIndex, lookupListIndex)` — the position inside the
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

// ---------------------------------------------------------------------
// Contextual (non-chained) format 1 — glyph-based
// ---------------------------------------------------------------------

/// Format 1 — glyph-based contextual rules. The covered first glyph
/// selects a `RuleSet`; each rule inside carries the remaining input
/// glyph ids to match plus the nested lookup records.
#[derive(Debug, Clone)]
pub struct Context1<'a> {
    coverage: Coverage<'a>,
    rule_sets: Vec<Option<RuleSet1>>,
}

/// All rules keyed off a single covered first glyph.
#[derive(Debug, Clone)]
pub struct RuleSet1 {
    /// Rules tried in declaration order; first match wins.
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
        let rule_set_count = r.read_u16()? as usize;
        let mut rule_set_offs = Vec::with_capacity(rule_set_count);
        for _ in 0..rule_set_count {
            rule_set_offs.push(r.read_u16()? as usize);
        }
        let coverage = Coverage::parse(data.get(coverage_off..).ok_or(Error::Malformed {
            offset: coverage_off,
            context: "context format 1 coverage offset past end",
        })?)?;

        let mut rule_sets = Vec::with_capacity(rule_set_count);
        for off in rule_set_offs {
            if off == 0 {
                // NULL ruleset — spec permits it.
                rule_sets.push(None);
                continue;
            }
            let set_bytes = data.get(off..).ok_or(Error::Malformed {
                offset: off,
                context: "context format 1 ruleSet offset past end",
            })?;
            rule_sets.push(Some(parse_rule_set1(data, off, set_bytes)?));
        }
        Ok(Self {
            coverage,
            rule_sets,
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
        self.rule_sets.get(coverage_index as usize)?.as_ref()
    }
}

fn parse_rule_set1(full: &[u8], set_off: usize, set_bytes: &[u8]) -> Result<RuleSet1> {
    let mut r = Reader::new(set_bytes);
    let rule_count = r.read_u16()? as usize;
    let mut rule_offs = Vec::with_capacity(rule_count);
    for _ in 0..rule_count {
        rule_offs.push(r.read_u16()? as usize);
    }
    let mut rules = Vec::with_capacity(rule_count);
    for off in rule_offs {
        let abs = set_off + off;
        let bytes = full.get(abs..).ok_or(Error::Malformed {
            offset: abs,
            context: "context format 1 rule offset past end",
        })?;
        rules.push(parse_rule1(bytes)?);
    }
    Ok(RuleSet1 { rules })
}

fn parse_rule1(data: &[u8]) -> Result<Rule1> {
    let mut r = Reader::new(data);
    let glyph_count = r.read_u16()? as usize;
    let lookup_count = r.read_u16()?;
    if glyph_count == 0 {
        // Rule with zero input is meaningless; treat as no-match.
        return Ok(Rule1 {
            input_tail: Vec::new(),
            lookups: parse_sequence_lookup_records(&mut r, lookup_count)?,
        });
    }
    let mut input_tail = Vec::with_capacity(glyph_count - 1);
    for _ in 0..glyph_count - 1 {
        input_tail.push(r.read_u16()?);
    }
    let lookups = parse_sequence_lookup_records(&mut r, lookup_count)?;
    Ok(Rule1 {
        input_tail,
        lookups,
    })
}

// ---------------------------------------------------------------------
// Contextual format 2 — class-based
// ---------------------------------------------------------------------

/// Format 2 — class-based contextual rules. One shared `ClassDef`
/// defines every glyph's class; the first input glyph's coverage
/// gates matching and its class index selects a `ClassSet`.
#[derive(Debug, Clone)]
pub struct Context2<'a> {
    coverage: Coverage<'a>,
    class_def: ClassDef<'a>,
    class_sets: Vec<Option<ClassSet2>>,
}

/// All class-based rules keyed off a single input class.
#[derive(Debug, Clone)]
pub struct ClassSet2 {
    /// Rules tried in declaration order; first match wins.
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
        let class_set_count = r.read_u16()? as usize;
        let mut set_offs = Vec::with_capacity(class_set_count);
        for _ in 0..class_set_count {
            set_offs.push(r.read_u16()? as usize);
        }

        let coverage = Coverage::parse(data.get(coverage_off..).ok_or(Error::Malformed {
            offset: coverage_off,
            context: "context format 2 coverage offset past end",
        })?)?;
        let class_def = ClassDef::parse(data.get(class_def_off..).ok_or(Error::Malformed {
            offset: class_def_off,
            context: "context format 2 classDef offset past end",
        })?)?;

        let mut class_sets = Vec::with_capacity(class_set_count);
        for off in set_offs {
            if off == 0 {
                class_sets.push(None);
                continue;
            }
            let set_bytes = data.get(off..).ok_or(Error::Malformed {
                offset: off,
                context: "context format 2 classSet offset past end",
            })?;
            class_sets.push(Some(parse_class_set2(data, off, set_bytes)?));
        }
        Ok(Self {
            coverage,
            class_def,
            class_sets,
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
        self.class_sets.get(class_index as usize)?.as_ref()
    }
}

fn parse_class_set2(full: &[u8], set_off: usize, set_bytes: &[u8]) -> Result<ClassSet2> {
    let mut r = Reader::new(set_bytes);
    let rule_count = r.read_u16()? as usize;
    let mut offs = Vec::with_capacity(rule_count);
    for _ in 0..rule_count {
        offs.push(r.read_u16()? as usize);
    }
    let mut rules = Vec::with_capacity(rule_count);
    for off in offs {
        let abs = set_off + off;
        let bytes = full.get(abs..).ok_or(Error::Malformed {
            offset: abs,
            context: "context format 2 rule offset past end",
        })?;
        rules.push(parse_class_rule2(bytes)?);
    }
    Ok(ClassSet2 { rules })
}

fn parse_class_rule2(data: &[u8]) -> Result<ClassRule2> {
    let mut r = Reader::new(data);
    let glyph_count = r.read_u16()? as usize;
    let lookup_count = r.read_u16()?;
    if glyph_count == 0 {
        return Ok(ClassRule2 {
            input_classes_tail: Vec::new(),
            lookups: parse_sequence_lookup_records(&mut r, lookup_count)?,
        });
    }
    let mut input = Vec::with_capacity(glyph_count - 1);
    for _ in 0..glyph_count - 1 {
        input.push(r.read_u16()?);
    }
    let lookups = parse_sequence_lookup_records(&mut r, lookup_count)?;
    Ok(ClassRule2 {
        input_classes_tail: input,
        lookups,
    })
}

// ---------------------------------------------------------------------
// Contextual format 3 — coverage-based
// ---------------------------------------------------------------------

/// Format 3 — coverage-based contextual rules. A single sequence of
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
        let mut cov_offs = Vec::with_capacity(glyph_count);
        for _ in 0..glyph_count {
            cov_offs.push(r.read_u16()? as usize);
        }
        let lookups = parse_sequence_lookup_records(&mut r, lookup_count)?;
        let mut input = Vec::with_capacity(glyph_count);
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
        if self.input.is_empty() {
            return Some(0);
        }
        // First input glyph must be at position `i` (coverage gate —
        // the caller positioned us here deliberately; we do not skip
        // the first glyph).
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
        Some(last - i + 1)
    }
}

// ---------------------------------------------------------------------
// Chained-context format 1 — glyph-based
// ---------------------------------------------------------------------

/// Format 1 — glyph-based chained-context rules. The covered first
/// input glyph gates the match; each rule carries backtrack / input
/// tail / lookahead glyph-id sequences plus nested lookups.
#[derive(Debug, Clone)]
pub struct ChainContext1<'a> {
    coverage: Coverage<'a>,
    rule_sets: Vec<Option<ChainRuleSet1>>,
}

/// All chained rules keyed off a single covered first input glyph.
#[derive(Debug, Clone)]
pub struct ChainRuleSet1 {
    /// Rules tried in declaration order; first match wins.
    pub rules: Vec<ChainRule1>,
}

/// One chained glyph-id rule.
#[derive(Debug, Clone)]
pub struct ChainRule1 {
    /// Backtrack glyph ids, listed in *reverse* match order per spec —
    /// index 0 is the glyph immediately before the input.
    pub backtrack: Vec<u16>,
    /// Remaining input glyph ids after the coverage-matched first.
    pub input_tail: Vec<u16>,
    /// Lookahead glyph ids, forward order — index 0 is the glyph
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
        let set_count = r.read_u16()? as usize;
        let mut set_offs = Vec::with_capacity(set_count);
        for _ in 0..set_count {
            set_offs.push(r.read_u16()? as usize);
        }

        let coverage = Coverage::parse(data.get(coverage_off..).ok_or(Error::Malformed {
            offset: coverage_off,
            context: "chain context format 1 coverage offset past end",
        })?)?;

        let mut rule_sets = Vec::with_capacity(set_count);
        for off in set_offs {
            if off == 0 {
                rule_sets.push(None);
                continue;
            }
            let set_bytes = data.get(off..).ok_or(Error::Malformed {
                offset: off,
                context: "chain context format 1 ruleSet offset past end",
            })?;
            rule_sets.push(Some(parse_chain_rule_set1(data, off, set_bytes)?));
        }
        Ok(Self {
            coverage,
            rule_sets,
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
        self.rule_sets.get(coverage_index as usize)?.as_ref()
    }
}

fn parse_chain_rule_set1(full: &[u8], set_off: usize, set_bytes: &[u8]) -> Result<ChainRuleSet1> {
    let mut r = Reader::new(set_bytes);
    let rule_count = r.read_u16()? as usize;
    let mut offs = Vec::with_capacity(rule_count);
    for _ in 0..rule_count {
        offs.push(r.read_u16()? as usize);
    }
    let mut rules = Vec::with_capacity(rule_count);
    for off in offs {
        let abs = set_off + off;
        let bytes = full.get(abs..).ok_or(Error::Malformed {
            offset: abs,
            context: "chain context format 1 rule offset past end",
        })?;
        rules.push(parse_chain_rule1(bytes)?);
    }
    Ok(ChainRuleSet1 { rules })
}

fn parse_chain_rule1(data: &[u8]) -> Result<ChainRule1> {
    let mut r = Reader::new(data);
    let bt_count = r.read_u16()? as usize;
    let mut backtrack = Vec::with_capacity(bt_count);
    for _ in 0..bt_count {
        backtrack.push(r.read_u16()?);
    }
    let input_count = r.read_u16()? as usize;
    let mut input_tail = Vec::with_capacity(input_count.saturating_sub(1));
    for _ in 0..input_count.saturating_sub(1) {
        input_tail.push(r.read_u16()?);
    }
    let la_count = r.read_u16()? as usize;
    let mut lookahead = Vec::with_capacity(la_count);
    for _ in 0..la_count {
        lookahead.push(r.read_u16()?);
    }
    let lookup_count = r.read_u16()?;
    let lookups = parse_sequence_lookup_records(&mut r, lookup_count)?;
    Ok(ChainRule1 {
        backtrack,
        input_tail,
        lookahead,
        lookups,
    })
}

// ---------------------------------------------------------------------
// Chained-context format 2 — class-based
// ---------------------------------------------------------------------

/// Format 2 — class-based chained-context rules. Three `ClassDef`s
/// cover backtrack / input / lookahead classes; a `ClassSet` is
/// selected by the first input glyph's class.
#[derive(Debug, Clone)]
pub struct ChainContext2<'a> {
    coverage: Coverage<'a>,
    backtrack_class: ClassDef<'a>,
    input_class: ClassDef<'a>,
    lookahead_class: ClassDef<'a>,
    class_sets: Vec<Option<ChainClassSet2>>,
}

/// All chained class-based rules keyed off a single input class.
#[derive(Debug, Clone)]
pub struct ChainClassSet2 {
    /// Rules tried in declaration order; first match wins.
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
        let set_count = r.read_u16()? as usize;
        let mut set_offs = Vec::with_capacity(set_count);
        for _ in 0..set_count {
            set_offs.push(r.read_u16()? as usize);
        }

        let coverage = Coverage::parse(data.get(coverage_off..).ok_or(Error::Malformed {
            offset: coverage_off,
            context: "chain context format 2 coverage offset past end",
        })?)?;
        let backtrack_class = ClassDef::parse(data.get(bt_cd_off..).ok_or(Error::Malformed {
            offset: bt_cd_off,
            context: "chain context format 2 backtrackClassDef offset past end",
        })?)?;
        let input_class = ClassDef::parse(data.get(in_cd_off..).ok_or(Error::Malformed {
            offset: in_cd_off,
            context: "chain context format 2 inputClassDef offset past end",
        })?)?;
        let lookahead_class = ClassDef::parse(data.get(la_cd_off..).ok_or(Error::Malformed {
            offset: la_cd_off,
            context: "chain context format 2 lookaheadClassDef offset past end",
        })?)?;

        let mut class_sets = Vec::with_capacity(set_count);
        for off in set_offs {
            if off == 0 {
                class_sets.push(None);
                continue;
            }
            let set_bytes = data.get(off..).ok_or(Error::Malformed {
                offset: off,
                context: "chain context format 2 classSet offset past end",
            })?;
            class_sets.push(Some(parse_chain_class_set2(data, off, set_bytes)?));
        }
        Ok(Self {
            coverage,
            backtrack_class,
            input_class,
            lookahead_class,
            class_sets,
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
        self.class_sets.get(class_index as usize)?.as_ref()
    }
}

fn parse_chain_class_set2(full: &[u8], set_off: usize, set_bytes: &[u8]) -> Result<ChainClassSet2> {
    let mut r = Reader::new(set_bytes);
    let rule_count = r.read_u16()? as usize;
    let mut offs = Vec::with_capacity(rule_count);
    for _ in 0..rule_count {
        offs.push(r.read_u16()? as usize);
    }
    let mut rules = Vec::with_capacity(rule_count);
    for off in offs {
        let abs = set_off + off;
        let bytes = full.get(abs..).ok_or(Error::Malformed {
            offset: abs,
            context: "chain context format 2 rule offset past end",
        })?;
        rules.push(parse_chain_class_rule2(bytes)?);
    }
    Ok(ChainClassSet2 { rules })
}

fn parse_chain_class_rule2(data: &[u8]) -> Result<ChainClassRule2> {
    let mut r = Reader::new(data);
    let bt_count = r.read_u16()? as usize;
    let mut backtrack = Vec::with_capacity(bt_count);
    for _ in 0..bt_count {
        backtrack.push(r.read_u16()?);
    }
    let input_count = r.read_u16()? as usize;
    let mut input = Vec::with_capacity(input_count.saturating_sub(1));
    for _ in 0..input_count.saturating_sub(1) {
        input.push(r.read_u16()?);
    }
    let la_count = r.read_u16()? as usize;
    let mut lookahead = Vec::with_capacity(la_count);
    for _ in 0..la_count {
        lookahead.push(r.read_u16()?);
    }
    let lookup_count = r.read_u16()?;
    let lookups = parse_sequence_lookup_records(&mut r, lookup_count)?;
    Ok(ChainClassRule2 {
        backtrack,
        input_classes_tail: input,
        lookahead,
        lookups,
    })
}

// ---------------------------------------------------------------------
// Matchers — return (input_len, &lookups) on a successful match.
// ---------------------------------------------------------------------

impl Context1<'_> {
    /// Tries every rule in the ruleset for `glyphs[i]`. Pass-through
    /// filter shorthand — equivalent to [`Context1::matches_filtered`]
    /// with `MatchFilter::none()`.
    #[must_use]
    pub fn matches(&self, glyphs: &[u16], i: usize) -> Option<(usize, &[SequenceLookupRecord])> {
        self.matches_filtered(glyphs, i, &MatchFilter::none())
    }

    /// Filter-aware match: walks the input stream via the
    /// skip-iterator semantics baked into `filter`. Returns the span
    /// (from the first input glyph to the last, inclusive) in raw
    /// glyph positions — the dispatcher uses this to advance past
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

    /// Filter-aware match — see [`Context1::matches_filtered`] for
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
            // Backtrack — walk left from `i` via prev_unskipped, one
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
            // Lookahead — walk right from `last+1`.
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
// Chained-context format 3 — coverage-based, GSUB/GPOS agnostic
// ---------------------------------------------------------------------

/// Format 3 — coverage-based chained-context rules. Three parallel
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
        // Backtrack — walk left from `i`.
        let mut bt_cursor = i;
        for cov in &self.backtrack {
            let pos = filter.prev_unskipped(glyphs, bt_cursor)?;
            if !cov.contains(glyphs[pos]) {
                return None;
            }
            bt_cursor = pos;
        }
        // Input — first at position i, rest via next_unskipped.
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
        // Lookahead — walk right from last+1.
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
    let mut out = Vec::with_capacity(count);
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

        // First glyph uncovered — no match.
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

    /// Build a minimal GDEF where each listed glyph has the given class.
    /// ClassDef format 2 requires sorted ranges — the helper sorts
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

        // [10, 99, 20, 30] — plain matcher fails on the mark in backtrack (i=2).
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
}
