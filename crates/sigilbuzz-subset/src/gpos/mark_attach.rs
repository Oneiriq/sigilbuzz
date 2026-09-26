//! Mark attachment rewriter: MarkBasePos (type 4), MarkLigPos (type 5)
//! and MarkMarkPos (type 6).
//!
//! The three types share a 12-byte header:
//!
//! ```text
//!   u16      posFormat = 1
//!   Offset16 markCoverageOffset       (mark1 for type 6)
//!   Offset16 baseCoverageOffset       (ligature / mark2 for 5 / 6)
//!   u16      markClassCount
//!   Offset16 markArrayOffset
//!   Offset16 baseArrayOffset          (ligatureArray / mark2Array)
//!
//!   MarkArray:
//!     u16 markCount
//!     MarkRecord { u16 markClass, Offset16 markAnchorOffset }[markCount]
//!                                    (anchor offsets from the MarkArray)
//!   BaseArray / Mark2Array (types 4 / 6):
//!     u16 baseCount
//!     Offset16 anchorOffsets[baseCount][markClassCount]
//!                                    (from the array, 0 = no anchor)
//!   LigatureArray (type 5):
//!     u16 ligatureCount
//!     Offset16 ligatureAttachOffsets[ligatureCount]   (from the array)
//!   LigatureAttach:
//!     u16 componentCount
//!     Offset16 anchorOffsets[componentCount][markClassCount]
//!                                    (from the LigatureAttach)
//! ```
//!
//! Coverages are filtered through the gid map with the array entries
//! dropped in lockstep. Anchors carry no glyph ids and are copied with
//! their Device and VariationIndex tables (see
//! [`crate::device::copy_anchor`]); identical anchors share one copy
//! within each array.
//!
//! # Layout and splitting
//!
//! A rebuilt subtable is laid out header, both Coverages, MarkArray,
//! then the base-side array. The Coverages are small, so every
//! subtable-relative offset stays short, and the arrays reach their
//! anchors through offsets measured from the array itself. Only the
//! start of the base-side array has to lie within 64 KiB of the
//! subtable, and only its own anchors within 64 KiB of it.
//!
//! Private anchor device copies can still push a big subtable past
//! that. It is then split the way HarfBuzz's repacker splits it: into
//! pieces that each keep a run of consecutive mark classes (renumbered
//! from 0), the marks of those classes, and every base glyph that has
//! an anchor for one of them. A class too big even alone is split
//! further across runs of base glyphs, each piece repeating the class's
//! marks. The pieces take the original subtable's place in the lookup.
//!
//! Shaping does not change. A mark glyph has one class, so only the
//! pieces for its class cover it; every other piece rejects it at the
//! mark Coverage and the shaper moves on to the next subtable, as it
//! does when a covered base lacks an anchor. Among the pieces for its
//! class, the one covering the base supplies the source's anchors.

use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

use crate::coverage::emit_coverage_from_glyphs;
use crate::device::{copy_anchor, Dedup};
use crate::layout::{parse_coverage_glyphs, RewriterCtx, RewrittenSubtable};

/// Distinguishes the base-side array. Types 4 and 6 use a fixed
/// `markClassCount` row of anchor offsets per Coverage entry; type 5
/// uses one Offset16 per entry pointing at a LigatureAttach. The
/// caller passes it in: from the bytes alone a LigatureArray with one
/// mark class can look like a valid BaseArray.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(super) enum MarkAttachKind {
    /// Type 4 / 6: base / mark2 array.
    FixedClassRow,
    /// Type 5: ligature array.
    LigatureAttach,
}

/// A kept mark glyph.
struct Mark {
    gid: u16,
    class: u16,
    anchor: Vec<u8>,
}

/// A kept base, mark2 or ligature glyph: one row of `markClassCount`
/// anchor copies per ligature component, a single row for types 4 and
/// 6. An empty copy is a null anchor.
struct Base {
    gid: u16,
    rows: Vec<Vec<Vec<u8>>>,
}

