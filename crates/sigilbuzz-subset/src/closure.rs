//! Glyph-id closure walker.
//!
//! Starting from the caller's gid set, we expand transitively through:
//!
//! - **Composite components** in `glyf`: a kept composite glyph keeps
//!   every reference it needs to render correctly.
//! - **Ligature components** in `GSUB` type 4: if a kept gid is the
//!   *output* of a ligature substitution, every input component must
//!   also survive so shaping the input string still triggers the
//!   substitution. The forward direction is also pulled: if a kept
//!   gid is the *first component* (i.e. listed in Coverage) **and**
//!   every other component is also kept, the result gid is pulled in
//!   too. Otherwise `subset(face, &[f, i])` would silently lose the
//!   `fi` ligature gid and shaping the input pair against the subset
//!   would fall back to the unligatured glyph stream.
//! - **Substitution targets** in `GSUB` types 1, 2, and 3 (see
//!   [`crate::gsub::pull_in_substitution_targets`]).
//! - **Mark attachment partners** in `GPOS` types 4, 5, and 6: when a
//!   kept mark is covered by a mark attachment subtable, every base,
//!   ligature, or mark2 glyph of that subtable is pulled in, so the
//!   mark can still attach. The opposite direction is *not* pulled in:
//!   marks attach optionally, and a base subset that drops its marks
//!   simply renders without them.
//! - **VARC components** (see [`crate::varc::varc_closure_bitset`]).
//!
//! Glyph 0 (`.notdef`) is always retained: every SFNT font has one,
//! every glyph index that fails a cmap lookup falls back to it, and
//! every TrueType-outlined font's first glyf entry is reserved for it.
//!
//! The walker iterates to a fixed point: pulling in a ligature
//! component may expand the kept set, which may itself be the output
//! of another ligature, etc. Glyph references at or past `numGlyphs`
//! are ignored, so the kept set never names a glyph the font lacks.
//!
//! Every pass charges a shared [`WorkBudget`]. Offsets in a hostile
//! font can make each pass revisit the same bytes billions of times;
//! once the budget runs out the walk stops and returns the glyphs kept
//! so far.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::tables::Reader;
use sigilbuzz::Face;

use crate::layout::parse_coverage_glyphs;
use crate::util::{WorkBudget, WORK_LIMIT};
use crate::SubsetError;

/// Computes the closure of `seed` over the source font's reference
/// graph (composites, ligatures, substitutions, mark anchors, VARC
/// components). The returned vec is sorted ascending and contains gid
/// 0 even when `seed` does not. Seed gids at or past `numGlyphs` are
/// ignored.
///
/// Returns [`SubsetError::GidOutOfRange`] for gid 0 when the font
/// reports zero glyphs, since it then has no `.notdef` to keep.
pub fn compute_closure(face: &Face<'_>, seed: &[u16]) -> Result<Vec<u16>, SubsetError> {
    let num_glyphs = face.maxp()?.num_glyphs;

    // Bitset for membership: a Vec<bool> sized to num_glyphs is
    // O(numGlyphs) in memory but lookups are O(1) and writes are
    // deterministic: no HashMap iteration order to worry about.
    let mut keep = alloc::vec![false; num_glyphs as usize];
    let Some(notdef) = keep.first_mut() else {
        return Err(SubsetError::GidOutOfRange { gid: 0, num_glyphs });
    };
    *notdef = true;
    for &g in seed {
        if let Some(slot) = keep.get_mut(g as usize) {
            *slot = true;
        }
    }

    // Iterate to a fixed point. Each pass pulls in references from one
    // table; subsequent passes pick up second-order pull-ins (e.g. a
    // ligature whose output was itself dragged in by a composite).
    let budget = WorkBudget::new(WORK_LIMIT);
    loop {
        // Each pass scans the whole bitset a few times.
        if !budget.spend(keep.len()) {
            break;
        }
        let before = count_kept(&keep);
        expand_glyf_composites(face, &mut keep, &budget)?;
        expand_gsub_ligatures(face, &mut keep, &budget);
        // Substitution-target pull-ins: GSUB type 1/2/3 outputs are
        // implicitly kept whenever their inputs are kept, and type 8
        // outputs when their context can still match too. The byte-
        // level rewriter in `crate::gsub` honors the same rule when
        // it filters surviving subtable pairs.
        crate::gsub::pull_in_substitution_targets(face, &mut keep, &budget);
        expand_gpos_mark_anchors(face, &mut keep, &budget);
        // VARC-covered glyphs reference component gids the same way
        // glyf composites do; pull them into the kept set so the
        // outline graph stays whole after subset.
        crate::varc::varc_closure_bitset(face, &mut keep, &budget);
        let after = count_kept(&keep);
        if before == after || budget.is_spent() {
            break;
        }
    }

    // Glyph ids fit in u16 because `keep` has `num_glyphs` entries.
    Ok(keep
        .iter()
        .enumerate()
        .filter_map(|(i, &k)| if k { Some(i as u16) } else { None })
        .collect())
}

