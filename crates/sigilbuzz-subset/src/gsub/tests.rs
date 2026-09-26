//! Unit tests for the GSUB rewriters and closure pulls, one module per
//! lookup family; type 1 and the Extension wrapper live here.

use super::closure::{pull_alternate_default, pull_multiple, pull_reverse_chain, pull_single};
use super::*;
use crate::layout::GidMap;
use alloc::{vec, vec::Vec};
use sigilbuzz::tables::layout::Coverage as CoverageParser;

mod chain_context;
mod context;
mod ligature;
mod multiple_alternate;
mod reverse_chain;

fn build_coverage_format1(glyphs: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
    for g in glyphs {
        out.extend_from_slice(&g.to_be_bytes());
    }
    out
}

fn build_single_format1(covered: &[u16], delta: i16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&0u16.to_be_bytes()); // cov off placeholder
    out.extend_from_slice(&delta.to_be_bytes());
    let cov_start = out.len();
    out.extend_from_slice(&build_coverage_format1(covered));
    out[2..4].copy_from_slice(&(cov_start as u16).to_be_bytes());
    out
}

fn build_single_format2(covered: &[u16], substitutes: &[u16]) -> Vec<u8> {
    assert_eq!(covered.len(), substitutes.len());
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes()); // format
    out.extend_from_slice(&0u16.to_be_bytes()); // cov off placeholder
    out.extend_from_slice(&(substitutes.len() as u16).to_be_bytes());
    for s in substitutes {
        out.extend_from_slice(&s.to_be_bytes());
    }
    let cov_start = out.len();
    out.extend_from_slice(&build_coverage_format1(covered));
    out[2..4].copy_from_slice(&(cov_start as u16).to_be_bytes());
    out
}

fn map_from_pairs(pairs: &[(u16, u16)]) -> GidMap {
    // Build a GidMap by old gid -> new gid; gids not in `pairs` map
    // to None.
    let max_old = pairs.iter().map(|(o, _)| *o).max().unwrap_or(0);
    let mut table = vec![None; (max_old as usize + 1).max(1)];
    for &(old, new) in pairs {
        table[old as usize] = Some(new);
    }
    GidMap::from_table(table)
}

