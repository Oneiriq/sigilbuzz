//! The Slug encoder driver.
//!
//! Walks a [`sigilbuzz::tables::Outline`], flattens any cubic
//! segments into quadratics, then partitions the glyph bbox into
//! horizontal bands and emits per-band segment lists.
//!
//! The output is deterministic: given the same `Face`, `glyph_id`,
//! and [`SlugOptions`], the encoder produces the same `bands` and
//! `segments` byte-for-byte. Segment ordering inside each band
//! follows path-traversal order — the same order in which they were
//! produced by the upstream PathOp stream.

use alloc::vec::Vec;

use sigilbuzz::tables::PathOp;
use sigilbuzz::Face;

use crate::flatten::cubic_to_quads;
use crate::types::{Band, Bbox, QuadSegment, SlugGlyph, Vec2};

/// Encoder configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlugOptions {
    /// Override the auto-computed band count. `None` (the default)
    /// asks the encoder to pick a band count proportional to glyph
    /// height in em-units.
    pub band_count: Option<u32>,
    /// Maximum allowed geometric error (design units) when flattening
    /// cubic Beziers into quadratics. Defaults to roughly 0.05 em on
    /// a 2048-upem font (~ 1 design unit).
    pub cubic_tolerance: f32,
}

impl SlugOptions {
    /// Default tolerance, in design units. Calibrated against a
    /// 2048-upem font: `0.05 em ≈ 102 design units` is too loose for
    /// readable text, so we use 1 design unit, well below the
    /// "perceptible" error for sub-pixel accuracy. Callers can
    /// override per-glyph for non-2048 upems.
    pub const DEFAULT_CUBIC_TOLERANCE: f32 = 1.0;
}

impl Default for SlugOptions {
    fn default() -> Self {
        Self {
            band_count: None,
            cubic_tolerance: Self::DEFAULT_CUBIC_TOLERANCE,
        }
    }
}

/// Encodes the static outline of `glyph_id` from `face` using `opts`.
///
/// Returns `None` when the glyph has no outline (whitespace, missing
/// glyph id) or when the outline has zero rasterisable extent. Errors
/// from outline extraction are swallowed into `None`; callers who
/// need to distinguish the two cases can call
/// [`Face::glyph_outline`] directly first.
#[must_use]
pub fn encode_glyph(face: &Face<'_>, glyph_id: u16, opts: &SlugOptions) -> Option<SlugGlyph> {
    encode_glyph_at_coords(face, glyph_id, &[], opts)
}

/// Like [`encode_glyph`] but applies variable-font axis coordinates.
/// `coords` is a slice of normalized axis values in `[-1.0, 1.0]`,
/// one per `fvar` axis.
#[must_use]
pub fn encode_glyph_at_coords(
    face: &Face<'_>,
    glyph_id: u16,
    coords: &[f32],
    opts: &SlugOptions,
) -> Option<SlugGlyph> {
    let outline = face.glyph_outline_at_coords(glyph_id, coords).ok()??;
    encode_outline_ops(outline.ops(), opts)
}

/// Lower-level entry point: encode an arbitrary [`PathOp`] stream.
/// Mostly useful for tests with hand-crafted paths.
#[must_use]
pub(crate) fn encode_outline_ops(ops: &[PathOp], opts: &SlugOptions) -> Option<SlugGlyph> {
    let segments = flatten_to_quads(ops, opts.cubic_tolerance);
    if segments.is_empty() {
        return None;
    }

    // Compute bbox from segments — the most reliable source. Glyf
    // bbox in the font header is *not* always tight (composites
    // approximate, hinting changes extents) and we want a bbox that
    // genuinely contains every emitted segment.
    let bbox = segment_pool_bbox(&segments);
    if bbox.is_empty() {
        return None;
    }

    let band_count = opts.band_count.unwrap_or_else(|| auto_band_count(&bbox));
    let band_count = band_count.max(1);

    let (bands, banded_segments) = decompose_into_bands(&segments, &bbox, band_count);

    Some(SlugGlyph {
        bbox,
        bands,
        segments: banded_segments,
    })
}

