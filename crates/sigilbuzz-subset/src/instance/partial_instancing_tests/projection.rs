//! Tuple projection tests: the per-axis support scalar and a region
//! projected onto the kept axes, finite and not.

use super::*;

// --------------------------------------------------------------
// axis_support_scalar: single-axis ramp matches OpenType spec.
// --------------------------------------------------------------

#[test]
fn axis_support_scalar_peak_returns_one() {
    // At the peak the scalar is 1.
    assert!((axis_support_scalar(0.0, 1.0, 1.0, 1.0) - 1.0).abs() < 1e-6);
    assert!((axis_support_scalar(-1.0, -1.0, 0.0, -1.0) - 1.0).abs() < 1e-6);
}

#[test]
fn axis_support_scalar_zero_peak_means_axis_ignored() {
    // Per spec a peak of zero means the axis does not participate
    // in the tuple. The scalar is 1 regardless of coord.
    assert!((axis_support_scalar(0.0, 0.0, 0.0, 0.5) - 1.0).abs() < 1e-6);
    assert!((axis_support_scalar(-1.0, 0.0, 1.0, 0.5) - 1.0).abs() < 1e-6);
}

#[test]
fn axis_support_scalar_outside_region_returns_zero() {
    // coord beyond [start, end] -> zero contribution.
    assert!(axis_support_scalar(0.0, 1.0, 1.0, -0.5).abs() < 1e-6);
    assert!(axis_support_scalar(0.0, 1.0, 1.0, 1.1).abs() < 1e-6);
}

#[test]
fn axis_support_scalar_linear_ramp_below_peak() {
    // start=0, peak=1, end=1: coord=0.5 is halfway up the ramp.
    assert!((axis_support_scalar(0.0, 1.0, 1.0, 0.5) - 0.5).abs() < 1e-6);
    // 0.25 quarter up.
    assert!((axis_support_scalar(0.0, 1.0, 1.0, 0.25) - 0.25).abs() < 1e-6);
}

#[test]
fn axis_support_scalar_linear_ramp_above_peak() {
    // start=0, peak=0.5, end=1: coord=0.75 ramps down from 1 at
    // peak to 0 at end. Halfway -> 0.5.
    // (peak == 0 would short-circuit to 1.0 per the spec's
    // "axis ignored" convention; we use a non-zero peak here.)
    assert!((axis_support_scalar(0.0, 0.5, 1.0, 0.75) - 0.5).abs() < 1e-6);
}

#[test]
fn axis_support_scalar_ignores_invalid_regions() {
    // A region whose start passes its peak, whose peak passes its end,
    // or that crosses zero is invalid; the spec and HarfBuzz ignore the
    // axis, so it scales by 1 wherever the coordinate is.
    for (start, peak, end) in [(0.8, 0.5, 1.0), (0.0, 1.0, 0.5), (-1.0, 0.5, 1.0)] {
        for coord in [-1.0, 0.0, 0.3, 0.75, 1.0] {
            assert_eq!(axis_support_scalar(start, peak, end, coord), 1.0);
        }
    }
}

#[test]
fn axis_support_scalar_degenerate_peak_eq_start_returns_zero() {
    // peak == start, coord between them -> division by zero
    // guarded with a 0.0 fallback.
    assert!(axis_support_scalar(1.0, 1.0, 1.0, 0.5).abs() < 1e-6);
}

// --------------------------------------------------------------
// axis_support_scalar: non-finite inputs are clamped to 0.0
// (regression #185). Matches HarfBuzz hb_array_t::evaluate.
// --------------------------------------------------------------

#[test]
fn axis_support_scalar_nan_coord_returns_zero() {
    // NaN coord -> axis is "outside the region": scalar 0.
    assert_eq!(axis_support_scalar(0.0, 1.0, 1.0, f32::NAN), 0.0);
}

#[test]
fn axis_support_scalar_inf_coord_returns_zero() {
    // +Inf and -Inf coords are both clamped to scalar 0.
    assert_eq!(axis_support_scalar(0.0, 1.0, 1.0, f32::INFINITY), 0.0);
    assert_eq!(axis_support_scalar(0.0, 1.0, 1.0, f32::NEG_INFINITY), 0.0);
}

#[test]
fn axis_support_scalar_nan_peak_returns_zero() {
    // NaN peak: the region itself is corrupt; clamp to 0.
    assert_eq!(axis_support_scalar(0.0, f32::NAN, 1.0, 0.5), 0.0);
}

