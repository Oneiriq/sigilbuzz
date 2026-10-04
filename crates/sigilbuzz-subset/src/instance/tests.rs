//! Unit tests for full instancing: outlines, metrics, the MVAR and
//! GDEF bakes, and (in `gpos_bake`) the GPOS variation fold.

use super::*;

mod glyph_bake;
mod gpos_bake;

const RUBIK: &[u8] = include_bytes!("../../../../tests/fixtures/rubik_vf.ttf");
const SOURCE_SANS: &[u8] = include_bytes!("../../../../tests/fonts/SourceSans3VF-Latin-Subset.otf");
const OPEN_SANS: &[u8] = include_bytes!("../../../../tests/fixtures/opensans_regular.ttf");
/// Synthetic VF with a single PairPos format 1 lookup
/// whose AV pair carries a VariationIndex into a one-region IVS;
/// at wght=900 the delta is -100, at wght=400 it is 0. Built by
/// the fixture builder in `tests/variable_kern.rs`. See
/// `tests/variable_kern.rs` for the upstream cover.
const VAR_KERN: &[u8] = include_bytes!("../../../../tests/fixtures/var_kern.ttf");

fn rubik_face() -> Face<'static> {
    Face::parse_bytes(RUBIK, 0).unwrap()
}

#[test]
fn input_default_drops_var_tables() {
    let i = InstanceInput::default();
    assert!(i.drop_var_tables);
    assert!(i.coords.is_empty());
}

