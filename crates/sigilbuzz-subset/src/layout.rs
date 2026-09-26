//! Layout-table (`GSUB` / `GPOS` / `GDEF`) emission.
//!
//! The subsetter ships byte-level rewriters keyed on the new
//! gid namespace. The rewriters live in [`crate::gsub`],
//! [`crate::gpos`], and [`crate::gdef`]; this module owns the
//! infrastructure they share:
//!
//! - [`GidMap`]: the old->new gid translator built once per subset.
//! - [`RewriterCtx`]: the borrow bag passed to per-lookup-type
//!   rewriters.
//! - [`RewrittenLookup`] / [`RewrittenSubtable`]: the value types
//!   the per-lookup-type rewriters produce.
//! - [`parse_coverage_glyphs`] / [`parse_classdef_pairs_from_bytes`]: small
//!   byte-walking helpers for the rewriters and the closure walker.
//! - [`build_gsub`] / [`build_gpos`]: the drivers that walk every
//!   lookup, call the per-type rewriter, run the drop cascade, and
//!   renumber surviving lookups.
//!
//! ## Drop cascade
//!
//! After every per-lookup-type rewriter has run we walk the result
//! to drop:
//!
//! 1. Lookups whose every subtable rewrote to empty.
//! 2. Features that name no surviving lookup, unless a FeatureVariations
//!    alternate still gives them one.
//! 3. Scripts whose every feature dropped.
//! 4. The container table entirely if no script survives.
//!
//! Surviving lookups are then renumbered to 0..N. Every reference
//! (the FeatureList's lookup-index list, and the feature and lookup
//! indices inside the FeatureVariations of a 1.1 table) is rewritten
//! through the same renumber maps; see [`crate::feature_variations`].
//!
//! # Coverage matrix today
//!
//! - **GSUB type 1 (single-sub)**: full byte-level rewriter (formats
//!   1+2). See [`crate::gsub`].
//! - **GSUB types 2-6 + 8**: full byte-level rewriters. See [`crate::gsub`].
//! - **GSUB type 7 (extension)**: pass-through, recurses into the
//!   inner subtable.
//! - **Any GSUB lookup type without a rewriter**: returns `None` for
//!   every subtable. The drop cascade handles propagation.
//! - **GPOS types 1 (single-adj), 2 (pair-adj: fmt 1 + fmt 2 with
//!   class-collapse fmt-1 fallback), 3 (cursive), 4 / 5 / 6 (mark
//!   attachment), 7 (context), 8 (chained context), 9 (extension)**:
//!   full byte-level rewriters. See [`crate::gpos`].
//! - **GDEF**: GlyphClassDef and MarkAttachClassDef through the
//!   ClassDef rewriter, AttachList and LigCaretList remapped through
//!   their Coverage, MarkGlyphSetsDef remapped set by set with stable
//!   indices, and the ItemVariationStore carried verbatim when
//!   variations are retained. See [`crate::gdef`].
//!
//! Per-type rewriters slot in here one at a time; the per-type module
//! call sites are stable, so adding a new lookup-type rewriter does
//! not require touching this module.
//!
//! ## Work budget
//!
//! Offsets in a layout table may point many records at the same
//! bytes, so a small table can describe billions of rules. The
//! [`GidMap`] carries a [`WorkBudget`] that every rewriter charges for
//! the records it visits and the bytes it emits. When the budget runs
//! out the driver drops the whole table and reports it through the
//! subset warnings.
//!
//! ## Offset overflow
//!
//! Every emitted offset is range-checked (see [`crate::offset16`]). A
//! rebuilt subtable that no longer fits its 16-bit offsets fails the
//! subset, after the mark attachment and PairPos rewriters have tried
//! splitting it. When the LookupList itself overflows,
//! [`crate::lookup_list`] moves every subtable behind an Extension
//! lookup, which uses 32-bit offsets.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use crate::offset16::Offset16Guard;
use crate::util::{WorkBudget, WORK_LIMIT};
use crate::warnings::{Diag, Warnings};
use crate::{gdef, GlyphId, SubsetError, SubsetInput};

mod bytes;
mod driver;
mod lists;

use bytes::parse_classdef_pairs_from_bytes;
pub(crate) use bytes::{extension_target, parse_coverage_glyphs, read_u16};
pub(crate) use driver::{build_gpos, build_gsub};

