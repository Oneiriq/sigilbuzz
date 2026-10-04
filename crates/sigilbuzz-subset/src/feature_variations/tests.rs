//! Hand-built GSUB tables with FeatureVariations: reading them, the
//! subset remap, and each rule of the instancer's condition settling.
//! Shaping parity against rustybuzz on real fonts lives in the
//! `feature_variations` integration test.

use alloc::vec;
use alloc::vec::Vec;

use sigilbuzz::tables::gsub::Gsub;
use sigilbuzz::Error;

use super::{instance_table, read, subset, Condition, FeatureVariations};
use crate::warnings::{Diag, Warnings};

const F2DOT14_ONE: i16 = 0x4000;

fn push(out: &mut Vec<u8>, values: &[u16]) {
    for v in values {
        out.extend_from_slice(&v.to_be_bytes());
    }
}

/// `(axis, min, max)` conditions and `(feature, lookups)` substitutions
/// of one record.
type Rec = (Vec<(u16, i16, i16)>, Vec<(u16, Vec<u16>)>);

/// A FeatureVariations table with one ConditionSet and one
/// FeatureTableSubstitution per record, written by hand.
fn feature_variations(records: &[Rec]) -> Vec<u8> {
    let mut out = Vec::new();
    push(&mut out, &[1, 0]);
    out.extend_from_slice(&(records.len() as u32).to_be_bytes());
    out.resize(8 + records.len() * 8, 0);
    for (i, (conditions, substitutions)) in records.iter().enumerate() {
        let set = out.len() as u32;
        push(&mut out, &[conditions.len() as u16]);
        let first = 2 + conditions.len() * 4;
        for k in 0..conditions.len() {
            out.extend_from_slice(&((first + k * 8) as u32).to_be_bytes());
        }
        for &(axis, min, max) in conditions {
            push(&mut out, &[1, axis, min as u16, max as u16]);
        }
        let fts = out.len();
        push(&mut out, &[1, 0, substitutions.len() as u16]);
        let recs = out.len();
        out.resize(recs + substitutions.len() * 6, 0);
        for (k, (feature, lookups)) in substitutions.iter().enumerate() {
            let at = (out.len() - fts) as u32;
            out[recs + k * 6..recs + k * 6 + 2].copy_from_slice(&feature.to_be_bytes());
            out[recs + k * 6 + 2..recs + k * 6 + 6].copy_from_slice(&at.to_be_bytes());
            push(&mut out, &[0, lookups.len() as u16]);
            push(&mut out, lookups);
        }
        out[8 + i * 8..12 + i * 8].copy_from_slice(&set.to_be_bytes());
        out[12 + i * 8..16 + i * 8].copy_from_slice(&(fts as u32).to_be_bytes());
    }
    out
}

/// A GSUB with a DFLT script listing every feature, `features` as
/// `(tag, lookups)`, `lookups` empty type 1 lookups, and `fv` behind
/// them when given (version 1.1).
fn gsub(features: &[([u8; 4], Vec<u16>)], lookups: u16, fv: Option<&[u8]>) -> Vec<u8> {
    let mut scripts = Vec::new();
    push(&mut scripts, &[1]);
    scripts.extend_from_slice(b"DFLT");
    push(&mut scripts, &[8, 4, 0, 0, 0xFFFF, features.len() as u16]);
    for i in 0..features.len() {
        push(&mut scripts, &[i as u16]);
    }
    let mut list = Vec::new();
    push(&mut list, &[features.len() as u16]);
    list.resize(2 + features.len() * 6, 0);
    for (i, (tag, feature_lookups)) in features.iter().enumerate() {
        let at = list.len() as u16;
        list[2 + i * 6..6 + i * 6].copy_from_slice(tag);
        list[6 + i * 6..8 + i * 6].copy_from_slice(&at.to_be_bytes());
        push(&mut list, &[0, feature_lookups.len() as u16]);
        push(&mut list, feature_lookups);
    }
    let mut lookup_list = Vec::new();
    push(&mut lookup_list, &[lookups]);
    for i in 0..lookups {
        push(&mut lookup_list, &[2 + lookups * 2 + i * 6]);
    }
    for _ in 0..lookups {
        push(&mut lookup_list, &[1, 0, 0]);
    }
    let header = if fv.is_some() { 14 } else { 10 };
    let mut out = Vec::new();
    let feature_at = header + scripts.len();
    let lookup_at = feature_at + list.len();
    push(
        &mut out,
        &[
            1,
            u16::from(fv.is_some()),
            header as u16,
            feature_at as u16,
            lookup_at as u16,
        ],
    );
    if fv.is_some() {
        out.extend_from_slice(&[0; 4]);
    }
    out.extend_from_slice(&scripts);
    out.extend_from_slice(&list);
    out.extend_from_slice(&lookup_list);
    if let Some(fv) = fv {
        let at = out.len() as u32;
        out[10..14].copy_from_slice(&at.to_be_bytes());
        out.extend_from_slice(fv);
    }
    out
}

