//! Layout-table (`GSUB` / `GPOS` / `GDEF`) emission.
//!
//! The subsetter ships byte-level rewriters keyed on the new
//! gid namespace. The rewriters live in [`crate::gsub`],
//! [`crate::gpos`], and [`crate::gdef`]; this module owns the
//! infrastructure they share:
//!
//! - [`GidMap`] — the old→new gid translator built once per subset.
//! - [`RewriterCtx`] — the borrow bag passed to per-lookup-type
//!   rewriters.
//! - [`RewrittenLookup`] / [`RewrittenSubtable`] — the value types
//!   the per-lookup-type rewriters produce.
//! - [`parse_coverage_glyphs`] / [`parse_classdef_pairs`] — small
//!   byte-walking helpers for the rewriters and the closure walker.
//! - [`build_gsub`] / [`build_gpos`] — the drivers that walk every
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
//! Surviving lookups are then renumbered to 0..N. Every reference —
//! the FeatureList's lookup-index list — is rewritten through the
//! same renumber map.
//!
//! # Coverage matrix today
//!
//! - **GSUB type 1 (single-sub)** — full byte-level rewriter (formats
//!   1+2). See [`crate::gsub`].
//! - **GSUB types 2–6 + 8** — full byte-level rewriters. See [`crate::gsub`].
//! - **GSUB type 7 (extension)** — pass-through, recurses into the
//!   inner subtable.
//! - **Any GSUB lookup type without a rewriter** — returns `None` for
//!   every subtable. The drop cascade handles propagation.
//! - **All GPOS lookup types** — drop. The drop cascade then drops
//!   GPOS entirely.
//! - **GDEF GlyphClassDef + MarkAttachClassDef** — full ClassDef
//!   rewriter via [`crate::classdef`]. AttachList, LigCaretList,
//!   MarkGlyphSetsDef, ItemVariationStore drop.
//!
//! Per-type rewriters slot in here one at a time; the per-type module
//! call sites are stable, so adding a new lookup-type rewriter does
//! not require touching this module.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use crate::{gdef, gpos, gsub, GlyphId, SubsetError, SubsetInput};

/// A new-namespace gid translator. `map(old) → Some(new)` when the
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
    /// new-gid 1, and so on — matching the convention `subset()` uses
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
    /// Optional old → new lookup-index map. Set during the second
    /// pass over context-style lookups (GSUB types 5 / 6) so their
    /// nested `SubstLookupRecord` entries can be patched. `None` on
    /// the first pass — context rewriters preserve the source's
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
        match gdef::rewrite_gdef(face, &map) {
            Some(b) => Decision::Rewrite(b),
            None => Decision::Drop,
        }
    } else {
        Decision::Drop
    };

    Ok(LayoutPlan { gsub, gpos, gdef })
}