/// A valid Coverage or ClassDef lists each glyph at most once, so it
/// never names more than this many glyphs. The byte walkers stop
/// there, which bounds their output on overlapping ranges.
pub(crate) const MAX_GLYPH_ENTRIES: usize = 1 << 16;

/// A new-namespace gid translator. `map(old) -> Some(new)` when the
/// gid is kept, `None` when it has been dropped.
///
/// Built once per subset from the closure walker's kept-gid set. It
/// also carries the work budget the layout rewriters charge (see the
/// module docs).
pub(crate) struct GidMap {
    /// Indexed by old gid; `None` means the gid was dropped.
    table: Vec<Option<u16>>,
    /// Number of `Some` entries in `table`.
    kept_len: usize,
    /// Work left for the current table rewrite.
    budget: WorkBudget,
}

impl GidMap {
    /// Builds a [`GidMap`] from a sorted-ascending kept-gid set. The
    /// kept set's position-in-vector becomes the new gid (so the
    /// 0th kept gid maps to new-gid 0, the 1st kept gid maps to
    /// new-gid 1, and so on, matching the convention `subset()` uses
    /// for `gid_map`).
    pub(crate) fn from_kept(kept: &[GlyphId]) -> Self {
        let max = kept.iter().copied().max().unwrap_or(0);
        let mut table = alloc::vec![None; max as usize + 1];
        for (new, &old) in kept.iter().enumerate() {
            if let Some(slot) = table.get_mut(old as usize) {
                *slot = Some(new as u16);
            }
        }
        Self::with_table(table)
    }

    #[cfg(test)]
    pub(crate) fn from_table(table: Vec<Option<u16>>) -> Self {
        Self::with_table(table)
    }

    fn with_table(table: Vec<Option<u16>>) -> Self {
        let kept_len = table.iter().filter(|slot| slot.is_some()).count();
        Self {
            table,
            kept_len,
            budget: WorkBudget::new(WORK_LIMIT),
        }
    }

    /// Number of kept gids, the length of [`GidMap::iter_kept`].
    pub(crate) fn kept_len(&self) -> usize {
        self.kept_len
    }

    /// Translates `old` to its new gid, or `None` if dropped.
    #[must_use]
    pub(crate) fn map(&self, old: u16) -> Option<u16> {
        self.table.get(old as usize).copied().flatten()
    }

    /// Charges `units` of work to the rewrite budget. Returns false once
    /// the budget is spent. The caller should stop and drop its output.
    pub(crate) fn spend(&self, units: usize) -> bool {
        self.budget.spend(units)
    }

    /// True once the rewrite budget has run out.
    pub(crate) fn budget_spent(&self) -> bool {
        self.budget.is_spent()
    }

    /// Refills the rewrite budget before the next table.
    pub(crate) fn reset_budget(&self) {
        self.budget.reset(WORK_LIMIT);
    }

    /// Enumerates a Coverage table's glyphs (see
    /// [`parse_coverage_glyphs`]) and charges the budget for them.
    /// Returns `None` once the budget is spent.
    pub(crate) fn coverage_glyphs(&self, bytes: &[u8]) -> Option<Vec<u16>> {
        if self.budget_spent() {
            return None;
        }
        let glyphs = parse_coverage_glyphs(bytes);
        self.spend(glyphs.len() + 1).then_some(glyphs)
    }

    /// Enumerates a ClassDef table's `(gid, class)` pairs (see
    /// [`parse_classdef_pairs_from_bytes`]) and charges the budget for
    /// them. Returns `None` once the budget is spent.
    pub(crate) fn classdef_pairs(&self, bytes: &[u8]) -> Option<Vec<(u16, u16)>> {
        if self.budget_spent() {
            return None;
        }
        let pairs = parse_classdef_pairs_from_bytes(bytes);
        self.spend(pairs.len() + 1).then_some(pairs)
    }

