//! `gvar` partial-instancing rewrite (#190).
//!
//! When a TrueType + `gvar` source is partial-instanced (some axes
//! `Pin`, others `Keep`), the spec-level shape of `gvar` has to change:
//! the file-wide axis count drops to the kept-axis count, every per-
//! glyph `TupleVariationHeader`'s peak / intermediate region keeps
//! only its `Keep`-axis dimensions, and every per-point delta scales
//! by the product of `axis_support_scalar(...)` over the `Pin` axes.
//! Tuples that contribute nothing at the pin coords (a `Pin` axis
//! falls outside its region or hits a peak-of-zero edge case with
//! `axis_support_scalar` evaluating to 0) are dropped entirely.
//!
//! The rewrite follows HarfBuzz's and fontTools' instancers:
//!
//! - A tuple whose region lies on the pinned axes only applies at
//!   every kept coordinate alike. Its deltas belong in the default
//!   outline, where the instancer has already baked them (see
//!   [`crate::instance()`]), so the tuple goes.
//! - Tuples whose projected regions match merge into one tuple, their
//!   scaled deltas summed before they round, so a merged delta is off
//!   by at most half a unit. Tuples that list different points merge
//!   into a tuple covering every point, the points each one skips
//!   inferred from the source's default points (IUP).
//! - Moving the default outline changes what the points a sparse tuple
//!   skips infer from the points it lists, so a sparse tuple of a glyph
//!   whose points moved is checked: when its inferred deltas now land
//!   more than half a unit from where the source puts them, the tuple
//!   is written out with every point's delta instead.
//!
//! The rewrite is implemented as a self-contained re-emit:
//!
//! 1. Walk the source's `glyphCount`, parse each glyph's
//!    `GlyphVariationData` body.
//! 2. For each tuple, resolve its peak (via shared-tuples or the
//!    embedded form), compute the projection through
//!    [`crate::instance::project_region_onto_kept_axes`], scale the
//!    packed deltas by the `pin_scalar`, then re-emit the tuple as an
//!    embedded-peak header (no shared-tuples reference).  Forcing
//!    embedded peaks is slightly larger than reusing the source's
//!    shared-tuple list but eliminates the cross-glyph shared-tuple
//!    coordination problem cleanly: every output tuple stands alone.
//! 3. Output `sharedTupleCount = 0`. A tuple that keeps its source
//!    points keeps the source's shared-points / private-points
//!    structure: its packed point numbers are copied byte-for-byte.
//! 4. The header's `axisCount` becomes `new_axis_count`; the header's
//!    `glyphCount` is the source's verbatim (instancing keeps every
//!    glyph).
//!
//! All-`Keep` short-circuit: a no-op partial pins zero axes; the
//! source bytes pass through verbatim.
//!
//! # Determinism
//!
//! Deltas round half up, as in HarfBuzz and fontTools; tuple ordering
//! follows the source, a merged tuple taking the place of its first
//! member.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::instance::{project_region_onto_kept_axes, AxisPin, F2Dot14};
use crate::util::{round_half_up, WorkBudget};
use crate::warnings::Warnings;
use crate::SubsetError;

mod iup;
pub(crate) mod tuple;

use iup::SparseTuple;
use tuple::{
    count_packed_deltas, encode_packed_deltas, f2dot14_raw, packed_point_numbers_byte_len,
    parse_packed_point_numbers, parse_tuple_header, read_packed_deltas_n, write_f2dot14,
    ParsedTupleHeader,
};

const FLAG_EMBEDDED_PEAK: u16 = 0x8000;
const FLAG_INTERMEDIATE_REGION: u16 = 0x4000;
const FLAG_PRIVATE_POINT_NUMBERS: u16 = 0x2000;
const TUPLE_INDEX_MASK: u16 = 0x0FFF;

const SHARED_POINTS_FLAG: u16 = 0x8000;
const TUPLE_COUNT_MASK: u16 = 0x0FFF;

const DELTA_ALL_ZERO: u8 = 0x80;
const DELTA_WORDS: u8 = 0x40;
const DELTA_COUNT_MASK: u8 = 0x3F;

/// The default points of one glyph: its own gvar points before the
/// partial instance (contour points, or one per component of a
/// composite), and, for a simple glyph whose points the instance moved,
/// after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GlyphPoints {
    /// The glyph's `endPtsOfContours`; empty for a composite.
    pub(crate) end_pts: Vec<u16>,
    /// The source's default points.
    pub(crate) before: Vec<(i32, i32)>,
    /// The instance's default points, as many as `before`, when they
    /// moved.
    pub(crate) after: Option<Vec<(i32, i32)>>,
}

/// How far, in font units, an inferred delta may land from the source's
/// before a sparse tuple is written out in full. fontTools' instancer
/// optimizes its tuples to the same tolerance.
const IUP_TOLERANCE: f32 = 0.5;

/// Work units the rewrite may spend per byte of the source `gvar` on
/// inferring sparse tuples, on top of [`MIN_WORK`]. A unit is one point
/// delta inferred or written. Inferring a sparse tuple over all of its
/// glyph's points, or writing it with every point, costs the point
/// count, which a few bytes of tuple can claim thousands of times over.
/// Real fonts spend well under one unit per byte.
const WORK_PER_BYTE: u64 = 8;

