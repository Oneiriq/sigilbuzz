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
//! - [`parse_coverage_glyphs`] / [`parse_classdef_pairs`]: small
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
//! 2. Features that name no surviving lookup.
//! 3. Scripts whose every feature dropped.
//! 4. The container table entirely if no script survives.
//!
//! Surviving lookups are then renumbered to 0..N. Every reference
//! (the FeatureList's lookup-index list) is rewritten through the
//! same renumber map.
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
//! - **GDEF GlyphClassDef + MarkAttachClassDef**: full ClassDef
//!   rewriter via [`crate::classdef`]. AttachList, LigCaretList,
//!   MarkGlyphSetsDef, ItemVariationStore drop.
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
//! out the driver drops the whole table.
//!
//! ## Offset overflow
//!
//! Every emitted offset is range-checked. A subtable whose rewritten
//! form no longer fits its 16-bit offsets is dropped. When the lookup
//! list itself overflows, [`assemble_layout_table`] moves every
//! subtable behind an Extension lookup, which uses 32-bit offsets.

use alloc::vec::Vec;

use sigilbuzz::tables::layout::{FeatureList, Lookup, LookupList};
use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use crate::util::{WorkBudget, WORK_LIMIT};
use crate::{gdef, gpos, gsub, GlyphId, SubsetError, SubsetInput};

/// A valid Coverage or ClassDef lists each glyph at most once, so it
/// never names more than this many glyphs. The byte walkers stop
/// there, which bounds their output on overlapping ranges.
const MAX_GLYPH_ENTRIES: usize = 1 << 16;

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
    /// the budget is spent; the caller should stop and drop its output.
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
pub(crate) fn decide(
    face: &Face<'_>,
    kept: &[GlyphId],
    input: &SubsetInput,
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
        return Ok(LayoutPlan {
            gsub: if has_gsub {
                Decision::Preserve
            } else {
                Decision::Drop
            },
            gpos: if has_gpos {
                Decision::Preserve
            } else {
                Decision::Drop
            },
            gdef: if has_gdef {
                Decision::Preserve
            } else {
                Decision::Drop
            },
        });
    }

    // Non-identity: invoke the rewriter. Each driver returns either
    // bytes to substitute or None when the whole table dropped.
    let map = GidMap::from_kept(kept);
    let ctx = RewriterCtx {
        gid_map: &map,
        lookup_renumber: None,
    };

    let gsub = if has_gsub {
        match build_gsub(face, &ctx) {
            Some(b) => Decision::Rewrite(b),
            None => Decision::Drop,
        }
    } else {
        Decision::Drop
    };
    let gpos = if has_gpos {
        match build_gpos(face, &ctx) {
            Some(b) => Decision::Rewrite(b),
            None => Decision::Drop,
        }
    } else {
        Decision::Drop
    };
    let gdef = if has_gdef {
        map.reset_budget();
        match gdef::rewrite_gdef(face, &map) {
            Some(b) => Decision::Rewrite(b),
            None => Decision::Drop,
        }
    } else {
        Decision::Drop
    };

    Ok(LayoutPlan { gsub, gpos, gdef })
}

/// Signature shared by [`gsub::rewrite_lookup`] and
/// [`gpos::rewrite_lookup`].
type RewriteLookupFn = fn(&RewriterCtx, u16, u16, Option<u16>, &[&[u8]]) -> Option<RewrittenLookup>;

