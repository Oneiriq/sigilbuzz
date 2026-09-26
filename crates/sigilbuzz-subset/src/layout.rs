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

use alloc::vec::Vec;

use sigilbuzz::tables::layout::{FeatureList, Lookup, LookupList};
use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use crate::offset16::Offset16Guard;
use crate::warnings::{error_context, Diag, Warnings};
use crate::{feature_variations, gdef, gpos, gsub, GlyphId, SubsetError, SubsetInput};

/// A new-namespace gid translator. `map(old) -> Some(new)` when the
/// gid is kept, `None` when it has been dropped.
///
/// Built once per subset from the closure walker's kept-gid set.
pub(crate) struct GidMap {
    /// Indexed by old gid; `None` means the gid was dropped.
    table: Vec<Option<u16>>,
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
            table[old as usize] = Some(new as u16);
        }
        Self { table }
    }

    #[cfg(test)]
    pub(crate) fn from_table(table: Vec<Option<u16>>) -> Self {
        Self { table }
    }

    /// Translates `old` to its new gid, or `None` if dropped.
    #[must_use]
    pub(crate) fn map(&self, old: u16) -> Option<u16> {
        self.table.get(old as usize).copied().flatten()
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
    /// pass over context-style lookups (GSUB types 5 / 6) so their
    /// nested `SubstLookupRecord` entries can be patched. `None` on
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
        let gdef_store = face.table_bytes(tag::GDEF).ok().is_some_and(|gdef| {
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
        match gdef::rewrite_gdef(face, &map, input.retain_variations, warnings)? {
            Some(b) => Decision::Rewrite(b),
            None => Decision::Drop,
        }
    } else {
        Decision::Drop
    };

    Ok(LayoutPlan { gsub, gpos, gdef })
}

/// Signature of the per-lookup rewriters in [`crate::gsub`] and
/// [`crate::gpos`].
type LookupRewriter = fn(
    &RewriterCtx,
    u16,
    u16,
    Option<u16>,
    &[&[u8]],
) -> Result<Option<RewrittenLookup>, SubsetError>;

/// What differs between the GSUB and the GPOS driver.
struct LayoutKind {
    tag: [u8; 4],
    /// The Extension lookup type: 7 in GSUB, 9 in GPOS.
    extension_type: u16,
    /// Reads the LookupList and FeatureList with the shaper's parser.
    parse: fn(&[u8]) -> sigilbuzz::Result<(LookupList<'_>, FeatureList<'_>)>,
    rewrite_lookup: LookupRewriter,
    /// Picks out the lookups whose nested lookup records the second
    /// pass renumbers.
    context_lookup_type: fn(&Lookup<'_>) -> Option<u16>,
}

const GSUB_KIND: LayoutKind = LayoutKind {
    tag: tag::GSUB,
    extension_type: sigilbuzz::tables::gsub::lookup_type::EXTENSION,
    parse: parse_gsub_lists,
    rewrite_lookup: gsub::rewrite_lookup,
    context_lookup_type: gsub::context_lookup_type,
};

const GPOS_KIND: LayoutKind = LayoutKind {
    tag: tag::GPOS,
    extension_type: sigilbuzz::tables::gpos::lookup_type::EXTENSION,
    parse: parse_gpos_lists,
    rewrite_lookup: gpos::rewrite_lookup,
    context_lookup_type: gpos::context_lookup_type,
};

fn parse_gsub_lists(bytes: &[u8]) -> sigilbuzz::Result<(LookupList<'_>, FeatureList<'_>)> {
    let table = sigilbuzz::tables::gsub::Gsub::parse(bytes)?;
    Ok((*table.lookup_list(), *table.feature_list()))
}

fn parse_gpos_lists(bytes: &[u8]) -> sigilbuzz::Result<(LookupList<'_>, FeatureList<'_>)> {
    let table = sigilbuzz::tables::gpos::Gpos::parse(bytes)?;
    Ok((*table.lookup_list(), *table.feature_list()))
}

/// Drives the GSUB rewrite; see [`build_layout`]. Returns the new GSUB
/// bytes or `None` when the table drops entirely.
pub(crate) fn build_gsub(
    face: &Face<'_>,
    ctx: &RewriterCtx,
) -> Result<Option<Vec<u8>>, SubsetError> {
    build_layout(face, ctx, &GSUB_KIND)
}

/// Drives the GPOS rewrite; see [`build_layout`]. The per-type
/// rewriters cover every GPOS lookup type (1-9).
pub(crate) fn build_gpos(
    face: &Face<'_>,
    ctx: &RewriterCtx,
) -> Result<Option<Vec<u8>>, SubsetError> {
    build_layout(face, ctx, &GPOS_KIND)
}

/// Drives a GSUB or GPOS rewrite. Walks every lookup, runs the per-type
/// rewriter, then runs the drop cascade and renumbers surviving
/// lookups. Returns the new table bytes or `None` when the table drops
/// entirely.
///
/// Context lookups (GSUB types 5 / 6 / 8, GPOS types 7 / 8) carry
/// nested lookup records that name sibling lookups by index. The first
/// pass keeps the source indices, because it does not yet know which
/// siblings survive; the second pass rewrites them through the
/// renumber map and drops records aiming at dropped lookups. Rules
/// left without records stay (they act as `ignore sub` / `ignore pos`
/// rules), so a context lookup only drops when its glyph coverage
/// empties, which the first pass already saw. The second pass is still
/// a loop: should a lookup drop there, the renumber map is rebuilt and
/// the pass repeats, bounded by the lookup count since each round only
/// drops more lookups.
///
/// A table the parser rejects, a lookup or subtable that cannot be
/// reached, and a feature, script or language system that cannot be
/// read are left out and reported through `ctx.diag`.
fn build_layout(
    face: &Face<'_>,
    ctx: &RewriterCtx,
    kind: &LayoutKind,
) -> Result<Option<Vec<u8>>, SubsetError> {
    let bytes = match face.table_bytes(kind.tag) {
        Ok(bytes) => bytes,
        Err(sigilbuzz::Error::MissingTable { .. }) => return Ok(None),
        Err(e) => {
            let diag = ctx.diag.for_table(kind.tag, &[]);
            diag.at(0, error_context(&e), "the whole table");
            return Ok(None);
        }
    };
    let diag = ctx.diag.for_table(kind.tag, bytes);
    let (lookups, feature_list) = match (kind.parse)(bytes) {
        Ok(lists) => lists,
        Err(e) => {
            // The parser measures nested errors from the nested list,
            // so only the table itself can be located reliably.
            diag.at(0, error_context(&e), "the whole table");
            return Ok(None);
        }
    };

    // Read every lookup once; both passes rewrite from these.
    let lookup_list_at = header_offset(bytes, 8);
    let sources: Vec<Option<LookupSource<'_>>> = (0..lookups.len())
        .map(|li| read_lookup(bytes, &lookups, lookup_list_at, li, &diag))
        .collect();
    let rewrite = |ctx: &RewriterCtx, source: &LookupSource<'_>| {
        (kind.rewrite_lookup)(
            ctx,
            source.lookup.lookup_type(),
            source.lookup.flag(),
            source.lookup.mark_filtering_set(),
            &source.subtables,
        )
    };

    // First pass: every lookup, nested lookup indices left as they are.
    let first = RewriterCtx {
        keep_variations: ctx.keep_variations,
        diag,
        ..RewriterCtx::new(ctx.gid_map, None)
    };
    let mut rewritten: Vec<Option<RewrittenLookup>> = Vec::with_capacity(sources.len());
    for source in &sources {
        rewritten.push(match source {
            Some(source) => rewrite(&first, source)?,
            None => None,
        });
    }

    // Second pass: context lookups again, with the renumber map.
    let mut renumber = build_renumber(&rewritten);
    for _ in 0..sources.len() {
        let mut changed = false;
        let inner = RewriterCtx {
            keep_variations: ctx.keep_variations,
            diag,
            ..RewriterCtx::new(ctx.gid_map, Some(&renumber))
        };
        for (li, source) in sources.iter().enumerate() {
            let Some(source) = source else {
                continue;
            };
            // Only lookups that survived the first pass, and only
            // context ones: the others do not depend on the map.
            if rewritten[li].is_none() || (kind.context_lookup_type)(&source.lookup).is_none() {
                continue;
            }
            let new_lookup = rewrite(&inner, source)?;
            changed |= new_lookup.is_none();
            rewritten[li] = new_lookup;
        }
        if !changed {
            break;
        }
        renumber = build_renumber(&rewritten);
    }

    // FeatureVariations (1.1 tables): a feature some alternate still
    // gives a surviving lookup stays, even with no default lookup left.
    let variations = feature_variations::read(bytes).unwrap_or_else(|e| {
        diag.error(&e, "the FeatureVariations");
        None
    });
    let live_alternates = variations.as_ref().map_or_else(Vec::new, |fv| {
        fv.features_with_live_alternates(usize::from(feature_list.len()), &renumber)
    });

    // Last: features and scripts. The ScriptList is walked as raw
    // bytes because the parser only looks LangSys records up by tag.
    let new_features = rewrite_features(
        feature_list,
        &renumber,
        &live_alternates,
        &diag,
        header_offset(bytes, 6),
    )?;
    let script_list = bytes.get(header_offset(bytes, 4)..).unwrap_or_default();
    let Some(new_scripts) =
        rewrite_scripts_from_bytes(script_list, &new_features.feature_renumber, &diag)?
    else {
        return Ok(None);
    };

    let new_lookups: Vec<RewrittenLookup> = rewritten.into_iter().flatten().collect();
    if new_lookups.is_empty() {
        return Ok(None);
    }
    let new_variations = match &variations {
        Some(fv) => {
            feature_variations::subset(fv, &new_features.feature_renumber, &renumber, &diag)?
        }
        None => None,
    };

    assemble_layout_table(
        &new_scripts,
        &new_features.bytes,
        &new_lookups,
        kind.extension_type,
        new_variations.as_deref(),
    )
    .map(Some)
}

/// The Offset16 at `pos` of a GSUB or GPOS header, which the parser
/// has already read, so it is in bounds.
fn header_offset(table: &[u8], pos: usize) -> usize {
    table
        .get(pos..pos + 2)
        .map_or(0, |b| usize::from(u16::from_be_bytes([b[0], b[1]])))
}

/// A lookup the driver could read, with the subtables it could reach.
struct LookupSource<'a> {
    lookup: Lookup<'a>,
    subtables: Vec<&'a [u8]>,
}

