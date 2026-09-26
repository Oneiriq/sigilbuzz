//! Tests for the `BASE` parser: axes, scripts, baselines, min/max
//! extents and v1.1 variable baselines.

use super::*;

fn u16be(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}
fn i16be(v: i16) -> [u8; 2] {
    v.to_be_bytes()
}

/// Builds a minimal BASE v1.0 with one horizontal axis carrying
/// one script (`latn`) and one baseline tag (`romn`). When
/// `min_max` is `Some`, also embeds a default MinMax with those
/// (min, max) bounds.
fn build_minimal_base(min_max: Option<(i16, i16)>, baseline_y: i16) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();

    // Header.
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(8));
    out.extend_from_slice(&u16be(0));

    // Horizontal axis (offset 8).
    let axis_off = out.len();
    let tag_list_slot = axis_off;
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(0));

    let tag_list_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(b"romn");

    let script_list_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(b"latn");
    let script_off_slot = out.len();
    out.extend_from_slice(&u16be(0));

    let script_off = out.len();
    let base_values_slot = out.len();
    out.extend_from_slice(&u16be(0));
    let default_min_max_slot = out.len();
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(0));

    let base_values_off = out.len();
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(1));
    let coord_off_slot = out.len();
    out.extend_from_slice(&u16be(0));

    let coord_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(&i16be(baseline_y));

    let min_max_off = if let Some((mn, mx)) = min_max {
        let mm_off = out.len();
        let mm_min_slot = out.len();
        out.extend_from_slice(&u16be(0));
        let mm_max_slot = out.len();
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(0));

        let min_coord_off = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(&i16be(mn));
        let max_coord_off = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(&i16be(mx));

        out[mm_min_slot..mm_min_slot + 2].copy_from_slice(&u16be((min_coord_off - mm_off) as u16));
        out[mm_max_slot..mm_max_slot + 2].copy_from_slice(&u16be((max_coord_off - mm_off) as u16));
        Some(mm_off)
    } else {
        None
    };

    out[tag_list_slot..tag_list_slot + 2].copy_from_slice(&u16be((tag_list_off - axis_off) as u16));
    out[tag_list_slot + 2..tag_list_slot + 4]
        .copy_from_slice(&u16be((script_list_off - axis_off) as u16));
    out[script_off_slot..script_off_slot + 2]
        .copy_from_slice(&u16be((script_off - script_list_off) as u16));
    out[base_values_slot..base_values_slot + 2]
        .copy_from_slice(&u16be((base_values_off - script_off) as u16));
    out[coord_off_slot..coord_off_slot + 2]
        .copy_from_slice(&u16be((coord_off - base_values_off) as u16));
    if let Some(mm_off) = min_max_off {
        out[default_min_max_slot..default_min_max_slot + 2]
            .copy_from_slice(&u16be((mm_off - script_off) as u16));
    }
    out
}

#[test]
fn parses_minimal_base_table() {
    let data = build_minimal_base(None, 0);
    let base = Base::parse(&data).unwrap();
    assert!(base.horizontal_axis().is_some());
    assert!(base.vertical_axis().is_none());
}

#[test]
fn rejects_unsupported_major_version() {
    let mut data = build_minimal_base(None, 0);
    data[0..2].copy_from_slice(&2u16.to_be_bytes());
    assert!(matches!(Base::parse(&data), Err(Error::Malformed { .. })));
}

#[test]
fn axis_returns_baseline_tags_in_order() {
    let data = build_minimal_base(None, 0);
    let base = Base::parse(&data).unwrap();
    let axis = base.horizontal_axis().unwrap();
    assert_eq!(axis.baseline_tags(), alloc::vec![*b"romn"]);
}

#[test]
fn axis_returns_none_for_unknown_script() {
    let data = build_minimal_base(None, 0);
    let base = Base::parse(&data).unwrap();
    let axis = base.horizontal_axis().unwrap();
    assert!(axis.script(*b"hang").is_none());
}

#[test]
fn script_returns_baseline_for_known_tag() {
    let data = build_minimal_base(None, 100);
    let base = Base::parse(&data).unwrap();
    let axis = base.horizontal_axis().unwrap();
    let script = axis.script(*b"latn").unwrap();
    assert_eq!(script.baseline(*b"romn"), Some(100));
    assert_eq!(script.baseline(*b"hang"), None);
}

