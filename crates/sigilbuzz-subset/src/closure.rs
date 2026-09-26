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
//! - **Mark-base anchor partners** in `GPOS` type 4: when a kept mark
//!   has an anchor pointing at a base, the base is pulled in, so the
//!   mark can still attach. The opposite direction is *not* pulled in:
//!   marks attach optionally, and a base subset that drops its marks
//!   simply renders without them.
//!
//! Glyph 0 (`.notdef`) is always retained: every SFNT font has one,
//! every glyph index that fails a cmap lookup falls back to it, and
//! every TrueType-outlined font's first glyf entry is reserved for it.
//!
//! The walker iterates to a fixed point: pulling in a ligature
//! component may expand the kept set, which may itself be the output
//! of another ligature, etc.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::tables::Reader;
use sigilbuzz::Face;

use crate::SubsetError;

/// Computes the closure of `seed` over the source font's reference
/// graph (composites, ligatures, mark anchors). The returned vec is
/// sorted ascending and contains gid 0 even when `seed` does not.
pub fn compute_closure(face: &Face<'_>, seed: &[u16]) -> Result<Vec<u16>, SubsetError> {
    let num_glyphs = face.maxp()?.num_glyphs;

    // Bitset for membership: a Vec<bool> sized to num_glyphs is
    // O(numGlyphs) in memory but lookups are O(1) and writes are
    // deterministic: no HashMap iteration order to worry about.
    let mut keep = alloc::vec![false; num_glyphs as usize];
    keep[0] = true;
    for &g in seed {
        if (g as usize) < keep.len() {
            keep[g as usize] = true;
        }
    }

    // Iterate to a fixed point. Each pass pulls in references from one
    // table; subsequent passes pick up second-order pull-ins (e.g. a
    // ligature whose output was itself dragged in by a composite).
    loop {
        let before = count_kept(&keep);
        expand_glyf_composites(face, &mut keep)?;
        expand_gsub_ligatures(face, &mut keep)?;
        // Substitution-target pull-ins: GSUB type 1/2/3 outputs are
        // implicitly kept whenever their inputs are kept, and type 8
        // outputs when their context can still match too. The byte-
        // level rewriter in `crate::gsub` honors the same rule when
        // it filters surviving subtable pairs.
        crate::gsub::pull_in_substitution_targets(face, &mut keep);
        expand_gpos_mark_anchors(face, &mut keep)?;
        // VARC-covered glyphs reference component gids the same way
        // glyf composites do; pull them into the kept set so the
        // outline graph stays whole after subset.
        crate::varc::varc_closure_bitset(face, &mut keep);
        let after = count_kept(&keep);
        if before == after {
            break;
        }
    }

    let mut out: Vec<u16> = keep
        .iter()
        .enumerate()
        .filter_map(|(i, &k)| if k { Some(i as u16) } else { None })
        .collect();
    out.sort_unstable();
    Ok(out)
}

fn count_kept(keep: &[bool]) -> usize {
    keep.iter().filter(|k| **k).count()
}

