//! GPOS types 7 and 8 (context and chained context positioning) rewriter tests.

use super::*;

// ----- Type 7: Context Positioning -----

fn build_pos_lookup_records(records: &[(u16, u16)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(records.len() * 4);
    for (s, l) in records {
        out.extend_from_slice(&s.to_be_bytes());
        out.extend_from_slice(&l.to_be_bytes());
    }
    out
}

/// One PosRule's `(input_tail_gids, lookup_records)` pair.
type ContextPosRule = (Vec<u16>, Vec<(u16, u16)>);

/// Builds a fmt-1 context-positioning subtable around one
/// PosRuleSet per Coverage entry. Each rule lists the input tail
/// gids + a single PosLookupRecord.
fn build_context_pos_format1(covered: &[u16], rules_per_set: &[Vec<ContextPosRule>]) -> Vec<u8> {
    assert_eq!(covered.len(), rules_per_set.len());
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(rules_per_set.len() as u16).to_be_bytes());
    let set_offsets_start = out.len();
    for _ in 0..rules_per_set.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    for (i, rules) in rules_per_set.iter().enumerate() {
        let set_start = out.len();
        // RuleSet: u16 ruleCount + Offset16[count] + bodies.
        out.extend_from_slice(&(rules.len() as u16).to_be_bytes());
        let rule_off_start = out.len();
        for _ in 0..rules.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        for (ri, (input_tail, recs)) in rules.iter().enumerate() {
            let body_start = out.len();
            let glyph_count = (input_tail.len() + 1) as u16;
            out.extend_from_slice(&glyph_count.to_be_bytes());
            out.extend_from_slice(&(recs.len() as u16).to_be_bytes());
            for g in input_tail {
                out.extend_from_slice(&g.to_be_bytes());
            }
            out.extend_from_slice(&build_pos_lookup_records(recs));
            let rel = (body_start - set_start) as u16;
            let slot = rule_off_start + ri * 2;
            out[slot..slot + 2].copy_from_slice(&rel.to_be_bytes());
        }
        let slot = set_offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&(set_start as u16).to_be_bytes());
    }
    let cov_start = out.len();
    out.extend_from_slice(&build_coverage_format1(covered));
    out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
    out
}

#[test]
fn rewrite_context_pos_format1_remaps_input_tail_and_lookup_index() {
    // Coverage [10]; one rule with input tail [20], one PosLookupRecord
    // (sequence_index=0, lookup_list_index=3).
    let bytes = build_context_pos_format1(&[10], &[vec![(vec![20], vec![(0, 3)])]]);
    let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2)]);
    let renumber: Vec<Option<u16>> = vec![Some(0), Some(1), Some(2), Some(7)];
    let ctx = RewriterCtx::new(&map, Some(&renumber));
    let rs = rewrite_context_pos(&ctx, &bytes).unwrap();
    // Subtable parses through the shared layout::Context1 helper.
    let parsed = sigilbuzz::tables::gpos::ContextPos::parse(&rs.bytes).unwrap();
    assert!(matches!(
        parsed,
        sigilbuzz::tables::gpos::ContextPos::Format1(_)
    ));
    // Walk the rewritten bytes manually to verify the lookup index.
    // Layout: u16 fmt, Offset16 cov, u16 setCount, Offset16[count],
    // then sets with rules. We pluck the first set's first rule.
    let set_off = u16::from_be_bytes([rs.bytes[6], rs.bytes[7]]) as usize;
    let rule_off = u16::from_be_bytes([rs.bytes[set_off + 2], rs.bytes[set_off + 3]]) as usize;
    let rule_abs = set_off + rule_off;
    // Rule: u16 glyphCount, u16 recCount, u16 tail[count-1], records[]
    let glyph_count = u16::from_be_bytes([rs.bytes[rule_abs], rs.bytes[rule_abs + 1]]);
    assert_eq!(glyph_count, 2);
    let rec_count = u16::from_be_bytes([rs.bytes[rule_abs + 2], rs.bytes[rule_abs + 3]]);
    assert_eq!(rec_count, 1);
    // Tail (1 entry): the remapped 20 -> 2.
    let tail0 = u16::from_be_bytes([rs.bytes[rule_abs + 4], rs.bytes[rule_abs + 5]]);
    assert_eq!(tail0, 2);
    // Record: sequence=0, lookup=7 (renumbered from 3).
    let seq = u16::from_be_bytes([rs.bytes[rule_abs + 6], rs.bytes[rule_abs + 7]]);
    let li = u16::from_be_bytes([rs.bytes[rule_abs + 8], rs.bytes[rule_abs + 9]]);
    assert_eq!(seq, 0);
    assert_eq!(li, 7);
}