/// The parts of a GSUB or GPOS table that [`build_layout`] reads, plus
/// the per-table rewrite hooks.
struct LayoutSource<'a, 'b> {
    /// Raw table bytes. The ScriptList is walked from these.
    table_bytes: &'a [u8],
    lookups: &'b LookupList<'a>,
    features: &'b FeatureList<'a>,
    rewrite_lookup: RewriteLookupFn,
    context_lookup_type: fn(&Lookup<'_>) -> Option<u16>,
    /// Extension lookup type of this table (GSUB 7, GPOS 9).
    extension_type: u16,
}

/// Drives the GSUB rewrite. See [`build_layout`].
pub(crate) fn build_gsub(face: &Face<'_>, ctx: &RewriterCtx) -> Option<Vec<u8>> {
    let table = face.gsub().ok().flatten()?;
    let table_bytes = face.table_bytes(tag::GSUB).ok()?;
    build_layout(
        ctx,
        &LayoutSource {
            table_bytes,
            lookups: table.lookup_list(),
            features: table.feature_list(),
            rewrite_lookup: gsub::rewrite_lookup,
            context_lookup_type: gsub::context_lookup_type,
            extension_type: sigilbuzz::tables::gsub::lookup_type::EXTENSION,
        },
    )
}

/// Drives the GPOS rewrite. See [`build_layout`]. The per-type
/// rewriters cover every GPOS lookup type (1-9); context lookups
/// (types 7 / 8) carry nested `PosLookupRecord`s that the second pass
/// patches.
pub(crate) fn build_gpos(face: &Face<'_>, ctx: &RewriterCtx) -> Option<Vec<u8>> {
    let table = face.gpos().ok().flatten()?;
    let table_bytes = face.table_bytes(tag::GPOS).ok()?;
    build_layout(
        ctx,
        &LayoutSource {
            table_bytes,
            lookups: table.lookup_list(),
            features: table.feature_list(),
            rewrite_lookup: gpos::rewrite_lookup,
            context_lookup_type: gpos::context_lookup_type,
            extension_type: sigilbuzz::tables::gpos::lookup_type::EXTENSION,
        },
    )
}

/// Rewrites one GSUB or GPOS table. Walks every lookup, runs the
/// per-type rewriter, then runs the drop cascade and renumbers
/// surviving lookups. Returns the new table bytes, or `None` when the
/// table drops entirely or the work budget runs out.
fn build_layout(ctx: &RewriterCtx, src: &LayoutSource<'_, '_>) -> Option<Vec<u8>> {
    let map = ctx.gid_map;
    map.reset_budget();
    let lookups = src.lookups;
    let rewrite_one = |rctx: &RewriterCtx, lookup: &Lookup<'_>| -> Option<RewrittenLookup> {
        let bodies: Vec<&[u8]> = (0..lookup.subtable_count())
            .filter_map(|si| lookup.subtable_bytes(si))
            .collect();
        (src.rewrite_lookup)(
            rctx,
            lookup.lookup_type(),
            lookup.flag(),
            lookup.mark_filtering_set(),
            &bodies,
        )
    };

    // Pass 1: per-lookup rewrite. Context-style lookups carry nested
    // lookup records that point at sibling lookups by index. This pass
    // does not yet know which siblings survive, so the rewriters keep
    // the source indices and pass 2 patches them once the renumber map
    // is known.
    let mut rewritten: Vec<Option<RewrittenLookup>> = Vec::with_capacity(lookups.len() as usize);
    for li in 0..lookups.len() {
        let Some(lookup) = lookups.get(li) else {
            rewritten.push(None);
            continue;
        };
        if !map.spend(1 + usize::from(lookup.subtable_count())) {
            return None;
        }
        rewritten.push(rewrite_one(ctx, &lookup));
    }

    // Pass 2: iterate the context-lookup renumber to a fixed point.
    // Each iteration rebuilds the renumber map from the surviving
    // lookups, then re-rewrites every context-style lookup with the new
    // map. A context lookup whose nested records all point at dropped
    // lookups loses every subtable and falls out, which may in turn
    // cascade into other context lookups. Bounded by `lookups.len()`
    // because each iteration only ever drops more lookups (or
    // stabilizes), and by the work budget.
    let mut renumber = build_renumber(&rewritten);
    for _ in 0..lookups.len() {
        let mut changed = false;
        let inner_ctx = RewriterCtx {
            gid_map: map,
            lookup_renumber: Some(&renumber),
        };
        for li in 0..lookups.len() {
            // Only re-rewrite slots that survived pass 1; nothing to
            // resurrect here.
            let Some(slot) = rewritten.get_mut(usize::from(li)) else {
                continue;
            };
            if slot.is_none() {
                continue;
            }
            let Some(lookup) = lookups.get(li) else {
                continue;
            };
            // Non-context lookups do not depend on the renumber map.
            if (src.context_lookup_type)(&lookup).is_none() {
                continue;
            }
            if !map.spend(1 + usize::from(lookup.subtable_count())) {
                return None;
            }
            let new_lookup = rewrite_one(&inner_ctx, &lookup);
            // A context lookup whose every nested target dropped
            // returns None now that the renumber knows. Mark it as
            // dropped and trigger another pass.
            changed |= new_lookup.is_none();
            *slot = new_lookup;
        }
        if !changed {
            break;
        }
        renumber = build_renumber(&rewritten);
    }

    // Pass 3: rewrite features and scripts. ScriptList walks raw
    // bytes because the parser doesn't expose enumeration of named
    // LangSys records.
    let new_features = rewrite_features(src.features, &renumber, map)?;
    let script_list_off = usize::from(read_u16(src.table_bytes, 4)?);
    let script_list_bytes = src.table_bytes.get(script_list_off..)?;
    let new_scripts =
        rewrite_scripts_from_bytes(script_list_bytes, &new_features.feature_renumber, map)?;
    if map.budget_spent() {
        return None;
    }

