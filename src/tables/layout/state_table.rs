//! Apple AAT extended state table primitive.
//!
//! AAT's `morx` and `kerx` share the same state-machine layout. A
//! "state table" is a 2D table indexed by `(state, class)` that yields
//! an *entry index*; a parallel *entry array* then carries the
//! (next_state, flags, action_index) tuple that drives the machine.
//!
//! # Format (extended state table, used by `morx` / `kerx` v2+)
//!
//! ```text
//!   u32 nClasses
//!   u32 classTableOffset    // from start of state table
//!   u32 stateArrayOffset    //       "
//!   u32 entryTableOffset    //       "
//!   ... (type-specific tails follow in the containing subtable)
//!
//!   Class subtable (at classTableOffset): an AAT Lookup Table that
//!   maps a glyph id to a u16 class number. Classes 0..3 are reserved
//!   (end-of-text, out-of-bounds, deleted-glyph, end-of-line); real
//!   classes start at 4.
//!
//!   State array (at stateArrayOffset): a packed u16 array of length
//!   `nStates * nClasses`. `stateArray[state * nClasses + class]`
//!   yields a *u16 entry index* into the entry array.
//!
//!   Entry array (at entryTableOffset): parallel `Entry` records.
//!   Each entry is `u16 newState, u16 flags` followed by any
//!   type-specific payload (action indices, etc.). The size of one
//!   entry is therefore subtable-type specific.
//! ```
//!
//! sigilbuzz exposes just enough machinery for the three morx
//! subtable types we implement (0: rearrangement, 1: contextual
//! glyph substitution, 2: ligature substitution) and kerx's
//! state-based formats. The class-subtable parser here covers the
//! three AAT lookup formats those tables actually use: format 0
//! (simple array), format 2 (segment-array) and format 6
//! (single-table). Extended morx ships formats 2 and 6; kerx
//! format-2 class tables in the wild also lean on format 0 because
//! the value stream there is dense per-glyph offsets rather than
//! sparse classes. We surface [`Error::Unsupported`] for any other
//! format so the caller can fall back gracefully.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// Reserved state 0: start of text / out of bounds. Every state
/// machine starts here.
pub const START_OF_TEXT: u16 = 0;

/// Reserved class 0: end-of-text marker glyph. The subtable walker
/// injects this class after the last real glyph so state machines
/// that carry pending actions (e.g. a pending rearrangement) get one
/// more chance to fire before the run ends.
pub const CLASS_END_OF_TEXT: u16 = 0;
/// Reserved class 1: out-of-bounds glyph id. Rare outside broken
/// fonts.
pub const CLASS_OUT_OF_BOUNDS: u16 = 1;
/// Reserved class 2: deleted glyph. AAT allows earlier passes to
/// tombstone a glyph without actually removing it from the buffer;
/// sigilbuzz does not emit these, but the class is reserved in the
/// layout so we respect its reservation.
pub const CLASS_DELETED_GLYPH: u16 = 2;
/// Reserved class 3: end-of-line. AAT exposes this so subtables can
/// conditionally disable actions at line breaks; sigilbuzz never
/// assigns this to any glyph.
pub const CLASS_END_OF_LINE: u16 = 3;

/// Parsed header of an extended state table. The header owns no
/// payload slices. Callers combine this with the subtable bytes to
/// walk the state array and the entry array themselves, because
/// entry records are type-specific.
///
/// `data` is the byte slice whose offset zero coincides with the
/// start of the `nClasses` u32. Every offset stored here is relative
/// to that origin, which matches how AAT subtables declare them.
#[derive(Debug, Clone, Copy)]
pub struct StateTableHeader<'a> {
    data: &'a [u8],
    n_classes: u32,
    class_table_off: usize,
    state_array_off: usize,
    entry_table_off: usize,
}

