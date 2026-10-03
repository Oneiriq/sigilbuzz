//! OpenType `FeatureVariations`: feature substitutions a variable font
//! makes in parts of its design space.
//!
//! GSUB and GPOS 1.1 can carry a FeatureVariations table. Each of its
//! records pairs a ConditionSet with a FeatureTableSubstitution. A
//! shaper evaluates the records in order at the font's normalized
//! coordinates and takes the first record whose conditions all hold.
//! For every feature that record substitutes, the lookups come from
//! the record's alternate Feature table instead of the FeatureList's.
//! The feature keeps its FeatureList tag.
//!
//! ```text
//!   FeatureVariations
//!     u16      majorVersion = 1
//!     u16      minorVersion
//!     u32      featureVariationRecordCount
//!     FeatureVariationRecord records[featureVariationRecordCount]:
//!       Offset32 conditionSetOffset              (from FeatureVariations)
//!       Offset32 featureTableSubstitutionOffset  (from FeatureVariations)
//!
//!   ConditionSet
//!     u16      conditionCount
//!     Offset32 conditionOffsets[conditionCount]  (from ConditionSet)
//!
//!   Condition format 1, axis range
//!     u16      format = 1
//!     u16      axisIndex
//!     F2DOT14  filterRangeMinValue
//!     F2DOT14  filterRangeMaxValue
//!   Condition format 2, value
//!     u16      format = 2
//!     i16      defaultValue
//!     u32      varIndex
//!   Condition format 3 (and) and format 4 (or)
//!     u16      format
//!     u8       conditionCount
//!     Offset24 conditionOffsets[conditionCount]  (from this condition)
//!   Condition format 5, negate
//!     u16      format = 5
//!     Offset24 conditionOffset                   (from this condition)
//!
//!   FeatureTableSubstitution
//!     u16      majorVersion = 1
//!     u16      minorVersion
//!     u16      substitutionCount
//!     FeatureTableSubstitutionRecord substitutions[substitutionCount]:
//!       u16      featureIndex
//!       Offset32 alternateFeatureOffset          (from FeatureTableSubstitution)
//! ```
//!
//! # Evaluation
//!
//! The conditions follow HarfBuzz 14.5.0:
//!
//! - An axis range compares the axis coordinate, converted to F2DOT14,
//!   with its bounds, both ends included. An axis past the end of the
//!   coordinates reads as 0, so a font at its default instance (no
//!   coordinates at all) still selects the records that hold there.
//! - A value condition holds when `defaultValue` plus the delta of
//!   `varIndex` in GDEF's ItemVariationStore is above 0. The delta is 0
//!   without coordinates or for the index `0xFFFFFFFF`.
//! - An empty "and" holds; an empty "or" does not.
//! - A format other than 1 to 5 never holds.
//!
//! # Null offsets and damaged tables
//!
//! A null offset reads as HarfBuzz's empty object: a null ConditionSet
//! has no conditions, so its record matches everywhere; a null
//! condition never holds; a null FeatureTableSubstitution substitutes
//! nothing; a null alternate Feature has no lookups.
//!
//! Anything else that cannot be read fails [`FeatureVariations::parse`],
//! with the byte offset of the problem: an offset past the end of the
//! table, a subtable cut short, a FeatureVariations or
//! FeatureTableSubstitution major version other than 1, or conditions
//! nested more than 64 deep (HarfBuzz's `HB_MAX_NESTING_LEVEL`).
//! `parse` checks every record, as HarfBuzz's sanitizer does before it
//! uses the table. HarfBuzz 14.5.0 does not repair a table that fails
//! that check: it drops the whole GSUB or GPOS, and the shaper does
//! the same.
//!
//! One limit differs from HarfBuzz. `parse` makes at most
//! [`CHECK_BUDGET`] checks and rejects a table that needs more, so
//! conditions that share subtrees cannot make it take exponential
//! time. HarfBuzz instead allows 64 checks per byte of the whole GSUB
//! or GPOS.

