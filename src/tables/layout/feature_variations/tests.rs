//! Hand-built byte fixtures for the FeatureVariations parser.

use super::*;
use alloc::boxed::Box;
use alloc::vec;

/// A condition to serialize. `Null` is a null offset where a parent
/// names one, and `Raw` is a condition of any format with the given
/// bytes after the format.
#[derive(Clone)]
enum Cond {
    Range(u16, i16, i16),
    Value(i16, u32),
    And(Vec<Cond>),
    Or(Vec<Cond>),
    Not(Box<Cond>),
    Raw(u16, Vec<u8>),
    Null,
}

use Cond::{And, Not, Null, Or, Range, Raw, Value};

/// F2DOT14 for `v`.
fn f2(v: f32) -> i16 {
    (v * 16384.0) as i16
}

/// `cond` at byte 0, its children after it, offsets measured from its
/// start.
fn cond_bytes(cond: &Cond) -> Vec<u8> {
    let mut out = Vec::new();
    match cond {
        Range(axis, min, max) => {
            out.extend_from_slice(&1u16.to_be_bytes());
            out.extend_from_slice(&axis.to_be_bytes());
            out.extend_from_slice(&min.to_be_bytes());
            out.extend_from_slice(&max.to_be_bytes());
        }
        Value(default, index) => {
            out.extend_from_slice(&2u16.to_be_bytes());
            out.extend_from_slice(&default.to_be_bytes());
            out.extend_from_slice(&index.to_be_bytes());
        }
        And(list) | Or(list) => {
            let format: u16 = if matches!(cond, And(_)) { 3 } else { 4 };
            out.extend_from_slice(&format.to_be_bytes());
            out.push(list.len() as u8);
            out.resize(3 + 3 * list.len(), 0);
            for (i, child) in list.iter().enumerate() {
                if matches!(child, Null) {
                    continue;
                }
                let offset = out.len() as u32;
                out[3 + 3 * i..6 + 3 * i].copy_from_slice(&offset.to_be_bytes()[1..]);
                out.extend(cond_bytes(child));
            }
        }
        Not(child) => {
            out.extend_from_slice(&5u16.to_be_bytes());
            out.extend_from_slice(&[0, 0, 0]);
            if !matches!(**child, Null) {
                let offset = out.len() as u32;
                out[2..5].copy_from_slice(&offset.to_be_bytes()[1..]);
                out.extend(cond_bytes(child));
            }
        }
        Raw(format, body) => {
            out.extend_from_slice(&format.to_be_bytes());
            out.extend_from_slice(body);
        }
        Null => unreachable!("a null condition has no bytes"),
    }
    out
}

/// A ConditionSet at byte 0, its conditions after it.
fn set_bytes(conds: &[Cond]) -> Vec<u8> {
    let mut out = (conds.len() as u16).to_be_bytes().to_vec();
    out.resize(2 + 4 * conds.len(), 0);
    for (i, cond) in conds.iter().enumerate() {
        if matches!(cond, Null) {
            continue;
        }
        let offset = out.len() as u32;
        out[2 + 4 * i..6 + 4 * i].copy_from_slice(&offset.to_be_bytes());
        out.extend(cond_bytes(cond));
    }
    out
}

/// One FeatureTableSubstitution: its major version and records of
/// `(featureIndex, alternate lookups)`, `None` for a null alternate.
struct Subst {
    major: u16,
    records: Vec<(u16, Option<Vec<u16>>)>,
}

fn subst(records: &[(u16, &[u16])]) -> Subst {
    Subst {
        major: 1,
        records: records
            .iter()
            .map(|&(i, l)| (i, Some(l.to_vec())))
            .collect(),
    }
}

fn subst_bytes(s: &Subst) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&s.major.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(s.records.len() as u16).to_be_bytes());
    out.resize(6 + 6 * s.records.len(), 0);
    for (i, (feature, lookups)) in s.records.iter().enumerate() {
        let slot = 6 + 6 * i;
        out[slot..slot + 2].copy_from_slice(&feature.to_be_bytes());
        let Some(lookups) = lookups else {
            continue;
        };
        let offset = out.len() as u32;
        out[slot + 2..slot + 6].copy_from_slice(&offset.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // featureParamsOffset
        out.extend_from_slice(&(lookups.len() as u16).to_be_bytes());
        for l in lookups {
            out.extend_from_slice(&l.to_be_bytes());
        }
    }
    out
}

