//! Level normalization ahead of reordering (rule L1). The L2 reversal
//! itself is [`BidiInfo::reorder`](super::BidiInfo::reorder).

use super::{BidiCell, BidiClass};

// ---------------------------------------------------------------------
// L1 normalization.
// ---------------------------------------------------------------------

/// L1: reset segment separators (S), paragraph separators (B), and
/// any whitespace / isolate-format characters at the end of a line
/// or before a B/S to the paragraph level. We don't have explicit
/// line breaking here. We apply L1 paragraph-globally, treating the
/// whole input as one line. (Line-breaking is the consumer's job.)
pub(super) fn apply_l1(cells: &mut [BidiCell], para_level: u8) {
    let n = cells.len();
    if n == 0 {
        return;
    }
    // Each S or B resets to the paragraph level, and so does any run
    // of whitespace / isolate-format characters before it. Strict L1
    // needs the *original* Bidi_Class, which explicit-level resolution
    // has overwritten by now. As an approximation we reset based on
    // the resolved class: any cell whose resolved class is WS/Iso/B/S
    // gets reset. Walk backwards from each S/B.
    let mut i = n;
    let mut reset_run = false;
    while i > 0 {
        i -= 1;
        let post = cells[i].cls;
        match post {
            BidiClass::B | BidiClass::S => {
                cells[i].level = para_level;
                reset_run = true;
            }
            BidiClass::Ws | BidiClass::Fsi | BidiClass::Lri | BidiClass::Rli | BidiClass::Pdi => {
                if reset_run {
                    cells[i].level = para_level;
                }
            }
            _ => {
                reset_run = false;
            }
        }
    }
    // Trailing whitespace / isolate-format at end of paragraph also
    // resets.
    let mut i = n;
    while i > 0 {
        i -= 1;
        match cells[i].cls {
            BidiClass::Ws | BidiClass::Fsi | BidiClass::Lri | BidiClass::Rli | BidiClass::Pdi => {
                cells[i].level = para_level;
            }
            _ => break,
        }
    }
}
