//! Coverage table iteration, lookup, and format 1 emit for the VARC
//! subsetter.

use alloc::vec::Vec;

use super::MAX_COVERAGE_ENTRIES;
use crate::GlyphId;

/// Iterator over coverage entries yielding `(gid, record_index)` pairs.
/// Used by [`subset_varc`](super::subset_varc) to walk the source coverage in order while
/// filtering on the kept-gid set.
///
/// Stops after [`MAX_COVERAGE_ENTRIES`] entries: a valid coverage never
/// has more, and overlapping ranges in a malformed one could otherwise
/// yield billions of entries.
pub(super) struct CoverageIter<'a> {
    bytes: &'a [u8],
    format: u16,
    count: usize,
    cursor: usize,
    /// For format 2: which range we're inside.
    range_idx: usize,
    /// For format 2: current glyph inside the range (offset from start).
    /// Wider than a gid so a full `0..=0xFFFF` range can step past its
    /// end.
    range_offset: u32,
    /// Entries yielded so far.
    yielded: usize,
}

impl<'a> CoverageIter<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        let (format, count) = match (
            crate::layout::read_u16(bytes, 0),
            crate::layout::read_u16(bytes, 2),
        ) {
            (Some(format), Some(count)) => (format, usize::from(count)),
            _ => (0, 0),
        };
        Self {
            bytes,
            format,
            count,
            cursor: 0,
            range_idx: 0,
            range_offset: 0,
            yielded: 0,
        }
    }

    fn next_entry(&mut self) -> Option<(GlyphId, usize)> {
        match self.format {
            1 => {
                if self.cursor >= self.count {
                    return None;
                }
                let g = crate::layout::read_u16(self.bytes, 4 + self.cursor * 2)?;
                let idx = self.cursor;
                self.cursor += 1;
                Some((g, idx))
            }
            2 => {
                while self.range_idx < self.count {
                    let rec = self
                        .bytes
                        .get(4 + self.range_idx * 6..)?
                        .first_chunk::<6>()?;
                    let start = u16::from_be_bytes([rec[0], rec[1]]);
                    let end = u16::from_be_bytes([rec[2], rec[3]]);
                    let start_cov = u16::from_be_bytes([rec[4], rec[5]]);
                    let span = u32::from(end.saturating_sub(start));
                    if self.range_offset > span {
                        self.range_idx += 1;
                        self.range_offset = 0;
                        continue;
                    }
                    // start + range_offset <= end, so this fits a gid.
                    let g = u16::try_from(u32::from(start) + self.range_offset).ok()?;
                    let idx = usize::from(start_cov) + self.range_offset as usize;
                    self.range_offset += 1;
                    return Some((g, idx));
                }
                None
            }
            _ => None,
        }
    }
}

impl Iterator for CoverageIter<'_> {
    type Item = (GlyphId, usize);

    fn next(&mut self) -> Option<Self::Item> {
        if self.yielded >= MAX_COVERAGE_ENTRIES {
            return None;
        }
        let entry = self.next_entry()?;
        self.yielded += 1;
        Some(entry)
    }
}

/// Builds a Coverage format-1 table from a sorted ascending iterator of
/// gids. Used by [`subset_varc`](super::subset_varc) to emit the
/// rewritten coverage. The caller passes at most one entry per gid, so
/// the count fits in 16 bits.
pub(super) fn build_coverage_format1(gids: impl IntoIterator<Item = GlyphId>) -> Vec<u8> {
    let gids: Vec<GlyphId> = gids.into_iter().collect();
    let mut out = Vec::with_capacity(4 + gids.len() * 2);
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    let count = gids.len() as u16;
    out.extend_from_slice(&count.to_be_bytes());
    for g in gids {
        out.extend_from_slice(&g.to_be_bytes());
    }
    out
}

/// Walks a coverage table for the index of `gid`. Mirrors the parser's
/// search; returns `usize` so callers can index into the glyph-records
/// vec directly.
#[cfg(test)]
pub(super) fn coverage_index_of(bytes: &[u8], gid: GlyphId) -> Option<usize> {
    CoverageIter::new(bytes).find_map(|(g, idx)| (g == gid).then_some(idx))
}