/// Outcome of laying out one piece.
enum Piece {
    /// Nothing to attach: no mark or no base anchor in the piece.
    Empty,
    Fits(Vec<u8>),
    TooBig,
}

/// Rewrites a mark attachment subtable, split into several when one
/// cannot hold it (see the module docs). Empty when nothing survives.
/// A single class and base that cannot fit even alone are recorded in
/// [`RewriterCtx::offsets`].
pub(super) fn rewrite_mark_attach(
    ctx: &RewriterCtx,
    sub: &[u8],
    kind: MarkAttachKind,
) -> Vec<RewrittenSubtable> {
    let Some((class_count, marks, bases)) = read(ctx, sub, kind) else {
        return Vec::new();
    };
    match lay_out(kind, 0..class_count, &marks, &bases, false) {
        Piece::Fits(bytes) => return vec![RewrittenSubtable { bytes }],
        Piece::Empty => return Vec::new(),
        Piece::TooBig => {}
    }
    let mut pieces = Vec::new();
    let mut lo = 0;
    while lo < class_count {
        let by_class = longest_fit(usize::from(lo), usize::from(class_count), |a, b| {
            lay_out(kind, a as u16..b as u16, &marks, &bases, true)
        });
        if let Some((hi, piece)) = by_class {
            pieces.extend(piece.map(|bytes| RewrittenSubtable { bytes }));
            lo = hi as u16;
            continue;
        }
        // Class `lo` does not fit even alone: spread its bases.
        let mut first = 0;
        while first < bases.len() {
            let by_base = longest_fit(first, bases.len(), |a, b| {
                lay_out(kind, lo..lo + 1, &marks, &bases[a..b], true)
            });
            let Some((next, piece)) = by_base else {
                ctx.offsets.record();
                return Vec::new();
            };
            pieces.extend(piece.map(|bytes| RewrittenSubtable { bytes }));
            first = next;
        }
        lo += 1;
    }
    pieces
}

/// Finds a long run `start..end` (with `end` at most `limit`) that
/// `lay_out` fits, by doubling and then bisecting on the run length.
/// Returns the run's end and its piece (`None` when the run is empty),
/// or `None` when not even a run of one fits.
fn longest_fit(
    start: usize,
    limit: usize,
    mut lay_out: impl FnMut(usize, usize) -> Piece,
) -> Option<(usize, Option<Vec<u8>>)> {
    let mut try_len = |len: usize| match lay_out(start, start + len) {
        Piece::Fits(bytes) => Some(Some(bytes)),
        Piece::Empty => Some(None),
        Piece::TooBig => None,
    };
    let mut good = (1, try_len(1)?);
    let mut bad = None;
    while bad.is_none() && good.0 < limit - start {
        let len = (good.0 * 2).min(limit - start);
        match try_len(len) {
            Some(piece) => good = (len, piece),
            None => bad = Some(len),
        }
    }
    if let Some(mut bad) = bad {
        while bad - good.0 > 1 {
            let len = good.0 + (bad - good.0) / 2;
            match try_len(len) {
                Some(piece) => good = (len, piece),
                None => bad = len,
            }
        }
    }
    Some((start + good.0, good.1))
}