    let new_lookups: Vec<RewrittenLookup> = rewritten.into_iter().flatten().collect();
    if new_lookups.is_empty() {
        return None;
    }

    assemble_layout_table(
        &new_scripts,
        &new_features.bytes,
        &new_lookups,
        src.extension_type,
    )
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
            // At most `u16::MAX` lookups exist, so the last survivor
            // gets index `u16::MAX - 1` and this never saturates.
            next = next.saturating_add(1);
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
/// becomes empty after the lookup renumber. Returns the new bytes plus
/// a feature-index renumber map, or `None` when the list no longer fits
/// its 16-bit offsets or the work budget runs out.
fn rewrite_features(
    feature_list: &FeatureList<'_>,
    lookup_renumber: &[Option<u16>],
    map: &GidMap,
) -> Option<RewrittenFeatures> {
    let mut surviving: Vec<([u8; 4], Vec<u16>)> = Vec::new();
    let mut feature_renumber: Vec<Option<u16>> = Vec::with_capacity(feature_list.len() as usize);
    // Total size of the Feature bodies kept so far. Each body must start
    // within 16 bits of the FeatureList, so once this passes `u16::MAX`
    // the next body cannot be addressed.
    let mut bodies_len = 0usize;
    for fi in 0..feature_list.len() {
        let Some((tag, feature)) = feature_list.get(fi) else {
            feature_renumber.push(None);
            continue;
        };
        if !map.spend(1 + usize::from(feature.len())) {
            return None;
        }
        let new_indices: Vec<u16> = feature
            .lookup_indices()
            .filter_map(|li| lookup_renumber.get(li as usize).copied().flatten())
            .collect();
        if new_indices.is_empty() {
            feature_renumber.push(None);
        } else {
            if bodies_len > usize::from(u16::MAX) {
                return None;
            }
            bodies_len += 4 + new_indices.len() * 2;
            feature_renumber.push(Some(u16::try_from(surviving.len()).ok()?));
            surviving.push((tag, new_indices));
        }
    }

    // Encode FeatureList:
    //   u16 featureCount
    //   FeatureRecord records[featureCount]: { tag(4) + Offset16 }
    //   Feature[] bodies
    let mut out = Vec::new();
    push_count16(&mut out, surviving.len())?;
    let records_start = out.len();
    out.resize(records_start + surviving.len() * 6, 0);
    for (i, (tag, indices)) in surviving.iter().enumerate() {
        let body_start = out.len();
        let rec_off = records_start + i * 6;
        out.get_mut(rec_off..rec_off + 4)?.copy_from_slice(tag);
        patch_offset16(&mut out, rec_off + 4, body_start)?;
        out.extend_from_slice(&0u16.to_be_bytes()); // featureParamsOffset
        push_count16(&mut out, indices.len())?;
        for idx in indices {
            out.extend_from_slice(&idx.to_be_bytes());
        }
    }

    Some(RewrittenFeatures {
        bytes: out,
        feature_renumber,
    })
}

/// Rewrites the ScriptList by walking the raw bytes (the parser
/// doesn't expose enumeration of named LangSys records, only binary
/// search by tag). Drops any LangSys whose feature indices all
/// dropped, drops any Script with no surviving default LangSys + no
/// surviving named LangSys, and returns the new bytes when at least
/// one script survives and the list fits its 16-bit offsets.
fn rewrite_scripts_from_bytes(
    bytes: &[u8],
    feature_renumber: &[Option<u16>],
    map: &GidMap,
) -> Option<Vec<u8>> {
    // ScriptList:
    //   u16 scriptCount
    //   ScriptRecord records[scriptCount]: { tag(4) + Offset16 (relative to ScriptList start) }
    let script_count = usize::from(read_u16(bytes, 0)?);
    if bytes.len() < 2 + script_count * 6 {
        return None;
    }

    type ScriptEntry = (
        [u8; 4],
        Option<RewrittenLangSys>,
        Vec<([u8; 4], RewrittenLangSys)>,
    );
    let mut surviving_scripts: Vec<ScriptEntry> = Vec::new();
    // Every Script body must start within 16 bits of the ScriptList, and
    // every LangSys body within 16 bits of its Script. These running
    // sizes let the walk stop as soon as that can no longer hold.
    let mut scripts_len = 0usize;

    for i in 0..script_count {
        let rec_off = 2 + i * 6;
        let tag = read_tag(bytes, rec_off)?;
        let script_off = usize::from(read_u16(bytes, rec_off + 4)?);
        let Some(script_body) = bytes.get(script_off..) else {
            continue;
        };
        // Script:
        //   Offset16 defaultLangSysOffset (Script-relative; 0 means none)
        //   u16      langSysCount
        //   LangSysRecord records[langSysCount]: { tag(4) + Offset16 (Script-relative) }
        let (Some(default_off), Some(langsys_count)) =
            (read_u16(script_body, 0), read_u16(script_body, 2))
        else {
            continue;
        };
        let default_off = usize::from(default_off);
        let langsys_count = usize::from(langsys_count);
        if !map.spend(1 + langsys_count) {
            return None;
        }
        let langsys_records_off = 4;
        let langsys_records_end = langsys_records_off + langsys_count * 6;
        if script_body.len() < langsys_records_end {
            continue;
        }

        let default = if default_off != 0 {
            script_body
                .get(default_off..)
                .and_then(|b| rewrite_langsys_from_bytes(b, feature_renumber, map))
        } else {
            None
        };

        let mut langsystems: Vec<([u8; 4], RewrittenLangSys)> = Vec::new();
        let mut langsys_len = default.as_ref().map_or(0, RewrittenLangSys::encoded_len);
        for j in 0..langsys_count {
            let lr = langsys_records_off + j * 6;
            let ls_tag = read_tag(script_body, lr)?;
            let ls_off = usize::from(read_u16(script_body, lr + 4)?);
            let Some(ls_body) = script_body.get(ls_off..) else {
                continue;
            };
            if let Some(rls) = rewrite_langsys_from_bytes(ls_body, feature_renumber, map) {
                if langsys_len > usize::from(u16::MAX) {
                    return None;
                }
                langsys_len += rls.encoded_len();
                langsystems.push((ls_tag, rls));
            }
        }
        if map.budget_spent() {
            return None;
        }

        if default.is_some() || !langsystems.is_empty() {
            if scripts_len > usize::from(u16::MAX) {
                return None;
            }
            scripts_len += 4 + langsystems.len() * 6 + langsys_len;
            surviving_scripts.push((tag, default, langsystems));
        }
    }

    if surviving_scripts.is_empty() {
        return None;
    }

    // Encode ScriptList:
    //   u16 scriptCount
    //   ScriptRecord records[scriptCount]: { tag(4) + Offset16 }
    //   Script[] bodies (each: defaultLangSysOffset + langSysCount + LangSysRecord[])
    //   LangSys[] bodies (each: lookupOrderOffset(0) + reqFeatureIndex + featureCount + indices[])
    let mut out = Vec::new();
    push_count16(&mut out, surviving_scripts.len())?;
    let script_records_start = out.len();
    out.resize(script_records_start + surviving_scripts.len() * 6, 0);
    for (i, (script_tag, default, langsystems)) in surviving_scripts.iter().enumerate() {
        let script_body_start = out.len();
        // Patch the ScriptRecord pointing at this body.
        let rec_off = script_records_start + i * 6;
        out.get_mut(rec_off..rec_off + 4)?
            .copy_from_slice(script_tag);
        patch_offset16(&mut out, rec_off + 4, script_body_start)?;

        let default_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // defaultLangSysOffset placeholder
        push_count16(&mut out, langsystems.len())?;
        let langsys_records_start = out.len();
        out.resize(langsys_records_start + langsystems.len() * 6, 0);
        // Default LangSys body, if any.
        if let Some(d) = default.as_ref() {
            let langsys_body_pos = out.len() - script_body_start;
            patch_offset16(&mut out, default_slot, langsys_body_pos)?;
            encode_langsys(&mut out, d)?;
        }
        // Named LangSys bodies.
        for (j, (ls_tag, ls)) in langsystems.iter().enumerate() {
            let langsys_body_pos = out.len() - script_body_start;
            let lr_off = langsys_records_start + j * 6;
            out.get_mut(lr_off..lr_off + 4)?.copy_from_slice(ls_tag);
            patch_offset16(&mut out, lr_off + 4, langsys_body_pos)?;
            encode_langsys(&mut out, ls)?;
        }
    }
    Some(out)
}

struct RewrittenLangSys {
    required_feature_index: u16,
    feature_indices: Vec<u16>,
}

impl RewrittenLangSys {
    /// Size of the encoded LangSys body in bytes.
    fn encoded_len(&self) -> usize {
        6 + self.feature_indices.len() * 2
    }
}

/// Walks LangSys raw bytes:
///   Offset16 lookupOrderOffset (=0)
///   u16 requiredFeatureIndex
///   u16 featureIndexCount
///   u16 featureIndices[featureIndexCount]
fn rewrite_langsys_from_bytes(
    bytes: &[u8],
    feature_renumber: &[Option<u16>],
    map: &GidMap,
) -> Option<RewrittenLangSys> {
    let required = read_u16(bytes, 2)?;
    let count = usize::from(read_u16(bytes, 4)?);
    let need = 6 + count * 2;
    if bytes.len() < need || !map.spend(1 + count) {
        return None;
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
    for fi in bytes
        .get(6..need)?
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
    {
        if let Some(Some(new)) = feature_renumber.get(fi as usize) {
            new_indices.push(*new);
        }
    }
    if new_required == 0xFFFF && new_indices.is_empty() {
        return None;
    }
    Some(RewrittenLangSys {
        required_feature_index: new_required,
        feature_indices: new_indices,
    })
}

/// Appends an encoded LangSys body to `out`. Returns `None` when the
/// feature index count does not fit in 16 bits.
fn encode_langsys(out: &mut Vec<u8>, ls: &RewrittenLangSys) -> Option<()> {
    // LangSys:
    //   Offset16 lookupOrderOffset (0, reserved)
    //   u16 requiredFeatureIndex
    //   u16 featureIndexCount
    //   u16 featureIndices[featureIndexCount]
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&ls.required_feature_index.to_be_bytes());
    push_count16(out, ls.feature_indices.len())?;
    for idx in &ls.feature_indices {
        out.extend_from_slice(&idx.to_be_bytes());
    }
    Some(())
}

/// Assembles a complete GSUB or GPOS table (their headers are
/// identical). Builds the LookupList around the rewritten lookups.
///
/// Lookups are first laid out inline, which is what the source font
/// used when it fit. When a lookup or subtable offset would overflow
/// 16 bits, every lookup is re-emitted as an Extension lookup
/// (`extension_type`) whose 32-bit offsets reach subtables stored after
/// the LookupList. Returns `None` when even that layout does not fit.
fn assemble_layout_table(
    script_list: &[u8],
    feature_list: &[u8],
    lookups: &[RewrittenLookup],
    extension_type: u16,
) -> Option<Vec<u8>> {
    assemble_inline(script_list, feature_list, lookups)
        .or_else(|| assemble_with_extensions(script_list, feature_list, lookups, extension_type))
}

/// Writes the GSUB / GPOS v1.0 header followed by the ScriptList and
/// FeatureList. The LookupList starts at the returned buffer's end.
fn start_layout_table(script_list: &[u8], feature_list: &[u8]) -> Option<Vec<u8>> {
    //   u16 majorVersion = 1
    //   u16 minorVersion = 0
    //   Offset16 scriptListOffset
    //   Offset16 featureListOffset
    //   Offset16 lookupListOffset
    let header_len = 10usize;
    let feature_list_off = header_len + script_list.len();
    let lookup_list_off = feature_list_off + feature_list.len();
    let mut out = Vec::with_capacity(lookup_list_off);
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&u16::try_from(header_len).ok()?.to_be_bytes());
    out.extend_from_slice(&u16::try_from(feature_list_off).ok()?.to_be_bytes());
    out.extend_from_slice(&u16::try_from(lookup_list_off).ok()?.to_be_bytes());
    out.extend_from_slice(script_list);
    out.extend_from_slice(feature_list);
    Some(out)
}