/// The conditions and substitutions of every record, as shapers see
/// them.
fn records_of(fv: &FeatureVariations) -> Vec<Rec> {
    fv.records
        .iter()
        .map(|r| {
            let conditions = r
                .conditions
                .iter()
                .map(|c| match *c {
                    Condition::AxisRange { axis, min, max } => (axis, min, max),
                    Condition::Unknown { .. } => panic!("unexpected unknown condition"),
                })
                .collect();
            let subs = r
                .substitutions
                .iter()
                .map(|s| (s.feature, s.lookups.clone()))
                .collect();
            (conditions, subs)
        })
        .collect()
}

/// `(tag, lookups)` of every feature of a GSUB.
fn features_of(table: &[u8]) -> Vec<([u8; 4], Vec<u16>)> {
    let parsed = Gsub::parse(table).expect("the GSUB parses");
    parsed
        .feature_list()
        .iter()
        .map(|(tag, f)| (tag, f.lookup_indices().collect()))
        .collect()
}

fn two_features() -> Vec<([u8; 4], Vec<u16>)> {
    vec![(*b"rvrn", vec![]), (*b"liga", vec![0])]
}

#[test]
fn reads_records_and_treats_null_offsets_as_empty() {
    let fv = feature_variations(&[
        (vec![(0, 100, F2DOT14_ONE)], vec![(0, vec![1])]),
        (vec![], vec![(1, vec![])]),
    ]);
    let table = gsub(&two_features(), 2, Some(&fv));
    let parsed = read(&table).unwrap().expect("1.1 with variations");
    assert_eq!(
        records_of(&parsed),
        [
            (vec![(0, 100, F2DOT14_ONE)], vec![(0, vec![1])]),
            (vec![], vec![(1, vec![])]),
        ]
    );

    // Null ConditionSet and FeatureTableSubstitution offsets.
    let mut nulls = fv.clone();
    nulls[8..16].copy_from_slice(&[0; 8]);
    let table = gsub(&two_features(), 2, Some(&nulls));
    let parsed = read(&table).unwrap().unwrap();
    assert_eq!(records_of(&parsed)[0], (vec![], vec![]));

    // Version 1.0, and 1.1 with a null offset: nothing to read.
    assert_eq!(read(&gsub(&two_features(), 2, None)), Ok(None));
    let mut null = gsub(&two_features(), 2, Some(&fv));
    null[10..14].copy_from_slice(&[0; 4]);
    assert_eq!(read(&null), Ok(None));
}

