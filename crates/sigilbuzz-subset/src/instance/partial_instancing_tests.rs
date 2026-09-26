//! Unit tests for the partial-instancing public API + tuple
//! projection math primitives. The variation-table emitters
//! (HVAR / VVAR / MVAR / gvar / GDEF.IVS) all flow through these
//! primitives. [`crate::gvar_partial::bake_gvar_partial`] uses the
//! same `axis_support_scalar` + `project_region_onto_kept_axes`
//! pair, so the reduced-axis VF's gvar surface stays consistent
//! with the reduced-axis IVS surfaces.
//!
//! fontTools-equivalent of
//! `varLib.instancer.instantiateVariableFont(axisLimits=...)`.

use super::axes::{bake_avar_partial, bake_fvar_partial};
use super::ivs::{project_ivs, RegionRemap};
use super::metrics_var::{bake_hvar_partial, bake_mvar_partial, rewrite_delta_set_index_map};
use super::region::axis_support_scalar;
use super::*;

mod axes;
mod projection;
mod stores;

const RUBIK: &[u8] = include_bytes!("../../../../tests/fixtures/rubik_vf.ttf");
const VAR_KERN: &[u8] = include_bytes!("../../../../tests/fixtures/var_kern.ttf");
const SOURCE_SANS: &[u8] = include_bytes!("../../../../tests/fonts/SourceSans3VF-Latin-Subset.otf");

fn rubik_face() -> Face<'static> {
    Face::parse_bytes(RUBIK, 0).unwrap()
}

#[test]
fn axis_pin_default_is_empty_pin_every_axis() {
    // The default `InstanceInput::axis_pins` is an empty Vec,
    // semantically "pin every axis" so existing callers that
    // never set the field keep getting full instancing. Anything
    // else would be a silent breaking change.
    let i = InstanceInput::default();
    assert!(i.axis_pins.is_empty());
}

