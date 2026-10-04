//! Store projection tests: the ItemVariationStore, the HVAR, VVAR,
//! MVAR and GDEF tables around it, and their checked offsets.

use super::*;

// --------------------------------------------------------------
// bake_ivs_partial: IVS region trim + delta scale.
// --------------------------------------------------------------

/// Builds a 2-axis IVS with `regions`, `subtables[i] = (regionIndexes,
/// rows)` where each row has one i16 delta per region index.
fn build_ivs2(
    regions: &[[(f32, f32, f32); 2]],
    subtables: &[(Vec<u16>, Vec<Vec<i16>>)],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    let region_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(subtables.len() as u16).to_be_bytes());
    let sub_slot_start = out.len();
    for _ in 0..subtables.len() {
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    // Region list.
    let region_off = out.len() as u32;
    out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_off.to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes()); // axisCount
    out.extend_from_slice(&(regions.len() as u16).to_be_bytes());
    for region in regions {
        for (s, p, e) in region {
            write_f2dot14(&mut out, *s);
            write_f2dot14(&mut out, *p);
            write_f2dot14(&mut out, *e);
        }
    }
    // Subtables.
    for (i, (region_indexes, rows)) in subtables.iter().enumerate() {
        let sub_off = out.len() as u32;
        let slot = sub_slot_start + i * 4;
        out[slot..slot + 4].copy_from_slice(&sub_off.to_be_bytes());
        out.extend_from_slice(&(rows.len() as u16).to_be_bytes()); // itemCount
                                                                   // wordDeltaCount = regionIndexCount, all i16.
        out.extend_from_slice(&(region_indexes.len() as u16).to_be_bytes());
        out.extend_from_slice(&(region_indexes.len() as u16).to_be_bytes());
        for ri in region_indexes {
            out.extend_from_slice(&ri.to_be_bytes());
        }
        for row in rows {
            assert_eq!(row.len(), region_indexes.len());
            for v in row {
                out.extend_from_slice(&v.to_be_bytes());
            }
        }
    }
    out
}

#[test]
fn bake_ivs_partial_pin_one_axis_keep_other_drops_pin_dimension() {
    // 2-axis IVS, one region (peak (1, 1)), one subtable with one
    // delta of 100. Pin wght=1.0 (scalar 1.0), keep wdth.
    let bytes = build_ivs2(
        &[[(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)]],
        &[(alloc::vec![0], alloc::vec![alloc::vec![100]])],
    );
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let coords = [1.0, 0.0];
    let (out, remap) = bake_ivs_partial(&bytes, &coords, &pins).expect("survives");
    let parsed = sigilbuzz::tables::variation_store::ItemVariationStore::parse(&out).unwrap();
    assert_eq!(parsed.axis_count(), 1);
    assert_eq!(parsed.region_count(), 1);
    // At wdth=1.0, the delta is the original 100 (scaled by Pin
    // scalar of 1.0 because wght pin is at the region's peak).
    let d = parsed.delta(0, 0, &[1.0]);
    assert!((d - 100.0).abs() < 1e-3, "got {}", d);
    assert_eq!(remap.lookup(0, 0), Some((0, 0)));
}

#[test]
fn bake_ivs_partial_drops_region_when_pin_outside() {
    // Region peaks at wght=1, wdth=1. Pin wght=0 (outside [0, 1]
    // boundary trivially gives scalar=0 because peak=1, coord=0:
    // ramp from start=0 to peak=1 -> 0). Region drops, subtable
    // collapses.
    let bytes = build_ivs2(
        &[[(0.5, 1.0, 1.0), (0.0, 1.0, 1.0)]],
        &[(alloc::vec![0], alloc::vec![alloc::vec![100]])],
    );
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let coords = [0.0, 0.0];
    let (_out, remap) = bake_ivs_partial(&bytes, &coords, &pins).expect("emits empty IVS");
    // Subtable collapsed entirely.
    assert_eq!(remap.lookup(0, 0), None);
}