/// Reads the kept marks and base glyphs, sorted by new glyph id.
/// `None` drops the subtable: it is malformed, or no mark or no base
/// survives.
fn read(
    ctx: &RewriterCtx,
    sub: &[u8],
    kind: MarkAttachKind,
) -> Option<(u16, Vec<Mark>, Vec<Base>)> {
    let at = |pos: usize| -> Option<usize> {
        let bytes = sub.get(pos..pos + 2)?;
        Some(usize::from(u16::from_be_bytes([bytes[0], bytes[1]])))
    };
    if at(0)? != 1 {
        return None;
    }
    let class_count = u16::try_from(at(6)?).ok()?;
    let mcc = usize::from(class_count);
    let mark_glyphs = parse_coverage_glyphs(sub.get(at(2)?..)?);
    let base_glyphs = parse_coverage_glyphs(sub.get(at(4)?..)?);
    let map = ctx.gid_map;

    let mark_array = sub.get(at(8)?..)?;
    let mark_count = usize::from(u16::from_be_bytes([
        *mark_array.first()?,
        *mark_array.get(1)?,
    ]));
    if mark_array.len() < 2 + mark_count * 4 {
        return None;
    }
    let mut marks = Vec::new();
    for (i, &old) in mark_glyphs.iter().enumerate().take(mark_count) {
        let Some(gid) = map.map(old) else {
            continue;
        };
        let rec = 2 + i * 4;
        let class = u16::from_be_bytes([mark_array[rec], mark_array[rec + 1]]);
        let rel = u16::from_be_bytes([mark_array[rec + 2], mark_array[rec + 3]]);
        let anchor = copy_anchor(mark_array, usize::from(rel), ctx.keep_variations, &ctx.diag);
        // A class past markClassCount or a null anchor cannot attach.
        if class < class_count && !anchor.is_empty() {
            marks.push(Mark { gid, class, anchor });
        }
    }

    let array = sub.get(at(10)?..)?;
    let count = usize::from(u16::from_be_bytes([*array.first()?, *array.get(1)?]));
    let row = |table: &[u8], first: usize| -> Vec<Vec<u8>> {
        (0..mcc)
            .map(|c| {
                let slot = first + c * 2;
                let rel = u16::from_be_bytes([table[slot], table[slot + 1]]);
                copy_anchor(table, usize::from(rel), ctx.keep_variations, &ctx.diag)
            })
            .collect()
    };
    // Row sizes are summed in u64: markClassCount times the entry
    // count can pass what a 32-bit usize holds.
    let rows_fit =
        |table: &[u8], rows: usize| 2 + rows as u64 * mcc as u64 * 2 <= table.len() as u64;
    // A BaseArray too short for its rows drops the subtable.
    if kind == MarkAttachKind::FixedClassRow && !rows_fit(array, count) {
        return None;
    }
    let mut bases = Vec::new();
    for (i, &old) in base_glyphs.iter().enumerate().take(count) {
        let Some(gid) = map.map(old) else {
            continue;
        };
        let rows = match kind {
            MarkAttachKind::FixedClassRow => vec![row(array, 2 + i * mcc * 2)],
            MarkAttachKind::LigatureAttach => {
                let slot = 2 + i * 2;
                let Some(rel) = array.get(slot..slot + 2) else {
                    continue;
                };
                let rel = usize::from(u16::from_be_bytes([rel[0], rel[1]]));
                let Some(attach) = array.get(rel..).filter(|a| a.len() >= 2) else {
                    continue;
                };
                let components = usize::from(u16::from_be_bytes([attach[0], attach[1]]));
                if !rows_fit(attach, components) {
                    continue;
                }
                (0..components)
                    .map(|k| row(attach, 2 + k * mcc * 2))
                    .collect()
            }
        };
        bases.push(Base { gid, rows });
    }

    marks.sort_by_key(|m| m.gid);
    marks.dedup_by_key(|m| m.gid);
    bases.sort_by_key(|b| b.gid);
    bases.dedup_by_key(|b| b.gid);
    if marks.is_empty() || bases.is_empty() {
        return None;
    }
    Some((class_count, marks, bases))
}

/// Lays out one subtable holding the mark classes in `classes`
/// (renumbered from 0) and `bases`. With `split` set, a base without
/// an anchor for any of those classes is left out; the unsplit
/// subtable keeps every base like the source.
fn lay_out(
    kind: MarkAttachKind,
    classes: Range<u16>,
    marks: &[Mark],
    bases: &[Base],
    split: bool,
) -> Piece {
    let span = usize::from(classes.start)..usize::from(classes.end);
    let marks: Vec<&Mark> = marks
        .iter()
        .filter(|m| classes.contains(&m.class))
        .collect();
    let bases: Vec<&Base> = bases
        .iter()
        .filter(|b| {
            !split
                || b.rows
                    .iter()
                    .any(|r| r[span.clone()].iter().any(|a| !a.is_empty()))
        })
        .collect();
    if marks.is_empty() || bases.is_empty() {
        return Piece::Empty;
    }
    match assemble(kind, classes.start, span.len(), &marks, &bases) {
        Some(bytes) => Piece::Fits(bytes),
        None => Piece::TooBig,
    }
}

