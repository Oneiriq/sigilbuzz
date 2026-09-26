//! `kerx` format 4: control-point kerning that emits anchor events.

use super::Kerx4Action;
use crate::error::{Error, Result};
use crate::tables::layout::state_table::{
    StateTableHeader, CLASS_END_OF_TEXT, CLASS_OUT_OF_BOUNDS,
};

/// Format 4: control-point kerning. The state machine pushes glyph
/// indices onto a "mark stack" (max depth 1, the most recent push)
/// and entries with a non-`0xFFFF` action index look up an anchor
/// pair in the action table. The pair tells the apply code which
/// point on the marked glyph and the current glyph's outlines should
/// coincide; the resulting (dx, dy) becomes the offset applied to
/// the current glyph's pen position.
///
/// Action type lives in flags bits 30-31:
/// - 0 = control points (u16 pairs into glyf points)
/// - 1 = anchor points (u16 pairs into ankr)
/// - 2 = coordinates    (four i16 in FUnits, inline)
#[derive(Debug, Clone, Copy)]
pub(super) struct Format4<'a> {
    state: StateTableHeader<'a>,
    action_type: u8,
    action_table: &'a [u8],
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
/// Parsing only checks that the action table starts inside the body.
/// Each record read in [`Format4::apply`] is bounds-checked on its
/// own.
pub(super) fn parse_format4(
    data: &[u8],
    sub_start: usize,
    sub_end: usize,
) -> Result<Option<Format4<'_>>> {
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

// --- Format 4 flag bits (per Apple kerx spec) ---
/// Mark the current glyph as the "marked" glyph for the next anchor
/// action.
const FLAG_F4_MARK: u16 = 1 << 15;
/// Don't advance the cursor: re-process the current glyph in the new
/// state.
const FLAG_F4_DONT_ADVANCE: u16 = 1 << 14;
/// Sentinel meaning "this entry has no action".
pub(super) const ACTION_INDEX_NONE: u16 = 0xFFFF;

impl Format4<'_> {
    /// Walks `glyph_ids` through the state machine and emits one
    /// [`Kerx4Action`] event per anchor-action entry. AAT semantics:
    /// each entry can mark the current glyph (storing its run index)
    /// and / or invoke an action; an action looks up `actionIndex`'s
    /// pair in the action table and emits an event referencing both
    /// the previously-marked glyph and the current glyph.
    pub(super) fn apply<F>(&self, glyph_ids: &[u16], emit: &mut F)
    where
        F: FnMut(Kerx4Action),
    {
        // Format 4 entry size: newState + flags + actionIndex = 6 B.
        const ENTRY_SIZE: usize = 6;
        let mut cur_state: u16 = 0;
        let mut marked: Option<usize> = None;
        let mut i = 0usize;
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
            let action_index = self
                .state
                .entry_tail_u16(entry_idx, ENTRY_SIZE, 4)
                .unwrap_or(ACTION_INDEX_NONE);

            // Mark before action so the spec's "self-anchor" idiom
            // (mark + action on the same entry) emits the action with
            // the current glyph as both the mark and the current glyph.
            if flags & FLAG_F4_MARK != 0 && i < glyph_ids.len() {
                marked = Some(i);
            }
            if action_index != ACTION_INDEX_NONE && i < glyph_ids.len() {
                if let Some(mark_index) = marked {
                    self.emit_action(action_index, mark_index, i, emit);
                }
            }

            cur_state = new_state;
            if flags & FLAG_F4_DONT_ADVANCE == 0 {
                i += 1;
            } else if i == glyph_ids.len() {
                return;
            }
        }
    }

    /// Reads a single record from the action table at `action_index`
    /// and emits the corresponding [`Kerx4Action`]. The record shape
    /// depends on the action type stamped in the format-4 flags:
    ///
    /// - 0 (control points): two u16 (`mark_point`, `current_point`).
    /// - 1 (anchor points):  two u16 (`mark_anchor`, `current_anchor`).
    /// - 2 (coordinates):    four i16 (mark x/y, current x/y).
    fn emit_action<F>(
        &self,
        action_index: u16,
        mark_index: usize,
        current_index: usize,
        emit: &mut F,
    ) where
        F: FnMut(Kerx4Action),
    {
        match self.action_type {
            0 | 1 => {
                let off = (action_index as usize).saturating_mul(4);
                let Some(rec) = self.action_table.get(off..off + 4) else {
                    return;
                };
                let a = u16::from_be_bytes([rec[0], rec[1]]);
                let b = u16::from_be_bytes([rec[2], rec[3]]);
                if self.action_type == 0 {
                    emit(Kerx4Action::ControlPoints {
                        mark_index,
                        current_index,
                        mark_point: a,
                        current_point: b,
                    });
                } else {
                    emit(Kerx4Action::AnchorPoints {
                        mark_index,
                        current_index,
                        mark_anchor: a,
                        current_anchor: b,
                    });
                }
            }
            2 => {
                let off = (action_index as usize).saturating_mul(8);
                let Some(rec) = self.action_table.get(off..off + 8) else {
                    return;
                };
                let mark_x = i16::from_be_bytes([rec[0], rec[1]]);
                let mark_y = i16::from_be_bytes([rec[2], rec[3]]);
                let current_x = i16::from_be_bytes([rec[4], rec[5]]);
                let current_y = i16::from_be_bytes([rec[6], rec[7]]);
                emit(Kerx4Action::Coordinates {
                    mark_index,
                    current_index,
                    mark_x,
                    mark_y,
                    current_x,
                    current_y,
                });
            }
            _ => {
                // Reserved action type (3): ignore.
            }
        }
    }
}