#[test]
fn bake_ivs_partial_scales_delta_by_pin_scalar() {
    // Region with wght peak=1, wdth peak=1. Pin wght=0.5 -> scalar
    // 0.5. Source delta 100 -> new delta 50.
    let bytes = build_ivs2(
        &[[(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)]],
        &[(alloc::vec![0], alloc::vec![alloc::vec![100]])],
    );
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let coords = [0.5, 0.0];
    let (out, _remap) = bake_ivs_partial(&bytes, &coords, &pins).unwrap();
    let parsed = sigilbuzz::tables::variation_store::ItemVariationStore::parse(&out).unwrap();
    // At wdth=1.0, evaluate the new tuple: scalar = 1.0 (peak),
    // delta = 50.
    let d = parsed.delta(0, 0, &[1.0]);
    assert!((d - 50.0).abs() < 1.0, "got {}", d);
}

#[test]
fn bake_ivs_partial_round_trips_at_keep_coord() {
    // The pivotal correctness property: evaluating the trimmed IVS
    // at (Keep coord) reproduces evaluating the source IVS at
    // (Keep coord, Pin coord). Two regions, two-axis source, pin
    // wght=0.6, keep wdth. Item delta = (regionA: 100, regionB: 50).
    // Source A: peak=(1, 1), so scalar at (0.6, wdth) = 0.6 * wdth.
    // Source B: peak=(0, 1), wght peak=0 means "axis ignored" so
    // scalar is just wdth.
    // Source eval at (0.6, wdth=1) = 0.6*1*100 + 1*1*50 = 110.
    // Trimmed eval at (wdth=1) = 1*60 + 1*50 = 110. (delta_A
    // pre-scaled by 0.6 -> 60; delta_B pre-scaled by 1 -> 50.)
    let bytes = build_ivs2(
        &[
            [(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)],
            [(0.0, 0.0, 0.0), (0.0, 1.0, 1.0)],
        ],
        &[(alloc::vec![0, 1], alloc::vec![alloc::vec![100, 50]])],
    );
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let coords = [0.6, 0.0];
    let (out, _remap) = bake_ivs_partial(&bytes, &coords, &pins).unwrap();
    let parsed = sigilbuzz::tables::variation_store::ItemVariationStore::parse(&out).unwrap();
    let d = parsed.delta(0, 0, &[1.0]);
    assert!((d - 110.0).abs() < 1.0, "got {}", d);
}

#[test]
fn bake_ivs_partial_collapses_empty_subtable() {
    // Two subtables; subtable 1 only references a region that
    // drops. RegionRemap reflects the elision.
    let bytes = build_ivs2(
        &[
            [(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)], // region 0: keeps
            [(0.5, 1.0, 1.0), (0.0, 1.0, 1.0)], // region 1: drops at coord 0
        ],
        &[
            (alloc::vec![0], alloc::vec![alloc::vec![100]]),
            (alloc::vec![1], alloc::vec![alloc::vec![999]]),
        ],
    );
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let coords = [0.0, 0.0];
    let (_out, remap) = bake_ivs_partial(&bytes, &coords, &pins).unwrap();
    // Subtable 0 referenced only region 0. Region 0 drops at
    // coord=0 too (peak=1, coord=0 -> scalar 0 on wght). So both
    // subtables collapse.
    assert_eq!(remap.lookup(0, 0), None);
    assert_eq!(remap.lookup(1, 0), None);
}

