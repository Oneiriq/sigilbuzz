//! Variable-font subsetting round-trip tests.
//!
//! Verify that subsetting a variable font with `retain_variations =
//! true` (the default) preserves the per-glyph variation behavior
//! through the new gid namespace:
//!
//! - `gvar`: glyph outline deltas at non-default coords match the
//!   source font's deltas for the same gid.
//! - `HVAR`: advance-width deltas at non-default coords match.
//! - `fvar` / `avar`: byte-identical pass-through.
//!
//! The fixture is Rubik VF (`tests/fixtures/rubik_vf.ttf`), the same
//! variable font the workspace's shaping integration tests already
//! load. It carries a single `wght` axis spanning 300..900.

use sigilbuzz::tables::tag;
use sigilbuzz::{Blob, Face};
use sigilbuzz_subset::{subset, SubsetInput};

const RUBIK: &[u8] = include_bytes!("../../../tests/fixtures/rubik_vf.ttf");

fn rubik_face() -> Face<'static> {
    Face::parse_bytes(RUBIK, 0).unwrap()
}

fn cmap_lookup(face: &Face<'_>, ch: char) -> u16 {
    face.cmap().unwrap().glyph_id(ch).unwrap()
}

fn subset_to_abc() -> (Face<'static>, Vec<u8>, Vec<(u16, u16)>) {
    let face = rubik_face();
    let gid_a = cmap_lookup(&face, 'A');
    let gid_b = cmap_lookup(&face, 'B');
    let gid_c = cmap_lookup(&face, 'C');
    let input = SubsetInput {
        gids: vec![gid_a, gid_b, gid_c],
        ..Default::default()
    };
    let out = subset(&face, &input).expect("subset succeeds");
    (face, out.bytes, out.gid_map)
}

#[test]
fn fvar_passthrough_is_byte_identical() {
    let (face, bytes, _) = subset_to_abc();
    let blob = Blob::from_vec(bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    let src_fvar = face.table_bytes(tag::FVAR).unwrap();
    let new_fvar = subset_face.table_bytes(tag::FVAR).unwrap();
    assert_eq!(src_fvar, new_fvar);
}

#[test]
fn avar_passthrough_is_byte_identical_when_present() {
    let (face, bytes, _) = subset_to_abc();
    let blob = Blob::from_vec(bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    match face.table_bytes(tag::AVAR) {
        Ok(src_avar) => {
            let new_avar = subset_face.table_bytes(tag::AVAR).unwrap();
            assert_eq!(src_avar, new_avar);
        }
        Err(_) => {
            // Source has no avar; subset must also have no avar.
            assert!(subset_face.table_bytes(tag::AVAR).is_err());
        }
    }
}

#[test]
fn hvar_advance_delta_matches_source_for_kept_gids() {
    let (face, bytes, gid_map) = subset_to_abc();
    let blob = Blob::from_vec(bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    let src_hvar = face.hvar().unwrap().expect("rubik has HVAR");
    let new_hvar = subset_face.hvar().unwrap().expect("subset retains HVAR");
    let coords = face.fvar().unwrap().unwrap().normalize_coords(&[900.0]);
    for (old_gid, new_gid) in &gid_map {
        let want = src_hvar.advance_delta(*old_gid, &coords);
        let got = new_hvar.advance_delta(*new_gid, &coords);
        // Allow 1-unit drift from i16 quantization on rebuild.
        assert!(
            (want - got).abs() <= 1.0,
            "HVAR delta mismatch at old={old_gid} new={new_gid}: want {want} got {got}",
        );
    }
}

#[test]
fn gvar_deltas_match_source_for_kept_gids() {
    let (face, bytes, gid_map) = subset_to_abc();
    let blob = Blob::from_vec(bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    let src_gvar = face.gvar().unwrap().expect("rubik has gvar");
    let new_gvar = subset_face.gvar().unwrap().expect("subset retains gvar");
    let coords = face.fvar().unwrap().unwrap().normalize_coords(&[900.0]);

    let src_loca = face.loca().unwrap();
    let src_glyf = face.glyf().unwrap();

    for (old_gid, new_gid) in &gid_map {
        let Some(num_points) = src_glyf.point_count(&src_loca, *old_gid).unwrap() else {
            continue;
        };
        let want = src_gvar.glyph_deltas(*old_gid, &coords, num_points);
        let got = new_gvar.glyph_deltas(*new_gid, &coords, num_points);
        assert_eq!(
            want.len(),
            got.len(),
            "gvar delta count mismatch at old={old_gid} new={new_gid}",
        );
        for (a, b) in want.iter().zip(got.iter()) {
            assert_eq!(a.point, b.point);
            assert!(
                (a.dx - b.dx).abs() <= 1e-3,
                "gvar dx mismatch at old={old_gid} new={new_gid} pt={}",
                a.point
            );
            assert!(
                (a.dy - b.dy).abs() <= 1e-3,
                "gvar dy mismatch at old={old_gid} new={new_gid} pt={}",
                a.point
            );
        }
    }
}

#[test]
fn glyph_bounds_at_coords_match_source_for_kept_gids() {
    // Ties gvar + glyf together: the shifted bbox at heavy weight
    // for a kept gid in the subset must equal the source's shifted
    // bbox for the same gid at the same coord.
    let (face, bytes, gid_map) = subset_to_abc();
    let blob = Blob::from_vec(bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    let coords = face.fvar().unwrap().unwrap().normalize_coords(&[900.0]);
    for (old_gid, new_gid) in &gid_map {
        let Some(want) = face.glyph_bounds_at_coords(*old_gid, &coords).unwrap() else {
            continue;
        };
        let Some(got) = subset_face
            .glyph_bounds_at_coords(*new_gid, &coords)
            .unwrap()
        else {
            continue;
        };
        assert_eq!(
            want, got,
            "glyph_bounds_at_coords mismatch old={old_gid} new={new_gid}",
        );
    }
}

#[test]
fn retain_variations_false_drops_var_tables() {
    let face = rubik_face();
    let gid_a = cmap_lookup(&face, 'A');
    let input = SubsetInput {
        gids: vec![gid_a],
        retain_variations: false,
        ..Default::default()
    };
    let out = subset(&face, &input).expect("subset succeeds");
    let blob = Blob::from_vec(out.bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    assert!(
        subset_face.table_bytes(tag::FVAR).is_err(),
        "fvar should be dropped"
    );
    assert!(
        subset_face.table_bytes(tag::AVAR).is_err(),
        "avar should be dropped"
    );
    assert!(
        subset_face.table_bytes(tag::GVAR).is_err(),
        "gvar should be dropped"
    );
    assert!(
        subset_face.table_bytes(tag::HVAR).is_err(),
        "HVAR should be dropped"
    );
}

#[test]
fn variable_subset_is_deterministic() {
    let face = rubik_face();
    let gid_a = cmap_lookup(&face, 'A');
    let gid_b = cmap_lookup(&face, 'B');
    let gid_c = cmap_lookup(&face, 'C');
    let input = SubsetInput {
        gids: vec![gid_a, gid_b, gid_c],
        ..Default::default()
    };
    let a = subset(&face, &input).unwrap();
    let b = subset(&face, &input).unwrap();
    assert_eq!(a.bytes, b.bytes);
}
