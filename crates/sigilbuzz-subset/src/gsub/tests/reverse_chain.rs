//! GSUB type 8 (reverse chaining single substitution) rewriter and closure tests.

use super::*;

// ===== GSUB type 8 (Reverse Chained Single Substitution) rewriter tests =====

/// Builds a format-1 type-8 subtable. `subs[i]` is the substitute
/// for `input[i]`.
fn build_type8(
    input: &[u16],
    backtrack: &[Vec<u16>],
    lookahead: &[Vec<u16>],
    subs: &[u16],
) -> Vec<u8> {
    assert_eq!(input.len(), subs.len());
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // substFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverage placeholder
    out.extend_from_slice(&(backtrack.len() as u16).to_be_bytes());
    let bt_slots = out.len();
    for _ in 0..backtrack.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    out.extend_from_slice(&(lookahead.len() as u16).to_be_bytes());
    let la_slots = out.len();
    for _ in 0..lookahead.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    out.extend_from_slice(&(subs.len() as u16).to_be_bytes());
    for s in subs {
        out.extend_from_slice(&s.to_be_bytes());
    }
    let cov_off = out.len();
    out.extend_from_slice(&build_coverage_format1(input));
    out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());
    let mut patch = |slots: usize, covs: &[Vec<u16>]| {
        for (i, gs) in covs.iter().enumerate() {
            let body_start = out.len() as u16;
            out.extend_from_slice(&build_coverage_format1(gs));
            let slot = slots + i * 2;
            out[slot..slot + 2].copy_from_slice(&body_start.to_be_bytes());
        }
    };
    patch(bt_slots, backtrack);
    patch(la_slots, lookahead);
    out
}

