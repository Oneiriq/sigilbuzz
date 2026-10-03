//! CFF INDEX structures: count-prefixed arrays of variable-length
//! byte slices, plus the bounds-checked slicing helpers they use.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

// ----------------------------------------------------------------------------
// CFF INDEX structure.
// ----------------------------------------------------------------------------

/// A lazy view of a CFF or CFF2 INDEX.
///
/// Reading the INDEX checks only its header: the count, the offset
/// size, that the offset array fits, and that the last offset is at
/// least 1 and the data it implies fits. Entries are sliced on demand
/// by [`Index::get`], which checks that entry's two offsets. Opening
/// an INDEX is therefore O(1) however many entries it holds, and a
/// malformed entry fails only the lookups that touch it.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Index<'a> {
    /// The offset array: `count + 1` big-endian offsets of `off_size`
    /// bytes each.
    offsets: &'a [u8],
    /// The object data region, `last offset - 1` bytes long.
    objects: &'a [u8],
    /// Number of entries.
    count: u32,
    /// Bytes per offset, 1 to 4.
    off_size: u8,
    /// Absolute position of `objects` in the table, for error offsets.
    data_start: usize,
}

impl<'a> Index<'a> {
    /// Number of entries.
    pub(crate) fn len(&self) -> usize {
        self.count as usize
    }

    /// True when the INDEX has no entries.
    pub(crate) fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns entry `i`.
    ///
    /// Fails with [`Error::Malformed`] when `i` is out of range, when
    /// the entry's offsets are zero or descend, or when the entry ends
    /// past the data region. The first two report the position of
    /// offset slot `i`, where entry `i`'s offsets start.
    pub(crate) fn get(&self, i: usize) -> Result<&'a [u8]> {
        if i >= self.len() {
            return Err(Error::Malformed {
                offset: self.slot_position(i),
                context: "CFF INDEX entry out of range",
            });
        }
        let non_monotone = || Error::Malformed {
            offset: self.slot_position(i),
            context: "CFF INDEX offsets non-monotone",
        };
        let a = self.offset(i).ok_or_else(non_monotone)?;
        let b = self.offset(i + 1).ok_or_else(non_monotone)?;
        if a == 0 || b < a {
            return Err(non_monotone());
        }
        let start = a - 1;
        let end = b - 1;
        self.objects.get(start..end).ok_or(Error::Malformed {
            offset: self.data_start.saturating_add(end),
            context: "CFF INDEX entry past end",
        })
    }

    /// Absolute position of offset slot `k` in the table. The offset
    /// array ends where the data region starts. Saturates for a `k` far
    /// past the array.
    fn slot_position(&self, k: usize) -> usize {
        let offsets_start = self.data_start.saturating_sub(self.offsets.len());
        offsets_start.saturating_add(k.saturating_mul(usize::from(self.off_size)))
    }

    /// Reads offset `k` (0-based, up to `count`). `None` only if the
    /// offset array is shorter than the header promised, which the
    /// constructor rules out.
    fn offset(&self, k: usize) -> Option<usize> {
        let size = usize::from(self.off_size);
        let at = k.checked_mul(size)?;
        let bytes = self.offsets.get(at..at.checked_add(size)?)?;
        Some(bytes.iter().fold(0usize, |v, &b| (v << 8) | usize::from(b)))
    }
}

/// Reads a CFF1 INDEX starting at the reader's current position and
/// advances the cursor past it.
///
/// CFF1 (the `CFF ` table) uses a `Card16` (u16) count prefix. The
/// CFF2 INDEX is layout-compatible except the count is a u32; CFF2
/// callers go through [`read_index2`].
pub(crate) fn read_index<'a>(r: &mut Reader<'a>) -> Result<Index<'a>> {
    let count = u32::from(r.read_u16()?);
    read_index_body(r, count)
}

/// Reads a CFF2 INDEX. Identical to [`read_index`] but with a u32
/// count prefix per the OpenType 1.8 CFF2 spec.
pub(crate) fn read_index2<'a>(r: &mut Reader<'a>) -> Result<Index<'a>> {
    let count = r.read_u32()?;
    read_index_body(r, count)
}

