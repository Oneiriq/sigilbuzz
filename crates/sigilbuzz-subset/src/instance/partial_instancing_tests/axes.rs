//! fvar and avar trim tests.

use super::*;

// --------------------------------------------------------------
// bake_fvar_partial: fvar trim.
// --------------------------------------------------------------

/// Builds a synthetic 2-axis fvar (wght 100..400..900,
/// wdth 50..100..200) with `instances`, each carrying a
/// (subfamilyNameID, flags, [coord_per_axis], optional ps_name_id).
fn build_fvar2(instances: &[(u16, u16, [f32; 2], Option<u16>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let with_ps = instances.iter().any(|(_, _, _, p)| p.is_some());
    let inst_size: u16 = if with_ps { 4 + 4 * 2 + 2 } else { 4 + 4 * 2 };
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&16u16.to_be_bytes()); // axesArrayOffset
    out.extend_from_slice(&2u16.to_be_bytes()); // reserved
    out.extend_from_slice(&2u16.to_be_bytes()); // axisCount
    out.extend_from_slice(&20u16.to_be_bytes()); // axisSize
    out.extend_from_slice(&(instances.len() as u16).to_be_bytes());
    out.extend_from_slice(&inst_size.to_be_bytes());
    // Axis 0: wght
    out.extend_from_slice(b"wght");
    write_f16dot16(&mut out, 100.0);
    write_f16dot16(&mut out, 400.0);
    write_f16dot16(&mut out, 900.0);
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&256u16.to_be_bytes());
    // Axis 1: wdth
    out.extend_from_slice(b"wdth");
    write_f16dot16(&mut out, 50.0);
    write_f16dot16(&mut out, 100.0);
    write_f16dot16(&mut out, 200.0);
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&257u16.to_be_bytes());
    for (sub, flags, coords, ps) in instances {
        out.extend_from_slice(&sub.to_be_bytes());
        out.extend_from_slice(&flags.to_be_bytes());
        for c in coords {
            write_f16dot16(&mut out, *c);
        }
        if with_ps {
            out.extend_from_slice(&ps.unwrap_or(0).to_be_bytes());
        }
    }
    out
}

#[test]
fn bake_fvar_partial_returns_none_when_every_axis_pins() {
    let bytes = build_fvar2(&[]);
    assert!(bake_fvar_partial(&bytes, &[AxisPin::Pin, AxisPin::Pin]).is_none());
}

#[test]
fn bake_fvar_partial_drops_pin_axis_records() {
    // Pin wght, keep wdth: survivor fvar has only the wdth axis.
    let bytes = build_fvar2(&[]);
    let trimmed = bake_fvar_partial(&bytes, &[AxisPin::Pin, AxisPin::Keep]).unwrap();
    // Header layout matches the spec: 16 bytes, axisCount = 1.
    assert_eq!(u16::from_be_bytes([trimmed[8], trimmed[9]]), 1);
    // First axis tag is now wdth.
    assert_eq!(&trimmed[16..20], b"wdth");
    // Re-parse via the public Fvar parser. It must accept the
    // emitted bytes.
    let parsed = sigilbuzz::tables::Fvar::parse(&trimmed).unwrap();
    assert_eq!(parsed.axes().len(), 1);
    assert_eq!(parsed.axes()[0].tag, *b"wdth");
}

#[test]
fn bake_fvar_partial_keeps_kept_axis_in_source_order() {
    // Pin wdth, keep wght -> only wght survives.
    let bytes = build_fvar2(&[]);
    let trimmed = bake_fvar_partial(&bytes, &[AxisPin::Keep, AxisPin::Pin]).unwrap();
    assert_eq!(u16::from_be_bytes([trimmed[8], trimmed[9]]), 1);
    assert_eq!(&trimmed[16..20], b"wght");
}

