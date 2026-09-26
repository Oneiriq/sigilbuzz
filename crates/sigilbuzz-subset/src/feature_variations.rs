//! FeatureVariations in GSUB and GPOS 1.1.
//!
//! A variable font can swap the lookups of a feature by region of the
//! design space: `rvrn` alternates for heavy weights, or kerning that
//! only applies to narrow widths. The table lists records of a
//! condition set (axis ranges, all of which must hold) and a
//! FeatureTableSubstitution (feature index to alternate Feature). A
//! shaper applies the first record whose conditions hold at the
//! current coordinates and uses each substituted feature's alternate
//! instead of its default.
//!
//! ```text
//!   FeatureVariations:
//!     u16      majorVersion = 1
//!     u16      minorVersion = 0
//!     u32      featureVariationRecordCount
//!     FeatureVariationRecord records[count]:
//!       Offset32 conditionSetOffset               (from FeatureVariations)
//!       Offset32 featureTableSubstitutionOffset   (from FeatureVariations)
//!   ConditionSet:
//!     u16      conditionCount
//!     Offset32 conditionOffsets[conditionCount]   (from ConditionSet)
//!   ConditionFormat1 (axis range):
//!     u16      format = 1
//!     u16      axisIndex
//!     F2Dot14  filterRangeMinValue
//!     F2Dot14  filterRangeMaxValue
//!   FeatureTableSubstitution:
//!     u16      majorVersion = 1
//!     u16      minorVersion = 0
//!     u16      substitutionCount
//!     FeatureTableSubstitutionRecord substitutions[count]:
//!       u16      featureIndex
//!       Offset32 alternateFeatureOffset           (from FeatureTableSubstitution)
//!   Feature:
//!     Offset16 featureParamsOffset                (from Feature)
//!     u16      lookupIndexCount
//!     u16      lookupListIndices[lookupIndexCount]
//! ```
//!
//! The subsetter ([`subset`]) keeps the records and sends the feature
//! and lookup indices in them through the same remaps as the FeatureList
//! and LookupList. The instancer ([`instance_table`]) settles every
//! condition on a pinned axis the way HarfBuzz's instancer does and
//! renumbers the axes that stay; when the first record left always
//! applies, its substitutions move into the default FeatureList and the
//! FeatureVariations go.
//!
//! A null ConditionSet or FeatureTableSubstitution offset reads as an
//! empty one, as in HarfBuzz. A table that cannot be read is dropped
//! whole and reported.

use alloc::vec::Vec;

use sigilbuzz::Error;

use crate::device::Dedup;
use crate::offset16::Offset16Guard;
use crate::read::{array_at, offset32_at, slice_at, u16_at, u32_at};
use crate::util::{WorkBudget, WORK_LIMIT};
use crate::warnings::{Diag, Warnings};
use crate::SubsetError;

const CTX: &str = "FeatureVariations truncated";
const OFFSET: &str = "FeatureVariations offset past the end of the table";

/// One condition of a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Condition {
    /// Format 1: holds while the normalized coordinate of `axis` lies
    /// in `min..=max` (F2Dot14 units).
    AxisRange { axis: u16, min: i16, max: i16 },
    /// A format this crate does not read. Shapers treat a condition
    /// they cannot read as never holding.
    Unknown { format: u16, at: usize },
}

/// One substitution of a record: `feature` takes the alternate Feature
/// at `alternate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Substitution {
    pub feature: u16,
    /// Position of the alternate Feature table in the GSUB or GPOS.
    pub alternate: usize,
    /// The alternate's featureParamsOffset, from the alternate.
    pub params: u16,
    pub lookups: Vec<u16>,
}

/// One FeatureVariationRecord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    pub conditions: Vec<Condition>,
    pub substitutions: Vec<Substitution>,
    /// The record's featureTableSubstitutionOffset as stored, measured
    /// from the FeatureVariations table (0 for none).
    substitution_offset: u32,
}

/// A parsed FeatureVariations table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FeatureVariations {
    /// Where the table starts in the GSUB or GPOS.
    at: usize,
    pub records: Vec<Record>,
}