#[test]
fn unreadable_tables_report_where() {
    let fv = feature_variations(&[(vec![(0, 100, F2DOT14_ONE)], vec![(0, vec![1])])]);
    let table = gsub(&two_features(), 2, Some(&fv));
    let at = u32::from_be_bytes([table[10], table[11], table[12], table[13]]) as usize;
    // Offset past the table, at the header slot.
    let mut far = table.clone();
    far[10..14].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(matches!(
        read(&far),
        Err(Error::Malformed { offset: 10, .. })
    ));
    // A record count running past the table.
    let mut long = table.clone();
    long[at + 4..at + 8].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(matches!(read(&long), Err(Error::Truncated { offset, .. }) if offset == at + 8));
    // A truncated alternate Feature.
    let cut = &table[..table.len() - 1];
    assert!(read(cut).is_err());
}

#[test]
fn a_subset_remaps_features_and_lookups_and_trims_trailing_records() {
    let fv = feature_variations(&[
        (
            vec![(0, 100, F2DOT14_ONE)],
            vec![(0, vec![1, 2]), (1, vec![2])],
        ),
        (vec![(0, -F2DOT14_ONE, -100)], vec![(1, vec![1])]),
        (vec![(1, 0, 50)], vec![(0, vec![2])]),
        (vec![(1, 60, 70)], vec![(1, vec![2])]),
    ]);
    let table = gsub(&two_features(), 3, Some(&fv));
    let parsed = read(&table).unwrap().unwrap();
    // Lookup 1 drops, lookup 2 becomes 1; feature 1 drops, feature 0
    // stays 0. Record 3 only substituted feature 1 and goes.
    let lookup_renumber = [Some(0), None, Some(1)];
    let feature_renumber = [Some(0), None];
    let out = subset(&parsed, &feature_renumber, &lookup_renumber, &Diag::NONE)
        .unwrap()
        .expect("a record still substitutes");
    let rebuilt = gsub(&two_features(), 2, Some(&out));
    let records = records_of(&read(&rebuilt).unwrap().unwrap());
    assert_eq!(
        records,
        [
            (vec![(0, 100, F2DOT14_ONE)], vec![(0, vec![1])]),
            // Kept, now empty: it still stops record 2 from applying.
            (vec![(0, -F2DOT14_ONE, -100)], vec![]),
            (vec![(1, 0, 50)], vec![(0, vec![1])]),
        ]
    );

    // With lookup 2 gone too, the alternates of feature 0 are empty but
    // still stand in for its default lookups where they apply.
    let out = subset(
        &parsed,
        &feature_renumber,
        &[Some(0), None, None],
        &Diag::NONE,
    )
    .unwrap()
    .unwrap();
    let rebuilt = gsub(&two_features(), 1, Some(&out));
    let records = records_of(&read(&rebuilt).unwrap().unwrap());
    assert_eq!(
        records,
        [
            (vec![(0, 100, F2DOT14_ONE)], vec![(0, vec![])]),
            (vec![(0, -F2DOT14_ONE, -100)], vec![]),
            (vec![(1, 0, 50)], vec![(0, vec![])]),
        ]
    );

    // With feature 0 gone as well, nothing is left.
    assert_eq!(
        subset(&parsed, &[None, None], &lookup_renumber, &Diag::NONE),
        Ok(None)
    );
}

#[test]
fn alternates_keep_their_features_alive() {
    let fv = feature_variations(&[(vec![(0, 100, F2DOT14_ONE)], vec![(0, vec![1])])]);
    let table = gsub(&two_features(), 2, Some(&fv));
    let parsed = read(&table).unwrap().unwrap();
    assert_eq!(
        parsed.features_with_live_alternates(2, &[Some(0), Some(1)]),
        [true, false]
    );
    assert_eq!(
        parsed.features_with_live_alternates(2, &[Some(0), None]),
        [false, false]
    );
}