/// Inference work every rewrite may spend, however small the table.
const MIN_WORK: u64 = 1 << 20;

/// Point deltas the rewrite may decode per byte of the source `gvar`,
/// on top of [`MIN_DECODE`]. A run of zeros packs 64 deltas into one
/// byte, so decoding can outgrow the table that many times; each
/// decoded delta is held until its glyph is written. Real fonts decode
/// about one delta per byte.
const DECODE_PER_BYTE: u64 = 16;

/// Point deltas every rewrite may decode, however small the table.
const MIN_DECODE: u64 = 1 << 20;

/// What a rewrite reports when a glyph's tuples cannot all be decoded
/// within the budget.
const OVER_BUDGET: &str = "gvar partial: variation work exceeds its budget";

/// [`bake_gvar_partial_with`] for a font whose default points are not
/// known: tuples merge only when they list the same points, and sparse
/// tuples keep their points.
#[cfg(test)]
pub(crate) fn bake_gvar_partial(
    gvar_bytes: &[u8],
    coords: &[F2Dot14],
    pins: &[AxisPin],
    new_axis_count: u16,
) -> Result<Vec<u8>, SubsetError> {
    bake_gvar_partial_with(
        gvar_bytes,
        coords,
        pins,
        new_axis_count,
        &|_| None,
        &Warnings::default(),
    )
}

/// Bakes a partial-instancing rewrite of `gvar` against `coords` and
/// `pins`, dropping every `Pin`-axis dimension from the file-wide axis
/// count and every tuple region while scaling the per-point deltas by
/// the corresponding `axis_support_scalar` product.
///
/// `new_axis_count` must equal the count of `AxisPin::Keep` entries in
/// `pins` (the caller derives both from a single source-axis vector).
///
/// `points` gives each glyph's default points (see [`GlyphPoints`]),
/// read when tuples that list different points merge, or when a moved
/// glyph's sparse tuple is checked; `None` when they are not known.
///
/// Returns the new `gvar` bytes. When every axis is `AxisPin::Keep`
/// the source bytes pass through verbatim. No re-emit is needed.
pub(crate) fn bake_gvar_partial_with(
    gvar_bytes: &[u8],
    coords: &[F2Dot14],
    pins: &[AxisPin],
    new_axis_count: u16,
    points: &dyn Fn(u16) -> Option<GlyphPoints>,
    warnings: &Warnings,
) -> Result<Vec<u8>, SubsetError> {
    if pins.iter().all(|p| matches!(p, AxisPin::Keep)) {
        // No pinning: passthrough preserves byte-identity, which the
        // round-trip test relies on.
        return Ok(gvar_bytes.to_vec());
    }
    if pins.len() != coords.len() {
        return Err(SubsetError::Unsupported(
            "gvar partial: pins / coords length mismatch",
        ));
    }
    let kept_count = pins.iter().filter(|p| matches!(p, AxisPin::Keep)).count() as u16;
    if kept_count != new_axis_count {
        return Err(SubsetError::Unsupported(
            "gvar partial: new_axis_count must equal the count of Keep entries",
        ));
    }

    let header = parse_gvar_header(gvar_bytes)?;
    if header.axis_count as usize != pins.len() {
        return Err(SubsetError::Unsupported(
            "gvar partial: axis count mismatch with pins",
        ));
    }

    // Pre-resolve the shared tuple list (peaks only, axis_count
    // dimensions). We project each one once and cache the result:
    // any source tuple that points at it via tuple_index without its
    // own intermediate region reuses the same projection. Without the
    // cache every such tuple would cost a walk over every source axis.
    let shared_tuples = read_shared_tuples(gvar_bytes, &header)?;
    let shared_projections: Vec<Option<ProjectedRegion>> = shared_tuples
        .iter()
        .map(|peak| project_tuple_region(peak, None, header.axis_count, coords, pins))
        .collect();
    let tuples = SharedTuples {
        peaks: &shared_tuples,
        projections: &shared_projections,
    };

    // Walk every glyph's body and re-emit. Each glyph stores its data
    // in its own byte range in a well-formed table, so the bodies add
    // up to at most the table size. Offsets that make many glyphs
    // share one large range would multiply the work instead.
    let mut bodies: Vec<Vec<u8>> = Vec::with_capacity(header.glyph_count as usize);
    let mut body_budget = gvar_bytes.len();
    // The work of the whole rewrite, scaled to the table (see
    // `WORK_PER_BYTE` and `DECODE_PER_BYTE`). Past the inference
    // budget, tuples keep the points they list rather than being
    // inferred over all of them; past the decoding budget, a glyph
    // loses its variations. Both are reported, and the output and the
    // memory held stay in proportion to the input.
    let per_byte = |units: u64, floor: u64| {
        WorkBudget::new(
            (gvar_bytes.len() as u64)
                .saturating_mul(units)
                .saturating_add(floor),
        )
    };
    let work = per_byte(WORK_PER_BYTE, MIN_WORK);
    let decode = per_byte(DECODE_PER_BYTE, MIN_DECODE);
    // The output so far, checked as it grows rather than once every
    // body is held.
    let mut written: u64 = 0;
    for gid in 0..header.glyph_count {
        let body = pull_glyph_body(gvar_bytes, &header, gid);
        body_budget = body_budget
            .checked_sub(body.len())
            .ok_or(SubsetError::Unsupported(
                "gvar partial: glyph data ranges overlap",
            ))?;
        let new_body = if body.is_empty() {
            Vec::new()
        } else {
            let glyph_points = points(gid);
            let glyph = GlyphCtx {
                shared: &tuples,
                src_axis_count: header.axis_count,
                coords,
                pins,
                points: glyph_points.as_ref(),
                work: &work,
                decode: &decode,
                warnings,
            };
            match rewrite_glyph_body(body, &glyph) {
                Err(SubsetError::Unsupported(OVER_BUDGET)) => {
                    warnings.push(
                        GVAR_TAG,
                        0,
                        OVER_BUDGET,
                        "the variations of the glyphs past it",
                    );
                    Vec::new()
                }
                other => other?,
            }
        };
        written = written.saturating_add(new_body.len() as u64 + 1);
        if written > u64::from(u32::MAX) {
            return Err(SubsetError::Unsupported("gvar partial: offset overflow"));
        }
        bodies.push(new_body);
    }

    // Pad each body to 2-byte alignment so short offsets stay aligned.
    for body in &mut bodies {
        if body.len() % 2 != 0 {
            body.push(0);
        }
    }

    // Compute glyph offsets.
    let mut offsets: Vec<u32> = Vec::with_capacity(bodies.len() + 1);
    let mut cursor: u32 = 0;
    offsets.push(0);
    for body in &bodies {
        cursor = cursor
            .checked_add(body.len() as u32)
            .ok_or(SubsetError::Unsupported("gvar partial: offset overflow"))?;
        offsets.push(cursor);
    }
    let total_data_len = *offsets.last().unwrap_or(&0);
    let long_offsets = total_data_len > 0x1_FFFE;

    // Layout: header (20) + offsets + (no shared tuples) + data array.
    let header_len = 20usize;
    let off_entry_size: usize = if long_offsets { 4 } else { 2 };
    let offsets_len = (header.glyph_count as usize + 1) * off_entry_size;
    let offsets_padded = (offsets_len + 3) & !3;
    let shared_tuples_off: u32 = (header_len + offsets_padded) as u32;
    // sharedTupleCount = 0 -> shared tuple region is empty; data array
    // sits immediately after the offsets-padded region.
    let data_array_off: u32 = shared_tuples_off;

    let mut out: Vec<u8> =
        Vec::with_capacity(header_len + offsets_padded + total_data_len as usize);

    // Header.
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&new_axis_count.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // sharedTupleCount = 0
    out.extend_from_slice(&shared_tuples_off.to_be_bytes());
    out.extend_from_slice(&header.glyph_count.to_be_bytes());
    let flags: u16 = if long_offsets { 0x0001 } else { 0x0000 };
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&data_array_off.to_be_bytes());

    // Offsets.
    if long_offsets {
        for o in &offsets {
            out.extend_from_slice(&o.to_be_bytes());
        }
    } else {
        for o in &offsets {
            let half = (*o / 2) as u16;
            out.extend_from_slice(&half.to_be_bytes());
        }
    }
    while out.len() < (header_len + offsets_padded) {
        out.push(0);
    }

    // Data array (no shared tuples, sharedTupleCount = 0).
    for body in &bodies {
        out.extend_from_slice(body);
    }

    Ok(out)
}