#[test]
fn bake_ivs_partial_preserves_inner_index_order() {
    // Two items in one subtable. The trimmed IVS keeps both, in
    // the same inner-index order, scaled by the pin scalar.
    let bytes = build_ivs2(
        &[[(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)]],
        &[(
            alloc::vec![0],
            alloc::vec![alloc::vec![100], alloc::vec![200]],
        )],
    );
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let coords = [1.0, 0.0]; // pin at peak -> scalar 1
    let (out, remap) = bake_ivs_partial(&bytes, &coords, &pins).unwrap();
    let parsed = sigilbuzz::tables::variation_store::ItemVariationStore::parse(&out).unwrap();
    assert!((parsed.delta(0, 0, &[1.0]) - 100.0).abs() < 1e-3);
    assert!((parsed.delta(0, 1, &[1.0]) - 200.0).abs() < 1e-3);
    assert_eq!(remap.lookup(0, 0), Some((0, 0)));
    assert_eq!(remap.lookup(0, 1), Some((0, 1)));
}

#[test]
fn bake_ivs_partial_all_keep_is_identity_modulo_format() {
    // With every axis Keep, the IVS must round-trip: same regions,
    // same deltas, just possibly re-encoded with a uniform format.
    let bytes = build_ivs2(
        &[
            [(0.0, 1.0, 1.0), (-1.0, -1.0, 0.0)],
            [(0.0, 0.5, 1.0), (0.0, 0.0, 0.0)],
        ],
        &[(
            alloc::vec![0, 1],
            alloc::vec![alloc::vec![100, 50], alloc::vec![-30, 70]],
        )],
    );
    let pins = [AxisPin::Keep, AxisPin::Keep];
    let coords = [0.0, 0.0];
    let (out, _remap) = bake_ivs_partial(&bytes, &coords, &pins).unwrap();
    let src = sigilbuzz::tables::variation_store::ItemVariationStore::parse(&bytes).unwrap();
    let dst = sigilbuzz::tables::variation_store::ItemVariationStore::parse(&out).unwrap();
    assert_eq!(src.axis_count(), dst.axis_count());
    assert_eq!(src.region_count(), dst.region_count());
    // Same deltas at the same coords.
    for c0 in [-1.0, -0.5, 0.0, 0.5, 1.0] {
        for c1 in [-1.0, -0.5, 0.0, 0.5, 1.0] {
            let a = src.delta(0, 0, &[c0, c1]);
            let b = dst.delta(0, 0, &[c0, c1]);
            assert!(
                (a - b).abs() < 1.0,
                "mismatch at ({}, {}): src={}, dst={}",
                c0,
                c1,
                a,
                b
            );
        }
    }
}

// --------------------------------------------------------------
// Host-table partial bakes (HVAR / VVAR / MVAR / GDEF.IVS).
// --------------------------------------------------------------

/// Builds an HVAR table (no maps; gid is the inner index directly)
/// wrapping the given IVS bytes.
fn build_hvar_no_maps(ivs: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&20u32.to_be_bytes()); // ivs offset = header end
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(ivs);
    out
}

#[test]
fn bake_hvar_partial_round_trips_at_keep_coord() {
    // 2-axis IVS, one region (peak at (1, 1)), one item delta = 100.
    // Pin wght=0.5, keep wdth -> delta scales to 50; HVAR's gid-0
    // delta at wdth=1 must be 50.
    let ivs = build_ivs2(
        &[[(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)]],
        &[(alloc::vec![0], alloc::vec![alloc::vec![100]])],
    );
    let hvar = build_hvar_no_maps(&ivs);
    let new_hvar =
        bake_hvar_partial(&hvar, &[0.5, 0.0], &[AxisPin::Pin, AxisPin::Keep]).expect("bake");
    let parsed = sigilbuzz::tables::Hvar::parse(&new_hvar).unwrap();
    let d = parsed.advance_delta(0, &[1.0]);
    assert!((d - 50.0).abs() < 1.0, "got {}", d);
}

