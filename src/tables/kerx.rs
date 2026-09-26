//! `kerx`: Apple Extended Kerning.
//!
//! `kerx` is the AAT successor to `kern`. AAT-only fonts ship their
//! kerning here. sigilbuzz consults `kerx` only when GPOS has no
//! `kern` feature, so modern OpenType fonts keep their existing
//! behavior. This matches HarfBuzz's AAT shaper policy.
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
//!     u32 tupleCount   (variation-font kerning, sigilbuzz ignores)
//!     Body body        (format-specific)
//! ```
//!
//! Three subtable formats are implemented:
//!
//! - Format 0: ordered pair list (the common case for AAT fonts
//!   that re-use legacy `kern` data).
//! - Format 1: state-machine kerning. Walks the run through an AAT
//!   extended state table; entries push glyph indices onto a "kern
//!   stack" and reference a list of i16 values that are popped and
//!   applied in pair order. Useful for contextual kerning (e.g. a
//!   spur joining only when not preceded by a space).
//! - Format 2: n-way class kerning. Two AAT lookup tables map
//!   left and right glyph ids to row / column offsets into a 2D
//!   array of i16 deltas; useful for dense matrices like Latin
//!   pair-class tables that would explode if expanded to flat
//!   pairs.
//!
//! Format 4: control-point kerning. The state machine walks the run
//! marking glyphs and firing actions; each action looks up an anchor
//! pair in the action table. Three action types exist:
//!
//! - **Type 0** (control points): pairs of glyf-point indices. The
//!   apply pass needs to read the (x, y) of point N on each glyph.
//!   sigilbuzz emits a [`Kerx4Action::ControlPoints`] event so the
//!   caller can resolve the points; the shaper integration drops
//!   these events until [`crate::Face`] grows a public glyph-point
//!   accessor (follow-up).
//! - **Type 1** (anchor points): pairs of `ankr` table indices.
//!   sigilbuzz emits the event but the shaper drops it; `ankr`
//!   support is on a separate track.
//! - **Type 2** (coordinates): four i16 in FUnits per record. The
//!   shaper applies `(mark_x - current_x, mark_y - current_y)` as
//!   x/y offsets directly. No glyf / ankr reads needed.
//!
//! The state-machine walk + event emission is driven by
//! [`Kerx::apply_format4`].
//!
//! # Format 6
//!
//! Apple's "simple n x m array": a compound-class layout like format
//! 2, but the row / column class tables are AAT lookups that yield
//! direct row / column *indices* into a 2D `(rowCount * columnCount)`
//! grid of i16 kern deltas. Layout (relative to the subtable origin):
//!
//! ```text
//!   u32 flags             (bit 0 = "long values"; sigilbuzz only
//!                          implements the short-value variant)
//!   u16 rowCount
//!   u16 columnCount
//!   u32 rowIndexTable     (offset to AAT lookup; glyph -> row index)
//!   u32 columnIndexTable  (offset to AAT lookup; glyph -> col index)
//!   u32 kerningArray      (offset to i16[rowCount * columnCount])
//!   u32 kerningVector     (long-value variant only, ignored)
//! ```
//!
//! Real shipping fonts pick small row / column counts (a few dozen)
//! and store the indices as plain u16, the same shape format 2 uses
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
//! lookup is a binary search, exactly as in the legacy `kern`
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
//! `flags` carries `PUSH` (bit 15: push the current glyph onto the
//! kern stack), `DONT_ADVANCE` (bit 14: re-process the current glyph
//! after switching state) and `RESET` (bit 13: clear the stack;
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
//! (the same origin Apple's spec uses).

mod format0;
mod format1;
mod format2;
mod format4;
mod format6;

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