/// Walks composite glyphs in `glyf`, pulling in component gids.
fn expand_glyf_composites(face: &Face<'_>, keep: &mut [bool]) -> Result<(), SubsetError> {
    if face.record(tag::GLYF).is_none() || face.record(tag::LOCA).is_none() {
        return Ok(());
    }
    let loca = face.loca()?;
    let glyf_bytes = face.table_bytes(tag::GLYF).map_err(SubsetError::from)?;
    let mut stack: Vec<u16> = Vec::new();
    for (gid, &k) in keep.iter().enumerate() {
        if k {
            stack.push(gid as u16);
        }
    }
    while let Some(g) = stack.pop() {
        let Some((start, end)) = loca.range(g) else {
            continue;
        };
        if start == end {
            continue;
        }
        let body = glyf_bytes
            .get(start as usize..end as usize)
            .ok_or(SubsetError::Unsupported("glyf offset past end"))?;
        for child in composite_components(body)? {
            if (child as usize) < keep.len() && !keep[child as usize] {
                keep[child as usize] = true;
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
fn expand_gsub_ligatures(face: &Face<'_>, keep: &mut [bool]) -> Result<(), SubsetError> {
    let Ok(Some(gsub)) = face.gsub() else {
        return Ok(());
    };
    let lookups = gsub.lookup_list();
    for li in 0..lookups.len() {
        let Some(lookup) = lookups.get(li) else {
            continue;
        };
        let lookup_type = unwrap_extension_type(&lookup, /* gsub */ true);
        if lookup_type != sigilbuzz::tables::gsub::lookup_type::LIGATURE {
            continue;
        }
        for si in 0..lookup.subtable_count() {
            let Some(sub) = subtable_with_extension(&lookup, si, /* gsub */ true) else {
                continue;
            };
            walk_ligature_subtable(sub, keep);
        }
    }
    Ok(())
}

/// Walks GPOS lookup type 4 (Mark-to-Base) subtables, pulling in the
/// base coverage when a kept gid is in the mark coverage. Symmetric
/// types 5/6 (mark-to-liga, mark-to-mark) are left alone. The same
/// "marks attach optionally" rule means a kept mark dragging in the
/// host glyph is sufficient; types 5/6 follow once mark coverage
/// pulls them in via the base coverage on type 4.
fn expand_gpos_mark_anchors(face: &Face<'_>, keep: &mut [bool]) -> Result<(), SubsetError> {
    let Ok(Some(gpos)) = face.gpos() else {
        return Ok(());
    };
    let lookups = gpos.lookup_list();
    for li in 0..lookups.len() {
        let Some(lookup) = lookups.get(li) else {
            continue;
        };
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
            walk_mark_attachment_subtable(sub, keep);
        }
    }
    Ok(())
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

fn walk_ligature_subtable(sub: &[u8], keep: &mut [bool]) {
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
    let first_components = parse_coverage_glyphs(cov_bytes);

    for i in 0..set_count as usize {
        let off_off = r.position();
        if off_off + 2 > sub.len() {
            return;
        }
        let set_off = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        if r.skip(2).is_err() {
            return;
        }
        let Some(set_bytes) = sub.get(set_off..) else {
            continue;
        };
        let first_gid = first_components.get(i).copied();
        walk_ligature_set(set_bytes, first_gid, keep);
    }
}

fn walk_ligature_set(set_bytes: &[u8], first_gid: Option<u16>, keep: &mut [bool]) {
    // LigatureSet:
    //   u16 ligatureCount
    //   Offset16 ligatureOffsets[ligatureCount]
    let mut r = Reader::new(set_bytes);
    let Ok(lig_count) = r.read_u16() else { return };
    for i in 0..lig_count {
        let off_off = 2 + i as usize * 2;
        if off_off + 2 > set_bytes.len() {
            return;
        }
        let lig_off = u16::from_be_bytes([set_bytes[off_off], set_bytes[off_off + 1]]) as usize;
        let Some(lig_bytes) = set_bytes.get(lig_off..) else {
            continue;
        };
        // Ligature:
        //   u16 ligatureGlyph
        //   u16 componentCount
        //   u16 componentGlyphIDs[componentCount - 1]
        if lig_bytes.len() < 4 {
            continue;
        }
        let lig_glyph = u16::from_be_bytes([lig_bytes[0], lig_bytes[1]]);
        let component_count = u16::from_be_bytes([lig_bytes[2], lig_bytes[3]]);
        if component_count == 0 {
            continue;
        }
        let tail = (component_count - 1) as usize;
        let needed = 4 + tail * 2;
        if lig_bytes.len() < needed {
            continue;
        }

        // Backward direction (output kept -> drag in every component).
        // Necessary so shaping the input string in the subset still
        // fires the kept ligature.
        if (lig_glyph as usize) < keep.len() && keep[lig_glyph as usize] {
            if let Some(first) = first_gid {
                if (first as usize) < keep.len() {
                    keep[first as usize] = true;
                }
            }
            for j in 0..tail {
                let off = 4 + j * 2;
                let g = u16::from_be_bytes([lig_bytes[off], lig_bytes[off + 1]]);
                if (g as usize) < keep.len() {
                    keep[g as usize] = true;
                }
            }
        }

        // Forward direction (every component kept -> drag in the result
        // gid). Necessary so `subset(face, &[f, i])` carries the `fi`
        // ligature gid into the output; otherwise the rewritten GSUB
        // type 4 lookup would resolve a result gid that was dropped
        // from the subset and the whole ligature would die during
        // rewrite.
        let first_kept = first_gid
            .map(|g| (g as usize) < keep.len() && keep[g as usize])
            .unwrap_or(false);
        if first_kept {
            let mut all_components_kept = true;
            for j in 0..tail {
                let off = 4 + j * 2;
                let g = u16::from_be_bytes([lig_bytes[off], lig_bytes[off + 1]]);
                if (g as usize) >= keep.len() || !keep[g as usize] {
                    all_components_kept = false;
                    break;
                }
            }
            if all_components_kept && (lig_glyph as usize) < keep.len() {
                keep[lig_glyph as usize] = true;
            }
        }
    }
}

fn walk_mark_attachment_subtable(sub: &[u8], keep: &mut [bool]) {
    // MarkBasePos / MarkLigaPos / MarkMarkPos all start with:
    //   u16 posFormat = 1
    //   Offset16 markCoverageOffset
    //   Offset16 baseCoverageOffset (baseCoverage / ligatureCoverage / mark2Coverage)
    if sub.len() < 6 {
        return;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    if format != 1 {
        return;
    }
    let mark_cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let base_cov_off = u16::from_be_bytes([sub[4], sub[5]]) as usize;

    let Some(mark_cov_bytes) = sub.get(mark_cov_off..) else {
        return;
    };
    let Some(base_cov_bytes) = sub.get(base_cov_off..) else {
        return;
    };
    let mark_glyphs = parse_coverage_glyphs(mark_cov_bytes);
    let base_glyphs = parse_coverage_glyphs(base_cov_bytes);

    // If any mark in the mark coverage is kept, pull in every base in
    // the base coverage. We don't try to resolve which specific anchor
    // pairs are live, being conservative: a kept mark may attach to
    // any of the bases this lookup covers, so all of them survive.
    let any_mark_kept = mark_glyphs
        .iter()
        .any(|&g| (g as usize) < keep.len() && keep[g as usize]);
    if any_mark_kept {
        for &g in &base_glyphs {
            if (g as usize) < keep.len() {
                keep[g as usize] = true;
            }
        }
    }
}

/// Best-effort enumeration of the glyphs covered by a Coverage table.
/// Returns an empty vec on any parse failure.
fn parse_coverage_glyphs(bytes: &[u8]) -> Vec<u16> {
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
    const COMP_ARGS_ARE_XY_VALUES: u16 = 0x0002;
    const COMP_WE_HAVE_A_SCALE: u16 = 0x0008;
    const COMP_MORE_COMPONENTS: u16 = 0x0020;
    const COMP_WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
    const COMP_WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
    const COMP_WE_HAVE_INSTRUCTIONS: u16 = 0x0100;

    loop {
        let flags = r
            .read_u16()
            .map_err(|_| SubsetError::Unsupported("composite flags truncated"))?;
        let component = r
            .read_u16()
            .map_err(|_| SubsetError::Unsupported("composite glyph index truncated"))?;
        out.push(component);

        // Skip the args and any 2x2 transform.
        if flags & COMP_ARG_1_AND_2_ARE_WORDS != 0 {
            r.skip(4)
                .map_err(|_| SubsetError::Unsupported("composite args truncated"))?;
        } else if flags & COMP_ARGS_ARE_XY_VALUES != 0 {
            r.skip(2)
                .map_err(|_| SubsetError::Unsupported("composite args truncated"))?;
        } else {
            r.skip(2)
                .map_err(|_| SubsetError::Unsupported("composite args truncated"))?;
        }

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

        if flags & COMP_MORE_COMPONENTS == 0 {
            // Last component: instructions (if present) follow but
            // we don't care about them in the closure pass.
            let _ = flags & COMP_WE_HAVE_INSTRUCTIONS;
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
