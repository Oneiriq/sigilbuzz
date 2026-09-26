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
//! 3. Output `sharedTupleCount = 0`. Per-glyph emit reproduces the
//!    source's shared-points / per-tuple-private-points structure
//!    verbatim: packed point numbers are copied byte-for-byte; only
//!    the delta payloads (and the headers around them) change.
//! 4. The header's `axisCount` becomes `new_axis_count`; the header's
//!    `glyphCount` is the source's verbatim (instancing keeps every
//!    glyph).
//!
//! All-`Keep` short-circuit: a no-op partial pins zero axes; the
//! source bytes pass through verbatim.
//!
//! # Determinism
//!
//! Every floating-point round goes through `f32::round()`; tuple
//! ordering matches the source. Packed-point streams are copied
//! byte-for-byte from the source so their (compressible) layout
//! choice is preserved when the deltas survive at all.

use alloc::vec::Vec;

use crate::instance::{project_region_onto_kept_axes, AxisPin, F2Dot14};
use crate::SubsetError;

mod tuple;

use tuple::{
    count_packed_deltas, encode_packed_deltas, packed_point_numbers_byte_len,
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

/// Bakes a partial-instancing rewrite of `gvar` against `coords` and
/// `pins`, dropping every `Pin`-axis dimension from the file-wide axis
/// count and every tuple region while scaling the per-point deltas by
/// the corresponding `axis_support_scalar` product.
///
/// `new_axis_count` must equal the count of `AxisPin::Keep` entries in
/// `pins` (the caller derives both from a single source-axis vector).
///
/// Returns the new `gvar` bytes. When every axis is `AxisPin::Keep`
/// the source bytes pass through verbatim. No re-emit is needed.
pub(crate) fn bake_gvar_partial(
    gvar_bytes: &[u8],
    coords: &[F2Dot14],
    pins: &[AxisPin],
    new_axis_count: u16,
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
            rewrite_glyph_body(body, &tuples, header.axis_count, coords, pins)?
        };
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

/// Rewrites a single glyph's `GlyphVariationData` body. The output
/// keeps the source's shared / private-points structure but emits
/// every surviving tuple with an embedded peak (no shared-tuple
/// references) so the output's `sharedTupleCount = 0` is consistent.
fn rewrite_glyph_body(
    body: &[u8],
    shared: &SharedTuples<'_>,
    src_axis_count: u16,
    coords: &[F2Dot14],
    pins: &[AxisPin],
) -> Result<Vec<u8>, SubsetError> {
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
    // Shared point count, parsed the first time a tuple needs it.
    let mut shared_point_count: Option<usize> = None;

    // Walk each tuple. Every survivor keeps its rewritten header
    // fields plus its pre-encoded data payload (private points +
    // packed x deltas + packed y deltas).
    struct Survivor {
        // The new tuple_index field for the output (with embedded-peak
        // bit set, optional intermediate / private-point bits copied
        // from source).
        new_tuple_index: u16,
        region: ProjectedRegion,
        payload: Vec<u8>,
    }

    let mut survivors: Vec<Survivor> = Vec::with_capacity(tuple_count);
    let mut keep_shared_points = false;
    // Size of the output header block (tuple count, data offset, and
    // every tuple header). Its end is the u16 data offset.
    let mut header_block_len = 4usize;

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
        let computed: Option<ProjectedRegion>;
        let region: &ProjectedRegion = match &header.embedded_peak {
            Some(peak) => {
                computed = project_tuple_region(peak, intermediate, src_axis_count, coords, pins);
                match &computed {
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
                if intermediate.is_some() {
                    computed =
                        project_tuple_region(peak, intermediate, src_axis_count, coords, pins);
                    match &computed {
                        Some(region) => region,
                        None => continue,
                    }
                } else {
                    match shared.projections.get(idx) {
                        Some(Some(region)) => region,
                        _ => continue,
                    }
                }
            }
        };

        // Decode the tuple's deltas.
        let mut tr = 0usize;
        // Private points (when the tuple's flag is set). We preserve
        // the raw bytes too so the output emits the same packed form.
        let (private_points, private_points_raw, tuple_is_all_points) =
            if header.private_point_numbers {
                let used = packed_point_numbers_byte_len(tuple_bytes)?;
                let raw = tuple_bytes.get(..used).unwrap_or_default();
                let pts = parse_packed_point_numbers(raw)?;
                let all_pts = pts.is_empty();
                tr = used;
                (Some(pts), Some(raw), all_pts)
            } else {
                (None, None, false)
            };
        let deltas_bytes = tuple_bytes.get(tr..).unwrap_or_default();

        // Resolve the effective point count.
        let n: usize = if header.private_point_numbers {
            if tuple_is_all_points {
                // All-points: caller-provided num_points needed, but
                // we don't have it here. Fortunately the packed-deltas
                // decoder eats whatever the headers say; we recover
                // n by decoding "all available" from the byte stream.
                count_packed_deltas(deltas_bytes)?
            } else {
                private_points.as_ref().map_or(0, Vec::len)
            }
        } else if has_shared_points {
            // shared_points: same logic. We need the count.
            let count = match shared_point_count {
                Some(count) => count,
                None => {
                    let count =
                        parse_packed_point_numbers(shared_points_raw.unwrap_or_default())?.len();
                    shared_point_count = Some(count);
                    count
                }
            };
            if count == 0 {
                count_packed_deltas(deltas_bytes)?
            } else {
                count
            }
        } else {
            // No point lists at all: spec says this is the all-
            // points case. Recover n from the delta stream byte
            // length.
            count_packed_deltas(deltas_bytes)?
        };

        let (xs, used_x) = read_packed_deltas_n(deltas_bytes, n)?;
        let (ys, _used_y) =
            read_packed_deltas_n(deltas_bytes.get(used_x..).unwrap_or_default(), n)?;

        // Drop survivors whose new region collapses to no-contribution
        // on every Keep axis (every peak is zero: the kept-axis
        // tuple is a no-op). The static contribution at the pin
        // coords is not carried by the rewritten gvar.
        if region.all_zero_peak {
            continue;
        }

        // Scale by pin_scalar and pre-encode the tuple's data region
        // so its variationDataSize is known.
        let scalar = region.pin_scalar;
        let scale = |v: i32| -> i32 {
            let scaled = (v as f32 * scalar).round() as i64;
            scaled.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
        };
        let x_scaled: Vec<i32> = xs.iter().copied().map(scale).collect();
        let y_scaled: Vec<i32> = ys.iter().copied().map(scale).collect();
        let mut payload: Vec<u8> = Vec::new();
        if let Some(raw) = private_points_raw {
            payload.extend_from_slice(raw);
        }
        encode_packed_deltas(&x_scaled, &mut payload);
        encode_packed_deltas(&y_scaled, &mut payload);
        // The variationDataSize field is a u16. The spec imposes no limit
        // beyond that.
        if payload.len() > usize::from(u16::MAX) {
            return Err(SubsetError::Unsupported(
                "gvar partial: tuple payload exceeds u16 size",
            ));
        }

        // Build the new tuple_index. Embedded-peak is always set; the
        // low 12 bits are unused in the new layout (no shared-tuple
        // references).
        let mut new_idx: u16 = FLAG_EMBEDDED_PEAK;
        if region.intermediate.is_some() {
            new_idx |= FLAG_INTERMEDIATE_REGION;
        }
        if header.private_point_numbers {
            new_idx |= FLAG_PRIVATE_POINT_NUMBERS;
        } else if has_shared_points {
            // Mark that this glyph still uses shared points.
            keep_shared_points = true;
        }

        // The data offset that follows the tuple headers is a u16.
        // A header block past it cannot be encoded.
        let axes_per_header = if region.intermediate.is_some() { 3 } else { 1 };
        header_block_len += 4 + 2 * axes_per_header * region.peak.len();
        if header_block_len > usize::from(u16::MAX) {
            return Err(SubsetError::Unsupported(
                "gvar partial: tuple headers exceed the u16 data offset",
            ));
        }

        survivors.push(Survivor {
            new_tuple_index: new_idx,
            region: region.clone(),
            payload,
        });
    }

    // If no tuples survive, the glyph has no variation. Emit empty.
    if survivors.is_empty() {
        return Ok(Vec::new());
    }

    // ---- Emit ----
    let mut out: Vec<u8> = Vec::new();

    // Header word: tuple count + sharedPoints flag.
    let mut tvc_word: u16 = survivors.len() as u16 & TUPLE_COUNT_MASK;
    if keep_shared_points {
        tvc_word |= SHARED_POINTS_FLAG;
    }
    out.extend_from_slice(&tvc_word.to_be_bytes());
    // dataOffset: the header block size, checked against u16 above.
    out.extend_from_slice(&(header_block_len as u16).to_be_bytes());

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

#[cfg(test)]
mod tests;
