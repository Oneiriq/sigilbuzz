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
    /// Charset: 0 to 2 for a predefined one, otherwise an offset.
    /// Operator 15. Default 0, ISOAdobe.
    pub(super) charset: Option<u32>,
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
                    15 => out.charset = operands.last().and_then(DictOperand::as_u32),
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
/// Opening it checks that the table fits and, for formats 3 and 4,
/// whether the ranges ascend, which takes one pass over the range
/// records. [`Self::fd_for_glyph`] then reads only what the one glyph
/// it is asked about needs, so nothing is expanded per glyph up front.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FdSelect<'a> {
    /// Format 0: one FD index byte per glyph, exactly `n_glyphs` bytes.
    Bytes(&'a [u8]),
    /// Format 3 or format 4.
    Ranges(FdRanges<'a>),
}

/// The range records of FDSelect format 3 (`Range3`: u16 first glyph,
/// u8 FD) or format 4 (`Range4`: u32 first glyph, u16 FD), followed by
/// a sentinel glyph id that ends the last range. Format 4 is what lets
/// a CFF2 font have more than 256 Font DICTs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FdRanges<'a> {
    /// The packed range records.
    ranges: &'a [u8],
    /// True for format 4 records.
    wide: bool,
    /// One past the last glyph of the last range.
    sentinel: usize,
    /// Glyph count from the CharStrings INDEX.
    n_glyphs: usize,
    /// True when no range starts before the one ahead of it. Such
    /// ranges cannot overlap, since each ends where the next begins, so
    /// the one range that can hold a glyph is found by binary search.
    ascending: bool,
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
                Ok(Self::Ranges(FdRanges::new(
                    ranges, false, sentinel, n_glyphs,
                )))
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
                Ok(Self::Ranges(FdRanges::new(
                    ranges, true, sentinel, n_glyphs,
                )))
            }
            _ => Err(Error::Unsupported {
                context: unsupported,
            }),
        }
    }

    /// The FD index for `gid`. A glyph that no range covers maps to
    /// FD 0. See [`FdRanges`] for how ranges resolve.
    pub(crate) fn fd_for_glyph(&self, gid: usize) -> u16 {
        match *self {
            Self::Bytes(fds) => fds.get(gid).copied().map_or(0, u16::from),
            Self::Ranges(ranges) => ranges.fd_for_glyph(gid),
        }
    }

    /// True when lookups binary-search the ranges.
    #[cfg(test)]
    pub(crate) fn binary_searches(&self) -> bool {
        matches!(self, Self::Ranges(r) if r.ascending)
    }
}

impl<'a> FdRanges<'a> {
    fn new(ranges: &'a [u8], wide: bool, sentinel: usize, n_glyphs: usize) -> Self {
        // `Cff::parse` and `Cff2::parse` run this for every outline drawn
        // through `Face`, so it reads each first glyph once, straight
        // from its record.
        let ascending = if wide {
            ascends(
                ranges
                    .chunks_exact(6)
                    .map(|r| u32::from_be_bytes([r[0], r[1], r[2], r[3]]) as usize),
            )
        } else {
            ascends(
                ranges
                    .chunks_exact(3)
                    .map(|r| usize::from(u16::from_be_bytes([r[0], r[1]]))),
            )
        };
        Self {
            ranges,
            wide,
            sentinel,
            n_glyphs,
            ascending,
        }
    }

    /// Bytes per range record.
    fn stride(&self) -> usize {
        if self.wide {
            6
        } else {
            3
        }
    }

    /// Number of range records.
    fn len(&self) -> usize {
        self.ranges.len() / self.stride()
    }

    /// First glyph of range `i`.
    fn first(&self, i: usize) -> usize {
        let width = if self.wide { 4 } else { 2 };
        be_uint(self.ranges, i * self.stride(), width)
    }

    /// FD of range `i`. Format 3 stores a u8 and format 4 a u16.
    fn fd(&self, i: usize) -> u16 {
        if self.wide {
            be_uint(self.ranges, i * self.stride() + 4, 2) as u16
        } else {
            be_uint(self.ranges, i * self.stride() + 2, 1) as u16
        }
    }

    /// One past the last glyph range `i` may cover: the next range's
    /// first glyph, or the sentinel for the last range, capped at the
    /// glyph count.
    fn end(&self, i: usize) -> usize {
        let end = if i + 1 < self.len() {
            self.first(i + 1)
        } else {
            self.sentinel
        };
        end.min(self.n_glyphs)
    }

    /// The FD for `gid`, or 0 when no range covers it.
    ///
    /// Ranges are expected to ascend, and then range `i` covers its
    /// first glyph up to the next range's first glyph (the sentinel for
    /// the last range). Unsorted ranges resolve the way a front-to-back
    /// fill would: the same span, minus any glyph an earlier range
    /// already passed.
    fn fd_for_glyph(&self, gid: usize) -> u16 {
        if self.ascending {
            self.search(gid)
        } else {
            self.scan(gid)
        }
    }

    /// Binary search over ascending ranges. Only the last range that
    /// starts at or before `gid` can cover it. Every later range starts
    /// past `gid`, and every earlier one ends where its successor
    /// starts, at or before `gid`.
    fn search(&self, gid: usize) -> u16 {
        // Ranges below `lo` start at or before `gid`, and ranges from
        // `hi` on start after it.
        let (mut lo, mut hi) = (0, self.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.first(mid) <= gid {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        match lo.checked_sub(1) {
            Some(i) if gid < self.end(i) => self.fd(i),
            _ => 0,
        }
    }

    /// Front-to-back scan for ranges that do not ascend. It stops at
    /// the range that covers `gid`, or as soon as no later range can.
    fn scan(&self, gid: usize) -> u16 {
        // Glyphs below `filled` were covered by an earlier range's span,
        // so no later range may claim them.
        let mut filled = 0usize;
        for i in 0..self.len() {
            let end = self.end(i);
            if gid >= self.first(i).max(filled) && gid < end {
                return self.fd(i);
            }
            filled = filled.max(end);
            if filled > gid {
                break;
            }
        }
        0
    }
}

/// True when no value is smaller than the one before it.
fn ascends(mut firsts: impl Iterator<Item = usize>) -> bool {
    let mut prev = 0;
    firsts.all(|first| {
        let ok = prev <= first;
        prev = first;
        ok
    })
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
pub(crate) fn fill_fd_ranges(
    ranges: &[(usize, u16)],
    sentinel: usize,
    n_glyphs: usize,
) -> Vec<u16> {
    let mut out = alloc::vec![0u16; n_glyphs];
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
