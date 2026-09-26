//! Variable layout survives a subset of Rubik VF.
//!
//! Rubik's mark anchors vary with weight through AnchorFormat3
//! `VariationIndex` tables that resolve in the GDEF
//! `ItemVariationStore`. A subset that keeps variations has to carry
//! both: the device tables inside the rebuilt GPOS and the store inside
//! the rebuilt GDEF. These tests shape the subset with rustybuzz (which
//! applies anchor variations) at several weights and compare against
//! the source, then instance the subset and check the static result
//! still matches, which runs the instancer's GDEF store prune and
//! partial bake over the rewritten GDEF.

use rustybuzz::ttf_parser::Tag;
use rustybuzz::{Face as RbFace, GlyphBuffer, UnicodeBuffer, Variation};
use sigilbuzz::Face;
use sigilbuzz_subset::{instance, subset, AxisPin, InstanceInput, SubsetInput, SubsetOutput};

const RUBIK: &[u8] = include_bytes!("../../../tests/fixtures/rubik_vf.ttf");

/// Base + combining-mark sequences with no precomposed form, so every
/// mark goes through mark-to-base or mark-to-mark attachment.
const CORPUS: &[&str] = &[
    "q\u{301}",
    "x\u{303}",
    "b\u{308}",
    "q\u{323}",
    "v\u{300}\u{301}",
    "q\u{308}\u{304}",
    "Q\u{301}",
    "X\u{302}\u{303}",
];

const WEIGHTS: [f32; 3] = [400.0, 650.0, 900.0];

fn shape(bytes: &[u8], wght: Option<f32>, text: &str) -> GlyphBuffer {
    let mut face = RbFace::from_slice(bytes, 0).expect("rustybuzz parses");
    if let Some(value) = wght {
        face.set_variations(&[Variation {
            tag: Tag::from_bytes(b"wght"),
            value,
        }]);
    }
    let mut buf = UnicodeBuffer::new();
    buf.push_str(text);
    rustybuzz::shape(&face, &[], buf)
}

/// `(glyph, x_advance, x_offset, y_offset)` per shaped glyph.
fn run(out: &GlyphBuffer) -> Vec<(u32, i32, i32, i32)> {
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| (i.glyph_id, p.x_advance, p.x_offset, p.y_offset))
        .collect()
}

/// Seeds the subset with every glyph the corpus shapes to at any of
/// the test weights, so the kept set covers what the corpus needs.
fn rubik_subset(retain_variations: bool) -> SubsetOutput {
    let face = Face::parse_bytes(RUBIK, 0).unwrap();
    let mut gids = Vec::new();
    for text in CORPUS {
        for wght in [None, Some(400.0), Some(900.0)] {
            gids.extend(run(&shape(RUBIK, wght, text)).iter().map(|g| g.0 as u16));
        }
    }
    let input = SubsetInput {
        gids,
        retain_variations,
        ..Default::default()
    };
    subset(&face, &input).expect("subset succeeds")
}

fn new_gid(out: &SubsetOutput, old: u32) -> u32 {
    out.gid_map
        .iter()
        .find_map(|&(o, n)| (u32::from(o) == old).then_some(u32::from(n)))
        .expect("shaped glyph kept")
}

#[test]
fn subset_marks_vary_with_weight_like_the_source() {
    let out = rubik_subset(true);
    for wght in WEIGHTS {
        for text in CORPUS {
            let expected: Vec<_> = run(&shape(RUBIK, Some(wght), text))
                .into_iter()
                .map(|(g, adv, x, y)| (new_gid(&out, g), adv, x, y))
                .collect();
            let got = run(&shape(&out.bytes, Some(wght), text));
            assert_eq!(got, expected, "wght={wght} {text:?}");
        }
    }
}