fn count_kept(keep: &[bool]) -> usize {
    keep.iter().filter(|k| **k).count()
}

/// Marks `gid` as kept when it is inside the font. Returns true when
/// it was not kept before.
fn mark_kept(keep: &mut [bool], gid: u16) -> bool {
    match keep.get_mut(gid as usize) {
        Some(slot) if !*slot => {
            *slot = true;
            true
        }
        _ => false,
    }
}

/// True when `gid` is inside the font and kept.
fn is_kept(keep: &[bool], gid: u16) -> bool {
    keep.get(gid as usize).copied().unwrap_or(false)
}

/// Walks composite glyphs in `glyf`, pulling in component gids.
///
/// Component cycles (a composite that reaches itself) terminate
/// because a gid is only pushed the first time it becomes kept.
fn expand_glyf_composites(
    face: &Face<'_>,
    keep: &mut [bool],
    budget: &WorkBudget,
) -> Result<(), SubsetError> {
    if face.record(tag::GLYF).is_none() || face.record(tag::LOCA).is_none() {
        return Ok(());
    }
    let loca = face.loca()?;
    let glyf_bytes = face.table_bytes(tag::GLYF).map_err(SubsetError::from)?;
    let mut stack: Vec<u16> = keep
        .iter()
        .enumerate()
        .filter_map(|(gid, &k)| if k { Some(gid as u16) } else { None })
        .collect();
    while let Some(g) = stack.pop() {
        if budget.is_spent() {
            break;
        }
        let Some((start, end)) = loca.range(g) else {
            continue;
        };
        if start == end {
            continue;
        }
        let body = glyf_bytes
            .get(start as usize..end as usize)
            .ok_or(SubsetError::Unsupported("glyf offset past end"))?;
        let children = composite_components(body)?;
        // Overlapping loca entries can make many glyphs share one large
        // composite, so charge for every component walked.
        if !budget.spend(1 + children.len()) {
            break;
        }
        for child in children {
            if mark_kept(keep, child) {
                stack.push(child);
            }
        }
    }
    Ok(())
}

/// Walks GSUB lookup type 4 (Ligature Substitution) subtables.
///
/// For each ligature whose output gid is currently kept, all of its
/// input components are pulled in. We tolerate parse errors silently
/// (a malformed GSUB subtable should not stop the closure walk); the
/// affected ligature simply does not contribute to the closure.
fn expand_gsub_ligatures(face: &Face<'_>, keep: &mut [bool], budget: &WorkBudget) {
    let Ok(Some(gsub)) = face.gsub() else {
        return;
    };
    let lookups = gsub.lookup_list();
    for li in 0..lookups.len() {
        let Some(lookup) = lookups.get(li) else {
            continue;
        };
        if !budget.spend(1 + usize::from(lookup.subtable_count())) {
            return;
        }
        let lookup_type = unwrap_extension_type(&lookup, /* gsub */ true);
        if lookup_type != sigilbuzz::tables::gsub::lookup_type::LIGATURE {
            continue;
        }
        for si in 0..lookup.subtable_count() {
            let Some(sub) = subtable_with_extension(&lookup, si, /* gsub */ true) else {
                continue;
            };
            walk_ligature_subtable(sub, keep, budget);
        }
    }
}

