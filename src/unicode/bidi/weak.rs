//! Weak-type resolution (W1-W7) for one isolating run sequence,
//! followed by the neutral (N0-N2) and implicit-level (I1-I2) rules
//! that finish the sequence.

use alloc::vec::Vec;

use super::explicit::IsolatingSequence;
use super::neutral::{apply_n0, is_ni, n_strong};
use super::{BidiCell, BidiClass};

// ---------------------------------------------------------------------
// W1-W7 + N0-N2 + I1-I2 against one isolating-run sequence.
// ---------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
pub(super) fn resolve_sequence(
    cells: &mut [BidiCell],
    chars: &[char],
    seq: &IsolatingSequence,
    _para_level: u8,
) {
    let n = seq.indices.len();
    if n == 0 {
        return;
    }
    // Snapshot the sequence into a working vector: cheaper to mutate
    // than reaching through `seq.indices` constantly.
    let mut classes: Vec<BidiClass> = seq.indices.iter().map(|&i| cells[i].cls).collect();

    // ---- W1: NSM inherits from preceding char (sos for first). ----
    for i in 0..n {
        if classes[i] == BidiClass::Nsm {
            let prev = if i == 0 { seq.sos } else { classes[i - 1] };
            classes[i] = match prev {
                BidiClass::Pdi | BidiClass::Lri | BidiClass::Rli | BidiClass::Fsi => BidiClass::On,
                other => other,
            };
        }
    }

    // ---- W2: EN preceded by AL (skipping non-strong) -> AN. ----
    for i in 0..n {
        if classes[i] == BidiClass::En {
            // Walk backward through non-strong classes.
            let mut k = i;
            let prev = loop {
                if k == 0 {
                    break seq.sos;
                }
                k -= 1;
                if classes[k].is_strong() {
                    break classes[k];
                }
            };
            if prev == BidiClass::Al {
                classes[i] = BidiClass::An;
            }
        }
    }

    // ---- W3: AL -> R. ----
    for c in &mut classes {
        if *c == BidiClass::Al {
            *c = BidiClass::R;
        }
    }

    // ---- W4: ES/CS between two ENs -> EN; CS between two ANs -> AN. ----
    for i in 1..n.saturating_sub(1) {
        let here = classes[i];
        if here == BidiClass::Es || here == BidiClass::Cs {
            let prev = classes[i - 1];
            let next = classes[i + 1];
            if prev == BidiClass::En && next == BidiClass::En {
                classes[i] = BidiClass::En;
            } else if here == BidiClass::Cs && prev == BidiClass::An && next == BidiClass::An {
                classes[i] = BidiClass::An;
            }
        }
    }

    // ---- W5: sequence of ETs adjacent to EN -> EN. ----
    let mut i = 0;
    while i < n {
        if classes[i] == BidiClass::Et {
            let start = i;
            while i < n && classes[i] == BidiClass::Et {
                i += 1;
            }
            let end = i; // exclusive
            let before = if start == 0 {
                seq.sos
            } else {
                classes[start - 1]
            };
            let after = if end >= n { seq.eos } else { classes[end] };
            if before == BidiClass::En || after == BidiClass::En {
                for c in &mut classes[start..end] {
                    *c = BidiClass::En;
                }
            }
        } else {
            i += 1;
        }
    }

    // ---- W6: remaining ES, ET, CS -> ON. ----
    for c in &mut classes {
        if matches!(*c, BidiClass::Es | BidiClass::Et | BidiClass::Cs) {
            *c = BidiClass::On;
        }
    }

    // ---- W7: EN preceded by L (skipping non-strong) -> L. ----
    for i in 0..n {
        if classes[i] == BidiClass::En {
            let mut k = i;
            let prev = loop {
                if k == 0 {
                    break seq.sos;
                }
                k -= 1;
                if classes[k].is_strong() || classes[k] == BidiClass::R {
                    break classes[k];
                }
            };
            if prev == BidiClass::L {
                classes[i] = BidiClass::L;
            }
        }
    }

    // ---- N0: paired-bracket resolution (UAX #9 §3.3.5). ----
    apply_n0(&mut classes, chars, seq);

    // ---- N1: span of NIs between same-strong text takes that strong. ----
    let mut i = 0;
    while i < n {
        if is_ni(classes[i]) {
            let start = i;
            while i < n && is_ni(classes[i]) {
                i += 1;
            }
            let end = i;
            let before = if start == 0 {
                n_strong(seq.sos)
            } else {
                n_strong(classes[start - 1])
            };
            let after = if end >= n {
                n_strong(seq.eos)
            } else {
                n_strong(classes[end])
            };
            if let (Some(b), Some(a)) = (before, after) {
                if b == a {
                    for c in &mut classes[start..end] {
                        *c = b;
                    }
                }
            }
        } else {
            i += 1;
        }
    }

    // ---- N2: any remaining NI takes embedding direction. ----
    let embed_dir = if seq.level % 2 == 1 {
        BidiClass::R
    } else {
        BidiClass::L
    };
    for c in &mut classes {
        if is_ni(*c) {
            *c = embed_dir;
        }
    }

    // ---- I1 / I2: implicit levels. ----
    // I1 (even level): R -> +1, AN/EN -> +2.
    // I2 (odd level):  L/EN/AN -> +1.
    for (idx_in_seq, &cell_i) in seq.indices.iter().enumerate() {
        let lvl = cells[cell_i].level;
        let cls = classes[idx_in_seq];
        let bump = if lvl % 2 == 0 {
            match cls {
                BidiClass::R => 1,
                BidiClass::An | BidiClass::En => 2,
                _ => 0,
            }
        } else {
            match cls {
                BidiClass::L | BidiClass::En | BidiClass::An => 1,
                _ => 0,
            }
        };
        cells[cell_i].level = lvl + bump;
        cells[cell_i].cls = cls;
    }
}
