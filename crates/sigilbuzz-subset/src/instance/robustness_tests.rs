//! Hostile-input regressions for the instancing helpers.

use super::glyf::{
    bake_simple_glyph, FLAG_ON_CURVE, FLAG_REPEAT, FLAG_X_SAME_OR_POS, FLAG_Y_SAME_OR_POS,
};
use super::ivs::project_ivs;
use super::metrics::apply_mvar_records;
use super::*;

/// Serializes a 1-axis IVS with one region peaking at +1 and one
/// subtable of `rows`, each row one delta. `long` selects i32 rows.
fn one_region_ivs(subtable_count: u16, rows: &[i32], long: bool) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    let region_list_off = 8 + 4 * u32::from(subtable_count);
    out.extend_from_slice(&region_list_off.to_be_bytes());
    out.extend_from_slice(&subtable_count.to_be_bytes());
    // Every subtable offset points at the same subtable.
    let subtable_off = region_list_off + 4 + 6;
    for _ in 0..subtable_count {
        out.extend_from_slice(&subtable_off.to_be_bytes());
    }
    out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
    out.extend_from_slice(&1u16.to_be_bytes()); // regionCount
    out.extend_from_slice(&0i16.to_be_bytes());
    out.extend_from_slice(&0x4000i16.to_be_bytes());
    out.extend_from_slice(&0x4000i16.to_be_bytes());
    out.extend_from_slice(&(rows.len() as u16).to_be_bytes()); // itemCount
    let word_delta_count: u16 = if long { 0x8001 } else { 0 };
    out.extend_from_slice(&word_delta_count.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
    out.extend_from_slice(&0u16.to_be_bytes()); // regionIndexes[0]
    for &row in rows {
        if long {
            out.extend_from_slice(&row.to_be_bytes());
        } else {
            out.push(row as i8 as u8);
        }
    }
    out
}

#[test]
fn gdef_ivs_offset_inside_header_rebuilds_without_panicking() {
    // GDEF 1.3 whose itemVarStoreOffset (4) points into its own
    // header. The bytes from offset 4 still form a valid IVS
    // (format 1, region list at +18, no subtables). A rewrite that
    // truncated the table at the store used to patch bytes 14..18
    // of a 4-byte buffer. The GDEF writer lays every table out
    // afresh instead.
    let mut gdef: Vec<u8> = Vec::new();
    gdef.extend_from_slice(&1u16.to_be_bytes()); // major
    gdef.extend_from_slice(&3u16.to_be_bytes()); // minor
    gdef.extend_from_slice(&1u16.to_be_bytes()); // glyphClassDef, read as IVS format
    gdef.extend_from_slice(&18u32.to_be_bytes()); // attach + ligCaret, read as region list offset
    gdef.extend_from_slice(&0u16.to_be_bytes()); // markAttach, read as subtable count
    gdef.extend_from_slice(&0u16.to_be_bytes()); // markGlyphSets
    gdef.extend_from_slice(&4u32.to_be_bytes()); // itemVarStoreOffset
    gdef.extend_from_slice(&[0, 0, 0, 0]); // padding up to the region list
    gdef.extend_from_slice(&1u16.to_be_bytes()); // axisCount
    gdef.extend_from_slice(&0u16.to_be_bytes()); // regionCount
    assert!(bake_ivs_partial(&gdef[4..], &[0.0], &[AxisPin::Keep]).is_some());
    let map = crate::layout::GidMap::from_kept(&[0]);
    let warnings = crate::warnings::Warnings::default();
    let rebuilt =
        store_remap::bake_gdef_bytes_partial(&gdef, &map, &[0.0], &[AxisPin::Keep], &warnings);
    assert!(rebuilt.is_ok());
}

