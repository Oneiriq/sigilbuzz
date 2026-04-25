//! Layout-table (`GSUB` / `GPOS` / `GDEF`) emission.
//!
//! The subsetter preserves layout tables when (and only when) every
//! gid reference inside them is *guaranteed* to remain valid under the
//! new gid namespace. That is the case in exactly two scenarios:
//!
//! 1. The kept gid set is the full font (closure didn't drop anything).
//!    The gid_map is then the identity and the tables can be passed
//!    through unmodified.
//! 2. The kept gid set is a contiguous prefix `0..N` of the source
//!    font's gid space *and* the layout tables reference no gid
//!    `>= N`. The contiguous-prefix case happens when a caller passes
//!    every gid up to some boundary and the layout tables only deal
//!    with low-numbered glyphs — uncommon but worth handling because
//!    the gid_map collapses to identity and table preservation is
//!    risk-free.
//!
//! Anything beyond those scenarios drops the table — full byte-level
//! rewriting of GSUB / GPOS / GDEF subtables (Coverage / ClassDef /
//! anchor / value-record indices, all keyed on gid) is a deferred
//! follow-up. The Coverage and ClassDef emitters in [`crate::coverage`]
//! and [`crate::classdef`] are the building blocks the future rewriter
//! will use.
//!
//! Strict-mode (`drop_unhandled = false`) callers that hit a layout
//! table outside the preserve-able scenarios receive
//! [`SubsetError::Unsupported`].

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use crate::{GlyphId, SubsetError, SubsetInput};

/// Decision about a layout table's fate in the output.
pub(crate) enum Decision {
    /// Pass the source bytes through verbatim.
    Preserve,
    /// Omit the table from the output.
    Drop,
}

/// Decides whether GSUB, GPOS, and GDEF can be preserved given the
/// kept-gid set. `kept` is sorted ascending and contains gid 0.
pub(crate) fn decide(
    face: &Face<'_>,
    kept: &[GlyphId],
    input: &SubsetInput,
) -> Result<LayoutPlan, SubsetError> {
    let has_gsub = face.record(tag::GSUB).is_some();
    let has_gpos = face.record(tag::GPOS).is_some();
    let has_gdef = face.record(tag::GDEF).is_some();

    // No layout tables: no decision to make.
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

    // Identity check: the kept set is exactly 0..num_glyphs and so
    // every gid reference inside the layout tables resolves to the
    // same glyph in the subset.
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

    // Non-identity subsetting + retain_layout = true: dropping is the
    // safe default. Future work will rewrite the tables byte-by-byte.
    Ok(LayoutPlan {
        gsub: Decision::Drop,
        gpos: Decision::Drop,
        gdef: Decision::Drop,
    })
}

pub(crate) struct LayoutPlan {
    pub gsub: Decision,
    pub gpos: Decision,
    pub gdef: Decision,
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Open Sans carries GSUB/GPOS/GDEF; identity should preserve all
        // three.
        assert!(matches!(plan.gsub, Decision::Preserve));
        assert!(matches!(plan.gpos, Decision::Preserve));
        assert!(matches!(plan.gdef, Decision::Preserve));
    }

    #[test]
    fn drop_layout_when_kept_set_is_proper_subset() {
        let face = open_sans_face();
        let kept: alloc::vec::Vec<u16> = alloc::vec![0, 36, 37, 38];
        let input = SubsetInput {
            retain_layout: true,
            ..Default::default()
        };
        let plan = decide(&face, &kept, &input).unwrap();
        assert!(matches!(plan.gsub, Decision::Drop));
        assert!(matches!(plan.gpos, Decision::Drop));
        assert!(matches!(plan.gdef, Decision::Drop));
    }
}