#[test]
fn script_returns_min_max_when_present() {
    let data = build_minimal_base(Some((-200, 800)), 0);
    let base = Base::parse(&data).unwrap();
    let axis = base.horizontal_axis().unwrap();
    let script = axis.script(*b"latn").unwrap();
    assert_eq!(script.min_max(None), Some((-200, 800)));
}

#[test]
fn script_returns_none_for_min_max_when_absent() {
    let data = build_minimal_base(None, 0);
    let base = Base::parse(&data).unwrap();
    let axis = base.horizontal_axis().unwrap();
    let script = axis.script(*b"latn").unwrap();
    assert!(script.min_max(None).is_none());
}

#[test]
fn missing_axis_offset_yields_none() {
    let mut data = Vec::new();
    data.extend_from_slice(&u16be(1));
    data.extend_from_slice(&u16be(0));
    data.extend_from_slice(&u16be(0));
    data.extend_from_slice(&u16be(0));
    let base = Base::parse(&data).unwrap();
    assert!(base.horizontal_axis().is_none());
    assert!(base.vertical_axis().is_none());
}

/// Multi-tag, multi-script BASE: two baseline tags (`romn`,
/// `ideo`) and two scripts (`latn` and `hani`) carrying
/// distinct y-coords for both tags. Exercises the slot-into-
/// tag-list lookup and the script scan.
fn build_multi_base() -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(8));
    out.extend_from_slice(&u16be(0));

    let axis_off = out.len();
    let tag_list_slot = axis_off;
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(0));

    let tag_list_off = out.len();
    out.extend_from_slice(&u16be(2));
    out.extend_from_slice(b"romn");
    out.extend_from_slice(b"ideo");

    let script_list_off = out.len();
    out.extend_from_slice(&u16be(2));
    out.extend_from_slice(b"latn");
    let latn_off_slot = out.len();
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(b"hani");
    let hani_off_slot = out.len();
    out.extend_from_slice(&u16be(0));

    let latn_script_off = out.len();
    let latn_bv_slot = out.len();
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(0));

    let latn_bv_off = out.len();
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(2));
    let latn_c0_slot = out.len();
    out.extend_from_slice(&u16be(0));
    let latn_c1_slot = out.len();
    out.extend_from_slice(&u16be(0));

    let latn_c0_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(&i16be(0));
    let latn_c1_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(&i16be(-120));

    let hani_script_off = out.len();
    let hani_bv_slot = out.len();
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(0));

    let hani_bv_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(&u16be(2));
    let hani_c0_slot = out.len();
    out.extend_from_slice(&u16be(0));
    let hani_c1_slot = out.len();
    out.extend_from_slice(&u16be(0));

    let hani_c0_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(&i16be(120));
    let hani_c1_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(&i16be(0));

    out[tag_list_slot..tag_list_slot + 2].copy_from_slice(&u16be((tag_list_off - axis_off) as u16));
    out[tag_list_slot + 2..tag_list_slot + 4]
        .copy_from_slice(&u16be((script_list_off - axis_off) as u16));
    out[latn_off_slot..latn_off_slot + 2]
        .copy_from_slice(&u16be((latn_script_off - script_list_off) as u16));
    out[hani_off_slot..hani_off_slot + 2]
        .copy_from_slice(&u16be((hani_script_off - script_list_off) as u16));
    out[latn_bv_slot..latn_bv_slot + 2]
        .copy_from_slice(&u16be((latn_bv_off - latn_script_off) as u16));
    out[hani_bv_slot..hani_bv_slot + 2]
        .copy_from_slice(&u16be((hani_bv_off - hani_script_off) as u16));
    out[latn_c0_slot..latn_c0_slot + 2].copy_from_slice(&u16be((latn_c0_off - latn_bv_off) as u16));
    out[latn_c1_slot..latn_c1_slot + 2].copy_from_slice(&u16be((latn_c1_off - latn_bv_off) as u16));
    out[hani_c0_slot..hani_c0_slot + 2].copy_from_slice(&u16be((hani_c0_off - hani_bv_off) as u16));
    out[hani_c1_slot..hani_c1_slot + 2].copy_from_slice(&u16be((hani_c1_off - hani_bv_off) as u16));
    out
}

