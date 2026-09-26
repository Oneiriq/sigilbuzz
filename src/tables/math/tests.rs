//! Tests for the `MATH` parser: the header and each subtable.

use super::*;
use crate::tables::math::constants::MATH_CONSTANTS_LEN;
use alloc::{vec, vec::Vec};

// ---- helpers ---------------------------------------------------------

fn push_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}
fn push_i16(v: &mut Vec<u8>, x: i16) {
    v.extend_from_slice(&x.to_be_bytes());
}

/// Build a minimal Coverage Format 1 covering the given gids in order.
fn coverage_fmt1(gids: &[u16]) -> Vec<u8> {
    let mut v = Vec::new();
    push_u16(&mut v, 1); // format
    push_u16(&mut v, gids.len() as u16);
    for g in gids {
        push_u16(&mut v, *g);
    }
    v
}

// ---- MATH header -----------------------------------------------------

#[test]
fn math_header_rejects_bad_version() {
    let mut v = Vec::new();
    push_u16(&mut v, 2); // major
    push_u16(&mut v, 0); // minor
    push_u16(&mut v, 0);
    push_u16(&mut v, 0);
    push_u16(&mut v, 0);
    assert!(matches!(Math::parse(&v), Err(Error::Malformed { .. })));
}

#[test]
fn math_header_with_all_offsets_zero_returns_none_subtables() {
    let mut v = Vec::new();
    push_u16(&mut v, 1);
    push_u16(&mut v, 0);
    push_u16(&mut v, 0);
    push_u16(&mut v, 0);
    push_u16(&mut v, 0);
    let m = Math::parse(&v).unwrap();
    assert!(m.constants().unwrap().is_none());
    assert!(m.glyph_info().unwrap().is_none());
    assert!(m.variants().unwrap().is_none());
}

#[test]
fn math_header_rejects_offset_past_end() {
    let mut v = Vec::new();
    push_u16(&mut v, 1);
    push_u16(&mut v, 0);
    push_u16(&mut v, 9999); // bogus constants offset
    push_u16(&mut v, 0);
    push_u16(&mut v, 0);
    assert!(matches!(Math::parse(&v), Err(Error::Malformed { .. })));
}

// ---- MathConstants ---------------------------------------------------

fn build_constants() -> Vec<u8> {
    let mut v = Vec::new();
    push_i16(&mut v, 80); // scriptPercentScaleDown
    push_i16(&mut v, 60); // scriptScriptPercentScaleDown
    push_u16(&mut v, 1500); // delimitedSubFormulaMinHeight
    push_u16(&mut v, 1800); // displayOperatorMinHeight
                            // 51 == NUM_VALUE_RECORDS; spelled inline to keep the literal
                            // a plain i16 for clippy's cast-possible-wrap lint.
    for i in 0i16..51 {
        push_i16(&mut v, 100 + i); // value
        push_u16(&mut v, 0); // device offset = 0 (none)
    }
    push_i16(&mut v, 65); // radicalDegreeBottomRaisePercent
    v
}

#[test]
fn math_constants_parses_scalar_header_fields() {
    let bytes = build_constants();
    let c = MathConstants::parse(&bytes).unwrap();
    assert_eq!(c.script_percent_scale_down(), 80);
    assert_eq!(c.script_script_percent_scale_down(), 60);
    assert_eq!(c.delimited_sub_formula_min_height(), 1500);
    assert_eq!(c.display_operator_min_height(), 1800);
    assert_eq!(c.radical_degree_bottom_raise_percent(), 65);
}

#[test]
fn math_constants_returns_value_records_for_named_fields() {
    let bytes = build_constants();
    let c = MathConstants::parse(&bytes).unwrap();
    // axisHeight is index 1 -> 100 + 1 = 101.
    assert_eq!(c.axis_height().value, 101);
    assert!(c.axis_height().device.is_none());
    // fractionRuleThickness is index 34 -> 100 + 34 = 134.
    assert_eq!(c.fraction_rule_thickness().value, 134);
    // radicalKernAfterDegree is the last value record (idx 50).
    assert_eq!(c.radical_kern_after_degree().value, 150);
}

#[test]
fn math_constants_rejects_truncated_input() {
    let bytes = vec![0u8; MATH_CONSTANTS_LEN - 1];
    assert!(matches!(
        MathConstants::parse(&bytes),
        Err(Error::Truncated { .. })
    ));
}

// ---- MathGlyphInfo ---------------------------------------------------

