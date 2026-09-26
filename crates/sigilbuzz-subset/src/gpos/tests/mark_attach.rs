//! GPOS types 4, 5 and 6 (mark attachment) rewriter tests.

use super::*;

// ----- Type 4: Mark to Base -----

#[allow(clippy::type_complexity)]
fn build_mark_base_pos(
    mark_glyphs: &[u16],
    base_glyphs: &[u16],
    mark_class_count: u16,
    marks: &[(u16, (i16, i16))],
    bases: &[Vec<Option<(i16, i16)>>],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    let mark_cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    let base_cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&mark_class_count.to_be_bytes());
    let mark_array_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    let base_array_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());

    let mark_array_start = out.len();
    out.extend_from_slice(&(marks.len() as u16).to_be_bytes());
    let mark_records_start = out.len();
    for _ in 0..marks.len() {
        out.extend_from_slice(&[0u8; 4]);
    }
    for (i, (mark_class, (x, y))) in marks.iter().enumerate() {
        let anchor_start = out.len();
        out.extend_from_slice(&build_anchor(*x, *y));
        let rel = (anchor_start - mark_array_start) as u16;
        let rec = mark_records_start + i * 4;
        out[rec..rec + 2].copy_from_slice(&mark_class.to_be_bytes());
        out[rec + 2..rec + 4].copy_from_slice(&rel.to_be_bytes());
    }

    let base_array_start = out.len();
    out.extend_from_slice(&(bases.len() as u16).to_be_bytes());
    let base_records_start = out.len();
    for _ in 0..bases.len() {
        for _ in 0..mark_class_count {
            out.extend_from_slice(&[0u8; 2]);
        }
    }
    for (i, base_row) in bases.iter().enumerate() {
        for (c, slot) in base_row.iter().enumerate() {
            if let Some((x, y)) = slot {
                let anchor_start = out.len();
                out.extend_from_slice(&build_anchor(*x, *y));
                let rel = (anchor_start - base_array_start) as u16;
                let at = base_records_start + i * (mark_class_count as usize) * 2 + c * 2;
                out[at..at + 2].copy_from_slice(&rel.to_be_bytes());
            }
        }
    }

    let mark_cov_start = out.len();
    out.extend_from_slice(&build_coverage_format1(mark_glyphs));
    let base_cov_start = out.len();
    out.extend_from_slice(&build_coverage_format1(base_glyphs));

    out[mark_cov_slot..mark_cov_slot + 2].copy_from_slice(&(mark_cov_start as u16).to_be_bytes());
    out[base_cov_slot..base_cov_slot + 2].copy_from_slice(&(base_cov_start as u16).to_be_bytes());
    out[mark_array_slot..mark_array_slot + 2]
        .copy_from_slice(&(mark_array_start as u16).to_be_bytes());
    out[base_array_slot..base_array_slot + 2]
        .copy_from_slice(&(base_array_start as u16).to_be_bytes());
    out
}

#[test]
fn rewrite_mark_base_keeps_surviving_marks_and_bases() {
    // marks: 20 (class 0, anchor (10,0)), 21 (class 1, anchor (12,0))
    // bases: 5 with anchors (250,500), (260,600) for classes 0/1.
    // Map: 20->1, 21->2, 5->3.
    let bytes = build_mark_base_pos(
        &[20, 21],
        &[5],
        2,
        &[(0, (10, 0)), (1, (12, 0))],
        &[vec![Some((250, 500)), Some((260, 600))]],
    );
    let map = map_from_pairs(&[(0, 0), (5, 3), (20, 1), (21, 2)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = one(rewrite_mark_attach(
        &ctx,
        &bytes,
        MarkAttachKind::FixedClassRow,
    ));
    let mbp = MarkBasePos::parse(&rs.bytes).unwrap();
    let a0 = mbp.attach(1, 3).unwrap();
    let a1 = mbp.attach(2, 3).unwrap();
    assert_eq!(a0.mark_anchor.x, 10);
    assert_eq!(a0.base_anchor.y, 500);
    assert_eq!(a1.mark_anchor.x, 12);
    assert_eq!(a1.base_anchor.y, 600);
}

#[test]
fn rewrite_mark_base_drops_when_marks_drop() {
    let bytes = build_mark_base_pos(&[20], &[5], 1, &[(0, (10, 0))], &[vec![Some((250, 500))]]);
    let map = map_from_pairs(&[(0, 0), (5, 1)]); // mark 20 dropped
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_mark_attach(&ctx, &bytes, MarkAttachKind::FixedClassRow).is_empty());
}

#[test]
fn rewrite_mark_base_drops_when_bases_drop() {
    let bytes = build_mark_base_pos(&[20], &[5], 1, &[(0, (10, 0))], &[vec![Some((250, 500))]]);
    let map = map_from_pairs(&[(0, 0), (20, 1)]); // base 5 dropped
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_mark_attach(&ctx, &bytes, MarkAttachKind::FixedClassRow).is_empty());
}

// ----- Type 5: Mark to Liga -----