use crate::error::{Error, Result};
use crate::tables::layout::{Feature, FeatureList};
use crate::tables::parse::Reader;
use crate::tables::variation_store::ItemVariationStore;
use alloc::vec::Vec;

/// HarfBuzz's `HB_MAX_NESTING_LEVEL`: the deepest a condition can be,
/// counting the conditions a ConditionSet names as depth 1.
pub const MAX_CONDITION_DEPTH: u8 = 64;

/// The most ConditionSets, conditions, FeatureTableSubstitutions, and
/// alternate Feature tables [`FeatureVariations::parse`] checks. A real
/// font has a few records of a few conditions each.
pub const CHECK_BUDGET: u32 = 1 << 18;

/// `VarIdx::NO_VARIATION`: a value condition with this index has no
/// delta.
const NO_VARIATION_INDEX: u32 = 0xFFFF_FFFF;

/// Size of the FeatureVariations header.
const HEADER_SIZE: usize = 8;

/// Size of a FeatureVariationRecord.
const RECORD_SIZE: usize = 8;

/// Size of the FeatureTableSubstitution header.
const SUBSTITUTION_HEADER_SIZE: usize = 6;

/// Size of a FeatureTableSubstitutionRecord.
const SUBSTITUTION_RECORD_SIZE: usize = 6;

/// A parsed `FeatureVariations` table.
#[derive(Debug, Clone, Copy)]
pub struct FeatureVariations<'a> {
    data: &'a [u8],
    record_count: u32,
}

impl<'a> FeatureVariations<'a> {
    /// Parses a `FeatureVariations` table from its raw bytes, which run
    /// to the end of the GSUB or GPOS table that holds it, and checks
    /// every subtable its records reach. Byte offsets in errors count
    /// from the start of `data`.
    ///
    /// # Errors
    ///
    /// [`Error::Truncated`] when a subtable does not fit, and
    /// [`Error::Malformed`] for a major version other than 1, conditions
    /// nested more than [`MAX_CONDITION_DEPTH`] deep, or a table that
    /// needs more than [`CHECK_BUDGET`] checks.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported FeatureVariations major version",
            });
        }
        let record_count = r.read_u32()?;
        let fits = usize::try_from(record_count)
            .is_ok_and(|count| fits(data, 0, HEADER_SIZE, count, RECORD_SIZE));
        if !fits {
            return Err(Error::Truncated {
                offset: HEADER_SIZE,
                context: "FeatureVariations records shorter than featureVariationRecordCount",
            });
        }
        let table = Self { data, record_count };
        let mut check = Check {
            data,
            budget: CHECK_BUDGET,
        };
        for index in 0..record_count {
            // The records fit, so neither the slot nor its fields
            // overflow or run past the end.
            let slot = HEADER_SIZE + index as usize * RECORD_SIZE;
            let (conditions, substitution) = table.record(index).ok_or(Error::Truncated {
                offset: slot,
                context: "FeatureVariationRecord past end of table",
            })?;
            if let Some(at) = check.target(0, conditions, slot)? {
                check.condition_set(at)?;
            }
            if let Some(at) = check.target(0, substitution, slot + 4)? {
                check.substitution(at)?;
            }
        }
        Ok(table)
    }

    /// Number of FeatureVariationRecords.
    #[must_use]
    pub const fn len(&self) -> u32 {
        self.record_count
    }

    /// True if the table has no records.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.record_count == 0
    }

    /// The condition set of record `record`, or `None` past the last
    /// record. A null offset reads as an empty set.
    #[must_use]
    pub fn condition_set(&self, record: u32) -> Option<ConditionSet<'a>> {
        let (conditions, _) = self.record(record)?;
        Some(ConditionSet::at(self.data, conditions))
    }

    /// The feature table substitution of record `record`, or `None`
    /// past the last record. A null offset reads as an empty one.
    #[must_use]
    pub fn substitution(&self, record: u32) -> Option<FeatureTableSubstitution<'a>> {
        let (_, substitution) = self.record(record)?;
        Some(FeatureTableSubstitution::at(self.data, substitution))
    }

    /// Index of the first record whose condition set holds at the
    /// normalized coordinates `coords`, one per `fvar` axis in file
    /// order (empty for the default instance). `store` is GDEF's
    /// ItemVariationStore, which value conditions read their deltas
    /// from. This is HarfBuzz's `find_index`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::Face;
    ///
    /// let data = include_bytes!("../../../tests/fixtures/rubik_vf.ttf");
    /// let face = Face::parse_bytes(data, 0)?;
    /// let gsub = face.gsub()?.expect("Rubik has GSUB");
    /// let variations = gsub.feature_variations()?.expect("GSUB 1.1");
    /// // Rubik swaps in its heavier currency signs from wght 500 on.
    /// assert_eq!(variations.find_index(&[], None), None);
    /// assert_eq!(variations.find_index(&[0.40625], None), Some(0));
    /// assert_eq!(variations.find_index(&[0.4062], None), None);
    /// # Ok::<(), sigilbuzz::Error>(())
    /// ```
    #[must_use]
    pub fn find_index(
        &self,
        coords: &[f32],
        store: Option<&ItemVariationStore<'_>>,
    ) -> Option<u32> {
        let cx = EvalContext::new(coords, store);
        (0..self.record_count)
            .find(|&index| self.condition_set(index).is_some_and(|set| set.holds(&cx)))
    }

    /// The alternate Feature table record `record` puts in place of
    /// feature `feature_index`: that of the first substitution record
    /// with the index. `None` when the record substitutes nothing for
    /// the feature or `record` is past the last record.
    #[must_use]
    pub fn substitute(&self, record: u32, feature_index: u16) -> Option<Feature<'a>> {
        self.substitution(record)?.find(feature_index)
    }

    /// `(conditionSetOffset, featureTableSubstitutionOffset)` of record
    /// `index`.
    fn record(&self, index: u32) -> Option<(u32, u32)> {
        if index >= self.record_count {
            return None;
        }
        let at = usize::try_from(index)
            .ok()?
            .checked_mul(RECORD_SIZE)?
            .checked_add(HEADER_SIZE)?;
        Some((u32_at(self.data, at)?, u32_at(self.data, at + 4)?))
    }
}