/// Reads the FeatureVariations of the GSUB or GPOS `table`. `Ok(None)`
/// when the table is version 1.0 or has a null offset; an error, located
/// from the start of `table`, when it cannot be read.
pub(crate) fn read(table: &[u8]) -> Result<Option<FeatureVariations>, Error> {
    if u16_at(table, 2, CTX)? < 1 {
        return Ok(None);
    }
    if u32_at(table, 10, CTX)? == 0 {
        return Ok(None);
    }
    let at = offset32_at(table, 10, 0, OFFSET)?;
    if u16_at(table, at, CTX)? != 1 {
        return Err(Error::Malformed {
            offset: at,
            context: "unsupported FeatureVariations major version",
        });
    }
    let count = u32_at(table, at + 4, CTX)?;
    let count = usize::try_from(count).map_err(|_| Error::Truncated {
        offset: at + 8,
        context: CTX,
    })?;
    array_at(table, at + 8, count, 8, CTX)?;
    // Records may share condition sets and substitution tables, so a
    // small table can name far more conditions and lookups than it
    // holds. Every one read is charged to a work budget.
    let budget = WorkBudget::new(WORK_LIMIT);
    let charge = |units: usize| {
        if budget.spend(units) {
            Ok(())
        } else {
            Err(Error::Malformed {
                offset: at,
                context: "FeatureVariations name more records than the subsetter reads",
            })
        }
    };
    let mut records = Vec::new();
    for i in 0..count {
        let slot = at + 8 + i * 8;
        charge(1)?;
        records.push(Record {
            conditions: read_condition_set(table, at, slot, &charge)?,
            substitutions: read_substitutions(table, at, slot + 4, &charge)?,
            substitution_offset: u32_at(table, slot + 4, CTX)?,
        });
    }
    Ok(Some(FeatureVariations { at, records }))
}

/// Reads the ConditionSet named by the Offset32 at `slot`, measured
/// from the FeatureVariations at `fv`.
fn read_condition_set(
    table: &[u8],
    fv: usize,
    slot: usize,
    charge: &dyn Fn(usize) -> Result<(), Error>,
) -> Result<Vec<Condition>, Error> {
    if u32_at(table, slot, CTX)? == 0 {
        return Ok(Vec::new());
    }
    let set = offset32_at(table, slot, fv, OFFSET)?;
    let count = usize::from(u16_at(table, set, CTX)?);
    array_at(table, set + 2, count, 4, CTX)?;
    charge(count)?;
    let mut conditions = Vec::with_capacity(count);
    for i in 0..count {
        let at = offset32_at(table, set + 2 + i * 4, set, OFFSET)?;
        conditions.push(match u16_at(table, at, CTX)? {
            1 => {
                let body = slice_at(table, at + 2, 6, CTX)?;
                Condition::AxisRange {
                    axis: u16::from_be_bytes([body[0], body[1]]),
                    min: i16::from_be_bytes([body[2], body[3]]),
                    max: i16::from_be_bytes([body[4], body[5]]),
                }
            }
            format => Condition::Unknown { format, at },
        });
    }
    Ok(conditions)
}