fn read_index_body<'a>(r: &mut Reader<'a>, count: u32) -> Result<Index<'a>> {
    if count == 0 {
        return Ok(Index {
            data_start: r.position(),
            ..Index::default()
        });
    }
    let off_size = r.read_u8()?;
    if !(1..=4).contains(&off_size) {
        return Err(Error::Malformed {
            offset: r.position(),
            context: "CFF INDEX offSize out of range",
        });
    }
    // The offset array alone takes (count + 1) * offSize bytes. Check
    // that before slicing, so a huge count in a short table fails
    // here instead of at some later lookup.
    let offsets_len = (count as usize)
        .saturating_add(1)
        .saturating_mul(usize::from(off_size));
    if offsets_len > r.remaining() {
        return Err(Error::Truncated {
            offset: r.position(),
            context: "CFF INDEX offset array",
        });
    }
    let offsets = r.read_bytes(offsets_len)?;
    // The data region begins after the final offset field. CFF
    // offsets are 1-based, so the last offset minus one is the data
    // length.
    let data_start = r.position();
    let mut index = Index {
        offsets,
        objects: &[],
        count,
        off_size,
        data_start,
    };
    let data_len = index
        .offset(count as usize)
        .and_then(|last| last.checked_sub(1))
        .ok_or(Error::Malformed {
            offset: data_start,
            context: "CFF INDEX offsets non-monotone",
        })?;
    if data_len > r.remaining() {
        return Err(Error::Truncated {
            offset: data_start.saturating_add(data_len),
            context: "CFF INDEX entry past end of data",
        });
    }
    index.objects = r.read_bytes(data_len)?;
    Ok(index)
}

