//! `morx` type 0: rearrangement subtables.

use super::{
    class_for, FLAG_DONT_ADVANCE, FLAG_MARK_FIRST, FLAG_MARK_LAST, FLAG_REARRANGE_VERB_MASK,
};
use crate::tables::layout::state_table::{StateTableHeader, CLASS_OUT_OF_BOUNDS};

// --- Type 0: Rearrangement ---

pub(super) fn apply_rearrangement(
    state: &StateTableHeader<'_>,
    glyphs: &mut [u16],
    origins: &mut [usize],
) {
    let mut cur_state: u16 = 0;
    let mut i = 0;
    let mut first: Option<usize> = None;
    let mut last: Option<usize> = None;
    // Iterate through the run, with an extra end-of-text step so a
    // state carrying a pending mark gets one more chance to fire.
    while i <= glyphs.len() {
        let class = class_for(state, glyphs.get(i).copied()).unwrap_or(CLASS_OUT_OF_BOUNDS);
        let Ok(entry_idx) = state.entry_index(cur_state, class) else {
            return;
        };
        let Ok((new_state, flags)) = state.entry_prefix(entry_idx, 4) else {
            return;
        };
        if flags & FLAG_MARK_FIRST != 0 {
            first = Some(i);
        }
        if flags & FLAG_MARK_LAST != 0 {
            last = Some(i);
        }
        let verb = flags & FLAG_REARRANGE_VERB_MASK;
        if verb != 0 {
            if let (Some(a), Some(b)) = (first, last) {
                if a <= b && b < glyphs.len() {
                    rearrange(verb, glyphs, origins, a, b);
                }
            }
        }
        cur_state = new_state;
        if flags & FLAG_DONT_ADVANCE == 0 {
            i += 1;
        } else if i == glyphs.len() {
            // End-of-text + DontAdvance would loop forever; bail.
            return;
        }
    }
}

// Rearrangement verbs: standard AAT table of 16 permutations on a
// window described by (A = first, B = first+1, C?, D = last-1, E = last).
// Only the verbs sigilbuzz is likely to see (1 = "Ax -> xA" and
// related swaps) are implemented; unknown verbs are a no-op so an
// unsupported rearrangement can't corrupt the glyph stream.
fn rearrange(verb: u16, glyphs: &mut [u16], origins: &mut [usize], first: usize, last: usize) {
    let len = last - first + 1;
    if len < 2 {
        return;
    }
    // Rearrangement verbs 1 / 2 / 3 all reduce to the same single
    // swap in sigilbuzz's two-element window coverage, a
    // conservative subset. Rarer verbs (4..=15) handle 3- to
    // 5-element windows and stay no-op until a real font needs them,
    // because producing a wrong permutation would corrupt the glyph
    // stream worse than leaving it alone.
    let _ = len;
    if let 1..=3 = verb {
        glyphs.swap(first, last);
        origins.swap(first, last);
    }
}
