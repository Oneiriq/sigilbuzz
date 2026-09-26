//! `kerx` format 0: an ordered pair list searched by 32-bit key.

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub(super) struct Format0<'a> {
    data: &'a [u8],
    pairs_off: usize,
    n_pairs: u32,
}

/// Parses one format-0 subtable body. Returns `Ok(None)` on a
/// recoverable shape error so the rest of `kerx` still loads.
pub(super) fn parse_format0(
    data: &[u8],
    body_start: usize,
    sub_end: usize,
) -> Result<Option<Format0<'_>>> {
    if body_start + 16 > sub_end {
        return Err(Error::Truncated {
            offset: body_start,
            context: "kerx format 0 header",
        });
    }
    let n_pairs = u32::from_be_bytes([
        data[body_start],
        data[body_start + 1],
        data[body_start + 2],
        data[body_start + 3],
    ]);
    let pairs_off = body_start + 16; // skip nPairs + 3 search hints
    let pairs_bytes = (n_pairs as usize).saturating_mul(6);
    let required = pairs_off.checked_add(pairs_bytes).ok_or(Error::Malformed {
        offset: pairs_off,
        context: "kerx format 0 pairs overflow",
    })?;
    if required > sub_end {
        return Err(Error::Truncated {
            offset: required,
            context: "kerx format 0 pairs exceed subtable",
        });
    }
    Ok(Some(Format0 {
        data,
        pairs_off,
        n_pairs,
    }))
}

impl Format0<'_> {
    fn pair_at(&self, i: u32) -> (u32, i16) {
        let off = self.pairs_off + i as usize * 6;
        let left = u16::from_be_bytes([self.data[off], self.data[off + 1]]);
        let right = u16::from_be_bytes([self.data[off + 2], self.data[off + 3]]);
        let value = i16::from_be_bytes([self.data[off + 4], self.data[off + 5]]);
        ((u32::from(left) << 16) | u32::from(right), value)
    }

    pub(super) fn find(&self, key: u32) -> Option<i16> {
        if self.n_pairs == 0 {
            return None;
        }
        let mut lo: u32 = 0;
        let mut hi: u32 = self.n_pairs;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (k, v) = self.pair_at(mid);
            match k.cmp(&key) {
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
                core::cmp::Ordering::Equal => return Some(v),
            }
        }
        None
    }
}