/// One FeatureVariationRecord: `None` for a null offset.
struct Record {
    conditions: Option<Vec<Cond>>,
    substitution: Option<Subst>,
}

fn record(conditions: &[Cond], substitution: Subst) -> Record {
    Record {
        conditions: Some(conditions.to_vec()),
        substitution: Some(substitution),
    }
}

/// A FeatureVariations table with its subtables after the records.
fn fv_bytes(records: &[Record]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(records.len() as u32).to_be_bytes());
    out.resize(8 + 8 * records.len(), 0);
    for (i, r) in records.iter().enumerate() {
        let slot = 8 + 8 * i;
        if let Some(conds) = &r.conditions {
            let offset = out.len() as u32;
            out[slot..slot + 4].copy_from_slice(&offset.to_be_bytes());
            out.extend(set_bytes(conds));
        }
        if let Some(s) = &r.substitution {
            let offset = out.len() as u32;
            out[slot + 4..slot + 8].copy_from_slice(&offset.to_be_bytes());
            out.extend(subst_bytes(s));
        }
    }
    out
}

/// A one-record table whose record substitutes feature 0, so
/// `find_index` says whether `conds` hold.
fn holds(conds: &[Cond], coords: &[f32]) -> bool {
    let bytes = fv_bytes(&[record(conds, subst(&[(0, &[1])]))]);
    let fv = FeatureVariations::parse(&bytes).unwrap();
    let set = fv.condition_set(0).unwrap();
    assert_eq!(
        set.matches(coords, None),
        fv.find_index(coords, None) == Some(0)
    );
    fv.find_index(coords, None) == Some(0)
}

fn lookups(feature: Option<Feature<'_>>) -> Option<Vec<u16>> {
    feature.map(|f| f.lookup_indices().collect())
}

#[test]
fn parses_records() {
    let bytes = fv_bytes(&[
        record(&[Range(0, 0, f2(1.0))], subst(&[(2, &[5])])),
        record(&[], subst(&[])),
    ]);
    let fv = FeatureVariations::parse(&bytes).unwrap();
    assert_eq!(fv.len(), 2);
    assert!(!fv.is_empty());
    let set = fv.condition_set(0).unwrap();
    assert_eq!(set.len(), 1);
    assert!(matches!(
        set.get(0),
        Some(Condition::AxisRange {
            axis_index: 0,
            min: 0,
            max: 16384
        })
    ));
    assert!(set.get(1).is_none());
    assert!(fv.condition_set(1).unwrap().is_empty());
    assert!(fv.condition_set(2).is_none());
    let s = fv.substitution(0).unwrap();
    assert_eq!(s.len(), 1);
    let (index, feature) = s.get(0).unwrap();
    assert_eq!(index, 2);
    assert_eq!(feature.lookup_indices().collect::<Vec<_>>(), vec![5]);
    assert!(s.get(1).is_none());
    assert!(fv.substitution(2).is_none());
}

#[test]
fn empty_table_selects_nothing() {
    let bytes = fv_bytes(&[]);
    let fv = FeatureVariations::parse(&bytes).unwrap();
    assert!(fv.is_empty());
    assert_eq!(fv.find_index(&[], None), None);
    assert!(fv.substitute(0, 0).is_none());
}

#[test]
fn rejects_unsupported_major_version() {
    let mut bytes = fv_bytes(&[]);
    bytes[0..2].copy_from_slice(&2u16.to_be_bytes());
    assert_eq!(
        FeatureVariations::parse(&bytes).unwrap_err(),
        Error::Malformed {
            offset: 0,
            context: "unsupported FeatureVariations major version",
        }
    );
}