/// Walks GPOS lookup types 4, 5, and 6 (Mark-to-Base, Mark-to-Ligature,
/// Mark-to-Mark), pulling in the second coverage (bases, ligatures, or
/// mark2 glyphs) whenever a kept gid is in the mark coverage.
fn expand_gpos_mark_anchors(face: &Face<'_>, keep: &mut [bool], budget: &WorkBudget) {
    let Ok(Some(gpos)) = face.gpos() else {
        return;
    };
    let lookups = gpos.lookup_list();
    for li in 0..lookups.len() {
        let Some(lookup) = lookups.get(li) else {
            continue;
        };
        if !budget.spend(1 + usize::from(lookup.subtable_count())) {
            return;
        }
        let lookup_type = unwrap_extension_type(&lookup, /* gsub */ false);
        if !matches!(
            lookup_type,
            sigilbuzz::tables::gpos::lookup_type::MARK_TO_BASE
                | sigilbuzz::tables::gpos::lookup_type::MARK_TO_LIGATURE
                | sigilbuzz::tables::gpos::lookup_type::MARK_TO_MARK
        ) {
            continue;
        }
        for si in 0..lookup.subtable_count() {
            let Some(sub) = subtable_with_extension(&lookup, si, /* gsub */ false) else {
                continue;
            };
            walk_mark_attachment_subtable(sub, keep, budget);
        }
    }
}

/// If the lookup is an Extension lookup (GSUB type 7 / GPOS type 9),
/// returns the underlying lookup type; otherwise returns the lookup's
/// own type. Falls back to the raw lookup type on any parse failure
/// because the closure is best-effort.
fn unwrap_extension_type(lookup: &sigilbuzz::tables::layout::Lookup<'_>, gsub: bool) -> u16 {
    let extension_type = if gsub {
        sigilbuzz::tables::gsub::lookup_type::EXTENSION
    } else {
        sigilbuzz::tables::gpos::lookup_type::EXTENSION
    };
    if lookup.lookup_type() != extension_type {
        return lookup.lookup_type();
    }
    let Some(sub) = lookup.subtable_bytes(0) else {
        return lookup.lookup_type();
    };
    if sub.len() < 8 {
        return lookup.lookup_type();
    }
    // ExtensionPosFormat1 / ExtensionSubstFormat1: u16 format (=1),
    // u16 extensionLookupType, u32 extensionOffset.
    u16::from_be_bytes([sub[2], sub[3]])
}

/// Returns the subtable bytes, transparently following an Extension
/// indirection when present.
fn subtable_with_extension<'a>(
    lookup: &sigilbuzz::tables::layout::Lookup<'a>,
    index: u16,
    gsub: bool,
) -> Option<&'a [u8]> {
    let sub = lookup.subtable_bytes(index)?;
    let extension_type = if gsub {
        sigilbuzz::tables::gsub::lookup_type::EXTENSION
    } else {
        sigilbuzz::tables::gpos::lookup_type::EXTENSION
    };
    if lookup.lookup_type() != extension_type {
        return Some(sub);
    }
    if sub.len() < 8 {
        return None;
    }
    // u16 format, u16 extensionLookupType, u32 extensionOffset.
    let ext_off = u32::from_be_bytes([sub[4], sub[5], sub[6], sub[7]]) as usize;
    sub.get(ext_off..)
}

/// Enumerates a Coverage table and charges `budget` for it. Returns an
/// empty list once the budget is spent.
fn budgeted_coverage(bytes: &[u8], budget: &WorkBudget) -> Vec<u16> {
    if budget.is_spent() {
        return Vec::new();
    }
    let glyphs = parse_coverage_glyphs(bytes);
    if budget.spend(glyphs.len() + 1) {
        glyphs
    } else {
        Vec::new()
    }
}

