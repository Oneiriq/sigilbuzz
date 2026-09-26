//! Unit tests for the VARC subsetter: record walking and rewriting,
//! coverage, CFF2 INDEX round trips, and MultiVarStore region pruning.

use super::component::rewrite_component_gids;
use super::mvs::{
    build_region_list_bytes, collect_referenced_regions, parse_region_list, RewrittenMvsSubtable,
};
use super::*;
use alloc::vec;

/// Builds a coverage format-1 table.
fn build_coverage(gids: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(gids.len() as u16).to_be_bytes());
    for g in gids {
        out.extend_from_slice(&g.to_be_bytes());
    }
    out
}

/// Builds a CFF2 INDEX with 1-byte offsets.
fn build_cff2_index_test(entries: &[&[u8]]) -> Vec<u8> {
    let count = entries.len() as u32;
    let mut out = Vec::new();
    out.extend_from_slice(&count.to_be_bytes());
    if entries.is_empty() {
        return out;
    }
    out.push(1);
    let mut cursor: u32 = 1;
    out.push(cursor as u8);
    for e in entries {
        cursor += e.len() as u32;
        out.push(cursor as u8);
    }
    for e in entries {
        out.extend_from_slice(e);
    }
    out
}

/// Builds a synthetic VARC with the listed coverage gids and raw
/// glyph records. Returns the assembled bytes.
fn build_varc(coverage_gids: &[u16], glyph_records: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    let cov_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // varStore
    out.extend_from_slice(&0u32.to_be_bytes()); // conditionList
    out.extend_from_slice(&0u32.to_be_bytes()); // axisIndicesList
    let gr_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());

    let cov_off = out.len() as u32;
    out[cov_slot..cov_slot + 4].copy_from_slice(&cov_off.to_be_bytes());
    out.extend_from_slice(&build_coverage(coverage_gids));

    let gr_off = out.len() as u32;
    out[gr_slot..gr_slot + 4].copy_from_slice(&gr_off.to_be_bytes());
    out.extend_from_slice(&build_cff2_index_test(glyph_records));
    out
}

/// Builds a single-component record carrying just translate_x/y.
fn build_translate_record(gid: u16, tx: i16, ty: i16) -> Vec<u8> {
    let flags = VC_HAVE_TRANSLATE_X | VC_HAVE_TRANSLATE_Y;
    let mut record = Vec::new();
    #[allow(clippy::cast_possible_truncation)]
    record.push(flags as u8);
    record.extend_from_slice(&gid.to_be_bytes());
    record.extend_from_slice(&tx.to_be_bytes());
    record.extend_from_slice(&ty.to_be_bytes());
    record
}

#[test]
fn parse_finds_glyph_records_and_coverage() {
    let rec = build_translate_record(7, 10, 20);
    let bytes = build_varc(&[1], &[&rec]);
    let parsed = ParsedVarc::parse(&bytes).unwrap();
    assert_eq!(parsed.glyph_records.len(), 1);
    assert_eq!(parsed.coverage_index_of(1), Some(0));
    assert_eq!(parsed.coverage_index_of(99), None);
}

#[test]
fn walk_component_gids_returns_referenced_gid() {
    let rec = build_translate_record(7, 10, 20);
    assert_eq!(walk_component_gids(&rec), vec![7u16]);
}

#[test]
fn walk_component_gids_returns_all_components() {
    let mut rec = build_translate_record(5, 10, 20);
    rec.extend(build_translate_record(9, 30, 40));
    assert_eq!(walk_component_gids(&rec), vec![5, 9]);
}

#[test]
fn coverage_iter_format1_walks_in_source_order() {
    let cov = build_coverage(&[1, 5, 10]);
    let pairs: Vec<_> = CoverageIter::new(&cov).collect();
    assert_eq!(pairs, vec![(1u16, 0usize), (5, 1), (10, 2)]);
}

#[test]
fn coverage_iter_format2_yields_one_pair_per_gid_in_range() {
    // Format 2 with one range 100..=102 starting at coverage idx 5.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&2u16.to_be_bytes()); // format
    bytes.extend_from_slice(&1u16.to_be_bytes()); // rangeCount
    bytes.extend_from_slice(&100u16.to_be_bytes()); // start
    bytes.extend_from_slice(&102u16.to_be_bytes()); // end
    bytes.extend_from_slice(&5u16.to_be_bytes()); // startCov
    let pairs: Vec<_> = CoverageIter::new(&bytes).collect();
    assert_eq!(pairs, vec![(100u16, 5usize), (101, 6), (102, 7)]);
}

