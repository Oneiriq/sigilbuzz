//! Top DICT and Private DICT parsing, Local Subr lookup and FDSelect
//! decoding.

use alloc::vec::Vec;

use super::index::{read_index, Index};
use crate::error::{Error, Result};
use crate::tables::parse::Reader;

// ----------------------------------------------------------------------------
// Top DICT / Private DICT parsing.
// ----------------------------------------------------------------------------

#[derive(Debug, Default)]
pub(crate) struct TopDict {
    /// CharStrings INDEX offset. Operator 17.
    pub(super) char_strings: Option<u32>,
    /// Private DICT (size, offset). Operator 18.
    pub(super) private: Option<(u32, u32)>,
    /// FDArray offset. Operator 12 36.
    pub(super) fd_array: Option<u32>,
    /// FDSelect offset. Operator 12 37.
    pub(super) fd_select: Option<u32>,
    /// Charstring type (must be 2). Operator 12 6. Default 2.
    pub(crate) charstring_type: u32,
    /// Local subr offset relative to Private DICT. Operator 19.
    pub(crate) local_subrs_off: Option<u32>,
}

impl TopDict {
    // Each operator keeps one arm with its operand-count check inside,
    // so the dispatch reads like the spec's operator table.
    #[allow(clippy::collapsible_match)]
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self> {
        let mut out = Self {
            charstring_type: 2,
            ..Default::default()
        };
        let mut r = Reader::new(bytes);
        let mut operands: Vec<DictOperand> = Vec::new();
        while !r.is_empty() {
            let b0 = r.peek_bytes(1)?[0];
            if b0 <= 21 {
                // Operator.
                let op = if b0 == 12 {
                    r.skip(1)?;
                    let b1 = r.read_u8()?;
                    0x0C00 | u16::from(b1)
                } else {
                    r.skip(1)?;
                    u16::from(b0)
                };
                match op {
                    17 => out.char_strings = operands.last().and_then(DictOperand::as_u32),
                    18 => {
                        if operands.len() >= 2 {
                            let size = operands[operands.len() - 2].as_u32();
                            let off = operands[operands.len() - 1].as_u32();
                            if let (Some(s), Some(o)) = (size, off) {
                                out.private = Some((s, o));
                            }
                        }
                    }
                    19 => out.local_subrs_off = operands.last().and_then(DictOperand::as_u32),
                    0x0C24 => out.fd_array = operands.last().and_then(DictOperand::as_u32),
                    0x0C25 => out.fd_select = operands.last().and_then(DictOperand::as_u32),
                    0x0C06 => {
                        if let Some(v) = operands.last().and_then(DictOperand::as_u32) {
                            out.charstring_type = v;
                        }
                    }
                    _ => {}
                }
                operands.clear();
            } else {
                operands.push(read_dict_operand(&mut r)?);
            }
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum DictOperand {
    Integer(i32),
    Real(f32),
}

impl DictOperand {
    fn as_u32(&self) -> Option<u32> {
        match *self {
            Self::Integer(i) if i >= 0 => Some(i as u32),
            Self::Real(f) if f >= 0.0 => Some(f as u32),
            _ => None,
        }
    }
}

pub(super) fn read_dict_operand(r: &mut Reader<'_>) -> Result<DictOperand> {
    let b0 = r.read_u8()?;
    if b0 == 28 {
        let hi = r.read_u8()?;
        let lo = r.read_u8()?;
        let v = i16::from_be_bytes([hi, lo]) as i32;
        Ok(DictOperand::Integer(v))
    } else if b0 == 29 {
        let v = r.read_i32()?;
        Ok(DictOperand::Integer(v))
    } else if b0 == 30 {
        // Real number: nibble-packed BCD, terminated by 0xf nibble.
        // Skip the content; we don't need real operands in sigilbuzz.
        loop {
            let b = r.read_u8()?;
            if (b & 0x0F) == 0x0F || (b >> 4) == 0x0F {
                break;
            }
        }
        Ok(DictOperand::Real(0.0))
    } else if (32..=246).contains(&b0) {
        Ok(DictOperand::Integer(i32::from(b0) - 139))
    } else if (247..=250).contains(&b0) {
        let b1 = r.read_u8()?;
        Ok(DictOperand::Integer(
            (i32::from(b0) - 247) * 256 + i32::from(b1) + 108,
        ))
    } else if (251..=254).contains(&b0) {
        let b1 = r.read_u8()?;
        Ok(DictOperand::Integer(
            -(i32::from(b0) - 251) * 256 - i32::from(b1) - 108,
        ))
    } else {
        Err(Error::Malformed {
            offset: r.position(),
            context: "CFF DICT operand out of range",
        })
    }
}

// ----------------------------------------------------------------------------
// Private DICT / Local Subrs.
// ----------------------------------------------------------------------------

pub(super) fn read_local_subrs<'a>(
    data: &'a [u8],
    priv_bytes: &'a [u8],
    priv_off: usize,
) -> Result<Index<'a>> {
    // Private DICT has the same structure as Top DICT. We care only
    // about operator 19 (Subrs), whose operand is an offset relative
    // to the start of the Private DICT.
    let priv_dict = TopDict::parse(priv_bytes)?;
    let Some(off) = priv_dict.local_subrs_off else {
        return Ok(Index::default());
    };
    let subr_off = priv_off.checked_add(off as usize).ok_or(Error::Malformed {
        offset: priv_off,
        context: "CFF Local Subrs offset overflow",
    })?;
    let mut r = Reader::at(data, subr_off)?;
    read_index(&mut r)
}

// ----------------------------------------------------------------------------
// FDSelect.
// ----------------------------------------------------------------------------

/// A lazy view of an FDSelect table, which maps each glyph to the
/// Font DICT in the FDArray that holds its Private DICT. Shared with
/// CFF2.
///
/// Opening it checks only that the table fits. [`Self::fd_for_glyph`]
/// then reads the one glyph it is asked about, so nothing is expanded
/// per glyph up front.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FdSelect<'a> {
    /// Format 0: one FD index byte per glyph, exactly `n_glyphs` bytes.
    Bytes(&'a [u8]),
    /// Format 3 (`Range3`: u16 first glyph, u8 FD) or format 4
    /// (`Range4`: u32 first glyph, u16 FD, `wide`), followed by a
    /// sentinel glyph id that ends the last range.
    Ranges {
        /// The packed range records.
        ranges: &'a [u8],
        /// True for format 4 records.
        wide: bool,
        /// One past the last glyph of the last range.
        sentinel: usize,
        /// Glyph count from the CharStrings INDEX.
        n_glyphs: usize,
    },
}

impl<'a> FdSelect<'a> {
    /// Opens the FDSelect at `off`. `allow_format4` is set for CFF2;
    /// CFF1 defines only formats 0 and 3. `unsupported` names the error
    /// for any other format.
    pub(crate) fn parse(
        data: &'a [u8],
        off: usize,
        n_glyphs: usize,
        allow_format4: bool,
        unsupported: &'static str,
    ) -> Result<Self> {
        let mut r = Reader::at(data, off)?;
        let format = r.read_u8()?;
        match format {
            0 => Ok(Self::Bytes(r.read_bytes(n_glyphs)?)),
            3 => {
                let n_ranges = usize::from(r.read_u16()?);
                let ranges = r.read_bytes(n_ranges * 3)?;
                let sentinel = usize::from(r.read_u16()?);
                Ok(Self::Ranges {
                    ranges,
                    wide: false,
                    sentinel,
                    n_glyphs,
                })
            }
            4 if allow_format4 => {
                // Format 4: 32-bit ranges. Used by huge CID fonts.
                let n_ranges = r.read_u32()? as usize;
                let len = n_ranges.checked_mul(6).ok_or(Error::Truncated {
                    offset: r.position(),
                    context: "CFF FDSelect ranges",
                })?;
                let ranges = r.read_bytes(len)?;
                let sentinel = r.read_u32()? as usize;
                Ok(Self::Ranges {
                    ranges,
                    wide: true,
                    sentinel,
                    n_glyphs,
                })
            }
            _ => Err(Error::Unsupported {
                context: unsupported,
            }),
        }
    }

