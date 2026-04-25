//! `kerx` — Apple Extended Kerning.
//!
//! `kerx` is the AAT successor to `kern`. AAT-only fonts ship their
//! kerning here. sigilbuzz consults `kerx` only when GPOS has no
//! `kern` feature, so modern OpenType fonts keep their existing
//! behaviour — this matches HarfBuzz's AAT shaper policy.
//!
//! # Layout
//!
//! ```text
//!   u16 version       (2 or 3)
//!   u16 _pad
//!   u32 nTables
//!   Subtable subtables[nTables]
//!
//!   Subtable:
//!     u32 length       (bytes, incl. this header)
//!     u32 coverage     (low byte = format; high bits = flags)
//!     u32 tupleCount   (variation-font kerning — sigilbuzz ignores)
//!     Body body        (format-specific)
//! ```
//!
//! Three subtable formats are implemented:
//!
//! - Format 0 — ordered pair list (the common case for AAT fonts
//!   that re-use legacy `kern` data).
//! - Format 1 — state-machine kerning. Walks the run through an AAT
//!   extended state table; entries push glyph indices onto a "kern
//!   stack" and reference a list of i16 values that are popped and
//!   applied in pair order. Useful for contextual kerning (e.g. a
//!   spur joining only when not preceded by a space).
//! - Format 2 — n-way class kerning. Two AAT lookup tables map
//!   left and right glyph ids to row / column offsets into a 2D
//!   array of i16 deltas; useful for dense matrices like Latin
//!   pair-class tables that would explode if expanded to flat
//!   pairs.
//!
//! Format 4 — control-point kerning. Parsed for structure (so a
//! mixed-format `kerx` doesn't trip on format-4 bytes), but the
//! apply path is a stub: control-point and anchor-point variants
//! need glyf-point or ankr coordinate reads that sigilbuzz's kerx
//! module deliberately keeps separate from the state-machine walk.
//! The "coordinates" variant (action type 2 — inline FUnit deltas)
//! is structurally parsed too. Walking the state machine to drive
//! actual offsets is left for a follow-up; until then format 4
//! subtables degrade to "parse cleanly, apply nothing", which is
//! strictly better than dropping the surrounding format-0 / 2 / 6
//! subtables on the floor.
//!
//! # Format 6
//!
//! Apple's "simple n × m array" — a compound-class layout like format
//! 2, but the row / column class tables are AAT lookups that yield
//! direct row / column *indices* into a 2D `(rowCount × columnCount)`
//! grid of i16 kern deltas. Layout (relative to the subtable origin):
//!
//! ```text
//!   u32 flags             (bit 0 = "long values"; sigilbuzz only
//!                          implements the short-value variant)
//!   u16 rowCount
//!   u16 columnCount
//!   u32 rowIndexTable     (offset to AAT lookup; glyph → row index)
//!   u32 columnIndexTable  (offset to AAT lookup; glyph → col index)
//!   u32 kerningArray      (offset to i16[rowCount * columnCount])
//!   u32 kerningVector     (long-value variant only — ignored)
//! ```
//!
//! Real shipping fonts pick small row / column counts (a few dozen)
//! and store the indices as plain u16 — the same shape format 2 uses
//! for its pre-multiplied byte offsets, just without the
//! pre-multiplication. The apply path mirrors format 2: resolve both
//! lookups, then read the cell at
//! `array + (row * columnCount + column) * 2`.
//!
//! # Format 0
//!
//! ```text
//!   u32 nPairs
//!   u32 searchRange
//!   u32 entrySelector
//!   u32 rangeShift
//!   Pair pairs[nPairs]:
//!     u16 left
//!     u16 right
//!     i16 value
//! ```
//!
//! Pairs are sorted by the 32-bit key `(left << 16) | right`, so
//! lookup is a binary search — exactly as in the legacy `kern`
//! table, just with a u32 count instead of u16.
//!
//! # Format 1
//!
//! ```text
//!   u32 nClasses
//!   u32 classTableOffset    (relative to format-1 body start)
//!   u32 stateArrayOffset    (        ")
//!   u32 entryTableOffset    (        ")
//!   u32 valueTableOffset    (        ")
//!   ... class subtable, state array, entry array, value list
//! ```
//!
//! Each entry is 6 bytes: `(newState: u16, flags: u16, valueIndex: u16)`.
//! `flags` carries `PUSH` (bit 15 — push the current glyph onto the
//! kern stack), `DONT_ADVANCE` (bit 14 — re-process the current glyph
//! after switching state) and `RESET` (bit 13 — clear the stack;
//! cross-stream only). `valueIndex` is a byte offset from the start
//! of the value table to the first i16 in this entry's value list;
//! `0xFFFF` means "no value list".
//!
//! Value lists are open-ended i16 arrays terminated by an entry with
//! bit 0 set. The masked value (`raw & !1`) is the actual kern delta
//! applied to the glyph popped from the kern stack. Multiple values
//! in a list pop multiple stack glyphs (last-pushed first), so a
//! list of three values applies to the three most recently pushed
//! glyphs.
//!
//! # Format 2
//!
//! ```text
//!   u32 rowWidth         (bytes per row of the kerning array)
//!   u32 leftClassTable   (offset from start of subtable)
//!   u32 rightClassTable  (offset from start of subtable)
//!   u32 array            (offset from start of subtable to i16 grid)
//! ```
//!
//! The class tables are AAT lookup tables. The left table yields a
//! pre-multiplied byte offset (`class * rowWidth`); the right table
//! yields a u16-aligned byte offset (`class * 2`). The kerning value
//! is the i16 at `array + leftValue + rightValue`. Subtable offsets
//! are measured from the start of the 12-byte common subtable header
//! — the same origin Apple's spec uses.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::layout::state_table::{
    lookup_class, StateTableHeader, CLASS_END_OF_TEXT, CLASS_OUT_OF_BOUNDS,
};
use crate::tables::parse::Reader;

const COVERAGE_FORMAT_MASK: u32 = 0xFF;
// Coverage flags (high byte of the coverage u32).
const COVERAGE_VERTICAL: u32 = 1 << 31;
const COVERAGE_CROSS_STREAM: u32 = 1 << 30;
const COVERAGE_VARIATION: u32 = 1 << 29;

/// Parsed `kerx` table.
#[derive(Debug, Clone)]
pub struct Kerx<'a> {
    version: u16,
    num_glyphs: u16,
    subtables: Vec<Subtable<'a>>,
}

#[derive(Debug, Clone, Copy)]
enum Subtable<'a> {
    Format0(Format0<'a>),
    Format1(Format1<'a>),
    Format2(Format2<'a>),
    #[allow(dead_code)] // apply path is a follow-up — see module docs
    Format4(Format4<'a>),
    Format6(Format6<'a>),
}

/// Format 1 — state-machine kerning. Wraps the AAT extended state
/// table primitive plus a value-table slice; the apply pass walks
/// the glyph stream through the state machine, pushing glyphs onto
/// a kern stack on each `PUSH` entry and popping + applying values
/// from the value table whenever an entry references a non-empty
/// value list.
#[derive(Debug, Clone, Copy)]
struct Format1<'a> {
    state: StateTableHeader<'a>,
    /// Slice of the format-1 subtable body that begins at the value
    /// table's origin. Value lists are i16 arrays terminated by an
    /// entry with bit 0 set; this slice provides the bytes those
    /// lists index into.
    value_table: &'a [u8],
}

#[derive(Debug, Clone, Copy)]
struct Format0<'a> {
    data: &'a [u8],
    pairs_off: usize,
    n_pairs: u32,
}

/// Format 2 — n-way class kerning. Records the subtable-relative
/// offsets to the class tables and the kerning array; a kern lookup
/// resolves both classes through the AAT lookup primitive and reads
/// the i16 cell at `array + leftClassValue + rightClassValue`.
#[derive(Debug, Clone, Copy)]
struct Format2<'a> {
    /// The subtable's full byte slice (the 12-byte common header
    /// plus the format-2 body). All recorded offsets are relative
    /// to byte 0 of this slice, matching the spec.
    sub: &'a [u8],
    row_width: u32,
    left_class_off: usize,
    right_class_off: usize,
    array_off: usize,
}

/// Format 4 — control-point kerning. Structurally parsed (state
/// table header + a 32-bit flags word that encodes the action type
/// and action-table offset) so a mixed-format `kerx` doesn't lose
/// surrounding subtables, but the apply path is intentionally a
/// stub — driving the state machine to glyf / ankr point reads
/// requires plumbing this module deliberately keeps separate.
///
/// Action type lives in flags bits 30-31:
/// - 0 = control points (u16 pairs into glyf points)
/// - 1 = anchor points (u16 pairs into ankr)
/// - 2 = coordinates    (four i16 in FUnits — inline)
#[derive(Debug, Clone, Copy)]
struct Format4<'a> {
    #[allow(dead_code)]
    state: StateTableHeader<'a>,
    #[allow(dead_code)]
    action_type: u8,
    #[allow(dead_code)]
    action_table: &'a [u8],
}