/// The checks [`FeatureVariations::parse`] makes on every subtable,
/// those of HarfBuzz's sanitizer.
struct Check<'a> {
    /// The FeatureVariations table, to the end of its GSUB or GPOS.
    data: &'a [u8],
    /// Subtables the check may still visit.
    budget: u32,
}

impl Check<'_> {
    /// Spends one unit of the budget on the subtable at `at`.
    fn spend(&mut self, at: usize) -> Result<()> {
        if self.budget == 0 {
            return Err(Error::Malformed {
                offset: at,
                context: "FeatureVariations need more checks than sigilbuzz makes",
            });
        }
        self.budget -= 1;
        Ok(())
    }

    /// Where the non-null `offset` from `base`, stored at byte `slot`,
    /// points: `None` for a null offset, an error for one past the end.
    fn target(&self, base: usize, offset: u32, slot: usize) -> Result<Option<usize>> {
        if offset == 0 {
            return Ok(None);
        }
        usize::try_from(offset)
            .ok()
            .and_then(|offset| base.checked_add(offset))
            .filter(|&at| at < self.data.len())
            .map(Some)
            .ok_or(Error::Malformed {
                offset: slot,
                context: "FeatureVariations offset past end of table",
            })
    }

    /// The ConditionSet at `at` and every condition it names.
    fn condition_set(&mut self, at: usize) -> Result<()> {
        self.spend(at)?;
        let count = u16_at(self.data, at).ok_or(Error::Truncated {
            offset: at,
            context: "ConditionSet shorter than its header",
        })?;
        if !fits(self.data, at, 2, usize::from(count), 4) {
            return Err(Error::Truncated {
                offset: at + 2,
                context: "ConditionSet shorter than conditionCount",
            });
        }
        for i in 0..usize::from(count) {
            let slot = at + 2 + i * 4;
            let offset = u32_at(self.data, slot).unwrap_or(0);
            if let Some(condition) = self.target(at, offset, slot)? {
                self.condition(condition, 1)?;
            }
        }
        Ok(())
    }

    /// The condition at `at`, nested `depth` deep, and the conditions
    /// it names.
    fn condition(&mut self, at: usize, depth: u8) -> Result<()> {
        if depth > MAX_CONDITION_DEPTH {
            return Err(Error::Malformed {
                offset: at,
                context: "conditions nested more than 64 deep",
            });
        }
        self.spend(at)?;
        let short = Error::Truncated {
            offset: at,
            context: "condition shorter than its format",
        };
        let format = u16_at(self.data, at).ok_or(short.clone())?;
        match format {
            1 | 2 => {
                if !fits(self.data, at, 8, 0, 0) {
                    return Err(short);
                }
            }
            3 | 4 => {
                let count = *self.data.get(at + 2).ok_or(short.clone())?;
                if !fits(self.data, at, 3, usize::from(count), 3) {
                    return Err(Error::Truncated {
                        offset: at + 3,
                        context: "condition list shorter than conditionCount",
                    });
                }
                for i in 0..usize::from(count) {
                    let slot = at + 3 + i * 3;
                    let offset = u24_at(self.data, slot).unwrap_or(0);
                    if let Some(child) = self.target(at, offset, slot)? {
                        self.condition(child, depth + 1)?;
                    }
                }
            }
            5 => {
                let offset = u24_at(self.data, at + 2).ok_or(short)?;
                if let Some(child) = self.target(at, offset, at + 2)? {
                    self.condition(child, depth + 1)?;
                }
            }
            // Other formats read fine and never hold.
            _ => {}
        }
        Ok(())
    }

    /// The FeatureTableSubstitution at `at` and its alternate Feature
    /// tables.
    fn substitution(&mut self, at: usize) -> Result<()> {
        self.spend(at)?;
        let (Some(major), Some(_minor)) = (u16_at(self.data, at), u16_at(self.data, at + 2)) else {
            return Err(Error::Truncated {
                offset: at,
                context: "FeatureTableSubstitution shorter than its header",
            });
        };
        if major != 1 {
            return Err(Error::Malformed {
                offset: at,
                context: "unsupported FeatureTableSubstitution major version",
            });
        }
        let count = u16_at(self.data, at + 4).ok_or(Error::Truncated {
            offset: at + 4,
            context: "FeatureTableSubstitution shorter than its header",
        })?;
        let header = SUBSTITUTION_HEADER_SIZE;
        if !fits(
            self.data,
            at,
            header,
            usize::from(count),
            SUBSTITUTION_RECORD_SIZE,
        ) {
            return Err(Error::Truncated {
                offset: at + header,
                context: "FeatureTableSubstitution records shorter than substitutionCount",
            });
        }
        for i in 0..usize::from(count) {
            let slot = at + header + i * SUBSTITUTION_RECORD_SIZE + 2;
            let offset = u32_at(self.data, slot).unwrap_or(0);
            if let Some(feature) = self.target(at, offset, slot)? {
                self.spend(feature)?;
                let lookups = u16_at(self.data, feature + 2).map(usize::from);
                if !lookups.is_some_and(|count| fits(self.data, feature, 4, count, 2)) {
                    return Err(Error::Truncated {
                        offset: feature,
                        context: "alternate Feature shorter than lookupIndexCount",
                    });
                }
            }
        }
        Ok(())
    }
}

