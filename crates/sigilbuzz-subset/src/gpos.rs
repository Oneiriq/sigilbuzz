//! GPOS byte-level rewriter.
//!
//! Walks every lookup in the source `GPOS` table and produces a new
//! `GPOS` whose Coverage / ClassDef / glyph references resolve against
//! the new gid namespace defined by the caller's
//! [`GidMap`](crate::layout::GidMap). GPOS lookups don't introduce new
//! gids (they only reposition existing ones), so the rewriter is purely
//! a filter + remap pass over the per-type byte layout.
//!
//! # Per-lookup-type coverage
//!
//! As of this commit the rewriter ships byte-level support for:
//!
//! - **Type 1 (single-adj)**: formats 1 (uniform ValueRecord) and 2
//!   (per-glyph ValueRecord array). Filters Coverage; for fmt 2 drops
//!   the corresponding ValueRecord slots in lockstep. The ValueRecord
//!   bytes themselves travel verbatim. They contain no gid references.
//! - **Type 2 (pair-adj) format 1**: explicit pair entries. Filters
//!   Coverage of the first glyph; for each surviving PairSet, walks
//!   PairValueRecords and drops pairs whose `secondGlyph` was dropped.
//!   Empty PairSets collapse to a Coverage drop. A subtable too big
//!   for its 16-bit offsets is split into runs of first glyphs (see
//!   [`pair_sets`]).
//! - **Type 2 (pair-adj) format 2**: class-based matrix. The rewriter
//!   has two strategies: a fast pass-through that preserves source
//!   class IDs and the matrix bytes verbatim (filtering both ClassDefs
//!   through the GidMap), and a fmt-1 fallback that synthesizes an
//!   explicit pair table from the kept-gid cross-product when class
//!   collapse would otherwise leave the matrix carrying rows/columns
//!   that no surviving gid can reach. The driver picks the fmt-1
//!   fallback for small subsets (<= 8 surviving first-glyphs or kept
//!   first x kept second cross <= 256 cells) and the pass-through path
//!   for larger ones.
//! - **Type 3 (cursive)**: Coverage + EntryExitRecord array. Filters
//!   Coverage; drops corresponding entry/exit slots. Anchors carry no
//!   gid references and are copied whole (identical anchors share one
//!   copy).
//! - **Type 4 / 5 / 6 (mark attachment)**: Mark+Base / Mark+Liga /
//!   Mark1+Mark2 Coverages with parallel MarkArray and BaseArray /
//!   LigatureArray / Mark2Array entries. Filters both Coverages, drops
//!   array entries in lockstep. Anchors and class IDs are copied whole.
//!   A subtable too big for its 16-bit offsets is split by mark class,
//!   and by base glyph within a class (see [`mark_attach`]).
//! - **Type 9 (extension)**: pass-through after rewriting the inner
//!   subtable. Falls back to a lookup drop when the inner type has no
//!   rewriter.
//!
//! - **Type 7 (context positioning)**: formats 1 / 2 / 3, mirroring
//!   the GSUB type-5 byte-level rewriter. Nested `PosLookupRecord`s
//!   are renumbered through the GPOS lookup-list renumber map driven
//!   by the two-pass build in [`crate::layout::build_gpos`]. Rules
//!   left without records (`ignore pos`) are kept, since they stop
//!   the later rules of their lookup from matching.
//! - **Type 8 (chained context positioning)**: formats 1 / 2 / 3,
//!   mirroring the GSUB type-6 byte-level rewriter. Same driver hook
//!   as type 7 for the lookup-renumber pass.
//!
//! # Device and VariationIndex tables
//!
//! ValueRecords and AnchorFormat3 records reach `Device` /
//! `VariationIndex` tables through offsets measured from their parent
//! table (the subtable, the PairSet, or the Anchor). Every rebuilt
//! parent copies the tables its records reference and re-points the
//! offsets; see [`crate::device`].

mod mark_attach;
mod pair_sets;

use alloc::vec::Vec;

use sigilbuzz::tables::gpos::lookup_type as gpos_type;

use crate::layout::{extension_target, RewriterCtx, RewrittenLookup, RewrittenSubtable};
use crate::warnings::error_context;
use crate::SubsetError;
use mark_attach::{rewrite_mark_attach, MarkAttachKind};

mod chain_context;
mod context;
mod cursive;
mod pair_pos;
mod single_adj;

use chain_context::rewrite_chain_context_pos;
use context::rewrite_context_pos;
use cursive::rewrite_cursive;
use pair_pos::rewrite_pair_pos;
use single_adj::rewrite_single_adj;