/// Format 6 — simple n×m kerning array. Mirrors format 2 but the
/// row / column lookup tables yield direct indices (not pre-multiplied
/// byte offsets) and the array is sized by `rowCount × columnCount`.
/// All offsets are relative to the subtable origin (the 12-byte
/// common header).
#[derive(Debug, Clone, Copy)]
struct Format6<'a> {
    sub: &'a [u8],
    row_count: u16,
    column_count: u16,
    row_index_off: usize,
    col_index_off: usize,
    array_off: usize,
}

impl<'a> Kerx<'a> {
    /// Parses a `kerx` table. Returns [`Error::Unsupported`] for
    /// versions outside {2, 3} — every AAT font sigilbuzz targets
    /// ships one of those two. `num_glyphs` is the font's `maxp`
    /// glyph count, used to bound-check format-0 lookup tables in
    /// format-2 class subtables.
    pub fn parse(data: &'a [u8], num_glyphs: u16) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.read_u16()?;
        if version != 2 && version != 3 {
            return Err(Error::Unsupported {
                context: "kerx version outside {2, 3}",
            });
        }
        let _pad = r.read_u16()?;
        let n_tables = r.read_u32()?;

        let mut subtables = Vec::new();
        for _ in 0..n_tables {
            let sub_start = r.position();
            if sub_start + 12 > data.len() {
                return Err(Error::Truncated {
                    offset: sub_start,
                    context: "kerx subtable header",
                });
            }
            let length = r.read_u32()? as usize;
            let coverage = r.read_u32()?;
            let _tuple_count = r.read_u32()?;

            let sub_end = sub_start.checked_add(length).ok_or(Error::Malformed {
                offset: sub_start,
                context: "kerx subtable length overflow",
            })?;
            if sub_end > data.len() {
                return Err(Error::Truncated {
                    offset: sub_end,
                    context: "kerx subtable extends past table",
                });
            }

            // The declared length must at least cover the 12-byte
            // common header; anything shorter would let `seek(sub_end)`
            // jump backwards into this subtable's own header bytes,
            // parking the next iteration mid-header. Treat such a
            // subtable as "skip cleanly past the header" — drop it
            // and use `sub_start + 12` as the cursor target. The
            // post-header portion (whatever the malformed `length`
            // claimed) is effectively unused.
            let next_cursor = if length < 12 { sub_start + 12 } else { sub_end };

            let format = (coverage & COVERAGE_FORMAT_MASK) as u8;
            // Skip vertical, cross-stream, and variation subtables —
            // sigilbuzz produces horizontal advances only for now.
            // The cross-stream bit moves a glyph's origin in the
            // opposite axis (e.g. Zapfino's connecting ligatures
            // nudge y to tuck the bowls together); applying it
            // blindly would corrupt positions, so we skip until the
            // feature lands.
            if coverage & (COVERAGE_VERTICAL | COVERAGE_CROSS_STREAM | COVERAGE_VARIATION) != 0 {
                r.seek(next_cursor)?;
                continue;
            }

            // Format 0 is the common case. Format 1 is the AAT state
            // machine for contextual kerning. Format 2 (compound-class
            // kerning) covers Latin / CJK fonts that ship a dense
            // pair matrix. Format 6 is the n×m simple grid. Format 4
            // (control-point anchors) is parsed for structure but its
            // apply path is a stub — see [`Format4`] for details.
            //
            // Per-subtable parse failures (declared length shorter
            // than the body, internal offsets out of range) are
            // swallowed: a malformed subtable drops out cleanly while
            // its peers in the same kerx still load. The error path
            // used to propagate, which meant one truncated subtable
            // poisoned the whole table.
            if length >= 12 {
                match format {
                    0 => {
                        if let Ok(Some(sub)) = parse_format0(data, r.position(), sub_end) {
                            subtables.push(Subtable::Format0(sub));
                        }
                    }
                    1 => {
                        if let Ok(Some(sub)) = parse_format1(data, sub_start, sub_end) {
                            subtables.push(Subtable::Format1(sub));
                        }
                    }
                    2 => {
                        if let Ok(Some(sub)) = parse_format2(data, sub_start, sub_end) {
                            subtables.push(Subtable::Format2(sub));
                        }
                    }
                    4 => {
                        if let Ok(Some(sub)) = parse_format4(data, sub_start, sub_end) {
                            subtables.push(Subtable::Format4(sub));
                        }
                    }
                    6 => {
                        if let Ok(Some(sub)) = parse_format6(data, sub_start, sub_end) {
                            subtables.push(Subtable::Format6(sub));
                        }
                    }
                    _ => {}
                }
            }

            r.seek(next_cursor)?;
        }

        Ok(Self {
            version,
            num_glyphs,
            subtables,
        })
    }

    /// Reported version word (2 or 3).
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Sum of pair-kerning deltas across every parsed *pair-lookup*
    /// subtable (formats 0 and 2) for the pair `(left, right)`. Zero
    /// when no pair matches. Format 1 (state machine) is stateful and
    /// is not consulted here — callers wanting full kerx coverage
    /// must also call [`Kerx::apply_state_machines`].
    #[must_use]
    pub fn kern(&self, left: u16, right: u16) -> i16 {
        let key = (u32::from(left) << 16) | u32::from(right);
        let mut total: i32 = 0;
        for sub in &self.subtables {
            let v = match sub {
                Subtable::Format0(f0) => f0.find(key),
                Subtable::Format2(f2) => f2.find(left, right, self.num_glyphs),
                Subtable::Format6(f6) => f6.find(left, right, self.num_glyphs),
                // Format 1 is the state machine — applied separately.
                // Format 4 has no pair-lookup semantics; its apply
                // path needs glyf / ankr coordinates and is deferred.
                Subtable::Format1(_) | Subtable::Format4(_) => continue,
            };
            if let Some(v) = v {
                total += i32::from(v);
            }
        }
        total.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
    }

    /// Walks every format-1 (state-machine) subtable across the run,
    /// applying each value-list pop directly to the targeted glyph's
    /// `x_advance`. Formats 0 / 2 are pair-only and are handled by
    /// [`Kerx::kern`] — this method only drives the stateful subtables.
    ///
    /// `apply` receives `(glyph_index, kern_delta)` for every kern
    /// the state machine emits and is responsible for splatting that
    /// delta wherever it should land (e.g. the glyph's advance). The
    /// callback indirection keeps `kerx` independent of the public
    /// `Glyph` struct.
    pub fn apply_state_machines<F>(&self, glyph_ids: &[u16], mut apply: F)
    where
        F: FnMut(usize, i16),
    {
        for sub in &self.subtables {
            if let Subtable::Format1(f1) = sub {
                f1.apply(glyph_ids, &mut apply);
            }
        }
    }

    /// True iff this `kerx` carries at least one state-machine
    /// (format 1) subtable. Callers can short-circuit the apply walk
    /// when no state machine is present.
    #[must_use]
    pub fn has_state_machine(&self) -> bool {
        self.subtables
            .iter()
            .any(|s| matches!(s, Subtable::Format1(_)))
    }

    /// Number of parsed subtables (any format) — useful in tests to
    /// assert which subtables were retained.
    #[must_use]
    pub fn subtable_count(&self) -> usize {
        self.subtables.len()
    }
}

/// Parses one format-0 subtable body. Returns `Ok(None)` on a
/// recoverable shape error so the rest of `kerx` still loads.
fn parse_format0(data: &[u8], body_start: usize, sub_end: usize) -> Result<Option<Format0<'_>>> {
    if body_start + 16 > sub_end {
        return Err(Error::Truncated {
            offset: body_start,
            context: "kerx format 0 header",
        });
    }
    let n_pairs = u32::from_be_bytes([
        data[body_start],
        data[body_start + 1],
        data[body_start + 2],
        data[body_start + 3],
    ]);
    let pairs_off = body_start + 16; // skip nPairs + 3 search hints
    let pairs_bytes = (n_pairs as usize).saturating_mul(6);
    let required = pairs_off.checked_add(pairs_bytes).ok_or(Error::Malformed {
        offset: pairs_off,
        context: "kerx format 0 pairs overflow",
    })?;
    if required > sub_end {
        return Err(Error::Truncated {
            offset: required,
            context: "kerx format 0 pairs exceed subtable",
        });
    }
    Ok(Some(Format0 {
        data,
        pairs_off,
        n_pairs,
    }))
}

