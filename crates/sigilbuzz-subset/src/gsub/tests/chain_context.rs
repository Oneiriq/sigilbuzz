//! GSUB type 6 (chained context substitution) rewriter tests.

use super::*;

// ===== GSUB type 6 (Chained Context Substitution) rewriter tests =====

/// Builds a format-3 type-6 subtable around explicit per-position
/// glyph arrays + SubstLookupRecord list.
fn build_type6_format3(
    backtrack: &[Vec<u16>],
    input: &[Vec<u16>],
    lookahead: &[Vec<u16>],
    records: &[(u16, u16)],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&3u16.to_be_bytes()); // substFormat
    out.extend_from_slice(&(backtrack.len() as u16).to_be_bytes());
    let bt_slots = out.len();
    for _ in 0..backtrack.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    out.extend_from_slice(&(input.len() as u16).to_be_bytes());
    let in_slots = out.len();
    for _ in 0..input.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    out.extend_from_slice(&(lookahead.len() as u16).to_be_bytes());
    let la_slots = out.len();
    for _ in 0..lookahead.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    out.extend_from_slice(&(records.len() as u16).to_be_bytes());
    for (s, l) in records {
        out.extend_from_slice(&s.to_be_bytes());
        out.extend_from_slice(&l.to_be_bytes());
    }
    let mut patch = |slots: usize, covs: &[Vec<u16>]| {
        for (i, gs) in covs.iter().enumerate() {
            let body_start = out.len() as u16;
            out.extend_from_slice(&build_coverage_format1(gs));
            let slot = slots + i * 2;
            out[slot..slot + 2].copy_from_slice(&body_start.to_be_bytes());
        }
    };
    patch(bt_slots, backtrack);
    patch(in_slots, input);
    patch(la_slots, lookahead);
    out
}

#[test]
fn rewrite_type6_format3_keeps_all_when_every_gid_survives() {
    // 1 backtrack position covering {5}, 2 input positions covering
    // {10, 11} / {12}, 1 lookahead position covering {30}.
    let bytes = build_type6_format3(
        &[vec![5]],
        &[vec![10, 11], vec![12]],
        &[vec![30]],
        &[(0, 1)],
    );
    let map = map_from_pairs(&[(0, 0), (5, 50), (10, 100), (11, 101), (12, 102), (30, 300)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_type6(&ctx, &bytes).unwrap();
    // Format-3 chained context parses through ChainContext.
    let parsed = sigilbuzz::tables::gsub::ChainContext::parse(&rs.bytes).unwrap();
    let (bt, inp, la) = parsed.context_len();
    assert_eq!((bt, inp, la), (1, 2, 1));
}

#[test]
fn rewrite_type6_format3_drops_when_backtrack_position_empties() {
    // Backtrack [5]: drop 5 -> backtrack coverage empties -> subtable dies.
    let bytes = build_type6_format3(&[vec![5]], &[vec![10]], &[vec![30]], &[(0, 1)]);
    let map = map_from_pairs(&[(0, 0), (10, 100), (30, 300)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_type6(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_type6_format3_drops_when_lookahead_position_empties() {
    let bytes = build_type6_format3(&[vec![5]], &[vec![10]], &[vec![30]], &[(0, 1)]);
    let map = map_from_pairs(&[(0, 0), (5, 50), (10, 100)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_type6(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_type6_format3_drops_record_when_target_lookup_drops() {
    // Two records (0, 1) and (1, 5); renumber drops 5.
    let bytes = build_type6_format3(&[], &[vec![10]], &[], &[(0, 1), (1, 5)]);
    let map = map_from_pairs(&[(0, 0), (10, 100)]);
    let renumber = vec![
        Some(0u16),
        Some(1u16),
        Some(2u16),
        Some(3u16),
        Some(4u16),
        None,
    ];
    let ctx = RewriterCtx::new(&map, Some(&renumber));
    let rs = rewrite_type6(&ctx, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::ChainContext::parse(&rs.bytes).unwrap();
    // One record survives.
    assert_eq!(parsed.substitutions().len(), 1);
    assert_eq!(parsed.substitutions()[0].lookup_list_index, 1);
}

#[test]
fn rewrite_type6_format3_keeps_subtable_when_all_records_drop() {
    // The subtable becomes an `ignore sub` rule, which still stops
    // the later subtables of its lookup from matching.
    let bytes = build_type6_format3(&[vec![5]], &[vec![10]], &[], &[(0, 1), (1, 5)]);
    let map = map_from_pairs(&[(0, 0), (5, 50), (10, 100)]);
    let renumber = vec![None, None, None, None, None, None];
    let ctx = RewriterCtx::new(&map, Some(&renumber));
    let rs = rewrite_type6(&ctx, &bytes).expect("the subtable survives");
    let parsed = sigilbuzz::tables::gsub::ChainContext::parse(&rs.bytes).unwrap();
    assert_eq!(parsed.context_len(), (1, 1, 0));
    assert!(parsed.substitutions().is_empty());
}

#[test]
fn rewrite_type6_format3_keeps_source_ignore_rule() {
    // Compiled from `ignore sub a b' c;`: no records at all, even
    // before any lookup drops.
    let bytes = build_type6_format3(&[vec![5]], &[vec![10]], &[vec![30]], &[]);
    let map = map_from_pairs(&[(0, 0), (5, 50), (10, 100), (30, 300)]);
    for renumber in [None, Some(alloc::vec![Some(0u16)])] {
        let ctx = RewriterCtx::new(&map, renumber.as_deref());
        let rs = rewrite_type6(&ctx, &bytes).expect("the ignore rule survives");
        let parsed = sigilbuzz::tables::gsub::ChainContext::parse(&rs.bytes).unwrap();
        assert_eq!(parsed.context_len(), (1, 1, 1));
        assert!(parsed.substitutions().is_empty());
    }
}

#[test]
fn rewrite_type6_format3_is_byte_deterministic() {
    let bytes = build_type6_format3(&[vec![5]], &[vec![10, 11]], &[vec![30]], &[(0, 1)]);
    let map = map_from_pairs(&[(0, 0), (5, 50), (10, 100), (11, 101), (30, 300)]);
    let ctx = RewriterCtx::new(&map, None);
    let a = rewrite_type6(&ctx, &bytes).unwrap();
    let b = rewrite_type6(&ctx, &bytes).unwrap();
    assert_eq!(a.bytes, b.bytes);
}

#[test]
fn rewrite_type6_via_dispatcher() {
    let bytes = build_type6_format3(&[], &[vec![10]], &[], &[(0, 1)]);
    let map = map_from_pairs(&[(0, 0), (10, 100)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_subtable(&ctx, gsub_type::CHAINED_CONTEXT, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::ChainContext::parse(&rs.bytes);
    assert!(parsed.is_ok());
}