/// Reads lookup `li` of the LookupList at `list_at`. A lookup whose
/// header cannot be read is left out, and so is a subtable whose offset
/// points past the table; both are reported through `diag`.
fn read_lookup<'a>(
    table: &'a [u8],
    lookups: &LookupList<'a>,
    list_at: usize,
    li: u16,
    diag: &Diag<'_>,
) -> Option<LookupSource<'a>> {
    let slot = list_at + 2 + usize::from(li) * 2;
    let Some(lookup) = lookups.get(li) else {
        diag.at(
            slot,
            "lookup offset past the end, or lookup header truncated",
            "a lookup",
        );
        return None;
    };
    let lookup_at = list_at + header_offset(table, slot);
    let mut subtables = Vec::with_capacity(usize::from(lookup.subtable_count()));
    for si in 0..lookup.subtable_count() {
        match lookup.subtable_bytes(si) {
            Some(body) => subtables.push(body),
            None => diag.at(
                lookup_at + 6 + usize::from(si) * 2,
                "lookup subtable offset past the end of the table",
                "a lookup subtable",
            ),
        }
    }
    Some(LookupSource { lookup, subtables })
}

/// Builds an `Option<u16>` array indexed by old lookup index. `Some(n)`
/// means the surviving lookup got new index `n`; `None` means the
/// lookup dropped.
fn build_renumber(rewritten: &[Option<RewrittenLookup>]) -> Vec<Option<u16>> {
    let mut out = Vec::with_capacity(rewritten.len());
    let mut next: u16 = 0;
    for slot in rewritten {
        if slot.is_some() {
            out.push(Some(next));
            next += 1;
        } else {
            out.push(None);
        }
    }
    out
}