#[allow(clippy::type_complexity)]
fn build_mark_liga_pos(
    mark_glyphs: &[u16],
    liga_glyphs: &[u16],
    mark_class_count: u16,
    marks: &[(u16, (i16, i16))],
    ligatures: &[Vec<Vec<Option<(i16, i16)>>>],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    let mark_cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    let liga_cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&mark_class_count.to_be_bytes());
    let mark_array_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    let liga_array_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());

    let mark_array_start = out.len();
    out.extend_from_slice(&(marks.len() as u16).to_be_bytes());
    let mark_records_start = out.len();
    for _ in 0..marks.len() {
        out.extend_from_slice(&[0u8; 4]);
    }
    for (i, (mark_class, (x, y))) in marks.iter().enumerate() {
        let anchor_start = out.len();
        out.extend_from_slice(&build_anchor(*x, *y));
        let rel = (anchor_start - mark_array_start) as u16;
        let rec = mark_records_start + i * 4;
        out[rec..rec + 2].copy_from_slice(&mark_class.to_be_bytes());
        out[rec + 2..rec + 4].copy_from_slice(&rel.to_be_bytes());
    }

    let liga_array_start = out.len();
    out.extend_from_slice(&(ligatures.len() as u16).to_be_bytes());
    let attach_slots_start = out.len();
    for _ in 0..ligatures.len() {
        out.extend_from_slice(&[0u8; 2]);
    }
    for (i, components) in ligatures.iter().enumerate() {
        let attach_start = out.len();
        out.extend_from_slice(&(components.len() as u16).to_be_bytes());
        let comp_records_start = out.len();
        for _ in 0..components.len() {
            for _ in 0..mark_class_count {
                out.extend_from_slice(&[0u8; 2]);
            }
        }
        for (c_i, comp) in components.iter().enumerate() {
            for (cls, slot) in comp.iter().enumerate() {
                if let Some((x, y)) = slot {
                    let anchor_start = out.len();
                    out.extend_from_slice(&build_anchor(*x, *y));
                    let rel = (anchor_start - attach_start) as u16;
                    let at = comp_records_start + c_i * (mark_class_count as usize) * 2 + cls * 2;
                    out[at..at + 2].copy_from_slice(&rel.to_be_bytes());
                }
            }
        }
        let rel = (attach_start - liga_array_start) as u16;
        let slot = attach_slots_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&rel.to_be_bytes());
    }

    let mark_cov_start = out.len();
    out.extend_from_slice(&build_coverage_format1(mark_glyphs));
    let liga_cov_start = out.len();
    out.extend_from_slice(&build_coverage_format1(liga_glyphs));

    out[mark_cov_slot..mark_cov_slot + 2].copy_from_slice(&(mark_cov_start as u16).to_be_bytes());
    out[liga_cov_slot..liga_cov_slot + 2].copy_from_slice(&(liga_cov_start as u16).to_be_bytes());
    out[mark_array_slot..mark_array_slot + 2]
        .copy_from_slice(&(mark_array_start as u16).to_be_bytes());
    out[liga_array_slot..liga_array_slot + 2]
        .copy_from_slice(&(liga_array_start as u16).to_be_bytes());
    out
}

#[test]
fn rewrite_mark_liga_keeps_components_intact() {
    // One mark (gid 30, class 0), one ligature (gid 50) with two
    // components, each component has class-0 anchor.
    let bytes = build_mark_liga_pos(
        &[30],
        &[50],
        1,
        &[(0, (5, 0))],
        &[vec![vec![Some((100, 600))], vec![Some((400, 600))]]],
    );
    let map = map_from_pairs(&[(0, 0), (30, 1), (50, 2)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = one(rewrite_mark_attach(
        &ctx,
        &bytes,
        MarkAttachKind::LigatureAttach,
    ));
    let mlp = MarkLigaPos::parse(&rs.bytes).unwrap();
    let a0 = mlp.attach(1, 2, 0).unwrap();
    let a1 = mlp.attach(1, 2, 1).unwrap();
    assert_eq!(a0.base_anchor.x, 100);
    assert_eq!(a1.base_anchor.x, 400);
}

// ----- Type 6: Mark to Mark -----

#[test]
fn rewrite_mark_mark_keeps_round_trip() {
    // Mark1 (gid 30, class 0), Mark2 (gid 5) with class-0 anchor.
    // Mark-to-mark uses the same shape as mark-to-base.
    let bytes = build_mark_base_pos(&[30], &[5], 1, &[(0, (5, 0))], &[vec![Some((100, 600))]]);
    let map = map_from_pairs(&[(0, 0), (5, 1), (30, 2)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = one(rewrite_mark_attach(
        &ctx,
        &bytes,
        MarkAttachKind::FixedClassRow,
    ));
    let mmp = MarkMarkPos::parse(&rs.bytes).unwrap();
    let attach = mmp.attach(2, 1).unwrap();
    assert_eq!(attach.base_anchor.x, 100);
    assert_eq!(attach.base_anchor.y, 600);
}

#[test]
fn rewrite_mark_base_with_truncated_base_array_drops_the_base() {
    // BaseArray claims one base with four anchor offsets, but the
    // subtable ends after the first offset. Reading the missing row
    // used to index past the end.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    bytes.extend_from_slice(&12u16.to_be_bytes()); // markCoverage
    bytes.extend_from_slice(&18u16.to_be_bytes()); // baseCoverage
    bytes.extend_from_slice(&4u16.to_be_bytes()); // markClassCount
    bytes.extend_from_slice(&24u16.to_be_bytes()); // markArray
    bytes.extend_from_slice(&36u16.to_be_bytes()); // baseArray
    bytes.extend_from_slice(&build_coverage_format1(&[10])); // 12..18
    bytes.extend_from_slice(&build_coverage_format1(&[20])); // 18..24
                                                             // MarkArray at 24: one record, class 0, anchor at +6.
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&6u16.to_be_bytes());
    bytes.extend_from_slice(&build_anchor(5, 7)); // 30..36
                                                  // BaseArray at 36: baseCount 1, then a single anchor offset.
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());

    let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_mark_attach(&ctx, &bytes, MarkAttachKind::FixedClassRow).is_empty());
}
