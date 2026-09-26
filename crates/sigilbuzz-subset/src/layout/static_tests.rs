//! A static subset (`retain_variations: false`) keeps no GDEF
//! ItemVariationStore, so nothing may name one: the rebuilt GPOS must
//! not even copy its VariationIndex tables, and a subset that keeps
//! every glyph must not pass the store or those tables through either.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use super::{decide, Decision, LayoutPlan};
use crate::gpos_var::{walk_gpos_device_slots, VARIATION_INDEX_DELTA_FORMAT};
use crate::SubsetInput;

const RUBIK: &[u8] = include_bytes!("../../../../tests/fixtures/rubik_vf.ttf");

fn plan(face: &Face<'_>, kept: &[u16], retain_variations: bool) -> LayoutPlan {
    let input = SubsetInput {
        gids: kept.to_vec(),
        retain_variations,
        ..SubsetInput::default()
    };
    decide(face, kept, &input).unwrap()
}

fn rewritten(decision: Decision) -> Vec<u8> {
    match decision {
        Decision::Rewrite(bytes) => bytes,
        Decision::Preserve => panic!("expected a rewrite, got Preserve"),
        Decision::Drop => panic!("expected a rewrite, got Drop"),
    }
}

/// Counts the device slots of `gpos` naming a VariationIndex table.
fn variation_indices(gpos: &[u8]) -> usize {
    let mut gpos = gpos.to_vec();
    let mut count = 0;
    walk_gpos_device_slots(&mut gpos, &mut |b, slot| {
        if slot.delta_format(b) == Some(VARIATION_INDEX_DELTA_FORMAT) {
            count += 1;
        }
    });
    count
}

/// Every glyph Rubik maps from Basic Latin and the combining marks,
/// which covers its varied kerning and mark anchors.
fn latin_and_marks(face: &Face<'_>) -> Vec<u16> {
    let cmap = face.cmap().unwrap();
    let mut gids: Vec<u16> = (0x20u32..0x7F)
        .chain(0x300..0x370)
        .filter_map(char::from_u32)
        .filter_map(|c| cmap.glyph_id(c))
        .collect();
    gids.push(0);
    gids.sort_unstable();
    gids.dedup();
    crate::closure::compute_closure(face, &gids).unwrap()
}

#[test]
fn static_rewrite_never_copies_variation_indices() {
    let face = Face::parse_bytes(RUBIK, 0).unwrap();
    let kept = latin_and_marks(&face);
    let varied = rewritten(plan(&face, &kept, true).gpos);
    let fixed = rewritten(plan(&face, &kept, false).gpos);
    assert!(
        variation_indices(&varied) > 0,
        "the fixture varies its GPOS"
    );
    assert_eq!(variation_indices(&fixed), 0);
    // Cleared slots alone would leave the copies behind; left out, they
    // take no space.
    assert!(
        fixed.len() < varied.len(),
        "static {} bytes vs variable {}",
        fixed.len(),
        varied.len()
    );
}

#[test]
fn static_identity_subset_drops_the_store_and_variation_indices() {
    let face = Face::parse_bytes(RUBIK, 0).unwrap();
    let all: Vec<u16> = (0..face.maxp().unwrap().num_glyphs).collect();

    let varied = plan(&face, &all, true);
    assert!(matches!(varied.gpos, Decision::Preserve));
    assert!(matches!(varied.gdef, Decision::Preserve));

    let fixed = plan(&face, &all, false);
    assert!(matches!(fixed.gsub, Decision::Preserve));
    let gpos = rewritten(fixed.gpos);
    assert_eq!(gpos.len(), face.table_bytes(tag::GPOS).unwrap().len());
    assert_eq!(variation_indices(&gpos), 0);
    let gdef = rewritten(fixed.gdef);
    let minor = u16::from_be_bytes([gdef[2], gdef[3]]);
    assert!(minor < 3, "GDEF 1.{minor} still has a store slot");
    // The rest of GDEF survives: Rubik's glyph classes and carets.
    let parsed = sigilbuzz::tables::gdef::Gdef::parse(&gdef).unwrap();
    assert!(parsed.item_variation_store().is_none());
    let source = face.gdef().unwrap().unwrap();
    for gid in 0..face.maxp().unwrap().num_glyphs {
        assert_eq!(
            parsed.glyph_class(gid),
            source.glyph_class(gid),
            "gid {gid}"
        );
    }
}