impl<'a> StateTableHeader<'a> {
    /// Parses the fixed 16-byte header. `data` must start at the
    /// state table's own origin (i.e. at the `nClasses` field). The
    /// header consumes the first 16 bytes; the surrounding subtable
    /// is free to layer its own fields after that.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let n_classes = r.read_u32()?;
        let class_table_off = r.read_u32()? as usize;
        let state_array_off = r.read_u32()? as usize;
        let entry_table_off = r.read_u32()? as usize;

        // Sanity: offsets must be inside the containing blob. The
        // state + entry arrays have type-specific extents so we only
        // validate their starts here.
        for (off, ctx) in [
            (class_table_off, "state table class offset"),
            (state_array_off, "state table state-array offset"),
            (entry_table_off, "state table entry-array offset"),
        ] {
            if off > data.len() {
                return Err(Error::Malformed {
                    offset: off,
                    context: ctx,
                });
            }
        }
        Ok(Self {
            data,
            n_classes,
            class_table_off,
            state_array_off,
            entry_table_off,
        })
    }

    /// Header size: every type-specific tail begins past this
    /// many bytes from the start of the state table.
    pub const SIZE: usize = 16;

    /// Number of class columns in the state array.
    #[must_use]
    pub const fn n_classes(&self) -> u32 {
        self.n_classes
    }

    /// Resolves a glyph id to its class column. Returns
    /// [`CLASS_OUT_OF_BOUNDS`] when the glyph is outside every
    /// segment the class subtable covers.
    ///
    /// Only lookup formats 0, 2 and 6 are handled: the formats
    /// real morx/kerx fonts ship. Anything else yields
    /// [`Error::Unsupported`].
    pub fn class_of(&self, glyph_id: u16) -> Result<u16> {
        let slice = self
            .data
            .get(self.class_table_off..)
            .ok_or(Error::Truncated {
                offset: self.class_table_off,
                context: "class subtable slice",
            })?;
        // morx state tables ship format 2 / 6 in practice. Pass 0
        // for n_glyphs because format 0 isn't used here; callers
        // wanting format-0 must use [`lookup_class`] directly with
        // the font's true numGlyphs.
        lookup_class(slice, glyph_id, 0)
    }

    /// Reads the entry index sitting at `(state, class)` in the
    /// state array.
    pub fn entry_index(&self, state: u16, class: u16) -> Result<u16> {
        // `nClasses` is a u32 from the file, so the row arithmetic can
        // overflow a 32-bit usize. Every step is checked.
        let off = usize::try_from(self.n_classes)
            .ok()
            .and_then(|n| usize::from(state).checked_mul(n))
            .and_then(|row| row.checked_add(usize::from(class)))
            .and_then(|cell| cell.checked_mul(2))
            .and_then(|rel| self.state_array_off.checked_add(rel))
            .ok_or(Error::Malformed {
                offset: self.state_array_off,
                context: "state-array index overflow",
            })?;
        read_u16_at(self.data, off, "state-array cell")
    }

    /// Reads the 4-byte "new state + flags" prefix of the entry at
    /// `entry_index`. `entry_size` is the full per-entry size
    /// (subtable-type specific, e.g. 8 for ligature subtables, 6
    /// for contextual subtables). Returns `(new_state, flags)`.
    pub fn entry_prefix(&self, entry_index: u16, entry_size: usize) -> Result<(u16, u16)> {
        let off = usize::from(entry_index)
            .checked_mul(entry_size)
            .and_then(|rel| self.entry_table_off.checked_add(rel))
            .ok_or(Error::Malformed {
                offset: self.entry_table_off,
                context: "entry-array index overflow",
            })?;
        let slice = off
            .checked_add(4)
            .and_then(|end| self.data.get(off..end))
            .ok_or(Error::Truncated {
                offset: off,
                context: "entry prefix",
            })?;
        Ok((
            u16::from_be_bytes([slice[0], slice[1]]),
            u16::from_be_bytes([slice[2], slice[3]]),
        ))
    }

    /// Reads a `u16` tail field of the entry at `entry_index`.
    /// `tail_offset` is the byte offset *within* one entry, past the
    /// 4-byte prefix. E.g. for a ligature subtable entry whose
    /// layout is `(newState, flags, ligActionIndex)`, `tail_offset`
    /// is 4 to read `ligActionIndex`.
    pub fn entry_tail_u16(
        &self,
        entry_index: u16,
        entry_size: usize,
        tail_offset: usize,
    ) -> Result<u16> {
        let off = usize::from(entry_index)
            .checked_mul(entry_size)
            .and_then(|rel| rel.checked_add(tail_offset))
            .and_then(|rel| self.entry_table_off.checked_add(rel))
            .ok_or(Error::Malformed {
                offset: self.entry_table_off,
                context: "entry tail index overflow",
            })?;
        read_u16_at(self.data, off, "entry tail u16")
    }

    /// Raw byte slice the header roots at, used by action-array
    /// readers that live past the header but inside the subtable.
    #[must_use]
    pub const fn data(&self) -> &'a [u8] {
        self.data
    }
}

