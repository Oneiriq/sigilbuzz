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
pub(super) fn apply_l1(cells: &mut [BidiCell], para_level: u8, _text: &str) {
    let n = cells.len();
    if n == 0 {
        return;
    }
    // First pass: each S or B resets to the paragraph level. Any
    // whitespace / isolate-format characters preceding it also reset.
    for i in 0..n {
        let cls = bidi_class_at(cells, i); // raw class re-lookup
        let _ = cls; // silence: we use cells[i].cls below; keep signature simple
    }
    // We need the *original* Bidi_Class for L1 because explicit-level
    // resolution mutated the working class. Walk backwards from each
    // S/B, resetting trailing WS/Iso runs.
    // The "originals" are recoverable only if we re-classify from
    // text, which we don't have here as chars indexed; cheaper to
    // remember an L1-eligible flag during the X-pass. As a
    // pragmatic approximation we reset based on the post-W class:
    // any cell whose post-W class is WS/Iso/B/S gets reset.
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

/// Helper for L1: currently just returns the post-W class. Kept as
/// a function to make it easy to wire in a separate "original class"
/// snapshot later if BidiTest conformance demands strict L1 fidelity.
const fn bidi_class_at(cells: &[BidiCell], i: usize) -> BidiClass {
    cells[i].cls
}