/// A `ConditionSet`: conditions that must all hold for its record to
/// apply. An empty set always holds.
#[derive(Debug, Clone, Copy)]
pub struct ConditionSet<'a> {
    /// The FeatureVariations table, to the end of its GSUB or GPOS.
    /// Offsets only point forward, so every subtable starts inside it.
    data: &'a [u8],
    /// Where the set starts in `data`.
    at: usize,
    count: u16,
}

impl<'a> ConditionSet<'a> {
    /// The set at `offset` from the start of the FeatureVariations
    /// table `data`: empty for a null offset. [`FeatureVariations::parse`]
    /// checked that any other set fits; one that does not reads as
    /// empty.
    fn at(data: &'a [u8], offset: u32) -> Self {
        let empty = Self {
            data,
            at: 0,
            count: 0,
        };
        let Some(at) = position(0, offset) else {
            return empty;
        };
        let Some(count) = u16_at(data, at) else {
            return empty;
        };
        if !fits(data, at, 2, usize::from(count), 4) {
            return empty;
        }
        Self { data, at, count }
    }

    /// Number of conditions.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.count
    }

    /// True if the set has no conditions, so it always holds.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Condition `index`, or `None` past the last one.
    #[must_use]
    pub fn get(&self, index: u16) -> Option<Condition<'a>> {
        if index >= self.count {
            return None;
        }
        let slot = self.at + 2 + usize::from(index) * 4;
        let offset = u32_at(self.data, slot)?;
        Some(Condition::read(self.data, position(self.at, offset)))
    }

    /// True when every condition holds at the normalized coordinates
    /// `coords` (see [`FeatureVariations::find_index`]).
    #[must_use]
    pub fn matches(&self, coords: &[f32], store: Option<&ItemVariationStore<'_>>) -> bool {
        self.holds(&EvalContext::new(coords, store))
    }

    fn holds(&self, cx: &EvalContext<'_>) -> bool {
        (0..self.count).all(|i| self.get(i).is_some_and(|c| c.holds(cx, 1)))
    }
}

