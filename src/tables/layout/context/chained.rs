//! Chained-context subtable formats 1 through 3: glyph-based,
//! class-based and coverage-based rules with backtrack and lookahead,
//! and their parsers.

use alloc::vec::Vec;

use super::{parse_sequence_lookup_records, SequenceLookupRecord};
use crate::error::{Error, Result};
use crate::tables::layout::skip_iter::MatchFilter;
use crate::tables::layout::{ClassDef, Coverage};
use crate::tables::parse::Reader;

// ---------------------------------------------------------------------
// Chained-context format 1: glyph-based
// ---------------------------------------------------------------------

/// Format 1: glyph-based chained-context rules. The covered first
/// input glyph gates the match; each rule carries backtrack / input
/// tail / lookahead glyph-id sequences plus nested lookups.
#[derive(Debug, Clone)]
pub struct ChainContext1<'a> {
    pub(super) coverage: Coverage<'a>,
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
// Chained-context format 2: class-based
// ---------------------------------------------------------------------

/// Format 2: class-based chained-context rules. Three `ClassDef`s
/// cover backtrack / input / lookahead classes; a `ClassSet` is
/// selected by the first input glyph's class.
#[derive(Debug, Clone)]
pub struct ChainContext2<'a> {
    pub(super) coverage: Coverage<'a>,
    pub(super) backtrack_class: ClassDef<'a>,
    pub(super) input_class: ClassDef<'a>,
    pub(super) lookahead_class: ClassDef<'a>,
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