/// Reads the FeatureTableSubstitution named by the Offset32 at `slot`,
/// measured from the FeatureVariations at `fv`.
fn read_substitutions(
    table: &[u8],
    fv: usize,
    slot: usize,
    charge: &dyn Fn(usize) -> Result<(), Error>,
) -> Result<Vec<Substitution>, Error> {
    if u32_at(table, slot, CTX)? == 0 {
        return Ok(Vec::new());
    }
    let fts = offset32_at(table, slot, fv, OFFSET)?;
    if u16_at(table, fts, CTX)? != 1 {
        return Err(Error::Malformed {
            offset: fts,
            context: "unsupported FeatureTableSubstitution major version",
        });
    }
    let count = usize::from(u16_at(table, fts + 4, CTX)?);
    array_at(table, fts + 6, count, 6, CTX)?;
    charge(count)?;
    let mut substitutions = Vec::with_capacity(count);
    for i in 0..count {
        let rec = fts + 6 + i * 6;
        let alternate = offset32_at(table, rec + 2, fts, OFFSET)?;
        let params = u16_at(table, alternate, CTX)?;
        let lookup_count = usize::from(u16_at(table, alternate + 2, CTX)?);
        charge(lookup_count)?;
        let lookups = array_at(table, alternate + 4, lookup_count, 2, CTX)?
            .chunks_exact(2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .collect();
        substitutions.push(Substitution {
            feature: u16_at(table, rec, CTX)?,
            alternate,
            params,
            lookups,
        });
    }
    Ok(substitutions)
}

impl Record {
    /// Whether a shaper can ever apply the record: a condition of a
    /// format it cannot read never holds.
    fn readable(&self) -> bool {
        self.conditions
            .iter()
            .all(|c| matches!(c, Condition::AxisRange { .. }))
    }

    /// Where the first condition of a format this crate cannot read
    /// sits, if any.
    fn unknown_condition(&self) -> Option<usize> {
        self.conditions.iter().find_map(|c| match c {
            Condition::Unknown { at, .. } => Some(*at),
            Condition::AxisRange { .. } => None,
        })
    }
}

impl FeatureVariations {
    /// Marks the features some substitution gives an alternate with a
    /// lookup that survives `lookup_renumber`. The subsetter keeps such
    /// a feature even when its default lookups all dropped, the rule
    /// HarfBuzz's subsetter applies: at some coordinates the feature
    /// still does something. Records no shaper can apply are left out.
    pub(crate) fn features_with_live_alternates(
        &self,
        feature_count: usize,
        lookup_renumber: &[Option<u16>],
    ) -> Vec<bool> {
        let mut live = alloc::vec![false; feature_count];
        let readable = self.records.iter().filter(|r| r.readable());
        for sub in readable.flat_map(|r| &r.substitutions) {
            let survives = sub.lookups.iter().any(|&l| {
                lookup_renumber
                    .get(usize::from(l))
                    .copied()
                    .flatten()
                    .is_some()
            });
            if let Some(slot) = live.get_mut(usize::from(sub.feature)) {
                *slot |= survives;
            }
        }
        live
    }
}

/// A record rebuilt for the output: its conditions and, per
/// substituted feature, the alternate's lookup indices.
type OutRecord = (Vec<Condition>, Vec<(u16, Vec<u16>)>);

/// Rebuilds `fv` for a subset: every substitution's feature index goes
/// through `feature_renumber` (a substitution of a dropped feature
/// goes) and its alternate's lookup indices through `lookup_renumber`
/// (dropped lookups go; an alternate left empty stays, since it still
/// turns its feature off). Records are kept up to the last one that
/// still substitutes something: an earlier record left empty still
/// stops the later ones from applying, a later one does nothing. Their
/// conditions are copied as they are. `None` when no record substitutes
/// anything; the table is then written as version 1.0.
///
/// A record with a condition of a format shapers cannot read never
/// applies, so it is left out (and reported through `diag`) without
/// changing what the others do. The alternates lose their
/// FeatureParams, as the rebuilt FeatureList does.
pub(crate) fn subset(
    fv: &FeatureVariations,
    feature_renumber: &[Option<u16>],
    lookup_renumber: &[Option<u16>],
    diag: &Diag<'_>,
) -> Result<Option<Vec<u8>>, SubsetError> {
    let mut records: Vec<OutRecord> = fv
        .records
        .iter()
        .filter(|record| match record.unknown_condition() {
            Some(at) => {
                diag.at(
                    at,
                    "unsupported FeatureVariations condition format",
                    "a FeatureVariations record",
                );
                false
            }
            None => true,
        })
        .map(|record| {
            let substitutions = record
                .substitutions
                .iter()
                .filter_map(|sub| {
                    let feature = feature_renumber
                        .get(usize::from(sub.feature))
                        .copied()
                        .flatten()?;
                    let lookups = sub
                        .lookups
                        .iter()
                        .filter_map(|&l| lookup_renumber.get(usize::from(l)).copied().flatten())
                        .collect();
                    Some((feature, lookups))
                })
                .collect();
            (record.conditions.clone(), substitutions)
        })
        .collect();
    let keep = records
        .iter()
        .rposition(|(_, subs)| !subs.is_empty())
        .map_or(0, |last| last + 1);
    records.truncate(keep);
    if records.is_empty() {
        return Ok(None);
    }
    emit(&records).map(Some)
}

/// Serializes a FeatureVariations table: the header and records, then
/// every ConditionSet and FeatureTableSubstitution, identical ones
/// shared.
fn emit(records: &[OutRecord]) -> Result<Vec<u8>, SubsetError> {
    const TOO_BIG: &str = "FeatureVariations rewrite: the table exceeds 4 GiB";
    let at32 = |pos: usize| u32::try_from(pos).map_err(|_| SubsetError::Unsupported(TOO_BIG));
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&at32(records.len())?.to_be_bytes());
    out.resize(8 + records.len() * 8, 0);
    let mut shared = Dedup::default();
    for (i, (conditions, substitutions)) in records.iter().enumerate() {
        let set = shared.place(&mut out, &encode_condition_set(conditions));
        let fts = shared.place(&mut out, &encode_substitutions(substitutions)?);
        let slot = 8 + i * 8;
        out[slot..slot + 4].copy_from_slice(&at32(set)?.to_be_bytes());
        out[slot + 4..slot + 8].copy_from_slice(&at32(fts)?.to_be_bytes());
    }
    Ok(out)
}