/// One condition. A null offset reads as [`Condition::Unknown`] with
/// format 0.
#[derive(Debug, Clone, Copy)]
pub enum Condition<'a> {
    /// Format 1: holds while the coordinate of axis `axis_index` lies
    /// in `min..=max`. All three are F2DOT14 values, as raw integers.
    AxisRange {
        /// Index of the axis in `fvar`.
        axis_index: u16,
        /// `filterRangeMinValue`.
        min: i16,
        /// `filterRangeMaxValue`.
        max: i16,
    },
    /// Format 2: holds while `default_value` plus the variation delta
    /// of `var_index` is above 0.
    Value {
        /// `defaultValue`, the value at the default instance.
        default_value: i16,
        /// `varIndex` into GDEF's ItemVariationStore: the outer index
        /// in the high 16 bits, the inner one in the low 16 bits.
        var_index: u32,
    },
    /// Format 3: holds when every condition of the list holds.
    And(ConditionList<'a>),
    /// Format 4: holds when any condition of the list holds.
    Or(ConditionList<'a>),
    /// Format 5: holds when the negated condition does not.
    Negate(Negation<'a>),
    /// Any other format, including format 0 for a null offset. It never
    /// holds.
    Unknown {
        /// The condition's format.
        format: u16,
    },
}

impl<'a> Condition<'a> {
    /// The condition at byte `at` of the FeatureVariations table
    /// `data`, or format 0 for a null offset (`None`). One that does not
    /// fit, which [`FeatureVariations::parse`] rules out, reads as
    /// format 0 too.
    fn read(data: &'a [u8], at: Option<usize>) -> Self {
        const NULL: Condition<'static> = Condition::Unknown { format: 0 };
        let Some(at) = at else {
            return NULL;
        };
        let Some(format) = u16_at(data, at) else {
            return NULL;
        };
        // The format fits, so `at + 2` does not overflow.
        let body = at + 2;
        match format {
            1 => match (
                u16_at(data, body),
                i16_at(data, body + 2),
                i16_at(data, body + 4),
            ) {
                (Some(axis_index), Some(min), Some(max)) => Self::AxisRange {
                    axis_index,
                    min,
                    max,
                },
                _ => NULL,
            },
            2 => match (i16_at(data, body), u32_at(data, body + 2)) {
                (Some(default_value), Some(var_index)) => Self::Value {
                    default_value,
                    var_index,
                },
                _ => NULL,
            },
            3 | 4 => {
                let Some(&count) = data.get(body) else {
                    return NULL;
                };
                if !fits(data, at, 3, usize::from(count), 3) {
                    return NULL;
                }
                let list = ConditionList { data, at, count };
                if format == 3 {
                    Self::And(list)
                } else {
                    Self::Or(list)
                }
            }
            5 => match u24_at(data, body) {
                Some(offset) => Self::Negate(Negation {
                    data,
                    child: position(at, offset),
                }),
                None => NULL,
            },
            format => Self::Unknown { format },
        }
    }

    /// True when the condition holds at the normalized coordinates
    /// `coords` (see [`FeatureVariations::find_index`]).
    #[must_use]
    pub fn matches(&self, coords: &[f32], store: Option<&ItemVariationStore<'_>>) -> bool {
        self.holds(&EvalContext::new(coords, store), 1)
    }

    /// Evaluates the condition at nesting depth `depth`, where the
    /// conditions of a ConditionSet are depth 1. `parse` rules out
    /// anything deeper than [`MAX_CONDITION_DEPTH`], and the check here
    /// keeps the recursion bounded whatever the data.
    fn holds(&self, cx: &EvalContext<'_>, depth: u8) -> bool {
        if depth > MAX_CONDITION_DEPTH {
            return false;
        }
        let next = depth + 1;
        match *self {
            Self::AxisRange {
                axis_index,
                min,
                max,
            } => {
                let coord = cx.coord(axis_index);
                i32::from(min) <= coord && coord <= i32::from(max)
            }
            Self::Value {
                default_value,
                var_index,
            } => f32::from(default_value) + cx.delta(var_index) > 0.0,
            Self::And(list) => list.iter().all(|c| c.holds(cx, next)),
            Self::Or(list) => list.iter().any(|c| c.holds(cx, next)),
            Self::Negate(negation) => !negation.condition().holds(cx, next),
            Self::Unknown { .. } => false,
        }
    }
}

/// The conditions of an "and" or "or" condition.
#[derive(Debug, Clone, Copy)]
pub struct ConditionList<'a> {
    data: &'a [u8],
    /// Where the owning condition starts in `data`.
    at: usize,
    count: u8,
}

impl<'a> ConditionList<'a> {
    /// Number of conditions.
    #[must_use]
    pub const fn len(&self) -> u8 {
        self.count
    }

    /// True if the list has no conditions.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Condition `index`, or `None` past the last one.
    #[must_use]
    pub fn get(&self, index: u8) -> Option<Condition<'a>> {
        if index >= self.count {
            return None;
        }
        let offset = u24_at(self.data, self.at + 3 + usize::from(index) * 3)?;
        Some(Condition::read(self.data, position(self.at, offset)))
    }

    /// Iterates the conditions in order.
    pub fn iter(&self) -> impl Iterator<Item = Condition<'a>> + 'a {
        let list = *self;
        (0..list.count).filter_map(move |i| list.get(i))
    }
}