/// Flattens a PathOp stream to a flat list of quadratic segments.
/// Lines become degenerate quadratics with the control point at the
/// segment midpoint. Cubics are subdivided per
/// [`crate::flatten::cubic_to_quads`].
fn flatten_to_quads(ops: &[PathOp], tolerance: f32) -> Vec<QuadSegment> {
    let mut out: Vec<QuadSegment> = Vec::with_capacity(ops.len());
    let mut start = Vec2::default();
    let mut current = Vec2::default();
    let mut have_subpath = false;
    let mut tmp_quads: Vec<(Vec2, Vec2)> = Vec::with_capacity(8);

    for op in ops {
        match *op {
            PathOp::MoveTo { x, y } => {
                start = Vec2::new(x, y);
                current = start;
                have_subpath = true;
            }
            PathOp::LineTo { x, y } => {
                if !have_subpath {
                    // Outline that opens with a LineTo is malformed
                    // upstream; treat the line origin as the implicit
                    // start.
                    start = current;
                    have_subpath = true;
                }
                let end = Vec2::new(x, y);
                let mid = Vec2::new(
                    (current.x + end.x) * 0.5,
                    (current.y + end.y) * 0.5,
                );
                out.push(QuadSegment {
                    p0: current,
                    p1: mid,
                    p2: end,
                });
                current = end;
            }
            PathOp::QuadTo { cx, cy, x, y } => {
                let end = Vec2::new(x, y);
                out.push(QuadSegment {
                    p0: current,
                    p1: Vec2::new(cx, cy),
                    p2: end,
                });
                current = end;
            }
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => {
                tmp_quads.clear();
                let p3 = Vec2::new(x, y);
                cubic_to_quads(
                    current,
                    Vec2::new(c1x, c1y),
                    Vec2::new(c2x, c2y),
                    p3,
                    tolerance,
                    &mut tmp_quads,
                );
                let mut prev = current;
                for &(ctrl, end) in &tmp_quads {
                    out.push(QuadSegment {
                        p0: prev,
                        p1: ctrl,
                        p2: end,
                    });
                    prev = end;
                }
                current = p3;
            }
            PathOp::Close => {
                if have_subpath && (current.x != start.x || current.y != start.y) {
                    let end = start;
                    let mid = Vec2::new(
                        (current.x + end.x) * 0.5,
                        (current.y + end.y) * 0.5,
                    );
                    out.push(QuadSegment {
                        p0: current,
                        p1: mid,
                        p2: end,
                    });
                    current = end;
                }
                have_subpath = false;
            }
        }
    }

    out
}

/// Tight bbox covering the convex hull (and therefore the curve) of
/// every quadratic in `segments`. A quadratic Bezier lies within the
/// triangle formed by its three control points, so taking the
/// min/max of those points gives a strict upper bound.
fn segment_pool_bbox(segments: &[QuadSegment]) -> Bbox {
    let mut bbox = Bbox::empty();
    for s in segments {
        bbox.expand(s.p0.x, s.p0.y);
        bbox.expand(s.p1.x, s.p1.y);
        bbox.expand(s.p2.x, s.p2.y);
    }
    bbox
}

/// Heuristic for the auto band count: 16 bands per em-equivalent of
/// height. The Slug paper shows diminishing returns above ~32 bands
/// per typical glyph; we tune for 16 to keep band metadata small.
/// The caller can override via [`SlugOptions::band_count`].
fn auto_band_count(bbox: &Bbox) -> u32 {
    // Without the upem we can't compute "bands per em" exactly. Use
    // the practical heuristic: 1 band per ~64 design units, capped.
    // For a 2048-upem font this gives ~32 bands across the full em
    // height, ~24 for a typical x-height glyph.
    let h = bbox.height().max(1.0);
    let n = (h / 64.0).round() as u32;
    n.clamp(4, 64)
}