/// A ConditionSet with its format 1 conditions right behind the offset
/// array. Only readable conditions reach here.
fn encode_condition_set(conditions: &[Condition]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(conditions.len() as u16).to_be_bytes());
    let first = 2 + conditions.len() * 4;
    for i in 0..conditions.len() {
        out.extend_from_slice(&((first + i * 8) as u32).to_be_bytes());
    }
    for condition in conditions {
        if let Condition::AxisRange { axis, min, max } = *condition {
            out.extend_from_slice(&1u16.to_be_bytes());
            out.extend_from_slice(&axis.to_be_bytes());
            out.extend_from_slice(&min.to_be_bytes());
            out.extend_from_slice(&max.to_be_bytes());
        }
    }
    out
}

/// A FeatureTableSubstitution with its alternate Features (without
/// FeatureParams) behind the records.
fn encode_substitutions(substitutions: &[(u16, Vec<u16>)]) -> Result<Vec<u8>, SubsetError> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(substitutions.len() as u16).to_be_bytes());
    out.resize(6 + substitutions.len() * 6, 0);
    for (i, (feature, lookups)) in substitutions.iter().enumerate() {
        let at = u32::try_from(out.len()).map_err(|_| {
            SubsetError::Unsupported("FeatureVariations rewrite: a substitution exceeds 4 GiB")
        })?;
        let rec = 6 + i * 6;
        out[rec..rec + 2].copy_from_slice(&feature.to_be_bytes());
        out[rec + 2..rec + 6].copy_from_slice(&at.to_be_bytes());
        out.extend_from_slice(&encode_feature(lookups));
    }
    Ok(out)
}

/// A Feature table without FeatureParams.
fn encode_feature(lookups: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + lookups.len() * 2);
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(lookups.len() as u16).to_be_bytes());
    for l in lookups {
        out.extend_from_slice(&l.to_be_bytes());
    }
    out
}

/// What instancing leaves of a FeatureVariations table.
#[derive(Debug, PartialEq, Eq)]
enum Settled {
    /// No record can apply any more: the table goes, and the default
    /// FeatureList stays as it is.
    Gone,
    /// The first record left always applies: its substitutions become
    /// the default features (record index into the source table) and
    /// the table goes.
    Fold(usize),
    /// These source records stay, each with the conditions it has left
    /// (axis indices renumbered).
    Keep(Vec<(usize, Vec<Condition>)>),
}

/// Settles every condition of `fv` on the axes `pinned` fixes, the way
/// HarfBuzz's instancer does:
///
/// - `pinned[axis]` is the axis's normalized F2Dot14 coordinate (after
///   avar) when it is pinned, `None` when it stays variable. An axis
///   index past `pinned` reads coordinate 0, as shapers do.
/// - A condition on a fixed coordinate either always holds (and is
///   dropped) or never does (and the record goes with it). So does a
///   condition whose range is empty, and one of a format shapers cannot
///   read, which never holds (reported to `warnings`).
/// - A condition on a variable axis stays, renumbered through
///   `new_axis`.
/// - A record whose conditions include every condition of an earlier
///   record left can never be the first to match, so it goes; that
///   covers everything after a record left without conditions.
fn settle(
    fv: &FeatureVariations,
    pinned: &[Option<i16>],
    new_axis: &[Option<u16>],
    table_tag: [u8; 4],
    warnings: &Warnings,
) -> Settled {
    let mut kept: Vec<(usize, Vec<Condition>)> = Vec::new();
    // Each record is compared with every one kept before it, so the
    // comparisons are charged to a work budget.
    let budget = WorkBudget::new(WORK_LIMIT);
    'records: for (index, record) in fv.records.iter().enumerate() {
        let mut left = Vec::new();
        for &condition in &record.conditions {
            let (axis, min, max) = match condition {
                Condition::AxisRange { axis, min, max } => (axis, min, max),
                Condition::Unknown { at, .. } => {
                    warnings.push(
                        table_tag,
                        at,
                        "unsupported FeatureVariations condition format",
                        "a FeatureVariations record",
                    );
                    continue 'records;
                }
            };
            if min > max {
                continue 'records;
            }
            let fixed = match pinned.get(usize::from(axis)) {
                Some(&coord) => coord,
                None => Some(0),
            };
            match fixed {
                Some(coord) if (min..=max).contains(&coord) => {}
                Some(_) => continue 'records,
                None => {
                    let Some(axis) = new_axis.get(usize::from(axis)).copied().flatten() else {
                        continue 'records;
                    };
                    left.push(Condition::AxisRange { axis, min, max });
                }
            }
        }
        left.sort_unstable();
        left.dedup();
        let work = kept
            .iter()
            .map(|(_, earlier)| 1 + earlier.len() * left.len())
            .sum();
        if !budget.spend(work) {
            warnings.push(
                table_tag,
                fv.at,
                "FeatureVariations name more records than the instancer compares",
                "the FeatureVariations",
            );
            return Settled::Gone;
        }
        if kept
            .iter()
            .any(|(_, earlier)| earlier.iter().all(|c| left.contains(c)))
        {
            continue;
        }
        kept.push((index, left));
    }
    match kept.first() {
        None => Settled::Gone,
        Some((index, conditions)) if conditions.is_empty() => Settled::Fold(*index),
        Some(_) => Settled::Keep(kept),
    }
}