#[test]
fn rejects_truncated_input() {
    let bytes = fv_bytes(&[record(&[], subst(&[]))]);
    // The header.
    for len in 0..8 {
        assert!(matches!(
            FeatureVariations::parse(&bytes[..len]),
            Err(Error::Truncated { .. })
        ));
    }
    // The records.
    for len in 8..16 {
        assert_eq!(
            FeatureVariations::parse(&bytes[..len]).unwrap_err(),
            Error::Truncated {
                offset: 8,
                context: "FeatureVariations records shorter than featureVariationRecordCount",
            }
        );
    }
    // A count that overflows the address space.
    let mut huge = fv_bytes(&[]);
    huge[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(FeatureVariations::parse(&huge).is_err());
}

#[test]
fn axis_range_includes_both_bounds() {
    let range = [Range(0, f2(-0.5), f2(0.5))];
    let step = 1.0 / 16384.0;
    assert!(holds(&range, &[-0.5]));
    assert!(holds(&range, &[0.5]));
    assert!(holds(&range, &[0.0]));
    assert!(!holds(&range, &[0.5 + step]));
    assert!(!holds(&range, &[-0.5 - step]));
    assert!(!holds(&range, &[1.0]));
}

#[test]
fn coordinates_round_to_f2dot14_like_harfbuzz() {
    let step = 1.0 / 16384.0;
    // Half a step below a bound rounds up onto it; less does not.
    let range = [Range(0, f2(0.5), f2(1.0))];
    assert!(holds(&range, &[0.5 - step / 2.0]));
    assert!(!holds(&range, &[0.5 - step * 0.6]));
    // HarfBuzz's roundf is floor(x + 0.5), so negative halves go up too.
    let range = [Range(0, f2(-1.0), f2(-0.5))];
    assert!(holds(&range, &[-0.5 - step / 2.0]));
    assert!(!holds(&range, &[-0.5 + step / 2.0]));
    assert_eq!(to_f2dot14(step / 2.0), 1);
    assert_eq!(to_f2dot14(-step / 2.0), 0);
    assert_eq!(to_f2dot14(-step * 1.5), -1);
    assert_eq!(to_f2dot14(-step * 1.6), -2);
    assert_eq!(to_f2dot14(step * 0.49), 0);
    assert_eq!(to_f2dot14(0.40625), 6656);
    assert_eq!(to_f2dot14(f32::NAN), 0);
    assert_eq!(to_f2dot14(3.0), 32767);
    assert_eq!(to_f2dot14(-3.0), -32768);
}

#[test]
fn missing_axes_read_as_zero() {
    let at_default = [Range(1, f2(-0.25), f2(0.25))];
    assert!(holds(&at_default, &[]));
    assert!(holds(&at_default, &[1.0]));
    assert!(!holds(&at_default, &[1.0, 0.5]));
    let heavy = [Range(0, f2(0.25), f2(1.0))];
    assert!(!holds(&heavy, &[]));
}

#[test]
fn every_condition_of_a_set_must_hold() {
    let set = [Range(0, f2(0.5), f2(1.0)), Range(1, f2(-1.0), 0)];
    assert!(holds(&set, &[0.75, -0.5]));
    assert!(!holds(&set, &[0.75, 0.5]));
    assert!(!holds(&set, &[0.25, -0.5]));
}

/// One axis, one region (0 to 1, peak 1), and one item whose delta is
/// `delta` at outer 0, inner 0.
fn store_bytes(delta: i16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&12u32.to_be_bytes()); // regionListOffset
    out.extend_from_slice(&1u16.to_be_bytes()); // subtable count
    out.extend_from_slice(&22u32.to_be_bytes()); // subtable offset
    out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
    out.extend_from_slice(&1u16.to_be_bytes()); // regionCount
    for v in [0i16, 16384, 16384] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(&1u16.to_be_bytes()); // itemCount
    out.extend_from_slice(&1u16.to_be_bytes()); // wordDeltaCount
    out.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
    out.extend_from_slice(&0u16.to_be_bytes()); // region index
    out.extend_from_slice(&delta.to_be_bytes());
    out
}