/// Lays every lookup out with its subtables inline. Returns `None` on
/// the first offset that does not fit in 16 bits.
fn assemble_inline(
    script_list: &[u8],
    feature_list: &[u8],
    lookups: &[RewrittenLookup],
) -> Option<Vec<u8>> {
    let mut out = start_layout_table(script_list, feature_list)?;

    // LookupList:
    //   u16 lookupCount
    //   Offset16 lookupOffsets[lookupCount]
    //   Lookup[] bodies
    let lookup_list_start = out.len();
    push_count16(&mut out, lookups.len())?;
    let offsets_start = out.len();
    out.resize(offsets_start + lookups.len() * 2, 0);
    for (i, lookup) in lookups.iter().enumerate() {
        let body_start = out.len();
        patch_offset16(
            &mut out,
            offsets_start + i * 2,
            body_start - lookup_list_start,
        )?;
        // Lookup header:
        //   u16 lookupType
        //   u16 lookupFlag
        //   u16 subtableCount
        //   Offset16 subtableOffsets[subtableCount]
        //   (u16 markFilteringSet, only if flag bit set)
        out.extend_from_slice(&lookup.lookup_type.to_be_bytes());
        out.extend_from_slice(&lookup.lookup_flag.to_be_bytes());
        push_count16(&mut out, lookup.subtables.len())?;
        let sub_offsets_start = out.len();
        out.resize(sub_offsets_start + lookup.subtables.len() * 2, 0);
        if let Some(mfs) = lookup.mark_filtering_set {
            out.extend_from_slice(&mfs.to_be_bytes());
        }
        for (j, sub) in lookup.subtables.iter().enumerate() {
            let sub_start = out.len();
            patch_offset16(&mut out, sub_offsets_start + j * 2, sub_start - body_start)?;
            out.extend_from_slice(&sub.bytes);
        }
    }
    Some(out)
}