#[test]
fn axis_support_scalar_nan_or_inf_endpoints_return_zero() {
    // Non-finite start or end: clamp to 0.
    assert_eq!(axis_support_scalar(f32::NAN, 1.0, 1.0, 0.5), 0.0);
    assert_eq!(axis_support_scalar(0.0, 1.0, f32::NAN, 0.5), 0.0);
    assert_eq!(axis_support_scalar(f32::NEG_INFINITY, 1.0, 1.0, 0.5), 0.0);
    assert_eq!(axis_support_scalar(0.0, 1.0, f32::INFINITY, 0.5), 0.0);
}

#[test]
fn axis_support_scalar_degenerate_region_at_peak_returns_one() {
    // start == end == peak == coord: the spec's degenerate region
    // collapses to a point and the coord lands on it -> scalar 1.
    // (Without the (coord - peak).abs() < EPSILON short-circuit
    // this would divide by zero and produce NaN.)
    assert!((axis_support_scalar(0.5, 0.5, 0.5, 0.5) - 1.0).abs() < 1e-6);
}

#[test]
fn axis_support_scalar_start_eq_peak_below_peak_returns_zero() {
    // start == peak == 0.5, end == 1.0; coord = 0.4 falls below
    // start so the outside-region branch returns 0.0 (no divide
    // by zero on the up-ramp denominator).
    assert_eq!(axis_support_scalar(0.5, 0.5, 1.0, 0.4), 0.0);
}

// --------------------------------------------------------------
// project_region_onto_kept_axes: full tuple projection.
// --------------------------------------------------------------

#[test]
fn project_two_axis_region_pin_first_keep_second() {
    // Two axes (wght + wdth). Region: wght (0, 1, 1), wdth (0, 1, 1).
    // Pin wght=0.5 (scalar 0.5), keep wdth.
    let region = [(0.0_f32, 1.0, 1.0), (0.0, 1.0, 1.0)];
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let coords = [0.5, 0.0];
    let p = project_region_onto_kept_axes(&region, &pins, &coords).expect("survives");
    assert!((p.pin_scalar - 0.5).abs() < 1e-6);
    assert_eq!(p.kept_axes, alloc::vec![(0.0, 1.0, 1.0)]);
}

#[test]
fn project_drops_tuple_when_pin_falls_outside_region() {
    // Pin coord 0.0 falls outside the wght region [0.5, 1.0]:
    // the scalar is zero and the tuple gets dropped.
    let region = [(0.5_f32, 1.0, 1.0), (0.0, 1.0, 1.0)];
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let coords = [0.0, 0.0];
    assert!(project_region_onto_kept_axes(&region, &pins, &coords).is_none());
}

#[test]
fn project_pin_at_peak_passes_kept_axes_through_at_unit_scalar() {
    // Pin axis at peak -> scalar 1, kept axes ride through.
    let region = [(0.0_f32, 1.0, 1.0), (-1.0, -1.0, 0.0)];
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let coords = [1.0, 0.0];
    let p = project_region_onto_kept_axes(&region, &pins, &coords).unwrap();
    assert!((p.pin_scalar - 1.0).abs() < 1e-6);
    assert_eq!(p.kept_axes, alloc::vec![(-1.0, -1.0, 0.0)]);
}

#[test]
fn project_all_pin_yields_empty_kept_axes() {
    // Every axis pinned: kept_axes is empty (the survivor tuple
    // becomes a plain delta-set with no region dimensions).
    let region = [(0.0_f32, 1.0, 1.0), (0.0, 1.0, 1.0)];
    let pins = [AxisPin::Pin, AxisPin::Pin];
    let coords = [0.5, 0.5];
    let p = project_region_onto_kept_axes(&region, &pins, &coords).unwrap();
    // Two ramps at 0.5 each -> 0.25 product.
    assert!((p.pin_scalar - 0.25).abs() < 1e-6);
    assert!(p.kept_axes.is_empty());
}

#[test]
fn project_all_keep_yields_unit_scalar_full_kept_axes() {
    // Every axis kept variable: scalar 1, kept_axes = source region.
    let region = [(0.0_f32, 1.0, 1.0), (-1.0, -0.5, 0.0)];
    let pins = [AxisPin::Keep, AxisPin::Keep];
    let coords = [0.0, 0.0]; // ignored
    let p = project_region_onto_kept_axes(&region, &pins, &coords).unwrap();
    assert!((p.pin_scalar - 1.0).abs() < 1e-6);
    assert_eq!(p.kept_axes, alloc::vec![(0.0, 1.0, 1.0), (-1.0, -0.5, 0.0)]);
}

