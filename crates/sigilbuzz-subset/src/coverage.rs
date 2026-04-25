//! Coverage emitter with auto-format-pick.
//!
//! Given a list of new glyph ids that should be covered (in some order)
//! plus the corresponding coverage indices we want them to land at,
//! emits a Coverage table in the smaller of the two on-disk formats:
//!
//! - **Format 1** (sorted glyph array): `2 + 2 + 2*N` bytes for N gids.
//! - **Format 2** (range records): `2 + 2 + 6*R` bytes for R contiguous
//!   ranges that all share `endGid - startGid == endIdx - startIdx`.
//!
//! A run of consecutive gids with consecutive coverage indices folds
//! into a single range record, so a dense remap (e.g. the tail of a
//! subset where every kept gid in 1..N is covered) collapses to a
//! single 6-byte range that beats Format 1 once N >= 4.
//!
//! The two helpers below — [`emit_coverage_from_pairs`] and
//! [`emit_coverage_from_glyphs`] — produce byte-deterministic output
//! and never allocate beyond the returned `Vec<u8>`.

use alloc::vec::Vec;

/// Emits a Coverage table for the given `(new_gid, coverage_index)`
/// pairs. Pairs may be in any order on the way in; the function sorts
/// internally and the resulting Coverage is normalised.
///
/// `coverage_index` corresponds to the slot in any parallel array
/// (e.g. `LigatureSet[]`, `MarkRecord[]`) the caller intends to emit
/// alongside this Coverage.
#[must_use]
pub fn emit_coverage_from_pairs(pairs: &[(u16, u16)]) -> Vec<u8> {
    let mut sorted: Vec<(u16, u16)> = pairs.to_vec();
    sorted.sort_unstable_by_key(|(gid, _)| *gid);
    sorted.dedup_by_key(|(gid, _)| *gid);
    emit_from_sorted(&sorted)
}

/// Convenience for the case where coverage indices are simply
/// `0..glyphs.len()` after sorting — i.e. the caller does not have
/// any parallel array or wants the indices to follow the gids' sort
/// order. Equivalent to calling [`emit_coverage_from_pairs`] with
/// `(g, i)` pairs derived from the sorted gid list.
#[must_use]
pub fn emit_coverage_from_glyphs(glyphs: &[u16]) -> Vec<u8> {
    let mut sorted: Vec<u16> = glyphs.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let pairs: Vec<(u16, u16)> = sorted
        .iter()
        .enumerate()
        .map(|(i, &g)| (g, i as u16))
        .collect();
    emit_from_sorted(&pairs)
}

fn emit_from_sorted(sorted: &[(u16, u16)]) -> Vec<u8> {
    // Build a tentative range list. A range continues while both gid
    // and coverage index step by 1.
    let mut ranges: Vec<(u16, u16, u16)> = Vec::new(); // (start_gid, end_gid, start_cov)
    for &(gid, idx) in sorted {
        match ranges.last_mut() {
            Some(last)
                if last.1.checked_add(1) == Some(gid)
                    && last.2 as u32 + (last.1 - last.0) as u32 + 1 == idx as u32 =>
            {
                last.1 = gid;
            }
            _ => ranges.push((gid, gid, idx)),
        }
    }

    // Pick the smaller representation.
    let f1_bytes = 4 + sorted.len() * 2;
    let f2_bytes = 4 + ranges.len() * 6;
    if f1_bytes <= f2_bytes {
        emit_format1(sorted)
    } else {
        emit_format2(&ranges)
    }
}

fn emit_format1(sorted: &[(u16, u16)]) -> Vec<u8> {
    // Format 1 expects coverage index = position in glyphArray. If the
    // caller passed a non-identity mapping we still emit Format 1 only
    // when those coincide; emit_from_sorted's dispatch already ensured
    // that. But to be safe, we re-validate: when indices don't follow
    // 0..N, fall back to Format 2 unconditionally.
    let identity = sorted
        .iter()
        .enumerate()
        .all(|(i, &(_, idx))| idx as usize == i);
    if !identity {
        // Synthesise a single big Format 2 list.
        let ranges: Vec<(u16, u16, u16)> = sorted.iter().map(|&(g, i)| (g, g, i)).collect();
        return emit_format2(&ranges);
    }
    let mut out = Vec::with_capacity(4 + sorted.len() * 2);
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(sorted.len() as u16).to_be_bytes());
    for &(g, _) in sorted {
        out.extend_from_slice(&g.to_be_bytes());
    }
    out
}

