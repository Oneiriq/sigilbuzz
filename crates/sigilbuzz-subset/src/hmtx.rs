//! `hmtx` subsetting.
//!
//! `hmtx` packs `numberOfHMetrics` long-metric records (advance + LSB)
//! at the front, then a tail of LSB-only records for glyphs whose
//! advance equals the last long-metric. We re-emit the kept gids in
//! new-gid order and fold a trailing run of identical advances into
//! the LSB-only tail.

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
    let mut out = Vec::with_capacity(kept.len() * 4);

    // Fold trailing identical advances into the LSB-only tail. This
    // is the spec-blessed compression and what hmtx parsers expect.
    let mut advances: Vec<u16> = Vec::with_capacity(kept.len());
    let mut lsbs: Vec<i16> = Vec::with_capacity(kept.len());
    for &old_gid in kept {
        advances.push(hmtx.advance(old_gid).unwrap_or(0));
        lsbs.push(hmtx.lsb(old_gid).unwrap_or(0));
    }

    // Find the longest tail where every advance equals the last
    // long-metric's advance. The long block must be at least 1
    // record per spec.
    let mut long_count = advances.len();
    if long_count > 1 {
        let last = advances[long_count - 1];
        while long_count > 1 && advances[long_count - 1] == last {
            long_count -= 1;
        }
        // The loop leaves `long_count` at the index where the run of
        // trailing identical advances starts (or at 1 when every
        // advance matches). The long block must include the run's
        // first entry, whose advance the LSB-only tail inherits, so
        // bump it by one.
        long_count += 1;
    }
    if long_count == 0 {
        long_count = 1;
    }

    for (advance, lsb) in advances.iter().zip(lsbs.iter()).take(long_count) {
        out.extend_from_slice(&advance.to_be_bytes());
        out.extend_from_slice(&lsb.to_be_bytes());
    }
    for lsb in lsbs.iter().skip(long_count) {
        out.extend_from_slice(&lsb.to_be_bytes());
    }

    Ok(HmtxOut {
        bytes: out,
        number_of_h_metrics: long_count as u16,
    })
}

#[cfg(test)]
mod tests {
    // hmtx tests live in the integration suite where a real Face is
    // available. The unit-level surface here is just a couple of
    // arithmetic loops.
}
