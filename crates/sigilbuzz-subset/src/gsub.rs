//! GSUB byte-level rewriter.
//!
//! Walks every lookup in the source `GSUB` table and produces a new
//! `GSUB` whose Coverage / ClassDef / substitution-target gid
//! references resolve against the new gid namespace defined by the
//! caller's [`GidMap`](crate::layout::GidMap).
//!
//! # Per-lookup-type coverage
//!
//! As of this commit the rewriter ships byte-level support for:
//!
//! - **Type 1 (single-sub)**: formats 1 (delta) and 2 (explicit). Auto-
//!   selects between formats; falls back to format 2 when a remapped
//!   delta would no longer produce contiguous targets.
//! - **Type 2 (multiple-sub)**: format 1. Filters Coverage to surviving
//!   input gids; drops any Sequence whose substitute glyphs are not all
//!   kept (a partial sequence would emit a missing gid), and drops the
//!   subtable when Coverage empties out.
//! - **Type 3 (alternate-sub)**: format 1. Filters Coverage to surviving
//!   input gids; remaps each AlternateSet's surviving alternates;
//!   drops the AlternateSet (and its Coverage entry) when every
//!   alternate dies, and drops the subtable when Coverage empties out.
//! - **Type 4 (ligature-sub)**: format 1. Filters Coverage to surviving
//!   first-component gids, drops any Ligature whose result gid or any
//!   component gid is not kept, drops empty LigatureSets, and drops the
//!   subtable when Coverage empties out. Result + component gids are
//!   remapped through the GidMap.
//! - **Type 5 (context)**: formats 1 (rule-based), 2 (class-based), 3
//!   (coverage-based). Filters first-glyph Coverage / ClassDef / per-
//!   position Coverage through the GidMap; drops any Rule whose input
//!   tail loses a gid; drops nested `SubstLookupRecord`s whose target
//!   lookup dropped (renumber map is wired in by the GSUB driver in a
//!   second pass, see [`context_lookup_type`]). A Rule that ends up
//!   with no records is kept: it is an `ignore sub` rule, or behaves
//!   like one, and still shields the rules after it.
//! - **Type 6 (chained context)**: formats 1, 2, 3. Mirrors type 5
//!   with three sequences (backtrack / input / lookahead). Drops a
//!   ChainRule when any required gid in any of the three sequences is
//!   not kept.
//! - **Type 7 (extension)**: pass-through after rewriting the inner
//!   subtable. Only inner types this module implements are passed
//!   through; everything else drops the lookup.
//! - **Type 8 (reverse chain)**: format 1. Filters Coverage to
//!   surviving input gids whose substitute gid also survives; rewrites
//!   the backtrack and lookahead Coverage arrays; drops the subtable
//!   when any Coverage in the context window empties out. The closure
//!   keeps the substitute of every kept input whose context can still
//!   match (see [`pull_in_substitution_targets`]), so a pair only
//!   loses its substitute when its subtable drops anyway.
//!
//! Every other lookup type drops its lookup. The drop cascade then
//! removes empty subtables, lookups with no surviving subtable,
//! features that name no surviving lookup, and scripts whose features
//! have all been dropped. See [`super::layout`].
//!
//! Issue tracking the remaining lookup types: see the sibling issue
//! filed alongside this module.

use alloc::vec::Vec;

use sigilbuzz::tables::gsub::lookup_type as gsub_type;

use crate::layout::{extension_target, RewriterCtx, RewrittenLookup, RewrittenSubtable};
use crate::warnings::error_context;
use crate::SubsetError;

mod chain_context;
mod closure;
mod context;
mod ligature;
mod reverse_chain;
mod single;

use chain_context::rewrite_type6;
use context::rewrite_type5;
use ligature::rewrite_type4;
use reverse_chain::rewrite_type8;
use single::{rewrite_single, rewrite_type2, rewrite_type3};

pub(crate) use closure::pull_in_substitution_targets;
pub(crate) use context::{encode_lookup_records, parse_and_remap_lookup_records};

/// Rewrites a single GSUB lookup. Returns `None` if the lookup has no
/// surviving subtables after rewriting (drop cascade will remove the
/// lookup), and an error when a rebuilt subtable outgrows its 16-bit
/// offsets (see [`crate::offset16`]). A subtable left out because it
/// cannot be read is reported through `ctx.diag`.
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
        if rewritten.is_none() {
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
    use sigilbuzz::tables::gsub as parser;
    match lookup_type {
        gsub_type::SINGLE => parser::Single::parse(sub).map(drop),
        gsub_type::MULTIPLE => parser::Multiple::parse(sub).map(drop),
        gsub_type::ALTERNATE => parser::Alternate::parse(sub).map(drop),
        gsub_type::LIGATURE => parser::Ligature::parse(sub).map(drop),
        gsub_type::CONTEXT => parser::Context::parse(sub).map(drop),
        gsub_type::CHAINED_CONTEXT => parser::ChainContextAny::parse(sub).map(drop),
        gsub_type::REVERSE_CHAINED => parser::ReverseChain::parse(sub).map(drop),
        gsub_type::EXTENSION => match extension_target(sub)? {
            (gsub_type::EXTENSION, _) => Err(sigilbuzz::Error::Malformed {
                offset: 2,
                context: "Extension subtable wraps another Extension",
            }),
            (inner_type, inner) => parse_subtable(inner_type, inner),
        },
        _ => Err(sigilbuzz::Error::Malformed {
            offset: 0,
            context: "unknown GSUB lookup type",
        }),
    }
}