/// Drives the GSUB rewrite. Walks every lookup, runs the per-type
/// rewriter, then runs the drop cascade and renumbers surviving
/// lookups. Returns the new GSUB bytes or `None` when the table
/// drops entirely.
pub(crate) fn build_gsub(face: &Face<'_>, ctx: &RewriterCtx) -> Option<Vec<u8>> {
    let gsub_table = face.gsub().ok().flatten()?;
    let lookups = gsub_table.lookup_list();

    // Phase 1: per-lookup rewrite. Context-style lookups (types
    // 5 / 6) carry nested `SubstLookupRecord` entries that point at
    // sibling lookups by index; on this pass we don't yet know
    // which sibling lookups survive, so the rewriters preserve the
    // source's lookup-list indices verbatim and we patch them in
    // phase 2 once the renumber map is known.
    let mut rewritten: Vec<Option<RewrittenLookup>> = Vec::with_capacity(lookups.len() as usize);
    for li in 0..lookups.len() {
        let Some(lookup) = lookups.get(li) else {
            rewritten.push(None);
            continue;
        };
        let mut subtable_bodies: Vec<&[u8]> = Vec::new();
        for si in 0..lookup.subtable_count() {
            if let Some(b) = lookup.subtable_bytes(si) {
                subtable_bodies.push(b);
            }
        }
        let rewritten_lookup = gsub::rewrite_lookup(
            ctx,
            lookup.lookup_type(),
            lookup.flag(),
            lookup.mark_filtering_set(),
            &subtable_bodies,
        );
        rewritten.push(rewritten_lookup);
    }

    // Phase 2: iterate context-lookup renumber to a fixed point.
    // Each iteration rebuilds the renumber map from the surviving
    // lookups, then re-rewrites every context-style lookup with the
    // new map; a context lookup whose `SubstLookupRecord`s all point
    // at dropped lookups loses every subtable and falls out, which
    // may in turn cascade into other context lookups losing their
    // targets. Bounded by `lookups.len()` since each iteration only
    // ever drops more lookups (or stabilises).
    let mut renumber = build_renumber(&rewritten);
    for _ in 0..lookups.len() {
        let mut changed = false;
        let inner_ctx = RewriterCtx {
            gid_map: ctx.gid_map,
            lookup_renumber: Some(&renumber),
        };
        for li in 0..lookups.len() {
            // Only re-rewrite slots that survived phase 1; nothing to
            // resurrect here.
            if rewritten
                .get(li as usize)
                .and_then(|s| s.as_ref())
                .is_none()
            {
                continue;
            }
            let Some(lookup) = lookups.get(li) else {
                continue;
            };
            // Skip non-context lookup types — their rewrite output is
            // independent of the renumber map.
            let lt = gsub::context_lookup_type(&lookup);
            let Some(_lt) = lt else { continue };
            let mut subtable_bodies: Vec<&[u8]> = Vec::new();
            for si in 0..lookup.subtable_count() {
                if let Some(b) = lookup.subtable_bytes(si) {
                    subtable_bodies.push(b);
                }
            }
            let new_lookup = gsub::rewrite_lookup(
                &inner_ctx,
                lookup.lookup_type(),
                lookup.flag(),
                lookup.mark_filtering_set(),
                &subtable_bodies,
            );
            // A context lookup whose every nested target dropped
            // returns None now that the renumber knows. Mark it as
            // dropped and trigger another pass.
            if new_lookup.is_none() {
                if rewritten[li as usize].is_some() {
                    rewritten[li as usize] = None;
                    changed = true;
                }
            } else {
                rewritten[li as usize] = new_lookup;
            }
        }
        if !changed {
            break;
        }
        renumber = build_renumber(&rewritten);
    }

    // Phase 3: rewrite features and scripts. ScriptList walks raw
    // bytes because the parser doesn't expose enumeration of named
    // LangSys records.
    let feature_list = gsub_table.feature_list();
    let new_features = rewrite_features(*feature_list, &renumber)?;

    let gsub_bytes = face.table_bytes(tag::GSUB).ok()?;
    let script_list_off = u16::from_be_bytes([gsub_bytes[4], gsub_bytes[5]]) as usize;
    let script_list_bytes = gsub_bytes.get(script_list_off..)?;
    let new_scripts =
        rewrite_scripts_from_bytes(script_list_bytes, &new_features.feature_renumber)?;

    let new_lookups: Vec<RewrittenLookup> = rewritten.into_iter().flatten().collect();
    if new_lookups.is_empty() {
        return None;
    }

    Some(assemble_layout_table(
        &new_scripts,
        &new_features.bytes,
        &new_lookups,
    ))
}

/// Drives the GPOS rewrite — same shape as [`build_gsub`]. Today the
/// GPOS per-type rewriters drop everything, so this returns `None`
/// whenever the source GPOS has any lookup.
pub(crate) fn build_gpos(face: &Face<'_>, ctx: &RewriterCtx) -> Option<Vec<u8>> {
    let gpos_table = face.gpos().ok().flatten()?;
    let lookups = gpos_table.lookup_list();

    let mut rewritten: Vec<Option<RewrittenLookup>> = Vec::with_capacity(lookups.len() as usize);
    for li in 0..lookups.len() {
        let Some(lookup) = lookups.get(li) else {
            rewritten.push(None);
            continue;
        };
        let mut subtable_bodies: Vec<&[u8]> = Vec::new();
        for si in 0..lookup.subtable_count() {
            if let Some(b) = lookup.subtable_bytes(si) {
                subtable_bodies.push(b);
            }
        }
        let rewritten_lookup = gpos::rewrite_lookup(
            ctx,
            lookup.lookup_type(),
            lookup.flag(),
            lookup.mark_filtering_set(),
            &subtable_bodies,
        );
        rewritten.push(rewritten_lookup);
    }

    let renumber = build_renumber(&rewritten);
    let feature_list = gpos_table.feature_list();
    let new_features = rewrite_features(*feature_list, &renumber)?;

    let gpos_bytes = face.table_bytes(tag::GPOS).ok()?;
    let script_list_off = u16::from_be_bytes([gpos_bytes[4], gpos_bytes[5]]) as usize;
    let script_list_bytes = gpos_bytes.get(script_list_off..)?;
    let new_scripts =
        rewrite_scripts_from_bytes(script_list_bytes, &new_features.feature_renumber)?;

    let new_lookups: Vec<RewrittenLookup> = rewritten.into_iter().flatten().collect();
    if new_lookups.is_empty() {
        return None;
    }

    Some(assemble_layout_table(
        &new_scripts,
        &new_features.bytes,
        &new_lookups,
    ))
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
    /// Old feature index → new feature index (or None if dropped).
    feature_renumber: Vec<Option<u16>>,
}