#[test]
fn value_condition_adds_the_variation_delta() {
    let store_data = store_bytes(-200);
    let store = ItemVariationStore::parse(&store_data).unwrap();
    let bytes = fv_bytes(&[record(&[Value(100, 0)], subst(&[]))]);
    let fv = FeatureVariations::parse(&bytes).unwrap();
    let at = |coords: &[f32]| fv.find_index(coords, Some(&store));
    // 100 - 200 * coord: above 0 below coord 0.5.
    assert_eq!(at(&[]), Some(0));
    assert_eq!(at(&[0.0]), Some(0));
    assert_eq!(at(&[0.25]), Some(0));
    assert_eq!(at(&[0.5]), None);
    assert_eq!(at(&[1.0]), None);
    // Without a store there is no delta.
    assert_eq!(fv.find_index(&[1.0], None), Some(0));
    let set = fv.condition_set(0).unwrap();
    assert!(set.matches(&[0.25], Some(&store)));
    assert!(!set.get(0).unwrap().matches(&[0.75], Some(&store)));
}

#[test]
fn value_condition_needs_a_positive_value() {
    assert!(holds(&[Value(1, NO_VARIATION_INDEX)], &[0.5]));
    assert!(!holds(&[Value(0, NO_VARIATION_INDEX)], &[0.5]));
    assert!(!holds(&[Value(-1, NO_VARIATION_INDEX)], &[]));
    // An index the store does not have has no delta.
    let store_data = store_bytes(-200);
    let store = ItemVariationStore::parse(&store_data).unwrap();
    let bytes = fv_bytes(&[record(&[Value(100, 0x0001_0000)], subst(&[]))]);
    let fv = FeatureVariations::parse(&bytes).unwrap();
    assert_eq!(fv.find_index(&[1.0], Some(&store)), Some(0));
    // Nor does the no-variation index.
    let bytes = fv_bytes(&[record(&[Value(100, NO_VARIATION_INDEX)], subst(&[]))]);
    let fv = FeatureVariations::parse(&bytes).unwrap();
    assert_eq!(fv.find_index(&[1.0], Some(&store)), Some(0));
}

#[test]
fn and_or_and_negate_combine_conditions() {
    let yes = Range(0, f2(-1.0), f2(1.0));
    let no = Range(0, f2(1.0), f2(1.0));
    let at = |c: Cond| holds(&[c], &[0.0]);
    assert!(at(And(vec![yes.clone(), yes.clone()])));
    assert!(!at(And(vec![yes.clone(), no.clone()])));
    assert!(at(And(vec![])));
    assert!(at(Or(vec![no.clone(), yes.clone()])));
    assert!(!at(Or(vec![no.clone(), no.clone()])));
    assert!(!at(Or(vec![])));
    assert!(!at(Not(Box::new(yes.clone()))));
    assert!(at(Not(Box::new(no.clone()))));
    assert!(at(Not(Box::new(Not(Box::new(yes.clone()))))));
    assert!(at(Or(vec![And(vec![no.clone()]), Not(Box::new(no))])));
}

#[test]
fn reads_nested_conditions() {
    let bytes = fv_bytes(&[record(
        &[And(vec![Range(2, 1, 3), Not(Box::new(Value(4, 5)))])],
        subst(&[]),
    )]);
    let fv = FeatureVariations::parse(&bytes).unwrap();
    let Some(Condition::And(list)) = fv.condition_set(0).unwrap().get(0) else {
        panic!("expected an and");
    };
    assert_eq!(list.len(), 2);
    assert!(!list.is_empty());
    assert!(matches!(
        list.get(0),
        Some(Condition::AxisRange {
            axis_index: 2,
            min: 1,
            max: 3
        })
    ));
    let Some(Condition::Negate(negation)) = list.get(1) else {
        panic!("expected a negate");
    };
    assert!(matches!(
        negation.condition(),
        Condition::Value {
            default_value: 4,
            var_index: 5
        }
    ));
    assert!(list.get(2).is_none());
    assert_eq!(list.iter().count(), 2);
}