fn walk_ligature_subtable(sub: &[u8], keep: &mut [bool], budget: &WorkBudget) {
    // Ligature substitution format 1:
    //   u16 substFormat = 1
    //   Offset16 coverageOffset
    //   u16 ligatureSetCount
    //   Offset16 ligatureSetOffsets[ligatureSetCount]
    let mut r = Reader::new(sub);
    let Ok(format) = r.read_u16() else { return };
    if format != 1 {
        return;
    }
    let Ok(cov_off) = r.read_u16() else { return };
    let Ok(set_count) = r.read_u16() else { return };
    let cov_off = cov_off as usize;
    // Iterate first-component glyphs by inspecting the coverage table.
    // We don't need parsed coverage for the closure: we walk every
    // ligature set and pull in components when the *output* glyph is
    // kept. The first-component glyph is implicit in the coverage.
    let Some(cov_bytes) = sub.get(cov_off..) else {
        return;
    };
    let first_components = budgeted_coverage(cov_bytes, budget);

    for i in 0..set_count as usize {
        let off_off = r.position();
        let Some(set_off) = crate::layout::read_u16(sub, off_off) else {
            return;
        };
        if r.skip(2).is_err() {
            return;
        }
        let Some(set_bytes) = sub.get(usize::from(set_off)..) else {
            continue;
        };
        let first_gid = first_components.get(i).copied();
        if !walk_ligature_set_in(set_bytes, first_gid, keep, budget) {
            return;
        }
    }
}

#[cfg(test)]
fn walk_ligature_set(set_bytes: &[u8], first_gid: Option<u16>, keep: &mut [bool]) {
    walk_ligature_set_in(set_bytes, first_gid, keep, &WorkBudget::new(WORK_LIMIT));
}

/// Walks one LigatureSet. Returns false once `budget` is spent.
fn walk_ligature_set_in(
    set_bytes: &[u8],
    first_gid: Option<u16>,
    keep: &mut [bool],
    budget: &WorkBudget,
) -> bool {
    // LigatureSet:
    //   u16 ligatureCount
    //   Offset16 ligatureOffsets[ligatureCount]
    let Some(lig_count) = crate::layout::read_u16(set_bytes, 0) else {
        return true;
    };
    if !budget.spend(usize::from(lig_count)) {
        return false;
    }
    for i in 0..usize::from(lig_count) {
        let Some(lig_off) = crate::layout::read_u16(set_bytes, 2 + i * 2) else {
            return true;
        };
        let Some(lig_bytes) = set_bytes.get(usize::from(lig_off)..) else {
            continue;
        };
        // Ligature:
        //   u16 ligatureGlyph
        //   u16 componentCount
        //   u16 componentGlyphIDs[componentCount - 1]
        let (Some(lig_glyph), Some(component_count)) = (
            crate::layout::read_u16(lig_bytes, 0),
            crate::layout::read_u16(lig_bytes, 2),
        ) else {
            continue;
        };
        let Some(tail) = component_count.checked_sub(1) else {
            continue;
        };
        let Some(tail_bytes) = lig_bytes.get(4..4 + usize::from(tail) * 2) else {
            continue;
        };
        if !budget.spend(usize::from(tail)) {
            return false;
        }
        let tail_glyphs = || {
            tail_bytes
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
        };

        // Backward direction (output kept -> drag in every component).
        // Necessary so shaping the input string in the subset still
        // fires the kept ligature.
        if is_kept(keep, lig_glyph) {
            if let Some(first) = first_gid {
                mark_kept(keep, first);
            }
            for g in tail_glyphs() {
                mark_kept(keep, g);
            }
        }

        // Forward direction (every component kept -> drag in the result
        // gid). Necessary so `subset(face, &[f, i])` carries the `fi`
        // ligature gid into the output; otherwise the rewritten GSUB
        // type 4 lookup would resolve a result gid that was dropped
        // from the subset and the whole ligature would die during
        // rewrite.
        let first_kept = first_gid.is_some_and(|g| is_kept(keep, g));
        if first_kept && tail_glyphs().all(|g| is_kept(keep, g)) {
            mark_kept(keep, lig_glyph);
        }
    }
    true
}

fn walk_mark_attachment_subtable(sub: &[u8], keep: &mut [bool], budget: &WorkBudget) {
    // MarkBasePos / MarkLigaPos / MarkMarkPos all start with:
    //   u16 posFormat = 1
    //   Offset16 markCoverageOffset
    //   Offset16 baseCoverageOffset (baseCoverage / ligatureCoverage / mark2Coverage)
    let (Some(format), Some(mark_cov_off), Some(base_cov_off)) = (
        crate::layout::read_u16(sub, 0),
        crate::layout::read_u16(sub, 2),
        crate::layout::read_u16(sub, 4),
    ) else {
        return;
    };
    if format != 1 {
        return;
    }
    let Some(mark_cov_bytes) = sub.get(usize::from(mark_cov_off)..) else {
        return;
    };
    let Some(base_cov_bytes) = sub.get(usize::from(base_cov_off)..) else {
        return;
    };
    let mark_glyphs = budgeted_coverage(mark_cov_bytes, budget);
    let base_glyphs = budgeted_coverage(base_cov_bytes, budget);

    // If any mark in the mark coverage is kept, pull in every base in
    // the base coverage. We don't try to resolve which specific anchor
    // pairs are live, being conservative: a kept mark may attach to
    // any of the bases this lookup covers, so all of them survive.
    if mark_glyphs.iter().any(|&g| is_kept(keep, g)) {
        for &g in &base_glyphs {
            mark_kept(keep, g);
        }
    }
}