/// Parses one format-2 subtable body. Offsets in the on-disk header
/// are relative to the subtable's own origin, so we keep a slice
/// that starts at `sub_start` and stash it on the descriptor.
fn parse_format2(data: &[u8], sub_start: usize, sub_end: usize) -> Result<Option<Format2<'_>>> {
    let body_start = sub_start + 12;
    if body_start + 16 > sub_end {
        return Err(Error::Truncated {
            offset: body_start,
            context: "kerx format 2 header",
        });
    }
    let row_width = u32::from_be_bytes([
        data[body_start],
        data[body_start + 1],
        data[body_start + 2],
        data[body_start + 3],
    ]);
    let left_off = u32::from_be_bytes([
        data[body_start + 4],
        data[body_start + 5],
        data[body_start + 6],
        data[body_start + 7],
    ]) as usize;
    let right_off = u32::from_be_bytes([
        data[body_start + 8],
        data[body_start + 9],
        data[body_start + 10],
        data[body_start + 11],
    ]) as usize;
    let array_off = u32::from_be_bytes([
        data[body_start + 12],
        data[body_start + 13],
        data[body_start + 14],
        data[body_start + 15],
    ]) as usize;

    let sub_len = sub_end - sub_start;
    // All three offsets must point inside the subtable. Anything
    // else is a malformed font; bail with `None` so the rest of the
    // table still loads instead of poisoning the whole `kerx` parse.
    if left_off >= sub_len || right_off >= sub_len || array_off >= sub_len {
        return Ok(None);
    }
    let sub = &data[sub_start..sub_end];
    Ok(Some(Format2 {
        sub,
        row_width,
        left_class_off: left_off,
        right_class_off: right_off,
        array_off,
    }))
}

/// Parses one format-4 subtable body. Layout (relative to the
/// subtable origin):
///
/// ```text
///   0  : 12 B common header
///  12  : 16 B extended state-table header
///  28  :  4 B flags  (bits 30-31 = action type;
///                     bits 0-29  = action-table offset relative
///                                  to the format-4 body start)
///  ..  : action table (variable, action-type-specific)
/// ```
///
/// The action table layout depends on the action type:
/// - 0 (control points): u16 pairs of glyph point indices
/// - 1 (anchor points):  u16 pairs of `ankr` indices
/// - 2 (coordinates):    four i16 (left x/y, right x/y) per record
///
/// sigilbuzz validates the offset / range for the action table but
/// the actual apply walk is deferred. See module docs.
fn parse_format4(data: &[u8], sub_start: usize, sub_end: usize) -> Result<Option<Format4<'_>>> {
    let body_start = sub_start + 12;
    if body_start + 20 > sub_end {
        return Err(Error::Truncated {
            offset: body_start,
            context: "kerx format 4 header",
        });
    }
    let body = data.get(body_start..sub_end).ok_or(Error::Truncated {
        offset: body_start,
        context: "kerx format 4 body slice",
    })?;
    let Ok(state) = StateTableHeader::parse(body) else {
        return Ok(None);
    };
    let flags = u32::from_be_bytes([body[16], body[17], body[18], body[19]]);
    // Apple uses bits 30-31 for action type; the low 30 bits hold the
    // action-table offset (relative to the format-4 body start).
    let action_type = ((flags >> 30) & 0x3) as u8;
    let action_off = (flags & 0x3FFF_FFFF) as usize;
    if action_off > body.len() {
        return Ok(None);
    }
    let action_table = &body[action_off..];
    Ok(Some(Format4 {
        state,
        action_type,
        action_table,
    }))
}

/// Parses one format-6 subtable body. Body layout is documented at
/// the module level; offsets are relative to the subtable origin
/// (the 12-byte common header sits at byte 0). The "long values"
/// flag is rejected with `Ok(None)` — sigilbuzz only implements the
/// short-value variant, which is what shipping AAT fonts use.
fn parse_format6(data: &[u8], sub_start: usize, sub_end: usize) -> Result<Option<Format6<'_>>> {
    let body_start = sub_start + 12;
    // 4 (flags) + 2 (rowCount) + 2 (columnCount) + 4 + 4 + 4 = 20 bytes
    // for the short-value header. The optional kerningVector u32 only
    // applies when long-values is set, which we reject anyway.
    if body_start + 20 > sub_end {
        return Err(Error::Truncated {
            offset: body_start,
            context: "kerx format 6 header",
        });
    }
    let flags = u32::from_be_bytes([
        data[body_start],
        data[body_start + 1],
        data[body_start + 2],
        data[body_start + 3],
    ]);
    // Bit 0 = long-values: cells become i32 instead of i16, and a
    // `kerningVector` offset follows the array. Real AAT fonts don't
    // ship this — drop the subtable rather than half-implement it.
    if flags & 0x0000_0001 != 0 {
        return Ok(None);
    }
    let row_count = u16::from_be_bytes([data[body_start + 4], data[body_start + 5]]);
    let column_count = u16::from_be_bytes([data[body_start + 6], data[body_start + 7]]);
    let row_index_off = u32::from_be_bytes([
        data[body_start + 8],
        data[body_start + 9],
        data[body_start + 10],
        data[body_start + 11],
    ]) as usize;
    let col_index_off = u32::from_be_bytes([
        data[body_start + 12],
        data[body_start + 13],
        data[body_start + 14],
        data[body_start + 15],
    ]) as usize;
    let array_off = u32::from_be_bytes([
        data[body_start + 16],
        data[body_start + 17],
        data[body_start + 18],
        data[body_start + 19],
    ]) as usize;

    let sub_len = sub_end - sub_start;
    if row_index_off >= sub_len || col_index_off >= sub_len || array_off >= sub_len {
        return Ok(None);
    }
    let sub = &data[sub_start..sub_end];
    Ok(Some(Format6 {
        sub,
        row_count,
        column_count,
        row_index_off,
        col_index_off,
        array_off,
    }))
}

/// Parses one format-1 subtable body. Layout (relative to the
/// subtable origin, i.e. byte 0 = the 12-byte common header):
///
/// ```text
///   0 .. 12 : common subtable header  (length, coverage, tupleCount)
///  12 .. 28 : extended state-table header (nClasses, classOff,
///             stateOff, entryOff)
///  28 .. 32 : valueTableOffset (relative to format-1 body start)
///   ...     : class subtable, state array, entry array, value table
/// ```
///
/// All recorded offsets are relative to byte 0 of the format-1
/// *body* (i.e. byte 12 of the full subtable), matching how the AAT
/// state-table primitive expects them.
fn parse_format1(data: &[u8], sub_start: usize, sub_end: usize) -> Result<Option<Format1<'_>>> {
    let body_start = sub_start + 12;
    if body_start + 20 > sub_end {
        return Err(Error::Truncated {
            offset: body_start,
            context: "kerx format 1 header",
        });
    }
    let body = data.get(body_start..sub_end).ok_or(Error::Truncated {
        offset: body_start,
        context: "kerx format 1 body slice",
    })?;
    // The state-table header lives at body bytes 0..16; the
    // valueTableOffset u32 sits immediately after.
    let Ok(state) = StateTableHeader::parse(body) else {
        return Ok(None);
    };
    let value_off = u32::from_be_bytes([body[16], body[17], body[18], body[19]]) as usize;
    if value_off > body.len() {
        return Ok(None);
    }
    let value_table = &body[value_off..];
    Ok(Some(Format1 { state, value_table }))
}