#[test]
fn bake_hvar_partial_zeroes_dropped_subtable_lookups() {
    // Region drops at the pin coord (peak at (1, 1), pin coord 0
    // on wght -> scalar 0). Subtable collapses; HVAR.advance_delta
    // must return 0, not NaN, not panic.
    let ivs = build_ivs2(
        &[[(0.5, 1.0, 1.0), (0.0, 1.0, 1.0)]],
        &[(alloc::vec![0], alloc::vec![alloc::vec![100]])],
    );
    let hvar = build_hvar_no_maps(&ivs);
    let new_hvar =
        bake_hvar_partial(&hvar, &[0.0, 0.0], &[AxisPin::Pin, AxisPin::Keep]).expect("bake");
    let parsed = sigilbuzz::tables::Hvar::parse(&new_hvar).unwrap();
    // Subtable count is now zero; (outer=0, inner=0) is out of
    // range -> IVS evaluator returns 0.
    let d = parsed.advance_delta(0, &[1.0]);
    assert!(d.abs() < 1e-3, "got {}", d);
}

#[test]
fn implicit_advances_keep_reading_the_first_subtable() {
    // No advance map: advances read outer 0 by glyph id. Subtable 0
    // varies them on axis 0 only; subtable 1 (which only a side
    // bearing map would name) varies on axis 1. Pinning axis 0 and
    // dropping the pinned-only regions empties subtable 0, which must
    // stay at outer 0 so advances read zero on the kept axis, not
    // subtable 1's deltas.
    let ivs = build_ivs2(
        &[
            [(0.0, 1.0, 1.0), (0.0, 0.0, 0.0)],
            [(0.0, 0.0, 0.0), (0.0, 1.0, 1.0)],
        ],
        &[
            (
                alloc::vec![0],
                alloc::vec![alloc::vec![100], alloc::vec![100]],
            ),
            (alloc::vec![1], alloc::vec![alloc::vec![7], alloc::vec![7]]),
        ],
    );
    let hvar = build_hvar_no_maps(&ivs);
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let dropped =
        super::super::metrics_var::bake_hvar_partial_with(&hvar, &[1.0, 0.0], &pins).expect("bake");
    let parsed = sigilbuzz::tables::Hvar::parse(&dropped).unwrap();
    for gid in 0..2 {
        for kept in [0.0, 1.0] {
            let d = parsed.advance_delta(gid, &[kept]);
            assert!(d.abs() < 1e-3, "gid {gid} at {kept}: {d}");
        }
    }
    // VVAR reads its advance heights the same way.
    let mut vvar = Vec::new();
    vvar.extend_from_slice(&[0, 1, 0, 0]);
    vvar.extend_from_slice(&24u32.to_be_bytes()); // ivs offset = header end
    vvar.extend_from_slice(&[0; 16]); // no maps
    vvar.extend_from_slice(&ivs);
    let dropped =
        super::super::metrics_var::bake_vvar_partial_with(&vvar, &[1.0, 0.0], &pins).expect("bake");
    let parsed = sigilbuzz::tables::Vvar::parse(&dropped).unwrap();
    for gid in 0..2 {
        for kept in [0.0, 1.0] {
            let d = parsed.advance_height_delta(gid, &[kept]);
            assert!(d.abs() < 1e-3, "VVAR gid {gid} at {kept}: {d}");
        }
    }
}

/// Builds an MVAR table with `records` x (tag, outer=0, inner=0)
/// pointing at the embedded IVS.
fn build_mvar(records: &[[u8; 4]], ivs: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&0u16.to_be_bytes()); // reserved
    out.extend_from_slice(&8u16.to_be_bytes()); // valueRecordSize
    out.extend_from_slice(&(records.len() as u16).to_be_bytes());
    let store_off_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // store offset placeholder
    for tag in records {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u16.to_be_bytes()); // outer
        out.extend_from_slice(&0u16.to_be_bytes()); // inner
    }
    let store_off = out.len() as u16;
    out[store_off_slot..store_off_slot + 2].copy_from_slice(&store_off.to_be_bytes());
    out.extend_from_slice(ivs);
    out
}