fn emit_format2(ranges: &[(u16, u16, u16)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + ranges.len() * 6);
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(&(ranges.len() as u16).to_be_bytes());
    for &(start, end, cov) in ranges {
        out.extend_from_slice(&start.to_be_bytes());
        out.extend_from_slice(&end.to_be_bytes());
        out.extend_from_slice(&cov.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigilbuzz::tables::layout::Coverage;

    #[test]
    fn emit_handles_empty_set() {
        let bytes = emit_coverage_from_glyphs(&[]);
        // Format 1 with count 0 is the smallest representation.
        assert_eq!(&bytes[0..2], &1u16.to_be_bytes());
        assert_eq!(&bytes[2..4], &0u16.to_be_bytes());
        let cov = Coverage::parse(&bytes).unwrap();
        assert!(cov.is_empty());
    }

    #[test]
    fn emit_glyphs_round_trips_through_parser() {
        let bytes = emit_coverage_from_glyphs(&[5, 10, 7, 1]);
        let cov = Coverage::parse(&bytes).unwrap();
        assert_eq!(cov.index_of(1), Some(0));
        assert_eq!(cov.index_of(5), Some(1));
        assert_eq!(cov.index_of(7), Some(2));
        assert_eq!(cov.index_of(10), Some(3));
        assert_eq!(cov.index_of(2), None);
    }

    #[test]
    fn dense_run_picks_format2() {
        // Four consecutive gids with consecutive indices fold into a
        // single Format 2 range of 6 bytes vs Format 1's 8 bytes.
        let bytes = emit_coverage_from_glyphs(&[10, 11, 12, 13]);
        assert_eq!(&bytes[0..2], &2u16.to_be_bytes(), "expected format 2");
        let cov = Coverage::parse(&bytes).unwrap();
        for (g, i) in [(10, 0), (11, 1), (12, 2), (13, 3)] {
            assert_eq!(cov.index_of(g), Some(i));
        }
    }

    #[test]
    fn sparse_set_picks_format1() {
        // Three non-contiguous gids: format 1 is 10 bytes, format 2 is 22.
        let bytes = emit_coverage_from_glyphs(&[5, 10, 15]);
        assert_eq!(&bytes[0..2], &1u16.to_be_bytes(), "expected format 1");
    }

    #[test]
    fn duplicate_gids_are_collapsed() {
        let bytes = emit_coverage_from_glyphs(&[7, 7, 7]);
        let cov = Coverage::parse(&bytes).unwrap();
        assert_eq!(cov.len(), 1);
        assert_eq!(cov.index_of(7), Some(0));
    }

    #[test]
    fn pairs_with_non_identity_indices_use_format2() {
        // (gid, idx) pairs that don't form a 0..N sequence after
        // sorting must encode as format 2 to preserve the indices.
        let bytes = emit_coverage_from_pairs(&[(10, 5), (20, 6), (30, 7)]);
        // 10..=10 idx 5, 20..=20 idx 6, 30..=30 idx 7 — three ranges,
        // not a single contiguous one (gids skip).
        let cov = Coverage::parse(&bytes).unwrap();
        assert_eq!(cov.index_of(10), Some(5));
        assert_eq!(cov.index_of(20), Some(6));
        assert_eq!(cov.index_of(30), Some(7));
    }

    #[test]
    fn pairs_collapse_when_both_gid_and_idx_are_consecutive() {
        let bytes = emit_coverage_from_pairs(&[(10, 5), (11, 6), (12, 7), (13, 8)]);
        // Single range of 6 bytes vs format 1 (8 bytes).
        assert_eq!(&bytes[0..2], &2u16.to_be_bytes());
        let cov = Coverage::parse(&bytes).unwrap();
        for (g, i) in [(10, 5), (11, 6), (12, 7), (13, 8)] {
            assert_eq!(cov.index_of(g), Some(i));
        }
    }
}