    /// The FD index for `gid`. A glyph that no range covers maps to
    /// FD 0.
    ///
    /// Ranges are expected to ascend. Unsorted ranges resolve the way
    /// a front-to-back fill would: range `i` covers glyphs from its
    /// first glyph up to the next range's first glyph (the sentinel
    /// for the last range), minus any glyph an earlier range already
    /// passed. The scan stops at the range that covers `gid`, or as
    /// soon as no later range can, so for sorted ranges it reads only
    /// the ranges up to `gid`.
    pub(crate) fn fd_for_glyph(&self, gid: usize) -> u8 {
        match *self {
            Self::Bytes(fds) => fds.get(gid).copied().unwrap_or(0),
            Self::Ranges {
                ranges,
                wide,
                sentinel,
                n_glyphs,
            } => {
                let stride = if wide { 6 } else { 3 };
                let first_at = |i: usize| -> usize {
                    if wide {
                        be_uint(ranges, i * stride, 4)
                    } else {
                        be_uint(ranges, i * stride, 2)
                    }
                };
                let n_ranges = ranges.len() / stride;
                // Glyphs below `filled` were covered by an earlier
                // range's span, so no later range may claim them.
                let mut filled = 0usize;
                for i in 0..n_ranges {
                    let end = if i + 1 < n_ranges {
                        first_at(i + 1)
                    } else {
                        sentinel
                    };
                    let end = end.min(n_glyphs);
                    if gid >= first_at(i).max(filled) && gid < end {
                        // Format 4 stores a u16 FD; only the low byte
                        // is kept, as FDArray indices fit in a u8 here.
                        return if wide {
                            be_uint(ranges, i * stride + 4, 2) as u8
                        } else {
                            be_uint(ranges, i * stride + 2, 1) as u8
                        };
                    }
                    filled = filled.max(end);
                    if filled > gid {
                        break;
                    }
                }
                0
            }
        }
    }
}

/// Reads a `width`-byte big-endian unsigned integer at `at`. Out of
/// range reads give 0; [`FdSelect::parse`] sized the slice so they do
/// not happen.
fn be_uint(bytes: &[u8], at: usize, width: usize) -> usize {
    bytes.get(at..at + width).map_or(0, |b| {
        b.iter().fold(0usize, |v, &x| (v << 8) | usize::from(x))
    })
}

/// Expands FDSelect `(first_glyph, fd)` ranges into one entry per
/// glyph. Range `i` covers glyphs up to the next range's first glyph,
/// and the last range ends at `sentinel`. This is the reference that
/// [`FdSelect::fd_for_glyph`] must agree with.
///
/// `filled` skips glyphs an earlier range already wrote, so unsorted
/// ranges cannot make the fill quadratic. For sorted ranges it changes
/// nothing.
#[cfg(test)]
pub(crate) fn fill_fd_ranges(ranges: &[(usize, u8)], sentinel: usize, n_glyphs: usize) -> Vec<u8> {
    let mut out = alloc::vec![0u8; n_glyphs];
    let mut filled = 0usize;
    for (i, &(first, fd)) in ranges.iter().enumerate() {
        let end = ranges.get(i + 1).map_or(sentinel, |next| next.0);
        let end = end.min(n_glyphs);
        for slot in out.get_mut(first.max(filled)..end).into_iter().flatten() {
            *slot = fd;
        }
        filled = filled.max(end);
    }
    out
}