#[test]
fn null_and_unknown_conditions_never_hold() {
    let yes = Range(0, f2(-1.0), f2(1.0));
    assert!(!holds(&[Null], &[]));
    assert!(!holds(&[yes.clone(), Null], &[]));
    assert!(!holds(&[Raw(0, vec![0; 6])], &[]));
    assert!(!holds(&[Raw(6, vec![0; 6])], &[]));
    assert!(!holds(&[Raw(0xFFFF, vec![])], &[]));
    assert!(!holds(&[Or(vec![Null, Null])], &[]));
    // Negating one that never holds holds.
    assert!(holds(&[Not(Box::new(Null))], &[]));
    assert!(holds(&[Not(Box::new(Raw(9, vec![])))], &[]));
    let bytes = fv_bytes(&[record(&[Raw(9, vec![1, 2])], subst(&[]))]);
    let fv = FeatureVariations::parse(&bytes).unwrap();
    let set = fv.condition_set(0).unwrap();
    assert!(matches!(set.get(0), Some(Condition::Unknown { format: 9 })));
    let bytes = fv_bytes(&[record(&[Null], subst(&[]))]);
    let fv = FeatureVariations::parse(&bytes).unwrap();
    let set = fv.condition_set(0).unwrap();
    assert!(matches!(set.get(0), Some(Condition::Unknown { format: 0 })));
}

/// A one-record table whose set (at 16) names one condition, placed at
/// the end of the table: `condition`. Returns the table and where the
/// condition starts.
fn with_condition_at_end(condition: &[u8]) -> (Vec<u8>, usize) {
    let mut bytes = fv_bytes(&[record(&[Range(0, 0, 0)], subst(&[]))]);
    let at = bytes.len();
    bytes[18..22].copy_from_slice(&((at - 16) as u32).to_be_bytes());
    bytes.extend_from_slice(condition);
    (bytes, at)
}

#[test]
fn rejects_truncated_conditions() {
    let cases = [
        cond_bytes(&Range(0, -16384, 16384)),
        cond_bytes(&Value(1, NO_VARIATION_INDEX)),
        cond_bytes(&And(vec![])),
        cond_bytes(&Or(vec![Range(0, 0, 0)])),
        cond_bytes(&Not(Box::new(Null))),
    ];
    for full in cases {
        let format = u16::from_be_bytes([full[0], full[1]]);
        // The bytes the format needs before any child: 8, 8, 3, 3 + 3,
        // and 5.
        let need = match format {
            1 | 2 => 8,
            3 => 3,
            4 => 6,
            _ => 5,
        };
        for len in 0..need {
            let (bytes, at) = with_condition_at_end(&full[..len]);
            let expected = match (format, len) {
                (_, 0) => Error::Malformed {
                    offset: 18,
                    context: "FeatureVariations offset past end of table",
                },
                (4, 3..) => Error::Truncated {
                    offset: at + 3,
                    context: "condition list shorter than conditionCount",
                },
                _ => Error::Truncated {
                    offset: at,
                    context: "condition shorter than its format",
                },
            };
            assert_eq!(
                FeatureVariations::parse(&bytes).unwrap_err(),
                expected,
                "format {format} cut to {len}"
            );
        }
        let (bytes, _) = with_condition_at_end(&full);
        assert!(FeatureVariations::parse(&bytes).is_ok(), "format {format}");
    }
    // An unknown format needs only its format field.
    let (bytes, _) = with_condition_at_end(&[0, 9]);
    assert!(FeatureVariations::parse(&bytes).is_ok());
}

/// `depth` nested "and" conditions around one that always holds, which
/// therefore sits at depth `depth + 1`.
fn nested_ands(depth: usize) -> Cond {
    let mut cond = Range(0, f2(-1.0), f2(1.0));
    for _ in 0..depth {
        cond = And(vec![cond]);
    }
    cond
}

#[test]
fn rejects_conditions_nested_deeper_than_sixty_four() {
    assert_eq!(MAX_CONDITION_DEPTH, 64);
    assert!(holds(&[nested_ands(0)], &[]));
    assert!(holds(&[nested_ands(63)], &[]));
    // The set is at 16 and names its condition at 22. Each "and" takes
    // 6 bytes, so the leaf under 64 of them, at depth 65, is at 406.
    let bytes = fv_bytes(&[record(&[nested_ands(64)], subst(&[]))]);
    assert_eq!(
        FeatureVariations::parse(&bytes).unwrap_err(),
        Error::Malformed {
            offset: 22 + 6 * 64,
            context: "conditions nested more than 64 deep",
        }
    );
    // Under a negation (5 bytes) at depth 1, 63 "and" conditions put the
    // leaf at depth 65.
    assert!(!holds(&[Not(Box::new(nested_ands(62)))], &[]));
    let bytes = fv_bytes(&[record(&[Not(Box::new(nested_ands(63)))], subst(&[]))]);
    assert_eq!(
        FeatureVariations::parse(&bytes).unwrap_err(),
        Error::Malformed {
            offset: 27 + 6 * 63,
            context: "conditions nested more than 64 deep",
        }
    );
}

