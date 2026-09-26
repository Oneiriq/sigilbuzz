//! `kerx` format 1: state-machine kerning driven by a kern stack.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::layout::state_table::{
    StateTableHeader, CLASS_END_OF_TEXT, CLASS_OUT_OF_BOUNDS,
};

/// Format 1: state-machine kerning. Wraps the AAT extended state
/// table primitive plus a value-table slice; the apply pass walks
/// the glyph stream through the state machine, pushing glyphs onto
/// a kern stack on each `PUSH` entry and popping + applying values
/// from the value table whenever an entry references a non-empty
/// value list.
#[derive(Debug, Clone, Copy)]
pub(super) struct Format1<'a> {
    state: StateTableHeader<'a>,
    /// Slice of the format-1 subtable body that begins at the value
    /// table's origin. Value lists are i16 arrays terminated by an
    /// entry with bit 0 set; this slice provides the bytes those
    /// lists index into.
    value_table: &'a [u8],
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
pub(super) fn parse_format1(
    data: &[u8],
    sub_start: usize,
    sub_end: usize,
) -> Result<Option<Format1<'_>>> {
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

// --- Format 1 flag bits (per Apple kerx spec) ---
/// Push the current glyph onto the kern stack.
const FLAG_F1_PUSH: u16 = 1 << 15;
/// Don't advance the cursor (re-process the current glyph in the
/// new state).
const FLAG_F1_DONT_ADVANCE: u16 = 1 << 14;
/// Reset the cross-stream kerning state. We honor the flag by
/// clearing the kern stack so a stale push cannot leak into the next
/// run. sigilbuzz does not yet emit cross-stream offsets so the
/// stricter cross-stream resync isn't needed.
const FLAG_F1_RESET: u16 = 1 << 13;
/// Sentinel meaning "this entry has no value list".
pub(super) const VALUE_INDEX_NONE: u16 = 0xFFFF;
/// Maximum kern stack depth. Apple's documented depth is eight
/// (we mirror that to bound memory on malformed fonts).
const KERN_STACK_MAX: usize = 8;

impl Format1<'_> {
    /// Walks `glyph_ids` through the state machine, invoking `apply`
    /// with each `(target_index, kern_delta)` the value lists emit.
    /// On any malformed read the walk bails cleanly (partial output
    /// is allowed but never panics), so a font with a corrupt format
    /// 1 subtable still positions whatever pairs the apply loop did
    /// reach.
    pub(super) fn apply<F>(&self, glyph_ids: &[u16], apply: &mut F)
    where
        F: FnMut(usize, i16),
    {
        // newState + flags + valueIndex
        const ENTRY_SIZE: usize = 6;
        // Kern stack: indices into `glyph_ids` of glyphs awaiting a
        // value-list pop. AAT semantics says new pushes go on top
        // and the next value list pops them in reverse (last pushed,
        // first applied), pairing each value with the matching glyph.
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
            // Mask out bit 0: the spec uses it as a list terminator
            // but the actual kern delta is the masked value.
            let value = raw & !1i16;
            if let Some(idx) = stack.pop() {
                if value != 0 {
                    apply(idx, value);
                }
            } else {
                // No glyph to apply against. Break to avoid walking
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