#[test]
fn project_zero_peak_pin_axis_passes_scalar_through() {
    // Pin-axis with peak == 0 (axis-doesn't-participate): scalar
    // contribution is 1 regardless of coord, so the survivor
    // carries through with no payload scaling.
    let region = [(0.0_f32, 0.0, 0.0), (0.0, 1.0, 1.0)];
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let coords = [0.5, 0.0];
    let p = project_region_onto_kept_axes(&region, &pins, &coords).unwrap();
    assert!((p.pin_scalar - 1.0).abs() < 1e-6);
    assert_eq!(p.kept_axes, alloc::vec![(0.0, 1.0, 1.0)]);
}

#[test]
fn project_length_mismatch_returns_none() {
    // Defensive: mismatched input lengths return None rather than
    // panicking on an OOB index.
    let region = [(0.0_f32, 1.0, 1.0)];
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let coords = [0.5, 0.5];
    assert!(project_region_onto_kept_axes(&region, &pins, &coords).is_none());
}

#[test]
fn project_two_pin_axes_multiplies_scalars() {
    // Both Pin axes contribute partial ramps; the survivor's
    // pin_scalar is their product (0.5 * 0.25 = 0.125).
    let region = [(0.0_f32, 1.0, 1.0), (0.0, 1.0, 1.0)];
    let pins = [AxisPin::Pin, AxisPin::Pin];
    let coords = [0.5, 0.25];
    let p = project_region_onto_kept_axes(&region, &pins, &coords).unwrap();
    assert!(
        (p.pin_scalar - 0.125).abs() < 1e-6,
        "expected 0.125, got {}",
        p.pin_scalar
    );
}

// --------------------------------------------------------------
// project_region_onto_kept_axes: non-finite Pin-axis inputs are
// clamped: the surviving tuple gets dropped rather than scaling
// every delta by NaN/Inf (regression #186).
// --------------------------------------------------------------

#[test]
fn project_drops_tuple_when_pin_coord_is_nan() {
    // NaN Pin coord poisons the scalar pipeline: drop the tuple
    // rather than emitting deltas multiplied by NaN.
    let region = [(0.0_f32, 1.0, 1.0), (0.0, 1.0, 1.0)];
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let coords = [f32::NAN, 0.0];
    assert!(project_region_onto_kept_axes(&region, &pins, &coords).is_none());
}

#[test]
fn project_drops_tuple_when_pin_coord_is_inf() {
    // +Inf and -Inf Pin coords also drop the tuple.
    let region = [(0.0_f32, 1.0, 1.0), (0.0, 1.0, 1.0)];
    let pins = [AxisPin::Pin, AxisPin::Keep];
    for coord in [f32::INFINITY, f32::NEG_INFINITY] {
        let coords = [coord, 0.0];
        assert!(
            project_region_onto_kept_axes(&region, &pins, &coords).is_none(),
            "expected drop for coord {coord}",
        );
    }
}

#[test]
fn project_drops_tuple_when_pin_axis_region_is_nan() {
    // Corrupt region triple (NaN peak) on a Pin axis: drop the
    // tuple. The math primitive returns 0.0 for non-finite
    // inputs and project_region_onto_kept_axes treats that as
    // "axis outside the region".
    let region = [(0.0_f32, f32::NAN, 1.0), (0.0, 1.0, 1.0)];
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let coords = [0.5, 0.0];
    assert!(project_region_onto_kept_axes(&region, &pins, &coords).is_none());
}

#[test]
fn project_ignores_nan_coord_on_keep_axis() {
    // Keep-axis coords are unused by the scalar pipeline; a NaN
    // there must not poison the projection. The Pin axis still
    // produces a clean scalar and the tuple survives.
    let region = [(0.0_f32, 1.0, 1.0), (0.0, 1.0, 1.0)];
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let coords = [0.5, f32::NAN];
    let p = project_region_onto_kept_axes(&region, &pins, &coords)
        .expect("Keep-axis coord is ignored, tuple should survive");
    assert!((p.pin_scalar - 0.5).abs() < 1e-6);
    assert_eq!(p.kept_axes, alloc::vec![(0.0, 1.0, 1.0)]);
}