#[test]
fn build_coverage_emits_format_1() {
    let cov = build_coverage_format1([1u16, 5, 10]);
    assert_eq!(&cov[0..2], &1u16.to_be_bytes()); // format
    assert_eq!(&cov[2..4], &3u16.to_be_bytes()); // count
    assert_eq!(&cov[4..6], &1u16.to_be_bytes());
    assert_eq!(&cov[6..8], &5u16.to_be_bytes());
    assert_eq!(&cov[8..10], &10u16.to_be_bytes());
}

#[test]
fn build_coverage_handles_empty_input() {
    let cov = build_coverage_format1(core::iter::empty());
    assert_eq!(&cov[0..2], &1u16.to_be_bytes());
    assert_eq!(&cov[2..4], &0u16.to_be_bytes());
    assert_eq!(cov.len(), 4);
}

#[test]
fn walk_component_gids_handles_24bit_gid() {
    // VC_GID_IS_24BIT (1<<12 = 0x1000) requires a 2-byte uint32var:
    // 0x80|0x10 = 0x90, 0x00.
    let mut record = Vec::new();
    record.push(0x90);
    record.push(0x00);
    record.extend_from_slice(&[0x00, 0x12, 0x34]); // 24-bit gid 0x1234
    assert_eq!(walk_component_gids(&record), vec![0x1234]);
}

#[test]
fn rewrite_component_gids_renumbers_basic_record() {
    let rec = build_translate_record(7, 10, 20);
    let map = |g: u16| if g == 7 { Some(42) } else { None };
    let new_rec = rewrite_component_gids(&rec, &map).unwrap();
    // Same length, but the embedded gid is now 42.
    assert_eq!(new_rec.len(), rec.len());
    assert_eq!(walk_component_gids(&new_rec), vec![42u16]);
}

#[test]
fn rewrite_component_gids_renumbers_multi_component_record() {
    let mut rec = build_translate_record(5, 10, 20);
    rec.extend(build_translate_record(9, 30, 40));
    let map = |g: u16| match g {
        5 => Some(1),
        9 => Some(2),
        _ => None,
    };
    let new_rec = rewrite_component_gids(&rec, &map).unwrap();
    assert_eq!(walk_component_gids(&new_rec), vec![1u16, 2]);
    assert_eq!(new_rec.len(), rec.len());
}

#[test]
fn rewrite_preserves_24bit_width() {
    // 24-bit gid must stay 24-bit on output even when the new gid
    // would fit in 16 bits. Keeps record byte length stable.
    let mut record = Vec::new();
    record.push(0x90);
    record.push(0x00);
    record.extend_from_slice(&[0x00, 0x12, 0x34]); // gid 0x1234
    let map = |g: u16| if g == 0x1234 { Some(7) } else { None };
    let new_record = rewrite_component_gids(&record, &map).unwrap();
    assert_eq!(new_record.len(), record.len());
    assert_eq!(walk_component_gids(&new_record), vec![7u16]);
}

#[test]
fn rewrite_errors_when_kept_gid_lacks_mapping() {
    let rec = build_translate_record(7, 10, 20);
    // Map returns None for the source gid, so rewriter must error.
    let map = |_: u16| None;
    let err = rewrite_component_gids(&rec, &map).unwrap_err();
    assert!(matches!(err, SubsetError::Unsupported(_)));
}