/// Decomposes a glyph's segment pool into per-band slices.
///
/// Each band spans an equal share of the bbox y-range. A segment
/// lands in every band whose y-range overlaps the segment's y-extent
/// (computed as the y-range of `[p0, p1, p2]` since the curve is
/// contained in the control hull).
fn decompose_into_bands(
    segments: &[QuadSegment],
    bbox: &Bbox,
    band_count: u32,
) -> (Vec<Band>, Vec<QuadSegment>) {
    let band_count = band_count as usize;
    let h = bbox.height().max(f32::EPSILON);
    let band_h = h / band_count as f32;

    // First pass: count segments per band so we can build a flat
    // pool. Avoids resizing per-band intermediate vectors and keeps
    // cache locality good.
    let mut counts: Vec<u32> = alloc::vec![0_u32; band_count];
    let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(segments.len());

    for s in segments {
        let y_lo = s.p0.y.min(s.p1.y).min(s.p2.y);
        let y_hi = s.p0.y.max(s.p1.y).max(s.p2.y);
        let (lo, hi) = band_indices_for(y_lo, y_hi, bbox.ymin, band_h, band_count);
        ranges.push((lo, hi));
        for c in counts.iter_mut().take(hi).skip(lo) {
            *c += 1;
        }
    }

    // Build offsets via prefix-sum.
    let mut bands: Vec<Band> = Vec::with_capacity(band_count);
    let mut acc: u32 = 0;
    for &c in &counts {
        bands.push(Band {
            segment_offset: acc,
            segment_count: c,
        });
        acc = acc.saturating_add(c);
    }

    // Second pass: scatter segments into the pool. We track a
    // running "next index" per band by reusing a copy of the offsets.
    let total = acc as usize;
    let mut pool: Vec<QuadSegment> = alloc::vec![QuadSegment::default(); total];
    let mut cursors: Vec<u32> = bands.iter().map(|b| b.segment_offset).collect();

    for (seg, &(lo, hi)) in segments.iter().zip(ranges.iter()) {
        for cursor in cursors.iter_mut().take(hi).skip(lo) {
            let dst = *cursor as usize;
            pool[dst] = *seg;
            *cursor += 1;
        }
    }

    (bands, pool)
}

/// Returns the `[lo, hi)` band-index range whose y-spans intersect
/// `[y_lo, y_hi]`. Both endpoints are clamped to `[0, band_count]`.
fn band_indices_for(
    y_lo: f32,
    y_hi: f32,
    bbox_ymin: f32,
    band_h: f32,
    band_count: usize,
) -> (usize, usize) {
    if band_count == 0 || band_h <= 0.0 {
        return (0, 0);
    }
    let lo_f = (y_lo - bbox_ymin) / band_h;
    let hi_f = (y_hi - bbox_ymin) / band_h;
    let lo = clamp_floor(lo_f, band_count);
    let mut hi = clamp_ceil(hi_f, band_count);
    if hi <= lo {
        // Zero-height segment (e.g. horizontal line) — make sure it
        // still lands in exactly one band so the renderer sees it.
        hi = (lo + 1).min(band_count);
    }
    (lo, hi)
}

#[inline]
fn clamp_floor(v: f32, max_band: usize) -> usize {
    if !v.is_finite() || v <= 0.0 {
        0
    } else {
        let i = v.floor() as i64;
        if i < 0 {
            0
        } else if i >= max_band as i64 {
            max_band.saturating_sub(1)
        } else {
            i as usize
        }
    }
}

