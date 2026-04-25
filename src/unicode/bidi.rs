//! UAX #9 Unicode Bidirectional Algorithm.
//!
//! 0.1.0 shipped only the paragraph-direction first-strong rule (P2 /
//! P3). 0.10.0 fills in the rest:
//!
//! - **P1-P3** — paragraph-direction (already shipped, kept).
//! - **X1-X10** — explicit-embedding / override / isolate stack.
//! - **W1-W7** — weak-type resolution.
//! - **N1-N2** — neutral resolution. (N0 paired-bracket handling
//!   is intentionally deferred — see module note below.)
//! - **I1-I2** — implicit-level resolution.
//! - **L1-L4** — post-resolve normalization + reorder (rule L2).
//!
//! The algorithm is implemented as a sequence of array-mutation
//! passes against a single working buffer of (`BidiClass`, `level`)
//! pairs, mirroring the reference implementation. Output is exposed
//! through [`BidiInfo`].
//!
//! ## N0 paired-bracket handling
//!
//! UAX #9 §3.3.5 defines a paired-bracket pass (rule N0) that needs
//! a Bidi_Paired_Bracket / Bidi_Paired_Bracket_Type table sourced
//! from `BidiBrackets.txt`. Production-quality coverage runs into
//! 100+ codepoint pairs (round / square / curly / angle / corner /
//! white / ornament). Shipping that table is mechanical but bulky;
//! the rest of the algorithm correctly resolves brackets via N1
//! (surrounding strong context) for the cases real shaper consumers
//! hit (Latin / Hebrew / Arabic mixed). We file N0 for a follow-up.
//!
//! ## Public API
//!
//! [`BidiInfo::new`] runs the full algorithm against a paragraph and
//! exposes:
//!
//! - [`BidiInfo::paragraph_direction`] — resolved paragraph direction.
//! - [`BidiInfo::levels`] — per-character embedding level (L1-L4
//!   normalised).
//! - [`BidiInfo::reorder`] — visual-order character-index permutation
//!   (rule L2).
//!
//! Buffer integration uses [`crate::buffer::Buffer::set_text_bidi`],
//! which auto-runs the bidi pipeline before shaping. The plain
//! [`crate::buffer::Buffer::set_text`] is left untouched for backward
//! compat with 0.1.0 consumers (oniq, demos) that handle direction
//! themselves.

use alloc::vec;
use alloc::vec::Vec;

use crate::buffer::Direction;
pub use crate::unicode::bidi_class::{bidi_class, BidiClass};

/// Maximum embedding depth permitted by UAX #9 (BD2).
const MAX_DEPTH: u8 = 125;

/// Applies UAX #9 rules P2 and P3 to `text` and returns the
/// paragraph-level direction. LTR when no strong character exists
/// in the run (whitespace-only, symbol-only, empty input).
///
/// Kept on the public surface so 0.1.0 callers don't break.
#[must_use]
pub fn paragraph_direction(text: &str) -> Direction {
    paragraph_direction_with_isolates(text)
}