#[test]
fn bake_fvar_partial_drops_instances_that_collapse_to_default() {
    // Three instances: (Regular wght=400 wdth=100, default-equal),
    // (Bold wght=700 wdth=100), (Condensed wght=400 wdth=75).
    // With wght pinned, the (400, 100) instance collapses to "wdth
    // default" -> drop. The (700, 100) instance collapses to "wdth
    // default" -> drop. The (400, 75) survives at wdth=75.
    let bytes = build_fvar2(&[
        (1, 0, [400.0, 100.0], None),
        (2, 0, [700.0, 100.0], None),
        (3, 0, [400.0, 75.0], None),
    ]);
    let trimmed = bake_fvar_partial(&bytes, &[AxisPin::Pin, AxisPin::Keep]).unwrap();
    // instanceCount = 1 (only Condensed survived).
    assert_eq!(u16::from_be_bytes([trimmed[12], trimmed[13]]), 1);
}

#[test]
fn bake_fvar_partial_round_trips_with_ps_name_variant() {
    let bytes = build_fvar2(&[(1, 0, [700.0, 100.0], Some(258))]);
    let trimmed = bake_fvar_partial(&bytes, &[AxisPin::Pin, AxisPin::Keep]).unwrap();
    // instanceSize for the trimmed (1-axis, with-ps) variant
    // = 4 + 4 * 1 + 2 = 10.
    assert_eq!(u16::from_be_bytes([trimmed[14], trimmed[15]]), 10);
    // Bold's (700, 100) collapses to wdth-default after Pin-wght
    // (instance dropped). instanceCount = 0.
    assert_eq!(u16::from_be_bytes([trimmed[12], trimmed[13]]), 0);
}

// --------------------------------------------------------------
// bake_avar_partial: avar trim.
// --------------------------------------------------------------

fn build_avar2(map_a: &[(f32, f32)], map_b: &[(f32, f32)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes()); // axisCount
    for map in &[map_a, map_b] {
        out.extend_from_slice(&(map.len() as u16).to_be_bytes());
        for (f, t) in *map {
            write_f2dot14(&mut out, *f);
            write_f2dot14(&mut out, *t);
        }
    }
    out
}

#[test]
fn bake_avar_partial_returns_none_when_every_axis_pins() {
    let bytes = build_avar2(&[(-1.0, -1.0), (0.0, 0.0), (1.0, 1.0)], &[]);
    assert!(bake_avar_partial(&bytes, &[AxisPin::Pin, AxisPin::Pin]).is_none());
}

#[test]
fn bake_avar_partial_drops_pin_axis_segment_map() {
    let map_w = &[(-1.0, -1.0), (0.0, 0.0), (0.5, 0.75), (1.0, 1.0)];
    let bytes = build_avar2(map_w, &[(-1.0, -1.0), (0.0, 0.0), (1.0, 1.0)]);
    let trimmed = bake_avar_partial(&bytes, &[AxisPin::Pin, AxisPin::Keep]).unwrap();
    let parsed = sigilbuzz::tables::Avar::parse(&trimmed).unwrap();
    assert_eq!(parsed.axis_count(), 1);
    // The surviving axis was axis 1 (the trivial 3-point identity).
    // Confirm round-trip: 0.5 -> 0.5.
    assert!((parsed.remap(0, 0.5) - 0.5).abs() < 1e-3);
}

#[test]
fn bake_avar_partial_keeps_first_axis_when_second_pins() {
    let map_w = &[(-1.0, -1.0), (0.0, 0.0), (0.5, 0.75), (1.0, 1.0)];
    let bytes = build_avar2(map_w, &[(-1.0, -1.0), (0.0, 0.0), (1.0, 1.0)]);
    let trimmed = bake_avar_partial(&bytes, &[AxisPin::Keep, AxisPin::Pin]).unwrap();
    let parsed = sigilbuzz::tables::Avar::parse(&trimmed).unwrap();
    assert_eq!(parsed.axis_count(), 1);
    // The non-trivial map survived: 0.5 -> 0.75.
    assert!((parsed.remap(0, 0.5) - 0.75).abs() < 1e-3);
}
