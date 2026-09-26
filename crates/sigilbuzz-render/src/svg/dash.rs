//! `stroke-dasharray`: parsing the dash list and walking it along a
//! flattened polyline.

use alloc::vec::Vec;

#[cfg(test)]
use super::MAX_DASH_SPLITS;

// =========================================================================
// Stroke dasharray
// =========================================================================

/// Parses a `stroke-dasharray` attribute body. Empty / `none` /
/// all-zero / unparseable inputs return an empty `Vec`. SVG mandates
/// that odd-length lists are doubled (e.g. `"2 3 5"` ->
/// `"2 3 5 2 3 5"`); we apply that here so the walker can iterate
/// without worrying about parity.
pub(super) fn parse_dasharray(s: &str) -> Vec<f32> {
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    let mut nums: Vec<f32> = Vec::new();
    for tok in trimmed.split(|c: char| c == ',' || c.is_ascii_whitespace()) {
        if tok.is_empty() {
            continue;
        }
        // Strip an optional `px` suffix; everything else (em/%/etc.)
        // we treat as "user-space units" per SVG.
        let body = tok.trim_end_matches("px");
        match body.parse::<f32>() {
            Ok(n) if n.is_finite() && n >= 0.0 => nums.push(n),
            _ => return Vec::new(), // SVG: any negative or invalid -> ignore the whole list.
        }
    }
    if nums.is_empty() || nums.iter().all(|&v| v == 0.0) {
        return Vec::new();
    }
    if nums.len() % 2 == 1 {
        let extra = nums.clone();
        nums.extend_from_slice(&extra);
    }
    nums
}

/// [`dash_polyline_limited`] with a fresh split budget.
#[cfg(test)]
pub(super) fn dash_polyline(
    points: &[(f32, f32)],
    arc_lengths: &[f32],
    closed: bool,
    pattern: &[f32],
    offset: f32,
) -> Vec<Vec<(f32, f32)>> {
    let mut splits_left = MAX_DASH_SPLITS;
    dash_polyline_limited(
        points,
        arc_lengths,
        closed,
        pattern,
        offset,
        &mut splits_left,
    )
}

/// Walks `points` by cumulative *true Bezier arc length* and returns
/// the polylines that fall inside the "draw" phase of the dash pattern.
/// `arc_lengths[i]` is the parent-curve arc length of the chord from
/// `points[i]` to `points[(i + 1) % n]`. For straight chords this is
/// the Euclidean distance, for chords flattened from Quad/Cubic Beziers
/// it is the Roger Willcocks chord+control-polygon estimate (~0.05 %
/// of the true Gauss-Legendre integral on typical sweeps). `pattern`
/// is even-length and non-empty (caller-checked); `offset` is applied
/// at the start of the contour, then resets per [SVG spec].
///
/// Position mapping: a dash boundary at arc-length `s` along chord
/// `i` lands geometrically at parameter `t = s / arc_lengths[i]`
/// linearly between `points[i]` and `points[i+1]`. This is the
/// standard mapping for chord-flattened curves. The dash is *placed*
/// at its true-arc-length position along the curve, but the geometry
/// is interpolated on the chord (which is what the rasterizer
/// already consumes).
///
/// Behavior at a glance:
///
/// - Stride alternates draw / skip starting from index 0 ("draw").
/// - `offset` may be negative or larger than the pattern; reduced
///   modulo `total = sum(pattern)` after sign-folding.
/// - Closed contours are walked as if a final segment connected back
///   to the first vertex; the resulting "wrap" sub-polyline is split
///   the same way as any other.
/// - For straight-chord polylines (rect, polygon, polyline, line,
///   `LineTo` paths), `arc_lengths[i]` is exactly the Euclidean
///   distance, so this function is bit-identical to a chord-only
///   walker on those inputs.
///
/// Each dash boundary walked costs one unit of `splits_left`, a budget
/// shared across the calls for one stroke. When it runs out the walk
/// stops and returns the dashes found so far. This also ends the walk
/// when float rounding stops a tiny dash length from advancing along a
/// long path.
pub(super) fn dash_polyline_limited(
    points: &[(f32, f32)],
    arc_lengths: &[f32],
    closed: bool,
    pattern: &[f32],
    offset: f32,
    splits_left: &mut usize,
) -> Vec<Vec<(f32, f32)>> {
    let total: f32 = pattern.iter().sum();
    let Some(&first) = pattern.first() else {
        return Vec::new();
    };
    if total <= 0.0 || points.len() < 2 {
        return Vec::new();
    }
    // Normalize offset into [0, total).
    let mut off = offset % total;
    if off < 0.0 {
        off += total;
    }
    // The current dash index (even = draw, odd = skip) and remaining
    // length within that dash segment after consuming `off`.
    let mut idx = 0usize;
    let mut remaining = first;
    // `off < total`, so this finishes within one pass over the pattern
    // plus rounding slack. The bound stops a pattern whose entries are
    // too small to change `off` from cycling forever.
    let mut steps_left = pattern.len().saturating_mul(2).saturating_add(1);
    while off > 0.0 && remaining <= off && steps_left > 0 {
        steps_left -= 1;
        off -= remaining;
        idx = (idx + 1) % pattern.len();
        remaining = pattern[idx];
    }
    remaining -= off;
    let mut drawing = idx % 2 == 0;

    // Build the list of segments to walk. For closed contours we
    // append the wraparound segment.
    let n = points.len();
    let segs = if closed { n } else { n - 1 };

    let mut out: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut cur: Vec<(f32, f32)> = Vec::new();
    if drawing {
        cur.push(points[0]);
    }

    for i in 0..segs {
        let a = points[i];
        let b = points[(i + 1) % n];
        let dx = b.0 - a.0;
        let dy = b.1 - a.1;
        // True arc length of this chord segment (parent curve's sweep
        // length, not the chord-Euclidean distance. They only differ
        // for curve-flattened chords).
        let seg_arc = arc_lengths.get(i).copied().unwrap_or_else(|| {
            // Defensive fallback: parallel array missing this entry
            // (shouldn't happen with `flatten_to_polylines`, but guards
            // against future callers passing a malformed pair).
            (dx * dx + dy * dy).sqrt()
        });
        if seg_arc < 1e-6 {
            continue;
        }
        let mut s_consumed = 0.0_f32;
        // Walk the segment, splitting at every dash boundary in
        // arc-length space.
        while seg_arc - s_consumed > remaining {
            let Some(left) = splits_left.checked_sub(1) else {
                return out;
            };
            *splits_left = left;
            // Boundary lands at arc-length `s_consumed + remaining`
            // along this chord; map to chord parameter `t` linearly.
            // For straight chords this is exact; for curve chords the
            // sub-chord is short enough (curve flattening tolerance
            // 0.25 px) that the linear-on-chord mapping is well within
            // a sub-pixel of the true curve position.
            let t = (s_consumed + remaining) / seg_arc;
            let bx = a.0 + dx * t;
            let by = a.1 + dy * t;
            if drawing {
                cur.push((bx, by));
                if cur.len() >= 2 {
                    out.push(core::mem::take(&mut cur));
                }
            }
            s_consumed += remaining;
            // Advance to the next pattern entry.
            idx = (idx + 1) % pattern.len();
            remaining = pattern[idx];
            drawing = idx % 2 == 0;
            if drawing {
                cur.clear();
                cur.push((bx, by));
            }
        }
        // Remainder of the segment.
        let used = seg_arc - s_consumed;
        remaining -= used;
        if drawing {
            cur.push(b);
        }
    }
    if drawing && cur.len() >= 2 {
        out.push(cur);
    }
    out
}
