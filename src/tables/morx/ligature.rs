//! `morx` type 2: ligature substitution subtables.

use alloc::vec::Vec;

use super::{
    class_for, max_steps, FLAG_DONT_ADVANCE, FLAG_LIG_PERFORM_ACTION, FLAG_LIG_SET_COMPONENT,
    LIG_ACTION_LAST, LIG_ACTION_OFFSET_MASK, LIG_ACTION_OFFSET_SIGN, LIG_ACTION_STORE,
};
use crate::tables::layout::state_table::{StateTableHeader, CLASS_OUT_OF_BOUNDS};

// --- Type 2: Ligature substitution ---

pub(super) fn apply_ligature(
    state: &StateTableHeader<'_>,
    lig_actions: &[u8],
    components: &[u8],
    ligatures: &[u8],
    glyphs: &mut Vec<u16>,
    origins: &mut Vec<usize>,
) {
    const ENTRY_SIZE: usize = 6; // newState + flags + actionIndex
    let mut cur_state: u16 = 0;
    let mut component_stack: Vec<usize> = Vec::new();
    let mut i = 0;
    // The step cap also bounds the component stack, since each step
    // pushes at most one entry.
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

        if flags & FLAG_LIG_SET_COMPONENT != 0 && i < glyphs.len() {
            component_stack.push(i);
        }
        if flags & FLAG_LIG_PERFORM_ACTION != 0 && !component_stack.is_empty() {
            perform_ligature_action(
                action_idx,
                lig_actions,
                components,
                ligatures,
                &mut component_stack,
                glyphs,
                origins,
                &mut i,
            );
        }

        cur_state = new_state;
        if flags & FLAG_DONT_ADVANCE == 0 {
            i += 1;
        } else if i == glyphs.len() {
            return;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn perform_ligature_action(
    action_idx: u16,
    lig_actions: &[u8],
    components: &[u8],
    ligatures: &[u8],
    stack: &mut Vec<usize>,
    glyphs: &mut Vec<u16>,
    origins: &mut Vec<usize>,
    cursor: &mut usize,
) {
    // Walk action entries starting at `action_idx`, summing
    // component-table lookups into a component-index offset. The
    // `LAST` bit ends the walk; on the final action, if `STORE` is
    // set, the offset is used to index the ligature table and emit
    // the resulting glyph id.
    let mut offset: i32 = 0;
    let mut action_pos = action_idx as usize;
    let mut consumed: Vec<usize> = Vec::new();
    loop {
        let Some(stack_top) = stack.pop() else {
            return;
        };
        consumed.push(stack_top);

        let Some(action) = u32_at(lig_actions, action_pos) else {
            return;
        };

        let raw_off = action & LIG_ACTION_OFFSET_MASK;
        // Sign-extend from the 30-bit signed offset field to i32. The
        // `as` casts wrap on purpose: that is the conversion.
        let signed_off: i32 = if action & LIG_ACTION_OFFSET_SIGN != 0 {
            (raw_off | 0xC000_0000) as i32
        } else {
            raw_off as i32
        };
        // An earlier ligature in this walk removes glyphs but leaves
        // the stack as is, so a stack entry can point past the run.
        // Stop the action instead of reading out of bounds.
        let Some(&glyph) = glyphs.get(stack_top) else {
            return;
        };
        // Cannot overflow: the glyph is at most 0xFFFF and the offset
        // is a 30-bit signed value.
        let comp_idx = i32::from(glyph) + signed_off;
        // A negative index points before the component table. Treat
        // it like any other out-of-range read.
        let Some(comp_val) = usize::try_from(comp_idx)
            .ok()
            .and_then(|idx| u16_at(components, idx))
        else {
            return;
        };
        offset = offset.wrapping_add(i32::from(comp_val));

        if action & LIG_ACTION_LAST != 0 {
            if action & LIG_ACTION_STORE != 0 {
                let lig_glyph = usize::try_from(offset)
                    .ok()
                    .and_then(|idx| u16_at(ligatures, idx));
                if let Some(lig_glyph) = lig_glyph {
                    // Replace the earliest consumed slot with the
                    // ligature, drop the later slots. Sort in
                    // ascending order so the earliest index lands
                    // first. Stack was LIFO so the natural order is
                    // reversed.
                    consumed.sort_unstable();
                    let Some((&keep, rest)) = consumed.split_first() else {
                        return;
                    };
                    if let Some(slot) = glyphs.get_mut(keep) {
                        *slot = lig_glyph;
                    }
                    // origins[keep] keeps the smallest originating
                    // input index so cluster merging finds the
                    // correct grapheme root.
                    // Remove every other consumed slot, highest index
                    // first so earlier indices stay valid. A glyph
                    // pushed twice shows up twice here, so an earlier
                    // removal can shorten the run past a later index.
                    // Skip those.
                    for &idx in rest.iter().rev() {
                        if idx >= glyphs.len().min(origins.len()) {
                            continue;
                        }
                        glyphs.remove(idx);
                        origins.remove(idx);
                        if idx < *cursor {
                            *cursor -= 1;
                        }
                    }
                }
            }
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