struct RewrittenFeatures {
    bytes: Vec<u8>,
    /// Old feature index -> new feature index (or None if dropped).
    feature_renumber: Vec<Option<u16>>,
}

/// Rewrites the FeatureList. Drops any feature whose lookup-index list
/// becomes empty after the lookup renumber, unless `live_alternates`
/// marks it (a FeatureVariations alternate still gives it a lookup).
/// Returns the new bytes plus a feature-index renumber map.
///
/// A feature whose table cannot be read is dropped and reported
/// through `diag` at its FeatureRecord; `list_at` is where the
/// FeatureList starts in the table.
fn rewrite_features(
    feature_list: FeatureList<'_>,
    lookup_renumber: &[Option<u16>],
    live_alternates: &[bool],
    diag: &Diag<'_>,
    list_at: usize,
) -> Result<RewrittenFeatures, SubsetError> {
    let offsets = Offset16Guard::default();
    let mut surviving: Vec<([u8; 4], Vec<u16>)> = Vec::new();
    let mut feature_renumber: Vec<Option<u16>> = Vec::with_capacity(feature_list.len() as usize);
    for fi in 0..feature_list.len() {
        let Some((tag, feature)) = feature_list.get(fi) else {
            diag.at(
                list_at + 2 + usize::from(fi) * 6,
                "feature offset past the end, or feature table truncated",
                "a feature",
            );
            feature_renumber.push(None);
            continue;
        };
        let new_indices: Vec<u16> = feature
            .lookup_indices()
            .filter_map(|li| lookup_renumber.get(li as usize).copied().flatten())
            .collect();
        let live = live_alternates
            .get(usize::from(fi))
            .copied()
            .unwrap_or(false);
        if new_indices.is_empty() && !live {
            feature_renumber.push(None);
        } else {
            feature_renumber.push(Some(surviving.len() as u16));
            surviving.push((tag, new_indices));
        }
    }

    // Encode FeatureList:
    //   u16 featureCount
    //   FeatureRecord records[featureCount]: { tag(4) + Offset16 }
    //   Feature[] bodies
    let mut out = Vec::new();
    out.extend_from_slice(&(surviving.len() as u16).to_be_bytes());
    let records_start = out.len();
    for _ in 0..surviving.len() {
        out.extend_from_slice(&[0u8; 6]); // placeholder: tag + offset
    }
    for (i, (tag, indices)) in surviving.iter().enumerate() {
        let body_start = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // featureParamsOffset
        out.extend_from_slice(&(indices.len() as u16).to_be_bytes());
        for idx in indices {
            out.extend_from_slice(&idx.to_be_bytes());
        }
        let rec_off = records_start + i * 6;
        out[rec_off..rec_off + 4].copy_from_slice(tag);
        let body_off = offsets.narrow(body_start);
        out[rec_off + 4..rec_off + 6].copy_from_slice(&body_off.to_be_bytes());
    }
    offsets.check("FeatureList rewrite: an offset exceeds 64 KiB")?;

    Ok(RewrittenFeatures {
        bytes: out,
        feature_renumber,
    })
}