/// The condition a "negate" condition negates.
#[derive(Debug, Clone, Copy)]
pub struct Negation<'a> {
    data: &'a [u8],
    /// Where the negated condition starts in `data`, `None` for a null
    /// offset.
    child: Option<usize>,
}

impl<'a> Negation<'a> {
    /// The negated condition.
    #[must_use]
    pub fn condition(&self) -> Condition<'a> {
        Condition::read(self.data, self.child)
    }
}

/// A `FeatureTableSubstitution`: alternate Feature tables by feature
/// index.
#[derive(Debug, Clone, Copy)]
pub struct FeatureTableSubstitution<'a> {
    /// The table, from its start to the end of the FeatureVariations.
    data: &'a [u8],
    count: u16,
}

impl<'a> FeatureTableSubstitution<'a> {
    /// The table at `offset` from the start of the FeatureVariations
    /// table `data`: empty for a null offset. [`FeatureVariations::parse`]
    /// checked any other one; one that does not read reads as empty.
    fn at(data: &'a [u8], offset: u32) -> Self {
        let empty = Self {
            data: &[],
            count: 0,
        };
        let Some(table) = position(0, offset).and_then(|at| data.get(at..)) else {
            return empty;
        };
        let (Some(1), Some(count)) = (u16_at(table, 0), u16_at(table, 4)) else {
            return empty;
        };
        if !fits(
            table,
            0,
            SUBSTITUTION_HEADER_SIZE,
            usize::from(count),
            SUBSTITUTION_RECORD_SIZE,
        ) {
            return empty;
        }
        Self { data: table, count }
    }