#[derive(Debug, Clone, Copy)]
struct GvarHeader {
    axis_count: u16,
    shared_tuple_count: u16,
    shared_tuples_off: u32,
    glyph_count: u16,
    long_offsets: bool,
    data_array_off: u32,
    glyph_offsets_start: usize,
}

fn parse_gvar_header(bytes: &[u8]) -> Result<GvarHeader, SubsetError> {
    if bytes.len() < 20 {
        return Err(SubsetError::Unsupported("gvar partial: header too short"));
    }
    let major = u16::from_be_bytes([bytes[0], bytes[1]]);
    if major != 1 {
        return Err(SubsetError::Unsupported("gvar partial: major != 1"));
    }
    let axis_count = u16::from_be_bytes([bytes[4], bytes[5]]);
    let shared_tuple_count = u16::from_be_bytes([bytes[6], bytes[7]]);
    let shared_tuples_off = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    let glyph_count = u16::from_be_bytes([bytes[12], bytes[13]]);
    let flags = u16::from_be_bytes([bytes[14], bytes[15]]);
    let data_array_off = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let long_offsets = flags & 0x0001 != 0;
    let glyph_offsets_start = 20usize;
    Ok(GvarHeader {
        axis_count,
        shared_tuple_count,
        shared_tuples_off,
        glyph_count,
        long_offsets,
        data_array_off,
        glyph_offsets_start,
    })
}