/// Lays every lookup out as an Extension lookup. The lookup headers and
/// their 8-byte Extension records stay within 16 bits of the
/// LookupList; the wrapped subtables follow the whole list and are
/// reached through 32-bit offsets.
fn assemble_with_extensions(
    script_list: &[u8],
    feature_list: &[u8],
    lookups: &[RewrittenLookup],
    extension_type: u16,
) -> Option<Vec<u8>> {
    let mut out = start_layout_table(script_list, feature_list)?;
    let lookup_list_start = out.len();
    push_count16(&mut out, lookups.len())?;
    let offsets_start = out.len();
    out.resize(offsets_start + lookups.len() * 2, 0);

    // (Extension record position, wrapped subtable bytes), in emit order.
    let mut pending: Vec<(usize, &[u8])> = Vec::new();
    for (i, lookup) in lookups.iter().enumerate() {
        let body_start = out.len();
        patch_offset16(
            &mut out,
            offsets_start + i * 2,
            body_start - lookup_list_start,
        )?;
        out.extend_from_slice(&extension_type.to_be_bytes());
        out.extend_from_slice(&lookup.lookup_flag.to_be_bytes());
        push_count16(&mut out, lookup.subtables.len())?;
        let sub_offsets_start = out.len();
        out.resize(sub_offsets_start + lookup.subtables.len() * 2, 0);
        if let Some(mfs) = lookup.mark_filtering_set {
            out.extend_from_slice(&mfs.to_be_bytes());
        }
        for (j, sub) in lookup.subtables.iter().enumerate() {
            let (inner_type, inner) = extension_payload(lookup, sub, extension_type)?;
            let record_start = out.len();
            patch_offset16(
                &mut out,
                sub_offsets_start + j * 2,
                record_start - body_start,
            )?;
            // ExtensionSubstFormat1 / ExtensionPosFormat1:
            //   u16 format = 1, u16 extensionLookupType, Offset32 extensionOffset
            out.extend_from_slice(&1u16.to_be_bytes());
            out.extend_from_slice(&inner_type.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            pending.push((record_start, inner));
        }
    }
    for (record_start, inner) in pending {
        let rel = u32::try_from(out.len() - record_start).ok()?;
        out.get_mut(record_start + 4..record_start + 8)?
            .copy_from_slice(&rel.to_be_bytes());
        out.extend_from_slice(inner);
    }
    Some(out)
}

/// Returns the lookup type and subtable bytes an Extension record
/// should wrap for `sub`. Subtables of a lookup that already is an
/// Extension lookup carry their own 8-byte Extension header, which is
/// peeled off here.
fn extension_payload<'s>(
    lookup: &RewrittenLookup,
    sub: &'s RewrittenSubtable,
    extension_type: u16,
) -> Option<(u16, &'s [u8])> {
    if lookup.lookup_type != extension_type {
        return Some((lookup.lookup_type, &sub.bytes));
    }
    let inner_type = read_u16(&sub.bytes, 2)?;
    let inner_off = usize::try_from(read_u32(&sub.bytes, 4)?).ok()?;
    Some((inner_type, sub.bytes.get(inner_off..)?))
}