#[test]
fn records_with_unreadable_conditions_are_dropped_and_reported() {
    let fv = feature_variations(&[
        (vec![(0, 100, F2DOT14_ONE)], vec![(0, vec![1])]),
        (vec![(1, 0, 50)], vec![(0, vec![1])]),
    ]);
    let mut table = gsub(&two_features(), 2, Some(&fv));
    let at = u32::from_be_bytes([table[10], table[11], table[12], table[13]]) as usize;
    let set = at
        + u32::from_be_bytes([table[at + 8], table[at + 9], table[at + 10], table[at + 11]])
            as usize;
    let condition = set + 6;
    table[condition..condition + 2].copy_from_slice(&5u16.to_be_bytes());
    let parsed = read(&table).unwrap().unwrap();
    assert_eq!(
        parsed.features_with_live_alternates(2, &[Some(0), Some(1)]),
        [true, false],
        "the second record still counts"
    );
    let sink = Warnings::default();
    let diag = Diag::new(&sink).for_table(*b"GSUB", &table);
    let out = subset(&parsed, &[Some(0), Some(1)], &[Some(0), Some(1)], &diag)
        .unwrap()
        .unwrap();
    let rebuilt = gsub(&two_features(), 2, Some(&out));
    assert_eq!(
        records_of(&read(&rebuilt).unwrap().unwrap()),
        [(vec![(1, 0, 50)], vec![(0, vec![1])])]
    );
    let warnings = sink.into_sorted();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].offset, condition);
}

/// Instances `table` with `pinned` / `new_axis` and returns the result
/// and its warnings.
fn instanced(
    table: &[u8],
    pinned: &[Option<i16>],
    new_axis: &[Option<u16>],
) -> (Option<Vec<u8>>, Vec<crate::SubsetWarning>) {
    let sink = Warnings::default();
    let out = instance_table(table, *b"GSUB", pinned, new_axis, &sink).unwrap();
    (out, sink.into_sorted())
}

/// Four records over two axes, all on feature 0 (`rvrn`, empty by
/// default): 0 needs axis 0 high; 1 needs axis 1 high and axis 0 above
/// a quarter; 2 needs axis 0 above a half and turns the feature off;
/// 3 needs axis 1 low.
fn two_axis_table() -> Vec<u8> {
    let q = F2DOT14_ONE / 4;
    let fv = feature_variations(&[
        (vec![(0, 3 * q, F2DOT14_ONE)], vec![(0, vec![0])]),
        (
            vec![(1, 2 * q, F2DOT14_ONE), (0, q, F2DOT14_ONE)],
            vec![(0, vec![1])],
        ),
        (vec![(0, 2 * q, F2DOT14_ONE)], vec![(0, vec![])]),
        (vec![(1, -F2DOT14_ONE, -2 * q)], vec![(0, vec![1])]),
    ]);
    gsub(&two_features(), 2, Some(&fv))
}

#[test]
fn pinning_settles_conditions_and_renumbers_the_kept_axis() {
    let q = F2DOT14_ONE / 4;
    let table = two_axis_table();
    // Pin axis 0 at 0.6: record 0 cannot match, record 1 keeps only its
    // axis 1 condition (now axis 0), record 2 always holds and is
    // kept, record 3 can never be first any more.
    let (out, warnings) = instanced(&table, &[Some(10_000), None], &[None, Some(0)]);
    let out = out.expect("rewritten");
    assert!(warnings.is_empty());
    assert_eq!(u16::from_be_bytes([out[2], out[3]]), 1, "still 1.1");
    assert_eq!(features_of(&out), features_of(&table), "FeatureList as is");
    let records = records_of(&read(&out).unwrap().unwrap());
    assert_eq!(
        records,
        [
            (vec![(0, 2 * q, F2DOT14_ONE)], vec![(0, vec![1])]),
            (vec![], vec![(0, vec![])]),
        ]
    );

    // Pin axis 1 at -0.75: record 1 cannot match, record 3 always holds.
    let (out, _) = instanced(&table, &[None, Some(-3 * q)], &[Some(0), None]);
    let records = records_of(&read(&out.unwrap()).unwrap().unwrap());
    assert_eq!(
        records,
        [
            (vec![(0, 3 * q, F2DOT14_ONE)], vec![(0, vec![0])]),
            (vec![(0, 2 * q, F2DOT14_ONE)], vec![(0, vec![])]),
            (vec![], vec![(0, vec![1])]),
        ]
    );
}