#[test]
fn bake_mvar_partial_round_trips_at_keep_coord() {
    let ivs = build_ivs2(
        &[[(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)]],
        &[(alloc::vec![0], alloc::vec![alloc::vec![80]])],
    );
    let mvar = build_mvar(&[*b"hasc"], &ivs);
    let new_mvar =
        bake_mvar_partial(&mvar, &[0.5, 0.0], &[AxisPin::Pin, AxisPin::Keep]).expect("bake");
    let parsed = sigilbuzz::tables::Mvar::parse(&new_mvar).unwrap();
    // Pin scalar 0.5; at wdth=1.0 the trimmed tuple gives 40.
    let d = parsed.metric_delta(*b"hasc", &[1.0]).unwrap();
    assert!((d - 40.0).abs() < 1.0, "got {}", d);
}

#[test]
fn bake_mvar_partial_zeroes_collapsed_record() {
    let ivs = build_ivs2(
        &[[(0.5, 1.0, 1.0), (0.0, 1.0, 1.0)]],
        &[(alloc::vec![0], alloc::vec![alloc::vec![80]])],
    );
    let mvar = build_mvar(&[*b"hasc"], &ivs);
    let new_mvar =
        bake_mvar_partial(&mvar, &[0.0, 0.0], &[AxisPin::Pin, AxisPin::Keep]).expect("bake");
    let parsed = sigilbuzz::tables::Mvar::parse(&new_mvar).unwrap();
    // Subtable collapsed; (outer=0, inner=0) is now out of range
    // -> 0 delta.
    let d = parsed.metric_delta(*b"hasc", &[1.0]).unwrap();
    assert!(d.abs() < 1e-3, "got {}", d);
}

// --------------------------------------------------------------
// Checked offsets and sizes in the partial bakes.
// --------------------------------------------------------------

/// The byte offset a parse error reports.
fn error_offset(err: &SubsetError) -> usize {
    match err {
        SubsetError::Parse(
            sigilbuzz::Error::Truncated { offset, .. } | sigilbuzz::Error::Malformed { offset, .. },
        ) => *offset,
        other => panic!("expected a parse error, got {other:?}"),
    }
}

fn one_region_ivs() -> Vec<u8> {
    build_ivs2(
        &[[(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)]],
        &[(alloc::vec![0], alloc::vec![alloc::vec![100]])],
    )
}

const PIN_KEEP: [AxisPin; 2] = [AxisPin::Pin, AxisPin::Keep];

#[test]
fn store_offset32s_past_the_table_are_reported_at_their_slot() {
    // regionListOffset and itemVariationDataOffsets[0] near the top
    // of the u32 range: added to a position they would wrap a
    // 32-bit usize. Every target reports the slot instead.
    for slot in [2, 8] {
        let mut ivs = one_region_ivs();
        ivs[slot..slot + 4].copy_from_slice(&(u32::MAX - 1).to_be_bytes());
        let err = project_ivs(&ivs, &[1.0, 0.0], &PIN_KEEP).unwrap_err();
        assert_eq!(error_offset(&err), slot);
        assert!(bake_ivs_partial(&ivs, &[1.0, 0.0], &PIN_KEEP).is_none());
    }
}

#[test]
fn huge_store_counts_are_truncation_not_wraparound() {
    // regionCount and then itemCount at their u16 maximum, with a
    // LONG_WORDS row as wide as it gets: the sizes are checked
    // products, so the store is merely too short.
    let mut ivs = one_region_ivs();
    let regions = u32::from_be_bytes([ivs[2], ivs[3], ivs[4], ivs[5]]) as usize;
    ivs[regions + 2..regions + 4].copy_from_slice(&u16::MAX.to_be_bytes());
    let err = project_ivs(&ivs, &[1.0, 0.0], &PIN_KEEP).unwrap_err();
    assert_eq!(error_offset(&err), regions + 4);

    let mut ivs = one_region_ivs();
    let sub = u32::from_be_bytes([ivs[8], ivs[9], ivs[10], ivs[11]]) as usize;
    ivs[sub..sub + 2].copy_from_slice(&u16::MAX.to_be_bytes());
    ivs[sub + 2..sub + 4].copy_from_slice(&0x8001u16.to_be_bytes());
    let err = project_ivs(&ivs, &[1.0, 0.0], &PIN_KEEP).unwrap_err();
    assert_eq!(
        error_offset(&err),
        sub + 8,
        "the rows start after one index"
    );
}