#[test]
fn rewrite_context_pos_format1_drops_when_input_tail_drops() {
    let bytes = build_context_pos_format1(&[10], &[vec![(vec![20], vec![(0, 3)])]]);
    // Drop gid 20: the rule can't fire.
    let map = map_from_pairs(&[(0, 0), (10, 1)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_context_pos(&ctx, &bytes).is_none());
}

/// Builds a fmt-3 context-positioning subtable.
fn build_context_pos_format3(coverages: &[Vec<u16>], records: &[(u16, u16)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&3u16.to_be_bytes()); // posFormat
    out.extend_from_slice(&(coverages.len() as u16).to_be_bytes());
    out.extend_from_slice(&(records.len() as u16).to_be_bytes());
    let cov_slots = out.len();
    for _ in 0..coverages.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    out.extend_from_slice(&build_pos_lookup_records(records));
    for (i, gs) in coverages.iter().enumerate() {
        let body_start = out.len() as u16;
        out.extend_from_slice(&build_coverage_format1(gs));
        let slot = cov_slots + i * 2;
        out[slot..slot + 2].copy_from_slice(&body_start.to_be_bytes());
    }
    out
}

#[test]
fn rewrite_context_pos_format3_remaps_lookup_index() {
    let bytes = build_context_pos_format3(&[vec![10, 11], vec![20]], &[(0, 1), (1, 5)]);
    let map = map_from_pairs(&[(0, 0), (10, 1), (11, 2), (20, 3)]);
    // renumber drops 5 -> record at sequence_index=1 falls out, only
    // the (0, 1) record with new lookup index 9 survives.
    let renumber: Vec<Option<u16>> = vec![
        Some(0u16),
        Some(9u16),
        Some(2u16),
        Some(3u16),
        Some(4u16),
        None,
    ];
    let ctx = RewriterCtx::new(&map, Some(&renumber));
    let rs = rewrite_context_pos(&ctx, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gpos::ContextPos::parse(&rs.bytes).unwrap();
    assert!(matches!(
        parsed,
        sigilbuzz::tables::gpos::ContextPos::Format3(_)
    ));
    // Pluck the surviving record. Layout: u16 fmt, u16 glyphCount,
    // u16 recCount, Offset16[glyphCount], record[recCount].
    let glyph_count = u16::from_be_bytes([rs.bytes[2], rs.bytes[3]]);
    assert_eq!(glyph_count, 2);
    let rec_count = u16::from_be_bytes([rs.bytes[4], rs.bytes[5]]);
    assert_eq!(rec_count, 1);
    let recs_start = 6 + glyph_count as usize * 2;
    let li = u16::from_be_bytes([rs.bytes[recs_start + 2], rs.bytes[recs_start + 3]]);
    assert_eq!(li, 9);
}

#[test]
fn rewrite_context_pos_format3_keeps_rule_when_record_targets_dropped() {
    // Left without records the rule acts as `ignore pos`: it still
    // stops the later subtables of its lookup, so it must stay.
    let bytes = build_context_pos_format3(&[vec![10]], &[(0, 5)]);
    let map = map_from_pairs(&[(0, 0), (10, 1)]);
    let renumber: Vec<Option<u16>> = vec![None, None, None, None, None, None];
    let ctx = RewriterCtx::new(&map, Some(&renumber));
    let rs = rewrite_context_pos(&ctx, &bytes).expect("the rule survives");
    let glyph_count = u16::from_be_bytes([rs.bytes[2], rs.bytes[3]]);
    let rec_count = u16::from_be_bytes([rs.bytes[4], rs.bytes[5]]);
    assert_eq!((glyph_count, rec_count), (1, 0));
}

#[test]
fn rewrite_chain_context_pos_format3_keeps_source_ignore_rule() {
    // Compiled from `ignore pos a b' c;`: no records to begin with.
    let bytes = build_chain_context_pos_format3(&[vec![5]], &[vec![10]], &[vec![30]], &[]);
    let map = map_from_pairs(&[(0, 0), (5, 50), (10, 100), (30, 300)]);
    let renumber: Vec<Option<u16>> = vec![Some(0)];
    let ctx = RewriterCtx::new(&map, Some(&renumber));
    let rs = rewrite_chain_context_pos(&ctx, &bytes).expect("the ignore rule survives");
    // u16 fmt, then per sequence a count and its Offset16s, then
    // the record count.
    let at = |pos: usize| u16::from_be_bytes([rs.bytes[pos], rs.bytes[pos + 1]]);
    assert_eq!((at(2), at(6), at(10), at(14)), (1, 1, 1, 0));
}

// ----- Type 8: Chained Context Positioning -----

/// Builds a fmt-3 chained-context positioning subtable.
fn build_chain_context_pos_format3(
    backtrack: &[Vec<u16>],
    input: &[Vec<u16>],
    lookahead: &[Vec<u16>],
    records: &[(u16, u16)],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&3u16.to_be_bytes());
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
    out.extend_from_slice(&build_pos_lookup_records(records));
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
fn rewrite_chain_context_pos_format3_keeps_when_all_survive() {
    let bytes = build_chain_context_pos_format3(
        &[vec![5]],
        &[vec![10, 11], vec![12]],
        &[vec![30]],
        &[(0, 1)],
    );
    let map = map_from_pairs(&[(0, 0), (5, 50), (10, 100), (11, 101), (12, 102), (30, 300)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_chain_context_pos(&ctx, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gpos::ChainContextPos::parse(&rs.bytes).unwrap();
    assert!(matches!(
        parsed,
        sigilbuzz::tables::gpos::ChainContextPos::Format3(_)
    ));
}

#[test]
fn rewrite_chain_context_pos_format3_drops_when_backtrack_empties() {
    let bytes = build_chain_context_pos_format3(&[vec![5]], &[vec![10]], &[vec![30]], &[(0, 1)]);
    // Drop gid 5: backtrack coverage empties -> subtable dies.
    let map = map_from_pairs(&[(0, 0), (10, 100), (30, 300)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_chain_context_pos(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_chain_context_pos_format3_renumbers_lookup_index() {
    let bytes = build_chain_context_pos_format3(&[], &[vec![10]], &[], &[(0, 5)]);
    let map = map_from_pairs(&[(0, 0), (10, 100)]);
    let renumber: Vec<Option<u16>> = vec![Some(0), Some(1), Some(2), Some(3), Some(4), Some(11)];
    let ctx = RewriterCtx::new(&map, Some(&renumber));
    let rs = rewrite_chain_context_pos(&ctx, &bytes).unwrap();
    // Format-3 chain layout: u16 fmt, u16 btCount, Offset16[bt],
    // u16 inCount, Offset16[in], u16 laCount, Offset16[la],
    // u16 recCount, record[recCount], coverages.
    let mut p = 2usize;
    let bt_count = u16::from_be_bytes([rs.bytes[p], rs.bytes[p + 1]]) as usize;
    p += 2 + bt_count * 2;
    let in_count = u16::from_be_bytes([rs.bytes[p], rs.bytes[p + 1]]) as usize;
    p += 2 + in_count * 2;
    let la_count = u16::from_be_bytes([rs.bytes[p], rs.bytes[p + 1]]) as usize;
    p += 2 + la_count * 2;
    let _rec_count = u16::from_be_bytes([rs.bytes[p], rs.bytes[p + 1]]);
    p += 2;
    let li = u16::from_be_bytes([rs.bytes[p + 2], rs.bytes[p + 3]]);
    assert_eq!(li, 11);
}

#[test]
fn rewrite_chain_context_pos_via_dispatcher() {
    let bytes = build_chain_context_pos_format3(&[], &[vec![10]], &[], &[(0, 1)]);
    let map = map_from_pairs(&[(0, 0), (10, 100)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = one(rewrite_subtable(&ctx, gpos_type::CHAINED_CONTEXT, &bytes));
    assert!(sigilbuzz::tables::gpos::ChainContextPos::parse(&rs.bytes).is_ok());
}

#[test]
fn rewrite_context_pos_via_dispatcher() {
    let bytes = build_context_pos_format3(&[vec![10]], &[(0, 1)]);
    let map = map_from_pairs(&[(0, 0), (10, 100)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = one(rewrite_subtable(&ctx, gpos_type::CONTEXT, &bytes));
    assert!(sigilbuzz::tables::gpos::ContextPos::parse(&rs.bytes).is_ok());
}