/// Instancing the subset folds the anchor deltas it carried; the
/// static font then positions marks where the variable source does.
#[test]
fn instancing_a_subset_matches_the_variable_source() {
    let out = rubik_subset(true);
    let subset_face = Face::parse_bytes(&out.bytes, 0).unwrap();
    for wght in WEIGHTS {
        let coords = subset_face
            .fvar()
            .unwrap()
            .unwrap()
            .normalize_coords(&[wght]);
        let input = InstanceInput {
            coords,
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let baked = instance(&subset_face, &input).expect("instance succeeds");
        let baked_face = Face::parse_bytes(&baked.bytes, 0).unwrap();
        let gdef = baked_face.gdef().unwrap().expect("GDEF survives");
        assert!(gdef.item_variation_store().is_none(), "store pruned");
        assert!(gdef.mark_filtering_set(0).is_some(), "mark sets kept");
        for text in CORPUS {
            let source = run(&shape(RUBIK, Some(wght), text));
            let got = run(&shape(&baked.bytes, None, text));
            assert_eq!(got.len(), source.len(), "wght={wght} {text:?}");
            for (s, g) in source.iter().zip(&got) {
                // One unit of slack: rustybuzz evaluates the store in
                // fixed point, the instancer in f32.
                assert_eq!(new_gid(&out, s.0), g.0, "wght={wght} {text:?}");
                for (a, b) in [(s.1, g.1), (s.2, g.2), (s.3, g.3)] {
                    assert!(
                        (a - b).abs() <= 1,
                        "wght={wght} {text:?}: {source:?} vs {got:?}"
                    );
                }
            }
        }
    }
}

/// Keeping the weight axis runs the instancer's partial GDEF bake over
/// the rewritten table: the store must still parse, and the subtables
/// before it must ride through intact.
#[test]
fn partially_instancing_a_subset_keeps_its_gdef() {
    let out = rubik_subset(true);
    let subset_face = Face::parse_bytes(&out.bytes, 0).unwrap();
    let input = InstanceInput {
        coords: vec![0.0],
        drop_var_tables: false,
        axis_pins: vec![AxisPin::Keep],
    };
    let partial = instance(&subset_face, &input).expect("partial instance succeeds");
    let partial_face = Face::parse_bytes(&partial.bytes, 0).unwrap();
    let gdef = partial_face.gdef().unwrap().expect("GDEF survives");
    assert!(gdef.item_variation_store().is_some(), "store kept");
    let source_gdef = subset_face.gdef().unwrap().unwrap();
    for set in 0..2 {
        let (a, b) = (
            source_gdef.mark_filtering_set(set).unwrap(),
            gdef.mark_filtering_set(set).unwrap(),
        );
        for &(_, g) in &out.gid_map {
            assert_eq!(a.contains(g), b.contains(g), "set {set}, glyph {g}");
        }
    }
    for text in CORPUS {
        let expected = run(&shape(&out.bytes, Some(900.0), text));
        assert_eq!(run(&shape(&partial.bytes, Some(900.0), text)), expected);
    }
}

/// Keeping nearly the whole font (everything but Hebrew) makes the
/// rebuilt GPOS far larger than 64 KiB once its anchors carry their
/// device tables, which forces the Extension layout. Marks must still
/// land where the source puts them.
#[test]
fn large_variable_subset_shapes_like_the_source() {
    let face = Face::parse_bytes(RUBIK, 0).unwrap();
    let cmap = face.cmap().unwrap();
    let hebrew: Vec<u16> = ('\u{590}'..='\u{5ff}')
        .chain('\u{fb1d}'..='\u{fb4f}')
        .filter_map(|c| cmap.glyph_id(c))
        .collect();
    let num_glyphs = face.maxp().unwrap().num_glyphs;
    let input = SubsetInput {
        gids: (0..num_glyphs).filter(|g| !hebrew.contains(g)).collect(),
        ..Default::default()
    };
    let out = subset(&face, &input).expect("subset succeeds");
    assert!(out.gid_map.len() < usize::from(num_glyphs), "renumbered");
    let subset_face = Face::parse_bytes(&out.bytes, 0).unwrap();
    let gpos = subset_face.gpos().unwrap().unwrap();
    let lookups = gpos.lookup_list();
    let types: Vec<u16> = (0..lookups.len())
        .map(|i| lookups.get(i).unwrap().lookup_type())
        .collect();
    assert!(types.iter().all(|&t| t == 9), "Extension layout: {types:?}");
    assert_eq!(types.len(), 13, "every lookup survives");
    for wght in WEIGHTS {
        for text in CORPUS
            .iter()
            .chain(&["\u{433}\u{301}", "\u{436}\u{323}\u{302}"])
        {
            let expected: Vec<_> = run(&shape(RUBIK, Some(wght), text))
                .into_iter()
                .map(|(g, adv, x, y)| (new_gid(&out, g), adv, x, y))
                .collect();
            let got = run(&shape(&out.bytes, Some(wght), text));
            assert_eq!(got, expected, "wght={wght} {text:?}");
        }
    }
}

/// A static subset keeps no store and no VariationIndex, so it shapes
/// like the source at its default weight whatever weight is asked for.
#[test]
fn static_subset_shapes_like_the_default_instance() {
    let out = rubik_subset(false);
    let subset_face = Face::parse_bytes(&out.bytes, 0).unwrap();
    assert!(subset_face
        .gdef()
        .unwrap()
        .unwrap()
        .item_variation_store()
        .is_none());
    for text in CORPUS {
        let expected: Vec<_> = run(&shape(RUBIK, None, text))
            .into_iter()
            .map(|(g, adv, x, y)| (new_gid(&out, g), adv, x, y))
            .collect();
        assert_eq!(run(&shape(&out.bytes, None, text)), expected, "{text:?}");
    }
}