/// Reads the source's shared tuple list as `Vec<peak>`, where each
/// peak is one f32 per axis (length = `axis_count`).
fn read_shared_tuples(bytes: &[u8], header: &GvarHeader) -> Result<Vec<Vec<f32>>, SubsetError> {
    let count = usize::from(header.shared_tuple_count);
    let axis_count = usize::from(header.axis_count);
    let base = header.shared_tuples_off as usize;
    let end = count
        .checked_mul(axis_count)
        .and_then(|n| n.checked_mul(2))
        .and_then(|len| base.checked_add(len))
        .ok_or(SubsetError::Unsupported(
            "gvar partial: shared tuples size overflow",
        ))?;
    let data = bytes.get(base..end).ok_or(SubsetError::Unsupported(
        "gvar partial: shared tuples past end",
    ))?;
    if axis_count == 0 {
        return Ok(alloc::vec![Vec::new(); count]);
    }
    Ok(data
        .chunks_exact(axis_count * 2)
        .map(|tuple| {
            tuple
                .chunks_exact(2)
                .map(|raw| f32::from(i16::from_be_bytes([raw[0], raw[1]])) / 16384.0)
                .collect()
        })
        .collect())
}

/// Pulls the source `GlyphVariationData` body for `gid` out of the
/// gvar bytes. Returns an empty slice for missing / empty entries.
fn pull_glyph_body<'a>(bytes: &'a [u8], header: &GvarHeader, gid: u16) -> &'a [u8] {
    if gid >= header.glyph_count {
        return &[];
    }
    let entry_size: usize = if header.long_offsets { 4 } else { 2 };
    let off_a = header.glyph_offsets_start + usize::from(gid) * entry_size;
    // Both offsets are read from the array entry pair at `off_a`.
    let Some(pair) = bytes.get(off_a..).and_then(|b| b.get(..entry_size * 2)) else {
        return &[];
    };
    let (start, end) = match *pair {
        [a0, a1, a2, a3, b0, b1, b2, b3] => (
            u32::from_be_bytes([a0, a1, a2, a3]),
            u32::from_be_bytes([b0, b1, b2, b3]),
        ),
        [a0, a1, b0, b1] => (
            u32::from(u16::from_be_bytes([a0, a1])) * 2,
            u32::from(u16::from_be_bytes([b0, b1])) * 2,
        ),
        _ => return &[],
    };
    if end <= start {
        return &[];
    }
    let data_array_off = header.data_array_off as usize;
    let body = data_array_off
        .checked_add(start as usize)
        .zip(data_array_off.checked_add(end as usize))
        .and_then(|(body_start, body_end)| bytes.get(body_start..body_end));
    body.unwrap_or_default()
}

/// The source's shared tuple peaks plus their precomputed projections
/// (for tuples that carry no intermediate region of their own).
struct SharedTuples<'a> {
    peaks: &'a [Vec<f32>],
    projections: &'a [Option<ProjectedRegion>],
}

/// A tuple region projected onto the Keep axes, in the shape the
/// rewritten tuple header needs.
#[derive(Debug, Clone)]
struct ProjectedRegion {
    /// Pin-axis support-scalar product.
    pin_scalar: f32,
    /// Keep-axis peak coordinates.
    peak: Vec<f32>,
    /// Keep-axis intermediate start / end, when the projected region
    /// differs from the default region its peak implies.
    intermediate: Option<(Vec<f32>, Vec<f32>)>,
    /// True when every Keep-axis peak is zero. Such a tuple has no
    /// variation left on the Keep axes and is dropped.
    all_zero_peak: bool,
}

/// Builds the source region of a tuple and projects it onto the Keep
/// axes. `intermediate` carries the tuple's own start / end vectors.
/// Returns `None` when the tuple contributes nothing at the pin coords.
fn project_tuple_region(
    peak: &[f32],
    intermediate: Option<(&[f32], &[f32])>,
    src_axis_count: u16,
    coords: &[F2Dot14],
    pins: &[AxisPin],
) -> Option<ProjectedRegion> {
    // Build the (start, peak, end) region per axis. The source's
    // intermediate region wins when present. Otherwise the spec says
    // it spans [0, peak] or [peak, 0].
    let mut region: Vec<(f32, f32, f32)> = Vec::with_capacity(usize::from(src_axis_count));
    for a in 0..usize::from(src_axis_count) {
        let p = peak.get(a).copied().unwrap_or(0.0);
        let (s, e) = match intermediate {
            Some((ss, ee)) => (
                ss.get(a).copied().unwrap_or(0.0),
                ee.get(a).copied().unwrap_or(0.0),
            ),
            None => {
                if p > 0.0 {
                    (0.0, p)
                } else {
                    (p, 0.0)
                }
            }
        };
        region.push((s, p, e));
    }

    let proj = project_region_onto_kept_axes(&region, pins, coords)?;

    // Build the new tuple's region (kept-axes only).
    let mut new_peak: Vec<f32> = Vec::with_capacity(proj.kept_axes.len());
    let mut new_int_start: Vec<f32> = Vec::with_capacity(proj.kept_axes.len());
    let mut new_int_end: Vec<f32> = Vec::with_capacity(proj.kept_axes.len());
    let mut needs_intermediate = false;
    for &(s, p, e) in &proj.kept_axes {
        new_peak.push(p);
        new_int_start.push(s);
        new_int_end.push(e);
        // Default region for `p` would be [0,p] (p>0) or [p,0]
        // (p<0). We need an explicit intermediate when (s,e)
        // doesn't match that default.
        let default_region = if p > 0.0 { (0.0, p) } else { (p, 0.0) };
        if (s - default_region.0).abs() > f32::EPSILON
            || (e - default_region.1).abs() > f32::EPSILON
        {
            needs_intermediate = true;
        }
    }
    let all_zero_peak = !new_peak.is_empty() && new_peak.iter().all(|&p| p == 0.0);
    Some(ProjectedRegion {
        pin_scalar: proj.pin_scalar,
        peak: new_peak,
        intermediate: needs_intermediate.then_some((new_int_start, new_int_end)),
        all_zero_peak,
    })
}