#[test]
fn first_matching_record_wins() {
    let bytes = fv_bytes(&[
        record(&[Range(0, f2(0.5), f2(1.0))], subst(&[(0, &[10])])),
        record(&[Range(0, f2(-1.0), f2(0.25))], subst(&[(0, &[11])])),
        record(&[Range(0, f2(-1.0), f2(1.0))], subst(&[(0, &[12])])),
        record(&[], subst(&[(0, &[13])])),
    ]);
    let fv = FeatureVariations::parse(&bytes).unwrap();
    assert_eq!(fv.find_index(&[0.75], None), Some(0));
    assert_eq!(fv.find_index(&[0.0], None), Some(1));
    assert_eq!(fv.find_index(&[], None), Some(1));
    assert_eq!(fv.find_index(&[0.4], None), Some(2));
    assert_eq!(fv.find_index(&[1.5], None), Some(3));
    assert_eq!(lookups(fv.substitute(2, 0)), Some(vec![12]));
}

#[test]
fn empty_and_null_condition_sets_always_match() {
    let bytes = fv_bytes(&[
        Record {
            conditions: None,
            substitution: Some(subst(&[(0, &[1])])),
        },
        record(&[], subst(&[(0, &[2])])),
    ]);
    let fv = FeatureVariations::parse(&bytes).unwrap();
    assert!(fv.condition_set(0).unwrap().is_empty());
    assert_eq!(fv.find_index(&[], None), Some(0));
    assert_eq!(fv.find_index(&[1.0], None), Some(0));
    let bytes = fv_bytes(&[record(&[], subst(&[]))]);
    let fv = FeatureVariations::parse(&bytes).unwrap();
    assert_eq!(fv.find_index(&[-1.0], None), Some(0));
}

#[test]
fn rejects_a_condition_set_that_does_not_fit() {
    // The set (at 16) claims five conditions and the table ends after
    // two offsets.
    let mut bytes = fv_bytes(&[record(&[Range(0, 1, 1), Range(0, 1, 1)], subst(&[]))]);
    bytes[16..18].copy_from_slice(&5u16.to_be_bytes());
    bytes.truncate(16 + 2 + 4 * 2);
    assert_eq!(
        FeatureVariations::parse(&bytes).unwrap_err(),
        Error::Truncated {
            offset: 18,
            context: "ConditionSet shorter than conditionCount",
        }
    );
    // An offset past the end.
    let mut bytes = fv_bytes(&[record(&[Range(0, 1, 1)], subst(&[]))]);
    bytes[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        FeatureVariations::parse(&bytes).unwrap_err(),
        Error::Malformed {
            offset: 8,
            context: "FeatureVariations offset past end of table",
        }
    );
}

#[test]
fn substitution_scan_takes_the_first_record_for_a_feature() {
    let bytes = fv_bytes(&[record(&[], subst(&[(3, &[7, 8]), (1, &[9]), (3, &[10])]))]);
    let fv = FeatureVariations::parse(&bytes).unwrap();
    assert_eq!(lookups(fv.substitute(0, 3)), Some(vec![7, 8]));
    assert_eq!(lookups(fv.substitute(0, 1)), Some(vec![9]));
    assert_eq!(lookups(fv.substitute(0, 2)), None);
    assert_eq!(lookups(fv.substitute(1, 3)), None);
}

#[test]
fn rejects_an_unsupported_substitution_version() {
    let major_two = Subst {
        major: 2,
        records: vec![(0, Some(vec![4]))],
    };
    // The set (2 bytes, at 16) comes first, then the substitution.
    let bytes = fv_bytes(&[Record {
        conditions: Some(vec![]),
        substitution: Some(major_two),
    }]);
    assert_eq!(
        FeatureVariations::parse(&bytes).unwrap_err(),
        Error::Malformed {
            offset: 18,
            context: "unsupported FeatureTableSubstitution major version",
        }
    );
}

