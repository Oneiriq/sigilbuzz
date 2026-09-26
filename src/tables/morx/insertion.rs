//! `morx` type 5: glyph insertion subtables.

use alloc::vec::Vec;

use super::{
    class_for, max_steps, FLAG_DONT_ADVANCE, FLAG_INS_CURRENT_BEFORE, FLAG_INS_CURRENT_COUNT_MASK,
    FLAG_INS_CURRENT_COUNT_SHIFT, FLAG_INS_MARKED_BEFORE, FLAG_INS_MARKED_COUNT_MASK,
    FLAG_INS_SET_MARK,
};
use crate::tables::layout::state_table::{StateTableHeader, CLASS_OUT_OF_BOUNDS};

// --- Type 5: Insertion Substitution ---

/// Applies one type-5 (insertion) subtable. State-table entries are
/// 8 bytes each: `(newState, flags, currentInsertIndex, markedInsertIndex)`.
/// Flags carry the SetMark / DontAdvance bits plus before/after
/// orientation flags and the two count fields (5 bits each).
///
/// On an entry whose currentInsertIndex (or markedInsertIndex) is
/// non-`0xFFFF` and whose corresponding count is non-zero, we splice
/// `count` glyphs from the insertion-glyph table at the chosen
/// position, taking care to keep the cursor and origin map in sync.
///
/// The insertion-glyph table is a flat u16 array indexed in units of
/// glyph ids (so byte offset = index * 2).
///
/// Insertions that would grow the run past `max_len` glyphs are
/// dropped.
pub(super) fn apply_insertion(
    state: &StateTableHeader<'_>,
    insertion_table: &[u8],
    glyphs: &mut Vec<u16>,
    origins: &mut Vec<usize>,
    max_len: usize,
) {
    const ENTRY_SIZE: usize = 8;
    let mut cur_state: u16 = 0;
    let mut mark: Option<usize> = None;
    let mut i = 0usize;
    // Bound the walk: every glyph processed at most a handful of
    // times (DontAdvance retries) before we cap, so a malformed font
    // can't loop the shaper.
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
        let cur_index = state
            .entry_tail_u16(entry_idx, ENTRY_SIZE, 4)
            .unwrap_or(0xFFFF);
        let marked_index = state
            .entry_tail_u16(entry_idx, ENTRY_SIZE, 6)
            .unwrap_or(0xFFFF);

        let cur_count =
            ((flags & FLAG_INS_CURRENT_COUNT_MASK) >> FLAG_INS_CURRENT_COUNT_SHIFT) as usize;
        let mark_count = (flags & FLAG_INS_MARKED_COUNT_MASK) as usize;

        // Apply marked insertions first. They sit earlier in the
        // run, so splicing them first leaves the current-position
        // index valid afterwards. When the marked position lands at
        // or before the cursor, we shift the cursor forward by the
        // number of inserted glyphs.
        if marked_index != 0xFFFF && mark_count > 0 {
            if let Some(m) = mark {
                let pos = if flags & FLAG_INS_MARKED_BEFORE != 0 {
                    m
                } else {
                    m + 1
                };
                let n = splice_insertions(
                    insertion_table,
                    marked_index,
                    mark_count,
                    pos,
                    glyphs,
                    origins,
                    max_len,
                );
                if pos <= i {
                    i += n;
                }
            }
        }
        // Current-glyph insertions. After-position inserts leave the
        // cursor on the same glyph (so the next tick advances past
        // both it and the inserted glyphs); before-position inserts
        // push the cursor past the new run so the original glyph is
        // re-processed in the new state.
        if cur_index != 0xFFFF && cur_count > 0 && i <= glyphs.len() {
            let before = flags & FLAG_INS_CURRENT_BEFORE != 0;
            let pos = if before { i } else { i + 1 };
            let n = splice_insertions(
                insertion_table,
                cur_index,
                cur_count,
                pos,
                glyphs,
                origins,
                max_len,
            );
            if before {
                i += n;
            }
        }

        if flags & FLAG_INS_SET_MARK != 0 {
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

/// Reads `count` u16 glyph ids from `insertion_table` at `index`
/// and splices them into `glyphs` / `origins` at `pos`. Returns the
/// number of glyphs actually inserted (zero when `pos` is past the
/// run end, the table doesn't cover the request, or the run would
/// grow past `max_len`).
fn splice_insertions(
    insertion_table: &[u8],
    index: u16,
    count: usize,
    pos: usize,
    glyphs: &mut Vec<u16>,
    origins: &mut Vec<usize>,
    max_len: usize,
) -> usize {
    // `glyphs` and `origins` always have the same length.
    if pos > glyphs.len().min(origins.len()) {
        return 0;
    }
    let inserts = read_insertions(insertion_table, index, count);
    let n = inserts.len();
    if glyphs.len().saturating_add(n) > max_len {
        return 0;
    }
    glyphs.splice(pos..pos, inserts);
    origins.splice(pos..pos, core::iter::repeat(usize::MAX).take(n));
    n
}

/// Reads `count` u16 glyph ids from the insertion-glyph table
/// starting at `index` (units of u16, not bytes). Returns an empty
/// vector if the slice doesn't cover the request. The caller treats
/// that as "no insertion".
fn read_insertions(table: &[u8], index: u16, count: usize) -> Vec<u16> {
    let start = index as usize * 2;
    let end = start + count * 2;
    let Some(slice) = table.get(start..end) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(count);
    for chunk in slice.chunks_exact(2) {
        out.push(u16::from_be_bytes([chunk[0], chunk[1]]));
    }
    out
}
