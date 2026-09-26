//! GSUB type 4 (ligature substitution) rewriter tests.

use super::*;

// ===== GSUB type 4 (Ligature Substitution) rewriter tests =====

fn build_ligature(ligature_glyph: u16, tail_components: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&ligature_glyph.to_be_bytes());
    out.extend_from_slice(&((tail_components.len() + 1) as u16).to_be_bytes());
    for c in tail_components {
        out.extend_from_slice(&c.to_be_bytes());
    }
    out
}

fn build_ligature_set(ligatures: &[(u16, Vec<u16>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(ligatures.len() as u16).to_be_bytes());
    let offsets_start = out.len();
    for _ in 0..ligatures.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    for (i, (lg, tail)) in ligatures.iter().enumerate() {
        let body_start = out.len();
        out.extend_from_slice(&build_ligature(*lg, tail));
        let slot = offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
    }
    out
}

/// Builds a type-4 subtable; `sets` is `(first_gid, [(out, tail)])`.
#[allow(clippy::type_complexity)]
fn build_type4_subtable(sets: &[(u16, Vec<(u16, Vec<u16>)>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverage offset placeholder
    out.extend_from_slice(&(sets.len() as u16).to_be_bytes());
    let set_offsets_start = out.len();
    for _ in 0..sets.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    for (i, (_first, ligs)) in sets.iter().enumerate() {
        let body_start = out.len();
        out.extend_from_slice(&build_ligature_set(ligs));
        let slot = set_offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
    }
    let cov_start = out.len();
    let first_glyphs: Vec<u16> = sets.iter().map(|(f, _)| *f).collect();
    out.extend_from_slice(&build_coverage_format1(&first_glyphs));
    out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
    out
}

#[test]
fn rewrite_type4_keeps_all_when_every_gid_survives() {
    // f=10, i=20 -> fi=100. Every gid is kept and renumbered down by
    // 1: 10->9, 20->19, 100->99.
    let bytes = build_type4_subtable(&[(10, vec![(100, vec![20])])]);
    let map = map_from_pairs(&[(0, 0), (10, 9), (20, 19), (100, 99)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_type4(&ctx, &bytes).unwrap();

    // Round-trip through the parser to validate semantics.
    let parsed = sigilbuzz::tables::gsub::Ligature::parse(&rs.bytes).unwrap();
    let (out_gid, span) = parsed.apply(&[9, 19, 30]).unwrap();
    assert_eq!(out_gid, 99);
    assert_eq!(span, 2);
}

#[test]
fn rewrite_type4_drops_ligature_when_result_gid_drops() {
    // f=10, i=20 -> fi=100; the result gid 100 is not in the map, so
    // the ligature must die. Coverage must lose the entry too: no
    // surviving LigatureSet anchors it.
    let bytes = build_type4_subtable(&[(10, vec![(100, vec![20])])]);
    let map = map_from_pairs(&[(0, 0), (10, 9), (20, 19)]);
    let ctx = RewriterCtx::new(&map, None);
    // Only one Ligature in one LigatureSet; that ligature dies, so
    // the LigatureSet is empty, the Coverage entry dies, the
    // Coverage empties, the subtable dies.
    assert!(rewrite_type4(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_type4_drops_ligature_when_component_drops() {
    // 10 + 20 + 30 -> 100; component 20 dropped -> entire ligature
    // dies (single missing component kills the rule).
    let bytes = build_type4_subtable(&[(10, vec![(100, vec![20, 30])])]);
    let map = map_from_pairs(&[(0, 0), (10, 9), (30, 29), (100, 99)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_type4(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_type4_partial_ligature_set_survives() {
    // First-component=10 has two ligatures: (10+20->100) and
    // (10+30->200). Drop component 30 -> second ligature dies, first
    // survives. LigatureSet stays, Coverage entry stays.
    let bytes = build_type4_subtable(&[(10, vec![(100, vec![20]), (200, vec![30])])]);
    let map = map_from_pairs(&[
        (0, 0),
        (10, 9),
        (20, 19),
        (100, 99),
        (200, 199), // 200 stays mapped, but its tail (30) is dropped
    ]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_type4(&ctx, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::Ligature::parse(&rs.bytes).unwrap();
    // 9, 19 still fires.
    let (out_gid, span) = parsed.apply(&[9, 19]).unwrap();
    assert_eq!(out_gid, 99);
    assert_eq!(span, 2);
    // The 30-component ligature is gone; matching 9, then anything
    // other than 19, must miss.
    assert!(parsed.apply(&[9, 200]).is_none());
}

#[test]
fn rewrite_type4_drops_subtable_when_first_component_drops() {
    // Two LigatureSets (first-components 10 and 40), but only the
    // 40-set has a survivable ligature. Dropping all of 10's
    // ligatures (output 100 dropped) collapses that Coverage entry;
    // dropping 40 itself collapses the second.
    let bytes = build_type4_subtable(&[(10, vec![(100, vec![20])]), (40, vec![(200, vec![50])])]);
    // Map keeps everything *except* 10 (first comp drops) and 100
    // (output of the only 10-ligature drops). 40 + 50 + 200 stay.
    let map = map_from_pairs(&[(0, 0), (40, 39), (50, 49), (200, 199), (20, 19)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_type4(&ctx, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::Ligature::parse(&rs.bytes).unwrap();
    // The 40-rooted ligature still fires.
    let (out_gid, span) = parsed.apply(&[39, 49]).unwrap();
    assert_eq!(out_gid, 199);
    assert_eq!(span, 2);
    // The 10-rooted ligature is gone. Its first component is no
    // longer in Coverage.
    let cov_off = u16::from_be_bytes([rs.bytes[2], rs.bytes[3]]) as usize;
    let cov = CoverageParser::parse(&rs.bytes[cov_off..]).unwrap();
    // The new first-component gid for 40 is 39; 10's new gid would
    // be 9 if it survived, but it didn't.
    assert!(cov.index_of(39).is_some());
    assert!(cov.index_of(9).is_none());
}

#[test]
fn rewrite_type4_returns_none_when_coverage_empties() {
    // Single-set, single-ligature subtable; drop everything.
    let bytes = build_type4_subtable(&[(10, vec![(100, vec![20])])]);
    let map = map_from_pairs(&[(0, 0)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_type4(&ctx, &bytes).is_none());
}

#[test]
fn rewrite_type4_locks_byte_layout_for_kept_all_case() {
    // Lock down the exact byte counts to catch accidental layout
    // regressions. Single-set, single-ligature, identity-ish remap
    // (every gid kept).
    //
    // Expected layout:
    //   header           = 6 bytes (format + cov off + setCount)
    //   ligSetOffsets    = 2 bytes (one set)
    //   LigatureSet body = 2 bytes (ligCount) + 2 bytes (ligOff)
    //                     + 4 bytes (Ligature: lig glyph + cc)
    //                     + 2 bytes (one tail component)
    //                    = 10 bytes
    //   Coverage fmt 1   = 4 bytes header + 2 bytes (one gid)
    //                    = 6 bytes
    //   Total            = 6 + 2 + 10 + 6 = 24 bytes
    let bytes = build_type4_subtable(&[(10, vec![(100, vec![20])])]);
    let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2), (100, 3)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_type4(&ctx, &bytes).unwrap();
    assert_eq!(
        rs.bytes.len(),
        24,
        "expected 24-byte type-4 subtable, got {}",
        rs.bytes.len()
    );
}

#[test]
fn rewrite_type4_is_byte_deterministic() {
    let bytes = build_type4_subtable(&[
        (10, vec![(100, vec![20]), (101, vec![25])]),
        (40, vec![(200, vec![50])]),
    ]);
    let map = map_from_pairs(&[
        (0, 0),
        (10, 1),
        (20, 2),
        (25, 3),
        (40, 4),
        (50, 5),
        (100, 6),
        (101, 7),
        (200, 8),
    ]);
    let ctx = RewriterCtx::new(&map, None);
    let a = rewrite_type4(&ctx, &bytes).unwrap();
    let b = rewrite_type4(&ctx, &bytes).unwrap();
    assert_eq!(a.bytes, b.bytes);
}

#[test]
fn rewrite_type4_via_dispatcher() {
    // Ensure the per-type dispatcher routes lookup_type=4 into
    // rewrite_type4 and not a fall-through drop.
    let bytes = build_type4_subtable(&[(10, vec![(100, vec![20])])]);
    let map = map_from_pairs(&[(0, 0), (10, 9), (20, 19), (100, 99)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_subtable(&ctx, gsub_type::LIGATURE, &bytes).unwrap();
    let parsed = sigilbuzz::tables::gsub::Ligature::parse(&rs.bytes).unwrap();
    assert_eq!(parsed.apply(&[9, 19]).unwrap(), (99, 2));
}