impl Format1<'_> {
    /// Walks `glyph_ids` through the state machine, invoking `apply`
    /// with each `(target_index, kern_delta)` the value lists emit.
    /// On any malformed read the walk bails cleanly — partial output
    /// is allowed but never panics — so a font with a corrupt format
    /// 1 subtable still positions whatever pairs the apply loop did
    /// reach.
    fn apply<F>(&self, glyph_ids: &[u16], apply: &mut F)
    where
        F: FnMut(usize, i16),
    {
        // newState + flags + valueIndex
        const ENTRY_SIZE: usize = 6;
        // Kern stack: indices into `glyph_ids` of glyphs awaiting a
        // value-list pop. AAT semantics says new pushes go on top
        // and the next value list pops them in reverse — last pushed,
        // first applied — pairing each value with the matching glyph.
        let mut stack: Vec<usize> = Vec::new();
        let mut cur_state: u16 = 0;
        let mut i = 0usize;
        // Bound the walk: at most one pass per glyph plus a few
        // DontAdvance retries. AAT's spec doesn't cap the loop, so
        // we cap it here defensively to avoid pathological fonts
        // looping the shaper.
        let max_iters = glyph_ids.len().saturating_mul(8) + 16;
        let mut iters = 0usize;
        while i <= glyph_ids.len() {
            iters += 1;
            if iters > max_iters {
                return;
            }
            let class = if i == glyph_ids.len() {
                CLASS_END_OF_TEXT
            } else {
                self.state
                    .class_of(glyph_ids[i])
                    .unwrap_or(CLASS_OUT_OF_BOUNDS)
            };
            let Ok(entry_idx) = self.state.entry_index(cur_state, class) else {
                return;
            };
            let Ok((new_state, flags)) = self.state.entry_prefix(entry_idx, ENTRY_SIZE) else {
                return;
            };
            let value_index = self
                .state
                .entry_tail_u16(entry_idx, ENTRY_SIZE, 4)
                .unwrap_or(VALUE_INDEX_NONE);

            if flags & FLAG_F1_PUSH != 0 && i < glyph_ids.len() {
                // Cap the stack at 8 (AAT's documented depth limit
                // for kern actions) to keep a malformed font from
                // ballooning memory.
                if stack.len() < KERN_STACK_MAX {
                    stack.push(i);
                }
            }
            if flags & FLAG_F1_RESET != 0 {
                stack.clear();
            }
            if value_index != VALUE_INDEX_NONE {
                self.consume_value_list(value_index, &mut stack, apply);
            }

            cur_state = new_state;
            if flags & FLAG_F1_DONT_ADVANCE == 0 {
                i += 1;
            } else if i == glyph_ids.len() {
                // End-of-text + DontAdvance would loop forever; bail.
                return;
            }
        }
    }

    /// Reads i16 values starting at `value_index` (a byte offset
    /// from the start of the value table) until an entry with bit 0
    /// set ends the list. Each value is masked to clear bit 0 and
    /// applied to the top of the kern stack via `apply`.
    fn consume_value_list<F>(&self, value_index: u16, stack: &mut Vec<usize>, apply: &mut F)
    where
        F: FnMut(usize, i16),
    {
        let mut off = value_index as usize;
        // Cap the walk at the value table's length so a malformed
        // entry that never sets bit 0 cannot loop forever.
        let max_steps = self.value_table.len() / 2 + 1;
        for _ in 0..max_steps {
            let Some(slice) = self.value_table.get(off..off + 2) else {
                return;
            };
            let raw = i16::from_be_bytes([slice[0], slice[1]]);
            let is_last = (raw as u16) & 1 != 0;
            // Mask out bit 0 — the spec uses it as a list terminator
            // but the actual kern delta is the masked value.
            let value = raw & !1i16;
            if let Some(idx) = stack.pop() {
                if value != 0 {
                    apply(idx, value);
                }
            } else {
                // No glyph to apply against — break to avoid walking
                // past meaningful data.
                return;
            }
            if is_last {
                return;
            }
            off += 2;
        }
    }
}

// --- Format 1 flag bits (per Apple kerx spec) ---
/// Push the current glyph onto the kern stack.
const FLAG_F1_PUSH: u16 = 1 << 15;
/// Don't advance the cursor (re-process the current glyph in the
/// new state).
const FLAG_F1_DONT_ADVANCE: u16 = 1 << 14;
/// Reset the cross-stream kerning state. We honour the flag by
/// clearing the kern stack so a stale push cannot leak into the next
/// run — sigilbuzz does not yet emit cross-stream offsets so the
/// stricter cross-stream resync isn't needed.
const FLAG_F1_RESET: u16 = 1 << 13;
/// Sentinel meaning "this entry has no value list".
const VALUE_INDEX_NONE: u16 = 0xFFFF;
/// Maximum kern stack depth. Apple's documented depth is eight
/// — we mirror that to bound memory on malformed fonts.
const KERN_STACK_MAX: usize = 8;

impl Format0<'_> {
    fn pair_at(&self, i: u32) -> (u32, i16) {
        let off = self.pairs_off + i as usize * 6;
        let left = u16::from_be_bytes([self.data[off], self.data[off + 1]]);
        let right = u16::from_be_bytes([self.data[off + 2], self.data[off + 3]]);
        let value = i16::from_be_bytes([self.data[off + 4], self.data[off + 5]]);
        ((u32::from(left) << 16) | u32::from(right), value)
    }

    fn find(&self, key: u32) -> Option<i16> {
        if self.n_pairs == 0 {
            return None;
        }
        let mut lo: u32 = 0;
        let mut hi: u32 = self.n_pairs;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (k, v) = self.pair_at(mid);
            match k.cmp(&key) {
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
                core::cmp::Ordering::Equal => return Some(v),
            }
        }
        None
    }
}

impl Format2<'_> {
    /// Resolves `(left, right)` through the class tables and reads
    /// the i16 cell. Returns `None` for any defensive failure: bad
    /// offsets, unsupported lookup formats, glyphs that fall in the
    /// reserved-class slots, or a cell that lands outside the
    /// subtable. Format 2 always returns deltas — no half-split,
    /// no cross-stream — so a `Some(0)` would be indistinguishable
    /// from "no rule"; callers don't need the distinction.
    fn find(&self, left: u16, right: u16, num_glyphs: u16) -> Option<i16> {
        let left_table = self.sub.get(self.left_class_off..)?;
        let right_table = self.sub.get(self.right_class_off..)?;

        let left_value = lookup_class(left_table, left, num_glyphs).ok()?;
        let right_value = lookup_class(right_table, right, num_glyphs).ok()?;

        // Reserved-class lookups (out-of-bounds, deleted, etc.) are
        // returned by the AAT lookup helper as small sentinel values
        // (1, 2, 3). Format-2 class tables on real fonts fold these
        // into the row-0 / column-0 default cell, which is *almost*
        // always zero. Rather than special-casing, we follow the
        // spec: read the cell at the resolved offset; out-of-range
        // glyphs land on row 0 (default) and the array there is
        // typically zeroed.

        let cell_off = self
            .array_off
            .checked_add(usize::from(left_value))?
            .checked_add(usize::from(right_value))?;
        // The cell must be a fully-contained i16. row_width is also
        // a sanity hint: a left value beyond row_width would mean a
        // malformed lookup table, but again we tolerate it by
        // letting the slice bound check do the work.
        let _ = self.row_width; // referenced for the doc-driven invariant
        let bytes = self.sub.get(cell_off..cell_off + 2)?;
        Some(i16::from_be_bytes([bytes[0], bytes[1]]))
    }
}