#[test]
fn a_first_record_that_always_holds_becomes_the_default() {
    // Axis 0 at 0.9: record 0 always applies, so rvrn takes lookup 0
    // outright and the variations go.
    let table = two_axis_table();
    let (out, _) = instanced(&table, &[Some(14_000), None], &[None, Some(0)]);
    let out = out.unwrap();
    assert_eq!(u16::from_be_bytes([out[2], out[3]]), 0, "now 1.0");
    assert_eq!(read(&out), Ok(None));
    assert_eq!(
        features_of(&out),
        [(*b"rvrn", vec![0]), (*b"liga", vec![0])]
    );
    // Everything else reads as before.
    let (before, after) = (Gsub::parse(&table).unwrap(), Gsub::parse(&out).unwrap());
    assert_eq!(before.lookup_list().len(), after.lookup_list().len());
    let script = |t: &Gsub<'_>| {
        let s = t.script_list().find(*b"DFLT").expect("DFLT");
        let ls = s.default_lang_sys().expect("default LangSys");
        ls.feature_indices().collect::<Vec<u16>>()
    };
    assert_eq!(script(&before), script(&after));
}

#[test]
fn full_instancing_keeps_only_the_matching_record() {
    let q = F2DOT14_ONE / 4;
    let table = two_axis_table();
    // (0.6, 0.6): record 1 matches first.
    let (out, _) = instanced(&table, &[Some(10_000), Some(10_000)], &[None, None]);
    let out = out.unwrap();
    assert_eq!(features_of(&out)[0], (*b"rvrn", vec![1]));
    // (0.1, 0): nothing matches; the defaults stay.
    let (out, _) = instanced(&table, &[Some(q / 2), Some(0)], &[None, None]);
    let out = out.unwrap();
    assert_eq!(read(&out), Ok(None));
    assert_eq!(features_of(&out), features_of(&table));
}

#[test]
fn unreadable_variations_are_dropped_and_reported() {
    let mut table = two_axis_table();
    table[10..14].copy_from_slice(&u32::MAX.to_be_bytes());
    let (out, warnings) = instanced(&table, &[Some(0), None], &[None, Some(0)]);
    let out = out.unwrap();
    assert_eq!(read(&out), Ok(None));
    assert_eq!(warnings.len(), 1);
    assert_eq!(
        (warnings[0].offset, warnings[0].dropped),
        (10, "the FeatureVariations")
    );
    // A table without variations is left alone, and a stub too short
    // to hold a header is only reported.
    let plain = gsub(&two_features(), 2, None);
    assert_eq!(instanced(&plain, &[Some(0)], &[None]).0, None);
    let (out, warnings) = instanced(&[0, 1, 0], &[Some(0)], &[None]);
    assert_eq!(out, Some(vec![0, 1, 0]));
    assert_eq!(warnings.len(), 1);
}

#[test]
fn folding_keeps_an_alternate_s_feature_params_reachable() {
    // One record that always applies at the pin, substituting rvrn with
    // an alternate that carries FeatureParams (two marker bytes appended
    // to the table).
    let fv = feature_variations(&[(vec![(0, 0, F2DOT14_ONE)], vec![(0, vec![1])])]);
    let mut table = gsub(&two_features(), 2, Some(&fv));
    let alternate = read(&table).unwrap().unwrap().records[0].substitutions[0].alternate;
    let params = table.len();
    table.extend_from_slice(&[0xAB, 0xCD]);
    let rel = (params - alternate) as u16;
    table[alternate..alternate + 2].copy_from_slice(&rel.to_be_bytes());

    let (out, _) = instanced(&table, &[Some(100)], &[None]);
    let out = out.unwrap();
    assert_eq!(features_of(&out)[0], (*b"rvrn", vec![1]));
    let u16_at = |pos: usize| usize::from(u16::from_be_bytes([out[pos], out[pos + 1]]));
    let list = u16_at(6);
    let feature = list + u16_at(list + 6);
    let params = feature + u16_at(feature);
    assert_eq!(&out[params..params + 2], &[0xAB, 0xCD]);
}