use format0::{parse_format0, Format0};
use format1::{parse_format1, Format1};
use format2::{parse_format2, Format2};
use format4::{parse_format4, Format4};
use format6::{parse_format6, Format6};

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
    Format4(Format4<'a>),
    Format6(Format6<'a>),
}

/// One control-point apply event emitted by [`Kerx::apply_format4`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kerx4Action {
    /// Action type 0: control points. Look up point `mark_point` on
    /// the glyph at `mark_index`, point `current_point` on the glyph
    /// at `current_index`, and apply the FUnit delta `mark - current`
    /// as an offset to the current glyph.
    ControlPoints {
        /// Index into the run of the previously-marked glyph.
        mark_index: usize,
        /// Index into the run of the glyph the offset is applied to.
        current_index: usize,
        /// Point id on the marked glyph.
        mark_point: u16,
        /// Point id on the current glyph.
        current_point: u16,
    },
    /// Action type 1: anchor points (`ankr` table). sigilbuzz does
    /// not yet parse `ankr`; the consumer should treat this as a
    /// no-op until `ankr` lands.
    AnchorPoints {
        /// Index into the run of the previously-marked glyph.
        mark_index: usize,
        /// Index into the run of the glyph the offset is applied to.
        current_index: usize,
        /// `ankr` lookup index for the marked glyph.
        mark_anchor: u16,
        /// `ankr` lookup index for the current glyph.
        current_anchor: u16,
    },
    /// Action type 2: inline coordinates. Pre-computed FUnit deltas
    /// (`mark_x`, `mark_y`, `current_x`, `current_y`); apply
    /// `(mark_x - current_x, mark_y - current_y)` to the current glyph.
    Coordinates {
        /// Index into the run of the previously-marked glyph.
        mark_index: usize,
        /// Index into the run of the glyph the offset is applied to.
        current_index: usize,
        /// Marked glyph anchor x in FUnits.
        mark_x: i16,
        /// Marked glyph anchor y in FUnits.
        mark_y: i16,
        /// Current glyph anchor x in FUnits.
        current_x: i16,
        /// Current glyph anchor y in FUnits.
        current_y: i16,
    },
}

impl<'a> Kerx<'a> {
    /// Parses a `kerx` table. Returns [`Error::Unsupported`] for
    /// versions outside {2, 3}. Every AAT font sigilbuzz targets
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
            // subtable as "skip cleanly past the header": drop it
            // and use `sub_start + 12` as the cursor target. The
            // post-header portion (whatever the malformed `length`
            // claimed) is effectively unused.
            let next_cursor = if length < 12 { sub_start + 12 } else { sub_end };

            let format = (coverage & COVERAGE_FORMAT_MASK) as u8;
            // Skip vertical, cross-stream, and variation subtables:
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
            // pair matrix. Format 6 is the n x m simple grid. Format 4
            // (control-point anchors) is parsed for structure but its
            // apply path is a stub. See [`Format4`] for details.
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
    /// is not consulted here. Callers wanting full kerx coverage
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
                // Format 1 is the state machine, applied separately.
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

    /// Pair-kerning delta for `(left, right)` in the `index`-th parsed
    /// subtable: `Some(delta)` (zero when the pair is absent) for a
    /// pair subtable (format 0, 2 or 6), `None` for a state-machine or
    /// control-point subtable or an index past the end. HarfBuzz runs
    /// its pair kerning one subtable at a time, splitting each value
    /// across the pair separately, which is what this supports.
    #[must_use]
    pub fn subtable_pair_kern(&self, index: usize, left: u16, right: u16) -> Option<i16> {
        let v = match self.subtables.get(index)? {
            Subtable::Format0(f0) => f0.find((u32::from(left) << 16) | u32::from(right)),
            Subtable::Format2(f2) => f2.find(left, right, self.num_glyphs),
            Subtable::Format6(f6) => f6.find(left, right, self.num_glyphs),
            Subtable::Format1(_) | Subtable::Format4(_) => return None,
        };
        Some(v.unwrap_or(0))
    }

    /// Walks every format-1 (state-machine) subtable across the run,
    /// applying each value-list pop directly to the targeted glyph's
    /// `x_advance`. Formats 0 / 2 are pair-only and are handled by
    /// [`Kerx::kern`]. This method only drives the stateful subtables.
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

    /// Walks every format-4 (control-point) subtable across the run,
    /// emitting a [`Kerx4Action`] event for each anchor pair the state
    /// machine fires. The caller is responsible for resolving the
    /// referenced points to FUnit coords (via `glyf` for action type
    /// 0, the `ankr` table for type 1) and applying the resulting
    /// offset to the current glyph's pen position.
    ///
    /// Action type 2 (inline coordinates) is fully self-contained:
    /// the consumer can apply `(mark_x - current_x, mark_y -
    /// current_y)` directly without a second table lookup.
    pub fn apply_format4<F>(&self, glyph_ids: &[u16], mut emit: F)
    where
        F: FnMut(Kerx4Action),
    {
        for sub in &self.subtables {
            if let Subtable::Format4(f4) = sub {
                f4.apply(glyph_ids, &mut emit);
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

    /// True iff this `kerx` carries at least one format-4
    /// (control-point) subtable. Callers can short-circuit the
    /// [`Kerx::apply_format4`] walk when no fmt-4 is present.
    #[must_use]
    pub fn has_format4(&self) -> bool {
        self.subtables
            .iter()
            .any(|s| matches!(s, Subtable::Format4(_)))
    }

    /// Number of parsed subtables (any format). Useful in tests to
    /// assert which subtables were retained.
    #[must_use]
    pub fn subtable_count(&self) -> usize {
        self.subtables.len()
    }
}

#[cfg(test)]
mod tests;