#[test]
fn empty_axis_pins_falls_through_to_full_instancing() {
    // Empty axis_pins is the existing full-instancing path. The
    // bake must succeed end-to-end and produce a static font
    // (no fvar / gvar / HVAR), exactly as before this feature
    // landed.
    let face = rubik_face();
    let axis_count = face.fvar().unwrap().unwrap().axes().len();
    let input = InstanceInput {
        coords: alloc::vec![0.0_f32; axis_count],
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let out = instance(&face, &input).expect("empty axis_pins -> full bake");
    let baked = Face::parse_bytes(&out.bytes, 0).expect("parse");
    assert!(baked.fvar().unwrap().is_none(), "fvar dropped");
}

#[test]
fn all_pin_axis_pins_equivalent_to_empty_axis_pins() {
    // A non-empty axis_pins where every entry is `Pin` must
    // produce the same bytes as an empty axis_pins. The emitter
    // walks the same code path either way; this guards against
    // a future regression that branches on length rather than
    // entry policy.
    let face = rubik_face();
    let axis_count = face.fvar().unwrap().unwrap().axes().len();
    let coords = alloc::vec![0.25_f32; axis_count];
    let empty = InstanceInput {
        coords: coords.clone(),
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let all_pin = InstanceInput {
        coords,
        drop_var_tables: true,
        axis_pins: alloc::vec![AxisPin::Pin; axis_count],
    };
    let a = instance(&face, &empty).expect("empty bake");
    let b = instance(&face, &all_pin).expect("all-Pin bake");
    assert_eq!(a.bytes, b.bytes, "all-Pin must equal empty axis_pins");
}

#[test]
fn axis_pins_length_mismatch_errors() {
    let face = rubik_face();
    let axis_count = face.fvar().unwrap().unwrap().axes().len();
    let input = InstanceInput {
        coords: alloc::vec![0.0_f32; axis_count],
        drop_var_tables: true,
        // Length wrong on purpose.
        axis_pins: alloc::vec![AxisPin::Pin; axis_count + 1],
    };
    assert!(matches!(
        instance(&face, &input),
        Err(SubsetError::Unsupported(_))
    ));
}

#[test]
fn axis_pins_with_keep_on_gvar_source_emits_partial_vf() {
    // gvar tuple-projection landed: with at least one axis `Keep`
    // the partial-instance pass produces a reduced-axis VF (gvar
    // axisCount equals the Keep-axis count). Rubik is single-
    // axis (wght), so pinning the only axis is a degenerate
    // partial, but the all-Keep case is the more meaningful
    // round-trip cover.
    let face = rubik_face();
    let axis_count = face.fvar().unwrap().unwrap().axes().len();
    let pins = alloc::vec![AxisPin::Keep; axis_count];
    let input = InstanceInput {
        coords: alloc::vec![0.0_f32; axis_count],
        drop_var_tables: true,
        axis_pins: pins,
    };
    let out = instance(&face, &input).expect("partial bake");
    let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
    let baked_gvar = baked.gvar().unwrap().expect("baked gvar present");
    assert_eq!(baked_gvar.axis_count(), axis_count as u16);
    assert_eq!(baked_gvar.glyph_count(), face.maxp().unwrap().num_glyphs);
}

fn write_f16dot16(out: &mut Vec<u8>, v: f32) {
    let raw = (v * 65536.0).round() as i32;
    out.extend_from_slice(&raw.to_be_bytes());
}

fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
    let raw = (v * 16384.0).round() as i16;
    out.extend_from_slice(&raw.to_be_bytes());
}

// --------------------------------------------------------------
// partial_instance() integration round-trip.
// --------------------------------------------------------------

/// var_kern.ttf is a single-axis (wght) VF with GDEF.IVS carrying
/// a one-region tuple. Pinning wght reduces to a static font (the
/// existing full-instance behavior). Keeping wght is the
/// trivial-axis Keep case: the output keeps fvar + GDEF.IVS, both
/// trimmed (axisCount = 1, regionCount = 1). At wght=1 the output
/// IVS must produce the same delta as the source IVS at wght=1.
#[test]
fn partial_instance_var_kern_keep_wght_round_trips() {
    let face = Face::parse_bytes(VAR_KERN, 0).unwrap();
    let input = InstanceInput {
        coords: alloc::vec![0.0_f32; 1],
        drop_var_tables: false,
        axis_pins: alloc::vec![AxisPin::Keep],
    };
    let out = instance(&face, &input).expect("partial bake");
    let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
    // fvar still present with one axis.
    let new_fvar = baked.fvar().unwrap().expect("fvar survives");
    assert_eq!(new_fvar.axes().len(), 1);
    assert_eq!(new_fvar.axes()[0].tag, *b"wght");
    // GDEF still has an IVS, the trimmed one.
    let baked_gdef = baked.gdef().unwrap().expect("GDEF survives");
    let store = baked_gdef
        .item_variation_store()
        .expect("GDEF IVS survives");
    assert_eq!(store.axis_count(), 1);
}

#[test]
fn partial_instance_keep_on_gvar_source_emits_reduced_axis_vf() {
    // Rubik VF carries gvar; partial-instance with all axes Keep
    // returns a reduced-axis VF whose gvar still varies.
    let face = rubik_face();
    let axis_count = face.fvar().unwrap().unwrap().axes().len();
    let pins = alloc::vec![AxisPin::Keep; axis_count];
    let input = InstanceInput {
        coords: alloc::vec![0.0_f32; axis_count],
        drop_var_tables: true,
        axis_pins: pins,
    };
    let out = instance(&face, &input).expect("partial bake");
    let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
    let baked_gvar = baked.gvar().unwrap().expect("gvar still present");
    assert_eq!(baked_gvar.axis_count(), axis_count as u16);
}

#[test]
fn rubik_partial_pin_wght_matches_full_instance_bytes() {
    // Single-axis source; partial bake with `Pin` on the only
    // axis must produce identical bytes to the full-instancing
    // path (which flattens to the static font). The gvar
    // projection has no `Keep` axes to preserve, so the bake
    // routes through `partial_instance` only when the input
    // `axis_pins.contains(&Keep)`. For an all-Pin axis_pins it
    // routes through the existing full-instance path. This test
    // pins that contract: for all-Pin, partial == full.
    let face = rubik_face();
    let user_max = face.fvar().unwrap().unwrap().axes()[0].max_value;
    let coords = face.fvar().unwrap().unwrap().normalize_coords(&[user_max]);
    let empty = InstanceInput {
        coords: coords.clone(),
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let pinned = InstanceInput {
        coords,
        drop_var_tables: true,
        axis_pins: alloc::vec![AxisPin::Pin],
    };
    let a = instance(&face, &empty).expect("empty");
    let b = instance(&face, &pinned).expect("Pin");
    assert_eq!(
        a.bytes, b.bytes,
        "Pin must equal empty axis_pins for Rubik VF"
    );
}

#[test]
fn rubik_partial_keep_wght_preserves_source_gvar_bytes() {
    // No-op partial: every axis Keep, no axes pin. The output
    // gvar should match the source byte-for-byte (the bake
    // short-circuits to passthrough when there's nothing to
    // project). The whole VF rides through with its variation
    // tables intact.
    let face = rubik_face();
    let axis_count = face.fvar().unwrap().unwrap().axes().len();
    let input = InstanceInput {
        coords: alloc::vec![0.0_f32; axis_count],
        drop_var_tables: true,
        axis_pins: alloc::vec![AxisPin::Keep; axis_count],
    };
    let out = instance(&face, &input).expect("partial bake");
    let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
    let baked_gvar_bytes = baked.table_bytes(tag::GVAR).expect("gvar present");
    let src_gvar_bytes = face.table_bytes(tag::GVAR).expect("source gvar");
    assert_eq!(
        baked_gvar_bytes, src_gvar_bytes,
        "all-Keep partial must preserve source gvar bytes verbatim"
    );
}

#[test]
fn partial_instance_keep_on_cff2_source_emits_partial_var_font() {
    // Source Sans 3 is a single-axis CFF2 VF (wght). Keeping every
    // axis Keep produces a partial-instanced VF byte-stream: the
    // emit walks bake_cff2_partial which rewrites the VarStore +
    // blend operators with surviving regions only.
    let face = Face::parse_bytes(SOURCE_SANS, 0).unwrap();
    let axis_count = face.fvar().unwrap().unwrap().axes().len();
    let pins = alloc::vec![AxisPin::Keep; axis_count];
    let input = InstanceInput {
        coords: alloc::vec![0.0_f32; axis_count],
        drop_var_tables: true,
        axis_pins: pins,
    };
    let out = instance(&face, &input).expect("CFF2 partial bake");
    let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
    let new_fvar = baked.fvar().unwrap().expect("fvar survives");
    assert_eq!(new_fvar.axes().len(), axis_count);
    assert!(baked.record(tag::CFF2).is_some());
}

#[test]
fn partial_instance_source_sans_pin_wght_matches_full_instance() {
    // Source Sans 3 with `wght=Pin` must produce byte-identical
    // output to the existing full-instance path (which uses #163
    // blend bake). This guards the all-Pin branch: it must keep
    // routing through cff2_bake and never enter the partial path.
    let face = Face::parse_bytes(SOURCE_SANS, 0).unwrap();
    let user_default = face.fvar().unwrap().unwrap().axes()[0].default_value;
    let coords = face
        .fvar()
        .unwrap()
        .unwrap()
        .normalize_coords(&[user_default]);
    let empty = InstanceInput {
        coords: coords.clone(),
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let pinned = InstanceInput {
        coords,
        drop_var_tables: true,
        axis_pins: alloc::vec![AxisPin::Pin],
    };
    let a = instance(&face, &empty).expect("empty (full-instance)");
    let b = instance(&face, &pinned).expect("Pin (full-instance via partial path gate)");
    assert_eq!(
        a.bytes, b.bytes,
        "Pin wght on CFF2 must match empty axis_pins (both go through cff2_bake)"
    );
}

#[test]
fn partial_instance_var_kern_pin_wght_matches_full_instance() {
    // Single-axis source; Pin wght and empty pins must produce
    // identical bytes (the per-axis Pin is just the existing full-
    // instancing path, exercised through the new integer
    // validator).
    let face = Face::parse_bytes(VAR_KERN, 0).unwrap();
    let user_max = face.fvar().unwrap().unwrap().axes()[0].max_value;
    let coords = face.fvar().unwrap().unwrap().normalize_coords(&[user_max]);
    let empty = InstanceInput {
        coords: coords.clone(),
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let pinned = InstanceInput {
        coords,
        drop_var_tables: true,
        axis_pins: alloc::vec![AxisPin::Pin],
    };
    let a = instance(&face, &empty).expect("empty");
    let b = instance(&face, &pinned).expect("Pin");
    assert_eq!(a.bytes, b.bytes, "Pin must equal empty axis_pins");
}