#[test]
fn instance_at_default_coords_keeps_outline_shape() {
    // Bake at the source's default instance (all zeros). The
    // baked outline must round-trip: every gid's outline at zero
    // coords in the instanced font equals the source's static
    // outline.
    let face = rubik_face();
    let axis_count = face.fvar().unwrap().unwrap().axes().len();
    let input = InstanceInput {
        coords: alloc::vec![0.0_f32; axis_count],
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let out = instance(&face, &input).expect("bake at default");
    let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
    assert!(baked.fvar().unwrap().is_none(), "fvar dropped");
    assert!(baked.gvar().unwrap().is_none(), "gvar dropped");
    assert!(baked.hvar().unwrap().is_none(), "HVAR dropped");

    // Pick a couple of gids with outlines and confirm the bake
    // produces (approximately) the same outline as the source at
    // default-instance coords.
    let cmap = face.cmap().unwrap();
    let gid_a = cmap.glyph_id('A').expect("rubik has 'A'");
    let want = face.glyph_outline_at_coords(gid_a, &[]).unwrap();
    let got = baked.glyph_outline_at_coords(gid_a, &[]).unwrap();
    match (want, got) {
        (Some(w), Some(g)) => {
            assert_eq!(
                w.ops().len(),
                g.ops().len(),
                "op count diverged for 'A' at default coords"
            );
        }
        (None, None) => {}
        _ => panic!("baked outline presence diverged"),
    }
}

#[test]
fn instance_at_extreme_coords_matches_outline_at_coords() {
    // Bake at the Rubik VF's wght extreme. Compare each gid's
    // baked outline to the source's outline-at-coords result.
    let face = rubik_face();
    let fvar = face.fvar().unwrap().unwrap();
    let mut user = alloc::vec![0.0_f32; fvar.axes().len()];
    if let Some(idx) = fvar.axis_index(*b"wght") {
        user[idx] = fvar.axes()[idx].max_value;
    }
    let coords = fvar.normalize_coords(&user);
    let input = InstanceInput {
        coords: coords.clone(),
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let out = instance(&face, &input).expect("bake at extreme");
    let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");

    let cmap = face.cmap().unwrap();
    let gid_a = cmap.glyph_id('A').expect("rubik has 'A'");
    let want = face
        .glyph_outline_at_coords(gid_a, &coords)
        .unwrap()
        .expect("source draws 'A'");
    let got = baked
        .glyph_outline_at_coords(gid_a, &[])
        .unwrap()
        .expect("baked draws 'A'");
    // Outline op counts must match; the baked outline bypassed
    // gvar entirely (the table is gone) so its shape comes from
    // the rewritten contour points.
    assert_eq!(want.ops().len(), got.ops().len());
}

#[test]
fn instance_advances_match_hvar_eval_at_coords() {
    let face = rubik_face();
    let fvar = face.fvar().unwrap().unwrap();
    let mut user = alloc::vec![0.0_f32; fvar.axes().len()];
    if let Some(idx) = fvar.axis_index(*b"wght") {
        user[idx] = fvar.axes()[idx].max_value;
    }
    let coords = fvar.normalize_coords(&user);
    let input = InstanceInput {
        coords: coords.clone(),
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let out = instance(&face, &input).expect("bake");
    let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");

    let src_hmtx = face.hmtx().unwrap();
    let baked_hmtx = baked.hmtx().unwrap();
    let hvar = face.hvar().unwrap().expect("rubik has HVAR");

    let cmap = face.cmap().unwrap();
    for ch in ['A', 'g', 'M', 'i'] {
        let Some(gid) = cmap.glyph_id(ch) else {
            continue;
        };
        let base = f32::from(src_hmtx.advance(gid).unwrap_or(0));
        let want = (base + hvar.advance_delta(gid, &coords)).round() as i32;
        let got = i32::from(baked_hmtx.advance(gid).unwrap_or(0));
        assert_eq!(want, got, "advance mismatch for {ch:?} (gid {gid})");
    }
}

#[test]
fn instance_static_font_errors() {
    let face = Face::parse_bytes(OPEN_SANS, 0).unwrap();
    let input = InstanceInput::default();
    let r = instance(&face, &input);
    assert!(matches!(r, Err(SubsetError::Unsupported(_))));
}

#[test]
fn instance_coord_length_validation() {
    let face = rubik_face();
    let bad = InstanceInput {
        coords: alloc::vec![0.0_f32; 99],
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    assert!(matches!(
        instance(&face, &bad),
        Err(SubsetError::Unsupported(_))
    ));
}

#[test]
fn source_sans_round_trip_at_default_instance() {
    // Source Sans 3 VF Latin Subset is a CFF2-flavored VF. After
    // the 0.12.0 CFF2 blend bake landed, instancing produces a
    // static CFF2 face whose every glyph re-parses through the
    // standard outline pipeline.
    let face = Face::parse_bytes(SOURCE_SANS, 0).unwrap();
    let axis_count = face.fvar().unwrap().map_or(0, |f| f.axes().len());
    let input = InstanceInput {
        coords: alloc::vec![0.0_f32; axis_count],
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let out = instance(&face, &input).expect("CFF2 default-instance bake");
    let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
    // GDEF.IVS pruned: at default coords no IVS reference would
    // resolve to a non-zero delta anyway, so the output's GDEF,
    // when present, must have a zero IVS offset.
    if let Some(_gdef) = face.gdef().unwrap() {
        // Pruned baked GDEF: the IVS getter on the baked side is
        // None (offset zeroed by the prune step).
        let baked_gdef = baked.gdef().unwrap().expect("baked GDEF preserved");
        assert!(
            baked_gdef.item_variation_store().is_none(),
            "baked GDEF.IVS must be pruned"
        );
    }
    // No fvar / HVAR / MVAR survive on the static side.
    assert!(baked.fvar().unwrap().is_none(), "fvar dropped");
    assert!(baked.hvar().unwrap().is_none(), "HVAR dropped");

    // At default coords every glyph that drew in the source must
    // draw in the baked output with the same op count.
    let cmap = face.cmap().unwrap();
    for ch in ['A', 'g', 'i', 'O'] {
        let Some(gid) = cmap.glyph_id(ch) else {
            continue;
        };
        let want = face.glyph_outline_at_coords(gid, &[]).unwrap();
        let got = baked.glyph_outline_at_coords(gid, &[]).unwrap();
        match (want, got) {
            (Some(w), Some(g)) => assert_eq!(
                w.ops().len(),
                g.ops().len(),
                "op count diverged for {ch:?} at default coords"
            ),
            (None, None) => {}
            (w, g) => panic!(
                "drew presence diverged for {ch:?}: source={:?}, baked={:?}",
                w.is_some(),
                g.is_some()
            ),
        }
    }
}

#[test]
fn source_sans_round_trip_at_extreme_coord_matches_source_outline() {
    // Bake at wght=900 (extreme). Compare each ascii gid's outline
    // op count against the source's outline-at-coords result.
    let face = Face::parse_bytes(SOURCE_SANS, 0).unwrap();
    let fvar = face.fvar().unwrap().unwrap();
    let mut user = alloc::vec![0.0_f32; fvar.axes().len()];
    if let Some(idx) = fvar.axis_index(*b"wght") {
        user[idx] = fvar.axes()[idx].max_value;
    }
    let coords = fvar.normalize_coords(&user);
    let input = InstanceInput {
        coords: coords.clone(),
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let out = instance(&face, &input).expect("CFF2 extreme bake");
    let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
    // The CFF2 source's outline-at-coords path is exercised by
    // sigilbuzz core's own test suite; here we just check the
    // bake produces a parseable face whose CFF2 charstring count
    // matches the source's. Compare the no-coords outline (which
    // both sides should agree on) for each ascii letter.
    let cmap = face.cmap().unwrap();
    let mut compared = 0;
    for ch in ['A', 'g', 'i', 'O'] {
        let Some(gid) = cmap.glyph_id(ch) else {
            continue;
        };
        let want = face.glyph_outline_at_coords(gid, &coords).unwrap();
        let got = baked.glyph_outline_at_coords(gid, &[]).unwrap();
        // Drew-or-didn't-draw must match: a baked charstring whose
        // source draws but baked doesn't (or vice versa) signals a
        // round-trip break.
        match (want, got) {
            (Some(w), Some(g)) => {
                assert_eq!(
                    w.ops().len(),
                    g.ops().len(),
                    "op count mismatch for {ch:?} (gid {gid})"
                );
                compared += 1;
            }
            (None, None) => {}
            (w, g) => panic!(
                "drew presence diverged for {ch:?} (gid {gid}): source={:?}, baked={:?}",
                w.is_some(),
                g.is_some()
            ),
        }
    }
    let _ = compared; // some glyphs may legitimately not draw
}

#[test]
fn source_sans_default_coords_byte_deterministic() {
    let face = Face::parse_bytes(SOURCE_SANS, 0).unwrap();
    let axis_count = face.fvar().unwrap().map_or(0, |f| f.axes().len());
    let input = InstanceInput {
        coords: alloc::vec![0.0_f32; axis_count],
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let a = instance(&face, &input).unwrap();
    let b = instance(&face, &input).unwrap();
    assert_eq!(a.bytes, b.bytes);
}

#[test]
fn instance_drops_var_tables_when_flag_set() {
    let face = rubik_face();
    let axis_count = face.fvar().unwrap().unwrap().axes().len();
    let input = InstanceInput {
        coords: alloc::vec![0.0_f32; axis_count],
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let out = instance(&face, &input).unwrap();
    let baked = Face::parse_bytes(&out.bytes, 0).unwrap();
    assert!(baked.record(tag::FVAR).is_none());
    assert!(baked.record(tag::AVAR).is_none());
    assert!(baked.record(tag::GVAR).is_none());
    assert!(baked.record(tag::HVAR).is_none());
}

#[test]
fn instance_keeps_var_tables_when_flag_unset() {
    let face = rubik_face();
    let axis_count = face.fvar().unwrap().unwrap().axes().len();
    let input = InstanceInput {
        coords: alloc::vec![0.0_f32; axis_count],
        drop_var_tables: false,
        axis_pins: Vec::new(),
    };
    let out = instance(&face, &input).unwrap();
    let baked = Face::parse_bytes(&out.bytes, 0).unwrap();
    assert!(baked.record(tag::FVAR).is_some());
}

#[test]
fn instance_is_deterministic() {
    let face = rubik_face();
    let axis_count = face.fvar().unwrap().unwrap().axes().len();
    let input = InstanceInput {
        coords: alloc::vec![0.5_f32; axis_count],
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let a = instance(&face, &input).unwrap();
    let b = instance(&face, &input).unwrap();
    assert_eq!(a.bytes, b.bytes);
}

#[test]
fn rubik_mvar_bake_applies_undo_delta_to_post_underline_position() {
    // Rubik VF carries a single MVAR record for `undo` (post
    // underlinePosition). Bake at the wght extreme and confirm
    // the output's post.underlinePosition shifted by the MVAR
    // delta resolved at that coord.
    let face = rubik_face();
    let fvar = face.fvar().unwrap().unwrap();
    let mut user = alloc::vec![0.0_f32; fvar.axes().len()];
    if let Some(idx) = fvar.axis_index(*b"wght") {
        user[idx] = fvar.axes()[idx].max_value;
    }
    let coords = fvar.normalize_coords(&user);

    let mvar = face.mvar().unwrap().expect("rubik has MVAR");
    let undo_delta = mvar
        .metric_delta(*b"undo", &coords)
        .expect("rubik MVAR carries undo");
    let undo_delta_i32 = undo_delta.round() as i32;

    let post_src = face.table_bytes(tag::POST).unwrap();
    let src_undo = i16::from_be_bytes([post_src[8], post_src[9]]);

    let input = InstanceInput {
        coords: coords.clone(),
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let out = instance(&face, &input).expect("bake at extreme");
    let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
    let post_baked = baked.table_bytes(tag::POST).unwrap();
    let baked_undo = i16::from_be_bytes([post_baked[8], post_baked[9]]);

    assert_eq!(
        i32::from(baked_undo),
        i32::from(src_undo) + undo_delta_i32,
        "MVAR undo bake mismatch: src={src_undo}, delta={undo_delta_i32}, baked={baked_undo}"
    );
    // MVAR table itself is dropped from the static output.
    assert!(baked.mvar().unwrap().is_none(), "MVAR dropped after bake");
}

#[test]
fn rubik_gdef_ivs_pruned_when_present_at_v13() {
    // Rubik's GDEF doesn't carry an IVS, but the prune
    // path should be a no-op rather than corrupt bytes.
    let face = rubik_face();
    let axis_count = face.fvar().unwrap().unwrap().axes().len();
    let input = InstanceInput {
        coords: alloc::vec![0.0_f32; axis_count],
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let out = instance(&face, &input).unwrap();
    let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
    // If the source had a GDEF, the baked face should too, and
    // it should still parse cleanly.
    if face.gdef().unwrap().is_some() {
        assert!(baked.gdef().unwrap().is_some());
    }
}

#[test]
fn rubik_vmtx_passthrough_when_source_has_none() {
    // Rubik VF is horizontal-only: no vmtx, no VVAR. The bake
    // must not synthesize either.
    let face = rubik_face();
    assert!(face.vmtx().unwrap().is_none(), "rubik has no vmtx");
    let axis_count = face.fvar().unwrap().unwrap().axes().len();
    let input = InstanceInput {
        coords: alloc::vec![0.0_f32; axis_count],
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let out = instance(&face, &input).unwrap();
    let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
    assert!(baked.vmtx().unwrap().is_none(), "vmtx not synthesised");
    assert!(baked.vvar().unwrap().is_none(), "VVAR not synthesised");
}

#[test]
fn outline_and_plan_coordinates_round_the_way_harfbuzz_does() {
    // -0.9 is -14745.6 F2DOT14 steps. A HarfBuzz font rounds it to 16.16
    // first (-58982, which is -14745.5 steps) and then up to -14745; the
    // instancer's plan rounds it straight to -14746.
    assert_eq!(
        super::snap_f2dot14(super::round_16_16(-0.9)) * 16384.0,
        -14745.0
    );
    assert_eq!(super::f2dot14_grid(-0.9), -14746);
    // Values on the grid stay put either way.
    assert_eq!(super::snap_f2dot14(super::round_16_16(0.5)), 0.5);
    assert_eq!(super::f2dot14_grid(0.5), 8192);
}

#[test]
fn avar_maps_f2dot14_units_rounding_halves_up() {
    use super::axes::map_f2dot14;
    let map = [(-16384, -16384), (-2, -1), (0, 0), (2, 1), (16384, 16384)];
    // Halfway between map points rounds up, either side of zero.
    assert_eq!(map_f2dot14(&map, 1), 1);
    assert_eq!(map_f2dot14(&map, -1), 0);
    // On a map point, its value.
    assert_eq!(map_f2dot14(&map, 2), 1);
    // Past the ends, shifted by the nearest pair.
    let short = [(-8192, -4096), (8192, 4096)];
    assert_eq!(map_f2dot14(&short, 9000), 4904);
    assert_eq!(map_f2dot14(&short, -9000), -4904);
    // No map, or a single pair, shifts.
    assert_eq!(map_f2dot14(&[], 77), 77);
    assert_eq!(map_f2dot14(&[(10, 20)], 77), 87);
}