/// What one glyph's rewrite reads besides its variation data.
struct GlyphCtx<'a> {
    shared: &'a SharedTuples<'a>,
    src_axis_count: u16,
    coords: &'a [F2Dot14],
    pins: &'a [AxisPin],
    /// The glyph's default points, when the caller knows them.
    points: Option<&'a GlyphPoints>,
    /// The inference work left for the whole rewrite.
    work: &'a WorkBudget,
    /// The point deltas the whole rewrite may still decode.
    decode: &'a WorkBudget,
    warnings: &'a Warnings,
}

/// The `gvar` tag, for warnings.
const GVAR_TAG: [u8; 4] = *b"gvar";

impl GlyphCtx<'_> {
    /// Spends `units` of the rewrite's work, or reports that inferring
    /// tuples over every point stopped, so they keep their points.
    fn afford(&self, units: usize) -> bool {
        let ok = self.work.spend(units);
        if !ok {
            self.warnings.push(
                GVAR_TAG,
                0,
                "gvar partial: variation work exceeds its budget; tuples keep their points",
                "the inferred deltas of tuples whose default points moved",
            );
        }
        ok
    }
}

/// One source tuple that survives the projection, decoded.
struct Decoded<'b> {
    region: ProjectedRegion,
    /// The point numbers it lists; `None` when it covers every point.
    points: Option<Vec<u16>>,
    /// The source x deltas, one per listed point.
    xs: Vec<i32>,
    /// The source y deltas, one per listed point.
    ys: Vec<i32>,
    /// The tuple's own packed point numbers, when it has them.
    private_raw: Option<&'b [u8]>,
}

impl Decoded<'_> {
    /// The deltas scaled by the pinned axes' scalar, as floats.
    fn scaled(&self) -> impl Iterator<Item = (f32, f32)> + '_ {
        let s = self.region.pin_scalar;
        self.xs
            .iter()
            .zip(&self.ys)
            .map(move |(&x, &y)| (x as f32 * s, y as f32 * s))
    }

    /// The tuple's scaled deltas for all `count` points of the glyph
    /// whose default points `points` gives, the points it skips
    /// inferred from the source's default points.
    fn dense(&self, points: &GlyphPoints) -> Vec<(f32, f32)> {
        let scaled: Vec<(f32, f32)> = self.scaled().collect();
        match &self.points {
            Some(listed) => iup::infer(
                listed,
                |k| scaled.get(k).copied().unwrap_or((0.0, 0.0)),
                &points.before,
                &points.end_pts,
            ),
            None => {
                let mut out = alloc::vec![(0.0, 0.0); points.before.len() + 4];
                for (slot, d) in out.iter_mut().zip(&scaled) {
                    *slot = *d;
                }
                out
            }
        }
    }
}

/// A tuple about to be written: the first source tuple it stands for,
/// its deltas, and whether they cover every point of the glyph rather
/// than the points `first` lists.
struct Merged<'d, 'b> {
    first: &'d Decoded<'b>,
    sum: Vec<(f32, f32)>,
    dense: bool,
}

/// One tuple of the rewritten glyph.
struct Survivor {
    /// The tuple index field: embedded peak, plus the intermediate and
    /// private point flags it needs.
    new_tuple_index: u16,
    region: ProjectedRegion,
    /// Private point numbers (when it has them) and packed deltas.
    payload: Vec<u8>,
}