#[test]
fn multi_script_baseline_lookup() {
    let data = build_multi_base();
    let base = Base::parse(&data).unwrap();
    let axis = base.horizontal_axis().unwrap();
    assert_eq!(axis.baseline_tags(), alloc::vec![*b"romn", *b"ideo"]);

    let latn = axis.script(*b"latn").unwrap();
    assert_eq!(latn.baseline(*b"romn"), Some(0));
    assert_eq!(latn.baseline(*b"ideo"), Some(-120));

    let hani = axis.script(*b"hani").unwrap();
    assert_eq!(hani.baseline(*b"romn"), Some(120));
    assert_eq!(hani.baseline(*b"ideo"), Some(0));
}

/// MinMax with a per-feature override: `sups` (superscripts)
/// gets a tighter clamp than the script default; an unknown
/// feature tag falls back to the default range.
fn build_base_with_feature_minmax() -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(8));
    out.extend_from_slice(&u16be(0));

    let axis_off = out.len();
    let tl_slot = axis_off;
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(0));

    let tag_list_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(b"romn");

    let script_list_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(b"latn");
    let s_slot = out.len();
    out.extend_from_slice(&u16be(0));

    let script_off = out.len();
    out.extend_from_slice(&u16be(0));
    let mm_slot = out.len();
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(0));

    let mm_off = out.len();
    let mm_min_slot = out.len();
    out.extend_from_slice(&u16be(0));
    let mm_max_slot = out.len();
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(b"sups");
    let f_min_slot = out.len();
    out.extend_from_slice(&u16be(0));
    let f_max_slot = out.len();
    out.extend_from_slice(&u16be(0));

    let dmin = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(&i16be(-200));
    let dmax = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(&i16be(800));
    let fmin = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(&i16be(-50));
    let fmax = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(&i16be(600));

    out[tl_slot..tl_slot + 2].copy_from_slice(&u16be((tag_list_off - axis_off) as u16));
    out[tl_slot + 2..tl_slot + 4].copy_from_slice(&u16be((script_list_off - axis_off) as u16));
    out[s_slot..s_slot + 2].copy_from_slice(&u16be((script_off - script_list_off) as u16));
    out[mm_slot..mm_slot + 2].copy_from_slice(&u16be((mm_off - script_off) as u16));
    out[mm_min_slot..mm_min_slot + 2].copy_from_slice(&u16be((dmin - mm_off) as u16));
    out[mm_max_slot..mm_max_slot + 2].copy_from_slice(&u16be((dmax - mm_off) as u16));
    out[f_min_slot..f_min_slot + 2].copy_from_slice(&u16be((fmin - mm_off) as u16));
    out[f_max_slot..f_max_slot + 2].copy_from_slice(&u16be((fmax - mm_off) as u16));
    out
}

#[test]
fn min_max_feature_override_wins_for_known_tag() {
    let data = build_base_with_feature_minmax();
    let base = Base::parse(&data).unwrap();
    let axis = base.horizontal_axis().unwrap();
    let script = axis.script(*b"latn").unwrap();
    assert_eq!(script.min_max(None), Some((-200, 800)));
    assert_eq!(script.min_max(Some(*b"sups")), Some((-50, 600)));
    assert_eq!(script.min_max(Some(*b"subs")), Some((-200, 800)));
}

// --------------------------------------------------------------
// v1.1 IVS-varied baseline (BaseCoord format 3).
// --------------------------------------------------------------

fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
    #[allow(clippy::cast_possible_truncation)]
    let raw = (v * 16384.0).round() as i16;
    out.extend_from_slice(&raw.to_be_bytes());
}

/// Same one-axis, one-region, one-item IVS used by the
/// `mvar`/`hvar` tests. Maps `(outer=0, inner=0)` to the given
/// `delta` at the +1.0 axis tip, tapering linearly to 0 at
/// the default position.
fn build_ivs_one_item(delta: i16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    let region_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // subtable count
    let subtable_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());

    let region_start = out.len() as u32;
    out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_start.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
    out.extend_from_slice(&1u16.to_be_bytes()); // regionCount
    write_f2dot14(&mut out, 0.0);
    write_f2dot14(&mut out, 1.0);
    write_f2dot14(&mut out, 1.0);

    let sub_start = out.len() as u32;
    out[subtable_slot..subtable_slot + 4].copy_from_slice(&sub_start.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // itemCount
    out.extend_from_slice(&1u16.to_be_bytes()); // wordDeltaCount
    out.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
    out.extend_from_slice(&0u16.to_be_bytes()); // region index 0
    out.extend_from_slice(&delta.to_be_bytes());
    out
}

