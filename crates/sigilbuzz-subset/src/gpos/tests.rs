//! Unit tests for the GPOS rewriters, one module per lookup family;
//! type 1, the Extension wrapper and the lookup driver live here.

use super::pair_pos::{rewrite_pair_pos_format1, rewrite_pair_pos_format2};
use super::*;
use crate::layout::{parse_coverage_glyphs, GidMap};
use alloc::vec;
use sigilbuzz::tables::gpos::value_record::{X_ADVANCE, X_PLACEMENT};
use sigilbuzz::tables::gpos::{MarkBasePos, MarkLigaPos, MarkMarkPos, PairPos, SinglePos};

mod context;
mod cursive;
mod mark_attach;
mod pair_pos;

/// The single subtable a rewrite produced; the fixtures here are
/// far too small to be split.
fn one(mut pieces: Vec<RewrittenSubtable>) -> RewrittenSubtable {
    assert_eq!(pieces.len(), 1, "expected one subtable");
    pieces.remove(0)
}

fn build_coverage_format1(glyphs: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
    for g in glyphs {
        out.extend_from_slice(&g.to_be_bytes());
    }
    out
}

fn map_from_pairs(pairs: &[(u16, u16)]) -> GidMap {
    let max_old = pairs.iter().map(|(o, _)| *o).max().unwrap_or(0);
    let mut table = vec![None; (max_old as usize + 1).max(1)];
    for &(old, new) in pairs {
        table[old as usize] = Some(new);
    }
    GidMap::from_table(table)
}

fn build_anchor(x: i16, y: i16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format 1
    out.extend_from_slice(&x.to_be_bytes());
    out.extend_from_slice(&y.to_be_bytes());
    out
}

// ----- Type 1: Single Adjustment -----

fn build_single_adj_format1(covered: &[u16], value_format: u16, fields: &[i16]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
    out.extend_from_slice(&value_format.to_be_bytes());
    for f in fields {
        out.extend_from_slice(&f.to_be_bytes());
    }
    let cov_start = out.len();
    out.extend_from_slice(&build_coverage_format1(covered));
    out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
    out
}

fn build_single_adj_format2(covered: &[u16], value_format: u16, records: &[&[i16]]) -> Vec<u8> {
    assert_eq!(covered.len(), records.len());
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes()); // posFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset placeholder
    out.extend_from_slice(&value_format.to_be_bytes());
    out.extend_from_slice(&(records.len() as u16).to_be_bytes()); // valueCount
    for rec in records {
        for f in *rec {
            out.extend_from_slice(&f.to_be_bytes());
        }
    }
    let cov_start = out.len();
    out.extend_from_slice(&build_coverage_format1(covered));
    out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
    out
}

#[test]
fn rewrite_single_adj_format1_remaps_coverage() {
    // Covered: 10, 20, 30. Map 10->1, 20->2, drop 30. Shared
    // x_advance = -25 should still apply.
    let bytes = build_single_adj_format1(&[10, 20, 30], X_ADVANCE, &[-25]);
    let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_single_adj(&ctx, &bytes).unwrap();
    let parsed = SinglePos::parse(&rs.bytes).unwrap();
    assert_eq!(parsed.adjustment(1).unwrap().x_advance, -25);
    assert_eq!(parsed.adjustment(2).unwrap().x_advance, -25);
    assert!(parsed.adjustment(3).is_none()); // 30 dropped
}

#[test]
fn rewrite_single_adj_format2_drops_corresponding_value() {
    // Three glyphs with per-glyph deltas. Drop the middle one.
    let bytes = build_single_adj_format2(&[10, 20, 30], X_ADVANCE, &[&[-5], &[-10], &[-15]]);
    let map = map_from_pairs(&[(0, 0), (10, 1), (30, 3)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_single_adj(&ctx, &bytes).unwrap();
    let parsed = SinglePos::parse(&rs.bytes).unwrap();
    assert_eq!(parsed.adjustment(1).unwrap().x_advance, -5);
    assert_eq!(parsed.adjustment(3).unwrap().x_advance, -15);
    // gid 2 not covered (was old 20, dropped).
    assert!(parsed.adjustment(2).is_none());
}

#[test]
fn rewrite_single_adj_returns_none_when_all_dropped() {
    let bytes = build_single_adj_format1(&[10, 20], X_ADVANCE, &[-5]);
    let map = map_from_pairs(&[(0, 0)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_single_adj(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_single_adj_format1_preserves_value_record_with_multiple_fields() {
    // value_format = X_PLACEMENT | X_ADVANCE -> two i16 fields.
    let bytes = build_single_adj_format1(&[5], X_PLACEMENT | X_ADVANCE, &[4, -10]);
    let map = map_from_pairs(&[(0, 0), (5, 1)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_single_adj(&ctx, &bytes).unwrap();
    let parsed = SinglePos::parse(&rs.bytes).unwrap();
    let v = parsed.adjustment(1).unwrap();
    assert_eq!(v.x_placement, 4);
    assert_eq!(v.x_advance, -10);
}

// ----- Type 9: Extension wrapper around inner type 1 -----

#[test]
fn rewrite_extension_wraps_inner_single_adj() {
    let inner = build_single_adj_format1(&[10], X_ADVANCE, &[-25]);
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&gpos_type::SINGLE_ADJUSTMENT.to_be_bytes());
    out.extend_from_slice(&8u32.to_be_bytes()); // inner offset = 8
    out.extend_from_slice(&inner);
    let map = map_from_pairs(&[(0, 0), (10, 1)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = one(rewrite_extension(&ctx, &out));
    // Verify the wrapper is preserved and inner parses.
    assert_eq!(&rs.bytes[0..2], &1u16.to_be_bytes());
    let inner_type = u16::from_be_bytes([rs.bytes[2], rs.bytes[3]]);
    assert_eq!(inner_type, gpos_type::SINGLE_ADJUSTMENT);
    let inner_off =
        u32::from_be_bytes([rs.bytes[4], rs.bytes[5], rs.bytes[6], rs.bytes[7]]) as usize;
    let parsed = SinglePos::parse(&rs.bytes[inner_off..]).unwrap();
    assert_eq!(parsed.adjustment(1).unwrap().x_advance, -25);
}

#[test]
fn rewrite_lookup_drops_malformed_context() {
    let map = map_from_pairs(&[(0, 0), (10, 1)]);
    let ctx = RewriterCtx::new(&map, None);
    // Format 0 inside a context subtable is unknown. It falls
    // out as None and the cascade drops the lookup.
    let dummy: Vec<&[u8]> = vec![&[0u8; 6]];
    assert!(rewrite_lookup(&ctx, gpos_type::CONTEXT, 0, None, &dummy)
        .unwrap()
        .is_none());
}