    /// Number of substitution records.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.count
    }

    /// True if the table substitutes nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Record `index` as `(featureIndex, alternate Feature)`, or `None`
    /// past the last record. A null alternate has no lookups.
    #[must_use]
    pub fn get(&self, index: u16) -> Option<(u16, Feature<'a>)> {
        if index >= self.count {
            return None;
        }
        let at = SUBSTITUTION_HEADER_SIZE + usize::from(index) * SUBSTITUTION_RECORD_SIZE;
        let feature_index = u16_at(self.data, at)?;
        let offset = u32_at(self.data, at + 2)?;
        let feature = if offset == 0 {
            Feature::empty()
        } else {
            Feature::parse_alternate(self.data, offset).unwrap_or(Feature::empty())
        };
        Some((feature_index, feature))
    }

    /// The alternate for feature `feature_index`: that of the first
    /// record with the index, as HarfBuzz's `find_substitute` scans.
    #[must_use]
    pub fn find(&self, feature_index: u16) -> Option<Feature<'a>> {
        (0..self.count)
            .filter_map(|i| self.get(i))
            .find(|&(index, _)| index == feature_index)
            .map(|(_, feature)| feature)
    }
}

/// The FeatureVariations at `offset` from the start of the GSUB or
/// GPOS table `table`: `Ok(None)` for a null offset, and a
/// [`Error::Malformed`] at byte 10, the offset field, with `context`
/// when it points past the end of the table.
pub(crate) fn locate<'a>(
    table: &'a [u8],
    offset: u32,
    context: &'static str,
) -> Result<Option<FeatureVariations<'a>>> {
    if offset == 0 {
        return Ok(None);
    }
    let data = usize::try_from(offset)
        .ok()
        .and_then(|at| table.get(at..))
        .ok_or(Error::Malformed {
            offset: 10,
            context,
        })?;
    FeatureVariations::parse(data).map(Some)
}

/// The record of `variations` that applies at the normalized
/// coordinates `coords` with GDEF's ItemVariationStore `store`, with
/// the table, ready for [`ActiveFeatures::with_variation`].
pub(crate) fn select<'a>(
    variations: Option<FeatureVariations<'a>>,
    coords: &[f32],
    store: Option<&ItemVariationStore<'_>>,
) -> Option<(FeatureVariations<'a>, u32)> {
    let variations = variations?;
    Some((variations, variations.find_index(coords, store)?))
}

/// A table's FeatureList as the FeatureVariations record the shaper
/// selected changes it: a substituted feature keeps its tag and takes
/// its lookups from the record's alternate Feature table.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ActiveFeatures<'a> {
    list: FeatureList<'a>,
    variation: Option<(FeatureVariations<'a>, u32)>,
}

