//! Tuple projection for partial instancing: the support scalar of one
//! axis, and a variation region projected onto the kept axes.

use super::AxisPin;

// ---------------------------------------------------------------------------
// Partial-instancing tuple projection math.
//
// Each variation tuple is a region defined by per-axis (peak, start, end)
// triples plus a delta payload. To project a tuple onto a Keep-axis
// subspace:
//
//   1. Resolve every Pin-axis dimension to a constant scalar at
//      `coords[i]` (the OpenType `supportScalar`-style ramp the
//      shaper already uses to evaluate variation tables).
//   2. Multiply the per-Pin-axis scalars together. If the product is
//      zero (meaning the pin coord falls outside the tuple's region
//      on at least one Pin-axis), the tuple contributes nothing at
//      this pin and gets dropped.
//   3. Otherwise the survivor tuple keeps only the Keep-axis dimensions
//      of its region triples; its delta payload is multiplied by the
//      Pin-axis product so that evaluating the trimmed tuple at the
//      Keep-axis coords reproduces the source tuple's contribution
//      exactly at every (Keep-coord, Pin-coord) pair where the Pin
//      coord matches `coords[i]`.
//
// These primitives are the building blocks the variation-table
// rewriters (HVAR / VVAR / MVAR / gvar / GDEF.IVS) consume to emit
// trimmed `ItemVariationStore` / gvar tuples in a partial-instance
// font. They are tested in isolation here so the math stays correct
// independently of the table rewriters that use them.
// ---------------------------------------------------------------------------

/// Computes the support-scalar contribution of a single axis dimension
/// at `coord`. Mirrors the OpenType `supportScalar` formula used by
/// the gvar / IVS evaluators in `sigilbuzz::tables::gvar`,
/// re-implemented here because the subset crate cannot import
/// crate-private helpers from the parent crate, and the formula is
/// trivially small.
///
/// Returns `1.0` when the axis does not participate in the tuple
/// (peak == 0 with the spec's "axis ignored" convention), or when its
/// region is invalid (start past the peak, the peak past the end, or a
/// region that crosses zero), which the spec and HarfBuzz ignore too;
/// and `0.0` when `coord` falls outside `[start, end]`.
#[must_use]
pub(crate) fn axis_support_scalar(start: f32, peak: f32, end: f32, coord: f32) -> f32 {
    // Hardening (#185): any non-finite input returns 0. The axis is
    // treated as outside this region. This matches HarfBuzz's
    // hb_array_t::evaluate clamping behavior and prevents NaN/Inf from
    // propagating into the per-tuple scalar product downstream.
    if !coord.is_finite() || !peak.is_finite() || !start.is_finite() || !end.is_finite() {
        return 0.0;
    }
    // Spec: peak of zero means the axis does not participate.
    if peak == 0.0 {
        return 1.0;
    }
    if start > peak || peak > end || (start < 0.0 && end > 0.0) {
        return 1.0;
    }
    if (coord - peak).abs() < f32::EPSILON {
        return 1.0;
    }
    if coord < start || coord > end {
        return 0.0;
    }
    if coord < peak {
        let denom = peak - start;
        if denom.abs() < f32::EPSILON {
            return 0.0;
        }
        (coord - start) / denom
    } else {
        // coord > peak
        let denom = end - peak;
        if denom.abs() < f32::EPSILON {
            return 0.0;
        }
        (end - coord) / denom
    }
}

/// One tuple region's per-axis (start, peak, end) triple, in the
/// source font's axis order. Length must equal the source's fvar axis
/// count.
pub(crate) type RegionAxes = [(f32, f32, f32)];

/// Result of projecting a variation tuple onto its Keep-axis subspace.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProjectedTuple {
    /// Pin-axis support-scalar product evaluated at the pin coords.
    /// Caller multiplies every delta in this tuple's payload by this
    /// scalar before emitting the trimmed tuple.
    pub pin_scalar: f32,
    /// Trimmed region triples: only the Keep-axis dimensions, in the
    /// source's Keep-axis order.
    pub kept_axes: alloc::vec::Vec<(f32, f32, f32)>,
}

/// Projects a tuple's per-axis region onto the Keep-axis subspace.
///
/// `region` carries one `(start, peak, end)` triple per source axis.
/// `pins` indicates which axes pin (`Pin`) and which stay variable
/// (`Keep`); `coords` carries the pin coord for every axis (entries
/// for `Keep` axes are ignored).
///
/// Returns `None` when the tuple contributes nothing at the pin coords
/// meaning the survivor would have a zero pin-scalar and the caller should
/// drop the tuple entirely. Returns `Some(ProjectedTuple)` otherwise.
///
/// Lengths must agree: `region.len() == pins.len() == coords.len()`.
/// Mismatched inputs return `None` (defensive: callers should validate
/// upstream, but a length skew should not produce silently-wrong deltas).
#[must_use]
pub(crate) fn project_region_onto_kept_axes(
    region: &RegionAxes,
    pins: &[AxisPin],
    coords: &[f32],
) -> Option<ProjectedTuple> {
    if region.len() != pins.len() || pins.len() != coords.len() {
        return None;
    }
    let mut pin_scalar: f32 = 1.0;
    let mut kept_axes: alloc::vec::Vec<(f32, f32, f32)> =
        alloc::vec::Vec::with_capacity(pins.len());
    for (i, &pin) in pins.iter().enumerate() {
        let (s, p, e) = region[i];
        match pin {
            AxisPin::Pin => {
                let s_axis = axis_support_scalar(s, p, e, coords[i]);
                // Hardening (#186): any non-finite scalar from the
                // pipeline drops the tuple. axis_support_scalar already
                // clamps non-finite inputs to 0.0, but the multiplication
                // chain itself is checked here defensively so any future
                // upstream change can never quietly poison deltas.
                if !s_axis.is_finite() || s_axis == 0.0 {
                    return None;
                }
                pin_scalar *= s_axis;
                // Subnormal underflow short-circuit: if the running
                // product collapsed to 0 (or went non-finite somehow),
                // drop the tuple now.
                if !pin_scalar.is_finite() || pin_scalar == 0.0 {
                    return None;
                }
            }
            AxisPin::Keep => {
                kept_axes.push((s, p, e));
            }
        }
    }
    if !pin_scalar.is_finite() || pin_scalar == 0.0 {
        return None;
    }
    Some(ProjectedTuple {
        pin_scalar,
        kept_axes,
    })
}