/// VARC closure walker must terminate on a cyclic component graph
/// (gid A references gid B, gid B references gid A). The fixed-
/// point loop should converge after one iteration once both gids
/// are in the kept set, regardless of the cycle.
#[test]
fn closure_terminates_on_circular_components() {
    // Two records: idx 0 covering gid 1 references gid 2; idx 1
    // covering gid 2 references gid 1.
    let cov = build_coverage(&[1, 2]);
    let rec_a = build_translate_record(2, 0, 0); // gid 1 -> gid 2
    let rec_b = build_translate_record(1, 0, 0); // gid 2 -> gid 1
    let bytes = build_varc(&[1, 2], &[&rec_a, &rec_b]);
    let _ = cov; // silence unused (build_varc constructs its own)
    let parsed = ParsedVarc::parse(&bytes).expect("parses");

    // Manually drive the cycle: start with gid 1, then iterate.
    let mut keep: alloc::collections::BTreeSet<GlyphId> = alloc::collections::BTreeSet::new();
    keep.insert(1);

    let mut iterations = 0;
    loop {
        let before = keep.len();
        let snapshot: alloc::vec::Vec<GlyphId> = keep.iter().copied().collect();
        for g in snapshot {
            let Some(idx) = parsed.coverage_index_of(g) else {
                continue;
            };
            let Some(record) = parsed.glyph_record(idx) else {
                continue;
            };
            for child in walk_component_gids(record) {
                keep.insert(child);
            }
        }
        iterations += 1;
        if keep.len() == before {
            break;
        }
        assert!(
            iterations < 10,
            "VARC cycle walker must terminate quickly; iter={iterations}",
        );
    }
    // Both gids end up in the kept set, no infinite loop.
    assert!(keep.contains(&1));
    assert!(keep.contains(&2));
}

/// Regression for #196: a 24-bit gid with a non-zero high byte
/// (>0xFFFF) used to truncate silently to its low 16 bits. The
/// closure walker would then claim a wrong glyph was referenced.
/// The walker must skip such records cleanly instead of fabricating
/// a fake gid in the kept set.
#[test]
fn walk_skips_24bit_gid_overflowing_u16() {
    // VC_GID_IS_24BIT (1<<12 = 0x1000) -> uint32var encoding is the
    // two-byte form (0x80..=0xBF first byte): 0x90, 0x00.
    // 24-bit gid 0x010005: high byte non-zero, doesn't fit u16.
    let mut record = Vec::new();
    record.push(0x90);
    record.push(0x00);
    record.extend_from_slice(&[0x01, 0x00, 0x05]);
    // Walker must NOT yield 0x0005 (the truncated low bits). That
    // would lie about the source's reference graph.
    let gids = walk_component_gids(&record);
    assert!(
        gids.is_empty(),
        "u24 gid > 0xFFFF must be rejected, not silently truncated; got {gids:?}",
    );
}

#[test]
fn cff2_index_round_trips_through_parser() {
    let entries: Vec<Vec<u8>> = vec![vec![0xAAu8, 0xBB], vec![0xCC, 0xDD, 0xEE]];
    let block = build_cff2_index(&entries);
    let parsed = parse_cff2_index(&block).unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0], &[0xAA, 0xBB][..]);
    assert_eq!(parsed[1], &[0xCC, 0xDD, 0xEE][..]);
}

#[test]
fn subset_drops_table_when_no_covered_gid_kept() {
    // Coverage covers gid 5 only; kept set has only gid 2 -> drop.
    let rec = build_translate_record(7, 0, 0);
    let bytes = build_varc(&[5], &[&rec]);
    let varc = sigilbuzz::tables::Varc::parse(&bytes).unwrap();
    let map = |g: u16| Some(g);
    let kept = vec![2u16];
    let out = subset_varc(&varc, &bytes, &kept, &map).unwrap();
    assert!(out.is_none());
}

#[test]
fn subset_keeps_table_with_renumbered_coverage() {
    let rec = build_translate_record(7, 10, 20);
    let bytes = build_varc(&[5], &[&rec]);
    let varc = sigilbuzz::tables::Varc::parse(&bytes).unwrap();
    // Map old gid 5 -> new gid 1, old gid 7 (component) -> new gid 2.
    let map = |g: u16| match g {
        5 => Some(1),
        7 => Some(2),
        _ => None,
    };
    let kept = vec![5u16, 7];
    let out = subset_varc(&varc, &bytes, &kept, &map).unwrap().unwrap();
    // Re-parse the output and verify it still passes the parser.
    let new_varc = sigilbuzz::tables::Varc::parse(&out).unwrap();
    assert!(new_varc.covers(1));
    assert!(!new_varc.covers(5));
    assert_eq!(new_varc.glyph_record_count(), 1);
    // Component gid in the new record is 2.
    let comp = new_varc.composite(1, &[]).unwrap();
    assert_eq!(comp.components.len(), 1);
    assert_eq!(comp.components[0].gid, 2);
    // Translation preserved verbatim.
    assert!((comp.components[0].transform[4] - 10.0).abs() < 1e-3);
    assert!((comp.components[0].transform[5] - 20.0).abs() < 1e-3);
}