/// Builds a v1.1 BASE with one horizontal axis, one script
/// (`latn`), one tag (`romn`), and a format-3 BaseCoord whose
/// VariationIndex points at the single `(outer=0, inner=0)`
/// item in the embedded IVS.
fn build_v11_base(static_y: i16, ivs_delta: i16) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();

    // Header (v1.1: 12 bytes).
    out.extend_from_slice(&u16be(1)); // major
    out.extend_from_slice(&u16be(1)); // minor
    out.extend_from_slice(&u16be(12)); // horizAxisOffset
    out.extend_from_slice(&u16be(0));
    let ivs_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // itemVarStoreOffset placeholder

    let axis_off = out.len();
    let tl_slot = axis_off;
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(0));

    let tag_list_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(b"romn");

    let script_list_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(b"latn");
    let s_slot = out.len();
    out.extend_from_slice(&u16be(0));

    let script_off = out.len();
    let bv_slot = out.len();
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(0));

    let bv_off = out.len();
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(1));
    let coord_slot = out.len();
    out.extend_from_slice(&u16be(0));

    // Format 3 BaseCoord: u16 fmt, i16 coord, Offset16 device.
    let coord_off = out.len();
    out.extend_from_slice(&u16be(3));
    out.extend_from_slice(&i16be(static_y));
    let dev_slot = out.len();
    out.extend_from_slice(&u16be(0));

    // VariationIndex (inline, pointed at by dev_slot).
    let var_index_off = out.len();
    out.extend_from_slice(&u16be(0)); // outer
    out.extend_from_slice(&u16be(0)); // inner
    out.extend_from_slice(&u16be(0x8000)); // deltaFormat = VARIATION_INDEX

    out[dev_slot..dev_slot + 2].copy_from_slice(&u16be((var_index_off - coord_off) as u16));

    out[tl_slot..tl_slot + 2].copy_from_slice(&u16be((tag_list_off - axis_off) as u16));
    out[tl_slot + 2..tl_slot + 4].copy_from_slice(&u16be((script_list_off - axis_off) as u16));
    out[s_slot..s_slot + 2].copy_from_slice(&u16be((script_off - script_list_off) as u16));
    out[bv_slot..bv_slot + 2].copy_from_slice(&u16be((bv_off - script_off) as u16));
    out[coord_slot..coord_slot + 2].copy_from_slice(&u16be((coord_off - bv_off) as u16));

    let ivs_off = out.len() as u32;
    out[ivs_slot..ivs_slot + 4].copy_from_slice(&ivs_off.to_be_bytes());
    out.extend_from_slice(&build_ivs_one_item(ivs_delta));

    out
}

#[test]
fn v11_static_baseline_is_unchanged_at_default_coord() {
    let data = build_v11_base(50, 30);
    let base = Base::parse(&data).unwrap();
    assert!(base.variation_store().is_some());
    let axis = base.horizontal_axis().unwrap();
    let script = axis.script(*b"latn").unwrap();
    // baseline() always returns the static coord.
    assert_eq!(script.baseline(*b"romn"), Some(50));
    // At coord 0 the IVS region (0..1..1) yields scalar 0, so no
    // delta.
    assert_eq!(script.baseline_at_coords(*b"romn", &[0.0]), Some(50));
}

#[test]
fn v11_baseline_picks_up_ivs_delta_at_max_coord() {
    let data = build_v11_base(50, 30);
    let base = Base::parse(&data).unwrap();
    let axis = base.horizontal_axis().unwrap();
    let script = axis.script(*b"latn").unwrap();
    // At coord 1.0 the region yields scalar 1.0 and the delta
    // is 30, so 50 + 30 = 80.
    assert_eq!(script.baseline_at_coords(*b"romn", &[1.0]), Some(80));
    // At coord 0.5 the linear taper gives 50 + 15 = 65.
    assert_eq!(script.baseline_at_coords(*b"romn", &[0.5]), Some(65));
}

#[test]
fn v11_baseline_at_coords_without_ivs_returns_static() {
    // A v1.0 fixture has no IVS; baseline_at_coords should
    // still return the static coord rather than failing.
    let data = build_minimal_base(None, 42);
    let base = Base::parse(&data).unwrap();
    let axis = base.horizontal_axis().unwrap();
    let script = axis.script(*b"latn").unwrap();
    assert_eq!(script.baseline_at_coords(*b"romn", &[0.5]), Some(42));
}