/// Returns the gids of every component referenced by a composite
/// glyph. Returns an empty vec for simple glyphs and zero-byte
/// (whitespace) glyphs.
fn composite_components(body: &[u8]) -> Result<Vec<u16>, SubsetError> {
    if body.len() < 10 {
        return Ok(Vec::new());
    }
    let mut r = Reader::new(body);
    // numberOfContours: negative => composite.
    let num_contours = r
        .read_i16()
        .map_err(|_| SubsetError::Unsupported("glyf header truncated"))?;
    if num_contours >= 0 {
        return Ok(Vec::new());
    }
    // Skip xMin/yMin/xMax/yMax.
    r.skip(8)
        .map_err(|_| SubsetError::Unsupported("glyf header truncated"))?;

    let mut out = Vec::new();
    // Composite-glyph flag bits we need to walk argument widths.
    const COMP_ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
    const COMP_WE_HAVE_A_SCALE: u16 = 0x0008;
    const COMP_MORE_COMPONENTS: u16 = 0x0020;
    const COMP_WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
    const COMP_WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;

    // Every iteration consumes at least six bytes, so the loop ends
    // once the body runs out.
    loop {
        let flags = r
            .read_u16()
            .map_err(|_| SubsetError::Unsupported("composite flags truncated"))?;
        let component = r
            .read_u16()
            .map_err(|_| SubsetError::Unsupported("composite glyph index truncated"))?;
        out.push(component);

        // Skip the args: two words or two bytes.
        let args_len = if flags & COMP_ARG_1_AND_2_ARE_WORDS != 0 {
            4
        } else {
            2
        };
        r.skip(args_len)
            .map_err(|_| SubsetError::Unsupported("composite args truncated"))?;

        if flags & COMP_WE_HAVE_A_SCALE != 0 {
            r.skip(2)
                .map_err(|_| SubsetError::Unsupported("composite scale truncated"))?;
        } else if flags & COMP_WE_HAVE_AN_X_AND_Y_SCALE != 0 {
            r.skip(4)
                .map_err(|_| SubsetError::Unsupported("composite scales truncated"))?;
        } else if flags & COMP_WE_HAVE_A_TWO_BY_TWO != 0 {
            r.skip(8)
                .map_err(|_| SubsetError::Unsupported("composite 2x2 truncated"))?;
        }

        // Instructions may follow the last component, but the closure
        // pass does not need them.
        if flags & COMP_MORE_COMPONENTS == 0 {
            break;
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_components_empty_for_simple_glyph() {
        // A minimal simple glyph: numContours=1 + bbox + endpts +
        // instructionLength=0 + flags(1) + x(0) + y(0).
        let mut body = Vec::new();
        body.extend_from_slice(&1i16.to_be_bytes()); // numContours
        body.extend_from_slice(&[0u8; 8]); // bbox
        body.extend_from_slice(&0u16.to_be_bytes()); // endPtsOfContours[0] = 0
        body.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
        body.push(0); // flags
        body.push(0); // x
        body.push(0); // y
        let comps = composite_components(&body).unwrap();
        assert!(comps.is_empty());
    }

    #[test]
    fn composite_components_walks_two_children() {
        // numContours=-1, bbox, then two components in XY mode with
        // word args.
        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes()); // numContours
        body.extend_from_slice(&[0u8; 8]); // bbox

        // First component: flags = ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES
        // | MORE_COMPONENTS = 0x0023, gid = 7, args = (10, 20).
        body.extend_from_slice(&0x0023u16.to_be_bytes());
        body.extend_from_slice(&7u16.to_be_bytes());
        body.extend_from_slice(&10i16.to_be_bytes());
        body.extend_from_slice(&20i16.to_be_bytes());

        // Second component: flags = ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES
        // = 0x0003 (no MORE_COMPONENTS), gid = 9, args = (5, 6).
        body.extend_from_slice(&0x0003u16.to_be_bytes());
        body.extend_from_slice(&9u16.to_be_bytes());
        body.extend_from_slice(&5i16.to_be_bytes());
        body.extend_from_slice(&6i16.to_be_bytes());

        let comps = composite_components(&body).unwrap();
        assert_eq!(comps, alloc::vec![7, 9]);
    }

    #[test]
    fn composite_components_skips_two_by_two_block() {
        // numContours=-1, bbox, one component with WE_HAVE_A_TWO_BY_TWO.
        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&[0u8; 8]);

        // flags: ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES | WE_HAVE_A_TWO_BY_TWO
        // (no MORE_COMPONENTS) = 0x0083.
        body.extend_from_slice(&0x0083u16.to_be_bytes());
        body.extend_from_slice(&42u16.to_be_bytes());
        body.extend_from_slice(&[0u8; 4]); // word args
        body.extend_from_slice(&[0u8; 8]); // 2x2

        let comps = composite_components(&body).unwrap();
        assert_eq!(comps, alloc::vec![42]);
    }

    // ===== walk_ligature_set forward / backward pull tests =====

    /// Builds a single-ligature set body. The set is the LigatureSet
    /// table the GSUB type-4 walker consumes. Coverage / outer subtable
    /// framing is the caller's problem.
    fn build_lig_set(ligature_glyph: u16, tail: &[u16]) -> alloc::vec::Vec<u8> {
        let component_count = (tail.len() + 1) as u16;
        let mut out = alloc::vec::Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // ligatureCount
        out.extend_from_slice(&4u16.to_be_bytes()); // ligatureOffsets[0] = 4
        out.extend_from_slice(&ligature_glyph.to_be_bytes());
        out.extend_from_slice(&component_count.to_be_bytes());
        for c in tail {
            out.extend_from_slice(&c.to_be_bytes());
        }
        out
    }

    #[test]
    fn walk_ligature_set_pulls_in_components_when_output_kept() {
        // Backward direction: keep the fi ligature output 100, expect 10
        // and 20 to be pulled in.
        let set = build_lig_set(100, &[20]);
        let mut keep = alloc::vec![false; 256];
        keep[100] = true;
        walk_ligature_set(&set, Some(10), &mut keep);
        assert!(keep[10]);
        assert!(keep[20]);
        assert!(keep[100]);
    }

    #[test]
    fn walk_ligature_set_pulls_in_output_when_all_components_kept() {
        // Forward direction: keep f=10 and i=20, expect fi=100 to come in.
        let set = build_lig_set(100, &[20]);
        let mut keep = alloc::vec![false; 256];
        keep[10] = true;
        keep[20] = true;
        walk_ligature_set(&set, Some(10), &mut keep);
        assert!(
            keep[100],
            "forward direction must pull in the result gid when every component is kept",
        );
    }

    #[test]
    fn walk_ligature_set_does_not_pull_output_when_a_component_drops() {
        // Forward direction must hold off when *any* component is missing.
        // Three-component ligature: 10 + 20 + 30 -> 500. Keep 10 and 30
        // but not 20 -> must NOT pull 500.
        let set = build_lig_set(500, &[20, 30]);
        let mut keep = alloc::vec![false; 1024];
        keep[10] = true;
        keep[30] = true;
        walk_ligature_set(&set, Some(10), &mut keep);
        assert!(
            !keep[500],
            "forward direction must not pull result when a component is missing",
        );
    }

    #[test]
    fn walk_ligature_set_does_not_pull_output_when_first_component_drops() {
        // Forward direction requires the first component (Coverage entry)
        // to be kept too. A kept tail alone can't fire the lookup.
        let set = build_lig_set(100, &[20]);
        let mut keep = alloc::vec![false; 256];
        keep[20] = true; // first component (10) is NOT kept
        walk_ligature_set(&set, Some(10), &mut keep);
        assert!(!keep[100]);
        assert!(!keep[10]);
    }
}
