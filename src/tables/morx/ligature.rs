//! `morx` type 2: ligature substitution subtables.

use alloc::vec::Vec;

use super::{
    class_for, max_steps, DELETED_GLYPH, FLAG_DONT_ADVANCE, FLAG_LIG_PERFORM_ACTION,
    FLAG_LIG_SET_COMPONENT, LIG_ACTION_LAST, LIG_ACTION_OFFSET_MASK, LIG_ACTION_OFFSET_SIGN,
    LIG_ACTION_STORE,
};
use crate::tables::layout::state_table::{StateTableHeader, CLASS_OUT_OF_BOUNDS};

/// Component positions the stack holds, HarfBuzz's
/// `HB_MAX_CONTEXT_LENGTH`. Like HarfBuzz's `match_positions`, the
/// stack is a ring: a push past this many overwrites the oldest entry.
const STACK_SIZE: usize = 64;

/// The component stack of a ligature subtable pass: glyph positions in
/// push order, kept as HarfBuzz keeps `match_positions` and
/// `match_length`.
struct ComponentStack {
    positions: [usize; STACK_SIZE],
    /// Pushes not yet consumed. Can pass [`STACK_SIZE`]; positions
    /// wrap.
    len: usize,
}

impl ComponentStack {
    fn at(&self, index: usize) -> usize {
        self.positions[index % STACK_SIZE]
    }

    /// Pushes position `i`, unless it is already on top: an entry with
    /// DontAdvance can set the same glyph as a component again, and
    /// HarfBuzz never marks one index twice.
    fn push(&mut self, i: usize) {
        if self.len > 0 && self.at(self.len - 1) == i {
            self.len -= 1;
        }
        self.positions[self.len % STACK_SIZE] = i;
        self.len += 1;
    }
}

// --- Type 2: Ligature substitution ---

/// Runs a ligature subtable over `glyphs` the way HarfBuzz's
/// `LigatureSubtable` does. Components that a ligature absorbs become
/// [`DELETED_GLYPH`] in place, so positions stay put for the rest of
/// the pass; [`super::Morx::apply`] removes them after the last chain,
/// as HarfBuzz removes deleted glyphs after `morx`.
pub(super) fn apply_ligature(
    state: &StateTableHeader<'_>,
    lig_actions: &[u8],
    components: &[u8],
    ligatures: &[u8],
    glyphs: &mut [u16],
    origins: &mut [usize],
) {
    const ENTRY_SIZE: usize = 6; // newState + flags + actionIndex
    let mut cur_state: u16 = 0;
    let mut stack = ComponentStack {
        positions: [0; STACK_SIZE],
        len: 0,
    };
    let mut i = 0;
    let max_iters = max_steps(glyphs.len());
    let mut iters = 0usize;
    while i <= glyphs.len() {
        iters += 1;
        if iters > max_iters {
            return;
        }
        let class = class_for(state, glyphs.get(i).copied()).unwrap_or(CLASS_OUT_OF_BOUNDS);
        let Ok(entry_idx) = state.entry_index(cur_state, class) else {
            return;
        };
        let Ok((new_state, flags)) = state.entry_prefix(entry_idx, ENTRY_SIZE) else {
            return;
        };
        let action_idx = state.entry_tail_u16(entry_idx, ENTRY_SIZE, 4).unwrap_or(0);

        if flags & FLAG_LIG_SET_COMPONENT != 0 {
            stack.push(i);
        }
        // HarfBuzz performs no action with an empty stack or at the end
        // of text.
        if flags & FLAG_LIG_PERFORM_ACTION != 0 && stack.len > 0 && i < glyphs.len() {
            let tables = ActionTables {
                lig_actions,
                components,
                ligatures,
            };
            perform_ligature_action(action_idx, &tables, &mut stack, glyphs, origins);
        }

        cur_state = new_state;
        if flags & FLAG_DONT_ADVANCE == 0 {
            i += 1;
        } else if i == glyphs.len() {
            return;
        }
    }
}

/// The three arrays a ligature action reads.
struct ActionTables<'t> {
    lig_actions: &'t [u8],
    components: &'t [u8],
    ligatures: &'t [u8],
}

