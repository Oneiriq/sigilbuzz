//! GPOS type 3 (cursive attachment) rewriter tests.

use super::*;

// ----- Type 3: Cursive -----

type CursiveAnchor = Option<(i16, i16)>;

fn build_cursive(covered: &[u16], records: &[(CursiveAnchor, CursiveAnchor)]) -> Vec<u8> {
    assert_eq!(covered.len(), records.len());
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(records.len() as u16).to_be_bytes()); // entryExitCount
    let records_start = out.len();
    for _ in 0..records.len() {
        out.extend_from_slice(&[0u8; 4]); // placeholders
    }
    for (i, (entry, exit)) in records.iter().enumerate() {
        let entry_off = if let Some((x, y)) = entry {
            let pos = out.len() as u16;
            out.extend_from_slice(&build_anchor(*x, *y));
            pos
        } else {
            0
        };
        let exit_off = if let Some((x, y)) = exit {
            let pos = out.len() as u16;
            out.extend_from_slice(&build_anchor(*x, *y));
            pos
        } else {
            0
        };
        let rec = records_start + i * 4;
        out[rec..rec + 2].copy_from_slice(&entry_off.to_be_bytes());
        out[rec + 2..rec + 4].copy_from_slice(&exit_off.to_be_bytes());
    }
    let cov_start = out.len();
    out.extend_from_slice(&build_coverage_format1(covered));
    out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
    out
}

#[test]
fn rewrite_cursive_preserves_anchors() {
    // Two glyphs with entry/exit anchors.
    let bytes = build_cursive(
        &[10, 20],
        &[
            (Some((0, 0)), Some((100, 0))),
            (Some((0, 0)), Some((200, 0))),
        ],
    );
    let map = map_from_pairs(&[(0, 0), (10, 1), (20, 2)]);
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_cursive(&ctx, &bytes).unwrap();
    // Verify it parses as a valid GPOS subtable header: format 1.
    assert_eq!(&rs.bytes[0..2], &1u16.to_be_bytes());
    let count = u16::from_be_bytes([rs.bytes[4], rs.bytes[5]]);
    assert_eq!(count, 2);
    // Verify Coverage at offset header has glyphs 1, 2.
    let cov_off = u16::from_be_bytes([rs.bytes[2], rs.bytes[3]]) as usize;
    let cov_glyphs = parse_coverage_glyphs(&rs.bytes[cov_off..]);
    assert_eq!(cov_glyphs, vec![1, 2]);
}

#[test]
fn rewrite_cursive_drops_dropped_entries() {
    let bytes = build_cursive(
        &[10, 20],
        &[
            (Some((0, 0)), Some((100, 0))),
            (Some((0, 0)), Some((200, 0))),
        ],
    );
    let map = map_from_pairs(&[(0, 0), (10, 1)]); // 20 dropped
    let ctx = RewriterCtx::new(&map, None);
    let rs = rewrite_cursive(&ctx, &bytes).unwrap();
    let count = u16::from_be_bytes([rs.bytes[4], rs.bytes[5]]);
    assert_eq!(count, 1);
}

#[test]
fn rewrite_cursive_returns_none_when_all_dropped() {
    let bytes = build_cursive(&[10], &[(Some((0, 0)), Some((100, 0)))]);
    let map = map_from_pairs(&[(0, 0)]);
    let ctx = RewriterCtx::new(&map, None);
    assert!(rewrite_cursive(&ctx, &bytes).is_none());
}
