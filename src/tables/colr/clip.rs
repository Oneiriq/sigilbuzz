//! `COLR` v1 ClipList: per-glyph clip boxes.
//!
//! ```text
//!   ClipList       { u8 format; u32 numClips; Clip clips[numClips] }
//!   Clip           { u16 startGlyphID; u16 endGlyphID;
//!                    Offset24 clipBoxOffset }   // from the ClipList start
//!   ClipBoxFormat1 { u8 format = 1; FWORD xMin, yMin, xMax, yMax }
//!   ClipBoxFormat2 { u8 format = 2; FWORD xMin, yMin, xMax, yMax;
//!                    u32 varIndexBase }
//! ```
//!
//! Clip records are sorted by glyph range, so a lookup is a binary
//! search. A renderer clips the whole color glyph to its box; HarfBuzz
//! does this in `hb_font_paint_glyph` and around every `PaintColrGlyph`.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// Size of one `Clip` record: two glyph ids and an Offset24.
const CLIP_RECORD_LEN: usize = 7;

/// A parsed `ClipList`, borrowing the COLR table bytes.
#[derive(Debug, Clone, Copy)]
pub struct ClipList<'a> {
    /// The whole COLR table; clip box offsets resolve against `start`.
    data: &'a [u8],
    /// Absolute offset of the ClipList inside `data`.
    start: usize,
    /// Number of `Clip` records.
    count: u32,
}

/// One glyph's clip box in design units.
///
/// `var_index_base` is `Some` for the variable `ClipBoxFormat2`: the
/// four coordinates then take deltas `varIndexBase + 0` through
/// `varIndexBase + 3` from the COLR variation data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipBox {
    /// Left edge.
    pub x_min: i16,
    /// Bottom edge.
    pub y_min: i16,
    /// Right edge.
    pub x_max: i16,
    /// Top edge.
    pub y_max: i16,
    /// Variation index base of a `ClipBoxFormat2`, else `None`.
    pub var_index_base: Option<u32>,
}

impl<'a> ClipList<'a> {
    /// Parses the ClipList at absolute offset `start` of the COLR table
    /// `data`. The record array must fit in the table; the clip boxes
    /// themselves are read lazily by [`ClipList::get`].
    pub(crate) fn parse(data: &'a [u8], start: usize) -> Result<Self> {
        let mut r = Reader::at(data, start)?;
        let _format = r.read_u8()?;
        let count = r.read_u32()?;
        let records = (count as usize)
            .checked_mul(CLIP_RECORD_LEN)
            .ok_or(Error::Malformed {
                offset: start + 1,
                context: "COLR ClipList count overflow",
            })?;
        if r.remaining() < records {
            return Err(Error::Truncated {
                offset: r.position(),
                context: "COLR ClipList records truncated",
            });
        }
        Ok(Self { data, start, count })
    }

    /// Number of clip records (glyph ranges).
    #[must_use]
    pub const fn len(&self) -> u32 {
        self.count
    }

    /// True when the list has no records.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The clip box covering `glyph_id`, or `None` when no record's
    /// glyph range contains it or the record's box is out of range or
    /// of an unknown format.
    #[must_use]
    pub fn get(&self, glyph_id: u16) -> Option<ClipBox> {
        let records = self.start + 5;
        let (mut lo, mut hi) = (0usize, self.count as usize);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let rec = records + mid * CLIP_RECORD_LEN;
            let mut r = Reader::at(self.data, rec).ok()?;
            let first = r.read_u16().ok()?;
            let last = r.read_u16().ok()?;
            if glyph_id < first {
                hi = mid;
            } else if glyph_id > last {
                lo = mid + 1;
            } else {
                let rel = r.read_bytes(3).ok()?;
                let rel = u32::from_be_bytes([0, rel[0], rel[1], rel[2]]) as usize;
                return read_clip_box(self.data, self.start.checked_add(rel)?);
            }
        }
        None
    }
}