/// Builds a `MathItalicsCorrectionInfo` table covering a single gid
/// with the given correction value. Returned bytes: header at the
/// start, coverage appended after the value-record array.
fn italics_info_one(gid: u16, value: i16) -> Vec<u8> {
    let mut v = Vec::new();
    // coverageOffset = 8 (after the 4-byte header + 4-byte record).
    push_u16(&mut v, 8);
    push_u16(&mut v, 1); // count
    push_i16(&mut v, value);
    push_u16(&mut v, 0); // device = 0
    v.extend_from_slice(&coverage_fmt1(&[gid]));
    v
}

/// Build a minimal MathGlyphInfo with only italics correction populated.
fn glyph_info_italics_only(gid: u16, value: i16) -> Vec<u8> {
    let mut v = Vec::new();
    // header is 8 bytes (4 x Offset16). italics offset = 8.
    push_u16(&mut v, 8);
    push_u16(&mut v, 0);
    push_u16(&mut v, 0);
    push_u16(&mut v, 0);
    v.extend_from_slice(&italics_info_one(gid, value));
    v
}

#[test]
fn glyph_info_returns_italic_correction_for_covered_gid() {
    let bytes = glyph_info_italics_only(7, 42);
    let gi = MathGlyphInfo::parse(&bytes).unwrap();
    let mv = gi.italic_correction(7).unwrap();
    assert_eq!(mv.value, 42);
    assert!(gi.italic_correction(8).is_none());
}

#[test]
fn glyph_info_extended_shape_coverage_check() {
    let mut v = Vec::new();
    push_u16(&mut v, 0); // italics off
    push_u16(&mut v, 0); // top accent off
    push_u16(&mut v, 8); // extended shape off (after header)
    push_u16(&mut v, 0); // kern info off
    v.extend_from_slice(&coverage_fmt1(&[3, 5, 7]));
    let gi = MathGlyphInfo::parse(&v).unwrap();
    assert!(gi.is_extended_shape(3));
    assert!(gi.is_extended_shape(5));
    assert!(gi.is_extended_shape(7));
    assert!(!gi.is_extended_shape(4));
    assert!(!gi.is_extended_shape(99));
}

#[test]
fn glyph_info_absent_subtables_yield_none() {
    // All four offsets zero: nothing is present.
    let bytes = vec![0u8; 8];
    let gi = MathGlyphInfo::parse(&bytes).unwrap();
    assert!(gi.italic_correction(0).is_none());
    assert!(gi.top_accent_attachment(0).is_none());
    assert!(!gi.is_extended_shape(0));
    assert!(gi.kern_info(0, KernSide::TopRight).is_none());
}

// ---- MathKern --------------------------------------------------------

fn build_math_kern(heights: &[i16], kerns: &[i16]) -> Vec<u8> {
    assert_eq!(kerns.len(), heights.len() + 1);
    let mut v = Vec::new();
    push_u16(&mut v, heights.len() as u16);
    for h in heights {
        push_i16(&mut v, *h);
        push_u16(&mut v, 0);
    }
    for k in kerns {
        push_i16(&mut v, *k);
        push_u16(&mut v, 0);
    }
    v
}

#[test]
fn math_kern_exposes_height_and_kern_arrays() {
    let bytes = build_math_kern(&[100, 200, 300], &[5, 10, 15, 20]);
    let mk = MathKern::parse(&bytes).unwrap();
    assert_eq!(mk.height_count(), 3);
    assert_eq!(mk.correction_height(0).unwrap().value, 100);
    assert_eq!(mk.correction_height(2).unwrap().value, 300);
    assert!(mk.correction_height(3).is_none());
    assert_eq!(mk.kern_value(0).unwrap().value, 5);
    assert_eq!(mk.kern_value(3).unwrap().value, 20);
    assert!(mk.kern_value(4).is_none());
}

#[test]
fn math_kern_truncated_arrays_error() {
    let mut v = Vec::new();
    push_u16(&mut v, 5);
    v.extend_from_slice(&[0u8; 4]); // far short of needed
    assert!(matches!(MathKern::parse(&v), Err(Error::Truncated { .. })));
}

