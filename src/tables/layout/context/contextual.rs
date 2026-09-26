//! Contextual (non-chained) subtable formats 1 through 3: glyph-based,
//! class-based and coverage-based rules and their parsers.

use alloc::vec::Vec;

use super::{parse_sequence_lookup_records, SequenceLookupRecord};
use crate::error::{Error, Result};
use crate::tables::layout::skip_iter::MatchFilter;
use crate::tables::layout::{ClassDef, Coverage};
use crate::tables::parse::Reader;

// ---------------------------------------------------------------------
// Contextual (non-chained) format 1: glyph-based
// ---------------------------------------------------------------------

/// Format 1: glyph-based contextual rules. The covered first glyph
/// selects a `RuleSet`; each rule inside carries the remaining input
/// glyph ids to match plus the nested lookup records.
#[derive(Debug, Clone)]
pub struct Context1<'a> {
    pub(super) coverage: Coverage<'a>,
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
                // NULL ruleset: spec permits it.
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
// Contextual format 2: class-based
// ---------------------------------------------------------------------

/// Format 2: class-based contextual rules. One shared `ClassDef`
/// defines every glyph's class; the first input glyph's coverage
/// gates matching and its class index selects a `ClassSet`.
#[derive(Debug, Clone)]
pub struct Context2<'a> {
    pub(super) coverage: Coverage<'a>,
    pub(super) class_def: ClassDef<'a>,
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
        let class_def = ClassDef::parse_at(
            data,
            class_def_off,
            "context format 2 classDef offset past end",
        )?;

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
        // First input glyph must be at position `i` (coverage gate:
        // the caller positioned us here on purpose; we do not skip
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