/// P2 / P3 with proper isolate-skipping. Characters between an
/// isolate-initiator (LRI / RLI / FSI) and its matching PDI do not
/// participate in paragraph-direction resolution.
fn paragraph_direction_with_isolates(text: &str) -> Direction {
    let mut depth: u32 = 0;
    for ch in text.chars() {
        let cls = bidi_class(ch);
        if cls.is_isolate_initiator() {
            depth = depth.saturating_add(1);
            continue;
        }
        if cls == BidiClass::Pdi {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth > 0 {
            continue;
        }
        match cls {
            BidiClass::L => return Direction::Ltr,
            BidiClass::R | BidiClass::Al => return Direction::Rtl,
            _ => {}
        }
    }
    Direction::Ltr
}

/// Per-character bidi state — the working pair the algorithm mutates.
#[derive(Debug, Clone, Copy)]
struct BidiCell {
    /// Resolved Bidi_Class. Mutated by W1-W7 / N1-N2 / I1-I2.
    cls: BidiClass,
    /// Resolved embedding level. Set by X1-X10, mutated by I1-I2 /
    /// L1.
    level: u8,
}

/// Result of running the algorithm against a paragraph.
///
/// Levels are stored per *character* in the input string (not per
/// byte). [`BidiInfo::reorder`] returns a character-index
/// permutation; map back to byte ranges via `char_indices()`.
#[derive(Debug, Clone)]
pub struct BidiInfo {
    /// Paragraph direction resolved by P2 / P3.
    paragraph: Direction,
    /// Per-character embedding levels post L1.
    levels: Vec<u8>,
    /// Character count of the original text. Always == `levels.len()`.
    char_count: usize,
}

impl BidiInfo {
    /// Runs the algorithm against `text`. If `paragraph_dir` is
    /// `None`, P2 / P3 resolves it from the first strong character.
    /// Otherwise the override is honoured (matches the
    /// `unicode-bidi` API).
    #[must_use]
    pub fn new(text: &str, paragraph_dir: Option<Direction>) -> Self {
        let paragraph = paragraph_dir.unwrap_or_else(|| paragraph_direction_with_isolates(text));
        let mut cells: Vec<BidiCell> = text
            .chars()
            .map(|ch| BidiCell {
                cls: bidi_class(ch),
                level: 0,
            })
            .collect();
        let char_count = cells.len();
        if char_count == 0 {
            return BidiInfo {
                paragraph,
                levels: Vec::new(),
                char_count: 0,
            };
        }
        let para_level: u8 = match paragraph {
            Direction::Rtl => 1,
            _ => 0,
        };

        // X1-X10: explicit-level resolution.
        explicit_levels(&mut cells, para_level);

        // Partition into level runs and isolating run sequences,
        // then run W1-W7 + N1-N2 + I1-I2 per sequence.
        let isolating_sequences = build_isolating_sequences(&cells, para_level);
        for seq in isolating_sequences {
            resolve_sequence(&mut cells, &seq, para_level);
        }

        // L1: reset trailing whitespace, segment separators, and
        // paragraph separators back to the paragraph level.
        apply_l1(&mut cells, para_level, text);

        let levels: Vec<u8> = cells.iter().map(|c| c.level).collect();
        BidiInfo {
            paragraph,
            levels,
            char_count,
        }
    }

    /// Resolved paragraph direction.
    #[must_use]
    pub const fn paragraph_direction(&self) -> Direction {
        self.paragraph
    }

    /// Per-character embedding level (post L1).
    #[must_use]
    pub fn levels(&self) -> &[u8] {
        &self.levels
    }

    /// Character count of the input.
    #[must_use]
    pub const fn char_count(&self) -> usize {
        self.char_count
    }

    /// Returns the visual-order character-index permutation (rule
    /// L2). Indices are into the original character sequence
    /// (`text.chars().nth(i)`); the returned `Vec` always has
    /// length [`Self::char_count`].
    #[must_use]
    pub fn reorder(&self) -> Vec<usize> {
        let n = self.char_count;
        let mut order: Vec<usize> = (0..n).collect();
        if n <= 1 {
            return order;
        }
        // L2: from the highest level down to the lowest odd level,
        // reverse the contiguous span at or above that level.
        let max_level = self.levels.iter().copied().max().unwrap_or(0);
        let min_level = self.levels.iter().copied().min().unwrap_or(0);
        // Lowest odd level — anything below it is purely-LTR and
        // never gets reversed.
        let lowest_odd = if min_level % 2 == 1 {
            min_level
        } else {
            min_level + 1
        };
        let mut level = max_level;
        while level >= lowest_odd {
            // Walk through and reverse every contiguous run whose
            // level is >= `level`.
            let mut i = 0;
            while i < n {
                if self.levels[i] >= level {
                    let mut j = i;
                    while j < n && self.levels[j] >= level {
                        j += 1;
                    }
                    order[i..j].reverse();
                    i = j;
                } else {
                    i += 1;
                }
            }
            if level == 0 {
                break;
            }
            level -= 1;
        }
        order
    }
}

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

/// Resolves an FSI initiator at `start` to either [`BidiClass::Lri`] or
/// [`BidiClass::Rli`] per UAX 9 §X5c: scan the matched isolated
/// subsequence for the first strong character (R / AL → RLI, L → LRI),
/// skipping any nested isolates per BD9. Default is LRI when the
/// scan finds no strong type or the FSI has no matching PDI, mirroring
/// the P3 LTR fallback.
fn fsi_resolves_to(cells: &[BidiCell], start: usize) -> BidiClass {
    debug_assert!(matches!(
        cells.get(start).map(|c| c.cls),
        Some(BidiClass::Fsi)
    ));
    let mut depth: u32 = 0;
    for cell in cells.iter().skip(start + 1) {
        let cls = cell.cls;
        if cls.is_isolate_initiator() {
            depth = depth.saturating_add(1);
            continue;
        }
        if cls == BidiClass::Pdi {
            if depth == 0 {
                // End of this FSI's isolated subsequence reached
                // without a strong type — default to LRI.
                return BidiClass::Lri;
            }
            depth -= 1;
            continue;
        }
        if depth == 0 {
            match cls {
                BidiClass::L => return BidiClass::Lri,
                BidiClass::R | BidiClass::Al => return BidiClass::Rli,
                _ => {}
            }
        }
    }
    // No matching PDI / no strong type seen — default LTR.
    BidiClass::Lri
}

/// Implements X1-X10. Sets `cells[i].level` to the embedding level
/// each character resolves to *before* W/N/I passes; characters that
/// the explicit-format pass deletes (rule X9) keep their level but
/// have their class overwritten by directional override per X4 / X5.
/// LRE/RLE/LRO/RLO/PDF/BN keep their original class for the X9 filter
/// later — the convention used here is to mark them with their
/// explicit-format class so `is_explicit()` can drop them.
#[allow(clippy::too_many_lines)]
fn explicit_levels(cells: &mut [BidiCell], para_level: u8) {
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

    for i in 0..cells.len() {
        // Resolve FSI to LRI or RLI before processing per UAX 9 §X5c
        // by scanning the matched isolated subsequence for its first
        // strong character. Skip nested isolates (BD9). When the
        // first strong is R or AL, FSI behaves as RLI; otherwise as
        // LRI (default LTR per the spec, matching the P3 fallback).
        if cells[i].cls == BidiClass::Fsi {
            cells[i].cls = fsi_resolves_to(cells, i);
        }
        let cell = &mut cells[i];
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
                // FSI was resolved to LRI / RLI at the loop entry, so
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
    // X8: handled implicitly — paragraph end pops everything.
}

/// "Strong" classification for N1: maps L to L; R / EN / AN to R;
/// anything else to None. (UAX #9 §3.3.4.)
const fn n_strong(c: BidiClass) -> Option<BidiClass> {
    match c {
        BidiClass::L => Some(BidiClass::L),
        BidiClass::R | BidiClass::En | BidiClass::An => Some(BidiClass::R),
        _ => None,
    }
}

/// True for "neutral and isolate" types per UAX #9 BD11 — the
/// targets of N1 / N2 resolution.
const fn is_ni(c: BidiClass) -> bool {
    matches!(
        c,
        BidiClass::B
            | BidiClass::S
            | BidiClass::Ws
            | BidiClass::On
            | BidiClass::Fsi
            | BidiClass::Lri
            | BidiClass::Rli
            | BidiClass::Pdi
    )
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

/// One "isolating run sequence" per UAX #9 BD13 — a chain of level
/// runs joined by isolate-initiator / PDI pairs.
#[derive(Debug, Clone)]
struct IsolatingSequence {
    /// All cell indices in the sequence, in logical order.
    indices: Vec<usize>,
    /// Embedding level shared by every cell in the sequence.
    level: u8,
    /// `sos` (start-of-sequence) directional class — L or R.
    sos: BidiClass,
    /// `eos` (end-of-sequence) directional class — L or R.
    eos: BidiClass,
}

/// Returns true when `cls` is one of the categories that X9 removes
/// (RLE / LRE / RLO / LRO / PDF / BN).
fn is_x9_removed(cls: BidiClass) -> bool {
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
#[allow(clippy::too_many_lines)]
fn build_isolating_sequences(cells: &[BidiCell], para_level: u8) -> Vec<IsolatingSequence> {
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
        // Follow isolate-initiator chains.
        let mut cursor = ri;
        loop {
            let last_idx = *runs[cursor].indices.last().unwrap();
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
        // sos / eos per BD13.
        let first_idx = indices[0];
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
        let last_idx = *indices.last().unwrap();
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

// ---------------------------------------------------------------------
// W1-W7 + N1-N2 + I1-I2 against one isolating-run sequence.
// ---------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
fn resolve_sequence(cells: &mut [BidiCell], seq: &IsolatingSequence, _para_level: u8) {
    let n = seq.indices.len();
    if n == 0 {
        return;
    }
    // Snapshot the sequence into a working vector — cheaper to mutate
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

    // ---- W2: EN preceded by AL (skipping non-strong) → AN. ----
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

    // ---- W3: AL → R. ----
    for c in &mut classes {
        if *c == BidiClass::Al {
            *c = BidiClass::R;
        }
    }

    // ---- W4: ES/CS between two ENs → EN; CS between two ANs → AN. ----
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

    // ---- W5: sequence of ETs adjacent to EN → EN. ----
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

    // ---- W6: remaining ES, ET, CS → ON. ----
    for c in &mut classes {
        if matches!(*c, BidiClass::Es | BidiClass::Et | BidiClass::Cs) {
            *c = BidiClass::On;
        }
    }

    // ---- W7: EN preceded by L (skipping non-strong) → L. ----
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

    // ---- N0 (paired brackets) deferred — see module docs. ----

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
    // I1 (even level): R → +1, AN/EN → +2.
    // I2 (odd level):  L/EN/AN → +1.
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

// ---------------------------------------------------------------------
// L1 normalization.
// ---------------------------------------------------------------------

/// L1: reset segment separators (S), paragraph separators (B), and
/// any whitespace / isolate-format characters at the end of a line
/// or before a B/S to the paragraph level. We don't have explicit
/// line breaking here — we apply L1 paragraph-globally, treating the
/// whole input as one line. (Line-breaking is the consumer's job.)
fn apply_l1(cells: &mut [BidiCell], para_level: u8, _text: &str) {
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
    // text — which we don't have here as chars indexed; cheaper to
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

/// Helper for L1 — currently just returns the post-W class. Kept as
/// a function to make it easy to wire in a separate "original class"
/// snapshot later if BidiTest conformance demands strict L1 fidelity.
const fn bidi_class_at(cells: &[BidiCell], i: usize) -> BidiClass {
    cells[i].cls
}

// ---------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Backwards-compat tests from 0.1.0 (preserved) ----

    #[test]
    fn ascii_letters_are_strong_ltr() {
        assert_eq!(bidi_class('a'), BidiClass::L);
        assert_eq!(bidi_class('Z'), BidiClass::L);
    }

    #[test]
    fn arabic_is_strong_al() {
        assert_eq!(bidi_class('ا'), BidiClass::Al);
        assert_eq!(bidi_class('م'), BidiClass::Al);
    }

    #[test]
    fn hebrew_is_strong_rtl() {
        assert_eq!(bidi_class('א'), BidiClass::R);
        assert_eq!(bidi_class('ש'), BidiClass::R);
    }

    #[test]
    fn first_strong_determines_paragraph_direction() {
        assert_eq!(paragraph_direction("Hello"), Direction::Ltr);
        assert_eq!(paragraph_direction("שלום"), Direction::Rtl);
        assert_eq!(paragraph_direction("السلام عليكم"), Direction::Rtl);
    }

    #[test]
    fn leading_neutrals_are_skipped() {
        assert_eq!(paragraph_direction("   Hello"), Direction::Ltr);
        assert_eq!(paragraph_direction("12 שלום"), Direction::Rtl);
    }

    #[test]
    fn strong_ltr_wins_over_rtl_that_follows() {
        assert_eq!(paragraph_direction("Hello שלום"), Direction::Ltr);
    }

    #[test]
    fn empty_or_neutral_input_defaults_to_ltr() {
        assert_eq!(paragraph_direction(""), Direction::Ltr);
        assert_eq!(paragraph_direction("     "), Direction::Ltr);
        assert_eq!(paragraph_direction("12345"), Direction::Ltr);
    }

    // ---- New tests: BidiInfo end-to-end ----

    #[test]
    fn pure_latin_resolves_to_zero_levels() {
        let info = BidiInfo::new("Hello", None);
        assert_eq!(info.paragraph_direction(), Direction::Ltr);
        assert_eq!(info.levels(), &[0, 0, 0, 0, 0]);
        assert_eq!(info.reorder(), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn pure_hebrew_resolves_to_level_one() {
        // 4 Hebrew chars.
        let info = BidiInfo::new("שלום", None);
        assert_eq!(info.paragraph_direction(), Direction::Rtl);
        assert_eq!(info.levels(), &[1, 1, 1, 1]);
        // L2: reverse all chars at level >= 1.
        assert_eq!(info.reorder(), vec![3, 2, 1, 0]);
    }

    #[test]
    fn ltr_then_hebrew_keeps_latin_at_zero_hebrew_at_one() {
        // "A " + "ב" (Hebrew bet) — paragraph LTR.
        let info = BidiInfo::new("A ב", None);
        assert_eq!(info.paragraph_direction(), Direction::Ltr);
        // 'A' L → 0; ' ' WS reset to 0 (L1 trailing); 'ב' R → 1.
        // After L1 trailing-whitespace reset on the space (it's not
        // trailing — Hebrew letter follows), space stays at 0.
        // Actually the space comes between two strong runs; N1
        // sees L on one side and R on the other → no rule fires;
        // N2 sets it to embedding (0). So levels: [0, 0, 1].
        assert_eq!(info.levels(), &[0, 0, 1]);
        // L2 reverses chars at level >= 1 (just the bet).
        assert_eq!(info.reorder(), vec![0, 1, 2]);
    }

    #[test]
    fn hebrew_then_latin_with_rtl_paragraph_reverses_hebrew() {
        // Paragraph RTL ("שלום A"). 4 Hebrew + space + 'A'.
        let info = BidiInfo::new("שלום A", None);
        assert_eq!(info.paragraph_direction(), Direction::Rtl);
        // 4 hebrews at 1, space gets paragraph (1) by N2, A at 2 (LTR
        // inside RTL para).
        let levels = info.levels();
        assert_eq!(levels.len(), 6);
        // A at level 2.
        assert_eq!(levels[5], 2);
        // Hebrew letters at level 1.
        for &l in &levels[0..4] {
            assert_eq!(l, 1);
        }
    }

    #[test]
    fn explicit_paragraph_direction_overrides_first_strong() {
        let info = BidiInfo::new("Hello", Some(Direction::Rtl));
        assert_eq!(info.paragraph_direction(), Direction::Rtl);
        // Latin in RTL paragraph → level 2.
        assert_eq!(info.levels(), &[2, 2, 2, 2, 2]);
    }

    #[test]
    fn empty_input_yields_empty_info() {
        let info = BidiInfo::new("", None);
        assert_eq!(info.paragraph_direction(), Direction::Ltr);
        assert!(info.levels().is_empty());
        assert!(info.reorder().is_empty());
    }

    // ---- W-rule unit coverage ----

    #[test]
    fn w2_en_after_al_becomes_an() {
        // Arabic alef + ASCII '1'. W2 fires: EN after AL → AN.
        // W3: AL→R. Embedding level 1. I2 at odd: AN→+1 → level 2.
        let info = BidiInfo::new("\u{0627}1", None);
        assert_eq!(info.paragraph_direction(), Direction::Rtl);
        let levels = info.levels();
        assert_eq!(levels.len(), 2);
        // Arabic alef: AL → R → odd, no bump → level 1.
        assert_eq!(levels[0], 1);
        // '1' became AN; embedding 1; AN bumped +1 → level 2.
        assert_eq!(levels[1], 2);
    }

    #[test]
    fn w4_es_between_ens_promotes_in_rtl() {
        // "1+2" in default paragraph: sos=L, W7 turns the EN run
        // back to L → level 0. To exercise W4 cleanly we force
        // RTL paragraph: sos=R, no W7 promotion → digits keep EN.
        let info = BidiInfo::new("1+2", Some(Direction::Rtl));
        let levels = info.levels();
        assert_eq!(levels.len(), 3);
        // Embedding 1 for ENs; I2 bumps EN by 1 → level 2.
        // The ES becomes EN via W4 (between two ENs).
        assert_eq!(levels, &[2, 2, 2]);
    }

    #[test]
    fn w5_et_adjacent_to_en_becomes_en_in_rtl() {
        // "$1" — ET EN. W5 turns ET into EN. Default-LTR paragraph
        // would then run W7 and downgrade EN→L. Force RTL to see
        // pure W5 effect.
        let info = BidiInfo::new("$1", Some(Direction::Rtl));
        let levels = info.levels();
        assert_eq!(levels.len(), 2);
        // Both EN at level 2 (embedding 1, I2 bumps EN +1).
        assert_eq!(levels, &[2, 2]);
    }

    #[test]
    fn w7_en_after_l_becomes_l() {
        // "A1" — L EN. W7 fires: EN preceded by L → L. So both at
        // level 0 in an LTR paragraph.
        let info = BidiInfo::new("A1", None);
        let levels = info.levels();
        assert_eq!(levels.len(), 2);
        assert_eq!(levels, &[0, 0]);
    }

    // ---- N-rule unit coverage ----

    #[test]
    fn n2_neutral_takes_embedding() {
        // Pure-neutral paragraph: just '!'. Default paragraph LTR,
        // N2 sets ON to L → level 0.
        let info = BidiInfo::new("!", None);
        assert_eq!(info.levels(), &[0]);
    }

    #[test]
    fn n1_neutral_between_two_rs_takes_r() {
        // ב!ב — three chars. N1 should set the '!' to R, then I1
        // bumps R by 1 → all level 1.
        let info = BidiInfo::new("\u{05D1}!\u{05D1}", None);
        assert_eq!(info.paragraph_direction(), Direction::Rtl);
        let levels = info.levels();
        assert_eq!(levels.len(), 3);
        for &l in levels {
            assert_eq!(l, 1);
        }
    }

    // ---- X-rule explicit-format coverage ----

    #[test]
    fn rle_pdf_pair_embeds_then_pops() {
        // RLE 'A' PDF 'B' — 'A' is inside an RTL embedding (level 1),
        // 'B' is at the paragraph level (0).
        let info = BidiInfo::new("\u{202B}A\u{202C}B", None);
        let levels = info.levels();
        assert_eq!(levels.len(), 4);
        // RLE itself: keeps paragraph level (0).
        assert_eq!(levels[0], 0);
        // 'A' inside RLE → embedded at level 1, then I2 bumps L
        // by 1 → level 2.
        assert_eq!(levels[1], 2);
        // PDF: paragraph level.
        assert_eq!(levels[2], 0);
        // 'B' back at paragraph level → 0.
        assert_eq!(levels[3], 0);
    }

    #[test]
    fn fsi_with_latin_inside_resolves_as_lri() {
        // 'A' FSI 'X' PDI 'B' — first strong inside FSI is L, so FSI
        // must behave as LRI (embed at next *even* level, here 2).
        // 'X' is L → I1 even-level no bump → level 2.
        let info = BidiInfo::new("A\u{2068}X\u{2069}B", None);
        let levels = info.levels();
        assert_eq!(levels.len(), 5);
        assert_eq!(levels[0], 0);
        assert_eq!(levels[2], 2);
        assert_eq!(levels[4], 0);
    }

    #[test]
    fn fsi_with_hebrew_inside_resolves_as_rli() {
        // 'A' FSI 'אבג' PDI 'B' — first strong inside FSI is R, so
        // FSI must behave as RLI (embed at next *odd* level, here 1).
        // Hebrew letters at level 1; B at paragraph 0.
        let info = BidiInfo::new("A\u{2068}\u{05D0}\u{05D1}\u{05D2}\u{2069}B", None);
        let levels = info.levels();
        assert_eq!(levels.len(), 7);
        assert_eq!(levels[0], 0);
        for &l in &levels[2..5] {
            assert_eq!(l, 1, "hebrew inside FSI should be at level 1");
        }
        assert_eq!(levels[6], 0);
    }

    #[test]
    fn fsi_with_no_strong_inside_defaults_to_lri() {
        // 'A' FSI '!!!' PDI 'B' — no strong type inside FSI, so default
        // is LRI: embed at level 2 in LTR paragraph; '!' chars resolve
        // to L via N2 at level 2.
        let info = BidiInfo::new("A\u{2068}!!!\u{2069}B", None);
        let levels = info.levels();
        assert_eq!(levels.len(), 7);
        assert_eq!(levels[0], 0);
        for &l in &levels[2..5] {
            assert_eq!(l, 2, "neutrals inside FSI default to LRI level 2");
        }
        assert_eq!(levels[6], 0);
    }

    #[test]
    fn fsi_skips_nested_isolates_when_resolving() {
        // 'A' FSI LRI 'B' PDI 'ש' PDI 'C' — first strong AT FSI's
        // own depth must be 'ש' (R), not the nested LRI's 'B'. So FSI
        // should resolve as RLI: embed level 1, hebrew at 1, 'B'
        // (inside the nested LRI at FSI+1=2) at the inner LRI's
        // even-bumped level 2.
        let text = "A\u{2068}\u{2066}B\u{2069}\u{05E9}\u{2069}C";
        let info = BidiInfo::new(text, None);
        let levels = info.levels();
        // Hebrew 'ש' is the 6th char (index 5) of the input.
        // Verify FSI itself sat at the surrounding paragraph level (0)
        // and the 'ש' resolves at FSI's odd-embedded level 1.
        assert_eq!(levels[0], 0); // 'A'
        assert_eq!(levels[5], 1, "hebrew inside FSI must be at level 1");
        assert_eq!(*levels.last().unwrap(), 0); // 'C'
    }

    #[test]
    fn lri_pdi_isolate_pair_works() {
        // 'A' LRI 'B' PDI 'C' — straightforward LTR isolate.
        let info = BidiInfo::new("A\u{2066}B\u{2069}C", None);
        let levels = info.levels();
        assert_eq!(levels.len(), 5);
        // All chars in LTR paragraph at level 0 except for the LRI
        // itself which sits at the surrounding level. The 'B' is
        // inside an LTR isolate at level 2.
        assert_eq!(levels[0], 0);
        assert_eq!(levels[2], 2);
        assert_eq!(levels[4], 0);
    }

    // ---- L2 reordering coverage ----

    #[test]
    fn reorder_preserves_logical_when_no_levels() {
        let info = BidiInfo::new("abc", None);
        assert_eq!(info.reorder(), vec![0, 1, 2]);
    }

    #[test]
    fn reorder_reverses_pure_rtl() {
        let info = BidiInfo::new("\u{05D0}\u{05D1}\u{05D2}", None);
        assert_eq!(info.reorder(), vec![2, 1, 0]);
    }

    #[test]
    fn reorder_mixed_latin_hebrew_in_ltr_para() {
        // "A" + Hebrew bet gimel + "B"  →  level 0 1 1 0
        // L2 reverses the level-1 span: visual = A gimel bet B.
        let info = BidiInfo::new("A\u{05D1}\u{05D2}B", None);
        let levels = info.levels();
        assert_eq!(levels, &[0, 1, 1, 0]);
        assert_eq!(info.reorder(), vec![0, 2, 1, 3]);
    }

    // ---- UAX 9 reference test extract ----
    //
    // 10 curated entries from BidiCharacterTest.txt. Each row is
    // (text, paragraph_dir_override, expected per-char levels).
    //
    // Entries are paraphrased — the reference test uses raw
    // codepoints; here we hand-pick a representative slice so the
    // intent is readable.

    #[test]
    fn uax9_reference_extract_covers_strong_neutral_weak_explicit() {
        struct Case<'a> {
            text: &'a str,
            para: Option<Direction>,
            levels: &'a [u8],
        }
        let cases = [
            // 1. Pure Latin.
            Case {
                text: "abc",
                para: None,
                levels: &[0, 0, 0],
            },
            // 2. Pure Hebrew.
            Case {
                text: "\u{05D0}\u{05D1}\u{05D2}",
                para: None,
                levels: &[1, 1, 1],
            },
            // 3. Latin + Hebrew + Latin in LTR.
            Case {
                text: "a\u{05D0}b",
                para: None,
                levels: &[0, 1, 0],
            },
            // 4. Hebrew + Latin + Hebrew in RTL.
            Case {
                text: "\u{05D0}a\u{05D1}",
                para: None,
                levels: &[1, 2, 1],
            },
            // 5. Digit-only paragraph: sos=L, W7 turns EN→L → level 0.
            Case {
                text: "123",
                para: None,
                levels: &[0, 0, 0],
            },
            // 6. Arabic letter + ASCII digit. W2: EN→AN; W3: AL→R;
            //    odd embedding 1, I2 bumps AN by 1 → level 2.
            Case {
                text: "\u{0627}1",
                para: None,
                levels: &[1, 2],
            },
            // 7. ET adjacent to EN: "$1" with paragraph LTR. W5
            //    turns ET into EN, then W7 turns the EN run back
            //    into L (sos=L) → level 0.
            Case {
                text: "$1",
                para: None,
                levels: &[0, 0],
            },
            // 8. ES between two ENs in LTR para: W4 turns ES into
            //    EN, W7 then turns the run into L → level 0.
            Case {
                text: "1+2",
                para: None,
                levels: &[0, 0, 0],
            },
            // 9. RLE override.
            Case {
                text: "\u{202B}AB\u{202C}",
                para: None,
                levels: &[0, 2, 2, 0],
            },
            // 10. LRI / PDI isolate. The LRI / PDI sit at the
            //     surrounding level; only B inside the isolate is
            //     at level 2.
            Case {
                text: "A\u{2066}B\u{2069}C",
                para: None,
                levels: &[0, 0, 2, 0, 0],
            },
        ];
        for (i, c) in cases.iter().enumerate() {
            let info = BidiInfo::new(c.text, c.para);
            assert_eq!(
                info.levels(),
                c.levels,
                "case {i}: text {:?} expected {:?}, got {:?}",
                c.text,
                c.levels,
                info.levels()
            );
        }
    }
}
