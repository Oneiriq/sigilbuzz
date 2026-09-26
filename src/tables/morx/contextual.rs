//! `morx` type 1: contextual glyph substitution subtables.

use super::{class_for, max_steps, FLAG_CTX_SET_MARK, FLAG_DONT_ADVANCE};
use crate::error::Result;
use crate::tables::layout::state_table::{lookup_class, StateTableHeader, CLASS_OUT_OF_BOUNDS};

// --- Type 1: Contextual glyph substitution ---

pub(super) fn apply_contextual(
    state: &StateTableHeader<'_>,
    substitutions: &[u8],
    glyphs: &mut [u16],
) {
    const ENTRY_SIZE: usize = 8; // newState + flags + markIdx + currentIdx
    let mut cur_state: u16 = 0;
    let mut mark: Option<usize> = None;
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
        let mark_idx = state
            .entry_tail_u16(entry_idx, ENTRY_SIZE, 4)
            .unwrap_or(0xFFFF);
        let cur_idx = state
            .entry_tail_u16(entry_idx, ENTRY_SIZE, 6)
            .unwrap_or(0xFFFF);

        if mark_idx != 0xFFFF {
            if let Some(slot) = mark.and_then(|m| glyphs.get_mut(m)) {
                if let Some(replacement) = sub_lookup(substitutions, mark_idx, *slot) {
                    *slot = replacement;
                }
            }
        }
        if cur_idx != 0xFFFF {
            if let Some(slot) = glyphs.get_mut(i) {
                if let Some(replacement) = sub_lookup(substitutions, cur_idx, *slot) {
                    *slot = replacement;
                }
            }
        }

        if flags & FLAG_CTX_SET_MARK != 0 {
            mark = Some(i);
        }
        cur_state = new_state;
        if flags & FLAG_DONT_ADVANCE == 0 {
            i += 1;
        } else if i == glyphs.len() {
            return;
        }
    }
}

// Each "substitution lookup" referenced by index is itself an AAT
// lookup table; the substitutions blob is a sequence of such tables
// indexed by u32 offsets at its head.
//
// Layout: u16 lookupCount, then u32 offsets[lookupCount] pointing at
// the individual lookups relative to the substitutions blob.
//
// Each lookup maps a glyph to its replacement glyph id directly
// through the shared AAT lookup reader.
fn sub_lookup(substitutions: &[u8], idx: u16, glyph: u16) -> Option<u16> {
    // The substitutions table is laid out as in the type-1 spec:
    // u16 nTables, u32 offsets[nTables] (relative to substitutions
    // blob start). A missing or malformed entry yields None.
    if substitutions.len() < 2 {
        return None;
    }
    let n_tables = u16::from_be_bytes([substitutions[0], substitutions[1]]);
    if idx >= n_tables {
        return None;
    }
    let off_base = 2 + idx as usize * 4;
    if substitutions.len() < off_base + 4 {
        return None;
    }
    let off = u32::from_be_bytes([
        substitutions[off_base],
        substitutions[off_base + 1],
        substitutions[off_base + 2],
        substitutions[off_base + 3],
    ]) as usize;
    let lookup = substitutions.get(off..)?;
    // Reuse the class-lookup machinery: class value == replacement
    // glyph id; out-of-bounds yields the reserved class, which we
    // map back to None so the caller knows not to substitute.
    let Ok(replacement) = lookup_value(lookup, glyph) else {
        return None;
    };
    if replacement == CLASS_OUT_OF_BOUNDS {
        None
    } else {
        Some(replacement)
    }
}

/// Resolves `glyph` through the AAT lookup table at the start of
/// `data`. Passes a glyph count of zero, so a format-0 lookup covers
/// as many glyphs as the slice holds, the same rule
/// [`StateTableHeader::class_of`] uses. Reads the lookup in place,
/// without copying it.
pub(super) fn lookup_value(data: &[u8], glyph: u16) -> Result<u16> {
    lookup_class(data, glyph, 0)
}