/// Rewrites the ScriptList by walking the raw bytes (the parser
/// doesn't expose enumeration of named LangSys records, only binary
/// search by tag). Drops any LangSys whose feature indices all
/// dropped, drops any Script with no surviving default LangSys + no
/// surviving named LangSys, and returns the new bytes when at least
/// one script survives.
///
/// A script or language system that cannot be read is dropped and
/// reported through `diag`; `bytes` is the ScriptList, a sub-slice of
/// the table `diag` reports against.
fn rewrite_scripts_from_bytes(
    bytes: &[u8],
    feature_renumber: &[Option<u16>],
    diag: &Diag<'_>,
) -> Result<Option<Vec<u8>>, SubsetError> {
    // ScriptList:
    //   u16 scriptCount
    //   ScriptRecord records[scriptCount]: { tag(4) + Offset16 (relative to ScriptList start) }
    let script_count = match bytes.get(0..2) {
        Some(b) => usize::from(u16::from_be_bytes([b[0], b[1]])),
        None => {
            diag.in_part(bytes, 0, "ScriptList truncated", "the whole table");
            return Ok(None);
        }
    };
    if bytes.len() < 2 + script_count * 6 {
        diag.in_part(
            bytes,
            2,
            "ScriptList records shorter than scriptCount",
            "the whole table",
        );
        return Ok(None);
    }

    type ScriptEntry = (
        [u8; 4],
        Option<RewrittenLangSys>,
        Vec<([u8; 4], RewrittenLangSys)>,
    );
    let mut surviving_scripts: Vec<ScriptEntry> = Vec::new();

    for i in 0..script_count {
        let rec_off = 2 + i * 6;
        let tag = [
            bytes[rec_off],
            bytes[rec_off + 1],
            bytes[rec_off + 2],
            bytes[rec_off + 3],
        ];
        let script_off = u16::from_be_bytes([bytes[rec_off + 4], bytes[rec_off + 5]]) as usize;
        let Some(script_body) = bytes.get(script_off..).filter(|b| b.len() >= 4) else {
            diag.in_part(
                bytes,
                rec_off + 4,
                "script offset past the end, or script table truncated",
                "a script",
            );
            continue;
        };
        // Script:
        //   Offset16 defaultLangSysOffset (Script-relative; 0 means none)
        //   u16      langSysCount
        //   LangSysRecord records[langSysCount]: { tag(4) + Offset16 (Script-relative) }
        let default_off = u16::from_be_bytes([script_body[0], script_body[1]]) as usize;
        let langsys_count = u16::from_be_bytes([script_body[2], script_body[3]]) as usize;
        let langsys_records_off = 4;
        let langsys_records_end = langsys_records_off + langsys_count * 6;
        if script_body.len() < langsys_records_end {
            diag.in_part(
                script_body,
                2,
                "LangSysRecords shorter than langSysCount",
                "a script",
            );
            continue;
        }

        let default = if default_off != 0 {
            read_langsys(script_body, 0, default_off, feature_renumber, diag)
        } else {
            None
        };

        let mut langsystems: Vec<([u8; 4], RewrittenLangSys)> = Vec::new();
        for j in 0..langsys_count {
            let lr = langsys_records_off + j * 6;
            let ls_tag = [
                script_body[lr],
                script_body[lr + 1],
                script_body[lr + 2],
                script_body[lr + 3],
            ];
            let ls_off = u16::from_be_bytes([script_body[lr + 4], script_body[lr + 5]]) as usize;
            if let Some(rls) = read_langsys(script_body, lr + 4, ls_off, feature_renumber, diag) {
                langsystems.push((ls_tag, rls));
            }
        }

        if default.is_some() || !langsystems.is_empty() {
            surviving_scripts.push((tag, default, langsystems));
        }
    }

    if surviving_scripts.is_empty() {
        return Ok(None);
    }

    // Encode ScriptList:
    //   u16 scriptCount
    //   ScriptRecord records[scriptCount]: { tag(4) + Offset16 }
    //   Script[] bodies (each: defaultLangSysOffset + langSysCount + LangSysRecord[])
    //   LangSys[] bodies (each: lookupOrderOffset(0) + reqFeatureIndex + featureCount + indices[])
    let offsets = Offset16Guard::default();
    let mut out = Vec::new();
    out.extend_from_slice(&(surviving_scripts.len() as u16).to_be_bytes());
    let script_records_start = out.len();
    for _ in 0..surviving_scripts.len() {
        out.extend_from_slice(&[0u8; 6]);
    }
    for (i, (script_tag, default, langsystems)) in surviving_scripts.iter().enumerate() {
        let script_body_start = out.len();
        // Patch the ScriptRecord pointing at this body.
        let rec_off = script_records_start + i * 6;
        out[rec_off..rec_off + 4].copy_from_slice(script_tag);
        let body_off_u16 = offsets.narrow(script_body_start);
        out[rec_off + 4..rec_off + 6].copy_from_slice(&body_off_u16.to_be_bytes());

        let default_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // defaultLangSysOffset placeholder
        out.extend_from_slice(&(langsystems.len() as u16).to_be_bytes());
        let langsys_records_start = out.len();
        for _ in 0..langsystems.len() {
            out.extend_from_slice(&[0u8; 6]); // tag + offset placeholders
        }
        // Default LangSys body, if any.
        if let Some(d) = default.as_ref() {
            let langsys_body_pos = out.len() - script_body_start;
            out.extend_from_slice(&encode_langsys(d));
            out[default_slot..default_slot + 2]
                .copy_from_slice(&offsets.narrow(langsys_body_pos).to_be_bytes());
        }
        // Named LangSys bodies.
        for (j, (ls_tag, ls)) in langsystems.iter().enumerate() {
            let langsys_body_pos = out.len() - script_body_start;
            out.extend_from_slice(&encode_langsys(ls));
            let lr_off = langsys_records_start + j * 6;
            out[lr_off..lr_off + 4].copy_from_slice(ls_tag);
            out[lr_off + 4..lr_off + 6]
                .copy_from_slice(&offsets.narrow(langsys_body_pos).to_be_bytes());
        }
    }
    offsets.check("ScriptList rewrite: an offset exceeds 64 KiB")?;
    Ok(Some(out))
}