/// Runs the action list that starts at `action_idx`, as HarfBuzz's
/// `LigatureSubtable::driver_context_t::transition` does. Each action
/// pops a component off the stack and adds `components[glyph + offset]`
/// to the ligature index. An action with Store or Last puts
/// `ligatures[index]` in place of the component it popped, deletes the
/// components popped since the last store, and leaves the ligature on
/// the stack for later actions. The index carries on across stores.
/// Running out of components clears the stack; reading past an array
/// ends the list.
fn perform_ligature_action(
    action_idx: u16,
    tables: &ActionTables<'_>,
    stack: &mut ComponentStack,
    glyphs: &mut [u16],
    origins: &mut [usize],
) {
    let mut cursor = stack.len;
    let mut action_pos = usize::from(action_idx);
    let mut ligature_idx: u32 = 0;
    loop {
        if cursor == 0 {
            // Stack underflow: clear the stack.
            stack.len = 0;
            return;
        }
        cursor -= 1;
        let pos = stack.at(cursor);
        let Some(&glyph) = glyphs.get(pos) else {
            return;
        };
        let Some(action) = u32_at(tables.lig_actions, action_pos) else {
            return;
        };
        let raw_off = action & LIG_ACTION_OFFSET_MASK;
        // Sign-extend the 30-bit offset. HarfBuzz adds it to the glyph
        // id in unsigned arithmetic, so a negative sum wraps far past
        // the end of the component array.
        let offset = if raw_off & LIG_ACTION_OFFSET_SIGN != 0 {
            raw_off | 0xC000_0000
        } else {
            raw_off
        };
        let component_idx = u32::from(glyph).wrapping_add(offset);
        let Some(component) = usize::try_from(component_idx)
            .ok()
            .and_then(|idx| u16_at(tables.components, idx))
        else {
            return;
        };
        ligature_idx = ligature_idx.wrapping_add(u32::from(component));

        if action & (LIG_ACTION_STORE | LIG_ACTION_LAST) != 0 {
            let Some(lig) = usize::try_from(ligature_idx)
                .ok()
                .and_then(|idx| u16_at(tables.ligatures, idx))
            else {
                return;
            };
            glyphs[pos] = lig;
            let lig_end = stack.at(stack.len - 1) + 1;
            // Delete the components popped since the last store.
            while stack.len - 1 > cursor {
                stack.len -= 1;
                let Some(slot) = glyphs.get_mut(stack.at(stack.len)) else {
                    return;
                };
                *slot = DELETED_GLYPH;
            }
            // HarfBuzz merges the clusters from the ligature to the last
            // component into one; the ligature takes the earliest
            // origin among them.
            if let Some(span) = origins.get(pos..lig_end.min(origins.len())) {
                if let Some(&first) = span.iter().min() {
                    origins[pos] = first;
                }
            }
        }
        if action & LIG_ACTION_LAST != 0 {
            return;
        }
        action_pos += 1;
    }
}

/// Reads element `index` of a big-endian u16 array stored in `data`.
fn u16_at(data: &[u8], index: usize) -> Option<u16> {
    let start = index.checked_mul(2)?;
    let bytes = data.get(start..)?.first_chunk::<2>()?;
    Some(u16::from_be_bytes(*bytes))
}

/// Reads element `index` of a big-endian u32 array stored in `data`.
fn u32_at(data: &[u8], index: usize) -> Option<u32> {
    let start = index.checked_mul(4)?;
    let bytes = data.get(start..)?.first_chunk::<4>()?;
    Some(u32::from_be_bytes(*bytes))
}

/// Removes every [`DELETED_GLYPH`] and its origin.
pub(super) fn remove_deleted(glyphs: &mut Vec<u16>, origins: &mut Vec<usize>) {
    if !glyphs.contains(&DELETED_GLYPH) {
        return;
    }
    let mut keep = glyphs.iter().map(|&g| g != DELETED_GLYPH);
    origins.retain(|_| keep.next().unwrap_or(true));
    glyphs.retain(|&g| g != DELETED_GLYPH);
}
