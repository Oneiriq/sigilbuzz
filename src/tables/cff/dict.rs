//! Top DICT and Private DICT parsing, Local Subr lookup and FDSelect
//! decoding.

use alloc::vec::Vec;

use super::index::read_index;
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
) -> Result<Vec<&'a [u8]>> {
    // Private DICT has the same structure as Top DICT. We care only
    // about operator 19 (Subrs), whose operand is an offset relative
    // to the start of the Private DICT.
    let priv_dict = TopDict::parse(priv_bytes)?;
    let Some(off) = priv_dict.local_subrs_off else {
        return Ok(Vec::new());
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

pub(super) fn parse_fd_select(data: &[u8], off: usize, n_glyphs: usize) -> Result<Vec<u8>> {
    let mut r = Reader::at(data, off)?;
    let format = r.read_u8()?;
    match format {
        0 => {
            let mut out = Vec::with_capacity(n_glyphs);
            for _ in 0..n_glyphs {
                out.push(r.read_u8()?);
            }
            Ok(out)
        }
        3 => {
            let n_ranges = r.read_u16()? as usize;
            let mut ranges = Vec::with_capacity(n_ranges);
            for _ in 0..n_ranges {
                let first = r.read_u16()? as usize;
                let fd = r.read_u8()?;
                ranges.push((first, fd));
            }
            let sentinel = r.read_u16()? as usize;
            Ok(fill_fd_ranges(&ranges, sentinel, n_glyphs))
        }
        _ => Err(Error::Unsupported {
            context: "CFF FDSelect format != 0/3",
        }),
    }
}

/// Expands FDSelect `(first_glyph, fd)` ranges into one entry per
/// glyph. Range `i` covers glyphs up to the next range's first glyph,
/// and the last range ends at `sentinel`. Shared with CFF2.
///
/// Ranges must ascend. `filled` skips glyphs an earlier range already
/// wrote, so unsorted ranges cannot make the fill quadratic. For
/// sorted ranges it changes nothing.
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