struct RewrittenLangSys {
    required_feature_index: u16,
    feature_indices: Vec<u16>,
}

/// Walks LangSys raw bytes:
///
/// ```text
///   Offset16 lookupOrderOffset (=0)
///   u16 requiredFeatureIndex
///   u16 featureIndexCount
///   u16 featureIndices[featureIndexCount]
/// ```
///
/// Returns `Ok(None)` when no feature survives, and an error, measured
/// from the start of `bytes`, when the LangSys is truncated.
fn rewrite_langsys_from_bytes(
    bytes: &[u8],
    feature_renumber: &[Option<u16>],
) -> Result<Option<RewrittenLangSys>, sigilbuzz::Error> {
    if bytes.len() < 6 {
        return Err(sigilbuzz::Error::Truncated {
            offset: 0,
            context: "LangSys header truncated",
        });
    }
    let _lookup_order = u16::from_be_bytes([bytes[0], bytes[1]]);
    let required = u16::from_be_bytes([bytes[2], bytes[3]]);
    let count = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    let need = 6 + count * 2;
    if bytes.len() < need {
        return Err(sigilbuzz::Error::Truncated {
            offset: 6,
            context: "LangSys featureIndices shorter than featureIndexCount",
        });
    }
    let new_required = if required == 0xFFFF {
        0xFFFF
    } else {
        match feature_renumber.get(required as usize) {
            Some(Some(new)) => *new,
            _ => 0xFFFF,
        }
    };
    let mut new_indices: Vec<u16> = Vec::with_capacity(count);
    for i in 0..count {
        let off = 6 + i * 2;
        let fi = u16::from_be_bytes([bytes[off], bytes[off + 1]]);
        if let Some(Some(new)) = feature_renumber.get(fi as usize) {
            new_indices.push(*new);
        }
    }
    if new_required == 0xFFFF && new_indices.is_empty() {
        return Ok(None);
    }
    Ok(Some(RewrittenLangSys {
        required_feature_index: new_required,
        feature_indices: new_indices,
    }))
}

/// Rewrites the LangSys at `off` inside `script`, whose Offset16 sits
/// at byte `slot` of `script`. A LangSys that cannot be read is dropped
/// and reported through `diag`.
fn read_langsys(
    script: &[u8],
    slot: usize,
    off: usize,
    feature_renumber: &[Option<u16>],
    diag: &Diag<'_>,
) -> Option<RewrittenLangSys> {
    let Some(body) = script.get(off..) else {
        diag.in_part(
            script,
            slot,
            "LangSys offset past the end of the table",
            "a language system",
        );
        return None;
    };
    match rewrite_langsys_from_bytes(body, feature_renumber) {
        Ok(langsys) => langsys,
        Err(e) => {
            diag.part_error(body, &e, "a language system");
            None
        }
    }
}