/// Builds a region-list block with `regions.len()` entries. Each
/// region is `&[(axis_index, start, peak, end)]`. Returns the raw
/// bytes that would sit at the source MVS's `regionListOffset`.
fn build_region_list(regions: &[&[(u16, f32, f32, f32)]]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(regions.len() as u16).to_be_bytes());
    let off_table_start = out.len();
    for _ in regions {
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    let mut starts: Vec<u32> = Vec::with_capacity(regions.len());
    for region in regions {
        starts.push(out.len() as u32);
        out.extend_from_slice(&(region.len() as u16).to_be_bytes());
        for (ai, s, p, e) in *region {
            out.extend_from_slice(&ai.to_be_bytes());
            let raw_s = (s * 16384.0).round() as i16;
            let raw_p = (p * 16384.0).round() as i16;
            let raw_e = (e * 16384.0).round() as i16;
            out.extend_from_slice(&raw_s.to_be_bytes());
            out.extend_from_slice(&raw_p.to_be_bytes());
            out.extend_from_slice(&raw_e.to_be_bytes());
        }
    }
    for (i, s) in starts.iter().enumerate() {
        let slot = off_table_start + i * 4;
        out[slot..slot + 4].copy_from_slice(&s.to_be_bytes());
    }
    out
}

#[test]
fn parse_region_list_decodes_each_region_payload() {
    let bytes = build_region_list(&[
        &[(0u16, 0.0, 1.0, 1.0)],
        &[(0u16, -1.0, -1.0, 0.0), (1u16, 0.0, 1.0, 1.0)],
    ]);
    let regions = parse_region_list(&bytes).unwrap();
    assert_eq!(regions.len(), 2);
    // First region: 1 axis -> 2 (axisCount) + 8 (one axis triple) = 10 bytes.
    assert_eq!(regions[0].len(), 10);
    // Second region: 2 axes -> 2 + 16 = 18 bytes.
    assert_eq!(regions[1].len(), 18);
}

#[test]
fn parse_region_list_handles_zero_regions() {
    let bytes = build_region_list(&[]);
    let regions = parse_region_list(&bytes).unwrap();
    assert!(regions.is_empty());
}

#[test]
fn collect_referenced_regions_unions_all_subtable_indexes() {
    let s0 = RewrittenMvsSubtable {
        region_indexes: vec![0, 2],
        delta_sets: Vec::new(),
    };
    let s1 = RewrittenMvsSubtable {
        region_indexes: vec![2, 3],
        delta_sets: Vec::new(),
    };
    let refs = collect_referenced_regions(&[s0, s1]);
    let v: Vec<u16> = refs.into_iter().collect();
    assert_eq!(v, vec![0, 2, 3]);
}

#[test]
fn build_region_list_bytes_produces_parseable_output() {
    // Synthesize 3 regions, splice through parse_region_list,
    // re-emit via build_region_list_bytes, then re-parse.
    let src = build_region_list(&[
        &[(0u16, 0.0, 1.0, 1.0)],
        &[(1u16, -1.0, -1.0, 0.0)],
        &[(0u16, 0.0, 1.0, 1.0), (1u16, 0.0, 1.0, 1.0)],
    ]);
    let regions = parse_region_list(&src).unwrap();
    let rebuilt = build_region_list_bytes(&regions);
    let reparsed = parse_region_list(&rebuilt).unwrap();
    assert_eq!(reparsed.len(), 3);
    assert_eq!(reparsed[0], regions[0]);
    assert_eq!(reparsed[1], regions[1]);
    assert_eq!(reparsed[2], regions[2]);
}

#[test]
fn build_region_list_bytes_handles_zero_regions() {
    let bytes = build_region_list_bytes(&[]);
    // Just a u16 region count of 0; no offset table, no payloads.
    assert_eq!(bytes.len(), 2);
    assert_eq!(&bytes[..2], &0u16.to_be_bytes());
}