/// Rewrites the FeatureList. Drops any feature whose lookup-index list
/// becomes empty after the lookup renumber. Returns the new bytes plus
/// a feature-index renumber map.
fn rewrite_features(
    feature_list: sigilbuzz::tables::layout::FeatureList<'_>,
    lookup_renumber: &[Option<u16>],
) -> Option<RewrittenFeatures> {
    let mut surviving: Vec<([u8; 4], Vec<u16>)> = Vec::new();
    let mut feature_renumber: Vec<Option<u16>> = Vec::with_capacity(feature_list.len() as usize);
    for fi in 0..feature_list.len() {
        let Some((tag, feature)) = feature_list.get(fi) else {
            feature_renumber.push(None);
            continue;
        };
        let new_indices: Vec<u16> = feature
            .lookup_indices()
            .filter_map(|li| lookup_renumber.get(li as usize).copied().flatten())
            .collect();
        if new_indices.is_empty() {
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
        let body_off = body_start as u16;
        out[rec_off + 4..rec_off + 6].copy_from_slice(&body_off.to_be_bytes());
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
/// one script survives.
fn rewrite_scripts_from_bytes(bytes: &[u8], feature_renumber: &[Option<u16>]) -> Option<Vec<u8>> {
    // ScriptList:
    //   u16 scriptCount
    //   ScriptRecord records[scriptCount]: { tag(4) + Offset16 (relative to ScriptList start) }
    if bytes.len() < 2 {
        return None;
    }
    let script_count = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
    if bytes.len() < 2 + script_count * 6 {
        return None;
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
        let Some(script_body) = bytes.get(script_off..) else {
            continue;
        };
        if script_body.len() < 4 {
            continue;
        }
        // Script:
        //   Offset16 defaultLangSysOffset (Script-relative; 0 means none)
        //   u16      langSysCount
        //   LangSysRecord records[langSysCount]: { tag(4) + Offset16 (Script-relative) }
        let default_off = u16::from_be_bytes([script_body[0], script_body[1]]) as usize;
        let langsys_count = u16::from_be_bytes([script_body[2], script_body[3]]) as usize;
        let langsys_records_off = 4;
        let langsys_records_end = langsys_records_off + langsys_count * 6;
        if script_body.len() < langsys_records_end {
            continue;
        }

        let default = if default_off != 0 {
            script_body
                .get(default_off..)
                .and_then(|b| rewrite_langsys_from_bytes(b, feature_renumber))
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
            let Some(ls_body) = script_body.get(ls_off..) else {
                continue;
            };
            if let Some(rls) = rewrite_langsys_from_bytes(ls_body, feature_renumber) {
                langsystems.push((ls_tag, rls));
            }
        }

        if default.is_some() || !langsystems.is_empty() {
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
        let body_off_u16 = script_body_start as u16;
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
                .copy_from_slice(&(langsys_body_pos as u16).to_be_bytes());
        }
        // Named LangSys bodies.
        for (j, (ls_tag, ls)) in langsystems.iter().enumerate() {
            let langsys_body_pos = out.len() - script_body_start;
            out.extend_from_slice(&encode_langsys(ls));
            let lr_off = langsys_records_start + j * 6;
            out[lr_off..lr_off + 4].copy_from_slice(ls_tag);
            out[lr_off + 4..lr_off + 6].copy_from_slice(&(langsys_body_pos as u16).to_be_bytes());
        }
    }
    Some(out)
}

struct RewrittenLangSys {
    required_feature_index: u16,
    feature_indices: Vec<u16>,
}

/// Walks LangSys raw bytes:
///   Offset16 lookupOrderOffset (=0)
///   u16 requiredFeatureIndex
///   u16 featureIndexCount
///   u16 featureIndices[featureIndexCount]
fn rewrite_langsys_from_bytes(
    bytes: &[u8],
    feature_renumber: &[Option<u16>],
) -> Option<RewrittenLangSys> {
    if bytes.len() < 6 {
        return None;
    }
    let _lookup_order = u16::from_be_bytes([bytes[0], bytes[1]]);
    let required = u16::from_be_bytes([bytes[2], bytes[3]]);
    let count = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    let need = 6 + count * 2;
    if bytes.len() < need {
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
    for i in 0..count {
        let off = 6 + i * 2;
        let fi = u16::from_be_bytes([bytes[off], bytes[off + 1]]);
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

fn encode_langsys(ls: &RewrittenLangSys) -> Vec<u8> {
    // LangSys:
    //   Offset16 lookupOrderOffset (0 — reserved)
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
/// identical). Builds the LookupList around the rewritten lookups.
fn assemble_layout_table(
    script_list: &[u8],
    feature_list: &[u8],
    lookups: &[RewrittenLookup],
) -> Vec<u8> {
    // GSUB/GPOS header (v1.0):
    //   u16 majorVersion = 1
    //   u16 minorVersion = 0
    //   Offset16 scriptListOffset
    //   Offset16 featureListOffset
    //   Offset16 lookupListOffset
    let header_len: u16 = 10;
    let script_list_off = header_len;
    let feature_list_off = script_list_off + script_list.len() as u16;
    let lookup_list_off = feature_list_off + feature_list.len() as u16;

    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&script_list_off.to_be_bytes());
    out.extend_from_slice(&feature_list_off.to_be_bytes());
    out.extend_from_slice(&lookup_list_off.to_be_bytes());
    out.extend_from_slice(script_list);
    out.extend_from_slice(feature_list);

    // LookupList:
    //   u16 lookupCount
    //   Offset16 lookupOffsets[lookupCount]
    //   Lookup[] bodies
    let lookup_list_start = out.len();
    out.extend_from_slice(&(lookups.len() as u16).to_be_bytes());
    let offsets_start = out.len();
    for _ in 0..lookups.len() {
        out.extend_from_slice(&0u16.to_be_bytes());
    }
    for (i, lookup) in lookups.iter().enumerate() {
        let body_start = out.len();
        let rel = (body_start - lookup_list_start) as u16;
        let slot = offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&rel.to_be_bytes());
        // Lookup header:
        //   u16 lookupType
        //   u16 lookupFlag
        //   u16 subtableCount
        //   Offset16 subtableOffsets[subtableCount]
        //   (u16 markFilteringSet — only if flag bit set)
        out.extend_from_slice(&lookup.lookup_type.to_be_bytes());
        out.extend_from_slice(&lookup.lookup_flag.to_be_bytes());
        out.extend_from_slice(&(lookup.subtables.len() as u16).to_be_bytes());
        let sub_offsets_start = out.len();
        for _ in 0..lookup.subtables.len() {
            out.extend_from_slice(&0u16.to_be_bytes());
        }
        if let Some(mfs) = lookup.mark_filtering_set {
            out.extend_from_slice(&mfs.to_be_bytes());
        }
        for (j, sub) in lookup.subtables.iter().enumerate() {
            let sub_start = out.len();
            let rel = (sub_start - body_start) as u16;
            let slot = sub_offsets_start + j * 2;
            out[slot..slot + 2].copy_from_slice(&rel.to_be_bytes());
            out.extend_from_slice(&sub.bytes);
        }
    }
    out
}

// === Byte-level helpers used by the rewriters and the closure walker. ===

/// Best-effort enumeration of the glyphs covered by a Coverage table
/// given its raw bytes. Returns an empty vec on any parse failure.
///
/// Mirrors the helper in [`crate::closure`] — exposed here so the
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
}