fn encode_langsys(ls: &RewrittenLangSys) -> Vec<u8> {
    // LangSys:
    //   Offset16 lookupOrderOffset (0, reserved)
    //   u16 requiredFeatureIndex
    //   u16 featureIndexCount
    //   u16 featureIndices[featureIndexCount]
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&ls.required_feature_index.to_be_bytes());
    out.extend_from_slice(&(ls.feature_indices.len() as u16).to_be_bytes());
    for idx in &ls.feature_indices {
        out.extend_from_slice(&idx.to_be_bytes());
    }
    out
}

/// Assembles a complete GSUB or GPOS table (their headers are
/// identical). Builds the LookupList around the rewritten lookups
/// through [`crate::lookup_list::emit`], which falls back to Extension
/// lookups (`extension_type`: 7 for GSUB, 9 for GPOS) when the lookups
/// outgrow 16-bit offsets. With `feature_variations` the table is
/// version 1.1 and carries them after the LookupList. Errors when the
/// header offsets themselves, or even the Extension layout, cannot fit.
fn assemble_layout_table(
    script_list: &[u8],
    feature_list: &[u8],
    lookups: &[RewrittenLookup],
    extension_type: u16,
    feature_variations: Option<&[u8]>,
) -> Result<Vec<u8>, SubsetError> {
    // GSUB/GPOS header:
    //   u16 majorVersion = 1
    //   u16 minorVersion = 0, or 1 with FeatureVariations
    //   Offset16 scriptListOffset
    //   Offset16 featureListOffset
    //   Offset16 lookupListOffset
    //   Offset32 featureVariationsOffset   (1.1)
    let header_len: u16 = if feature_variations.is_some() { 14 } else { 10 };
    let script_list_off = header_len;
    let offsets = Offset16Guard::default();
    let feature_list_off = offsets.narrow(usize::from(header_len) + script_list.len());
    let lookup_list_off = offsets.narrow(usize::from(feature_list_off) + feature_list.len());
    offsets.check("layout rewrite: the ScriptList and FeatureList exceed 64 KiB")?;
    let lookup_list = crate::lookup_list::emit(lookups, extension_type).ok_or(
        SubsetError::Unsupported("layout rewrite: the LookupList outgrows even Extension lookups"),
    )?;

    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&u16::from(feature_variations.is_some()).to_be_bytes());
    out.extend_from_slice(&script_list_off.to_be_bytes());
    out.extend_from_slice(&feature_list_off.to_be_bytes());
    out.extend_from_slice(&lookup_list_off.to_be_bytes());
    if feature_variations.is_some() {
        out.extend_from_slice(&[0; 4]);
    }
    out.extend_from_slice(script_list);
    out.extend_from_slice(feature_list);
    out.extend_from_slice(&lookup_list);
    if let Some(variations) = feature_variations {
        let at = u32::try_from(out.len())
            .map_err(|_| SubsetError::Unsupported("layout rewrite: the table exceeds 4 GiB"))?;
        out[10..14].copy_from_slice(&at.to_be_bytes());
        out.extend_from_slice(variations);
    }
    Ok(out)
}

// === Byte-level helpers used by the rewriters and the closure walker. ===

/// The `(gid, class)` pairs of the ClassDef that `offset` points at
/// inside `sub`, class 0 left out. A null offset is the spec's empty
/// ClassDef (every glyph in class 0; fontmake leaves the backtrack
/// ClassDef of chained context format 2 null this way), so it yields
/// no pairs. An offset past the end of `sub` yields `None`.
pub(crate) fn classdef_pairs_at(sub: &[u8], offset: usize) -> Option<Vec<(u16, u16)>> {
    if offset == 0 {
        return Some(Vec::new());
    }
    sub.get(offset..).map(parse_classdef_pairs_from_bytes)
}

/// The lookup type and subtable an Extension subtable (GSUB type 7,
/// GPOS type 9) wraps:
///
/// ```text
///   u16      format = 1
///   u16      extensionLookupType
///   Offset32 extensionOffset        (from the Extension subtable)
/// ```
///
/// Errors are measured from the start of `sub`.
pub(crate) fn extension_target(sub: &[u8]) -> Result<(u16, &[u8]), sigilbuzz::Error> {
    let Some(header) = sub.get(..8) else {
        return Err(sigilbuzz::Error::Truncated {
            offset: 0,
            context: "Extension subtable truncated",
        });
    };
    if header[0..2] != [0, 1] {
        return Err(sigilbuzz::Error::Malformed {
            offset: 0,
            context: "unsupported Extension subtable format",
        });
    }
    let inner_type = u16::from_be_bytes([header[2], header[3]]);
    let inner_off = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    let inner = usize::try_from(inner_off)
        .ok()
        .and_then(|off| sub.get(off..))
        .ok_or(sigilbuzz::Error::Malformed {
            offset: 4,
            context: "Extension offset past the end of the table",
        })?;
    Ok((inner_type, inner))
}

