//! Device and VariationIndex tables for the layout rewriters.
//!
//! Several OpenType records reach a `Device` table (per-ppem hinting
//! deltas) or a `VariationIndex` table (an `(outer, inner)` row in the
//! GDEF `ItemVariationStore`) through an Offset16. The offset is
//! measured from the record's parent table, and the parent depends on
//! the record:
//!
//! - ValueRecord in SinglePos or the PairPos format 2 class matrix:
//!   the subtable.
//! - ValueRecord in a PairPos format 1 `PairValueRecord`: the `PairSet`.
//! - AnchorFormat3 `xDeviceOffset` / `yDeviceOffset`: the `Anchor`.
//! - CaretValueFormat3 `deviceOffset`: the `CaretValue`.
//!
//! The subsetter rebuilds every one of those parents, so a device
//! offset copied verbatim would point into unrelated bytes. The helpers
//! here copy the referenced tables along with their parent and re-point
//! the offsets at the copies. A slot whose table is missing or
//! malformed is cleared instead: the spec asks consumers to ignore an
//! unusable Device table, and a cleared slot cannot be misread. A
//! malformed table, or a malformed anchor left out, is reported through
//! the rewriter's [`Diag`].

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use sigilbuzz::Error;

use crate::warnings::Diag;

/// `deltaFormat` that marks a `VariationIndex` table.
pub(crate) const VARIATION_INDEX_FORMAT: u16 = 0x8000;

fn read_u16(buf: &[u8], pos: usize) -> Option<u16> {
    let bytes = buf.get(pos..pos.checked_add(2)?)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// Writes `value` at `pos`. A slot past the end of `buf` is left alone:
/// callers only write slots they have just read.
fn write_u16(buf: &mut [u8], pos: usize, value: u16) {
    if let Some(slot) = pos.checked_add(2).and_then(|end| buf.get_mut(pos..end)) {
        slot.copy_from_slice(&value.to_be_bytes());
    }
}

/// The exact bytes of the `Device` or `VariationIndex` table at
/// `offset` inside `parent`, or `None` where [`read_device`] finds none
/// or an unusable one. The tests read the tables they check with it.
#[cfg(test)]
pub(crate) fn device_table(parent: &[u8], offset: usize) -> Option<&[u8]> {
    read_device(parent, offset).ok().flatten()
}

/// Returns the exact bytes of the `Device` or `VariationIndex` table at
/// `offset` inside `parent`.
///
/// Layout (all three share the 6-byte header):
///
/// ```text
///   Device:          u16 startSize, u16 endSize, u16 deltaFormat (1..=3),
///                    u16 deltaValues[ceil((end - start + 1) * bits / 16)]
///   VariationIndex:  u16 outer, u16 inner, u16 deltaFormat = 0x8000
/// ```
///
/// `bits` is 2, 4, or 8 for delta formats 1, 2, and 3. Returns
/// `Ok(None)` for a null offset, and an error measured from the start
/// of `parent` for an offset past the end, a truncated table, an
/// inverted ppem range, or a delta format the spec does not define.
pub(crate) fn read_device(parent: &[u8], offset: usize) -> Result<Option<&[u8]>, Error> {
    const TRUNCATED: &str = "Device or VariationIndex table truncated";
    if offset == 0 {
        return Ok(None);
    }
    let Some(table) = parent.get(offset..) else {
        return Err(Error::Malformed {
            offset,
            context: "Device or VariationIndex offset past the end of its parent",
        });
    };
    let field = |pos: usize| {
        read_u16(table, pos).ok_or(Error::Truncated {
            offset: offset + pos,
            context: TRUNCATED,
        })
    };
    let (start, end, format) = (field(0)?, field(2)?, field(4)?);
    let len = match format {
        VARIATION_INDEX_FORMAT => 6,
        1..=3 => {
            if end < start {
                return Err(Error::Malformed {
                    offset: offset + 2,
                    context: "Device table endSize is below its startSize",
                });
            }
            let count = usize::from(end - start) + 1;
            let per_word = 8usize >> (format - 1);
            6 + count.div_ceil(per_word) * 2
        }
        _ => {
            return Err(Error::Malformed {
                offset: offset + 4,
                context: "unknown Device deltaFormat",
            })
        }
    };
    table.get(..len).map(Some).ok_or(Error::Truncated {
        offset: offset + 6,
        context: TRUNCATED,
    })
}

/// Places byte blobs into an output buffer, sharing one copy between
/// identical blobs. Positions are absolute indices into the buffer the
/// caller passes to [`Dedup::place`].
#[derive(Default)]
pub(crate) struct Dedup {
    placed: BTreeMap<Vec<u8>, usize>,
}

impl Dedup {
    /// Appends `blob` to `out` unless an identical blob was already
    /// placed through this pool, and returns the blob's position.
    pub(crate) fn place(&mut self, out: &mut Vec<u8>, blob: &[u8]) -> usize {
        if let Some(&pos) = self.placed.get(blob) {
            return pos;
        }
        let pos = out.len();
        out.extend_from_slice(blob);
        self.placed.insert(blob.to_vec(), pos);
        pos
    }
}

/// Re-points the Offset16 at `slot` in `out` at a copy of the device
/// table it names. The slot still holds the source offset, measured
/// from `src_parent`. The copy lands at the end of `out` (shared with
/// any identical table already placed through `pool`) and the slot is
/// rewritten relative to `out_base`. An unresolvable table clears the
/// slot (and is reported through `diag`), and so does a VariationIndex
/// when `keep_variations` is off: a static subset has no
/// ItemVariationStore for it to name, and leaving it out keeps it from
/// taking offset space. A copy whose offset would not fit in 16 bits
/// clears the slot too and returns `false`, so the caller can report
/// the overflow.
fn relocate_slot(
    out: &mut Vec<u8>,
    slot: usize,
    out_base: usize,
    src_parent: &[u8],
    pool: &mut Dedup,
    keep_variations: bool,
    diag: &Diag<'_>,
) -> bool {
    let Some(src_off) = read_u16(out, slot) else {
        return true;
    };
    let table = read_device(src_parent, usize::from(src_off))
        .unwrap_or_else(|e| {
            diag.part_error(src_parent, &e, "a Device or VariationIndex table");
            None
        })
        .filter(|t| keep_variations || read_u16(t, 4) != Some(VARIATION_INDEX_FORMAT));
    let (new_off, fits) = match table {
        Some(table) => match u16::try_from(pool.place(out, table).saturating_sub(out_base)) {
            Ok(rel) => (rel, true),
            Err(_) => (0, false),
        },
        None => (0, true),
    };
    write_u16(out, slot, new_off);
    fits
}

/// Copies the Anchor at `offset` inside `parent` into a standalone blob.
///
/// ```text
///   format 1: u16 format=1, i16 xCoord, i16 yCoord                (6 B)
///   format 2: u16 format=2, i16 xCoord, i16 yCoord, u16 anchorPt  (8 B)
///   format 3: u16 format=3, i16 xCoord, i16 yCoord,
///             o16 xDeviceOffset, o16 yDeviceOffset               (10 B)
/// ```
///
/// Formats 1 and 2 copy verbatim. Format 3 device offsets are measured
/// from the Anchor itself, so the referenced tables are copied right
/// after the 10-byte header and the offsets re-pointed at them. The
/// blob can then sit anywhere in the rebuilt subtable. Without
/// `keep_variations`, VariationIndex tables are left out and their
/// offsets cleared. Returns an empty Vec for a null offset or a
/// malformed anchor, which callers treat as "no anchor"; a malformed
/// anchor is reported through `diag`.
pub(crate) fn copy_anchor(
    parent: &[u8],
    offset: usize,
    keep_variations: bool,
    diag: &Diag<'_>,
) -> Vec<u8> {
    if offset == 0 {
        return Vec::new();
    }
    let Some(anchor) = parent.get(offset..) else {
        diag.in_part(parent, offset, "Anchor offset past the end", "an anchor");
        return Vec::new();
    };
    let len = match read_u16(anchor, 0) {
        Some(1) => 6,
        Some(2) => 8,
        Some(3) => 10,
        Some(_) => {
            diag.in_part(anchor, 0, "unknown Anchor format", "an anchor");
            return Vec::new();
        }
        None => 2,
    };
    let Some(header) = anchor.get(..len) else {
        diag.in_part(anchor, 0, "Anchor table truncated", "an anchor");
        return Vec::new();
    };
    let mut out = header.to_vec();
    if len == 10 {
        let mut pool = Dedup::default();
        for slot in [6, 8] {
            // Right behind the 10-byte header, the copies always fit.
            relocate_slot(&mut out, slot, 0, anchor, &mut pool, keep_variations, diag);
        }
    }
    out
}

/// Where the ValueRecords of one rebuilt parent table sit inside the
/// output buffer. The records are laid out as `count` groups spaced
/// `stride` bytes apart starting at `first`; each group holds one
/// ValueRecord per `(offset within the group, valueFormat)` entry.
pub(crate) struct RecordRun<'a> {
    /// Position of the first group in the output buffer.
    pub first: usize,
    /// Number of groups.
    pub count: usize,
    /// Bytes from one group to the next.
    pub stride: usize,
    /// The ValueRecords inside one group.
    pub records: &'a [(usize, u16)],
    /// Whether VariationIndex tables are copied. Off for a static
    /// subset, which clears their slots instead.
    pub keep_variations: bool,
}

