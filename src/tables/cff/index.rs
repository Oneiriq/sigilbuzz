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
    let mut offsets = Vec::with_capacity(count as usize + 1);
    for _ in 0..=count {
        offsets.push(read_offset(r, off_size)?);
    }
    // Data region begins after the final offset field.
    let data_start = r.position();
    let mut out = Vec::with_capacity(count as usize);
    for w in offsets.windows(2) {
        let a = w[0] as usize;
        let b = w[1] as usize;
        if a == 0 || b < a {
            return Err(Error::Malformed {
                offset: r.position(),
                context: "CFF INDEX offsets non-monotone",
            });
        }
        // CFF offsets are 1-based.
        let start = data_start + a - 1;
        let end = data_start + b - 1;
        if end > data_start + offsets[offsets.len() - 1] as usize - 1 + 1 {
            // soft sanity check; the strict bound is data length.
        }
        if end > data_start + (*offsets.last().unwrap() as usize - 1) {
            return Err(Error::Malformed {
                offset: end,
                context: "CFF INDEX entry past end",
            });
        }
        // Build slice manually via reader's underlying data.
        let slice = reader_slice(r, start, end)?;
        out.push(slice);
    }
    // Advance the reader past the last entry.
    let total = *offsets.last().unwrap_or(&1) as usize - 1;
    r.seek(data_start + total)?;
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

/// Hack helper: slices out of the Reader's underlying buffer by
/// absolute offsets. Exposed via `Reader::peek_bytes` after a `seek`
/// round-trip. Used only during INDEX parsing above.
fn reader_slice<'a>(r: &Reader<'a>, start: usize, end: usize) -> Result<&'a [u8]> {
    let mut tmp = *r;
    tmp.seek(start)?;
    let n = end - start;
    tmp.peek_bytes(n)
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