#[test]
fn a_subtable_keeping_more_regions_than_word_delta_count_holds_is_an_error() {
    // The rewrite writes every kept column wide, so wordDeltaCount
    // equals the kept region count. At 32,768 its top bit, the
    // LONG_WORDS flag, used to turn on and garble every row.
    let region = [(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)];
    let fits = |count: u16| {
        let ivs = build_ivs2(
            &alloc::vec![region; usize::from(count)],
            &[(
                (0..count).collect(),
                alloc::vec![alloc::vec![0; usize::from(count)]],
            )],
        );
        let sub = u32::from_be_bytes([ivs[8], ivs[9], ivs[10], ivs[11]]) as usize;
        project_ivs(&ivs, &[1.0, 0.0], &PIN_KEEP).map_err(|e| (error_offset(&e), e, sub))
    };
    assert!(fits(0x7FFF).is_ok());
    let (offset, err, sub) = fits(0x8000).unwrap_err();
    assert_eq!(offset, sub + 4, "reported at regionIndexCount");
    assert!(
        matches!(
            err,
            SubsetError::Parse(sigilbuzz::Error::Malformed {
                context: "ItemVariationData keeps more than 32,767 regions",
                ..
            })
        ),
        "{err:?}"
    );
}

#[test]
fn delta_set_index_maps_are_bounds_checked() {
    let remap = RegionRemap::default();
    // Format 1 with mapCount u32::MAX: the entry array would wrap a
    // 32-bit usize; it is reported as running out at the entries.
    let mut map = alloc::vec![1u8, 0x00];
    map.extend_from_slice(&u32::MAX.to_be_bytes());
    map.extend_from_slice(&[0, 0]);
    let err = rewrite_delta_set_index_map(&map, 0, &remap, 0).unwrap_err();
    assert!(
        matches!(err, sigilbuzz::Error::Truncated { offset: 6, .. }),
        "{err:?}"
    );
    // A map that starts past the data, and an unknown format.
    assert!(rewrite_delta_set_index_map(&map, usize::MAX - 1, &remap, 0).is_err());
    map[0] = 7;
    assert!(matches!(
        rewrite_delta_set_index_map(&map, 0, &remap, 0),
        Err(sigilbuzz::Error::Malformed { offset: 0, .. })
    ));
}