/// Rewrites a single GPOS lookup. Returns `None` if the lookup has no
/// surviving subtables after rewriting (drop cascade will remove the
/// lookup), and an error when a rebuilt subtable outgrows its 16-bit
/// offsets even after splitting (see [`crate::offset16`]). A subtable
/// left out because it cannot be read is reported through `ctx.diag`.
pub(crate) fn rewrite_lookup(
    ctx: &RewriterCtx,
    lookup_type: u16,
    lookup_flag: u16,
    mark_filtering_set: Option<u16>,
    subtable_bodies: &[&[u8]],
) -> Result<Option<RewrittenLookup>, SubsetError> {
    let mut rewritten_subs: Vec<RewrittenSubtable> = Vec::new();

    for &sub_bytes in subtable_bodies {
        let rewritten = rewrite_subtable(ctx, lookup_type, sub_bytes);
        ctx.offsets
            .check(overflow_context(lookup_type, sub_bytes))?;
        if rewritten.is_empty() {
            report_unreadable(ctx, lookup_type, sub_bytes);
        }
        rewritten_subs.extend(rewritten);
    }

    if rewritten_subs.is_empty() {
        return Ok(None);
    }

    Ok(Some(RewrittenLookup {
        lookup_type,
        lookup_flag,
        mark_filtering_set,
        subtables: rewritten_subs,
    }))
}

/// Reports a subtable the rewrite left out when the shaper's own parser
/// rejects it as well: it went for being malformed, or for a lookup
/// type no shaper applies, not for losing every glyph. The parser
/// measures nested errors from the nested table, so the warning sits
/// at the start of the subtable, with the parser's reason.
fn report_unreadable(ctx: &RewriterCtx, lookup_type: u16, sub: &[u8]) {
    if let Err(e) = parse_subtable(lookup_type, sub) {
        ctx.diag
            .in_part(sub, 0, error_context(&e), "a lookup subtable");
    }
}

/// Parses `sub`, a subtable of a lookup of `lookup_type`, with the
/// shaper's parser, looking through an Extension wrapper.
fn parse_subtable(lookup_type: u16, sub: &[u8]) -> Result<(), sigilbuzz::Error> {
    use sigilbuzz::tables::gpos as parser;
    match lookup_type {
        gpos_type::SINGLE_ADJUSTMENT => parser::SinglePos::parse(sub).map(drop),
        gpos_type::PAIR_ADJUSTMENT => parser::PairPos::parse(sub).map(drop),
        gpos_type::CURSIVE_ATTACHMENT => parser::CursivePos::parse(sub).map(drop),
        gpos_type::MARK_TO_BASE => parser::MarkBasePos::parse(sub).map(drop),
        gpos_type::MARK_TO_LIGATURE => parser::MarkLigaPos::parse(sub).map(drop),
        gpos_type::MARK_TO_MARK => parser::MarkMarkPos::parse(sub).map(drop),
        gpos_type::CONTEXT => parser::ContextPos::parse(sub).map(drop),
        gpos_type::CHAINED_CONTEXT => parser::ChainContextPos::parse(sub).map(drop),
        gpos_type::EXTENSION => match extension_target(sub)? {
            (gpos_type::EXTENSION, _) => Err(sigilbuzz::Error::Malformed {
                offset: 2,
                context: "Extension subtable wraps another Extension",
            }),
            (inner_type, inner) => parse_subtable(inner_type, inner),
        },
        _ => Err(sigilbuzz::Error::Malformed {
            offset: 0,
            context: "unknown GPOS lookup type",
        }),
    }
}

/// Names the subtable type an Offset16 overflow is reported against,
/// looking through an Extension wrapper.
fn overflow_context(lookup_type: u16, sub: &[u8]) -> &'static str {
    let kind = match (lookup_type, sub.get(2..4)) {
        (gpos_type::EXTENSION, Some(inner)) => u16::from_be_bytes([inner[0], inner[1]]),
        _ => lookup_type,
    };
    match kind {
        gpos_type::SINGLE_ADJUSTMENT => "GPOS SinglePos rewrite: an offset exceeds 64 KiB",
        gpos_type::PAIR_ADJUSTMENT => "GPOS PairPos rewrite: an offset exceeds 64 KiB",
        gpos_type::CURSIVE_ATTACHMENT => "GPOS CursivePos rewrite: an offset exceeds 64 KiB",
        gpos_type::MARK_TO_BASE => "GPOS MarkBasePos rewrite: one mark class and base overflow",
        gpos_type::MARK_TO_LIGATURE => {
            "GPOS MarkLigPos rewrite: one mark class and ligature overflow"
        }
        gpos_type::MARK_TO_MARK => "GPOS MarkMarkPos rewrite: one mark class and mark overflow",
        gpos_type::CONTEXT => "GPOS ContextPos rewrite: an offset exceeds 64 KiB",
        _ => "GPOS ChainContextPos rewrite: an offset exceeds 64 KiB",
    }
}