/// Best-effort enumeration of the glyphs covered by a Coverage table
/// given its raw bytes. Returns an empty vec on any parse failure.
///
/// Mirrors the helper in [`crate::closure`], exposed here so the
/// per-lookup-type rewriters in [`crate::gsub`] / [`crate::gpos`] can
/// share it without re-deriving the byte layout.
pub(crate) fn parse_coverage_glyphs(bytes: &[u8]) -> Vec<u16> {
    let mut out = Vec::new();
    if bytes.len() < 4 {
        return out;
    }
    let format = u16::from_be_bytes([bytes[0], bytes[1]]);
    let count = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
    match format {
        1 => {
            let need = 4 + count * 2;
            if bytes.len() < need {
                return out;
            }
            for i in 0..count {
                let off = 4 + i * 2;
                out.push(u16::from_be_bytes([bytes[off], bytes[off + 1]]));
            }
        }
        2 => {
            let need = 4 + count * 6;
            if bytes.len() < need {
                return out;
            }
            for i in 0..count {
                let off = 4 + i * 6;
                let start = u16::from_be_bytes([bytes[off], bytes[off + 1]]);
                let end = u16::from_be_bytes([bytes[off + 2], bytes[off + 3]]);
                for g in start..=end {
                    out.push(g);
                }
            }
        }
        _ => {}
    }
    out
}

