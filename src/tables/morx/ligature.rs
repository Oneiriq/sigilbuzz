//! `morx` type 2: ligature substitution subtables.

use alloc::vec::Vec;

use super::{
    class_for, FLAG_DONT_ADVANCE, FLAG_LIG_PERFORM_ACTION, FLAG_LIG_SET_COMPONENT, LIG_ACTION_LAST,
    LIG_ACTION_OFFSET_MASK, LIG_ACTION_OFFSET_SIGN, LIG_ACTION_STORE,
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
    while i <= glyphs.len() {
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
        if stack.is_empty() {
            return;
        }
        let stack_top = stack.pop().unwrap();
        consumed.push(stack_top);

        let action_off = action_pos * 4;
        let Some(action_bytes) = lig_actions.get(action_off..action_off + 4) else {
            return;
        };
        let action = u32::from_be_bytes([
            action_bytes[0],
            action_bytes[1],
            action_bytes[2],
            action_bytes[3],
        ]);

        let raw_off = action & LIG_ACTION_OFFSET_MASK;
        // Sign-extend from the 30-bit signed offset field to i32. Do
        // the arithmetic with two's-complement-safe casts so clippy's
        // cast_possible_wrap stays happy. We actively want the wrap,
        // that is the point of the conversion.
        let signed_off: i32 = if action & LIG_ACTION_OFFSET_SIGN != 0 {
            #[allow(clippy::cast_possible_wrap)]
            {
                (raw_off | 0xC000_0000) as i32
            }
        } else {
            #[allow(clippy::cast_possible_wrap)]
            {
                raw_off as i32
            }
        };
        let glyph_id = i32::from(glyphs[stack_top]);
        let comp_idx = glyph_id + signed_off;
        let comp_byte_off = (comp_idx as usize).saturating_mul(2);
        let Some(comp_bytes) = components.get(comp_byte_off..comp_byte_off + 2) else {
            return;
        };
        let comp_val = i32::from(u16::from_be_bytes([comp_bytes[0], comp_bytes[1]]));
        offset = offset.wrapping_add(comp_val);

        if action & LIG_ACTION_LAST != 0 {
            if action & LIG_ACTION_STORE != 0 {
                let lig_byte_off = (offset as usize).saturating_mul(2);
                if let Some(lig_bytes) = ligatures.get(lig_byte_off..lig_byte_off + 2) {
                    let lig_glyph = u16::from_be_bytes([lig_bytes[0], lig_bytes[1]]);
                    // Replace the earliest consumed slot with the
                    // ligature, drop the later slots. Sort in
                    // ascending order so the earliest index lands
                    // first. Stack was LIFO so the natural order is
                    // reversed.
                    consumed.sort_unstable();
                    let keep = consumed[0];
                    glyphs[keep] = lig_glyph;
                    // origins[keep] keeps the smallest originating
                    // input index so cluster merging finds the
                    // correct grapheme root.
                    // Remove every other consumed slot, highest index
                    // first so earlier indices stay valid.
                    for &idx in consumed.iter().skip(1).rev() {
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