/// Reads the big-endian u16 at `off`, or reports `Truncated` when the
/// two bytes are not inside `data`.
fn read_u16_at(data: &[u8], off: usize, context: &'static str) -> Result<u16> {
    match off.checked_add(2).and_then(|end| data.get(off..end)) {
        Some(&[hi, lo]) => Ok(u16::from_be_bytes([hi, lo])),
        _ => Err(Error::Truncated {
            offset: off,
            context,
        }),
    }
}

/// AAT "Lookup Table" class resolver. Covers the three formats that
/// morx / kerx use in practice:
///
/// - Format 0: simple array, one u16 per glyph id (dense)
/// - Format 2: segment single values (range-based, binary-searched)
/// - Format 6: single-table (sorted (glyph, value) pairs)
///
/// Formats 4 and 8 are rejected with [`Error::Unsupported`]; they
/// exist in the spec but are vanishingly rare on shipping fonts and
/// sigilbuzz can fall back cleanly to OpenType for whichever font
/// trips one.
///
/// `n_glyphs` is required by format 0, which is just a flat u16
/// array sized by the font's `numGlyphs`. Other formats ignore it.
pub(crate) fn lookup_class(data: &[u8], glyph_id: u16, n_glyphs: u16) -> Result<u16> {
    if data.len() < 2 {
        return Err(Error::Truncated {
            offset: 0,
            context: "AAT lookup header",
        });
    }
    let format = u16::from_be_bytes([data[0], data[1]]);
    match format {
        0 => lookup_format0(data, glyph_id, n_glyphs),
        2 => lookup_format2(data, glyph_id),
        6 => lookup_format6(data, glyph_id),
        4 | 8 => Err(Error::Unsupported {
            context: "AAT lookup format not yet implemented",
        }),
        _ => Err(Error::Malformed {
            offset: 0,
            context: "AAT lookup unknown format",
        }),
    }
}

// Format 0 layout:
//   u16 format = 0
//   u16 values[nGlyphs]
//
// The simplest AAT lookup: one u16 value per glyph in font order.
// Used when the value stream is dense: kerx format 2's left- and
// right-class tables are the canonical case, since they yield a
// per-glyph byte offset that's almost always non-default.
fn lookup_format0(data: &[u8], glyph_id: u16, n_glyphs: u16) -> Result<u16> {
    // Two limits gate the read: the caller-supplied glyph count
    // (when known) and the slice's actual byte length. The smaller
    // of the two wins so a truncated table can never panic.
    let slice_cap = (data.len().saturating_sub(2)) / 2;
    let declared = if n_glyphs == 0 {
        slice_cap
    } else {
        usize::from(n_glyphs)
    };
    let limit = declared.min(slice_cap);
    if usize::from(glyph_id) >= limit {
        return Ok(CLASS_OUT_OF_BOUNDS);
    }
    let off = 2usize + glyph_id as usize * 2;
    // The bound check above already guarantees off + 2 <= data.len(),
    // but ask the slice to confirm so a future refactor can't slip.
    let slice = data.get(off..off + 2).ok_or(Error::Truncated {
        offset: off,
        context: "AAT lookup format 0 cell",
    })?;
    Ok(u16::from_be_bytes([slice[0], slice[1]]))
}