/// Rewrites a single glyph's `GlyphVariationData` body. Every
/// surviving tuple is written with an embedded peak (no shared-tuple
/// references) so the output's `sharedTupleCount = 0` is consistent.
///
/// Tuples whose projected regions match merge into one, their scaled
/// deltas summed and rounded once, as HarfBuzz's instancer merges
/// them: a tuple per region keeps the rounding of a merged delta to
/// half a unit. Tuples listing the same points merge into a tuple
/// listing those points; others merge into a tuple covering every
/// point, the points each skipped inferred from the source's default
/// points. A tuple that merges with nothing keeps its source points. A
/// sparse tuple whose inferred deltas the moved default points would
/// change gets every point instead (see the module docs).
fn rewrite_glyph_body(body: &[u8], glyph: &GlyphCtx<'_>) -> Result<Vec<u8>, SubsetError> {
    let Some((&[tvc_hi, tvc_lo, _, _], _)) = body.split_first_chunk::<4>() else {
        return Err(SubsetError::Unsupported(
            "gvar partial: glyph body too short",
        ));
    };
    let has_shared_points = u16::from_be_bytes([tvc_hi, tvc_lo]) & SHARED_POINTS_FLAG != 0;
    let (decoded, shared_points_raw) = decode_tuples(body, glyph)?;

    // Group the survivors by projected region, in order of first
    // appearance.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_of: BTreeMap<RegionKey, usize> = BTreeMap::new();
    for (i, d) in decoded.iter().enumerate() {
        let next = groups.len();
        let g = *group_of.entry(RegionKey::of(&d.region)).or_insert(next);
        match groups.get_mut(g) {
            Some(members) => members.push(i),
            None => groups.push(alloc::vec![i]),
        }
    }

    let mut survivors: Vec<Survivor> = Vec::with_capacity(groups.len());
    let mut keep_shared_points = false;
    for members in &groups {
        let first = &decoded[members[0]];
        let same_points = members
            .iter()
            .all(|&m| decoded[m].points == first.points && decoded[m].xs.len() == first.xs.len());
        // The merged deltas: per listed point when the members list the
        // same points, else per point of the glyph.
        let merged: Option<(Vec<(f32, f32)>, bool)> = if members.len() == 1 || same_points {
            let mut sum = alloc::vec![(0.0_f32, 0.0_f32); first.xs.len()];
            for &m in members {
                for (slot, d) in sum.iter_mut().zip(decoded[m].scaled()) {
                    slot.0 += d.0;
                    slot.1 += d.1;
                }
            }
            Some((sum, false))
        } else if let Some(points) = glyph
            .points
            .filter(|p| glyph.afford(members.len().saturating_mul(p.before.len() + 4)))
        {
            let mut sum = alloc::vec![(0.0_f32, 0.0_f32); points.before.len() + 4];
            for &m in members {
                for (slot, d) in sum.iter_mut().zip(decoded[m].dense(points)) {
                    slot.0 += d.0;
                    slot.1 += d.1;
                }
            }
            Some((sum, true))
        } else {
            None
        };
        // Without default points to infer merged deltas from, or the
        // work to infer them, the members stay apart.
        let tuples: Vec<Merged<'_, '_>> = match merged {
            Some((sum, dense)) => alloc::vec![Merged { first, sum, dense }],
            None => members
                .iter()
                .map(|&m| Merged {
                    first: &decoded[m],
                    sum: decoded[m].scaled().collect(),
                    dense: false,
                })
                .collect(),
        };
        for Merged {
            first: d,
            sum,
            dense,
        } in tuples
        {
            // A tuple whose deltas all round to zero moves nothing.
            if sum
                .iter()
                .all(|&(x, y)| round_half_up(x) == 0 && round_half_up(y) == 0)
            {
                continue;
            }
            let tuple = if dense {
                dense_payload(&sum).map(|payload| (payload, true))
            } else {
                // Checking a sparse tuple of a moved glyph infers it
                // over every point twice and may write every point.
                let check = glyph.points.filter(|p| {
                    p.after.is_none()
                        || d.points.is_none()
                        || glyph.afford(3usize.saturating_mul(p.before.len() + 4))
                });
                sparse_tuple(d, &sum, check)
            };
            let (payload, private) = tuple.ok_or(SubsetError::Unsupported(
                "gvar partial: tuple payload exceeds u16 size",
            ))?;
            survivors.push(survivor(&d.region, payload, private)?);
            keep_shared_points |= !private && has_shared_points;
        }
    }

    // If no tuples survive, the glyph has no variation. Emit empty.
    if survivors.is_empty() {
        return Ok(Vec::new());
    }

    // The header block: tuple count, data offset, and every tuple
    // header. Its end is the u16 data offset.
    let header_block_len = survivors.iter().fold(4usize, |len, s| {
        let axes_per_header = if s.region.intermediate.is_some() {
            3
        } else {
            1
        };
        len + 4 + 2 * axes_per_header * s.region.peak.len()
    });
    let data_offset = u16::try_from(header_block_len).map_err(|_| {
        SubsetError::Unsupported("gvar partial: tuple headers exceed the u16 data offset")
    })?;

    // ---- Emit ----
    let mut out: Vec<u8> = Vec::new();

    // Header word: tuple count + sharedPoints flag.
    let mut tvc_word: u16 = survivors.len() as u16 & TUPLE_COUNT_MASK;
    if keep_shared_points {
        tvc_word |= SHARED_POINTS_FLAG;
    }
    out.extend_from_slice(&tvc_word.to_be_bytes());
    out.extend_from_slice(&data_offset.to_be_bytes());

    // Tuple variation headers.
    for s in &survivors {
        out.extend_from_slice(&(s.payload.len() as u16).to_be_bytes());
        out.extend_from_slice(&s.new_tuple_index.to_be_bytes());
        for &p in &s.region.peak {
            write_f2dot14(&mut out, p);
        }
        if let Some((starts, ends)) = &s.region.intermediate {
            for &v in starts {
                write_f2dot14(&mut out, v);
            }
            for &v in ends {
                write_f2dot14(&mut out, v);
            }
        }
    }
    debug_assert_eq!(out.len(), header_block_len);

    // Shared point numbers (if any survivor still references them).
    if keep_shared_points {
        if let Some(sp_raw) = shared_points_raw {
            out.extend_from_slice(sp_raw);
        }
    }

    // Per-tuple payloads.
    for s in &survivors {
        out.extend_from_slice(&s.payload);
    }

    Ok(out)
}

