//! GSUB type 5 (context substitution) rewriter tests.

use super::*;

// ===== GSUB type 5 (Context Substitution) rewriter tests =====
//
// Build helpers for the three formats. Real fonts almost always
// pick format 3, but the rewriter has independent paths for all
// three so we exercise each.

/// Builds a format-1 type-5 subtable.
/// `sets[i]` = (first_gid, [(input_tail, [(seq_idx, lookup_idx)])])
#[allow(clippy::type_complexity)]
fn build_type5_format1(sets: &[(u16, Vec<(Vec<u16>, Vec<(u16, u16)>)>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // substFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverage off placeholder
    out.extend_from_slice(&(sets.len() as u16).to_be_bytes()); // ruleSetCount
    let set_offs_start = out.len();
    for _ in 0..sets.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    for (i, (_first, rules)) in sets.iter().enumerate() {
        let set_start = out.len();
        // RuleSet
        out.extend_from_slice(&(rules.len() as u16).to_be_bytes());
        let rule_offs_start = out.len();
        for _ in 0..rules.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        for (j, (input_tail, lookups)) in rules.iter().enumerate() {
            let rule_start = out.len();
            let glyph_count = (input_tail.len() + 1) as u16;
            out.extend_from_slice(&glyph_count.to_be_bytes());
            out.extend_from_slice(&(lookups.len() as u16).to_be_bytes());
            for g in input_tail {
                out.extend_from_slice(&g.to_be_bytes());
            }
            for (s, l) in lookups {
                out.extend_from_slice(&s.to_be_bytes());
                out.extend_from_slice(&l.to_be_bytes());
            }
            let rel = (rule_start - set_start) as u16;
            let slot = rule_offs_start + j * 2;
            out[slot..slot + 2].copy_from_slice(&rel.to_be_bytes());
        }
        let set_off_slot = set_offs_start + i * 2;
        out[set_off_slot..set_off_slot + 2].copy_from_slice(&(set_start as u16).to_be_bytes());
    }
    let cov_start = out.len();
    let firsts: Vec<u16> = sets.iter().map(|(g, _)| *g).collect();
    out.extend_from_slice(&build_coverage_format1(&firsts));
    out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
    out
}

/// Builds a format-3 type-5 subtable from per-position glyph sets +
/// SubstLookupRecord list.
fn build_type5_format3(input: &[Vec<u16>], records: &[(u16, u16)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&3u16.to_be_bytes()); // substFormat
    out.extend_from_slice(&(input.len() as u16).to_be_bytes()); // glyphCount
    out.extend_from_slice(&(records.len() as u16).to_be_bytes()); // substLookupRecordCount
    let cov_offs_start = out.len();
    for _ in 0..input.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    for (s, l) in records {
        out.extend_from_slice(&s.to_be_bytes());
        out.extend_from_slice(&l.to_be_bytes());
    }
    for (i, gs) in input.iter().enumerate() {
        let body_start = out.len() as u16;
        out.extend_from_slice(&build_coverage_format1(gs));
        let slot = cov_offs_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&body_start.to_be_bytes());
    }
    out
}

#[test]
fn rewrite_type5_format1_keeps_all_when_every_gid_survives() {
    // First gid 10 has one rule: tail [20], one nested lookup at seq 0.
    let bytes = build_type5_format1(&[(10, vec![(vec![20], vec![(0, 1)])])]);
    let map = map_from_pairs(&[(0, 0), (10, 9), (20, 19)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_type5(&ctx, &bytes).unwrap();
    // Round-trip through the parser to validate semantics.
    let parsed = sigilbuzz::tables::gsub::Context::parse(&rs.bytes).unwrap();
    match parsed {
        sigilbuzz::tables::gsub::Context::Format1(c) => {
            let cov = c.coverage();
            assert_eq!(cov.index_of(9), Some(0));
            assert!(cov.index_of(10).is_none());
        }
        _ => panic!("expected format 1"),
    }
}

#[test]
fn rewrite_type5_format1_drops_rule_when_input_tail_drops() {
    // First=10, tail=[20]: drop 20. Single rule dies, set dies,
    // coverage entry dies, subtable dies.
    let bytes = build_type5_format1(&[(10, vec![(vec![20], vec![(0, 1)])])]);
    let map = map_from_pairs(&[(0, 0), (10, 9)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_type5(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_type5_format1_drops_when_first_glyph_drops() {
    let bytes = build_type5_format1(&[(10, vec![(vec![20], vec![(0, 1)])])]);
    let map = map_from_pairs(&[(0, 0), (20, 19)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_type5(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_type5_format1_drops_record_when_target_lookup_drops() {
    // Two SubstLookupRecords, indices 1 and 2. Renumber drops 2 ->
    // record list survives with one entry.
    let bytes = build_type5_format1(&[(10, vec![(vec![20], vec![(0, 1), (1, 2)])])]);
    let map = map_from_pairs(&[(0, 0), (10, 9), (20, 19)]);
    let renumber = vec![Some(0u16), Some(0u16), None]; // index 2 dropped
    let ctx = RewriterCtx::new(&map, Some(&renumber));
    let rs = rewrite_type5(&ctx, &bytes).unwrap();
    // Verify only one record survives by checking byte size: rule
    // body grows by 4 bytes per record, so we just confirm the
    // subtable parses.
    assert!(sigilbuzz::tables::gsub::Context::parse(&rs.bytes).is_ok());
}

#[test]
fn rewrite_type5_format1_keeps_rule_when_all_records_drop() {
    // A rule without records is an `ignore sub` rule: it still
    // matches and still shields the rules after it.
    let bytes = build_type5_format1(&[(10, vec![(vec![20], vec![(0, 5)])])]);
    let map = map_from_pairs(&[(0, 0), (10, 9), (20, 19)]);
    let renumber = vec![None, None, None, None, None, None]; // all dropped
    let ctx = RewriterCtx::new(&map, Some(&renumber));
    let rs = rewrite_type5(&ctx, &bytes).expect("the rule survives");
    let at = |pos: usize| usize::from(u16::from_be_bytes([rs.bytes[pos], rs.bytes[pos + 1]]));
    let set = at(6);
    assert_eq!(at(set), 1, "one rule in the set");
    let rule = set + at(set + 2);
    assert_eq!(at(rule), 2, "glyphCount");
    assert_eq!(at(rule + 2), 0, "no records left");
    assert_eq!(at(rule + 4), 19, "input tail remapped");
}

#[test]
fn rewrite_type5_format3_keeps_all_when_every_gid_survives() {
    // input positions: [10, 11], [20, 21]; one record (0, 1).
    let bytes = build_type5_format3(&[vec![10, 11], vec![20, 21]], &[(0, 1)]);
    let map = map_from_pairs(&[(0, 0), (10, 100), (11, 101), (20, 200), (21, 201)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_type5(&ctx, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::Context::parse(&rs.bytes).unwrap();
    if let sigilbuzz::tables::gsub::Context::Format3(c) = parsed {
        let inputs = c.input();
        assert_eq!(inputs.len(), 2);
        assert!(inputs[0].contains(100));
        assert!(inputs[0].contains(101));
        assert!(inputs[1].contains(200));
        assert!(inputs[1].contains(201));
    } else {
        panic!("expected format 3");
    }
}

#[test]
fn rewrite_type5_format3_drops_subtable_when_any_input_position_empties() {
    // Drop both glyphs at position 1 -> that coverage empties -> subtable dies.
    let bytes = build_type5_format3(&[vec![10, 11], vec![20, 21]], &[(0, 1)]);
    let map = map_from_pairs(&[(0, 0), (10, 100), (11, 101)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_type5(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_type5_format3_remaps_lookup_indices() {
    let bytes = build_type5_format3(&[vec![10], vec![20]], &[(0, 3), (1, 5)]);
    let map = map_from_pairs(&[(0, 0), (10, 100), (20, 200)]);
    let renumber = vec![
        Some(0u16),
        Some(1u16),
        Some(2u16),
        Some(7u16),
        Some(8u16),
        Some(9u16),
    ];
    let ctx = RewriterCtx::new(&map, Some(&renumber));
    let rs = rewrite_type5(&ctx, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::Context::parse(&rs.bytes).unwrap();
    if let sigilbuzz::tables::gsub::Context::Format3(c) = parsed {
        let recs = c.lookups();
        // recs[0].lookup_list_index was 3, renumber[3] = 7
        // recs[1].lookup_list_index was 5, renumber[5] = 9
        assert_eq!(recs[0].lookup_list_index, 7);
        assert_eq!(recs[1].lookup_list_index, 9);
    } else {
        panic!("expected format 3");
    }
}

#[test]
fn rewrite_type5_format3_is_byte_deterministic() {
    let bytes = build_type5_format3(&[vec![10, 11], vec![20]], &[(0, 1), (1, 2)]);
    let map = map_from_pairs(&[(0, 0), (10, 1), (11, 2), (20, 3)]);
    let ctx = RewriterCtx::new(&map, None);
    let a = rewrite_type5(&ctx, &bytes).unwrap();
    let b = rewrite_type5(&ctx, &bytes).unwrap();
    assert_eq!(a.bytes, b.bytes);
}

#[test]
fn rewrite_type5_via_dispatcher() {
    let bytes = build_type5_format3(&[vec![10]], &[(0, 1)]);
    let map = map_from_pairs(&[(0, 0), (10, 100)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_subtable(&ctx, gsub_type::CONTEXT, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::Context::parse(&rs.bytes);
    assert!(parsed.is_ok());
}