impl<'a> ActiveFeatures<'a> {
    /// `list` with the substitutions of record `record` of `variations`
    /// when `variation` is `Some((variations, record))`.
    pub(crate) const fn new(
        list: FeatureList<'a>,
        variation: Option<(FeatureVariations<'a>, u32)>,
    ) -> Self {
        Self { list, variation }
    }

    /// Feature `index` as `(tag, Feature)`: the FeatureList's tag and
    /// the substituted Feature table if there is one, the FeatureList's
    /// otherwise. `None` when the index is out of range or the
    /// FeatureList's table for it cannot be read.
    pub(crate) fn get(&self, index: u16) -> Option<([u8; 4], Feature<'a>)> {
        let tag = self.list.tag(index)?;
        if let Some(feature) = self
            .variation
            .and_then(|(variations, record)| variations.substitute(record, index))
        {
            return Some((tag, feature));
        }
        self.list.get(index)
    }

    /// Index of the first feature record tagged `tag`, as HarfBuzz's
    /// `find_feature_index` scans the FeatureList.
    pub(crate) fn find(&self, tag: [u8; 4]) -> Option<u16> {
        (0..self.list.len()).find(|&i| self.list.tag(i) == Some(tag))
    }
}

/// The coordinates one evaluation reads, and the store for value
/// conditions.
struct EvalContext<'s> {
    /// The coordinates as F2DOT14 integers, which axis ranges compare.
    coords: Vec<i32>,
    /// The same coordinates as floats, which the variation store reads.
    normalized: Vec<f32>,
    store: Option<&'s ItemVariationStore<'s>>,
}

impl<'s> EvalContext<'s> {
    fn new(coords: &[f32], store: Option<&'s ItemVariationStore<'_>>) -> Self {
        let coords: Vec<i32> = coords.iter().map(|&c| to_f2dot14(c)).collect();
        let normalized = coords.iter().map(|&c| c as f32 / 16384.0).collect();
        Self {
            coords,
            normalized,
            store,
        }
    }

    /// Coordinate of axis `axis_index`, 0 past the last axis.
    fn coord(&self, axis_index: u16) -> i32 {
        self.coords
            .get(usize::from(axis_index))
            .copied()
            .unwrap_or(0)
    }

    /// The delta of `var_index` at the coordinates: 0 without
    /// coordinates or a store, and for `NO_VARIATION_INDEX`.
    fn delta(&self, var_index: u32) -> f32 {
        if self.coords.is_empty() || var_index == NO_VARIATION_INDEX {
            return 0.0;
        }
        let outer = (var_index >> 16) as u16;
        let inner = (var_index & 0xFFFF) as u16;
        self.store
            .map_or(0.0, |store| store.delta(outer, inner, &self.normalized))
    }
}

/// A normalized coordinate as an F2DOT14 integer: scaled by 16384,
/// clamped to the F2DOT14 range, and rounded the way HarfBuzz's
/// `roundf` rounds, `floor(x + 0.5)`, so halves go up. NaN reads as 0.
fn to_f2dot14(coord: f32) -> i32 {
    if coord.is_nan() {
        return 0;
    }
    // Scaling by a power of two is exact, and so is adding 0.5 to a
    // value this small.
    let shifted = (coord * 16384.0).clamp(-32768.0, 32767.0) + 0.5;
    // The cast truncates toward zero; step down for a negative value
    // with a fraction to get the floor.
    let whole = shifted as i32;
    if whole as f32 > shifted {
        whole - 1
    } else {
        whole
    }
}

/// `base + offset` for a non-null offset, `None` for a null one or one
/// that overflows.
fn position(base: usize, offset: u32) -> Option<usize> {
    if offset == 0 {
        return None;
    }
    base.checked_add(usize::try_from(offset).ok()?)
}

/// True when `count` entries of `size` bytes after a `header` of
/// `header` bytes at byte `at` fit in `data`.
fn fits(data: &[u8], at: usize, header: usize, count: usize, size: usize) -> bool {
    count
        .checked_mul(size)
        .and_then(|n| n.checked_add(header))
        .and_then(|n| n.checked_add(at))
        .is_some_and(|end| end <= data.len())
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    let bytes = data.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn i16_at(data: &[u8], at: usize) -> Option<i16> {
    u16_at(data, at).map(|v| v as i16)
}

fn u24_at(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at.checked_add(3)?)?;
    Some(u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(test)]
mod tests;