#[test]
fn rewrite_single_format1_remaps_delta() {
    // A -> small-cap-A, B -> small-cap-B, C -> small-cap-C.
    // Old: covered {65,66,67}, delta +200 -> outputs 265,266,267.
    let bytes = build_single_format1(&[65, 66, 67], 200);
    // New gid map: 65->1, 66->2, 67->3, 265->4, 266->5, 267->6 (.notdef stays at 0).
    let map = map_from_pairs(&[
        (0, 0),
        (65, 1),
        (66, 2),
        (67, 3),
        (265, 4),
        (266, 5),
        (267, 6),
    ]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_single(&ctx, &bytes).unwrap();
    // Verify the rewritten subtable parses and applies correctly.
    let parsed = sigilbuzz::tables::gsub::Single::parse(&rs.bytes).unwrap();
    assert_eq!(parsed.apply(1), Some(4));
    assert_eq!(parsed.apply(2), Some(5));
    assert_eq!(parsed.apply(3), Some(6));
    assert_eq!(parsed.apply(0), None);
}

#[test]
fn rewrite_single_format1_falls_back_to_format2_when_delta_breaks() {
    // After remap, the deltas no longer line up. Old: covered
    // {10, 20, 30}, delta +5 -> outputs 15, 25, 35. New gid map
    // jumbles them: 10->1, 20->2, 30->3, 15->7, 25->9, 35->11. Now
    // input->output deltas are 6, 7, 8 (no constant).
    let bytes = build_single_format1(&[10, 20, 30], 5);
    let map = map_from_pairs(&[
        (0, 0),
        (10, 1),
        (20, 2),
        (30, 3),
        (15, 7),
        (25, 9),
        (35, 11),
    ]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_single(&ctx, &bytes).unwrap();
    // Format must be 2 because no constant delta works.
    assert_eq!(&rs.bytes[0..2], &2u16.to_be_bytes());
    let parsed = sigilbuzz::tables::gsub::Single::parse(&rs.bytes).unwrap();
    assert_eq!(parsed.apply(1), Some(7));
    assert_eq!(parsed.apply(2), Some(9));
    assert_eq!(parsed.apply(3), Some(11));
}

#[test]
fn rewrite_single_format2_drops_pairs_with_dropped_input() {
    // Format 2: 10->100, 20->200, 30->300. Drop input 20 from the
    // kept set. New gid map: 10->1, 30->3, 100->11, 300->33.
    let bytes = build_single_format2(&[10, 20, 30], &[100, 200, 300]);
    let map = map_from_pairs(&[(0, 0), (10, 1), (30, 3), (100, 11), (300, 33)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_single(&ctx, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::Single::parse(&rs.bytes).unwrap();
    assert_eq!(parsed.apply(1), Some(11));
    // Input 2 (the new gid for 20) is not in the map at all because
    // 20 was dropped, so it can't be in the rewritten coverage.
    assert!(parsed.apply(2).is_none() || parsed.apply(2) == Some(0));
    assert_eq!(parsed.apply(3), Some(33));
}

#[test]
fn rewrite_single_drops_pairs_with_dropped_output() {
    // 10->100 stays, 20->200 dies because 200 is dropped.
    let bytes = build_single_format2(&[10, 20], &[100, 200]);
    let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2), (100, 11)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_single(&ctx, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::Single::parse(&rs.bytes).unwrap();
    assert_eq!(parsed.apply(1), Some(11));
    // gid 2 (was 20) is not covered because 200 dropped.
    let cov_off = u16::from_be_bytes([rs.bytes[2], rs.bytes[3]]) as usize;
    let cov = CoverageParser::parse(&rs.bytes[cov_off..]).unwrap();
    assert!(cov.index_of(2).is_none());
}

#[test]
fn rewrite_single_returns_none_when_all_pairs_drop() {
    let bytes = build_single_format2(&[10, 20], &[100, 200]);
    let map = map_from_pairs(&[(0, 0)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_single(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_extension_recurses_into_inner_single_sub() {
    // Build an Extension wrapper around a single-sub format 1.
    let inner = build_single_format1(&[10, 11], 5);
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format=1
    out.extend_from_slice(&gsub_type::SINGLE.to_be_bytes());
    out.extend_from_slice(&8u32.to_be_bytes()); // inner offset = 8
    out.extend_from_slice(&inner);
    let map = map_from_pairs(&[(0, 0), (10, 1), (11, 2), (15, 7), (16, 8)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_extension(&ctx, &out).unwrap();
    // Output must still be an Extension wrapper around a single-sub.
    assert_eq!(&rs.bytes[0..2], &1u16.to_be_bytes());
    let inner_type = u16::from_be_bytes([rs.bytes[2], rs.bytes[3]]);
    assert_eq!(inner_type, gsub_type::SINGLE);
    let inner_off =
        u32::from_be_bytes([rs.bytes[4], rs.bytes[5], rs.bytes[6], rs.bytes[7]]) as usize;
    let parsed = sigilbuzz::tables::gsub::Single::parse(&rs.bytes[inner_off..]).unwrap();
    assert_eq!(parsed.apply(1), Some(7));
    assert_eq!(parsed.apply(2), Some(8));
}

#[test]
fn pull_single_extends_keep_set() {
    // covered {10}, delta +5 -> output 15. Mark 10 kept; pull should
    // mark 15 kept.
    let bytes = build_single_format1(&[10], 5);
    let mut keep = vec![false; 32];
    keep[10] = true;
    let changed = pull_single(&bytes, &mut keep);
    assert!(changed);
    assert!(keep[15]);
}

#[test]
fn pull_single_no_op_when_input_dropped() {
    let bytes = build_single_format1(&[10], 5);
    let mut keep = vec![false; 32];
    // 10 not kept -> 15 not pulled.
    let changed = pull_single(&bytes, &mut keep);
    assert!(!changed);
    assert!(!keep[15]);
}
