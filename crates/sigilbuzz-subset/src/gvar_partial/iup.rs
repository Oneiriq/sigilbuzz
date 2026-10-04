//! Inferred deltas (IUP) of one sparse tuple, and the check that
//! decides whether a sparse tuple survives a move of its glyph's
//! default points.
//!
//! The inference follows the shaper's (see
//! [`sigilbuzz::tables::Gvar::glyph_point_deltas`]): per contour, each
//! point the tuple skips takes its delta from the nearest listed
//! points before and after it, by where it sits between them.

use alloc::vec::Vec;

use super::{GlyphPoints, IUP_TOLERANCE};

/// A tuple that lists the deltas of some points only.
pub(super) struct SparseTuple<'a> {
    /// The point numbers it lists.
    pub(super) points: &'a [u16],
    /// Its exact deltas in the instance, one per listed point: the
    /// source's, scaled by the pinned axes.
    pub(super) deltas: &'a [(f32, f32)],
}

impl SparseTuple<'_> {
    /// The tuple's exact deltas for every point of the glyph (its own
    /// points, then the four phantom points), inferred from the
    /// source's default points, when the rounded deltas `xs` / `ys` the
    /// instance would store infer deltas more than [`IUP_TOLERANCE`]
    /// away from those once the default points move from
    /// `glyph.before` to `after`. `None` when the sparse tuple still
    /// holds.
    pub(super) fn densified_if_drifting(
        &self,
        glyph: &GlyphPoints,
        after: &[(i32, i32)],
        xs: &[i32],
        ys: &[i32],
    ) -> Option<Vec<(f32, f32)>> {
        let source = infer(
            self.points,
            |k| self.deltas.get(k).copied().unwrap_or((0.0, 0.0)),
            &glyph.before,
            &glyph.end_pts,
        );
        let instance = infer(
            self.points,
            |k| {
                let x = xs.get(k).copied().unwrap_or(0) as f32;
                let y = ys.get(k).copied().unwrap_or(0) as f32;
                (x, y)
            },
            after,
            &glyph.end_pts,
        );
        let drifts = source
            .iter()
            .zip(&instance)
            .any(|(s, i)| (s.0 - i.0).abs() > IUP_TOLERANCE || (s.1 - i.1).abs() > IUP_TOLERANCE);
        drifts.then_some(source)
    }
}

/// The delta of every point of a glyph whose default points are
/// `orig` and contours end at `end_pts`, for a tuple listing `points`
/// with deltas `delta(k)` for listed entry `k`: `orig.len() + 4`
/// deltas, the phantom points last. A point listed twice gets both
/// deltas; point numbers past the end are ignored.
pub(super) fn infer(
    points: &[u16],
    delta: impl Fn(usize) -> (f32, f32),
    orig: &[(i32, i32)],
    end_pts: &[u16],
) -> Vec<(f32, f32)> {
    let count = orig.len() + 4;
    let mut deltas = alloc::vec![(0.0_f32, 0.0_f32); count];
    let mut listed = alloc::vec![false; count];
    for (k, &pt) in points.iter().enumerate() {
        let i = usize::from(pt);
        let Some(slot) = deltas.get_mut(i) else {
            continue;
        };
        let (dx, dy) = delta(k);
        slot.0 += dx;
        slot.1 += dy;
        listed[i] = true;
    }
    // Contour membership, from the end point numbers. Points past the
    // last end point (and the phantom points) are on no contour.
    let mut is_end = alloc::vec![false; orig.len()];
    for &e in end_pts {
        if let Some(flag) = is_end.get_mut(usize::from(e)) {
            *flag = true;
        }
    }
    let mut start = 0;
    for (end, _) in is_end.iter().enumerate().filter(|&(_, &e)| e) {
        if end >= start {
            infer_contour(&mut deltas, &listed, orig, start, end);
        }
        start = end + 1;
    }
    deltas
}

/// [`infer`] for the contour `start..=end`.
fn infer_contour(
    deltas: &mut [(f32, f32)],
    listed: &[bool],
    orig: &[(i32, i32)],
    start: usize,
    end: usize,
) {
    let listed_count = listed[start..=end].iter().filter(|&&l| l).count();
    if listed_count == 0 || listed_count == end - start + 1 {
        return;
    }
    let next = |i: usize| if i >= end { start } else { i + 1 };
    let Some(first) = (start..=end).find(|&i| listed[i]) else {
        return;
    };
    // Walk the listed points around the contour once, filling each
    // run of unlisted points between a listed point and the next.
    let mut prev = first;
    loop {
        let mut after = next(prev);
        while !listed[after] {
            after = next(after);
        }
        let mut i = next(prev);
        while i != after {
            let target = orig[i];
            let (p, a) = (orig[prev], orig[after]);
            let (pd, ad) = (deltas[prev], deltas[after]);
            deltas[i] = (
                infer_delta(target.0 as f32, p.0 as f32, a.0 as f32, pd.0, ad.0),
                infer_delta(target.1 as f32, p.1 as f32, a.1 as f32, pd.1, ad.1),
            );
            i = next(i);
        }
        if after == first {
            break;
        }
        prev = after;
    }
}

