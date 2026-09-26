//! Instancing parity for GPOS mark and cursive anchors.
//!
//! Rubik VF varies most of its mark anchors through AnchorFormat3
//! `VariationIndex` tables. The instancer folds those deltas into the
//! static anchor coordinates. This test instances Rubik at several
//! weights and checks that rustybuzz positions marks in the static
//! output exactly where it positions them in the variable source at
//! the same weight (rustybuzz applies anchor variations itself).
//!
//! The corpus uses base + combining-mark sequences with no precomposed
//! form, so the normalizer cannot compose them away and every mark
//! goes through mark-to-base or mark-to-mark attachment.

use rustybuzz::ttf_parser::Tag;
use rustybuzz::{Face as RbFace, GlyphBuffer, UnicodeBuffer, Variation};
use sigilbuzz::Face;
use sigilbuzz_subset::{instance, InstanceInput};

const RUBIK: &[u8] = include_bytes!("../../../tests/fixtures/rubik_vf.ttf");

/// Base + mark sequences without a precomposed code point. The last
/// entries stack two marks to exercise mark-to-mark.
const CORPUS: &[&str] = &[
    "q\u{301}",
    "x\u{303}",
    "b\u{308}",
    "f\u{301}",
    "q\u{323}",
    "x\u{327}",
    "v\u{300}\u{301}",
    "q\u{308}\u{304}",
    "Q\u{301}",
    "X\u{302}\u{303}",
];

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

/// `(glyph, x_offset, y_offset)` for every glyph with a non-zero
/// offset, which on this corpus is every attached mark.
fn mark_offsets(out: &GlyphBuffer) -> Vec<(u32, i32, i32)> {
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .filter(|(_, p)| p.x_offset != 0 || p.y_offset != 0)
        .map(|(i, p)| (i.glyph_id, p.x_offset, p.y_offset))
        .collect()
}

fn instance_at(wght: f32) -> Vec<u8> {
    let face = Face::parse_bytes(RUBIK, 0).unwrap();
    let coords = face.fvar().unwrap().unwrap().normalize_coords(&[wght]);
    let input = InstanceInput {
        coords,
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    instance(&face, &input).expect("instance succeeds").bytes
}

#[test]
fn instanced_mark_anchors_match_rustybuzz_on_the_variable_source() {
    for wght in [400.0f32, 650.0, 900.0] {
        let baked = instance_at(wght);
        for text in CORPUS {
            let source = shape(RUBIK, Some(wght), text);
            let static_out = shape(&baked, None, text);
            let src_glyphs: Vec<u32> = source.glyph_infos().iter().map(|i| i.glyph_id).collect();
            let out_glyphs: Vec<u32> = static_out
                .glyph_infos()
                .iter()
                .map(|i| i.glyph_id)
                .collect();
            assert_eq!(src_glyphs, out_glyphs, "glyphs at wght={wght} {text:?}");
            for (s, o) in source
                .glyph_positions()
                .iter()
                .zip(static_out.glyph_positions())
            {
                // One unit of slack: rustybuzz evaluates the variation
                // store in fixed point, the instancer in f32.
                assert!(
                    (s.x_offset - o.x_offset).abs() <= 1
                        && (s.y_offset - o.y_offset).abs() <= 1
                        && (s.x_advance - o.x_advance).abs() <= 1,
                    "wght={wght} {text:?}: source {:?} vs instance {:?}",
                    mark_offsets(&source),
                    mark_offsets(&static_out),
                );
            }
        }
    }
}

/// Guards against a vacuous pass: the corpus must actually move marks
/// between the default and the heaviest weight, or the parity check
/// above would hold even with the anchor variations ignored.
#[test]
fn corpus_marks_move_across_the_weight_axis() {
    let moved = CORPUS.iter().any(|text| {
        let light = shape(RUBIK, None, text);
        let heavy = shape(RUBIK, Some(900.0), text);
        mark_offsets(&light)
            .iter()
            .zip(mark_offsets(&heavy).iter())
            .any(|(a, b)| (a.2 - b.2).abs() > 5)
    });
    assert!(
        moved,
        "expected at least one mark y offset to vary with weight"
    );
}