#[test]
fn lists_inside_the_header_cannot_be_folded() {
    // ScriptList and FeatureList offsets that point into the header: the
    // fold cannot move them, so it reports the variations as unreadable
    // and just drops them.
    let fv = feature_variations(&[(vec![(0, 0, F2DOT14_ONE)], vec![(0, vec![1])])]);
    let mut table = gsub(&two_features(), 2, Some(&fv));
    table[6..8].copy_from_slice(&4u16.to_be_bytes());
    table[4..6].copy_from_slice(&0u16.to_be_bytes());
    let (out, warnings) = instanced(&table, &[Some(100)], &[None]);
    let out = out.unwrap();
    assert_eq!(read(&out), Ok(None));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
}

/// Records that all share one condition set of 65535 conditions name
/// far more conditions than the table holds. Reading them stops once
/// the work budget runs out instead of reading every copy.
#[test]
fn shared_condition_sets_are_charged_to_a_budget() {
    let records: u32 = 400;
    let conditions = u16::MAX;
    // GSUB 1.1 header with the FeatureVariations right after it.
    let mut table = Vec::new();
    push(&mut table, &[1, 1, 0, 0, 0]);
    table.extend_from_slice(&14u32.to_be_bytes());
    let fv = table.len();
    push(&mut table, &[1, 0]);
    table.extend_from_slice(&records.to_be_bytes());
    let set = 8 + records as usize * 8;
    for _ in 0..records {
        table.extend_from_slice(&(set as u32).to_be_bytes());
        table.extend_from_slice(&0u32.to_be_bytes());
    }
    // The condition set, every condition pointing at one AxisRange.
    push(&mut table, &[conditions]);
    let condition = 2 + usize::from(conditions) * 4;
    for _ in 0..conditions {
        table.extend_from_slice(&(condition as u32).to_be_bytes());
    }
    push(&mut table, &[1, 0, 0, 0x4000]);
    assert_eq!(table.len(), fv + set + condition + 8);
    assert!(matches!(
        read(&table),
        Err(Error::Malformed { offset, .. }) if offset == fv
    ));
}

/// Records that share a condition set and a substitution share their
/// parse: memory follows the table, not the records naming it.
#[test]
fn records_share_the_sets_they_name() {
    let records: u32 = 200;
    let conditions: u16 = 100;
    let mut table = Vec::new();
    push(&mut table, &[1, 1, 0, 0, 0]);
    table.extend_from_slice(&14u32.to_be_bytes());
    push(&mut table, &[1, 0]);
    table.extend_from_slice(&records.to_be_bytes());
    let set = 8 + records as usize * 8;
    let condition = 2 + usize::from(conditions) * 4;
    let fts = set + condition + 8;
    for _ in 0..records {
        table.extend_from_slice(&(set as u32).to_be_bytes());
        table.extend_from_slice(&(fts as u32).to_be_bytes());
    }
    push(&mut table, &[conditions]);
    for _ in 0..conditions {
        table.extend_from_slice(&(condition as u32).to_be_bytes());
    }
    push(&mut table, &[1, 0, 0, 0x4000]);
    // A FeatureTableSubstitution 1.0 with no records.
    push(&mut table, &[1, 0, 0]);
    let fv = read(&table).unwrap().unwrap();
    assert_eq!(fv.records.len(), 200);
    let first = &fv.records[0];
    assert_eq!(first.conditions.len(), 100);
    for record in &fv.records {
        assert!(alloc::rc::Rc::ptr_eq(&record.conditions, &first.conditions));
        assert!(alloc::rc::Rc::ptr_eq(
            &record.substitutions,
            &first.substitutions
        ));
    }
}
