//! Integration test for the OpenType `MATH` table.
//!
//! Drives [`Face::math`] and the five MATH subtable parsers against
//! the hand-crafted fixture in `tests/fixtures/math_synthetic.ttf`
//! (built by `tests/tools/build_math_fixture.py`).
//!
//! Real math fonts (STIX 2 Math: ~500 KB, Latin Modern Math: ~700 KB,
//! Asana Math: ~150 KB) are too heavy to vendor for a single
//! integration test. The synthetic fixture maps the 'f' codepoint
//! (a stand-in for the SMP math italic 'f') to gid 1 with an italic
//! correction, and U+222B (∫) to gid 2 with a vertical glyph
//! construction (two progressive variants + a 3-part assembly).

use sigilbuzz::tables::KernSide;
use sigilbuzz::{Blob, Face};

const FIXTURE: &str = "tests/fixtures/math_synthetic.ttf";

#[test]
fn face_math_returns_some_for_math_font() {
    let bytes = std::fs::read(FIXTURE).unwrap();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(face.math().unwrap().is_some());
}

#[test]
fn math_constants_returns_sane_non_zero_values() {
    let bytes = std::fs::read(FIXTURE).unwrap();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let math = face.math().unwrap().expect("MATH present");
    let c = math.constants().unwrap().expect("constants present");

    // Header scalars.
    assert_eq!(c.script_percent_scale_down(), 80);
    assert_eq!(c.script_script_percent_scale_down(), 60);
    assert_eq!(c.delimited_sub_formula_min_height(), 1500);
    assert_eq!(c.display_operator_min_height(), 1800);

    // Spot-check a handful of MathValueRecords.
    assert_eq!(c.axis_height().value, 250);
    assert_eq!(c.fraction_rule_thickness().value, 50);
    assert_eq!(c.fraction_numerator_display_style_shift_up().value, 700);
    assert_eq!(c.radical_kern_after_degree().value, -50);

    // Trailing scalar after the value-record block.
    assert_eq!(c.radical_degree_bottom_raise_percent(), 65);
}

#[test]
fn math_glyph_info_finds_italic_correction_for_known_glyph() {
    let bytes = std::fs::read(FIXTURE).unwrap();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let math = face.math().unwrap().expect("MATH present");
    let gi = math.glyph_info().unwrap().expect("glyph-info present");

    // gid 1 (math italic 'f') has italic correction = 60 and top
    // accent attachment = 400.
    let ic = gi.italic_correction(1).expect("italic correction present");
    assert_eq!(ic.value, 60);
    let ta = gi.top_accent_attachment(1).expect("top accent present");
    assert_eq!(ta.value, 400);

    // gid 2 (∫) is in the extended-shape coverage.
    assert!(gi.is_extended_shape(2));
    assert!(!gi.is_extended_shape(1));

    // gid 1 has a top-right kern table; the other three sides return None.
    let mk = gi
        .kern_info(1, KernSide::TopRight)
        .expect("top-right kern present");
    assert_eq!(mk.height_count(), 2);
    assert_eq!(mk.correction_height(0).unwrap().value, 200);
    assert_eq!(mk.correction_height(1).unwrap().value, 400);
    assert_eq!(mk.kern_value(0).unwrap().value, 10);
    assert_eq!(mk.kern_value(1).unwrap().value, 20);
    assert_eq!(mk.kern_value(2).unwrap().value, 30);
    assert!(gi.kern_info(1, KernSide::TopLeft).is_none());
    assert!(gi.kern_info(1, KernSide::BottomLeft).is_none());
}

#[test]
fn math_variants_returns_construction_for_integral_sign() {
    let bytes = std::fs::read(FIXTURE).unwrap();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    // Resolve U+222B → gid via the cmap, just like a real shaper.
    let cmap = face.cmap().unwrap();
    let integral_gid = cmap.glyph_id('\u{222B}').expect("∫ in cmap");
    assert_eq!(integral_gid, 2);

    let math = face.math().unwrap().expect("MATH present");
    let mv = math.variants().unwrap().expect("variants present");
    assert_eq!(mv.min_connector_overlap(), 64);

    let cons = mv
        .vertical_glyph_construction(integral_gid)
        .expect("vertical construction for ∫");
    assert_eq!(cons.variant_count(), 2);
    let v0 = cons.variant(0).unwrap();
    assert_eq!(v0.variant_glyph, 2);
    assert_eq!(v0.advance_measurement, 1000);
    let v1 = cons.variant(1).unwrap();
    assert_eq!(v1.variant_glyph, 3);
    assert_eq!(v1.advance_measurement, 2000);

    // The fixture also provides a 3-part assembly (top, extender,
    // bottom). Sanity-check the extender flag.
    let asm = cons.assembly().expect("assembly present");
    assert_eq!(asm.italics_correction.value, 120);
    assert_eq!(asm.part_count(), 3);
    let parts: Vec<_> = asm.iter().collect();
    assert!(!parts[0].is_extender());
    assert!(parts[1].is_extender(), "middle part is the extender");
    assert!(!parts[2].is_extender());
    assert_eq!(parts[1].full_advance, 1500);

    // Horizontal lookup for a gid not in horizontal coverage → None.
    assert!(mv.horizontal_glyph_construction(integral_gid).is_none());
}

#[test]
fn face_math_returns_none_for_non_math_font() {
    // Open Sans has no MATH table; the accessor should yield Ok(None).
    let bytes = std::fs::read("tests/fixtures/opensans_regular.ttf").unwrap();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(face.math().unwrap().is_none());
}
