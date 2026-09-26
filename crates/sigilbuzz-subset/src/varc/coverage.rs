//! Coverage table iteration, lookup, and format 1 emit for the VARC
//! subsetter.

use alloc::vec::Vec;

use crate::GlyphId;

/// Iterator over coverage entries yielding `(gid, record_index)` pairs.
/// Used by [`subset_varc`](super::subset_varc) to walk the source coverage in order while
/// filtering on the kept-gid set.
pub(super) struct CoverageIter<'a> {
    bytes: &'a [u8],
    format: u16,
    count: usize,
    cursor: usize,
    /// For format 2: which range we're inside.
    range_idx: usize,
    /// For format 2: current glyph inside the range (offset from start).
    range_offset: u16,
}

impl<'a> CoverageIter<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        if bytes.len() < 4 {
            return Self {
                bytes,
                format: 0,
                count: 0,
                cursor: 0,
                range_idx: 0,
                range_offset: 0,
            };
        }
        let format = u16::from_be_bytes([bytes[0], bytes[1]]);
        let count = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
        Self {
            bytes,
            format,
            count,
            cursor: 0,
            range_idx: 0,
            range_offset: 0,
        }
    }
}

impl Iterator for CoverageIter<'_> {
    type Item = (GlyphId, usize);

    fn next(&mut self) -> Option<Self::Item> {
        match self.format {
            1 => {
                if self.cursor >= self.count {
                    return None;
                }
                let off = 4 + self.cursor * 2;
                if off + 2 > self.bytes.len() {
                    return None;
                }
                let g = u16::from_be_bytes([self.bytes[off], self.bytes[off + 1]]);
                let idx = self.cursor;
                self.cursor += 1;
                Some((g, idx))
            }
            2 => {
                while self.range_idx < self.count {
                    let off = 4 + self.range_idx * 6;
                    if off + 6 > self.bytes.len() {
                        return None;
                    }
                    let start = u16::from_be_bytes([self.bytes[off], self.bytes[off + 1]]);
                    let end = u16::from_be_bytes([self.bytes[off + 2], self.bytes[off + 3]]);
                    let start_cov = u16::from_be_bytes([self.bytes[off + 4], self.bytes[off + 5]]);
                    let span = end.saturating_sub(start);
                    if self.range_offset > span {
                        self.range_idx += 1;
                        self.range_offset = 0;
                        continue;
                    }
                    let g = start.checked_add(self.range_offset)?;
                    let idx = (start_cov as usize) + (self.range_offset as usize);
                    self.range_offset += 1;
                    return Some((g, idx));
                }
                None
            }
            _ => None,
        }
    }
}

/// Builds a Coverage format-1 table from a sorted ascending iterator of
/// gids. Used by [`subset_varc`](super::subset_varc) to emit the rewritten coverage.
pub(super) fn build_coverage_format1(gids: impl IntoIterator<Item = GlyphId>) -> Vec<u8> {
    let gids: Vec<GlyphId> = gids.into_iter().collect();
    let mut out = Vec::with_capacity(4 + gids.len() * 2);
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    #[allow(clippy::cast_possible_truncation)]
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
pub(super) fn coverage_index_of(bytes: &[u8], gid: GlyphId) -> Option<usize> {
    if bytes.len() < 4 {
        return None;
    }
    let format = u16::from_be_bytes([bytes[0], bytes[1]]);
    let count = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
    match format {
        1 => {
            let need = 4 + count * 2;
            if bytes.len() < need {
                return None;
            }
            for i in 0..count {
                let off = 4 + i * 2;
                let g = u16::from_be_bytes([bytes[off], bytes[off + 1]]);
                if g == gid {
                    return Some(i);
                }
            }
            None
        }
        2 => {
            let need = 4 + count * 6;
            if bytes.len() < need {
                return None;
            }
            for i in 0..count {
                let off = 4 + i * 6;
                let start = u16::from_be_bytes([bytes[off], bytes[off + 1]]);
                let end = u16::from_be_bytes([bytes[off + 2], bytes[off + 3]]);
                let start_cov = u16::from_be_bytes([bytes[off + 4], bytes[off + 5]]);
                if gid >= start && gid <= end {
                    let cov = (start_cov as usize) + (gid - start) as usize;
                    return Some(cov);
                }
            }
            None
        }
        _ => None,
    }
}