/// A projected region as written: its F2DOT14 peak, then its
/// intermediate start and end when it has them. Tuples merge when
/// their keys match.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RegionKey(Vec<i16>);

impl RegionKey {
    fn of(region: &ProjectedRegion) -> Self {
        let raw = |v: f32| f2dot14_raw(v);
        let mut key: Vec<i16> = region.peak.iter().map(|&p| raw(p)).collect();
        if let Some((starts, ends)) = &region.intermediate {
            key.push(i16::MIN); // Keeps a peak-only key apart.
            key.extend(starts.iter().chain(ends).map(|&v| raw(v)));
        }
        Self(key)
    }
}

/// The payload of a tuple listing `first`'s points (all of them when
/// it lists none) with the deltas `sum`, rounded, and whether it has
/// private point numbers. A sparse tuple of a glyph whose default
/// points moved is written with every point instead when its inferred
/// deltas would drift. `None` when the payload outgrows its u16 size.
fn sparse_tuple(
    first: &Decoded<'_>,
    sum: &[(f32, f32)],
    points: Option<&GlyphPoints>,
) -> Option<(Vec<u8>, bool)> {
    let xs: Vec<i32> = sum.iter().map(|d| round_half_up(d.0)).collect();
    let ys: Vec<i32> = sum.iter().map(|d| round_half_up(d.1)).collect();
    let moved = points.and_then(|p| p.after.as_ref().map(|after| (p, after)));
    if let (Some((points, after)), Some(listed)) = (moved, first.points.as_deref()) {
        let tuple = SparseTuple {
            points: listed,
            deltas: sum,
        };
        if let Some(dense) = tuple.densified_if_drifting(points, after, &xs, &ys) {
            // A glyph too large for every point keeps the sparse form.
            if let Some(payload) = dense_payload(&dense) {
                return Some((payload, true));
            }
        }
    }
    let mut payload: Vec<u8> = Vec::new();
    if let Some(raw) = first.private_raw {
        payload.extend_from_slice(raw);
    }
    encode_packed_deltas(&xs, &mut payload);
    encode_packed_deltas(&ys, &mut payload);
    (payload.len() <= usize::from(u16::MAX)).then_some((payload, first.private_raw.is_some()))
}

/// The payload of a tuple covering every point with the deltas `sum`,
/// rounded: private point numbers naming them all, then the deltas.
/// `None` when it outgrows its u16 size.
fn dense_payload(sum: &[(f32, f32)]) -> Option<Vec<u8>> {
    let xs: Vec<i32> = sum.iter().map(|d| round_half_up(d.0)).collect();
    let ys: Vec<i32> = sum.iter().map(|d| round_half_up(d.1)).collect();
    let mut payload = alloc::vec![0u8];
    encode_packed_deltas(&xs, &mut payload);
    encode_packed_deltas(&ys, &mut payload);
    (payload.len() <= usize::from(u16::MAX)).then_some(payload)
}

/// The survivor for `region` with `payload`, flagged when the payload
/// starts with private point numbers. Without them the tuple reads the
/// glyph's shared point numbers, or covers every point when the glyph
/// has none.
fn survivor(
    region: &ProjectedRegion,
    payload: Vec<u8>,
    private: bool,
) -> Result<Survivor, SubsetError> {
    // The variationDataSize field is a u16. The spec imposes no limit
    // beyond that.
    if payload.len() > usize::from(u16::MAX) {
        return Err(SubsetError::Unsupported(
            "gvar partial: tuple payload exceeds u16 size",
        ));
    }
    // Embedded-peak is always set; the low 12 bits are unused in the
    // new layout (no shared-tuple references).
    let mut new_tuple_index: u16 = FLAG_EMBEDDED_PEAK;
    if region.intermediate.is_some() {
        new_tuple_index |= FLAG_INTERMEDIATE_REGION;
    }
    if private {
        new_tuple_index |= FLAG_PRIVATE_POINT_NUMBERS;
    }
    Ok(Survivor {
        new_tuple_index,
        region: region.clone(),
        payload,
    })
}