/// Names the subtable type an Offset16 overflow is reported against,
/// looking through an Extension wrapper.
fn overflow_context(lookup_type: u16, sub: &[u8]) -> &'static str {
    let kind = match (lookup_type, sub.get(2..4)) {
        (gsub_type::EXTENSION, Some(inner)) => u16::from_be_bytes([inner[0], inner[1]]),
        _ => lookup_type,
    };
    match kind {
        gsub_type::SINGLE => "GSUB SingleSubst rewrite: an offset exceeds 64 KiB",
        gsub_type::MULTIPLE => "GSUB MultipleSubst rewrite: an offset exceeds 64 KiB",
        gsub_type::ALTERNATE => "GSUB AlternateSubst rewrite: an offset exceeds 64 KiB",
        gsub_type::LIGATURE => "GSUB LigatureSubst rewrite: an offset exceeds 64 KiB",
        gsub_type::CONTEXT => "GSUB ContextSubst rewrite: an offset exceeds 64 KiB",
        gsub_type::CHAINED_CONTEXT => "GSUB ChainContextSubst rewrite: an offset exceeds 64 KiB",
        _ => "GSUB ReverseChainSingleSubst rewrite: an offset exceeds 64 KiB",
    }
}

fn rewrite_subtable(ctx: &RewriterCtx, lookup_type: u16, sub: &[u8]) -> Option<RewrittenSubtable> {
    match lookup_type {
        gsub_type::SINGLE => rewrite_single(ctx, sub),
        gsub_type::MULTIPLE => rewrite_type2(ctx, sub),
        gsub_type::ALTERNATE => rewrite_type3(ctx, sub),
        gsub_type::LIGATURE => rewrite_type4(ctx, sub),
        gsub_type::CONTEXT => rewrite_type5(ctx, sub),
        gsub_type::CHAINED_CONTEXT => rewrite_type6(ctx, sub),
        gsub_type::EXTENSION => rewrite_extension(ctx, sub),
        gsub_type::REVERSE_CHAINED => rewrite_type8(ctx, sub),
        // Other types drop until their byte-level rewriter ships.
        // The drop cascade in [`crate::layout`] handles propagating
        // the loss up through lookups / features / scripts.
        _ => None,
    }
}

/// Returns the effective GSUB lookup type when the lookup is a
/// context-family type (5, 6, or 8), including the case where it is
/// wrapped in an Extension lookup (type 7). Returns `None` otherwise.
///
/// Used by the GSUB driver to identify lookups whose nested
/// `SubstLookupRecord` indices need to be patched once the renumber
/// map is known.
pub(crate) fn context_lookup_type(lookup: &sigilbuzz::tables::layout::Lookup<'_>) -> Option<u16> {
    let lt = unwrap_extension_lookup_type(lookup);
    match lt {
        gsub_type::CONTEXT | gsub_type::CHAINED_CONTEXT | gsub_type::REVERSE_CHAINED => Some(lt),
        _ => None,
    }
}

/// Rewrites a GSUB type 7 (Extension) subtable. The Extension wrapper
/// is u16 format=1, u16 extensionLookupType, u32 extensionOffset. We
/// recurse into the inner subtable using the wrapped lookup type, then
/// re-emit a fresh Extension subtable around the rewritten inner bytes.
fn rewrite_extension(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    if sub.len() < 8 {
        return None;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    if format != 1 {
        return None;
    }
    let inner_type = u16::from_be_bytes([sub[2], sub[3]]);
    let inner_off = u32::from_be_bytes([sub[4], sub[5], sub[6], sub[7]]) as usize;
    if inner_type == gsub_type::EXTENSION {
        // Spec disallows Extension referring to Extension.
        return None;
    }
    let inner = sub.get(inner_off..)?;
    let rewritten_inner = rewrite_subtable(ctx, inner_type, inner)?;

    // Re-wrap. The extension subtable points the inner bytes at offset
    // 8 (immediately after the wrapper).
    let mut out = Vec::with_capacity(8 + rewritten_inner.bytes.len());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&inner_type.to_be_bytes());
    out.extend_from_slice(&8u32.to_be_bytes());
    out.extend_from_slice(&rewritten_inner.bytes);
    Some(RewrittenSubtable { bytes: out })
}

fn unwrap_extension_lookup_type(lookup: &sigilbuzz::tables::layout::Lookup<'_>) -> u16 {
    if lookup.lookup_type() != gsub_type::EXTENSION {
        return lookup.lookup_type();
    }
    let Some(sub) = lookup.subtable_bytes(0) else {
        return lookup.lookup_type();
    };
    if sub.len() < 8 {
        return lookup.lookup_type();
    }
    u16::from_be_bytes([sub[2], sub[3]])
}

#[cfg(test)]
mod tests;