/// Instances the FeatureVariations of the GSUB or GPOS `table` (see
/// [`settle`] for `pinned` and `new_axis`). Returns the rebuilt table,
/// or `None` when it has no FeatureVariations to change.
///
/// - When a record is left that does not always apply, the table keeps
///   its FeatureList, and its FeatureVariations header is rewritten in
///   place over the source header (it can only shrink) with the records
///   left, each pointing at its source FeatureTableSubstitution and at
///   its new ConditionSet appended to the table.
/// - When the first record left always applies, its substitutions move
///   into the default FeatureList and the table becomes version 1.0.
///   The new FeatureList goes right behind the header and everything
///   else moves up by its size, so no offset into the source layout
///   changes.
/// - When no record is left, the table becomes version 1.0.
///
/// A FeatureVariations table that cannot be read is dropped the same
/// way and reported to `warnings`.
pub(crate) fn instance_table(
    table: &[u8],
    table_tag: [u8; 4],
    pinned: &[Option<i16>],
    new_axis: &[Option<u16>],
    warnings: &Warnings,
) -> Result<Option<Vec<u8>>, SubsetError> {
    let fv = match read(table) {
        Ok(Some(fv)) => fv,
        Ok(None) => return Ok(None),
        Err(e) => {
            warnings.parse_error(table_tag, 0, &e, "the FeatureVariations");
            return Ok(Some(without_feature_variations(table)));
        }
    };
    let rebuilt = match settle(&fv, pinned, new_axis, table_tag, warnings) {
        Settled::Gone => return Ok(Some(without_feature_variations(table))),
        Settled::Fold(index) => fold(table, &fv.records[index]),
        Settled::Keep(kept) => rewrite_in_place(table, &fv, &kept),
    };
    match rebuilt {
        Ok(bytes) => Ok(Some(bytes)),
        Err(SubsetError::Parse(e)) => {
            warnings.parse_error(table_tag, 0, &e, "the FeatureVariations");
            Ok(Some(without_feature_variations(table)))
        }
        Err(other) => Err(other),
    }
}

/// `table` as version 1.0: the minor version and the FeatureVariations
/// offset zeroed. Nothing else moves.
fn without_feature_variations(table: &[u8]) -> Vec<u8> {
    let mut out = table.to_vec();
    for field in [2..4, 10..14] {
        if let Some(bytes) = out.get_mut(field) {
            bytes.fill(0);
        }
    }
    out
}

/// Rewrites the FeatureVariations header in place with the records in
/// `kept`, and appends their ConditionSets to the table.
fn rewrite_in_place(
    table: &[u8],
    fv: &FeatureVariations,
    kept: &[(usize, Vec<Condition>)],
) -> Result<Vec<u8>, SubsetError> {
    const TOO_BIG: &str = "FeatureVariations instancing: the table exceeds 4 GiB";
    let mut out = table.to_vec();
    let mut header = Vec::with_capacity(8 + kept.len() * 8);
    header.extend_from_slice(&1u16.to_be_bytes());
    header.extend_from_slice(&0u16.to_be_bytes());
    header.extend_from_slice(&(kept.len() as u32).to_be_bytes());
    let mut sets = Dedup::default();
    for (index, conditions) in kept {
        let set = sets.place(&mut out, &encode_condition_set(conditions));
        let rel = u32::try_from(set - fv.at).map_err(|_| SubsetError::Unsupported(TOO_BIG))?;
        header.extend_from_slice(&rel.to_be_bytes());
        header.extend_from_slice(&fv.records[*index].substitution_offset.to_be_bytes());
    }
    // The source header held every record, so the new one fits.
    out[fv.at..fv.at + header.len()].copy_from_slice(&header);
    Ok(out)
}