#[test]
fn mvar_long_word_delta_saturates_the_patched_field() {
    // One `hasc` record whose long-word delta is i32::MAX. Adding it
    // to sTypoAscender used to overflow i32 before the clamp.
    let ivs = one_region_ivs(1, &[i32::MAX], true);
    let mut mvar: Vec<u8> = Vec::new();
    mvar.extend_from_slice(&1u16.to_be_bytes()); // major
    mvar.extend_from_slice(&0u16.to_be_bytes()); // minor
    mvar.extend_from_slice(&0u16.to_be_bytes()); // reserved
    mvar.extend_from_slice(&8u16.to_be_bytes()); // valueRecordSize
    mvar.extend_from_slice(&1u16.to_be_bytes()); // valueRecordCount
    mvar.extend_from_slice(&20u16.to_be_bytes()); // itemVariationStoreOffset
    mvar.extend_from_slice(b"hasc");
    mvar.extend_from_slice(&0u16.to_be_bytes()); // outer
    mvar.extend_from_slice(&0u16.to_be_bytes()); // inner
    mvar.extend_from_slice(&ivs);
    let mvar = sigilbuzz::tables::Mvar::parse(&mvar).expect("MVAR");
    let mut os2 = alloc::vec![0u8; 96];
    os2[68..70].copy_from_slice(&800i16.to_be_bytes());
    let baked = apply_mvar_records(&mvar, &[1.0], Some(os2), None, None, None).unwrap();
    let out = baked.os2.unwrap();
    assert_eq!(i16::from_be_bytes([out[68], out[69]]), i16::MAX);
}

#[test]
fn mvar_with_many_records_is_walked_in_linear_time() {
    // 65535 distinct unrecognized tags and no variation store. A
    // per-record scan of every earlier record is quadratic.
    let count: u16 = u16::MAX;
    let mut mvar: Vec<u8> = Vec::new();
    mvar.extend_from_slice(&1u16.to_be_bytes()); // major
    mvar.extend_from_slice(&0u16.to_be_bytes()); // minor
    mvar.extend_from_slice(&0u16.to_be_bytes()); // reserved
    mvar.extend_from_slice(&8u16.to_be_bytes()); // valueRecordSize
    mvar.extend_from_slice(&count.to_be_bytes());
    mvar.extend_from_slice(&0u16.to_be_bytes()); // no store
    for i in 0..count {
        let [hi, lo] = i.to_be_bytes();
        mvar.extend_from_slice(&[b'z', b'z', hi, lo, 0, 0, 0, 0]);
    }
    let mvar = sigilbuzz::tables::Mvar::parse(&mvar).expect("MVAR");
    let os2 = alloc::vec![0u8; 96];
    let baked = apply_mvar_records(&mvar, &[1.0], Some(os2.clone()), None, None, None).unwrap();
    assert_eq!(baked.os2, Some(os2));
}

#[test]
fn simple_glyph_with_many_points_bakes_in_linear_time() {
    // One contour of 65535 points, every flag repeated, every
    // coordinate "same as previous", and one delta per point. A
    // per-point scan of the delta list is quadratic.
    let last_point: u16 = u16::MAX - 1;
    let total = usize::from(last_point) + 1;
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(&1i16.to_be_bytes()); // numberOfContours
    body.extend_from_slice(&[0; 8]); // bbox
    body.extend_from_slice(&last_point.to_be_bytes());
    body.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
    let flag = FLAG_ON_CURVE | FLAG_X_SAME_OR_POS | FLAG_Y_SAME_OR_POS;
    let mut remaining = total;
    while remaining > 0 {
        let run = remaining.min(256);
        body.push(flag | FLAG_REPEAT);
        body.push((run - 1) as u8);
        remaining -= run;
    }
    let deltas: Vec<sigilbuzz::tables::PointDelta> = (0..=last_point)
        .map(|point| sigilbuzz::tables::PointDelta {
            point,
            dx: 1.0,
            dy: 0.0,
        })
        .collect();
    let baked = bake_simple_glyph(&body, &deltas).expect("bake");
    // Every point moved by +1 on x: the new bbox is (1, 0, 1, 0).
    assert_eq!(&baked[2..10], &[0, 1, 0, 0, 0, 1, 0, 0]);
}

#[test]
fn ivs_with_aliased_subtables_is_rejected() {
    // 2000 subtable offsets that all point at one subtable of 30000
    // rows. Rewriting each offset separately used to emit 2000
    // copies of the subtable.
    let rows: Vec<i32> = (0..30_000).map(|i| i % 100).collect();
    let ivs = one_region_ivs(2000, &rows, false);
    assert!(matches!(
        project_ivs(&ivs, &[1.0], &[AxisPin::Keep]),
        Err(SubsetError::Parse(sigilbuzz::Error::Malformed {
            offset: 6,
            ..
        }))
    ));
    // A single reference to the same subtable still rewrites.
    let single = one_region_ivs(1, &rows, false);
    assert!(bake_ivs_partial(&single, &[1.0], &[AxisPin::Keep]).is_some());
}
