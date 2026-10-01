//! Feature masks: a feature some glyphs only carry applies where the
//! mask turns it on, at the cursor and at every input glyph a rule
//! matches, and the mask moves with its glyph.

use super::lookups::{
    build_cov_fmt1, build_shapeable_font_with_gsub, build_single_fmt2_subst, DFLT_TEST,
};
use super::*;
use crate::buffer::Glyph;
use crate::tables::layout::Joiners;

/// Ligature substitution format 1: `first` followed by `rest` becomes
/// `lig`.
fn ligature_subtable(first: u16, rest: &[u16], lig: u16) -> Vec<u8> {
    let mut o = Vec::new();
    o.extend_from_slice(&1u16.to_be_bytes()); // format
    o.extend_from_slice(&0u16.to_be_bytes()); // coverage slot
    o.extend_from_slice(&1u16.to_be_bytes()); // set count
    o.extend_from_slice(&8u16.to_be_bytes()); // set offset
                                              // LigatureSet at 8: one ligature at offset 4 from the set.
    o.extend_from_slice(&1u16.to_be_bytes());
    o.extend_from_slice(&4u16.to_be_bytes());
    o.extend_from_slice(&lig.to_be_bytes());
    o.extend_from_slice(&(rest.len() as u16 + 1).to_be_bytes());
    for g in rest {
        o.extend_from_slice(&g.to_be_bytes());
    }
    let cov = o.len();
    o.extend_from_slice(&build_cov_fmt1(&[first]));
    o[2..4].copy_from_slice(&(cov as u16).to_be_bytes());
    o
}

/// Context substitution format 3 over the coverages `input`, running
/// `records` of (sequence index, lookup index).
fn context_subtable(input: &[&[u16]], records: &[(u16, u16)]) -> Vec<u8> {
    let mut o = Vec::new();
    o.extend_from_slice(&3u16.to_be_bytes());
    o.extend_from_slice(&(input.len() as u16).to_be_bytes());
    o.extend_from_slice(&(records.len() as u16).to_be_bytes());
    let slots = o.len();
    o.extend_from_slice(&alloc::vec![0u8; 2 * input.len()]);
    for (seq, lookup) in records {
        o.extend_from_slice(&seq.to_be_bytes());
        o.extend_from_slice(&lookup.to_be_bytes());
    }
    for (j, glyphs) in input.iter().enumerate() {
        let off = o.len();
        o.extend_from_slice(&build_cov_fmt1(glyphs));
        o[slots + 2 * j..slots + 2 * j + 2].copy_from_slice(&(off as u16).to_be_bytes());
    }
    o
}

/// Applies feature `test` of `font` to glyphs `ids`, on where `mask`
/// is true, and returns the glyph ids.
fn masked(font: &[u8], ids: &[u32], mask: &[bool]) -> Vec<u32> {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).unwrap();
    let gsub = face.gsub().unwrap().expect("GSUB");
    let mut glyphs: Vec<Glyph> = ids
        .iter()
        .enumerate()
        .map(|(i, &id)| Glyph::new(id, i as u32))
        .collect();
    apply_gsub_feature_masked(
        &gsub,
        &mut glyphs,
        None,
        *b"test",
        DFLT_TEST,
        mask,
        Joiners::AUTO,
    );
    glyphs.iter().map(|g| g.glyph_id).collect()
}

// Expectations: HarfBuzz 14.5.0 (uharfbuzz 0.56.2) shaping the same
// font bytes with feature `test` on a cluster range, which gives the
// glyphs in the range the feature's mask bit.

#[test]
fn a_ligature_needs_the_feature_on_at_every_component() {
    // `matcher_t::may_match`: an input glyph outside the lookup mask
    // does not match.
    let font = build_shapeable_font_with_gsub(&[(4, ligature_subtable(1, &[2], 3))], &[0]);
    assert_eq!(masked(&font, &[1, 2], &[true, true]), [3]);
    assert_eq!(masked(&font, &[1, 2], &[true, false]), [1, 2]);
    assert_eq!(masked(&font, &[1, 2], &[false, true]), [1, 2]);
}

#[test]
fn a_context_rule_needs_the_feature_on_at_every_input_glyph() {
    // A contextual lookup of a masked feature starts only where the
    // feature is on, and its input walk checks the mask like any
    // other. It used to run over the whole run.
    let font = build_shapeable_font_with_gsub(
        &[
            (1, build_single_fmt2_subst(&[1], &[3])),
            (5, context_subtable(&[&[1], &[2]], &[(0, 0)])),
        ],
        &[1],
    );
    assert_eq!(masked(&font, &[1, 2], &[true, true]), [3, 2]);
    assert_eq!(masked(&font, &[1, 2], &[true, false]), [1, 2]);
    assert_eq!(masked(&font, &[1, 2], &[false, true]), [1, 2]);
}

#[test]
fn the_mask_moves_with_its_glyph_through_the_features_lookups() {
    // Lookup 0 ligates the first two glyphs. Lookup 1 then looks at
    // the third, which the feature is off at, although its position
    // moved to where an "on" glyph used to be.
    let font = build_shapeable_font_with_gsub(
        &[
            (4, ligature_subtable(1, &[2], 3)),
            (1, build_single_fmt2_subst(&[1], &[2])),
        ],
        &[0, 1],
    );
    assert_eq!(masked(&font, &[1, 2, 1], &[true, true, false]), [3, 1]);
    assert_eq!(masked(&font, &[1, 2, 1], &[true, true, true]), [3, 2]);
}