#[test]
fn null_substitution_substitutes_nothing() {
    let bytes = fv_bytes(&[
        Record {
            conditions: Some(vec![]),
            substitution: None,
        },
        record(&[], subst(&[(0, &[5])])),
    ]);
    let fv = FeatureVariations::parse(&bytes).unwrap();
    // The record still wins, so record 1 never applies.
    assert_eq!(fv.find_index(&[], None), Some(0));
    assert!(fv.substitution(0).unwrap().is_empty());
    assert!(fv.substitute(0, 0).is_none());
}

#[test]
fn rejects_a_truncated_substitution() {
    let full = fv_bytes(&[record(&[], subst(&[(0, &[5]), (1, &[6])]))]);
    let start = u32::from_be_bytes(full[12..16].try_into().unwrap()) as usize;
    // Header, two records, and two alternates of 6 bytes each.
    assert_eq!(full.len(), start + 6 + 2 * 6 + 2 * 6);
    for len in start..full.len() {
        assert!(
            FeatureVariations::parse(&full[..len]).is_err(),
            "cut to {len}"
        );
    }
    assert_eq!(
        FeatureVariations::parse(&full[..start + 10]).unwrap_err(),
        Error::Truncated {
            offset: start + 6,
            context: "FeatureTableSubstitution records shorter than substitutionCount",
        }
    );
    // The second alternate's lookup list ends a byte short.
    assert_eq!(
        FeatureVariations::parse(&full[..full.len() - 1]).unwrap_err(),
        Error::Truncated {
            offset: start + 24,
            context: "alternate Feature shorter than lookupIndexCount",
        }
    );
    assert!(FeatureVariations::parse(&full).is_ok());
}

#[test]
fn null_alternate_has_no_lookups() {
    let with_null = Subst {
        major: 1,
        records: vec![(0, None), (1, Some(vec![3]))],
    };
    let bytes = fv_bytes(&[Record {
        conditions: Some(vec![]),
        substitution: Some(with_null),
    }]);
    let fv = FeatureVariations::parse(&bytes).unwrap();
    assert_eq!(lookups(fv.substitute(0, 0)), Some(vec![]));
    assert_eq!(lookups(fv.substitute(0, 1)), Some(vec![3]));
}

#[test]
fn rejects_runaway_shared_subtrees() {
    // Six levels of "or" whose 255 entries all name the same next
    // level: 255^6 paths to check.
    let mut bytes = fv_bytes(&[record(&[Range(0, 0, 0)], subst(&[(0, &[1])]))]);
    let first = bytes.len();
    bytes[18..22].copy_from_slice(&((first - 16) as u32).to_be_bytes());
    let level = 3 + 255 * 3;
    for _ in 0..6 {
        bytes.extend_from_slice(&4u16.to_be_bytes());
        bytes.push(255);
        for _ in 0..255 {
            bytes.extend_from_slice(&(level as u32).to_be_bytes()[1..]);
        }
    }
    bytes.extend(cond_bytes(&Range(0, f2(1.0), f2(1.0))));
    assert!(matches!(
        FeatureVariations::parse(&bytes),
        Err(Error::Malformed {
            context: "FeatureVariations need more checks than sigilbuzz makes",
            ..
        })
    ));
}

#[test]
fn locate_reads_the_offset_from_the_table() {
    let fv = fv_bytes(&[record(&[], subst(&[]))]);
    let mut table = vec![0u8; 14];
    table.extend_from_slice(&fv);
    assert_eq!(locate(&table, 0, "ctx").unwrap().map(|v| v.len()), None);
    assert_eq!(locate(&table, 14, "ctx").unwrap().map(|v| v.len()), Some(1));
    assert_eq!(
        locate(&table, table.len() as u32 + 1, "ctx").unwrap_err(),
        Error::Malformed {
            offset: 10,
            context: "ctx",
        }
    );
    assert!(locate(&table, table.len() as u32, "ctx").is_err());
}