    /// The `(gid, class)` pairs of the ClassDef that `offset` points at
    /// inside `sub`, class 0 left out, charged to the budget like
    /// [`GidMap::classdef_pairs`]. A null offset is the spec's empty
    /// ClassDef, every glyph in class 0, so it yields no pairs. fontmake
    /// leaves the backtrack ClassDef of chained context format 2 null
    /// this way. Returns `None` for an offset past the end of
    /// `sub` or once the budget is spent.
    pub(crate) fn classdef_pairs_at(&self, sub: &[u8], offset: usize) -> Option<Vec<(u16, u16)>> {
        if offset == 0 {
            return Some(Vec::new());
        }
        self.classdef_pairs(sub.get(offset..)?)
    }

    /// Iterates over every kept `(old_gid, new_gid)` pair in old-gid
    /// order. Used by the PairPos fmt-1 fallback to enumerate the
    /// surviving second-glyph universe (Coverage gates the first axis;
    /// the second axis must consider every kept gid because class-0 in
    /// the source classDef2 carries kerning too).
    pub(crate) fn iter_kept(&self) -> impl Iterator<Item = (u16, u16)> + '_ {
        self.table
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.map(|new| (i as u16, new)))
    }

    /// True when every kept gid maps to itself (the identity case the
    /// fast path in [`crate::subset`] still uses). Currently only
    /// exercised by tests; the [`decide`] driver checks identity from
    /// the kept-gid set directly to avoid building the GidMap when the
    /// fast path can be taken.
    #[cfg(test)]
    pub(crate) fn is_identity(&self) -> bool {
        self.table
            .iter()
            .enumerate()
            .all(|(i, slot)| matches!(slot, Some(g) if *g as usize == i))
    }
}

/// Borrow bag passed into per-lookup-type rewriters.
pub(crate) struct RewriterCtx<'a> {
    pub gid_map: &'a GidMap,
    /// Optional old -> new lookup-index map. Set during the second
    /// pass over context-style lookups (GSUB types 5 / 6, GPOS types
    /// 7 / 8) so their nested lookup records can be patched. `None` on
    /// the first pass. Context rewriters preserve the source's
    /// lookup-list indices unchanged so the caller can decide what
    /// survives and rebuild the renumber map afterwards.
    pub lookup_renumber: Option<&'a [Option<u16>]>,
    /// Whether rebuilt GPOS subtables copy the VariationIndex tables
    /// their anchors and ValueRecords name. Off for a static subset,
    /// which drops the GDEF ItemVariationStore they would point into;
    /// the slots are cleared instead, and the tables take no space.
    pub keep_variations: bool,
    /// Offset16s of the rebuilt subtables that could not reach their
    /// targets. The lookup rewriters check it after every subtable.
    pub offsets: Offset16Guard,
    /// Where malformed pieces of the source table are reported. The
    /// table drivers point it at the table they rewrite.
    pub diag: Diag<'a>,
}

impl<'a> RewriterCtx<'a> {
    pub(crate) fn new(gid_map: &'a GidMap, lookup_renumber: Option<&'a [Option<u16>]>) -> Self {
        Self {
            gid_map,
            lookup_renumber,
            keep_variations: true,
            offsets: Offset16Guard::default(),
            diag: Diag::NONE,
        }
    }

    /// Narrows a distance inside a rebuilt subtable to an Offset16,
    /// recording an overflow in [`RewriterCtx::offsets`].
    pub(crate) fn off16(&self, distance: usize) -> u16 {
        self.offsets.narrow(distance)
    }

    /// Narrows an entry count of a rebuilt subtable to a u16, recording
    /// an overflow in [`RewriterCtx::offsets`] the way
    /// [`RewriterCtx::off16`] does for offsets.
    pub(crate) fn count16(&self, count: usize) -> u16 {
        self.offsets.narrow(count)
    }
}

/// One subtable's worth of rewritten bytes.
pub(crate) struct RewrittenSubtable {
    pub bytes: Vec<u8>,
}

/// One lookup's worth of rewritten state, ready to be assembled into a
/// LookupList.
pub(crate) struct RewrittenLookup {
    pub lookup_type: u16,
    pub lookup_flag: u16,
    pub mark_filtering_set: Option<u16>,
    pub subtables: Vec<RewrittenSubtable>,
}

/// Decision about a layout table's fate in the output.
pub(crate) enum Decision {
    /// Pass the source bytes through verbatim.
    Preserve,
    /// Substitute these freshly-built bytes.
    Rewrite(Vec<u8>),
    /// Omit the table from the output.
    Drop,
}

