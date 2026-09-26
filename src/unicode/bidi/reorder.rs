//! After the implicit levels: the levels of the characters rule X9
//! removed, rule L1, and the L2 reversal.

use alloc::vec::Vec;

use super::{BidiCell, BidiClass};

/// True for the classes rule X9 removes (RLE, LRE, RLO, LRO, PDF,
/// BN).
pub(super) const fn is_x9_removed(cls: BidiClass) -> bool {
    matches!(
        cls,
        BidiClass::Rle
            | BidiClass::Lre
            | BidiClass::Rlo
            | BidiClass::Lro
            | BidiClass::Pdf
            | BidiClass::Bn
    )
}

/// True for the characters rule L1 resets when they run up to a
/// separator or the end of a line: whitespace, the isolate formatting
/// characters, and (UAX #9 section 5.2, retaining explicit formatting
/// characters) the characters X9 removed.
pub(crate) const fn is_l1_trailing(cls: BidiClass) -> bool {
    matches!(
        cls,
        BidiClass::Ws | BidiClass::Fsi | BidiClass::Lri | BidiClass::Rli | BidiClass::Pdi
    ) || is_x9_removed(cls)
}

/// Gives every character X9 removed the level of the character before
/// it, or the paragraph level at the start of the text.
///
/// UAX #9 leaves those levels unspecified; section 5.2 retains the
/// characters, and taking the previous level keeps a zero width
/// (non-)joiner or a PDF inside the run it belongs to instead of
/// splitting that run in two. `original` holds the classes before any
/// rule rewrote them.
pub(super) fn assign_removed_levels(cells: &mut [BidiCell], original: &[BidiClass], para: u8) {
    let mut previous = para;
    for (cell, &cls) in cells.iter_mut().zip(original) {
        if is_x9_removed(cls) {
            cell.level = previous;
        }
        previous = cell.level;
    }
}

/// L1, with the whole paragraph as one line: segment separators (S)
/// and paragraph separators (B) take the paragraph level, and so does
/// every run of [`is_l1_trailing`] characters before one of them or at
/// the end of the text. `original` holds the classes before any rule
/// rewrote them, as L1 requires.
pub(super) fn apply_l1(cells: &mut [BidiCell], original: &[BidiClass], para_level: u8) {
    let mut trailing = true;
    for (cell, &cls) in cells.iter_mut().zip(original).rev() {
        match cls {
            BidiClass::B | BidiClass::S => {
                cell.level = para_level;
                trailing = true;
            }
            _ if is_l1_trailing(cls) => {
                if trailing {
                    cell.level = para_level;
                }
            }
            _ => trailing = false,
        }
    }
}

/// Rule L2 over a sequence of items with the given levels: returns the
/// item indices in visual order, left to right.
///
/// From the highest level down to the lowest odd one, every maximal
/// span of items at that level or higher is reversed. The items can be
/// characters or whole runs.
pub(crate) fn reorder_visual(levels: &[u8]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..levels.len()).collect();
    let (Some(&max), Some(&min)) = (levels.iter().max(), levels.iter().min()) else {
        return order;
    };
    let lowest_odd = min | 1;
    let mut level = max;
    while level >= lowest_odd {
        let mut i = 0;
        while i < levels.len() {
            if levels[order[i]] >= level {
                let start = i;
                while i < levels.len() && levels[order[i]] >= level {
                    i += 1;
                }
                order[start..i].reverse();
            } else {
                i += 1;
            }
        }
        level -= 1;
    }
    order
}
