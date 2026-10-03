//! `hmtx` subsetting.
//!
//! `hmtx` packs `numberOfHMetrics` long-metric records (advance + LSB)
//! at the front, then a tail of LSB-only records for glyphs whose
//! advance equals the last long-metric. We re-emit the kept gids in
//! new-gid order and fold a trailing run of identical advances into
//! the LSB-only tail. `vmtx` has the same layout, so
//! [`emit_long_metrics`] serves [`crate::vmtx`] too.

use alloc::vec::Vec;

use sigilbuzz::Face;

use crate::SubsetError;

/// Output of hmtx subsetting.
pub struct HmtxOut {
    /// New `hmtx` bytes.
    pub bytes: Vec<u8>,
    /// New `numberOfHMetrics` value to write back into `hhea`.
    pub number_of_h_metrics: u16,
}

/// Subsets `hmtx` for the given new-gid order.
pub fn subset_hmtx(face: &Face<'_>, kept: &[u16]) -> Result<HmtxOut, SubsetError> {
    let hmtx = face.hmtx()?;
    let advances: Vec<u16> = kept
        .iter()
        .map(|&old_gid| hmtx.advance(old_gid).unwrap_or(0))
        .collect();
    let lsbs: Vec<i16> = kept
        .iter()
        .map(|&old_gid| hmtx.lsb(old_gid).unwrap_or(0))
        .collect();
    let (bytes, number_of_h_metrics) = emit_long_metrics(&advances, &lsbs);
    Ok(HmtxOut {
        bytes,
        number_of_h_metrics,
    })
}

/// Serializes an `hmtx` or `vmtx` body from per-glyph `advances` and
/// side `bearings` (one each per glyph, in new-gid order). Returns the
/// bytes and the long-metric count to write into `hhea` or `vhea`.
///
/// The longest trailing run of glyphs that share the last advance is
/// folded into the bearing-only tail, the spec-blessed compression
/// that metrics parsers expect. The long block keeps the run's first
/// glyph, whose advance the tail inherits, and is never empty. The
/// glyph count is at most 65,535, so the count fits in a `u16`.
pub(crate) fn emit_long_metrics(advances: &[u16], bearings: &[i16]) -> (Vec<u8>, u16) {
    debug_assert_eq!(advances.len(), bearings.len());
    let mut long_count = advances.len();
    if let Some(&last) = advances.last() {
        while long_count > 1 && advances[long_count - 1] == last {
            long_count -= 1;
        }
        // The loop stops at the index where the run of trailing
        // identical advances starts (or at 1 when every advance
        // matches). Bump it by one so the long block includes the
        // run's first entry. A single glyph stays a single long entry.
        // When every advance matches this keeps two long entries where
        // one would do; the output stays byte-identical to earlier
        // releases.
        if advances.len() > 1 {
            long_count += 1;
        }
    }
    let long_count = long_count.max(1);

    let mut out = Vec::with_capacity(advances.len() * 4);
    for (advance, bearing) in advances.iter().zip(bearings).take(long_count) {
        out.extend_from_slice(&advance.to_be_bytes());
        out.extend_from_slice(&bearing.to_be_bytes());
    }
    for bearing in bearings.iter().skip(long_count) {
        out.extend_from_slice(&bearing.to_be_bytes());
    }
    (out, long_count as u16)
}

#[cfg(test)]
mod tests {
    use super::emit_long_metrics;
    use alloc::vec::Vec;

    /// Splits an emitted body back into `(advance, bearing)` pairs,
    /// the way a metrics parser reads it.
    fn decode(bytes: &[u8], long_count: u16, glyphs: usize) -> Vec<(u16, i16)> {
        let long = usize::from(long_count);
        let mut out = Vec::new();
        let mut last = 0;
        for gid in 0..glyphs {
            if gid < long {
                let at = gid * 4;
                last = u16::from_be_bytes([bytes[at], bytes[at + 1]]);
                out.push((last, i16::from_be_bytes([bytes[at + 2], bytes[at + 3]])));
            } else {
                let at = long * 4 + (gid - long) * 2;
                out.push((last, i16::from_be_bytes([bytes[at], bytes[at + 1]])));
            }
        }
        out
    }

    #[test]
    fn trailing_equal_advances_fold_into_the_bearing_tail() {
        let advances = [500, 600, 1000, 1000, 1000];
        let bearings = [1, 2, 3, 4, 5];
        let (bytes, long) = emit_long_metrics(&advances, &bearings);
        assert_eq!(long, 3, "the run's first glyph stays long");
        assert_eq!(bytes.len(), 3 * 4 + 2 * 2);
        let want: Vec<(u16, i16)> = advances.iter().copied().zip(bearings).collect();
        assert_eq!(decode(&bytes, long, 5), want);
    }

    #[test]
    fn distinct_advances_stay_long() {
        let (bytes, long) = emit_long_metrics(&[1, 2, 3], &[-1, -2, -3]);
        assert_eq!(long, 3);
        assert_eq!(decode(&bytes, long, 3), [(1, -1), (2, -2), (3, -3)]);
    }

    #[test]
    fn one_shared_advance_keeps_a_short_long_block() {
        let (bytes, long) = emit_long_metrics(&[7, 7, 7], &[0, 1, 2]);
        assert_eq!(long, 2);
        assert_eq!(bytes.len(), 2 * 4 + 2);
        assert_eq!(decode(&bytes, long, 3), [(7, 0), (7, 1), (7, 2)]);
        let (bytes, long) = emit_long_metrics(&[9], &[4]);
        assert_eq!((bytes.len(), long), (4, 1));
    }
}