fn put(out: &mut [u8], slot: usize, distance: usize) -> Option<()> {
    let value = u16::try_from(distance).ok()?;
    out[slot..slot + 2].copy_from_slice(&value.to_be_bytes());
    Some(())
}

/// Serializes one subtable; `None` when an offset does not fit.
fn assemble(
    kind: MarkAttachKind,
    first_class: u16,
    width: usize,
    marks: &[&Mark],
    bases: &[&Base],
) -> Option<Vec<u8>> {
    let span = usize::from(first_class)..usize::from(first_class) + width;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.resize(12, 0);
    out[6..8].copy_from_slice(&u16::try_from(width).ok()?.to_be_bytes());

    // Coverages first: small, and reached from the subtable start.
    let mark_gids: Vec<u16> = marks.iter().map(|m| m.gid).collect();
    let at = out.len();
    put(&mut out, 2, at)?;
    out.extend_from_slice(&emit_coverage_from_glyphs(&mark_gids));
    let base_gids: Vec<u16> = bases.iter().map(|b| b.gid).collect();
    let at = out.len();
    put(&mut out, 4, at)?;
    out.extend_from_slice(&emit_coverage_from_glyphs(&base_gids));

    // MarkArray: records, then anchors measured from the array.
    let mark_array = out.len();
    put(&mut out, 8, mark_array)?;
    out.extend_from_slice(&(marks.len() as u16).to_be_bytes());
    out.resize(mark_array + 2 + marks.len() * 4, 0);
    let mut anchors = Dedup::default();
    for (i, mark) in marks.iter().enumerate() {
        let rec = mark_array + 2 + i * 4;
        out[rec..rec + 2].copy_from_slice(&(mark.class - first_class).to_be_bytes());
        let at = anchors.place(&mut out, &mark.anchor);
        put(&mut out, rec + 2, at - mark_array)?;
    }

    // The base-side array goes last: only its start must be reachable.
    let array = out.len();
    put(&mut out, 10, array)?;
    out.extend_from_slice(&(bases.len() as u16).to_be_bytes());
    match kind {
        MarkAttachKind::FixedClassRow => {
            out.resize(array + 2 + bases.len() * width * 2, 0);
            let mut anchors = Dedup::default();
            for (i, base) in bases.iter().enumerate() {
                for (c, anchor) in base.rows[0][span.clone()].iter().enumerate() {
                    if !anchor.is_empty() {
                        let at = anchors.place(&mut out, anchor);
                        put(&mut out, array + 2 + (i * width + c) * 2, at - array)?;
                    }
                }
            }
        }
        MarkAttachKind::LigatureAttach => {
            out.resize(array + 2 + bases.len() * 2, 0);
            let mut attaches = Dedup::default();
            for (i, base) in bases.iter().enumerate() {
                let attach = lig_attach(&base.rows, span.clone())?;
                let at = attaches.place(&mut out, &attach);
                put(&mut out, array + 2 + i * 2, at - array)?;
            }
        }
    }
    Some(out)
}

/// Serializes one LigatureAttach for the classes in `span`; `None` when
/// an anchor offset does not fit.
fn lig_attach(rows: &[Vec<Vec<u8>>], span: Range<usize>) -> Option<Vec<u8>> {
    let width = span.len();
    let mut out = Vec::new();
    out.extend_from_slice(&(rows.len() as u16).to_be_bytes());
    out.resize(2 + rows.len() * width * 2, 0);
    let mut anchors = Dedup::default();
    for (k, row) in rows.iter().enumerate() {
        for (c, anchor) in row[span.clone()].iter().enumerate() {
            if !anchor.is_empty() {
                let at = anchors.place(&mut out, anchor);
                put(&mut out, 2 + (k * width + c) * 2, at)?;
            }
        }
    }
    Some(out)
}