#[test]
fn metrics_variation_errors_count_from_the_host_table() {
    // HVAR whose store has an unknown format: the error sits at the
    // store, 20 bytes into HVAR.
    let mut ivs = one_region_ivs();
    ivs[0..2].copy_from_slice(&9u16.to_be_bytes());
    let hvar = build_hvar_no_maps(&ivs);
    let err = bake_hvar_partial(&hvar, &[1.0, 0.0], &PIN_KEEP).unwrap_err();
    assert_eq!(error_offset(&err), 20);
    // An HVAR store offset past the table is reported at its slot.
    let mut hvar = build_hvar_no_maps(&one_region_ivs());
    hvar[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
    let err = bake_hvar_partial(&hvar, &[1.0, 0.0], &PIN_KEEP).unwrap_err();
    assert_eq!(error_offset(&err), 4);
    // So is a DeltaSetIndexMap offset.
    let mut hvar = build_hvar_no_maps(&one_region_ivs());
    hvar[8..12].copy_from_slice(&(u32::MAX - 3).to_be_bytes());
    let err = bake_hvar_partial(&hvar, &[1.0, 0.0], &PIN_KEEP).unwrap_err();
    assert_eq!(error_offset(&err), 8);
}

#[test]
fn mvar_records_outgrowing_the_store_offset_are_an_error() {
    // Ten 7000-byte value records: 70000 bytes of records, so the
    // rebuilt store cannot sit within reach of MVAR's Offset16. The
    // source hides its store in the first record's padding.
    let ivs = one_region_ivs();
    let record_size = 7000usize;
    let mut mvar = Vec::new();
    for v in [1u16, 0, 0, record_size as u16, 10, 20] {
        mvar.extend_from_slice(&v.to_be_bytes());
    }
    mvar.resize(12 + 10 * record_size, 0);
    mvar[12..16].copy_from_slice(b"hasc");
    mvar[20..20 + ivs.len()].copy_from_slice(&ivs);
    assert_eq!(
        bake_mvar_partial(&mvar, &[1.0, 0.0], &PIN_KEEP),
        Err(SubsetError::Unsupported(
            "partial instancing: MVAR value records exceed 64 KiB"
        ))
    );
    // Records running past the table are a parse error at the
    // records, not a wrapped size.
    mvar.truncate(12 + 9 * record_size);
    let err = bake_mvar_partial(&mvar, &[1.0, 0.0], &PIN_KEEP).unwrap_err();
    assert_eq!(error_offset(&err), 12);
}

#[test]
fn a_malformed_hvar_is_dropped_and_reported_not_carried_through() {
    // Rubik with its HVAR store given an unknown format. The
    // partial instance cannot project it, so HVAR goes (the source
    // copy would still count the pinned axes) and the output says
    // why.
    let face = rubik_face();
    let hvar_at = |bytes: &[u8]| {
        let hvar = Face::parse_bytes(bytes, 0).unwrap();
        hvar.table_bytes(tag::HVAR).ok().map(<[u8]>::to_vec)
    };
    let mut hvar = hvar_at(RUBIK).expect("Rubik has HVAR");
    let store = u32::from_be_bytes([hvar[4], hvar[5], hvar[6], hvar[7]]) as usize;
    hvar[store..store + 2].copy_from_slice(&7u16.to_be_bytes());
    let tables: Vec<([u8; 4], Vec<u8>)> = face
        .records()
        .iter()
        .map(|rec| match rec.tag {
            tag::HVAR => (rec.tag, hvar.clone()),
            other => (other, face.table_bytes(other).unwrap().to_vec()),
        })
        .collect();
    let font = sfnt::build(face.sfnt_version(), &tables);
    let broken = Face::parse_bytes(&font, 0).unwrap();
    let input = InstanceInput {
        coords: alloc::vec![0.0],
        drop_var_tables: true,
        axis_pins: alloc::vec![AxisPin::Keep],
    };
    let out = instance(&broken, &input).expect("the instance succeeds");
    assert_eq!(hvar_at(&out.bytes), None, "HVAR is left out");
    let found: Vec<([u8; 4], usize, &str)> = out
        .warnings
        .iter()
        .map(|w| (w.table, w.offset, w.dropped))
        .collect();
    assert_eq!(found, [(tag::HVAR, store, "the whole table")]);
}

/// Builds a minimal v1.3 GDEF carrying just the IVS.
fn build_gdef_v13_ivs_only(ivs: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&3u16.to_be_bytes()); // minor
    out.extend_from_slice(&0u16.to_be_bytes()); // glyphClassDef
    out.extend_from_slice(&0u16.to_be_bytes()); // attachList
    out.extend_from_slice(&0u16.to_be_bytes()); // ligCaretList
    out.extend_from_slice(&0u16.to_be_bytes()); // markAttachClassDef
    out.extend_from_slice(&0u16.to_be_bytes()); // markGlyphSetsDef (v1.2+)
    out.extend_from_slice(&18u32.to_be_bytes()); // itemVarStoreOffset (v1.3)
    out.extend_from_slice(ivs);
    out
}