/// Moves `record`'s substitutions into the default FeatureList of
/// `table` and makes the table version 1.0.
///
/// The new FeatureList sits right behind the 10-byte header and every
/// other byte of the source moves up by its size, so the ScriptList,
/// the LookupList and every table under them keep their relative
/// offsets. Features without a substitution point at their source
/// tables in the moved block; a substituted feature gets a copy of its
/// alternate (its FeatureParams, if any, stay in the moved block).
///
/// A list or table that sits inside the source header cannot move with
/// the rest; that is a parse error. An offset the move pushes past
/// 64 KiB is [`SubsetError::Unsupported`].
fn fold(table: &[u8], record: &Record) -> Result<Vec<u8>, SubsetError> {
    const CTX16: &str = "FeatureVariations instancing: a FeatureList offset exceeds 64 KiB";
    const HEADER: usize = 10;
    let header_at = |pos: usize| u16_at(table, pos, CTX).map(usize::from);
    let script_list = header_at(4)?;
    let feature_list = header_at(6)?;
    let lookup_list = header_at(8)?;
    let count = usize::from(u16_at(table, feature_list, CTX)?);
    let records = array_at(table, feature_list + 2, count, 6, CTX)?;

    // Size of the new FeatureList: the records, then the copies.
    let substitute = |feature: usize| {
        record
            .substitutions
            .iter()
            .find(|sub| usize::from(sub.feature) == feature)
    };
    let copies: usize = (0..count)
        .filter_map(substitute)
        .map(|sub| 4 + sub.lookups.len() * 2)
        .sum();
    let size = 2 + count * 6 + copies;
    // Where a source byte at `pos` lands, measured from the new
    // FeatureList: the bytes after the header move up by `size`.
    let moved = |pos: usize, slot: usize| {
        if pos < HEADER {
            return Err(Error::Malformed {
                offset: slot,
                context: "a GSUB or GPOS table sits inside the header",
            });
        }
        Ok(pos + size - HEADER)
    };

    let offsets = Offset16Guard::default();
    let mut list = Vec::with_capacity(size);
    list.extend_from_slice(&(count as u16).to_be_bytes());
    list.resize(2 + count * 6, 0);
    for i in 0..count {
        let slot = feature_list + 2 + i * 6;
        let rec = &records[i * 6..i * 6 + 6];
        list[2 + i * 6..6 + i * 6].copy_from_slice(&rec[..4]);
        let target = match substitute(i) {
            Some(sub) => {
                let copy = list.len();
                let params = if sub.params == 0 {
                    0
                } else {
                    // Distance from the copy to the moved FeatureParams.
                    let at = moved(sub.alternate + usize::from(sub.params), sub.alternate)?;
                    offsets.narrow(at - copy)
                };
                list.extend_from_slice(&params.to_be_bytes());
                list.extend_from_slice(&(sub.lookups.len() as u16).to_be_bytes());
                for l in &sub.lookups {
                    list.extend_from_slice(&l.to_be_bytes());
                }
                copy
            }
            None => {
                let source = feature_list + usize::from(u16::from_be_bytes([rec[4], rec[5]]));
                moved(source, slot + 4)?
            }
        };
        let at = offsets.narrow(target);
        list[6 + i * 6..8 + i * 6].copy_from_slice(&at.to_be_bytes());
    }
    let mut out = Vec::with_capacity(table.len() + size);
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    let script_at = moved(script_list, 4)? + HEADER;
    let lookup_at = moved(lookup_list, 8)? + HEADER;
    for at in [script_at, HEADER, lookup_at] {
        out.extend_from_slice(&offsets.narrow(at).to_be_bytes());
    }
    offsets.check(CTX16)?;
    out.extend_from_slice(&list);
    out.extend_from_slice(&table[HEADER..]);
    Ok(out)
}

#[cfg(test)]
mod tests;