/// Reads a `ClipBoxFormat1` or `ClipBoxFormat2` at absolute `offset`.
fn read_clip_box(data: &[u8], offset: usize) -> Option<ClipBox> {
    let mut r = Reader::at(data, offset).ok()?;
    let format = r.read_u8().ok()?;
    if format != 1 && format != 2 {
        return None;
    }
    let x_min = r.read_i16().ok()?;
    let y_min = r.read_i16().ok()?;
    let x_max = r.read_i16().ok()?;
    let y_max = r.read_i16().ok()?;
    let var_index_base = if format == 2 {
        Some(r.read_u32().ok()?)
    } else {
        None
    };
    Some(ClipBox {
        x_min,
        y_min,
        x_max,
        y_max,
        var_index_base,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// A ClipList at offset `pad` of the returned bytes, with one
    /// record per `(first, last, box)`; a box is `(format, coords)`.
    fn clip_list(pad: usize, records: &[(u16, u16, u8, [i16; 4])]) -> Vec<u8> {
        let mut out = alloc::vec![0xEE; pad];
        out.push(1);
        out.extend_from_slice(&(records.len() as u32).to_be_bytes());
        let mut box_at = 5 + CLIP_RECORD_LEN * records.len();
        for (first, last, format, _) in records {
            out.extend_from_slice(&first.to_be_bytes());
            out.extend_from_slice(&last.to_be_bytes());
            let rel = box_at as u32;
            out.extend_from_slice(&rel.to_be_bytes()[1..]);
            box_at += if *format == 2 { 13 } else { 9 };
        }
        for (_, _, format, coords) in records {
            out.push(*format);
            for v in coords {
                out.extend_from_slice(&v.to_be_bytes());
            }
            if *format == 2 {
                out.extend_from_slice(&7u32.to_be_bytes());
            }
        }
        out
    }

    #[test]
    fn looks_up_glyph_ranges_by_binary_search() {
        let bytes = clip_list(
            3,
            &[
                (2, 4, 1, [0, 0, 100, 100]),
                (10, 10, 2, [-5, -6, 50, 60]),
                (20, 30, 1, [1, 2, 3, 4]),
            ],
        );
        let list = ClipList::parse(&bytes, 3).expect("parses");
        assert_eq!(list.len(), 3);
        assert!(!list.is_empty());
        let fixed = ClipBox {
            x_min: 0,
            y_min: 0,
            x_max: 100,
            y_max: 100,
            var_index_base: None,
        };
        assert_eq!(list.get(2), Some(fixed));
        assert_eq!(list.get(4), Some(fixed));
        assert_eq!(
            list.get(10),
            Some(ClipBox {
                x_min: -5,
                y_min: -6,
                x_max: 50,
                y_max: 60,
                var_index_base: Some(7),
            })
        );
        assert_eq!(list.get(25).map(|b| b.x_max), Some(3));
        for missing in [0, 1, 5, 9, 11, 19, 31, u16::MAX] {
            assert_eq!(list.get(missing), None, "gid {missing}");
        }
    }

    #[test]
    fn empty_list_has_no_boxes() {
        let bytes = clip_list(0, &[]);
        let list = ClipList::parse(&bytes, 0).expect("parses");
        assert!(list.is_empty());
        assert_eq!(list.get(0), None);
    }

    #[test]
    fn truncated_records_are_rejected_with_their_offset() {
        let mut bytes = clip_list(0, &[(1, 1, 1, [0, 0, 1, 1])]);
        bytes.truncate(5 + CLIP_RECORD_LEN - 1);
        assert!(matches!(
            ClipList::parse(&bytes, 0),
            Err(Error::Truncated { offset: 5, .. })
        ));
        // A header cut short is truncated too.
        assert!(matches!(
            ClipList::parse(&bytes[..3], 0),
            Err(Error::Truncated { .. })
        ));
        assert!(ClipList::parse(&bytes, 99).is_err());
    }

    #[test]
    fn bad_boxes_resolve_to_none() {
        // Box offset past the end, then an unknown box format.
        let mut bytes = clip_list(0, &[(1, 1, 1, [0, 0, 1, 1]), (2, 2, 1, [0, 0, 1, 1])]);
        bytes[5 + 4..5 + 7].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
        let second_box = 5 + 2 * CLIP_RECORD_LEN + 9;
        bytes[second_box] = 3;
        let list = ClipList::parse(&bytes, 0).expect("record array fits");
        assert_eq!(list.get(1), None);
        assert_eq!(list.get(2), None);
    }
}