#[test]
fn partial_gdef_bake_trims_the_store_and_keeps_its_offset() {
    let ivs = build_ivs2(
        &[[(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)]],
        &[(alloc::vec![0], alloc::vec![alloc::vec![100]])],
    );
    let gdef = build_gdef_v13_ivs_only(&ivs);
    let map = crate::layout::GidMap::from_kept(&[0]);
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let (bake, _) = super::store_remap::bake_gdef_bytes_partial(
        &gdef,
        &map,
        &[1.0, 0.0],
        &pins,
        &Warnings::default(),
    )
    .unwrap();
    let GdefBake::Rebuilt(new_gdef) = bake else {
        panic!("expected a rebuilt GDEF");
    };
    // The IVS offset slot is still 18 (header end) and non-zero.
    let new_off = u32::from_be_bytes([new_gdef[14], new_gdef[15], new_gdef[16], new_gdef[17]]);
    assert_eq!(new_off, 18);
    // The trimmed IVS at offset 18 has axisCount = 1.
    let new_ivs_off = new_off as usize;
    let parsed =
        sigilbuzz::tables::variation_store::ItemVariationStore::parse(&new_gdef[new_ivs_off..])
            .unwrap();
    assert_eq!(parsed.axis_count(), 1);
}

#[test]
fn bake_ivs_partial_keeps_cff2_subtables_without_rows() {
    // A CFF2 store's subtables hold no rows: the charstrings carry the
    // deltas. A subtable whose regions survive stays, so `vsindex` and
    // `blend` still find it.
    let bytes = build_ivs2(
        &[[(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)]],
        &[(alloc::vec![0], alloc::vec![])],
    );
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let (out, _remap) = bake_ivs_partial(&bytes, &[0.5, 0.0], &pins).expect("bakes");
    let store = sigilbuzz::tables::variation_store::ItemVariationStore::parse(&out).unwrap();
    assert_eq!(store.subtable_count(), 1);
    assert_eq!(store.variation_region_count(0), Some(1));
}

#[test]
fn regions_on_the_pinned_axes_only_fold_into_each_row() {
    // Region 0 on axis 0 alone, region 1 on axis 1 alone. Pinning axis
    // 0 at 0.5 drops region 0 from the store and reports half its
    // deltas per row, for the caller to add to its defaults.
    let ivs = build_ivs2(
        &[
            [(0.0, 1.0, 1.0), (0.0, 0.0, 0.0)],
            [(0.0, 0.0, 0.0), (0.0, 1.0, 1.0)],
        ],
        &[
            (
                alloc::vec![0, 1],
                alloc::vec![alloc::vec![100, 7], alloc::vec![-40, 3]],
            ),
            (alloc::vec![0], alloc::vec![alloc::vec![12]]),
        ],
    );
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let (out, remap) = super::super::ivs::project_ivs_with(
        &ivs,
        &[0.5, 0.0],
        &pins,
        super::super::ivs::Projection::MERGED,
    )
    .unwrap();
    assert_eq!(remap.folded(0, 0), 50.0);
    assert_eq!(remap.folded(0, 1), -20.0);
    // Subtable 1 had region 0 alone: it goes, and its row folds.
    assert_eq!(remap.lookup(1, 0), None);
    assert_eq!(remap.folded(1, 0), 6.0);
    let store = sigilbuzz::tables::variation_store::ItemVariationStore::parse(&out).unwrap();
    assert_eq!(store.region_count(), 1);
    assert_eq!(store.delta(0, 0, &[1.0]), 7.0);
    assert_eq!(store.delta(0, 1, &[1.0]), 3.0);
    assert_eq!(store.delta(0, 0, &[0.0]), 0.0, "nothing at the new default");
}
