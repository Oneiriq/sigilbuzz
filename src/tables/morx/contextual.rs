//! `morx` type 1: contextual glyph substitution subtables.

use super::{class_for, max_steps, FLAG_CTX_SET_MARK, FLAG_DONT_ADVANCE};
use crate::error::Result;
use crate::tables::layout::state_table::{self, StateTableHeader, CLASS_OUT_OF_BOUNDS};

// --- Type 1: Contextual glyph substitution ---

/// Runs a contextual substitution subtable over `glyphs` the way
/// HarfBuzz's `ContextualSubtable` does. Each entry may substitute the
/// marked glyph and the current glyph through the lookups its indices
/// name. The mark starts on the first glyph, so a mark substitution
/// before any SetMark replaces glyph 0. At the end of text nothing is
/// substituted unless a mark was set, and the current substitution then
/// applies to the last glyph.
pub(super) fn apply_contextual(
    state: &StateTableHeader<'_>,
    substitutions: &[u8],
    glyphs: &mut [u16],
) {
    const ENTRY_SIZE: usize = 8; // newState + flags + markIdx + currentIdx
    let mut cur_state: u16 = 0;
    let mut mark: usize = 0;
    let mut mark_set = false;
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

        // CoreText applies neither substitution at the end of text
        // unless a mark was set, and HarfBuzz follows it.
        if i < glyphs.len() || mark_set {
            if mark_idx != 0xFFFF {
                if let Some(slot) = glyphs.get_mut(mark) {
                    if let Some(replacement) = sub_lookup(substitutions, mark_idx, *slot) {
                        *slot = replacement;
                    }
                }
            }
            if cur_idx != 0xFFFF {
                // At the end of text the current glyph is the last one.
                let at = i.min(glyphs.len().saturating_sub(1));
                if let Some(slot) = glyphs.get_mut(at) {
                    if let Some(replacement) = sub_lookup(substitutions, cur_idx, *slot) {
                        *slot = replacement;
                    }
                }
            }
            if flags & FLAG_CTX_SET_MARK != 0 {
                mark_set = true;
                mark = i;
            }
        }
        cur_state = new_state;
        if flags & FLAG_DONT_ADVANCE == 0 {
            i += 1;
        } else if i == glyphs.len() {
            return;
        }
    }
}

// The substitution table is an unsized array of u32 offsets, one per
// lookup, each from the start of the table, as in the spec and
// HarfBuzz's `UnsizedListOfOffset16To<Lookup<HBGlyphID16>, HBUINT32>`.
// Entries name lookups by index; the table itself does not say how
// many there are.
//
// Each lookup maps a glyph to its replacement glyph id directly
// through the shared AAT lookup reader.
fn sub_lookup(substitutions: &[u8], idx: u16, glyph: u16) -> Option<u16> {
    let at = usize::from(idx) * 4;
    let off = u32::from_be_bytes(*substitutions.get(at..)?.first_chunk::<4>()?) as usize;
    lookup_value(substitutions.get(off..)?, glyph)
        .ok()
        .flatten()
}

/// Resolves `glyph` through the AAT lookup table at the start of
/// `data`: its replacement, or `None` when the lookup does not cover
/// it. Passes a glyph count of zero, so a format-0 lookup covers as
/// many glyphs as the slice holds, the same rule
/// [`StateTableHeader::class_of`] uses. Reads the lookup in place,
/// without copying it.
pub(super) fn lookup_value(data: &[u8], glyph: u16) -> Result<Option<u16>> {
    state_table::lookup_value(data, glyph, 0)
}