// === Byte-level helpers used by the rewriters and the closure walker. ===

/// Reads a big-endian `u16` at `off`, or `None` past the end.
pub(crate) fn read_u16(bytes: &[u8], off: usize) -> Option<u16> {
    let chunk = bytes.get(off..)?.first_chunk::<2>()?;
    Some(u16::from_be_bytes(*chunk))
}

/// Reads a big-endian `u32` at `off`, or `None` past the end.
fn read_u32(bytes: &[u8], off: usize) -> Option<u32> {
    let chunk = bytes.get(off..)?.first_chunk::<4>()?;
    Some(u32::from_be_bytes(*chunk))
}

/// Reads a 4-byte tag at `off`, or `None` past the end.
fn read_tag(bytes: &[u8], off: usize) -> Option<[u8; 4]> {
    bytes.get(off..)?.first_chunk::<4>().copied()
}

/// Writes `value` as a big-endian Offset16 into `out[slot..slot + 2]`.
/// Returns `None` when `value` does not fit in 16 bits, which means the
/// rewritten structure is too large for its offset field.
pub(crate) fn patch_offset16(out: &mut [u8], slot: usize, value: usize) -> Option<()> {
    let value = u16::try_from(value).ok()?;
    out.get_mut(slot..slot + 2)?
        .copy_from_slice(&value.to_be_bytes());
    Some(())
}

