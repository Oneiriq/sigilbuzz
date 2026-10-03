//! The Standard Encoding and charsets: how a `seac` finds the glyphs
//! of its base and accent characters.
//!
//! A `seac` names each character by its Standard Encoding code. The
//! encoding turns the code into a SID, and the font's charset gives the
//! glyph that has that SID.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// The SID of each Standard Encoding code, with 0 for the codes the
/// encoding leaves undefined. Adobe Technical Note #5176, Appendix B.
#[rustfmt::skip]
const STANDARD_ENCODING: [u8; 256] = [
    // 0-31: undefined.
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // 32-126: space through asciitilde, SIDs 1 to 95 in order.
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
    33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48,
    49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64,
    65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80,
    81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 0,
    // 128-159: undefined.
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // 160-255: exclamdown through germandbls, with gaps.
    0, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110,
    0, 111, 112, 113, 114, 0, 115, 116, 117, 118, 119, 120, 121, 122, 0, 123,
    0, 124, 125, 126, 127, 128, 129, 130, 131, 0, 132, 133, 0, 134, 135, 136,
    137, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 138, 0, 139, 0, 0, 0, 0, 140, 141, 142, 143, 0, 0, 0, 0,
    0, 144, 0, 0, 0, 145, 0, 0, 146, 147, 148, 149, 0, 0, 0, 0,
];

/// The SID that the Standard Encoding gives charstring operand `code`.
/// `None` when the operand is not a code from 0 to 255, or the encoding
/// leaves that code undefined. A fractional code is truncated, as
/// HarfBuzz and FreeType do.
pub(super) fn standard_encoding_sid(code: f64) -> Option<u16> {
    if !(0.0..256.0).contains(&code) {
        return None;
    }
    let sid = *STANDARD_ENCODING.get(code as usize)?;
    (sid != 0).then_some(u16::from(sid))
}

/// Top DICT `charset` values below 3 name a predefined charset instead
/// of an offset.
const ISO_ADOBE: u32 = 0;
const EXPERT: u32 = 1;
const EXPERT_SUBSET: u32 = 2;

/// Highest SID in the ISOAdobe charset (`zcaron`).
const ISO_ADOBE_LAST_SID: u16 = 228;

/// The glyph that the charset `charset` (the Top DICT value, default 0)
/// of a table with `n_glyphs` glyphs gives `sid`, which is not 0.
///
/// Fails with [`Error::Malformed`] when no glyph has that SID, and with
/// [`Error::Unsupported`] for the predefined Expert charsets, which no
/// `seac` component is expected to come from. HarfBuzz rejects those
/// too.
pub(super) fn glyph_for_sid(data: &[u8], charset: u32, sid: u16, n_glyphs: usize) -> Result<usize> {
    let found = match charset {
        // ISOAdobe: glyph i has SID i.
        ISO_ADOBE => (sid <= ISO_ADOBE_LAST_SID).then_some(usize::from(sid)),
        EXPERT | EXPERT_SUBSET => {
            return Err(Error::Unsupported {
                context: "CFF seac in a font with an Expert charset",
            })
        }
        off => custom_glyph_for_sid(data, off as usize, sid, n_glyphs)?,
    };
    found.filter(|&gid| gid < n_glyphs).ok_or(Error::Malformed {
        offset: if charset > EXPERT_SUBSET {
            charset as usize
        } else {
            0
        },
        context: "CFF seac glyph not in charset",
    })
}

/// Looks `sid` up in the charset table at `off`. Glyph 0 is `.notdef`,
/// which the table leaves out, so its entries start at glyph 1.
fn custom_glyph_for_sid(
    data: &[u8],
    off: usize,
    sid: u16,
    n_glyphs: usize,
) -> Result<Option<usize>> {
    let mut r = Reader::at(data, off)?;
    let format = r.read_u8()?;
    let mut gid = 1;
    match format {
        // One SID per glyph.
        0 => {
            while gid < n_glyphs {
                if r.read_u16()? == sid {
                    return Ok(Some(gid));
                }
                gid += 1;
            }
        }
        // Ranges of consecutive SIDs: a first SID, then a count of the
        // SIDs that follow it, a u8 in format 1 and a u16 in format 2.
        1 | 2 => {
            while gid < n_glyphs {
                let first = r.read_u16()?;
                let n_left = if format == 1 {
                    usize::from(r.read_u8()?)
                } else {
                    usize::from(r.read_u16()?)
                };
                if let Some(k) = sid.checked_sub(first).map(usize::from) {
                    if k <= n_left {
                        return Ok(Some(gid + k));
                    }
                }
                gid += n_left + 1;
            }
        }
        _ => {
            return Err(Error::Malformed {
                offset: off,
                context: "CFF charset format unknown",
            })
        }
    }
    Ok(None)
}