#[inline]
fn clamp_ceil(v: f32, max_band: usize) -> usize {
    if !v.is_finite() || v <= 0.0 {
        0
    } else {
        let i = v.ceil() as i64;
        if i < 0 {
            0
        } else if i > max_band as i64 {
            max_band
        } else {
            i as usize
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(p0: (f32, f32), p1: (f32, f32), p2: (f32, f32)) -> QuadSegment {
        QuadSegment {
            p0: Vec2::new(p0.0, p0.1),
            p1: Vec2::new(p1.0, p1.1),
            p2: Vec2::new(p2.0, p2.1),
        }
    }

    #[test]
    fn line_to_emits_degenerate_quadratic() {
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 10.0, y: 20.0 },
            PathOp::Close,
        ];
        // Force a single band so the segment pool equals the unique
        // segment count.
        let opts = SlugOptions {
            band_count: Some(1),
            ..SlugOptions::default()
        };
        let g = encode_outline_ops(&ops, &opts).unwrap();
        // 1 line + 1 close-segment back to start.
        assert_eq!(g.segments.len(), 2);
        // Every emitted line is a degenerate quadratic with control
        // at the segment midpoint.
        let line = g
            .segments
            .iter()
            .find(|s| s.p0 == Vec2::new(0.0, 0.0) && s.p2 == Vec2::new(10.0, 20.0))
            .unwrap();
        assert_eq!(line.p1, Vec2::new(5.0, 10.0));
    }

    #[test]
    fn close_skipped_when_already_at_start() {
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 10.0, y: 0.0 },
            PathOp::LineTo { x: 0.0, y: 0.0 },
            PathOp::Close,
        ];
        // The bbox here is degenerate (height = 0) so no
        // band-decomposition encoding is possible. Test the line
        // collapsing in isolation.
        let segs = flatten_to_quads(&ops, SlugOptions::DEFAULT_CUBIC_TOLERANCE);
        // 2 lines, the close is a no-op.
        assert_eq!(segs.len(), 2);
    }

    #[test]
    fn cubic_to_is_flattened() {
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::CubicTo {
                c1x: 100.0,
                c1y: 200.0,
                c2x: 200.0,
                c2y: -200.0,
                x: 300.0,
                y: 0.0,
            },
            PathOp::Close,
        ];
        // Inspect the un-banded segment chain via flatten_to_quads
        // directly: the encoder reorders segments by band.
        let segs = flatten_to_quads(&ops, SlugOptions::DEFAULT_CUBIC_TOLERANCE);
        // At least one cubic-derived quadratic plus the closing segment.
        assert!(segs.len() >= 2);
        // Chain is contiguous until the close segment.
        for w in segs.windows(2) {
            // p2 of segment i == p0 of segment i+1.
            assert_eq!(w[0].p2, w[1].p0);
        }
        // Last segment closes back to the move-to point.
        assert_eq!(segs.last().unwrap().p2, Vec2::new(0.0, 0.0));
    }

    #[test]
    fn band_decomposition_sums_to_pool_size() {
        // Two horizontal-ish segments in different y bands.
        let segments = alloc::vec![
            quad((0.0, 0.0), (5.0, 1.0), (10.0, 0.0)),
            quad((0.0, 50.0), (5.0, 51.0), (10.0, 50.0)),
        ];
        let bbox = segment_pool_bbox(&segments);
        let (bands, pool) = decompose_into_bands(&segments, &bbox, 4);
        let sum: u32 = bands.iter().map(|b| b.segment_count).sum();
        assert_eq!(sum as usize, pool.len());
    }

    #[test]
    fn segment_in_band_y_range() {
        let segments = alloc::vec![
            quad((0.0, 0.0), (50.0, 100.0), (100.0, 0.0)),
            quad((0.0, 100.0), (50.0, 0.0), (100.0, 100.0)),
        ];
        let bbox = segment_pool_bbox(&segments);
        let band_count = 4_u32;
        let (bands, pool) = decompose_into_bands(&segments, &bbox, band_count);
        let band_h = bbox.height() / band_count as f32;
        for (i, band) in bands.iter().enumerate() {
            let y0 = bbox.ymin + i as f32 * band_h;
            let y1 = y0 + band_h;
            let off = band.segment_offset as usize;
            let count = band.segment_count as usize;
            for s in &pool[off..off + count] {
                let s_lo = s.p0.y.min(s.p1.y).min(s.p2.y);
                let s_hi = s.p0.y.max(s.p1.y).max(s.p2.y);
                // Segment must overlap the band [y0, y1].
                assert!(
                    s_hi >= y0 - 1e-3 && s_lo <= y1 + 1e-3,
                    "segment y-range [{s_lo}, {s_hi}] outside band {i} [{y0}, {y1}]"
                );
            }
        }
    }

    #[test]
    fn auto_band_count_clamps() {
        let tiny = Bbox {
            xmin: 0.0,
            ymin: 0.0,
            xmax: 10.0,
            ymax: 10.0,
        };
        let huge = Bbox {
            xmin: 0.0,
            ymin: 0.0,
            xmax: 10.0,
            ymax: 100_000.0,
        };
        assert!(auto_band_count(&tiny) >= 4);
        assert!(auto_band_count(&huge) <= 64);
    }

    #[test]
    fn band_count_override_honoured() {
        let segments = alloc::vec![quad((0.0, 0.0), (5.0, 50.0), (10.0, 100.0))];
        let bbox = segment_pool_bbox(&segments);
        let (bands, _) = decompose_into_bands(&segments, &bbox, 7);
        assert_eq!(bands.len(), 7);
    }

    #[test]
    fn deterministic_encoding() {
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::CubicTo {
                c1x: 100.0,
                c1y: 200.0,
                c2x: 200.0,
                c2y: -200.0,
                x: 300.0,
                y: 0.0,
            },
            PathOp::QuadTo {
                cx: 250.0,
                cy: 50.0,
                x: 200.0,
                y: 100.0,
            },
            PathOp::LineTo { x: 0.0, y: 100.0 },
            PathOp::Close,
        ];
        let a = encode_outline_ops(&ops, &SlugOptions::default()).unwrap();
        let b = encode_outline_ops(&ops, &SlugOptions::default()).unwrap();
        assert_eq!(a, b);
    }
}