/// Appends `count` as a big-endian `u16`. Returns `None` when it does
/// not fit.
pub(crate) fn push_count16(out: &mut Vec<u8>, count: usize) -> Option<()> {
    out.extend_from_slice(&u16::try_from(count).ok()?.to_be_bytes());
    Some(())
}

/// Best-effort enumeration of the glyphs covered by a Coverage table
/// given its raw bytes. Returns an empty vec on any parse failure.
///
/// Glyphs come back in table order, so position `i` in the result is
/// the coverage index a valid table assigns. The walk stops after
/// [`MAX_GLYPH_ENTRIES`] glyphs: a valid table never lists more, and
/// overlapping ranges in a malformed one could otherwise expand into
/// billions of entries.
///
/// Shared by the per-lookup-type rewriters in [`crate::gsub`] /
/// [`crate::gpos`] and the closure walker in [`crate::closure`].
pub(crate) fn parse_coverage_glyphs(bytes: &[u8]) -> Vec<u16> {
    let mut out = Vec::new();
    let (Some(format), Some(count)) = (read_u16(bytes, 0), read_u16(bytes, 2)) else {
        return out;
    };
    let count = usize::from(count);
    match format {
        1 => {
            let Some(glyphs) = bytes.get(4..4 + count * 2) else {
                return out;
            };
            out.extend(
                glyphs
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]])),
            );
        }
        2 => {
            let Some(records) = bytes.get(4..4 + count * 6) else {
                return out;
            };
            for rec in records.chunks_exact(6) {
                let start = u16::from_be_bytes([rec[0], rec[1]]);
                let end = u16::from_be_bytes([rec[2], rec[3]]);
                let room = MAX_GLYPH_ENTRIES.saturating_sub(out.len());
                if room == 0 {
                    break;
                }
                out.extend((start..=end).take(room));
            }
        }
        _ => {}
    }
    out
}

