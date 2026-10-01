//! Contextual (non-chained) subtable formats 1 through 3: glyph-based,
//! class-based and coverage-based rules and their parsers.

use alloc::vec::Vec;

use super::matchers::context_rule;
use super::{
    parse_sequence_lookup_records, parse_shared_sets, read_offset_array, read_rule_offsets,
    read_u16_array, rule_bytes, RuleBudget, SequenceLookupRecord,
};
use crate::error::{Error, Result};
use crate::tables::layout::skip_iter::{
    InputMatch, MatchContext, MatchGlyph, MatchSeq, UnsafeRanges,
};
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
    pub(super) coverage: Coverage<'a>,
    pub(super) class_def: ClassDef<'a>,
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

    /// Matches the input coverages starting at `glyphs[i]`, which must
    /// be in the first coverage. `None` for an empty input sequence,
    /// which matches nothing (HarfBuzz reads its first coverage from a
    /// null offset).
    #[must_use]
    pub fn matches(
        &self,
        glyphs: &[MatchGlyph],
        i: usize,
        cx: &MatchContext<'_>,
    ) -> Option<InputMatch> {
        self.matches_in(glyphs, i, cx, &mut ())
    }

    /// [`Self::matches`] over any [`MatchSeq`], reporting unsafe
    /// ranges to `sink`.
    pub(crate) fn matches_in<S: MatchSeq + ?Sized>(
        &self,
        seq: &S,
        i: usize,
        cx: &MatchContext<'_>,
        sink: &mut impl UnsafeRanges,
    ) -> Option<InputMatch> {
        let (first, rest) = self.input.split_first()?;
        if !first.contains(seq.glyph(i)?.id) {
            return None;
        }
        context_rule(seq, i, cx, sink, rest.len(), |k, g| rest[k].contains(g))
    }
}