/// Decodes every tuple of the glyph `body` that survives the
/// projection, in order, and returns them with the glyph's packed
/// shared point numbers. Tuples whose region drops at the pin
/// coordinates, or keeps no kept-axis peak (the instance baked those
/// into the default outline), are skipped.
fn decode_tuples<'b>(
    body: &'b [u8],
    glyph: &GlyphCtx<'_>,
) -> Result<(Vec<Decoded<'b>>, Option<&'b [u8]>), SubsetError> {
    let (shared, src_axis_count, coords, pins) =
        (glyph.shared, glyph.src_axis_count, glyph.coords, glyph.pins);
    let Some((&[tvc_hi, tvc_lo, off_hi, off_lo], mut header_bytes)) = body.split_first_chunk::<4>()
    else {
        return Err(SubsetError::Unsupported(
            "gvar partial: glyph body too short",
        ));
    };
    let tvc = u16::from_be_bytes([tvc_hi, tvc_lo]);
    let tuple_count = (tvc & TUPLE_COUNT_MASK) as usize;
    let has_shared_points = tvc & SHARED_POINTS_FLAG != 0;
    let data_off = usize::from(u16::from_be_bytes([off_hi, off_lo]));

    // Read tuple variation headers.
    let mut headers: Vec<ParsedTupleHeader> = Vec::with_capacity(tuple_count);
    for _ in 0..tuple_count {
        let (h, used) = parse_tuple_header(header_bytes, src_axis_count)?;
        headers.push(h);
        header_bytes = header_bytes.get(used..).unwrap_or_default();
    }

    let data_region = body.get(data_off..).ok_or(SubsetError::Unsupported(
        "gvar partial: data offset past body end",
    ))?;

    // Read shared point numbers (raw bytes preserved). The source's
    // packed-points block is copied verbatim into the output when any
    // surviving tuple references it; that keeps the encoding bit-
    // identical when nothing changes.
    let mut data_cursor = 0usize;
    let shared_points_raw: Option<&[u8]> = if has_shared_points {
        let used = packed_point_numbers_byte_len(data_region)?;
        let raw = data_region.get(..used).unwrap_or_default();
        data_cursor = used;
        Some(raw)
    } else {
        None
    };
    // Shared point numbers, parsed the first time a tuple needs them.
    // Empty means every point.
    let mut shared_points: Option<Vec<u16>> = None;

    let mut out: Vec<Decoded<'b>> = Vec::with_capacity(tuple_count);
    for header in &headers {
        let tuple_data_len = header.variation_data_size as usize;
        let tuple_bytes = data_region
            .get(data_cursor..)
            .and_then(|rest| rest.get(..tuple_data_len))
            .ok_or(SubsetError::Unsupported(
                "gvar partial: tuple data region truncated",
            ))?;
        data_cursor += tuple_data_len;

        // Resolve and project the region. Tuples that point at a
        // shared peak and carry no intermediate region reuse the
        // projection computed once for that peak.
        let intermediate = header
            .intermediate_start
            .as_deref()
            .zip(header.intermediate_end.as_deref());
        // A `None` projection means the tuple drops: skip it.
        let region: ProjectedRegion = match &header.embedded_peak {
            Some(peak) => {
                match project_tuple_region(peak, intermediate, src_axis_count, coords, pins) {
                    Some(region) => region,
                    None => continue,
                }
            }
            None => {
                let idx = usize::from(header.tuple_index & TUPLE_INDEX_MASK);
                let Some(peak) = shared.peaks.get(idx) else {
                    // Malformed: drop the tuple silently.
                    continue;
                };
                let projected = if intermediate.is_some() {
                    project_tuple_region(peak, intermediate, src_axis_count, coords, pins)
                } else {
                    shared.projections.get(idx).cloned().flatten()
                };
                match projected {
                    Some(region) => region,
                    None => continue,
                }
            }
        };

        // Decode the tuple's deltas.
        let mut tr = 0usize;
        // Private points (when the tuple's flag is set). We preserve
        // the raw bytes too so the output emits the same packed form.
        let (private_points, private_raw) = if header.private_point_numbers {
            let used = packed_point_numbers_byte_len(tuple_bytes)?;
            let raw = tuple_bytes.get(..used).unwrap_or_default();
            tr = used;
            (Some(parse_packed_point_numbers(raw)?), Some(raw))
        } else {
            (None, None)
        };
        let deltas_bytes = tuple_bytes.get(tr..).unwrap_or_default();

        // The points the tuple lists: its own, the glyph's shared
        // ones, or (an empty list, or none at all) every point.
        let listed: Option<Vec<u16>> = match private_points {
            Some(points) => Some(points),
            None if has_shared_points => {
                if shared_points.is_none() {
                    shared_points = Some(parse_packed_point_numbers(
                        shared_points_raw.unwrap_or_default(),
                    )?);
                }
                shared_points.clone()
            }
            None => None,
        }
        .filter(|points| !points.is_empty());

        // The all-points form packs as many deltas as the glyph has
        // points; recover the count from the delta stream itself.
        // A tuple left with no Keep-axis peak applies everywhere alike:
        // the instance baked its deltas into the default outline, so
        // it goes, undecoded.
        if region.all_zero_peak {
            continue;
        }
        let n = match &listed {
            Some(points) => points.len(),
            None => count_packed_deltas(deltas_bytes)?,
        };
        if !glyph.decode.spend(n.saturating_mul(2).saturating_add(1)) {
            return Err(SubsetError::Unsupported(OVER_BUDGET));
        }
        let (xs, used_x) = read_packed_deltas_n(deltas_bytes, n)?;
        let (ys, _used_y) =
            read_packed_deltas_n(deltas_bytes.get(used_x..).unwrap_or_default(), n)?;
        out.push(Decoded {
            region,
            points: listed,
            xs,
            ys,
            private_raw,
        });
    }
    Ok((out, shared_points_raw))
}

#[cfg(test)]
mod tests;