/// The delta on one axis of a point at `target` between listed points
/// at `prev` and `next` whose deltas are `prev_delta` and `next_delta`.
// Exact comparisons on purpose: the coordinates are integers, and equal
// deltas must agree bit for bit, as in the shaper.
#[allow(clippy::float_cmp)]
fn infer_delta(target: f32, prev: f32, next: f32, prev_delta: f32, next_delta: f32) -> f32 {
    if prev == next {
        return if prev_delta == next_delta {
            prev_delta
        } else {
            0.0
        };
    }
    if target <= prev.min(next) {
        return if prev < next { prev_delta } else { next_delta };
    }
    if target >= prev.max(next) {
        return if prev > next { prev_delta } else { next_delta };
    }
    let r = (target - prev) / (next - prev);
    prev_delta + r * (next_delta - prev_delta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// A square contour of four points.
    fn square() -> Vec<(i32, i32)> {
        vec![(0, 0), (0, 100), (100, 100), (100, 0)]
    }

    #[test]
    fn unlisted_points_interpolate_between_listed_neighbors() {
        // Points 0 and 2 listed; 1 and 3 sit between them.
        let d = infer(&[0, 2], |k| [(10.0, 0.0), (20.0, 40.0)][k], &square(), &[3]);
        assert_eq!(d.len(), 8);
        assert_eq!(d[0], (10.0, 0.0));
        assert_eq!(d[2], (20.0, 40.0));
        // Point 1 (0, 100): x equals point 0's, so it takes point 0's
        // x delta; y equals point 2's, so it takes point 2's y delta.
        assert_eq!(d[1], (10.0, 40.0));
        assert_eq!(d[3], (20.0, 0.0));
        // Phantom points are never inferred.
        assert_eq!(&d[4..], &[(0.0, 0.0); 4]);
    }

    #[test]
    fn a_contour_with_one_listed_point_moves_rigidly() {
        let d = infer(&[1], |_| (5.0, -5.0), &square(), &[3]);
        assert_eq!(&d[..4], &[(5.0, -5.0); 4]);
    }

    #[test]
    fn listed_numbers_past_the_end_are_ignored_and_repeats_add() {
        let d = infer(&[0, 0, 99], |_| (1.0, 1.0), &square(), &[3]);
        assert_eq!(&d[..4], &[(2.0, 2.0); 4]);
    }

    #[test]
    fn a_tuple_whose_inference_still_holds_stays_sparse() {
        // Moving every point by the same amount keeps every ratio.
        let before = square();
        let after: Vec<(i32, i32)> = before.iter().map(|&(x, y)| (x + 7, y + 3)).collect();
        let glyph = GlyphPoints {
            end_pts: vec![3],
            before,
            after: None,
        };
        let tuple = SparseTuple {
            points: &[0, 2],
            deltas: &[(10.0, 0.0), (20.0, 40.0)],
        };
        assert_eq!(
            tuple.densified_if_drifting(&glyph, &after, &[10, 20], &[0, 40]),
            None
        );
    }

    #[test]
    fn a_tuple_whose_inference_drifts_lists_every_point() {
        // Point 1 sits halfway between points 0 and 2 on x before the
        // move, and at point 2's x after it.
        let glyph = GlyphPoints {
            end_pts: vec![3],
            before: vec![(0, 0), (50, 0), (100, 0), (50, 50)],
            after: None,
        };
        let after = [(0, 0), (100, 0), (100, 0), (50, 50)];
        let tuple = SparseTuple {
            points: &[0, 2, 3],
            deltas: &[(0.0, 0.0), (50.0, 0.0), (0.0, 0.0)],
        };
        let dense = tuple
            .densified_if_drifting(&glyph, &after, &[0, 50, 0], &[0, 0, 0])
            .expect("drifts");
        // The source infers 25 for point 1, halfway between 0 and 50.
        let dx: Vec<f32> = dense.iter().map(|d| d.0).collect();
        assert_eq!(dx, vec![0.0, 25.0, 50.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert!(dense.iter().all(|d| d.1 == 0.0));
    }
}
