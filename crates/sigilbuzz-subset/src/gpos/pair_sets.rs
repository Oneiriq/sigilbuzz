//! PairPos format 1 serialization, split into several subtables when
//! one cannot address all of its PairSets.
//!
//! ```text
//!   u16      posFormat = 1
//!   Offset16 coverageOffset
//!   u16      valueFormat1
//!   u16      valueFormat2
//!   u16      pairSetCount
//!   Offset16 pairSetOffsets[pairSetCount]      (from the subtable)
//! ```
//!
//! A PairSet measures the device offsets of its ValueRecords from its
//! own start, so each rebuilt PairSet carries private copies of its
//! Device and VariationIndex tables, and a subtable whose PairSets
//! shared those tables in the source can come out twice as large.
//!
//! The Coverage goes right after the offset array and the PairSets
//! last, identical ones shared. Only the start of each PairSet then
//! has to lie within 64 KiB of the subtable. When the PairSets outgrow
//! that, the first glyphs are cut into runs, each run its own subtable
//! in the lookup, in glyph order. Every first glyph lands in exactly
//! one run, and a subtable that does not cover the first glyph passes
//! it on to the next, so shaping does not change.

use alloc::vec;
use alloc::vec::Vec;

use crate::coverage::emit_coverage_from_glyphs;
use crate::device::Dedup;
use crate::layout::{RewriterCtx, RewrittenSubtable};

/// Kept first glyphs of a PairPos format 1 subtable, each with its
/// rebuilt PairSet body.
pub(super) type PairSets = Vec<(u16, Vec<u8>)>;

/// Serializes PairPos format 1 subtables for `sets`. Returns one
/// subtable when it fits and several otherwise. A PairSet that cannot
/// be addressed even alone is recorded in [`RewriterCtx::offsets`].
pub(super) fn emit_pair_sets(
    ctx: &RewriterCtx,
    value_format1: u16,
    value_format2: u16,
    mut sets: PairSets,
) -> Vec<RewrittenSubtable> {
    // Coverage order is glyph order; a malformed source Coverage may
    // not have been sorted.
    sets.sort_by_key(|&(gid, _)| gid);
    sets.dedup_by_key(|&mut (gid, _)| gid);
    let sets = sets.as_slice();
    if let Some(bytes) = assemble(value_format1, value_format2, sets) {
        return vec![RewrittenSubtable { bytes }];
    }
    let mut pieces = Vec::new();
    let mut first = 0;
    while first < sets.len() {
        let end = run_end(sets, first);
        match assemble(value_format1, value_format2, &sets[first..end]) {
            Some(bytes) => pieces.push(RewrittenSubtable { bytes }),
            None => {
                ctx.offsets.record();
                return Vec::new();
            }
        }
        first = end;
    }
    pieces
}

/// End of the longest run of sets from `first` whose last PairSet
/// starts within 64 KiB of the subtable, counting every PairSet in
/// full (no sharing) and the Coverage at its largest (format 1). At
/// least one set, so the split always makes progress.
fn run_end(sets: &[(u16, Vec<u8>)], first: usize) -> usize {
    let mut bodies = 0usize;
    let mut end = first;
    while end < sets.len() {
        let count = end - first + 1;
        // Header, offsets, Coverage header and glyph array, bodies.
        let start = 10 + count * 2 + 4 + count * 2 + bodies;
        if start > usize::from(u16::MAX) && end > first {
            break;
        }
        bodies += sets[end].1.len();
        end += 1;
    }
    end
}

/// Serializes one subtable; `None` when a PairSet offset does not fit.
fn assemble(value_format1: u16, value_format2: u16, sets: &[(u16, Vec<u8>)]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset
    out.extend_from_slice(&value_format1.to_be_bytes());
    out.extend_from_slice(&value_format2.to_be_bytes());
    out.extend_from_slice(&(sets.len() as u16).to_be_bytes());
    out.resize(10 + sets.len() * 2, 0);
    let coverage = u16::try_from(out.len()).ok()?;
    out[2..4].copy_from_slice(&coverage.to_be_bytes());
    let glyphs: Vec<u16> = sets.iter().map(|&(gid, _)| gid).collect();
    out.extend_from_slice(&emit_coverage_from_glyphs(&glyphs));
    let mut bodies = Dedup::default();
    for (i, (_, body)) in sets.iter().enumerate() {
        let at = u16::try_from(bodies.place(&mut out, body)).ok()?;
        out[10 + i * 2..12 + i * 2].copy_from_slice(&at.to_be_bytes());
    }
    Some(out)
}