// Format 2 layout:
//   u16 format = 2
//   u16 unitSize      (6: header + 3xu16)
//   u16 nUnits
//   u16 searchRange
//   u16 entrySelector
//   u16 rangeShift
//   Segment segments[nUnits]:
//     u16 lastGlyph
//     u16 firstGlyph
//     u16 value
//   (trailing sentinel segment (0xFFFF, 0xFFFF, 0) terminates the array)
//
// Segments are sorted by `lastGlyph`, so we can binary-search.
fn lookup_format2(data: &[u8], glyph_id: u16) -> Result<u16> {
    if data.len() < 12 {
        return Err(Error::Truncated {
            offset: 0,
            context: "AAT lookup format 2 header",
        });
    }
    let unit_size = u16::from_be_bytes([data[2], data[3]]) as usize;
    let n_units = u16::from_be_bytes([data[4], data[5]]) as usize;
    if unit_size < 6 {
        return Err(Error::Malformed {
            offset: 2,
            context: "AAT lookup format 2 unitSize too small",
        });
    }
    let body_off: usize = 12;
    let required = body_off
        .checked_add(n_units.saturating_mul(unit_size))
        .ok_or(Error::Malformed {
            offset: body_off,
            context: "AAT lookup format 2 body overflow",
        })?;
    if data.len() < required {
        return Err(Error::Truncated {
            offset: required,
            context: "AAT lookup format 2 body",
        });
    }
    // Binary search on `lastGlyph`. Classic AAT layout: search the
    // smallest `lastGlyph >= glyph_id`, then verify `firstGlyph <= glyph_id`.
    let mut lo = 0usize;
    let mut hi = n_units;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let off = body_off + mid * unit_size;
        let last = u16::from_be_bytes([data[off], data[off + 1]]);
        match last.cmp(&glyph_id) {
            core::cmp::Ordering::Less => lo = mid + 1,
            _ => hi = mid,
        }
    }
    if lo == n_units {
        return Ok(CLASS_OUT_OF_BOUNDS);
    }
    let off = body_off + lo * unit_size;
    let last = u16::from_be_bytes([data[off], data[off + 1]]);
    let first = u16::from_be_bytes([data[off + 2], data[off + 3]]);
    let value = u16::from_be_bytes([data[off + 4], data[off + 5]]);
    if glyph_id >= first && glyph_id <= last {
        Ok(value)
    } else {
        Ok(CLASS_OUT_OF_BOUNDS)
    }
}

