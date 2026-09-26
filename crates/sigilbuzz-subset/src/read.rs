//! Bounds-checked big-endian readers for the table rewriters.
//!
//! Every position is a byte offset from the start of the table being
//! read, and every failure reports a position, so a malformed font can
//! be bisected. Sums and products of offsets and counts are checked:
//! on a 32-bit target such as wasm32 an Offset32 plus a position, or a
//! count times a record size, can wrap a `usize`. A computation that
//! overflows fails exactly like one that runs past the table, so every
//! target reports the same error.

use sigilbuzz::Error;

/// Reads the big-endian u16 at `pos`.
pub(crate) fn u16_at(table: &[u8], pos: usize, context: &'static str) -> Result<u16, Error> {
    slice_at(table, pos, 2, context).map(|b| u16::from_be_bytes([b[0], b[1]]))
}

/// Reads the big-endian u32 at `pos`.
pub(crate) fn u32_at(table: &[u8], pos: usize, context: &'static str) -> Result<u32, Error> {
    slice_at(table, pos, 4, context).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// Returns `table[pos..pos + len]`, or a truncation error at `pos`.
pub(crate) fn slice_at<'a>(
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

/// Returns the `count` records of `size` bytes starting at `pos`, or a
/// truncation error at `pos`, also when `count * size` overflows.
pub(crate) fn array_at<'a>(
    table: &'a [u8],
    pos: usize,
    count: usize,
    size: usize,
    context: &'static str,
) -> Result<&'a [u8], Error> {
    let len = count.checked_mul(size).ok_or(Error::Truncated {
        offset: pos,
        context,
    })?;
    slice_at(table, pos, len, context)
}

/// Resolves the offset stored in the Offset32 at `slot`, measured from
/// `base`, to a position inside `table`. A target past the table (or
/// one that overflows the sum) is reported at `slot`, where the bad
/// offset sits.
pub(crate) fn offset32_at(
    table: &[u8],
    slot: usize,
    base: usize,
    context: &'static str,
) -> Result<usize, Error> {
    let rel = u32_at(table, slot, context)?;
    usize::try_from(rel)
        .ok()
        .and_then(|rel| base.checked_add(rel))
        .filter(|&at| at <= table.len())
        .ok_or(Error::Malformed {
            offset: slot,
            context,
        })
}

#[cfg(test)]
mod tests {
    use super::{array_at, offset32_at, u16_at, u32_at};
    use sigilbuzz::Error;

    #[test]
    fn reads_report_where_they_ran_out() {
        let table = [0u8, 1, 0, 0, 0, 9];
        assert_eq!(u16_at(&table, 0, "x"), Ok(1));
        assert_eq!(u32_at(&table, 2, "x"), Ok(9));
        assert_eq!(
            u16_at(&table, 5, "x"),
            Err(Error::Truncated {
                offset: 5,
                context: "x"
            })
        );
        assert!(u16_at(&table, usize::MAX, "x").is_err(), "no wrap");
    }

    #[test]
    fn overflowing_arrays_fail_like_short_ones() {
        let table = [0u8; 8];
        let short = array_at(&table, 2, 4, 2, "x");
        let huge = array_at(&table, 2, usize::MAX, 2, "x");
        assert_eq!(short, huge);
        assert_eq!(array_at(&table, 2, 3, 2, "x"), Ok(&table[2..8]));
    }

    #[test]
    fn offset32s_past_the_table_are_reported_at_their_slot() {
        let mut table = [0u8; 8];
        table[0..4].copy_from_slice(&8u32.to_be_bytes());
        assert_eq!(offset32_at(&table, 0, 0, "x"), Ok(8), "the end is in reach");
        table[0..4].copy_from_slice(&9u32.to_be_bytes());
        let past = offset32_at(&table, 0, 0, "x");
        table[0..4].copy_from_slice(&u32::MAX.to_be_bytes());
        let far = offset32_at(&table, 0, 4, "x");
        let expected = Err(Error::Malformed {
            offset: 0,
            context: "x",
        });
        assert_eq!((past, far), (expected.clone(), expected));
    }
}
