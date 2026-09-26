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
//! unusable Device table, and a cleared slot cannot be misread.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// `deltaFormat` that marks a `VariationIndex` table.
pub(crate) const VARIATION_INDEX_FORMAT: u16 = 0x8000;

fn read_u16(buf: &[u8], pos: usize) -> Option<u16> {
    let bytes = buf.get(pos..pos.checked_add(2)?)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn write_u16(buf: &mut [u8], pos: usize, value: u16) {
    buf[pos..pos + 2].copy_from_slice(&value.to_be_bytes());
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
/// `bits` is 2, 4, or 8 for delta formats 1, 2, and 3. Returns `None`
/// for a null offset, an offset past the end, a truncated table, an
/// inverted ppem range, or a delta format the spec does not define.
pub(crate) fn device_table(parent: &[u8], offset: usize) -> Option<&[u8]> {
    if offset == 0 {
        return None;
    }
    let table = parent.get(offset..)?;
    let (start, end, format) = (
        read_u16(table, 0)?,
        read_u16(table, 2)?,
        read_u16(table, 4)?,
    );
    let len = match format {
        VARIATION_INDEX_FORMAT => 6,
        1..=3 => {
            if end < start {
                return None;
            }
            let count = usize::from(end - start) + 1;
            let per_word = 8usize >> (format - 1);
            6 + count.div_ceil(per_word) * 2
        }
        _ => return None,
    };
    table.get(..len)
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
/// slot, and so does a VariationIndex when `keep_variations` is off: a
/// static subset has no ItemVariationStore for it to name, and leaving
/// it out keeps it from taking offset space. A copy whose offset would
/// not fit in 16 bits clears the slot too and returns `false`, so the
/// caller can report the overflow.
fn relocate_slot(
    out: &mut Vec<u8>,
    slot: usize,
    out_base: usize,
    src_parent: &[u8],
    pool: &mut Dedup,
    keep_variations: bool,
) -> bool {
    let Some(src_off) = read_u16(out, slot) else {
        return true;
    };
    let table = device_table(src_parent, usize::from(src_off))
        .filter(|t| keep_variations || read_u16(t, 4) != Some(VARIATION_INDEX_FORMAT));
    let (new_off, fits) = match table {
        Some(table) => match u16::try_from(pool.place(out, table) - out_base) {
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
/// malformed anchor, which callers treat as "no anchor".
pub(crate) fn copy_anchor(parent: &[u8], offset: usize, keep_variations: bool) -> Vec<u8> {
    if offset == 0 {
        return Vec::new();
    }
    let Some(anchor) = parent.get(offset..) else {
        return Vec::new();
    };
    let len = match read_u16(anchor, 0) {
        Some(1) => 6,
        Some(2) => 8,
        Some(3) => 10,
        _ => return Vec::new(),
    };
    let Some(header) = anchor.get(..len) else {
        return Vec::new();
    };
    let mut out = header.to_vec();
    if len == 10 {
        let mut pool = Dedup::default();
        for slot in [6, 8] {
            // Right behind the 10-byte header, the copies always fit.
            relocate_slot(&mut out, slot, 0, anchor, &mut pool, keep_variations);
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
/// of 16-bit reach of the parent (its slot is cleared).
pub(crate) fn relocate_value_records(
    out: &mut Vec<u8>,
    out_base: usize,
    src_parent: &[u8],
    run: &RecordRun<'_>,
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
