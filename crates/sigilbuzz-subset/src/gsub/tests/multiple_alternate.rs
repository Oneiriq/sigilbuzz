//! GSUB type 2 (multiple) and type 3 (alternate) rewriter and closure tests.

use super::*;

// ===== GSUB type 2 (Multiple Substitution) rewriter tests =====

/// Builds a type-2 subtable; `entries` is `(input_gid, [substitute_gid])`.
fn build_type2_subtable(entries: &[(u16, Vec<u16>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // cov off placeholder
    out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    let seq_offsets_start = out.len();
    for _ in 0..entries.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    for (i, (_input, seq)) in entries.iter().enumerate() {
        let body_start = out.len();
        out.extend_from_slice(&(seq.len() as u16).to_be_bytes());
        for g in seq {
            out.extend_from_slice(&g.to_be_bytes());
        }
        let slot = seq_offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
    }
    let cov_start = out.len();
    let inputs: Vec<u16> = entries.iter().map(|(g, _)| *g).collect();
    out.extend_from_slice(&build_coverage_format1(&inputs));
    out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
    out
}

#[test]
fn rewrite_type2_keeps_all_when_every_gid_survives() {
    // Input gid 100 decomposes to [40, 50, 60]. Renumber down by 1.
    let bytes = build_type2_subtable(&[(100, vec![40, 50, 60])]);
    let map = map_from_pairs(&[(0, 0), (40, 39), (50, 49), (60, 59), (100, 99)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_type2(&ctx, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::Multiple::parse(&rs.bytes).unwrap();
    assert_eq!(parsed.apply(99), Some(vec![39, 49, 59]));
    assert!(parsed.apply(99).is_some());
    assert!(parsed.apply(0).is_none());
}

#[test]
fn rewrite_type2_drops_sequence_when_substitute_drops() {
    // 100 -> [40, 50, 60] but 50 is dropped. The whole Sequence
    // dies because emitting [40, ?, 60] would point at a missing
    // gid.
    let bytes = build_type2_subtable(&[(100, vec![40, 50, 60])]);
    let map = map_from_pairs(&[(0, 0), (40, 39), (60, 59), (100, 99)]);
    let ctx = RewriterCtx::new(&map, None);
    // Single-entry subtable; that entry dies -> subtable dies.
    assert!(rewrite_type2(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_type2_drops_entry_when_input_drops() {
    // Two entries; drop input 100 entirely -> first entry vanishes,
    // second entry survives.
    let bytes = build_type2_subtable(&[(100, vec![40, 50]), (200, vec![70])]);
    let map = map_from_pairs(&[
        (0, 0),
        (40, 39),
        (50, 49),
        (70, 69),
        (200, 199),
        // 100 not in the map -> its Coverage entry dies.
    ]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_type2(&ctx, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::Multiple::parse(&rs.bytes).unwrap();
    // Surviving entry: input 199 -> [69].
    assert_eq!(parsed.apply(199), Some(vec![69]));
    // The dropped entry's input gid (100 was renumbered to nothing)
    // is gone: neither old nor any other gid produces a hit.
    assert!(parsed.apply(99).is_none());
}

#[test]
fn rewrite_type2_returns_none_when_coverage_empties() {
    let bytes = build_type2_subtable(&[(100, vec![40, 50])]);
    let map = map_from_pairs(&[(0, 0)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_type2(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_type2_via_dispatcher() {
    let bytes = build_type2_subtable(&[(100, vec![40, 50])]);
    let map = map_from_pairs(&[(0, 0), (40, 39), (50, 49), (100, 99)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_subtable(&ctx, gsub_type::MULTIPLE, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::Multiple::parse(&rs.bytes).unwrap();
    assert_eq!(parsed.apply(99), Some(vec![39, 49]));
}

#[test]
fn rewrite_type2_is_byte_deterministic() {
    let bytes = build_type2_subtable(&[
        (100, vec![40, 50]),
        (200, vec![70, 80, 90]),
        (300, vec![60]),
    ]);
    let map = map_from_pairs(&[
        (0, 0),
        (40, 1),
        (50, 2),
        (60, 3),
        (70, 4),
        (80, 5),
        (90, 6),
        (100, 7),
        (200, 8),
        (300, 9),
    ]);
    let ctx = RewriterCtx::new(&map, None);
    let a = rewrite_type2(&ctx, &bytes).unwrap();
    let b = rewrite_type2(&ctx, &bytes).unwrap();
    assert_eq!(a.bytes, b.bytes);
}

#[test]
fn pull_multiple_extends_keep_set() {
    // Closure walker: input 100 is kept -> every substitute in the
    // sequence gets pulled in.
    let bytes = build_type2_subtable(&[(100, vec![40, 50, 60])]);
    let mut keep = vec![false; 256];
    keep[100] = true;
    let changed = pull_multiple(&bytes, &mut keep);
    assert!(changed);
    assert!(keep[40]);
    assert!(keep[50]);
    assert!(keep[60]);
}

#[test]
fn pull_multiple_no_op_when_input_dropped() {
    let bytes = build_type2_subtable(&[(100, vec![40, 50])]);
    let mut keep = vec![false; 256];
    // 100 not kept -> no outputs pulled.
    let changed = pull_multiple(&bytes, &mut keep);
    assert!(!changed);
    assert!(!keep[40]);
    assert!(!keep[50]);
}

// ===== GSUB type 3 (Alternate Substitution) rewriter tests =====

/// Builds a type-3 subtable; `entries` is `(input_gid, [alternate_gid])`.
fn build_type3_subtable(entries: &[(u16, Vec<u16>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // cov off placeholder
    out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    let alt_offsets_start = out.len();
    for _ in 0..entries.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    for (i, (_input, alts)) in entries.iter().enumerate() {
        let body_start = out.len();
        out.extend_from_slice(&(alts.len() as u16).to_be_bytes());
        for g in alts {
            out.extend_from_slice(&g.to_be_bytes());
        }
        let slot = alt_offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
    }
    let cov_start = out.len();
    let inputs: Vec<u16> = entries.iter().map(|(g, _)| *g).collect();
    out.extend_from_slice(&build_coverage_format1(&inputs));
    out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
    out
}

#[test]
fn rewrite_type3_keeps_all_when_every_gid_survives() {
    // Input 10 has alternates [100, 101, 102].
    let bytes = build_type3_subtable(&[(10, vec![100, 101, 102])]);
    let map = map_from_pairs(&[(0, 0), (10, 9), (100, 99), (101, 100), (102, 101)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_type3(&ctx, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::Alternate::parse(&rs.bytes).unwrap();
    assert_eq!(parsed.apply(9, 0), Some(99));
    assert_eq!(parsed.apply(9, 1), Some(100));
    assert_eq!(parsed.apply(9, 2), Some(101));
    assert!(parsed.apply(9, 3).is_none());
}

#[test]
fn rewrite_type3_filters_partial_alternate_set() {
    // Input 10 has alternates [100, 101, 102]; 101 is dropped. The
    // surviving set is [100, 102] (renumbered).
    let bytes = build_type3_subtable(&[(10, vec![100, 101, 102])]);
    let map = map_from_pairs(&[(0, 0), (10, 9), (100, 99), (102, 101)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_type3(&ctx, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::Alternate::parse(&rs.bytes).unwrap();
    assert_eq!(parsed.apply(9, 0), Some(99));
    assert_eq!(parsed.apply(9, 1), Some(101));
    assert!(parsed.apply(9, 2).is_none());
}

#[test]
fn rewrite_type3_drops_entry_when_all_alternates_drop() {
    // Two Coverage entries; the first's alternates all drop, the
    // second survives untouched.
    let bytes = build_type3_subtable(&[(10, vec![100, 101]), (20, vec![200])]);
    let map = map_from_pairs(&[(0, 0), (10, 9), (20, 19), (200, 199)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_type3(&ctx, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::Alternate::parse(&rs.bytes).unwrap();
    // First entry's input (gid 9) is no longer covered.
    assert!(parsed.apply(9, 0).is_none());
    // Second entry survives.
    assert_eq!(parsed.apply(19, 0), Some(199));
}

#[test]
fn rewrite_type3_returns_none_when_coverage_empties() {
    let bytes = build_type3_subtable(&[(10, vec![100, 101])]);
    let map = map_from_pairs(&[(0, 0)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_type3(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_type3_via_dispatcher() {
    let bytes = build_type3_subtable(&[(10, vec![100, 101])]);
    let map = map_from_pairs(&[(0, 0), (10, 9), (100, 99), (101, 100)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_subtable(&ctx, gsub_type::ALTERNATE, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::Alternate::parse(&rs.bytes).unwrap();
    assert_eq!(parsed.apply(9, 0), Some(99));
    assert_eq!(parsed.apply(9, 1), Some(100));
}

#[test]
fn rewrite_type3_is_byte_deterministic() {
    let bytes = build_type3_subtable(&[(10, vec![100, 101]), (20, vec![200, 201, 202])]);
    let map = map_from_pairs(&[
        (0, 0),
        (10, 1),
        (20, 2),
        (100, 3),
        (101, 4),
        (200, 5),
        (201, 6),
        (202, 7),
    ]);
    let ctx = RewriterCtx::new(&map, None);
    let a = rewrite_type3(&ctx, &bytes).unwrap();
    let b = rewrite_type3(&ctx, &bytes).unwrap();
    assert_eq!(a.bytes, b.bytes);
}

#[test]
fn pull_alternate_default_extends_keep_set_with_first_alternate_only() {
    // Closure walker: input 10 is kept -> only the *first* alternate
    // (100) gets pulled in. The remaining alternates (101, 102) stay
    // dropped unless the caller requested them explicitly.
    let bytes = build_type3_subtable(&[(10, vec![100, 101, 102])]);
    let mut keep = vec![false; 256];
    keep[10] = true;
    let changed = pull_alternate_default(&bytes, &mut keep);
    assert!(changed);
    assert!(
        keep[100],
        "default alternate (index 0 = gid 100) must be pulled in"
    );
    assert!(
        !keep[101],
        "non-default alternate gid 101 must NOT be auto-pulled"
    );
    assert!(
        !keep[102],
        "non-default alternate gid 102 must NOT be auto-pulled"
    );
}

#[test]
fn pull_alternate_default_no_op_when_input_dropped() {
    let bytes = build_type3_subtable(&[(10, vec![100])]);
    let mut keep = vec![false; 256];
    // 10 not kept -> no outputs pulled.
    let changed = pull_alternate_default(&bytes, &mut keep);
    assert!(!changed);
    assert!(!keep[100]);
}