#[test]
fn glyph_info_kern_info_lookup_finds_top_right_kern() {
    // Build a MathKernInfo table covering gid 9 with a non-null
    // top-right kern. Layout:
    //   [0..2]   coverageOffset
    //   [2..4]   mathKernCount = 1
    //   [4..12]  MathKernInfoRecord (4 x Offset16: TR/TL/BR/BL)
    //   [12..]   coverage
    //   [..]     MathKern table (from build_math_kern)
    let kern_table = build_math_kern(&[50], &[7, 9]);
    let coverage = coverage_fmt1(&[9]);
    // Record top-right offset = 12 + coverage.len() (start of kern).
    let top_right_off = 12 + coverage.len() as u16;
    let mut info = Vec::new();
    push_u16(&mut info, 12); // coverage offset
    push_u16(&mut info, 1); // mathKernCount
    push_u16(&mut info, top_right_off); // top-right
    push_u16(&mut info, 0); // top-left
    push_u16(&mut info, 0); // bottom-right
    push_u16(&mut info, 0); // bottom-left
    info.extend_from_slice(&coverage);
    info.extend_from_slice(&kern_table);

    // Wrap in a MathGlyphInfo whose kern-info offset is 8.
    let mut gi_bytes = Vec::new();
    push_u16(&mut gi_bytes, 0);
    push_u16(&mut gi_bytes, 0);
    push_u16(&mut gi_bytes, 0);
    push_u16(&mut gi_bytes, 8); // kern info offset
    gi_bytes.extend_from_slice(&info);

    let gi = MathGlyphInfo::parse(&gi_bytes).unwrap();
    let mk = gi.kern_info(9, KernSide::TopRight).expect("present");
    assert_eq!(mk.height_count(), 1);
    assert_eq!(mk.correction_height(0).unwrap().value, 50);
    assert_eq!(mk.kern_value(1).unwrap().value, 9);
    assert!(gi.kern_info(9, KernSide::TopLeft).is_none());
    assert!(gi.kern_info(99, KernSide::TopRight).is_none());
}

// ---- MathVariants ----------------------------------------------------

/// Builds a `MathGlyphConstruction` with two variants and no
/// assembly. Returns just the construction-table bytes.
fn construction_two_variants(v1: u16, a1: u16, v2: u16, a2: u16) -> Vec<u8> {
    let mut v = Vec::new();
    push_u16(&mut v, 0); // assembly offset = 0 (no assembly)
    push_u16(&mut v, 2); // variant count
    push_u16(&mut v, v1);
    push_u16(&mut v, a1);
    push_u16(&mut v, v2);
    push_u16(&mut v, a2);
    v
}

/// Builds a `MathGlyphConstruction` with one variant + one
/// assembly part (extender) so we can exercise `assembly()`.
fn construction_with_assembly() -> Vec<u8> {
    let mut v = Vec::new();
    // Construction header: assembly offset will be written after
    // we know the variant array size. For one variant, header (4)
    // + 4 = 8.
    push_u16(&mut v, 8); // assembly offset
    push_u16(&mut v, 1); // variant count
    push_u16(&mut v, 11); // variant glyph
    push_u16(&mut v, 1000); // advance
                            // GlyphAssembly: italicsCorrection (4) + partCount (2) + parts.
    push_i16(&mut v, 25); // italics correction value
    push_u16(&mut v, 0); // device = 0
    push_u16(&mut v, 1); // partCount
    push_u16(&mut v, 22); // glyphID
    push_u16(&mut v, 100); // startConnector
    push_u16(&mut v, 100); // endConnector
    push_u16(&mut v, 500); // fullAdvance
    push_u16(&mut v, PART_FLAG_EXTENDER);
    v
}

/// Builds a `MathVariants` table that maps gid 7 to a vertical
/// construction with two variants and gid 8 to a horizontal
/// construction with assembly. Layout is hand-laid so the
/// integration of Coverage + offset arrays can be checked.
fn build_math_variants() -> Vec<u8> {
    // Header is 10 bytes; after that, two construction-offset
    // arrays (vert then horiz, 1 entry each) -> 4 more bytes.
    // After that we lay out the rest in order:
    //   verticalCoverage, verticalConstruction,
    //   horizontalCoverage, horizontalConstruction.
    let header_len = 10;
    let vert_offsets = 2;
    let horiz_offsets = 2;
    let mut v = Vec::with_capacity(64);
    let mut tail = Vec::new();

    let vert_cov_off = (header_len + vert_offsets + horiz_offsets + tail.len()) as u16;
    let vert_cov = coverage_fmt1(&[7]);
    tail.extend_from_slice(&vert_cov);

    let vert_cons_off = (header_len + vert_offsets + horiz_offsets + tail.len()) as u16;
    let vert_cons = construction_two_variants(101, 1000, 102, 2000);
    tail.extend_from_slice(&vert_cons);

    let horiz_cov_off = (header_len + vert_offsets + horiz_offsets + tail.len()) as u16;
    let horiz_cov = coverage_fmt1(&[8]);
    tail.extend_from_slice(&horiz_cov);

    let horiz_cons_off = (header_len + vert_offsets + horiz_offsets + tail.len()) as u16;
    let horiz_cons = construction_with_assembly();
    tail.extend_from_slice(&horiz_cons);

    // header
    push_u16(&mut v, 32); // minConnectorOverlap
    push_u16(&mut v, vert_cov_off);
    push_u16(&mut v, horiz_cov_off);
    push_u16(&mut v, 1); // vertGlyphCount
    push_u16(&mut v, 1); // horizGlyphCount
    push_u16(&mut v, vert_cons_off);
    push_u16(&mut v, horiz_cons_off);
    v.extend_from_slice(&tail);
    v
}

