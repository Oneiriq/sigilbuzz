//! Bounds-checked readers for the GDEF rewriters.
//!
//! Every position is a byte offset from the start of the GDEF table,
//! and every failure reports that offset so a malformed font can be
//! bisected.

use alloc::vec::Vec;

use sigilbuzz::Error;

/// Reads the big-endian u16 at `pos`.
pub(super) fn u16_at(table: &[u8], pos: usize, context: &'static str) -> Result<u16, Error> {
    pos.checked_add(2)
        .and_then(|end| table.get(pos..end))
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or(Error::Truncated {
            offset: pos,
            context,
        })
}

/// Reads the big-endian u32 at `pos`.
pub(super) fn u32_at(table: &[u8], pos: usize, context: &'static str) -> Result<u32, Error> {
    pos.checked_add(4)
        .and_then(|end| table.get(pos..end))
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(Error::Truncated {
            offset: pos,
            context,
        })
}

/// Returns `table[pos..pos + len]`, or a truncation error at `pos`.
pub(super) fn slice_at<'a>(
    table: &'a [u8],
    pos: usize,
    len: usize,
    context: &'static str,
) -> Result<&'a [u8], Error> {
    pos.checked_add(len)
        .and_then(|end| table.get(pos..end))
        .ok_or(Error::Truncated {
            offset: pos,
            context,
        })
}

/// Lists the `(glyph, class)` pairs of the ClassDef table at `off`,
/// leaving out glyphs of class 0.
///
/// ```text
///   format 1: u16 format, u16 startGlyphID, u16 glyphCount,
///             u16 classValueArray[glyphCount]
///   format 2: u16 format, u16 classRangeCount,
///             ClassRangeRecord[classRangeCount]: u16 start, u16 end, u16 class
/// ```
pub(super) fn class_def(table: &[u8], off: usize) -> Result<Vec<(u16, u16)>, Error> {
    const CTX: &str = "GDEF ClassDef table truncated";
    let mut out = Vec::new();
    match u16_at(table, off, CTX)? {
        1 => {
            let start = u16_at(table, off + 2, CTX)?;
            let count = u16_at(table, off + 4, CTX)?;
            if u32::from(start) + u32::from(count) > 0x1_0000 {
                return Err(Error::Malformed {
                    offset: off + 4,
                    context: "GDEF ClassDef format 1 runs past glyph 65535",
                });
            }
            for i in 0..count {
                let class = u16_at(table, off + 6 + usize::from(i) * 2, CTX)?;
                if class != 0 {
                    out.push((start + i, class));
                }
            }
        }
        2 => {
            let count = usize::from(u16_at(table, off + 2, CTX)?);
            for i in 0..count {
                let rec = off + 4 + i * 6;
                let start = u16_at(table, rec, CTX)?;
                let end = u16_at(table, rec + 2, CTX)?;
                let class = u16_at(table, rec + 4, CTX)?;
                // A range that ends before it starts names no glyph.
                if class != 0 {
                    out.extend((start..=end).map(|gid| (gid, class)));
                }
            }
        }
        _ => {
            return Err(Error::Malformed {
                offset: off,
                context: "unsupported GDEF ClassDef format",
            })
        }
    }
    Ok(out)
}

/// Lists the `(glyph, coverage index)` pairs of the Coverage table at
/// `off`, in table order.
///
/// ```text
///   format 1: u16 format, u16 glyphCount, u16 glyphArray[glyphCount]
///   format 2: u16 format, u16 rangeCount,
///             RangeRecord[rangeCount]: u16 start, u16 end, u16 startCoverageIndex
/// ```
pub(super) fn coverage(table: &[u8], off: usize) -> Result<Vec<(u16, u16)>, Error> {
    const CTX: &str = "GDEF Coverage table truncated";
    let format = u16_at(table, off, CTX)?;
    let count = usize::from(u16_at(table, off + 2, CTX)?);
    let mut out = Vec::new();
    match format {
        1 => {
            for i in 0..count {
                out.push((u16_at(table, off + 4 + i * 2, CTX)?, i as u16));
            }
        }
        2 => {
            for i in 0..count {
                let rec = off + 4 + i * 6;
                let start = u16_at(table, rec, CTX)?;
                let end = u16_at(table, rec + 2, CTX)?;
                let first_index = u16_at(table, rec + 4, CTX)?;
                if end < start {
                    return Err(Error::Malformed {
                        offset: rec,
                        context: "GDEF Coverage range ends before it starts",
                    });
                }
                for (k, gid) in (start..=end).enumerate() {
                    out.push((gid, first_index.wrapping_add(k as u16)));
                }
            }
        }
        _ => {
            return Err(Error::Malformed {
                offset: off,
                context: "unsupported GDEF Coverage format",
            })
        }
    }
    Ok(out)
}
