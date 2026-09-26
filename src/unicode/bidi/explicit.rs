//! Explicit embedding levels (UAX #9 rules X1-X10): the embedding,
//! override and isolate stack, then the level runs and isolating run
//! sequences (BD13) the weak, neutral and implicit rules work on.

use alloc::vec;
use alloc::vec::Vec;

use super::reorder::is_x9_removed;
use super::{BidiCell, BidiClass};

/// Maximum embedding depth permitted by UAX #9 (BD2).
const MAX_DEPTH: u8 = 125;

// ---------------------------------------------------------------------
// X1-X10: explicit-level resolution.
// ---------------------------------------------------------------------

/// One frame on the embedding stack. Tracks the level we'd push, the
/// directional override (if any), and whether the entry was isolating.
#[derive(Debug, Clone, Copy)]
struct StackEntry {
    level: u8,
    override_status: Override,
    isolate: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Override {
    None,
    Ltr,
    Rtl,
}

/// Resolves every FSI initiator to either [`BidiClass::Lri`] or
/// [`BidiClass::Rli`] per UAX 9 §X5c: the first strong character of
/// the FSI's isolated subsequence decides (R / AL -> RLI, L -> LRI),
/// skipping any nested isolates per BD9. Default is LRI when the
/// subsequence has no strong type or the FSI has no matching PDI,
/// mirroring the P3 LTR fallback.
///
/// One pass with a stack of open isolate initiators: a strong
/// character can only decide the innermost open FSI, and a PDI closes
/// the innermost open initiator. Scanning forward from every FSI
/// instead is quadratic on a long run of FSIs.
fn resolve_fsis(cells: &mut [BidiCell]) {
    // Open isolate initiators, innermost last: (index, FSI still
    // waiting for its first strong character).
    let mut open: Vec<(usize, bool)> = Vec::new();
    for j in 0..cells.len() {
        let cls = cells[j].cls;
        if cls.is_isolate_initiator() {
            open.push((j, cls == BidiClass::Fsi));
        } else if cls == BidiClass::Pdi {
            // End of the innermost isolated subsequence. An FSI that
            // saw no strong type defaults to LRI.
            if let Some((idx, true)) = open.pop() {
                cells[idx].cls = BidiClass::Lri;
            }
        } else if let Some((idx, pending)) = open.last_mut() {
            if *pending {
                let resolved = match cls {
                    BidiClass::L => Some(BidiClass::Lri),
                    BidiClass::R | BidiClass::Al => Some(BidiClass::Rli),
                    _ => None,
                };
                if let Some(r) = resolved {
                    cells[*idx].cls = r;
                    *pending = false;
                }
            }
        }
    }
    // No matching PDI and no strong type seen: default LTR.
    for (idx, pending) in open {
        if pending {
            cells[idx].cls = BidiClass::Lri;
        }
    }
}

/// Implements X1-X10. Sets `cells[i].level` to the embedding level
/// each character resolves to *before* W/N/I passes; characters that
/// the explicit-format pass deletes (rule X9) keep their level but
/// have their class overwritten by directional override per X4 / X5.
/// LRE/RLE/LRO/RLO/PDF/BN keep their original class for the X9 filter
/// later. The convention used here is to mark them with their
/// explicit-format class so `is_explicit()` can drop them.
pub(super) fn explicit_levels(cells: &mut [BidiCell], para_level: u8) {
    let mut stack: Vec<StackEntry> = Vec::with_capacity(8);
    stack.push(StackEntry {
        level: para_level,
        override_status: Override::None,
        isolate: false,
    });
    // Counters for X6a / X7a per UAX 9 (overflow tracking).
    let mut overflow_isolate: u32 = 0;
    let mut overflow_embedding: u32 = 0;
    let mut valid_isolate_count: u32 = 0;

    // Resolve FSI to LRI or RLI before processing per UAX 9 §X5c.
    // The resolution reads only the original classes, so doing it for
    // every FSI up front matches doing it as the loop reaches each one.
    resolve_fsis(cells);

    for cell in cells.iter_mut() {
        match cell.cls {
            // X2-X5: explicit embedding / override.
            BidiClass::Rle | BidiClass::Lre | BidiClass::Rlo | BidiClass::Lro => {
                let is_rtl = matches!(cell.cls, BidiClass::Rle | BidiClass::Rlo);
                let override_status = match cell.cls {
                    BidiClass::Lro => Override::Ltr,
                    BidiClass::Rlo => Override::Rtl,
                    _ => Override::None,
                };
                let last = stack.last().copied().unwrap_or(StackEntry {
                    level: para_level,
                    override_status: Override::None,
                    isolate: false,
                });
                let new_level = if is_rtl {
                    next_odd_level(last.level)
                } else {
                    next_even_level(last.level)
                };
                cell.level = last.level;
                if new_level <= MAX_DEPTH && overflow_isolate == 0 && overflow_embedding == 0 {
                    stack.push(StackEntry {
                        level: new_level,
                        override_status,
                        isolate: false,
                    });
                } else if overflow_isolate == 0 {
                    overflow_embedding = overflow_embedding.saturating_add(1);
                }
            }
            // X5a / X5b / X5c: isolate initiators.
            BidiClass::Rli | BidiClass::Lri | BidiClass::Fsi => {
                let last = stack.last().copied().unwrap_or(StackEntry {
                    level: para_level,
                    override_status: Override::None,
                    isolate: false,
                });
                cell.level = last.level;
                // Apply override to the isolate initiator itself.
                match last.override_status {
                    Override::Ltr => cell.cls = BidiClass::L,
                    Override::Rtl => cell.cls = BidiClass::R,
                    Override::None => {}
                }
                // FSI was resolved to LRI / RLI before the loop, so
                // only Rli is RTL here.
                let is_rtl = cell.cls == BidiClass::Rli;
                let new_level = if is_rtl {
                    next_odd_level(last.level)
                } else {
                    next_even_level(last.level)
                };
                if new_level <= MAX_DEPTH && overflow_isolate == 0 && overflow_embedding == 0 {
                    valid_isolate_count = valid_isolate_count.saturating_add(1);
                    stack.push(StackEntry {
                        level: new_level,
                        override_status: Override::None,
                        isolate: true,
                    });
                } else {
                    overflow_isolate = overflow_isolate.saturating_add(1);
                }
            }
            // X6a: PDI.
            BidiClass::Pdi => {
                if overflow_isolate > 0 {
                    overflow_isolate -= 1;
                } else if valid_isolate_count > 0 {
                    overflow_embedding = 0;
                    while let Some(top) = stack.last() {
                        if top.isolate {
                            stack.pop();
                            break;
                        }
                        stack.pop();
                    }
                    valid_isolate_count -= 1;
                }
                let last = stack.last().copied().unwrap_or(StackEntry {
                    level: para_level,
                    override_status: Override::None,
                    isolate: false,
                });
                cell.level = last.level;
                match last.override_status {
                    Override::Ltr => cell.cls = BidiClass::L,
                    Override::Rtl => cell.cls = BidiClass::R,
                    Override::None => {}
                }
            }
            // X7: PDF.
            BidiClass::Pdf => {
                if overflow_isolate > 0 {
                    // Ignored.
                } else if overflow_embedding > 0 {
                    overflow_embedding -= 1;
                } else if stack.len() > 1 && !stack.last().is_some_and(|e| e.isolate) {
                    stack.pop();
                }
                let last = stack.last().copied().unwrap_or(StackEntry {
                    level: para_level,
                    override_status: Override::None,
                    isolate: false,
                });
                cell.level = last.level;
            }
            // X6: any other character.
            _ => {
                let last = stack.last().copied().unwrap_or(StackEntry {
                    level: para_level,
                    override_status: Override::None,
                    isolate: false,
                });
                cell.level = last.level;
                match last.override_status {
                    Override::Ltr => cell.cls = BidiClass::L,
                    Override::Rtl => cell.cls = BidiClass::R,
                    Override::None => {}
                }
            }
        }
    }
    // X8: handled implicitly. Paragraph end pops everything.
}

const fn next_odd_level(current: u8) -> u8 {
    if current % 2 == 0 {
        current + 1
    } else {
        current + 2
    }
}

const fn next_even_level(current: u8) -> u8 {
    if current % 2 == 0 {
        current + 2
    } else {
        current + 1
    }
}

// ---------------------------------------------------------------------
// X9 / level-run partitioning + isolating-run sequence assembly.
// ---------------------------------------------------------------------

/// One contiguous run of cells that share the same embedding level
/// AND don't include explicit-format characters that X9 strips.
#[derive(Debug, Clone)]
struct LevelRun {
    /// Indices into the cells array, in logical order.
    indices: Vec<usize>,
    level: u8,
}

/// One "isolating run sequence" per UAX #9 BD13: a chain of level
/// runs joined by isolate-initiator / PDI pairs.
#[derive(Debug, Clone)]
pub(super) struct IsolatingSequence {
    /// All cell indices in the sequence, in logical order.
    pub(super) indices: Vec<usize>,
    /// Embedding level shared by every cell in the sequence.
    pub(super) level: u8,
    /// `sos` (start-of-sequence) directional class: L or R.
    pub(super) sos: BidiClass,
    /// `eos` (end-of-sequence) directional class: L or R.
    pub(super) eos: BidiClass,
}

fn build_level_runs(cells: &[BidiCell]) -> Vec<LevelRun> {
    let mut runs: Vec<LevelRun> = Vec::new();
    let mut current: Option<LevelRun> = None;
    for (i, cell) in cells.iter().enumerate() {
        if is_x9_removed(cell.cls) {
            continue;
        }
        match current.as_mut() {
            Some(run) if run.level == cell.level => run.indices.push(i),
            _ => {
                if let Some(prev) = current.take() {
                    runs.push(prev);
                }
                current = Some(LevelRun {
                    indices: vec![i],
                    level: cell.level,
                });
            }
        }
    }
    if let Some(run) = current.take() {
        runs.push(run);
    }
    runs
}

/// Joins level runs into BD13 isolating-run sequences. Each isolate
/// initiator (LRI / RLI / FSI) hands off to the level run starting
/// inside the isolate; the matching PDI rejoins.
pub(super) fn build_isolating_sequences(
    cells: &[BidiCell],
    para_level: u8,
) -> Vec<IsolatingSequence> {
    let runs = build_level_runs(cells);
    if runs.is_empty() {
        return Vec::new();
    }
    // For BD13: each run starting after an isolate initiator chains
    // back to the run containing that initiator. We find this by
    // scanning runs in order and pairing.
    let mut sequences: Vec<Vec<usize>> = Vec::new(); // each entry: list of run indices
    let mut used = vec![false; runs.len()];
    // Map: isolate initiator cell index -> run index that "owns" it.
    // Approach: walk runs left-to-right. Each unused run starts a
    // sequence; we extend it by following isolate initiators to the
    // run that picks up after the corresponding PDI.
    // To find that run we precompute for each isolate initiator the
    // matching PDI cell index.
    let isolate_map = build_isolate_map(cells);
    // Map cell index -> run index containing it.
    let mut cell_to_run = vec![usize::MAX; cells.len()];
    for (ri, run) in runs.iter().enumerate() {
        for &ci in &run.indices {
            cell_to_run[ci] = ri;
        }
    }
    for ri in 0..runs.len() {
        if used[ri] {
            continue;
        }
        let mut seq_runs: Vec<usize> = vec![ri];
        used[ri] = true;
        // Follow isolate-initiator chains. Level runs are never empty,
        // so `last()` always yields a cell.
        let mut cursor = ri;
        while let Some(&last_idx) = runs[cursor].indices.last() {
            let last_cls = cells[last_idx].cls;
            if !last_cls.is_isolate_initiator() {
                break;
            }
            let Some(pdi_ci) = isolate_map.get(&last_idx).copied() else {
                break;
            };
            // Run containing the cell *after* the PDI continues the sequence.
            // First locate the run containing the PDI itself, then take
            // the very same run if it has cells after the PDI within it,
            // otherwise stop.
            if pdi_ci >= cells.len() {
                break;
            }
            let pdi_run = cell_to_run[pdi_ci];
            if pdi_run == usize::MAX || used[pdi_run] {
                break;
            }
            used[pdi_run] = true;
            seq_runs.push(pdi_run);
            cursor = pdi_run;
        }
        sequences.push(seq_runs);
    }

    let mut out: Vec<IsolatingSequence> = Vec::with_capacity(sequences.len());
    for seq_runs in sequences {
        let mut indices: Vec<usize> = Vec::new();
        let mut level: u8 = 0;
        for &ri in &seq_runs {
            level = runs[ri].level;
            indices.extend_from_slice(&runs[ri].indices);
        }
        // Every sequence holds at least one non-empty run.
        let (Some(&first_idx), Some(&last_idx)) = (indices.first(), indices.last()) else {
            continue;
        };
        // sos / eos per BD13.
        let sos_level = if first_idx == 0 {
            para_level
        } else {
            // Walk back through X9-removed cells to find the previous
            // non-removed cell's level.
            let mut k = first_idx;
            loop {
                if k == 0 {
                    break para_level;
                }
                k -= 1;
                if !is_x9_removed(cells[k].cls) {
                    break cells[k].level;
                }
            }
        };
        // For eos: if the sequence ends in an isolate initiator that
        // had no matching PDI, eos is max(level, paragraph). Else
        // it's the level of the next cell beyond the sequence.
        let last_cls = cells[last_idx].cls;
        let eos_level = if last_cls.is_isolate_initiator() && !isolate_map.contains_key(&last_idx) {
            level.max(para_level)
        } else if last_idx + 1 >= cells.len() {
            para_level.max(level)
        } else {
            // First non-removed cell after.
            let mut k = last_idx + 1;
            loop {
                if k >= cells.len() {
                    break para_level.max(level);
                }
                if !is_x9_removed(cells[k].cls) {
                    break cells[k].level;
                }
                k += 1;
            }
        };
        let sos = if sos_level.max(level) % 2 == 1 {
            BidiClass::R
        } else {
            BidiClass::L
        };
        let eos = if eos_level.max(level) % 2 == 1 {
            BidiClass::R
        } else {
            BidiClass::L
        };
        out.push(IsolatingSequence {
            indices,
            level,
            sos,
            eos,
        });
    }
    out
}

/// Builds a map from isolate-initiator cell index to its matching PDI
/// cell index. Initiators without a matching PDI are absent.
fn build_isolate_map(cells: &[BidiCell]) -> alloc::collections::BTreeMap<usize, usize> {
    let mut map: alloc::collections::BTreeMap<usize, usize> = alloc::collections::BTreeMap::new();
    let mut stack: Vec<usize> = Vec::new();
    for (i, cell) in cells.iter().enumerate() {
        if cell.cls.is_isolate_initiator() {
            stack.push(i);
        } else if cell.cls == BidiClass::Pdi {
            if let Some(opener) = stack.pop() {
                map.insert(opener, i);
            }
        }
    }
    map
}