/// Walks a ClassDef's raw bytes to enumerate every `(gid, class)`
/// pair, skipping class-0 entries.
///
/// Pairs come back in table order. The walk stops after
/// [`MAX_GLYPH_ENTRIES`] pairs: a valid table never lists more, and
/// overlapping ranges in a malformed one could otherwise expand into
/// billions of entries.
pub(crate) fn parse_classdef_pairs_from_bytes(bytes: &[u8]) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    let Some(format) = read_u16(bytes, 0) else {
        return out;
    };
    match format {
        1 => {
            // Format 1: u16 format, u16 startGlyphID, u16 glyphCount, u16 values[count].
            let (Some(start), Some(count)) = (read_u16(bytes, 2), read_u16(bytes, 4)) else {
                return out;
            };
            let Some(values) = bytes.get(6..6 + usize::from(count) * 2) else {
                return out;
            };
            for (i, c) in values.chunks_exact(2).enumerate() {
                let class = u16::from_be_bytes([c[0], c[1]]);
                if class == 0 {
                    continue;
                }
                // `i < glyphCount <= u16::MAX`. A valid table never runs
                // past glyph 0xFFFF; a malformed one repeats that glyph.
                out.push((start.saturating_add(i as u16), class));
            }
        }
        2 => {
            // Format 2: u16 format, u16 rangeCount, RangeRecord[count]: u16 start, u16 end, u16 class.
            let Some(count) = read_u16(bytes, 2) else {
                return out;
            };
            let Some(records) = bytes.get(4..4 + usize::from(count) * 6) else {
                return out;
            };
            for r in records.chunks_exact(6) {
                let start = u16::from_be_bytes([r[0], r[1]]);
                let end = u16::from_be_bytes([r[2], r[3]]);
                let class = u16::from_be_bytes([r[4], r[5]]);
                if class == 0 {
                    continue;
                }
                let room = MAX_GLYPH_ENTRIES.saturating_sub(out.len());
                if room == 0 {
                    break;
                }
                out.extend((start..=end).take(room).map(|g| (g, class)));
            }
        }
        _ => {}
    }
    out
}

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
        let plan = decide(&face, &kept, &input).unwrap();
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
        let plan = decide(&face, &kept, &input).unwrap();
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
        let plan = decide(&face, &kept, &input).unwrap();
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

    /// Format 2 table (Coverage or ClassDef) with `count` copies of a
    /// range covering every glyph, all in class 1.
    fn overlapping_full_ranges(count: u16) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&count.to_be_bytes());
        for _ in 0..count {
            bytes.extend_from_slice(&0u16.to_be_bytes());
            bytes.extend_from_slice(&0xFFFFu16.to_be_bytes());
            bytes.extend_from_slice(&1u16.to_be_bytes());
        }
        bytes
    }

    #[test]
    fn parse_coverage_glyphs_caps_overlapping_ranges() {
        // 65535 full ranges describe about 4.3 billion entries in 393 KB.
        // The walk used to materialize all of them.
        let bytes = overlapping_full_ranges(u16::MAX);
        assert_eq!(parse_coverage_glyphs(&bytes).len(), MAX_GLYPH_ENTRIES);
    }

    #[test]
    fn parse_classdef_pairs_caps_overlapping_ranges() {
        let bytes = overlapping_full_ranges(u16::MAX);
        assert_eq!(
            parse_classdef_pairs_from_bytes(&bytes).len(),
            MAX_GLYPH_ENTRIES
        );
    }

    #[test]
    fn gid_map_budget_stops_coverage_walks() {
        let map = GidMap::from_kept(&[0, 1]);
        let bytes = overlapping_full_ranges(1);
        assert!(map.coverage_glyphs(&bytes).is_some());
        assert!(!map.spend(usize::MAX));
        assert!(map.budget_spent());
        assert!(map.coverage_glyphs(&bytes).is_none());
        assert!(map.classdef_pairs(&bytes).is_none());
        map.reset_budget();
        assert!(map.coverage_glyphs(&bytes).is_some());
    }

    /// One lookup of type 1 holding `count` subtables of `size` bytes
    /// each. Subtable bytes start with format 1 so they read as valid.
    fn big_lookup(count: usize, size: usize) -> RewrittenLookup {
        let mut body = vec![0u8; size];
        body[1] = 1;
        RewrittenLookup {
            lookup_type: 1,
            lookup_flag: 0,
            mark_filtering_set: None,
            subtables: (0..count)
                .map(|_| RewrittenSubtable {
                    bytes: body.clone(),
                })
                .collect(),
        }
    }

    /// Reads `(lookup type, [(wrapped type, subtable offset)])` for each
    /// lookup of an assembled table, following Extension records.
    fn read_lookups(table: &[u8], ext: u16) -> Vec<(u16, Vec<(u16, usize)>)> {
        let rd = |o: usize| usize::from(u16::from_be_bytes([table[o], table[o + 1]]));
        let ll = rd(8);
        (0..rd(ll))
            .map(|i| {
                let base = ll + rd(ll + 2 + i * 2);
                let ty = rd(base) as u16;
                let subs = (0..rd(base + 4))
                    .map(|s| {
                        let sub = base + rd(base + 6 + s * 2);
                        if ty == ext {
                            let off = u32::from_be_bytes([
                                table[sub + 4],
                                table[sub + 5],
                                table[sub + 6],
                                table[sub + 7],
                            ]) as usize;
                            (rd(sub + 2) as u16, sub + off)
                        } else {
                            (ty, sub)
                        }
                    })
                    .collect();
                (ty, subs)
            })
            .collect()
    }

    #[test]
    fn assemble_keeps_inline_layout_when_offsets_fit() {
        let lookups = vec![big_lookup(2, 100)];
        let table = assemble_layout_table(&[0, 0], &[0, 0], &lookups, 7).unwrap();
        let read = read_lookups(&table, 7);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].0, 1);
    }

    #[test]
    fn assemble_promotes_to_extension_lookups_past_16_bit_offsets() {
        // Three lookups of 40 KB each: the third lookup starts past
        // 64 KB, which used to wrap its Offset16 and point it at the
        // wrong bytes.
        let lookups = vec![
            big_lookup(1, 40_000),
            big_lookup(1, 40_000),
            big_lookup(2, 30_000),
        ];
        let table = assemble_layout_table(&[0, 0], &[0, 0], &lookups, 7).unwrap();
        let read = read_lookups(&table, 7);
        assert_eq!(read.len(), 3);
        for (lookup, subs) in &read {
            assert_eq!(*lookup, 7, "every lookup becomes an Extension lookup");
            for &(wrapped, off) in subs {
                assert_eq!(wrapped, 1);
                assert_eq!(&table[off..off + 2], &1u16.to_be_bytes());
            }
        }
        assert_eq!(read[2].1.len(), 2);
    }

    #[test]
    fn assemble_fails_when_script_and_feature_lists_overflow() {
        let lookups = vec![big_lookup(1, 10)];
        let huge = vec![0u8; 70_000];
        assert!(assemble_layout_table(&huge, &[0, 0], &lookups, 7).is_none());
    }
}