/// Rewrites one subtable. Mark attachment and PairPos format 1
/// subtables may come back split into several; every other type
/// yields at most one.
fn rewrite_subtable(ctx: &RewriterCtx, lookup_type: u16, sub: &[u8]) -> Vec<RewrittenSubtable> {
    match lookup_type {
        gpos_type::SINGLE_ADJUSTMENT => rewrite_single_adj(ctx, sub).into_iter().collect(),
        gpos_type::PAIR_ADJUSTMENT => rewrite_pair_pos(ctx, sub),
        gpos_type::CURSIVE_ATTACHMENT => rewrite_cursive(ctx, sub).into_iter().collect(),
        gpos_type::MARK_TO_BASE | gpos_type::MARK_TO_MARK => {
            rewrite_mark_attach(ctx, sub, MarkAttachKind::FixedClassRow)
        }
        gpos_type::MARK_TO_LIGATURE => {
            rewrite_mark_attach(ctx, sub, MarkAttachKind::LigatureAttach)
        }
        gpos_type::CONTEXT => rewrite_context_pos(ctx, sub).into_iter().collect(),
        gpos_type::CHAINED_CONTEXT => rewrite_chain_context_pos(ctx, sub).into_iter().collect(),
        gpos_type::EXTENSION => rewrite_extension(ctx, sub),
        _ => Vec::new(),
    }
}

/// Returns the effective GPOS lookup type when the lookup is a
/// context-family type (7 or 8), including the case where it is
/// wrapped in an Extension lookup (type 9). Returns `None` otherwise.
///
/// Used by the GPOS driver to identify lookups whose nested
/// `PosLookupRecord` indices need to be patched once the lookup-list
/// renumber map is known. Mirrors GSUB's `context_lookup_type`.
pub(crate) fn context_lookup_type(lookup: &sigilbuzz::tables::layout::Lookup<'_>) -> Option<u16> {
    let lt = unwrap_extension_lookup_type(lookup);
    match lt {
        gpos_type::CONTEXT | gpos_type::CHAINED_CONTEXT => Some(lt),
        _ => None,
    }
}

/// Peels a single Extension wrapper to expose the inner lookup type.
/// Returns the lookup's own type when no Extension is present, the
/// inner extensionLookupType otherwise.
fn unwrap_extension_lookup_type(lookup: &sigilbuzz::tables::layout::Lookup<'_>) -> u16 {
    if lookup.lookup_type() != gpos_type::EXTENSION {
        return lookup.lookup_type();
    }
    let Some(sub) = lookup.subtable_bytes(0) else {
        return lookup.lookup_type();
    };
    if sub.len() < 4 {
        return lookup.lookup_type();
    }
    u16::from_be_bytes([sub[2], sub[3]])
}

// ---------------------------------------------------------------------------
// Type 9: Extension Positioning
// ---------------------------------------------------------------------------

/// Rewrites a GPOS Extension subtable. Wrapper layout:
///
/// ```text
///   u16 format = 1
///   u16 extensionLookupType
///   u32 extensionOffset           (relative to extension subtable)
/// ```
///
/// Recurses into the inner subtable using the wrapped lookup type, then
/// re-emits a fresh Extension wrapper around each rewritten inner
/// subtable (a split inner subtable gets one wrapper per piece).
fn rewrite_extension(ctx: &RewriterCtx, sub: &[u8]) -> Vec<RewrittenSubtable> {
    let (Some(&[0, 1]), Some(kind), Some(off)) = (sub.get(..2), sub.get(2..4), sub.get(4..8))
    else {
        return Vec::new();
    };
    let inner_type = u16::from_be_bytes([kind[0], kind[1]]);
    let inner_off = u32::from_be_bytes([off[0], off[1], off[2], off[3]]) as usize;
    // Spec disallows Extension referring to Extension.
    let Some(inner) = sub
        .get(inner_off..)
        .filter(|_| inner_type != gpos_type::EXTENSION)
    else {
        return Vec::new();
    };
    rewrite_subtable(ctx, inner_type, inner)
        .into_iter()
        .map(|piece| {
            // Re-wrap: inner sits at offset 8 in the new wrapper.
            let mut out = Vec::with_capacity(8 + piece.bytes.len());
            out.extend_from_slice(&1u16.to_be_bytes());
            out.extend_from_slice(&inner_type.to_be_bytes());
            out.extend_from_slice(&8u32.to_be_bytes());
            out.extend_from_slice(&piece.bytes);
            RewrittenSubtable { bytes: out }
        })
        .collect()
}

#[cfg(test)]
mod device_tests;

#[cfg(test)]
mod split_tests;

#[cfg(test)]
mod tests;