/// Encodes a CFF1 INDEX (u16 count) holding `entries`, with
/// `off_size`-byte offsets. Test fixtures build subroutine and
/// CharStrings INDEX structures with it.
#[cfg(test)]
pub(crate) fn encode_index(entries: &[&[u8]], off_size: u8) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec::Vec::new();
    out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    if entries.is_empty() {
        return out;
    }
    out.push(off_size);
    let push = |out: &mut alloc::vec::Vec<u8>, v: u32| {
        let bytes = v.to_be_bytes();
        out.extend_from_slice(&bytes[4 - usize::from(off_size)..]);
    };
    let mut offset = 1u32;
    push(&mut out, offset);
    for e in entries {
        offset += e.len() as u32;
        push(&mut out, offset);
    }
    for e in entries {
        out.extend_from_slice(e);
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(entries: &[&[u8]], off_size: u8) -> alloc::vec::Vec<u8> {
        encode_index(entries, off_size)
    }

    fn parse(bytes: &[u8]) -> Result<Index<'_>> {
        read_index(&mut Reader::new(bytes))
    }

    #[test]
    fn entries_slice_lazily() {
        for off_size in 1..=4 {
            let bytes = encode(&[b"ab", b"", b"cde"], off_size);
            let index = parse(&bytes).unwrap();
            assert_eq!(index.len(), 3);
            assert_eq!(index.get(0).unwrap(), b"ab");
            assert_eq!(index.get(1).unwrap(), b"");
            assert_eq!(index.get(2).unwrap(), b"cde");
        }
    }

    #[test]
    fn reader_ends_after_the_data() {
        let mut bytes = encode(&[b"ab", b"c"], 2);
        let end = bytes.len();
        bytes.extend_from_slice(&[0xAA, 0xBB]);
        let mut r = Reader::new(&bytes);
        read_index(&mut r).unwrap();
        assert_eq!(r.position(), end);
    }

    #[test]
    fn cff2_index_has_a_u32_count() {
        let mut bytes = 2u32.to_be_bytes().to_vec();
        bytes.extend_from_slice(&[1, 1, 2, 4, b'x', b'y', b'z']);
        let index = read_index2(&mut Reader::new(&bytes)).unwrap();
        assert_eq!(index.len(), 2);
        assert_eq!(index.get(0).unwrap(), b"x");
        assert_eq!(index.get(1).unwrap(), b"yz");
    }

    #[test]
    fn empty_index_is_two_bytes() {
        let bytes = [0, 0, 0xFF];
        let mut r = Reader::new(&bytes);
        let index = read_index(&mut r).unwrap();
        assert!(index.is_empty());
        assert_eq!(r.position(), 2);
        assert!(matches!(index.get(0), Err(Error::Malformed { .. })));
    }

    #[test]
    fn get_past_the_last_entry_is_malformed() {
        let bytes = encode(&[b"a"], 1);
        let index = parse(&bytes).unwrap();
        assert!(matches!(index.get(1), Err(Error::Malformed { .. })));
        assert!(matches!(
            index.get(usize::MAX),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn off_size_out_of_range_is_malformed() {
        for off_size in [0, 5] {
            let bytes = [0, 1, off_size, 1, 1];
            assert!(matches!(parse(&bytes), Err(Error::Malformed { .. })));
        }
    }

    #[test]
    fn truncated_offset_array_is_truncated() {
        // Count 3 needs four offsets; only two are present.
        let bytes = [0, 3, 1, 1, 2];
        assert!(matches!(parse(&bytes), Err(Error::Truncated { .. })));
    }

    #[test]
    fn truncated_data_is_truncated() {
        // The last offset promises 4 data bytes; only 2 follow.
        let bytes = [0, 1, 1, 1, 5, b'a', b'b'];
        assert!(matches!(parse(&bytes), Err(Error::Truncated { .. })));
    }

    #[test]
    fn zero_last_offset_is_malformed() {
        let bytes = [0, 2, 1, 1, 1, 0];
        assert!(matches!(parse(&bytes), Err(Error::Malformed { .. })));
    }

    #[test]
    fn non_monotone_entry_fails_only_that_entry() {
        // Offsets [1, 3, 2, 4]: entry 1 runs backward. Entries 0 and 2
        // are still readable.
        let bytes = [0, 3, 1, 1, 3, 2, 4, b'a', b'b', b'c'];
        let index = parse(&bytes).unwrap();
        assert_eq!(index.get(0).unwrap(), b"ab");
        assert!(matches!(index.get(1), Err(Error::Malformed { .. })));
        assert_eq!(index.get(2).unwrap(), b"bc");
    }

    #[test]
    fn zero_first_offset_is_malformed() {
        let bytes = [0, 1, 1, 0, 2, b'a'];
        let index = parse(&bytes).unwrap();
        assert!(matches!(index.get(0), Err(Error::Malformed { .. })));
    }

    #[test]
    fn entry_past_the_data_region_is_malformed() {
        // Offsets [1, 9, 3]: entry 0 ends past the 2-byte data region,
        // entry 1 runs backward.
        let bytes = [0, 2, 1, 1, 9, 3, b'a', b'b'];
        let index = parse(&bytes).unwrap();
        assert!(matches!(index.get(0), Err(Error::Malformed { .. })));
        assert!(matches!(index.get(1), Err(Error::Malformed { .. })));
    }

    /// The byte offset a failed [`Index::get`] reports.
    fn get_error_offset(index: &Index<'_>, i: usize) -> usize {
        match index.get(i) {
            Err(Error::Malformed { offset, .. }) => offset,
            other => panic!("entry {i}: expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn get_errors_report_the_offset_slot() {
        // Seven unrelated bytes, then an INDEX with count 3, offSize 2,
        // offsets [1, 3, 2, 4], and three data bytes. Offset slot k sits
        // at 7 + 3 + 2k in the table.
        let mut table = alloc::vec![0xEE; 7];
        table.extend_from_slice(&[0, 3, 2, 0, 1, 0, 3, 0, 2, 0, 4, b'a', b'b', b'c']);
        let index = read_index(&mut Reader::at(&table, 7).unwrap()).unwrap();
        // Entry 1 runs backward, from offset 3 to offset 2.
        assert_eq!(get_error_offset(&index, 1), 7 + 3 + 2);
        // An entry past the end reports the slot it would start at.
        assert_eq!(get_error_offset(&index, 3), 7 + 3 + 6);
        assert_eq!(get_error_offset(&index, usize::MAX), usize::MAX);

        // A zero first offset, in an INDEX with offSize 1 four bytes in.
        let mut table = alloc::vec![0xEE; 4];
        table.extend_from_slice(&[0, 1, 1, 0, 2, b'a']);
        let index = read_index(&mut Reader::at(&table, 4).unwrap()).unwrap();
        assert_eq!(get_error_offset(&index, 0), 4 + 3);
    }

    #[test]
    fn huge_count_fails_before_reading_offsets() {
        let mut bytes = u32::MAX.to_be_bytes().to_vec();
        bytes.push(4);
        let err = read_index2(&mut Reader::new(&bytes)).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }), "{err:?}");
    }
}