/// Re-points the device slots of ValueRecords that were copied verbatim
/// into `out`.
///
/// The slots still hold offsets measured from `src_parent`, the parent
/// table the records came from (see the module docs). Every referenced
/// table is appended to `out`, identical tables sharing one copy, and
/// each slot is rewritten relative to `out_base`, the start of the
/// rebuilt parent inside `out`. Returns `false` when a copy landed out
/// of 16-bit reach of the parent (its slot is cleared). A table that
/// cannot be read is reported through `diag` and its slot cleared.
pub(crate) fn relocate_value_records(
    out: &mut Vec<u8>,
    out_base: usize,
    src_parent: &[u8],
    run: &RecordRun<'_>,
    diag: &Diag<'_>,
) -> bool {
    let mut pool = Dedup::default();
    let mut fits = true;
    for group in 0..run.count {
        for &(rel, format) in run.records {
            // Device slots follow the four static fields.
            let mut slot =
                run.first + group * run.stride + rel + 2 * (format & 0x000F).count_ones() as usize;
            for bit in [0x0010u16, 0x0020, 0x0040, 0x0080] {
                if format & bit != 0 {
                    fits &= relocate_slot(
                        out,
                        slot,
                        out_base,
                        src_parent,
                        &mut pool,
                        run.keep_variations,
                        diag,
                    );
                    slot += 2;
                }
            }
        }
    }
    fits
}

#[cfg(test)]
mod tests;