pub(crate) struct LayoutPlan {
    pub gsub: Decision,
    pub gpos: Decision,
    pub gdef: Decision,
}

/// Decides what to do with each layout table given the kept-gid set.
/// Malformed pieces the rewriters leave out are reported to `warnings`.
pub(crate) fn decide(
    face: &Face<'_>,
    kept: &[GlyphId],
    input: &SubsetInput,
    warnings: &Warnings,
) -> Result<LayoutPlan, SubsetError> {
    let has_gsub = face.record(tag::GSUB).is_some();
    let has_gpos = face.record(tag::GPOS).is_some();
    let has_gdef = face.record(tag::GDEF).is_some();

    if !(has_gsub || has_gpos || has_gdef) {
        return Ok(LayoutPlan {
            gsub: Decision::Drop,
            gpos: Decision::Drop,
            gdef: Decision::Drop,
        });
    }

    if !input.retain_layout {
        return Ok(LayoutPlan {
            gsub: Decision::Drop,
            gpos: Decision::Drop,
            gdef: Decision::Drop,
        });
    }

    // Identity check: when the kept-gid set is exactly 0..num_glyphs we
    // can pass the layout tables through verbatim, no rewriter needed.
    let num_glyphs = face.maxp()?.num_glyphs as usize;
    let identity =
        kept.len() == num_glyphs && kept.iter().enumerate().all(|(i, &g)| g as usize == i);

    if identity {
        // A static subset keeps no ItemVariationStore, so even here the
        // GPOS VariationIndex slots are cleared and a GDEF carrying a
        // store is rebuilt without it (and without caret variations).
        let statics = !input.retain_variations;
        let gpos = if !has_gpos {
            Decision::Drop
        } else if statics {
            let mut bytes = face.table_bytes(tag::GPOS)?.to_vec();
            crate::gpos_var::strip_variation_indices(&mut bytes);
            Decision::Rewrite(bytes)
        } else {
            Decision::Preserve
        };
        let gdef_store = face.table_bytes(tag::GDEF).is_ok_and(|gdef| {
            let minor = gdef
                .get(2..4)
                .map_or(0, |b| u16::from_be_bytes([b[0], b[1]]));
            minor >= 3 && gdef.get(14..18).is_some_and(|off| off != [0; 4])
        });
        let gdef = if !has_gdef {
            Decision::Drop
        } else if statics && gdef_store {
            match gdef::rewrite_gdef(face, &GidMap::from_kept(kept), false, warnings)? {
                Some(b) => Decision::Rewrite(b),
                None => Decision::Drop,
            }
        } else {
            Decision::Preserve
        };
        return Ok(LayoutPlan {
            gsub: if has_gsub {
                Decision::Preserve
            } else {
                Decision::Drop
            },
            gpos,
            gdef,
        });
    }

    // Non-identity: invoke the rewriter. Each driver returns either
    // bytes to substitute or None when the whole table dropped. A
    // static subset's GPOS never copies VariationIndex tables: the
    // GDEF ItemVariationStore they would name is dropped.
    let map = GidMap::from_kept(kept);
    let ctx = RewriterCtx {
        keep_variations: input.retain_variations,
        diag: Diag::new(warnings),
        ..RewriterCtx::new(&map, None)
    };

    let gsub = if has_gsub {
        match build_gsub(face, &ctx)? {
            Some(b) => Decision::Rewrite(b),
            None => Decision::Drop,
        }
    } else {
        Decision::Drop
    };
    let gpos = if has_gpos {
        match build_gpos(face, &ctx)? {
            Some(b) => Decision::Rewrite(b),
            None => Decision::Drop,
        }
    } else {
        Decision::Drop
    };
    let gdef = if has_gdef {
        map.reset_budget();
        match gdef::rewrite_gdef(face, &map, input.retain_variations, warnings)? {
            Some(b) => Decision::Rewrite(b),
            None => Decision::Drop,
        }
    } else {
        Decision::Drop
    };

    Ok(LayoutPlan { gsub, gpos, gdef })
}

#[cfg(test)]
mod static_tests;

#[cfg(test)]
mod truncation_tests;

#[cfg(test)]
mod warning_tests;

#[cfg(test)]
mod tests;