#[test]
fn rewrite_type8_keeps_all_when_every_gid_survives() {
    // Coverage {10}, backtrack [{5}], lookahead [{30}], substitute {100}.
    let bytes = build_type8(&[10], &[vec![5]], &[vec![30]], &[100]);
    let map = map_from_pairs(&[(0, 0), (5, 50), (10, 1), (30, 3), (100, 99)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_type8(&ctx, &bytes).unwrap();
    let rc = sigilbuzz::tables::gsub::ReverseChain::parse(&rs.bytes).unwrap();
    // Apply with surrounding context: [50, 1, 3] -> 99.
    assert_eq!(rc.apply(&[50, 1, 3], 1), Some(99));
}

#[test]
fn rewrite_type8_drops_pair_when_substitute_drops() {
    // Coverage {10, 20}, substitutes {100, 200}; drop 200.
    let bytes = build_type8(&[10, 20], &[], &[], &[100, 200]);
    let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2), (100, 99)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_type8(&ctx, &bytes).unwrap();
    let rc = sigilbuzz::tables::gsub::ReverseChain::parse(&rs.bytes).unwrap();
    // 1 still substitutes to 99; 2 (was 20) is no longer covered.
    assert_eq!(rc.apply(&[1], 0), Some(99));
    assert_eq!(rc.apply(&[2], 0), None);
}

#[test]
fn rewrite_type8_drops_pair_when_input_drops() {
    let bytes = build_type8(&[10, 20], &[], &[], &[100, 200]);
    let map = map_from_pairs(&[(0, 0), (20, 2), (200, 199)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_type8(&ctx, &bytes).unwrap();
    let rc = sigilbuzz::tables::gsub::ReverseChain::parse(&rs.bytes).unwrap();
    assert_eq!(rc.apply(&[2], 0), Some(199));
}

#[test]
fn rewrite_type8_drops_subtable_when_input_coverage_empties() {
    let bytes = build_type8(&[10], &[], &[], &[100]);
    let map = map_from_pairs(&[(0, 0)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_type8(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_type8_drops_subtable_when_backtrack_coverage_empties() {
    // Backtrack [{5}]: drop 5 -> backtrack coverage empties -> subtable dies.
    let bytes = build_type8(&[10], &[vec![5]], &[], &[100]);
    let map = map_from_pairs(&[(0, 0), (10, 1), (100, 99)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_type8(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_type8_drops_subtable_when_lookahead_coverage_empties() {
    let bytes = build_type8(&[10], &[], &[vec![30]], &[100]);
    let map = map_from_pairs(&[(0, 0), (10, 1), (100, 99)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_type8(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_type8_is_byte_deterministic() {
    let bytes = build_type8(&[10, 11], &[vec![5]], &[vec![30]], &[100, 101]);
    let map = map_from_pairs(&[
        (0, 0),
        (5, 50),
        (10, 1),
        (11, 2),
        (30, 3),
        (100, 99),
        (101, 98),
    ]);
    let ctx = RewriterCtx::new(&map, None);
    let a = rewrite_type8(&ctx, &bytes).unwrap();
    let b = rewrite_type8(&ctx, &bytes).unwrap();
    assert_eq!(a.bytes, b.bytes);
}

#[test]
fn rewrite_type8_via_dispatcher() {
    let bytes = build_type8(&[10], &[], &[], &[100]);
    let map = map_from_pairs(&[(0, 0), (10, 1), (100, 99)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_subtable(&ctx, gsub_type::REVERSE_CHAINED, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::ReverseChain::parse(&rs.bytes);
    assert!(parsed.is_ok());
}

/// A keep bitset over glyphs `0..256` with `kept` set.
fn keep_set(kept: &[u16]) -> Vec<bool> {
    let mut keep = vec![false; 256];
    for &g in kept {
        keep[usize::from(g)] = true;
    }
    keep
}

#[test]
fn closure_pulls_reverse_chain_substitutes_of_kept_inputs() {
    // 10 -> 100 and 11 -> 101 after a 5 and before a 30 or 31.
    let bytes = build_type8(&[10, 11], &[vec![5]], &[vec![30, 31]], &[100, 101]);
    let mut keep = keep_set(&[5, 10, 31]);
    assert!(pull_reverse_chain(&bytes, &mut keep));
    assert!(keep[100], "the kept input's substitute joins the closure");
    assert!(!keep[101], "an input that is not kept brings nothing in");
    assert!(
        !pull_reverse_chain(&bytes, &mut keep),
        "a second pass adds nothing"
    );
}

#[test]
fn closure_skips_reverse_chain_rules_whose_context_cannot_match() {
    let bytes = build_type8(&[10], &[vec![5]], &[vec![30]], &[100]);
    for kept in [&[10u16, 30][..], &[5, 10], &[10]] {
        let mut keep = keep_set(kept);
        assert!(!pull_reverse_chain(&bytes, &mut keep), "kept {kept:?}");
        assert!(!keep[100], "kept {kept:?}");
    }
}

#[test]
fn closure_then_rewrite_keeps_the_reverse_chain_substitution() {
    // Keeping the input and its context glyphs is enough: the
    // closure adds the substitute and the rewrite keeps the pair.
    let bytes = build_type8(&[10], &[vec![5]], &[vec![30]], &[100]);
    let mut keep = keep_set(&[0, 5, 10, 30]);
    pull_reverse_chain(&bytes, &mut keep);
    let kept: Vec<u16> = (0..256u16).filter(|&g| keep[usize::from(g)]).collect();
    let map = GidMap::from_kept(&kept);
    let rs = rewrite_type8(&RewriterCtx::new(&map, None), &bytes).expect("subtable survives");
    let rc = sigilbuzz::tables::gsub::ReverseChain::parse(&rs.bytes).unwrap();
    let new = |g: u16| map.map(g).unwrap();
    assert_eq!(
        rc.apply(&[new(5), new(10), new(30)], 1),
        Some(new(100)),
        "the subset still substitutes in context"
    );
}

#[test]
fn closure_ignores_truncated_reverse_chain_subtables() {
    let bytes = build_type8(&[10], &[vec![5]], &[vec![30]], &[100]);
    for len in 0..bytes.len() {
        let mut keep = keep_set(&[5, 10, 30]);
        let before = keep.clone();
        pull_reverse_chain(&bytes[..len], &mut keep);
        assert_eq!(keep, before, "cut at {len}");
    }
}