impl Format6<'_> {
    /// Resolves `(left, right)` through the row / column index
    /// lookups and reads the i16 cell at
    /// `array + (row * column_count + column) * 2`. Returns `None`
    /// for any defensive failure (bad lookup format, glyph in a
    /// reserved class, cell outside the subtable). Matches format 2's
    /// "no rule = zero" convention so the kern path doesn't need a
    /// distinct sentinel.
    fn find(&self, left: u16, right: u16, num_glyphs: u16) -> Option<i16> {
        let row_table = self.sub.get(self.row_index_off..)?;
        let col_table = self.sub.get(self.col_index_off..)?;

        let row = lookup_class(row_table, left, num_glyphs).ok()?;
        let col = lookup_class(col_table, right, num_glyphs).ok()?;

        // Reserved-class sentinels (1 / 2 / 3) typically fall on row
        // or column 0 in real fonts — read the cell as-is and let
        // the array's natural zero slots handle the case. If either
        // index lands past the declared row / column count, treat the
        // pair as "no rule".
        if row >= self.row_count || col >= self.column_count {
            return None;
        }
        let cell_idx = usize::from(row) * usize::from(self.column_count) + usize::from(col);
        let cell_off = self.array_off.checked_add(cell_idx * 2)?;
        let bytes = self.sub.get(cell_off..cell_off + 2)?;
        Some(i16::from_be_bytes([bytes[0], bytes[1]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn build_kerx_format0(pairs: &[(u16, u16, i16)]) -> Vec<u8> {
        let pair_bytes = pairs.len() * 6;
        let body_len = 16 + pair_bytes; // 4 × u32 + pairs
        let sub_len = 12 + body_len;

        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes()); // version
        out.extend_from_slice(&0u16.to_be_bytes()); // pad
        out.extend_from_slice(&1u32.to_be_bytes()); // nTables

        // Subtable header.
        out.extend_from_slice(&(sub_len as u32).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // coverage: horizontal, format 0
        out.extend_from_slice(&0u32.to_be_bytes()); // tupleCount

        // Format 0 body.
        out.extend_from_slice(&(pairs.len() as u32).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // searchRange
        out.extend_from_slice(&0u32.to_be_bytes()); // entrySelector
        out.extend_from_slice(&0u32.to_be_bytes()); // rangeShift
        for (l, r, v) in pairs {
            out.extend_from_slice(&l.to_be_bytes());
            out.extend_from_slice(&r.to_be_bytes());
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    /// Builds a one-subtable kerx with format 2, two left classes
    /// (mapped via lookup format 0) and two right classes. `n_glyphs`
    /// is the synthetic font's glyph count (keeps the format-0 table
    /// dense). `left_classes[i]` is the class for gid `i` (0-based);
    /// same for `right_classes`. `matrix[l][r]` is the i16 delta.
    fn build_kerx_format2(
        n_glyphs: u16,
        left_classes: &[u16],
        right_classes: &[u16],
        matrix: &[Vec<i16>],
    ) -> Vec<u8> {
        let n_left = matrix.len() as u32;
        let n_right = matrix[0].len() as u32;
        let row_width = n_right * 2;

        // Left table (format 0): each cell already pre-multiplied
        // by row_width.
        let mut left_lookup: Vec<u8> = Vec::new();
        left_lookup.extend_from_slice(&0u16.to_be_bytes()); // format 0
        for &c in left_classes {
            let off = (u32::from(c) * row_width) as u16;
            left_lookup.extend_from_slice(&off.to_be_bytes());
        }
        // Right table (format 0): each cell pre-multiplied by 2.
        let mut right_lookup: Vec<u8> = Vec::new();
        right_lookup.extend_from_slice(&0u16.to_be_bytes());
        for &c in right_classes {
            let off: u16 = c * 2;
            right_lookup.extend_from_slice(&off.to_be_bytes());
        }

        // Body layout (relative to subtable start):
        //   0  : 12 B common header
        //   12 : 16 B fmt2 header (rowWidth, leftOff, rightOff, arrOff)
        //   28 : left lookup
        //   .. : right lookup
        //   .. : kerning array
        let header_size = 12 + 16;
        let left_off = header_size;
        let right_off = left_off + left_lookup.len();
        let array_off = right_off + right_lookup.len();
        let array_bytes = (n_left * row_width) as usize;
        let sub_len = array_off + array_bytes;

        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes()); // version
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes()); // nTables

        // Common subtable header.
        out.extend_from_slice(&(sub_len as u32).to_be_bytes());
        out.extend_from_slice(&2u32.to_be_bytes()); // coverage: format 2
        out.extend_from_slice(&0u32.to_be_bytes()); // tupleCount

        // fmt2 header.
        out.extend_from_slice(&row_width.to_be_bytes());
        out.extend_from_slice(&(left_off as u32).to_be_bytes());
        out.extend_from_slice(&(right_off as u32).to_be_bytes());
        out.extend_from_slice(&(array_off as u32).to_be_bytes());

        out.extend_from_slice(&left_lookup);
        out.extend_from_slice(&right_lookup);
        for row in matrix {
            for v in row {
                out.extend_from_slice(&v.to_be_bytes());
            }
        }

        // Sanity: caller's class arrays must cover n_glyphs.
        assert_eq!(left_classes.len(), n_glyphs as usize);
        assert_eq!(right_classes.len(), n_glyphs as usize);
        out
    }

    #[test]
    fn format0_binary_search_finds_pairs() {
        let bytes = build_kerx_format0(&[(10, 20, -30), (10, 30, -5), (40, 5, 7)]);
        let k = Kerx::parse(&bytes, 256).unwrap();
        assert_eq!(k.version(), 2);
        assert_eq!(k.kern(10, 20), -30);
        assert_eq!(k.kern(40, 5), 7);
        assert_eq!(k.kern(99, 99), 0);
    }

    #[test]
    fn empty_kerx_table_yields_no_subtables() {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes()); // nTables
        let k = Kerx::parse(&bytes, 0).unwrap();
        assert_eq!(k.subtable_count(), 0);
        assert_eq!(k.kern(1, 2), 0);
    }

    #[test]
    fn vertical_subtable_is_skipped() {
        // Build a 2-subtable kerx: first horizontal, second vertical
        // (coverage bit 31 set). The vertical one should be dropped.
        let sub_body_len = 16 + 6; // one pair
        let sub_len = 12 + sub_body_len;
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes()); // 2 subtables

        for (coverage, value) in [(0u32, -10i16), (COVERAGE_VERTICAL, 99i16)] {
            bytes.extend_from_slice(&(sub_len as u32).to_be_bytes());
            bytes.extend_from_slice(&coverage.to_be_bytes());
            bytes.extend_from_slice(&0u32.to_be_bytes()); // tupleCount
            bytes.extend_from_slice(&1u32.to_be_bytes()); // nPairs
            bytes.extend_from_slice(&[0u8; 12]);
            bytes.extend_from_slice(&10u16.to_be_bytes());
            bytes.extend_from_slice(&20u16.to_be_bytes());
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        let k = Kerx::parse(&bytes, 256).unwrap();
        assert_eq!(k.subtable_count(), 1);
        assert_eq!(k.kern(10, 20), -10);
    }

    #[test]
    fn rejects_unknown_version() {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&5u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        assert!(matches!(
            Kerx::parse(&bytes, 0),
            Err(Error::Unsupported { .. })
        ));
    }

    #[test]
    fn format2_compound_class_lookup_resolves_pairs() {
        // 4-glyph synthetic font:
        //   gid 0 .notdef       → left class 0, right class 0
        //   gid 1 A             → left class 1, right class 0
        //   gid 2 B             → left class 1, right class 0
        //   gid 3 V             → left class 0, right class 1
        // Matrix [left][right]:
        //   [[ 0,   0],
        //    [-30,-50]]
        // So (A, V) and (B, V) both kern by -50, while every other
        // pair is zero (and therefore never matches).
        let bytes = build_kerx_format2(
            4,
            &[0, 1, 1, 0],
            &[0, 0, 0, 1],
            &[vec![0, 0], vec![-30, -50]],
        );
        let k = Kerx::parse(&bytes, 4).unwrap();
        assert_eq!(k.subtable_count(), 1);
        assert_eq!(k.kern(1, 3), -50, "A-V pair via classes (1, 1)");
        assert_eq!(k.kern(2, 3), -50, "B-V pair via classes (1, 1)");
        assert_eq!(k.kern(1, 1), -30, "A-A pair via classes (1, 0)");
        assert_eq!(k.kern(0, 0), 0, ".notdef pair → row 0 default");
        assert_eq!(k.kern(3, 1), 0, "V-A reversed pair → row 0 default");
    }

    #[test]
    fn format2_with_zero_cell_returns_zero() {
        // Pair lands on a zero entry — kern() must still return 0
        // without surfacing a parser error.
        let bytes = build_kerx_format2(3, &[0, 1, 1], &[0, 1, 1], &[vec![0, 0], vec![0, 7]]);
        let k = Kerx::parse(&bytes, 3).unwrap();
        assert_eq!(k.kern(1, 0), 0); // left class 1, right class 0 → 0
        assert_eq!(k.kern(2, 2), 7); // left class 1, right class 1
    }

    #[test]
    fn format2_bad_class_offset_silently_drops_subtable() {
        // Build a valid fmt2 then clobber the leftClassTable offset
        // to point past the subtable. parse() must still succeed and
        // simply skip the subtable rather than fail the table.
        let mut bytes = build_kerx_format2(3, &[0, 1, 1], &[0, 1, 1], &[vec![0, 0], vec![0, 7]]);
        // Subtable starts at offset 8 (kerx header size). fmt2
        // header at offset 8 + 12 = 20; leftClassTable u32 lives at
        // offset 24.
        let bad = u32::MAX.to_be_bytes();
        bytes[24] = bad[0];
        bytes[25] = bad[1];
        bytes[26] = bad[2];
        bytes[27] = bad[3];
        let k = Kerx::parse(&bytes, 3).unwrap();
        assert_eq!(k.subtable_count(), 0);
    }

    #[test]
    fn truncated_subtable_length_does_not_poison_following_subtables() {
        // Two subtables: the first declares a `length` field that
        // covers only the 12-byte header (no body) — too short for
        // any format-0 / format-2 body to fit. The whole-table
        // parse must still succeed and surface the *second*
        // subtable's pair, instead of bailing out and producing
        // zero kerning for the entire font.
        //
        // Pre-fix: `parse_format0` returns `Err(Truncated)` from
        // inside the loop, the `?` propagates, and the kerx parse
        // fails — even though the second subtable is well-formed.
        let pair_bytes = 6;
        let good_body = 16 + pair_bytes;
        let good_sub_len = 12 + good_body;
        // Bad subtable length: 12 (header only) — body is missing.
        let bad_sub_len: u32 = 12;

        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes()); // version
        bytes.extend_from_slice(&0u16.to_be_bytes()); // pad
        bytes.extend_from_slice(&2u32.to_be_bytes()); // nTables = 2

        // Subtable 1: malformed — header says 12 bytes total, no body.
        bytes.extend_from_slice(&bad_sub_len.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes()); // coverage: format 0, horizontal
        bytes.extend_from_slice(&0u32.to_be_bytes()); // tupleCount

        // Subtable 2: well-formed format 0 with one pair (10, 20) → -42.
        bytes.extend_from_slice(&(good_sub_len as u32).to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes()); // coverage: format 0
        bytes.extend_from_slice(&0u32.to_be_bytes()); // tupleCount
        bytes.extend_from_slice(&1u32.to_be_bytes()); // nPairs
        bytes.extend_from_slice(&[0u8; 12]); // search hints
        bytes.extend_from_slice(&10u16.to_be_bytes());
        bytes.extend_from_slice(&20u16.to_be_bytes());
        bytes.extend_from_slice(&(-42i16).to_be_bytes());

        let k = Kerx::parse(&bytes, 256).expect("kerx parse must not fail");
        assert_eq!(
            k.subtable_count(),
            1,
            "malformed subtable should be skipped, well-formed one kept"
        );
        assert_eq!(k.kern(10, 20), -42, "well-formed subtable's pair lookup");
    }

    #[test]
    fn subtable_length_smaller_than_header_does_not_loop_or_overlap() {
        // Pathological: subtable length declared as 5 bytes — smaller
        // than its own 12-byte common header. After the header read
        // the cursor sits at sub_start + 12, but `seek(sub_end)` would
        // jump backwards to sub_start + 5, parking the next iteration
        // mid-header. The parser must refuse to seek backwards (or
        // skip the subtable cleanly) so a malformed font cannot drag
        // the rest of `kerx` into garbage territory.
        let pair_bytes = 6;
        let good_body = 16 + pair_bytes;
        let good_sub_len = 12 + good_body;

        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes()); // version
        bytes.extend_from_slice(&0u16.to_be_bytes()); // pad
        bytes.extend_from_slice(&2u32.to_be_bytes()); // nTables

        // Subtable 1: length = 5 — body would overlap header.
        bytes.extend_from_slice(&5u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());

        // Subtable 2: well-formed.
        bytes.extend_from_slice(&(good_sub_len as u32).to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes()); // nPairs
        bytes.extend_from_slice(&[0u8; 12]);
        bytes.extend_from_slice(&10u16.to_be_bytes());
        bytes.extend_from_slice(&20u16.to_be_bytes());
        bytes.extend_from_slice(&(-7i16).to_be_bytes());

        let k = Kerx::parse(&bytes, 256).expect("kerx parse must not fail");
        // The sub-header-size subtable is dropped; the well-formed
        // one is preserved.
        assert_eq!(k.subtable_count(), 1);
        assert_eq!(k.kern(10, 20), -7);
    }

    // -----------------------------------------------------------------
    // Format 1 — state-machine kerning.
    // -----------------------------------------------------------------

    /// Builds an AAT lookup-table format 6 (sorted glyph→class
    /// pairs). Mirrors the helper in `state_table.rs::tests` since
    /// `mod tests` is private to its module.
    fn build_lookup_format6(pairs: &[(u16, u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&6u16.to_be_bytes()); // format
        out.extend_from_slice(&4u16.to_be_bytes()); // unitSize
        out.extend_from_slice(&(pairs.len() as u16).to_be_bytes());
        out.extend_from_slice(&[0u8; 6]); // search hints
        for (g, v) in pairs {
            out.extend_from_slice(&g.to_be_bytes());
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    /// Builds a one-subtable kerx with format 1 wired for a tiny
    /// "kern A before V only when not after space" state machine.
    /// The A glyph id, V glyph id and space glyph id are caller
    /// inputs so the test can pick non-overlapping gids.
    fn build_kerx_format1_av_after_letter(a_gid: u16, v_gid: u16, sp_gid: u16) -> Vec<u8> {
        // Class subtable (format 6): A→4, V→5, space→6. Sorted by
        // glyph id so the binary search keeps working regardless of
        // caller's choice of gids.
        let mut sorted = [(a_gid, 4u16), (v_gid, 5u16), (sp_gid, 6u16)];
        sorted.sort_by_key(|p| p.0);
        let class_lookup = build_lookup_format6(&sorted);

        // Format 1 body layout (everything offset from body start):
        //   0..16   state-table header
        //  16..20   valueTableOffset (u32)
        //  20..     class lookup (aligned to 2)
        //  ..       state array (nStates × nClasses × u16)
        //  ..       entry array (n_entries × 6)
        //  ..       value table
        let n_classes: u32 = 7;
        let n_states: u32 = 2;
        let n_entries: usize = 5;

        let header_len = 20;
        let class_off = header_len;
        let class_end = class_off + class_lookup.len();
        // 2-byte align state array.
        let state_off = class_end + (class_end % 2);
        let state_bytes = (n_states * n_classes) as usize * 2;
        let entry_off = state_off + state_bytes;
        let entry_bytes = n_entries * 6;
        let value_off = entry_off + entry_bytes;
        let value_bytes = 2usize; // single terminator-marked entry

        let body_len = value_off + value_bytes;

        let mut body: Vec<u8> = Vec::with_capacity(body_len);
        // --- State table header ---
        body.extend_from_slice(&n_classes.to_be_bytes());
        body.extend_from_slice(&(class_off as u32).to_be_bytes());
        body.extend_from_slice(&(state_off as u32).to_be_bytes());
        body.extend_from_slice(&(entry_off as u32).to_be_bytes());
        body.extend_from_slice(&(value_off as u32).to_be_bytes());
        // --- Class lookup ---
        body.extend_from_slice(&class_lookup);
        if body.len() < state_off {
            body.resize(state_off, 0);
        }
        // --- State array ---
        // State 0: cells per class.
        //   class 0..3 (reserved)  → entry 0 (noop)
        //   class 4 (A)            → entry 1 (push, stay state 0)
        //   class 5 (V)            → entry 2 (apply value 0, stay state 0)
        //   class 6 (space)        → entry 3 (no-op, go state 1)
        // State 1: cells per class.
        //   class 4 (A)            → entry 4 (no-op, go state 0; suppresses push)
        //   class 5 (V)            → entry 0 (noop — no V kern after solo space)
        //   class 6 (space)        → entry 3 (stay state 1)
        let s0: [u16; 7] = [0, 0, 0, 0, 1, 2, 3];
        let s1: [u16; 7] = [0, 0, 0, 0, 4, 0, 3];
        for v in s0.iter().chain(s1.iter()) {
            body.extend_from_slice(&v.to_be_bytes());
        }
        // --- Entries (newState, flags, valueIndex) ---
        let push: u16 = 0x8000;
        let entries: [(u16, u16, u16); 5] = [
            (0, 0, VALUE_INDEX_NONE),    // #0 noop
            (0, push, VALUE_INDEX_NONE), // #1 push
            (0, 0, 0),                   // #2 apply value at offset 0
            (1, 0, VALUE_INDEX_NONE),    // #3 → state 1
            (0, 0, VALUE_INDEX_NONE),    // #4 → state 0 (clears stale A)
        ];
        for (ns, fl, vi) in entries {
            body.extend_from_slice(&ns.to_be_bytes());
            body.extend_from_slice(&fl.to_be_bytes());
            body.extend_from_slice(&vi.to_be_bytes());
        }
        // --- Value table: one i16 = -50 with terminator bit. ---
        // The spec says: value list terminated by an entry whose bit
        // 0 is set; the kern delta is the value with bit 0 cleared.
        // -50 is even (0xFFCE), so writing 0xFFCF keeps the magnitude
        // and adds the terminator. Reinterpret the bit pattern as i16.
        #[allow(clippy::cast_possible_wrap)]
        let raw = 0xFFCFu16 as i16;
        body.extend_from_slice(&raw.to_be_bytes());

        // Wrap in the 12-byte common subtable header + 8-byte kerx
        // table header.
        let sub_len = 12 + body.len();
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes()); // version
        bytes.extend_from_slice(&0u16.to_be_bytes()); // pad
        bytes.extend_from_slice(&1u32.to_be_bytes()); // nTables
        bytes.extend_from_slice(&(sub_len as u32).to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes()); // coverage: format 1
        bytes.extend_from_slice(&0u32.to_be_bytes()); // tupleCount
        bytes.extend_from_slice(&body);
        bytes
    }

    /// Captures `(glyph_index, kern_delta)` callbacks during a
    /// state-machine apply pass.
    fn collect_kerns(k: &Kerx<'_>, ids: &[u16]) -> Vec<(usize, i16)> {
        let mut out = Vec::new();
        k.apply_state_machines(ids, |idx, delta| out.push((idx, delta)));
        out
    }

    #[test]
    fn format1_parses_and_reports_state_machine() {
        let bytes = build_kerx_format1_av_after_letter(1, 2, 3);
        let k = Kerx::parse(&bytes, 8).unwrap();
        assert_eq!(k.subtable_count(), 1);
        assert!(k.has_state_machine());
        // No pair-list subtable — the legacy kern() lookup must
        // return zero so the apply path doesn't double-count.
        assert_eq!(k.kern(1, 2), 0);
    }

    #[test]
    fn format1_kerns_av_when_not_after_space() {
        // gid 1 = A, gid 2 = V — a contiguous AV pair should kern.
        let bytes = build_kerx_format1_av_after_letter(1, 2, 3);
        let k = Kerx::parse(&bytes, 8).unwrap();
        let kerns = collect_kerns(&k, &[1, 2]);
        assert_eq!(kerns, alloc::vec![(0, -50)]);
    }

    #[test]
    fn format1_skips_av_after_space() {
        // gid 3 (space) before AV: the state machine's "after space"
        // state suppresses the A push, so no kern lands.
        let bytes = build_kerx_format1_av_after_letter(1, 2, 3);
        let k = Kerx::parse(&bytes, 8).unwrap();
        let kerns = collect_kerns(&k, &[3, 1, 2]);
        assert!(kerns.is_empty(), "no kern after a leading space");
    }

    #[test]
    fn format1_kerns_repeated_av_pairs() {
        // "AVAV" should kern both pairs — the state machine is
        // designed to reset to state 0 after each V.
        let bytes = build_kerx_format1_av_after_letter(1, 2, 3);
        let k = Kerx::parse(&bytes, 8).unwrap();
        let kerns = collect_kerns(&k, &[1, 2, 1, 2]);
        assert_eq!(kerns, alloc::vec![(0, -50), (2, -50)]);
    }

    #[test]
    fn format1_walk_terminates_on_corrupt_value_list() {
        // Build a working machine, then clobber the value-table byte
        // so bit 0 is *not* set — the consume_value_list cap should
        // bail before walking off the end.
        let mut bytes = build_kerx_format1_av_after_letter(1, 2, 3);
        // The value byte sits as the very last 2 bytes of the
        // table. Clear bit 0 so the list never terminates organically.
        let len = bytes.len();
        bytes[len - 1] &= !1;
        let k = Kerx::parse(&bytes, 8).unwrap();
        // Apply must still return without panicking; the kern that
        // *would* have been emitted may or may not land but the
        // shaper must not loop or crash.
        let _ = collect_kerns(&k, &[1, 2, 1, 2]);
    }

    #[test]
    fn format1_handles_empty_input() {
        let bytes = build_kerx_format1_av_after_letter(1, 2, 3);
        let k = Kerx::parse(&bytes, 8).unwrap();
        let kerns = collect_kerns(&k, &[]);
        assert!(kerns.is_empty());
    }

    // -----------------------------------------------------------------
    // Format 4 — control-point kerning. Parse-only coverage: the
    // apply path is a stub (glyf-coordinate / ankr reads live
    // outside this module), so the tests assert that a format-4
    // subtable parses cleanly and its presence does not corrupt the
    // surrounding kerx — the kerx must still yield zero pair kerns
    // on it without consuming subsequent format-0 subtables.
    // -----------------------------------------------------------------

    /// Builds a one-subtable kerx with format 4 wired with an empty
    /// state table (one state, two classes, single noop entry) and
    /// the action-type-2 (coordinates) flag. The inner action table
    /// holds one record of four zeros — enough to validate parsing
    /// without driving any glyph offset.
    fn build_kerx_format4(action_type: u8) -> Vec<u8> {
        // Format 4 body layout:
        //   0..16   state-table header (nClasses, classOff, stateOff,
        //                                entryOff)
        //  16..20   flags (action_type << 30 | action_off)
        //  20..     class lookup (format 6, empty)
        //  ..       state array (1 state × 4 classes × u16) = 8 B
        //  ..       entry array (1 entry × 6 B)
        //  ..       action table (one 8-B record)
        let n_classes: u32 = 4; // four reserved classes is the AAT minimum
        let header_len = 20;

        // Empty class lookup (format 6, zero entries).
        let mut class_lookup: Vec<u8> = Vec::new();
        class_lookup.extend_from_slice(&6u16.to_be_bytes());
        class_lookup.extend_from_slice(&4u16.to_be_bytes()); // unitSize
        class_lookup.extend_from_slice(&0u16.to_be_bytes()); // nUnits
        class_lookup.extend_from_slice(&[0u8; 6]); // search hints

        let class_off = header_len;
        let class_end = class_off + class_lookup.len();
        let state_off = class_end + (class_end % 2);
        let state_bytes = n_classes as usize * 2;
        let entry_off = state_off + state_bytes;
        let entry_bytes = 6;
        let action_off = entry_off + entry_bytes;
        let action_bytes = 8;

        let body_len = action_off + action_bytes;

        let mut body: Vec<u8> = Vec::with_capacity(body_len);
        // State-table header.
        body.extend_from_slice(&n_classes.to_be_bytes());
        body.extend_from_slice(&(class_off as u32).to_be_bytes());
        body.extend_from_slice(&(state_off as u32).to_be_bytes());
        body.extend_from_slice(&(entry_off as u32).to_be_bytes());
        // Flags: action_type in bits 30-31, action_off in low 30.
        let flags: u32 = (u32::from(action_type) << 30) | (action_off as u32);
        body.extend_from_slice(&flags.to_be_bytes());
        body.extend_from_slice(&class_lookup);
        if body.len() < state_off {
            body.resize(state_off, 0);
        }
        // State row: every cell points at entry 0 (noop).
        for _ in 0..n_classes {
            body.extend_from_slice(&0u16.to_be_bytes());
        }
        // Entry 0: noop.
        body.extend_from_slice(&0u16.to_be_bytes()); // newState
        body.extend_from_slice(&0u16.to_be_bytes()); // flags
        body.extend_from_slice(&0u16.to_be_bytes()); // actionIndex
                                                     // Action record: 8 bytes of zero (four i16s for the
                                                     // coordinates variant — for control-points / anchors the
                                                     // shape happens to overlap, so the same fill works).
        body.extend_from_slice(&[0u8; 8]);

        // Wrap in 12-byte common header + 8-byte kerx table header.
        let sub_len = 12 + body.len();
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes()); // version
        bytes.extend_from_slice(&0u16.to_be_bytes()); // pad
        bytes.extend_from_slice(&1u32.to_be_bytes()); // nTables
        bytes.extend_from_slice(&(sub_len as u32).to_be_bytes());
        bytes.extend_from_slice(&4u32.to_be_bytes()); // coverage: format 4
        bytes.extend_from_slice(&0u32.to_be_bytes()); // tupleCount
        bytes.extend_from_slice(&body);
        bytes
    }

    #[test]
    fn format4_parses_with_coordinates_action_type() {
        // Action type 2 = coordinates (inline FUnit deltas). Parses
        // cleanly and the subtable is retained.
        let bytes = build_kerx_format4(2);
        let k = Kerx::parse(&bytes, 8).unwrap();
        assert_eq!(k.subtable_count(), 1, "format 4 subtable retained");
        // Format 4 produces no pair kerns — it's stateful and its
        // apply path is deferred.
        assert_eq!(k.kern(1, 2), 0);
    }

    #[test]
    fn format4_parses_with_control_points_action_type() {
        let bytes = build_kerx_format4(0);
        let k = Kerx::parse(&bytes, 8).unwrap();
        assert_eq!(k.subtable_count(), 1);
        assert_eq!(k.kern(1, 2), 0);
    }

    #[test]
    fn format4_does_not_drop_following_subtables() {
        // Mixed kerx: format 4 first, then format 0 with one pair.
        // The format-4 subtable parses-but-emits-nothing path must
        // not interfere with the format-0 lookup. This guards the
        // "deferred apply path" promise from the module docs.
        let f4 = build_kerx_format4(2);
        // f4 layout: 8 B header + 1 subtable. Strip the kerx header
        // and re-emit with two subtables.
        let f4_sub = &f4[8..];

        // Build a tiny format-0 subtable directly.
        let pairs: &[(u16, u16, i16)] = &[(10, 20, -42)];
        let pair_bytes = pairs.len() * 6;
        let f0_body_len = 16 + pair_bytes;
        let f0_sub_len = 12 + f0_body_len;
        let mut f0_sub: Vec<u8> = Vec::new();
        f0_sub.extend_from_slice(&(f0_sub_len as u32).to_be_bytes());
        f0_sub.extend_from_slice(&0u32.to_be_bytes()); // coverage: fmt 0
        f0_sub.extend_from_slice(&0u32.to_be_bytes()); // tupleCount
        f0_sub.extend_from_slice(&(pairs.len() as u32).to_be_bytes());
        f0_sub.extend_from_slice(&[0u8; 12]);
        for (l, r, v) in pairs {
            f0_sub.extend_from_slice(&l.to_be_bytes());
            f0_sub.extend_from_slice(&r.to_be_bytes());
            f0_sub.extend_from_slice(&v.to_be_bytes());
        }

        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes()); // version
        bytes.extend_from_slice(&0u16.to_be_bytes()); // pad
        bytes.extend_from_slice(&2u32.to_be_bytes()); // nTables = 2
        bytes.extend_from_slice(f4_sub);
        bytes.extend_from_slice(&f0_sub);

        let k = Kerx::parse(&bytes, 256).expect("kerx with mixed fmt4+fmt0 parses");
        assert_eq!(k.subtable_count(), 2);
        assert_eq!(
            k.kern(10, 20),
            -42,
            "format 0 pair survives the format-4 neighbour"
        );
    }

    // -----------------------------------------------------------------
    // Format 6 — simple n×m kerning array.
    // -----------------------------------------------------------------

    /// Builds a one-subtable kerx with format 6, two row classes and
    /// three column classes. `n_glyphs` is the synthetic font's glyph
    /// count. `row_classes[i]` is the row index for gid `i` (0-based);
    /// same for `col_classes`. `matrix[r][c]` is the i16 delta.
    fn build_kerx_format6(
        n_glyphs: u16,
        row_classes: &[u16],
        col_classes: &[u16],
        matrix: &[Vec<i16>],
    ) -> Vec<u8> {
        let row_count = matrix.len() as u16;
        let column_count = matrix[0].len() as u16;

        // Row / column lookup tables (format 0): direct row / column
        // index per glyph.
        let mut row_lookup: Vec<u8> = Vec::new();
        row_lookup.extend_from_slice(&0u16.to_be_bytes()); // format 0
        for &c in row_classes {
            row_lookup.extend_from_slice(&c.to_be_bytes());
        }
        let mut col_lookup: Vec<u8> = Vec::new();
        col_lookup.extend_from_slice(&0u16.to_be_bytes());
        for &c in col_classes {
            col_lookup.extend_from_slice(&c.to_be_bytes());
        }

        // Body layout (relative to subtable start):
        //   0  : 12 B common header
        //   12 : 20 B fmt6 header
        //   32 : row lookup
        //   .. : col lookup
        //   .. : kerning array (row_count × column_count i16s)
        let header_size = 12 + 20;
        let row_off = header_size;
        let col_off = row_off + row_lookup.len();
        let array_off = col_off + col_lookup.len();
        let array_bytes = (row_count as usize) * (column_count as usize) * 2;
        let sub_len = array_off + array_bytes;

        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes()); // version
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes()); // nTables

        // Common subtable header.
        out.extend_from_slice(&(sub_len as u32).to_be_bytes());
        out.extend_from_slice(&6u32.to_be_bytes()); // coverage: format 6
        out.extend_from_slice(&0u32.to_be_bytes()); // tupleCount

        // fmt6 header.
        out.extend_from_slice(&0u32.to_be_bytes()); // flags (no long values)
        out.extend_from_slice(&row_count.to_be_bytes());
        out.extend_from_slice(&column_count.to_be_bytes());
        out.extend_from_slice(&(row_off as u32).to_be_bytes());
        out.extend_from_slice(&(col_off as u32).to_be_bytes());
        out.extend_from_slice(&(array_off as u32).to_be_bytes());

        out.extend_from_slice(&row_lookup);
        out.extend_from_slice(&col_lookup);
        for row in matrix {
            for v in row {
                out.extend_from_slice(&v.to_be_bytes());
            }
        }

        // Sanity: caller's class arrays must cover n_glyphs.
        assert_eq!(row_classes.len(), n_glyphs as usize);
        assert_eq!(col_classes.len(), n_glyphs as usize);
        out
    }

    #[test]
    fn format6_simple_grid_resolves_pairs() {
        // 4-glyph synthetic font:
        //   gid 0 .notdef → row 0, col 0
        //   gid 1 A       → row 1, col 0
        //   gid 2 B       → row 1, col 0
        //   gid 3 V       → row 0, col 1
        // Matrix [row][col]:
        //   [[ 0,   0,   0],
        //    [-30,-50, -70]]
        let bytes = build_kerx_format6(
            4,
            &[0, 1, 1, 0],
            &[0, 0, 0, 1],
            &[vec![0, 0, 0], vec![-30, -50, -70]],
        );
        let k = Kerx::parse(&bytes, 4).unwrap();
        assert_eq!(k.subtable_count(), 1);
        assert_eq!(k.kern(1, 3), -50, "A-V via (row 1, col 1)");
        assert_eq!(k.kern(2, 3), -50, "B-V shares row 1");
        assert_eq!(k.kern(1, 1), -30, "A-A via (row 1, col 0)");
        assert_eq!(k.kern(0, 0), 0, ".notdef pair → row 0 default");
        assert_eq!(k.kern(3, 1), 0, "V-A reversed pair → row 0 default");
    }

    #[test]
    fn format6_rejects_long_values_flag() {
        // Build a valid fmt6, then set the long-values flag bit so the
        // parse path drops the subtable cleanly. The subtable becomes
        // unusable but parse() must still succeed.
        let mut bytes = build_kerx_format6(3, &[0, 1, 1], &[0, 1, 1], &[vec![0, 0], vec![0, 7]]);
        // Subtable starts at offset 8 (kerx header). fmt6 header at
        // offset 8 + 12 = 20; flags u32 lives there.
        bytes[20..24].copy_from_slice(&0x0000_0001u32.to_be_bytes());
        let k = Kerx::parse(&bytes, 3).unwrap();
        assert_eq!(k.subtable_count(), 0, "long-values flag drops the subtable");
    }

    #[test]
    fn format6_bad_offset_silently_drops_subtable() {
        // Clobber the rowIndexTable u32 to point past the subtable —
        // parse must still succeed and skip the subtable.
        let mut bytes = build_kerx_format6(3, &[0, 1, 1], &[0, 1, 1], &[vec![0, 0], vec![0, 7]]);
        // fmt6 header at offset 20; rowIndexTable at +8 = 28.
        let bad = u32::MAX.to_be_bytes();
        bytes[28..32].copy_from_slice(&bad);
        let k = Kerx::parse(&bytes, 3).unwrap();
        assert_eq!(k.subtable_count(), 0);
    }

    #[test]
    fn format6_index_past_grid_returns_zero() {
        // Lookups can yield indices beyond the declared row / column
        // count; the find path must fall through to "no rule = 0"
        // rather than panic on the cell read.
        let bytes = build_kerx_format6(
            3,
            &[0, 99, 1], // gid 1 → row 99 (past row_count=2)
            &[0, 0, 1],
            &[vec![0, 0], vec![0, 7]],
        );
        let k = Kerx::parse(&bytes, 3).unwrap();
        assert_eq!(k.kern(1, 2), 0, "out-of-range row index falls through");
    }

    #[test]
    fn format1_unknown_glyphs_do_not_kern() {
        // Glyph ids that aren't in the class table fall through to
        // the reserved out-of-bounds class, which always lands on
        // entry 0 (noop) in our table.
        let bytes = build_kerx_format1_av_after_letter(1, 2, 3);
        let k = Kerx::parse(&bytes, 8).unwrap();
        let kerns = collect_kerns(&k, &[99, 99, 99]);
        assert!(kerns.is_empty());
    }
}