#[test]
fn math_variants_returns_vertical_construction_for_covered_gid() {
    let bytes = build_math_variants();
    let mv = MathVariants::parse(&bytes).unwrap();
    assert_eq!(mv.min_connector_overlap(), 32);
    let cons = mv.vertical_glyph_construction(7).expect("covered");
    assert_eq!(cons.variant_count(), 2);
    let v0 = cons.variant(0).unwrap();
    assert_eq!(v0.variant_glyph, 101);
    assert_eq!(v0.advance_measurement, 1000);
    let v1 = cons.variant(1).unwrap();
    assert_eq!(v1.variant_glyph, 102);
    assert_eq!(v1.advance_measurement, 2000);
    assert!(cons.assembly().is_none());
    assert!(mv.vertical_glyph_construction(99).is_none());
}

#[test]
fn math_variants_assembly_exposes_parts_and_extender_flag() {
    let bytes = build_math_variants();
    let mv = MathVariants::parse(&bytes).unwrap();
    let cons = mv.horizontal_glyph_construction(8).expect("horiz covered");
    assert_eq!(cons.variant_count(), 1);
    let asm = cons.assembly().expect("has assembly");
    assert_eq!(asm.italics_correction.value, 25);
    assert_eq!(asm.part_count(), 1);
    let p = asm.part(0).unwrap();
    assert_eq!(p.glyph_id, 22);
    assert_eq!(p.full_advance, 500);
    assert_eq!(p.start_connector_length, 100);
    assert_eq!(p.end_connector_length, 100);
    assert!(p.is_extender());
    assert_eq!(asm.iter().count(), 1);
}

#[test]
fn math_variants_horizontal_lookup_misses_for_vertical_gid() {
    let bytes = build_math_variants();
    let mv = MathVariants::parse(&bytes).unwrap();
    // gid 7 is in the vertical coverage, not horizontal.
    assert!(mv.horizontal_glyph_construction(7).is_none());
}

#[test]
fn math_variants_truncated_arrays_error() {
    // Header claims 5 vertical entries but the bytes stop at the header.
    let mut v = Vec::new();
    push_u16(&mut v, 0);
    push_u16(&mut v, 0);
    push_u16(&mut v, 0);
    push_u16(&mut v, 5);
    push_u16(&mut v, 0);
    assert!(matches!(
        MathVariants::parse(&v),
        Err(Error::Truncated { .. })
    ));
}

// ---- end-to-end via Math header --------------------------------------

#[test]
fn math_table_routes_offsets_to_each_subtable() {
    // Build a MATH table whose header points at a constants block,
    // glyph-info block, and variants block laid out back to back.
    let constants = build_constants();
    let gi = glyph_info_italics_only(3, 90);
    let mv = build_math_variants();

    let mut bytes = Vec::new();
    // Header is 10 bytes.
    push_u16(&mut bytes, 1); // major
    push_u16(&mut bytes, 0); // minor
    let const_off = 10u16;
    let gi_off = const_off + constants.len() as u16;
    let mv_off = gi_off + gi.len() as u16;
    push_u16(&mut bytes, const_off);
    push_u16(&mut bytes, gi_off);
    push_u16(&mut bytes, mv_off);
    bytes.extend_from_slice(&constants);
    bytes.extend_from_slice(&gi);
    bytes.extend_from_slice(&mv);

    let math = Math::parse(&bytes).unwrap();
    let c = math.constants().unwrap().expect("constants present");
    assert_eq!(c.script_percent_scale_down(), 80);
    let info = math.glyph_info().unwrap().expect("glyph info present");
    assert_eq!(info.italic_correction(3).unwrap().value, 90);
    let variants = math.variants().unwrap().expect("variants present");
    assert_eq!(variants.min_connector_overlap(), 32);
}
