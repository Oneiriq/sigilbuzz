//! CFF INDEX structures: count-prefixed arrays of variable-length
//! byte slices, plus the bounds-checked slicing helpers they use.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

// ----------------------------------------------------------------------------
// CFF INDEX structure.
// ----------------------------------------------------------------------------

/// Reads a CFF1 INDEX starting at the reader's current position and
/// advances the cursor past it. Returns one byte slice per entry.
///
/// CFF1 (the `CFF ` table) uses a `Card16` (u16) count prefix. The
/// CFF2 INDEX is layout-compatible except the count is a u32; CFF2
/// callers go through [`read_index2`].
pub(crate) fn read_index<'a>(r: &mut Reader<'a>) -> Result<Vec<&'a [u8]>> {
    let count = u32::from(r.read_u16()?);
    read_index_body(r, count)
}

/// Reads a CFF2 INDEX. Identical to [`read_index`] but with a u32
/// count prefix per the OpenType 1.8 CFF2 spec.
pub(crate) fn read_index2<'a>(r: &mut Reader<'a>) -> Result<Vec<&'a [u8]>> {
    let count = r.read_u32()?;
    read_index_body(r, count)
}

fn read_index_body<'a>(r: &mut Reader<'a>, count: u32) -> Result<Vec<&'a [u8]>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let off_size = r.read_u8()? as usize;
    if !(1..=4).contains(&off_size) {
        return Err(Error::Malformed {
            offset: r.position(),
            context: "CFF INDEX offSize out of range",
        });
    }
    // The offset array alone takes (count + 1) * offSize bytes. Check
    // that before allocating, so a huge count in a short table cannot
    // request gigabytes of memory.
    let n_offsets = (count as usize).saturating_add(1);
    if n_offsets.saturating_mul(off_size) > r.remaining() {
        return Err(Error::Truncated {
            offset: r.position(),
            context: "CFF INDEX offset array",
        });
    }
    let mut offsets = Vec::with_capacity(n_offsets);
    for _ in 0..n_offsets {
        offsets.push(read_offset(r, off_size)?);
    }
    // Data region begins after the final offset field. CFF offsets
    // are 1-based, so the last offset minus one is the data length.
    let data_start = r.position();
    let data_len = offsets
        .last()
        .and_then(|&last| (last as usize).checked_sub(1))
        .ok_or(Error::Malformed {
            offset: data_start,
            context: "CFF INDEX offsets non-monotone",
        })?;
    let rest = r.peek_bytes(r.remaining())?;
    let mut out = Vec::with_capacity(count as usize);
    for w in offsets.windows(2) {
        let a = w[0] as usize;
        let b = w[1] as usize;
        if a == 0 || b < a {
            return Err(Error::Malformed {
                offset: data_start,
                context: "CFF INDEX offsets non-monotone",
            });
        }
        let start = a - 1;
        let end = b - 1;
        if end > data_len {
            return Err(Error::Malformed {
                offset: data_start.saturating_add(end),
                context: "CFF INDEX entry past end",
            });
        }
        let entry = rest.get(start..end).ok_or(Error::Truncated {
            offset: data_start.saturating_add(start),
            context: "CFF INDEX entry past end of data",
        })?;
        out.push(entry);
    }
    // Advance the reader past the last entry.
    r.skip(data_len)?;
    Ok(out)
}

fn read_offset(r: &mut Reader<'_>, off_size: usize) -> Result<u32> {
    let b = r.read_bytes(off_size)?;
    let mut v = 0u32;
    for &byte in b {
        v = (v << 8) | u32::from(byte);
    }
    Ok(v)
}

pub(super) fn slice_at(data: &[u8], off: usize, len: usize) -> Result<&[u8]> {
    let end = off.checked_add(len).ok_or(Error::Malformed {
        offset: off,
        context: "CFF slice overflow",
    })?;
    if end > data.len() {
        return Err(Error::Truncated {
            offset: end,
            context: "CFF slice past end",
        });
    }
    Ok(&data[off..end])
}
