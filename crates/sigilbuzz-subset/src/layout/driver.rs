//! The GSUB and GPOS drivers: they read every lookup, run the
//! per-type rewriters in two passes, then rebuild the FeatureList,
//! ScriptList, LookupList and FeatureVariations around the survivors.

use alloc::vec::Vec;

use sigilbuzz::tables::layout::{FeatureList, Lookup, LookupList};
use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use super::lists::{rewrite_features, rewrite_scripts_from_bytes};
use super::{RewriterCtx, RewrittenLookup};
use crate::offset16::Offset16Guard;
use crate::warnings::{error_context, Diag};
use crate::{feature_variations, gpos, gsub, SubsetError};

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
pub(super) fn build_renumber(rewritten: &[Option<RewrittenLookup>]) -> Vec<Option<u16>> {
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