/// Walks a ClassDef's raw bytes to enumerate every `(gid, class)`
/// pair, skipping class-0 entries.
pub(crate) fn parse_classdef_pairs_from_bytes(bytes: &[u8]) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    if bytes.len() < 2 {
        return out;
    }
    let format = u16::from_be_bytes([bytes[0], bytes[1]]);
    match format {
        1 => {
            // Format 1: u16 format, u16 startGlyphID, u16 glyphCount, u16 values[count].
            if bytes.len() < 6 {
                return out;
            }
            let start = u16::from_be_bytes([bytes[2], bytes[3]]);
            let count = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
            let need = 6 + count * 2;
            if bytes.len() < need {
                return out;
            }
            for i in 0..count {
                let off = 6 + i * 2;
                let class = u16::from_be_bytes([bytes[off], bytes[off + 1]]);
                if class == 0 {
                    continue;
                }
                let gid = start.saturating_add(i as u16);
                out.push((gid, class));
            }
        }
        2 => {
            // Format 2: u16 format, u16 rangeCount, RangeRecord[count]: u16 start, u16 end, u16 class.
            if bytes.len() < 4 {
                return out;
            }
            let count = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
            let need = 4 + count * 6;
            if bytes.len() < need {
                return out;
            }
            for i in 0..count {
                let off = 4 + i * 6;
                let start = u16::from_be_bytes([bytes[off], bytes[off + 1]]);
                let end = u16::from_be_bytes([bytes[off + 2], bytes[off + 3]]);
                let class = u16::from_be_bytes([bytes[off + 4], bytes[off + 5]]);
                if class == 0 {
                    continue;
                }
                for g in start..=end {
                    out.push((g, class));
                }
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod static_tests;

#[cfg(test)]
mod truncation_tests;

#[cfg(test)]
mod warning_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{vec, vec::Vec};

    const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

    fn open_sans_face() -> sigilbuzz::Face<'static> {
        sigilbuzz::Face::parse_bytes(OPEN_SANS, 0).unwrap()
    }

    #[test]
    fn drop_layout_when_retain_layout_false() {
        let face = open_sans_face();
        let kept: alloc::vec::Vec<u16> = (0..face.maxp().unwrap().num_glyphs).collect();
        let input = SubsetInput {
            retain_layout: false,
            ..Default::default()
        };
        let plan = decide(&face, &kept, &input, &Warnings::default()).unwrap();
        assert!(matches!(plan.gsub, Decision::Drop));
        assert!(matches!(plan.gpos, Decision::Drop));
        assert!(matches!(plan.gdef, Decision::Drop));
    }

    #[test]
    fn preserve_layout_when_kept_set_is_identity() {
        let face = open_sans_face();
        let kept: alloc::vec::Vec<u16> = (0..face.maxp().unwrap().num_glyphs).collect();
        let input = SubsetInput {
            retain_layout: true,
            ..Default::default()
        };
        let plan = decide(&face, &kept, &input, &Warnings::default()).unwrap();
        assert!(matches!(plan.gsub, Decision::Preserve));
        assert!(matches!(plan.gpos, Decision::Preserve));
        assert!(matches!(plan.gdef, Decision::Preserve));
    }

    #[test]
    fn proper_subset_routes_through_rewriter() {
        // Today the rewriter ships GSUB type 1 + GDEF classdefs; GPOS
        // drops everything. So we expect gsub: Rewrite-or-Drop, gpos:
        // Drop, gdef: Rewrite-or-Drop. The exact pick depends on
        // whether the source's lookups have any type-1 subtables, so
        // we just check that we do *not* hit Preserve (the old
        // identity-only policy) and that the proper-subset case
        // doesn't blow up.
        let face = open_sans_face();
        let kept: alloc::vec::Vec<u16> = alloc::vec![0, 36, 37, 38];
        let input = SubsetInput {
            retain_layout: true,
            ..Default::default()
        };
        let plan = decide(&face, &kept, &input, &Warnings::default()).unwrap();
        assert!(!matches!(plan.gsub, Decision::Preserve));
        assert!(!matches!(plan.gpos, Decision::Preserve));
        assert!(!matches!(plan.gdef, Decision::Preserve));
    }

    #[test]
    fn gid_map_is_identity_for_full_kept_set() {
        let kept: Vec<u16> = (0..10).collect();
        let map = GidMap::from_kept(&kept);
        assert!(map.is_identity());
    }

    #[test]
    fn gid_map_filters_dropped_gids() {
        let kept: Vec<u16> = vec![0, 5, 10];
        let map = GidMap::from_kept(&kept);
        assert_eq!(map.map(0), Some(0));
        assert_eq!(map.map(5), Some(1));
        assert_eq!(map.map(10), Some(2));
        assert_eq!(map.map(1), None);
        assert_eq!(map.map(99), None);
        assert!(!map.is_identity());
    }

    #[test]
    fn parse_coverage_glyphs_handles_format1() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes());
        for g in [10u16, 20, 30] {
            bytes.extend_from_slice(&g.to_be_bytes());
        }
        let glyphs = parse_coverage_glyphs(&bytes);
        assert_eq!(glyphs, vec![10, 20, 30]);
    }

    #[test]
    fn parse_coverage_glyphs_handles_format2() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes()); // rangeCount
        bytes.extend_from_slice(&5u16.to_be_bytes()); // start
        bytes.extend_from_slice(&7u16.to_be_bytes()); // end
        bytes.extend_from_slice(&0u16.to_be_bytes()); // startCov
        let glyphs = parse_coverage_glyphs(&bytes);
        assert_eq!(glyphs, vec![5, 6, 7]);
    }

    #[test]
    fn parse_classdef_pairs_skips_class_zero() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes()); // format
        bytes.extend_from_slice(&2u16.to_be_bytes()); // rangeCount
        for (start, end, class) in [(5u16, 5u16, 1u16), (7, 7, 0)] {
            bytes.extend_from_slice(&start.to_be_bytes());
            bytes.extend_from_slice(&end.to_be_bytes());
            bytes.extend_from_slice(&class.to_be_bytes());
        }
        let pairs = parse_classdef_pairs_from_bytes(&bytes);
        assert_eq!(pairs, vec![(5, 1)]);
    }

    #[test]
    fn classdef_pairs_at_reads_a_null_offset_as_empty() {
        // A context format 2 header: its first word (2) would read as a
        // ClassDef format, so a null offset must not parse from 0.
        let mut sub = Vec::new();
        sub.extend_from_slice(&2u16.to_be_bytes()); // subtable format
        sub.extend_from_slice(&1u16.to_be_bytes());
        sub.extend_from_slice(&[0, 5, 0, 5, 0, 9]);
        assert_eq!(classdef_pairs_at(&sub, 0), Some(Vec::new()));
        let cd_off = sub.len();
        sub.extend_from_slice(&2u16.to_be_bytes()); // ClassDef format 2
        sub.extend_from_slice(&1u16.to_be_bytes()); // rangeCount
        sub.extend_from_slice(&[0, 8, 0, 8, 0, 2]);
        assert_eq!(classdef_pairs_at(&sub, cd_off), Some(vec![(8, 2)]));
        assert_eq!(classdef_pairs_at(&sub, sub.len() + 1), None);
    }

    #[test]
    fn build_renumber_skips_dropped() {
        let rewritten: Vec<Option<RewrittenLookup>> = vec![
            Some(RewrittenLookup {
                lookup_type: 1,
                lookup_flag: 0,
                mark_filtering_set: None,
                subtables: vec![RewrittenSubtable { bytes: vec![] }],
            }),
            None,
            Some(RewrittenLookup {
                lookup_type: 1,
                lookup_flag: 0,
                mark_filtering_set: None,
                subtables: vec![RewrittenSubtable { bytes: vec![] }],
            }),
        ];
        let r = build_renumber(&rewritten);
        assert_eq!(r, vec![Some(0), None, Some(1)]);
    }
}