// Format 6 layout:
//   u16 format = 6
//   u16 unitSize      (4: u16 glyph + u16 value)
//   u16 nUnits
//   u16 searchRange
//   u16 entrySelector
//   u16 rangeShift
//   Record records[nUnits]:
//     u16 glyph
//     u16 value
//   (trailing sentinel (0xFFFF, 0) optional)
//
// Records are sorted by `glyph`.
fn lookup_format6(data: &[u8], glyph_id: u16) -> Result<u16> {
    if data.len() < 12 {
        return Err(Error::Truncated {
            offset: 0,
            context: "AAT lookup format 6 header",
        });
    }
    let unit_size = u16::from_be_bytes([data[2], data[3]]) as usize;
    let n_units = u16::from_be_bytes([data[4], data[5]]) as usize;
    if unit_size < 4 {
        return Err(Error::Malformed {
            offset: 2,
            context: "AAT lookup format 6 unitSize too small",
        });
    }
    let body_off: usize = 12;
    let required = body_off
        .checked_add(n_units.saturating_mul(unit_size))
        .ok_or(Error::Malformed {
            offset: body_off,
            context: "AAT lookup format 6 body overflow",
        })?;
    if data.len() < required {
        return Err(Error::Truncated {
            offset: required,
            context: "AAT lookup format 6 body",
        });
    }
    let mut lo = 0usize;
    let mut hi = n_units;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let off = body_off + mid * unit_size;
        let g = u16::from_be_bytes([data[off], data[off + 1]]);
        match g.cmp(&glyph_id) {
            core::cmp::Ordering::Less => lo = mid + 1,
            core::cmp::Ordering::Equal => {
                let v = u16::from_be_bytes([data[off + 2], data[off + 3]]);
                return Ok(v);
            }
            core::cmp::Ordering::Greater => hi = mid,
        }
    }
    Ok(CLASS_OUT_OF_BOUNDS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Builds an AAT lookup table, format 6, mapping each `(glyph,
    /// class)` pair sorted by glyph. Useful for state-table tests
    /// without pulling in a full fixture.
    pub(crate) fn build_lookup_format6(pairs: &[(u16, u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&6u16.to_be_bytes()); // format
        out.extend_from_slice(&4u16.to_be_bytes()); // unitSize
        out.extend_from_slice(&(pairs.len() as u16).to_be_bytes()); // nUnits
        out.extend_from_slice(&0u16.to_be_bytes()); // searchRange
        out.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
        out.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
        for (g, v) in pairs {
            out.extend_from_slice(&g.to_be_bytes());
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    #[test]
    fn huge_class_count_and_entry_index_report_errors() {
        // nClasses = u32::MAX makes `state * nClasses` overflow a
        // 32-bit usize. Every cell and entry read past the table must
        // come back as an error, never a panic or a wrapped offset.
        let mut data = Vec::new();
        data.extend_from_slice(&u32::MAX.to_be_bytes()); // nClasses
        data.extend_from_slice(&16u32.to_be_bytes()); // class table
        data.extend_from_slice(&16u32.to_be_bytes()); // state array
        data.extend_from_slice(&16u32.to_be_bytes()); // entry table
        data.extend_from_slice(&[0u8; 4]);
        let header = StateTableHeader::parse(&data).unwrap();
        assert_eq!(header.entry_index(0, 0).unwrap(), 0);
        assert!(header.entry_index(u16::MAX, u16::MAX).is_err());
        assert!(header.entry_index(1, 0).is_err());
        assert!(header.entry_prefix(u16::MAX, usize::MAX).is_err());
        assert!(header
            .entry_tail_u16(u16::MAX, usize::MAX, usize::MAX)
            .is_err());
        assert!(header.entry_prefix(0, 8).is_ok());
        assert!(header.entry_prefix(1, 8).is_err());
    }

    #[test]
    fn format6_binary_search_finds_known_glyphs() {
        let tbl = build_lookup_format6(&[(5, 42), (10, 99), (200, 7)]);
        assert_eq!(lookup_class(&tbl, 5, 0).unwrap(), 42);
        assert_eq!(lookup_class(&tbl, 10, 0).unwrap(), 99);
        assert_eq!(lookup_class(&tbl, 200, 0).unwrap(), 7);
    }

    #[test]
    fn format6_returns_out_of_bounds_for_missing() {
        let tbl = build_lookup_format6(&[(5, 42), (10, 99)]);
        assert_eq!(lookup_class(&tbl, 0, 0).unwrap(), CLASS_OUT_OF_BOUNDS);
        assert_eq!(lookup_class(&tbl, 7, 0).unwrap(), CLASS_OUT_OF_BOUNDS);
        assert_eq!(lookup_class(&tbl, 999, 0).unwrap(), CLASS_OUT_OF_BOUNDS);
    }

    #[test]
    fn format2_segments_resolve_ranges() {
        // Segments: [10..=12] -> class 4, [20..=25] -> class 5.
        let mut tbl = Vec::new();
        tbl.extend_from_slice(&2u16.to_be_bytes()); // format
        tbl.extend_from_slice(&6u16.to_be_bytes()); // unitSize
        tbl.extend_from_slice(&2u16.to_be_bytes()); // nUnits
        tbl.extend_from_slice(&[0u8; 6]); // search hints
        tbl.extend_from_slice(&12u16.to_be_bytes()); // last
        tbl.extend_from_slice(&10u16.to_be_bytes()); // first
        tbl.extend_from_slice(&4u16.to_be_bytes()); // value
        tbl.extend_from_slice(&25u16.to_be_bytes()); // last
        tbl.extend_from_slice(&20u16.to_be_bytes()); // first
        tbl.extend_from_slice(&5u16.to_be_bytes()); // value

        assert_eq!(lookup_class(&tbl, 11, 0).unwrap(), 4);
        assert_eq!(lookup_class(&tbl, 20, 0).unwrap(), 5);
        assert_eq!(lookup_class(&tbl, 25, 0).unwrap(), 5);
        assert_eq!(lookup_class(&tbl, 15, 0).unwrap(), CLASS_OUT_OF_BOUNDS);
        assert_eq!(lookup_class(&tbl, 26, 0).unwrap(), CLASS_OUT_OF_BOUNDS);
    }

    #[test]
    fn format0_simple_array_indexes_by_glyph_id() {
        // 4-glyph font: gid 0 -> 0, gid 1 -> 12, gid 2 -> 24, gid 3 -> 36.
        let mut tbl = Vec::new();
        tbl.extend_from_slice(&0u16.to_be_bytes()); // format
        for v in [0u16, 12, 24, 36] {
            tbl.extend_from_slice(&v.to_be_bytes());
        }
        assert_eq!(lookup_class(&tbl, 0, 4).unwrap(), 0);
        assert_eq!(lookup_class(&tbl, 1, 4).unwrap(), 12);
        assert_eq!(lookup_class(&tbl, 3, 4).unwrap(), 36);
        // Glyph past nGlyphs falls through to OOB rather than reading
        // garbage.
        assert_eq!(lookup_class(&tbl, 4, 4).unwrap(), CLASS_OUT_OF_BOUNDS);
    }

    #[test]
    fn format0_truncated_slice_is_defensive() {
        // Format byte but no payload: must not panic.
        let tbl = vec![0x00, 0x00];
        assert_eq!(lookup_class(&tbl, 0, 4).unwrap(), CLASS_OUT_OF_BOUNDS);
    }

    #[test]
    fn unsupported_lookup_format_surfaces_error() {
        let mut tbl = Vec::new();
        // Format 4 (segment array of u16 records, rare) is still
        // rejected until a real font needs it.
        tbl.extend_from_slice(&4u16.to_be_bytes());
        assert!(matches!(
            lookup_class(&tbl, 0, 0),
            Err(Error::Unsupported { .. })
        ));
    }

    #[test]
    fn state_table_header_roundtrips_offsets() {
        // 16-byte header pointing at offsets inside a 64-byte blob.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&4u32.to_be_bytes()); // nClasses
        bytes.extend_from_slice(&16u32.to_be_bytes()); // class off
        bytes.extend_from_slice(&32u32.to_be_bytes()); // state off
        bytes.extend_from_slice(&48u32.to_be_bytes()); // entry off
        bytes.resize(128, 0);
        // State array: one row of 4 u16 cells. Cell at (0, 2) = 7.
        bytes[32 + 4] = 0x00;
        bytes[32 + 5] = 0x07;
        // Entry array: entry #7 prefix = (new_state = 1, flags = 0x8000).
        let entry_off = 48 + 7 * 4;
        bytes[entry_off] = 0;
        bytes[entry_off + 1] = 1;
        bytes[entry_off + 2] = 0x80;
        bytes[entry_off + 3] = 0x00;

        let hdr = StateTableHeader::parse(&bytes).unwrap();
        assert_eq!(hdr.n_classes(), 4);
        assert_eq!(hdr.entry_index(0, 2).unwrap(), 7);
        let (new_state, flags) = hdr.entry_prefix(7, 4).unwrap();
        assert_eq!(new_state, 1);
        assert_eq!(flags, 0x8000);
    }
}
