//! `cvt ` and `cvar` at an instance.
//!
//! The glyph instructions an instance keeps read `cvt `, and `cvar`
//! varies it. As HarfBuzz's instancer does, a full instance adds the
//! `cvar` deltas at the coordinates to the `cvt ` values, and the
//! `cvar` goes. A partial instance adds the deltas of the tuples left
//! on the pinned axes only; the other tuples stay in a `cvar` rebuilt
//! for the kept axes, their deltas scaled by the pinned axes' scalar
//! and the tuples whose regions now match merged into one. Every value
//! is summed as a float and rounds once, halves up.
//!
//! A `cvar` that cannot be read leaves `cvt ` as the source has it and
//! is left out, with a warning. The work is bounded by the deltas the
//! table packs: a tuple whose region drops at the coordinates is not
//! decoded at all.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use sigilbuzz::Face;

use super::{project_region_onto_kept_axes, AxisPin};
use crate::gvar_partial::tuple::{
    encode_packed_deltas, f2dot14_raw, packed_point_numbers_byte_len, parse_packed_point_numbers,
    parse_tuple_header, read_packed_deltas_n,
};
use crate::util::round_half_up;
use crate::warnings::Warnings;

/// The control value table.
pub(super) const CVT: [u8; 4] = *b"cvt ";
/// Its variations.
pub(super) const CVAR: [u8; 4] = *b"cvar";

const SHARED_POINT_NUMBERS: u16 = 0x8000;
const TUPLE_COUNT_MASK: u16 = 0x0FFF;
const EMBEDDED_PEAK: u16 = 0x8000;
const INTERMEDIATE_REGION: u16 = 0x4000;
const PRIVATE_POINT_NUMBERS: u16 = 0x2000;
/// The most points a packed point number list names.
const MAX_LISTED: usize = 0x7FFF;

/// The `cvt ` and `cvar` of an instance.
#[derive(Debug, Default, PartialEq)]
pub(super) struct CvtBake {
    /// The `cvt ` with the deltas added; `None` keeps the source's.
    pub(super) cvt: Option<Vec<u8>>,
    /// The `cvar` for the kept axes; `None` leaves it out.
    pub(super) cvar: Option<Vec<u8>>,
}

/// Bakes the `cvar` of `face` into its `cvt ` at the post-avar
/// `coords`, the axes `pins` keeps staying variable (every axis pins
/// when `pins` is empty). `None` when the font has no `cvar`.
pub(super) fn bake_cvt(
    face: &Face<'_>,
    coords: &[f32],
    pins: &[AxisPin],
    warnings: &Warnings,
) -> Option<CvtBake> {
    let cvar = face.table_bytes(CVAR).ok()?;
    // Without a `cvt ` there is nothing for `cvar` to vary.
    let Ok(cvt) = face.table_bytes(CVT) else {
        return Some(CvtBake::default());
    };
    let all_pinned;
    let pins = if pins.is_empty() {
        all_pinned = alloc::vec![AxisPin::Pin; coords.len()];
        &all_pinned
    } else {
        pins
    };
    match rebuild(cvt, cvar, coords, pins) {
        Ok(bake) => Some(bake),
        Err(context) => {
            warnings.push(CVAR, 0, context, "the whole table");
            Some(CvtBake::default())
        }
    }
}

/// What a `cvar` that cannot be read reports.
const MALFORMED: &str = "cvar tuple data is truncated or malformed";

/// One tuple of the rebuilt `cvar`.
struct Kept {
    /// The kept axes' peaks, then their starts and ends when the
    /// region needs them.
    region: Vec<f32>,
    intermediate: bool,
    /// The scaled nonzero deltas, summed by `cvt ` index.
    deltas: Deltas,
}

/// The deltas of one rebuilt tuple: a list of `(index, delta)` while it
/// is short, one value per `cvt ` entry once that is smaller. Either way
/// it holds at most 8 bytes per nonzero delta the source packs.
enum Deltas {
    Sparse(Vec<(u32, f32)>),
    Dense(Vec<f32>),
}

impl Deltas {
    /// Adds `d` to entry `index` of a `cvt ` of `num_cvt` values.
    fn add(&mut self, index: usize, d: f32, num_cvt: usize) {
        match self {
            Self::Sparse(list) => {
                list.push((index as u32, d));
                if list.len() > num_cvt / 2 {
                    let mut dense = alloc::vec![0.0f32; num_cvt];
                    for &(i, v) in list.iter() {
                        if let Some(slot) = dense.get_mut(i as usize) {
                            *slot += v;
                        }
                    }
                    *self = Self::Dense(dense);
                }
            }
            Self::Dense(values) => {
                if let Some(slot) = values.get_mut(index) {
                    *slot += d;
                }
            }
        }
    }

    /// The summed deltas, rounded halves up, without the zeros, in
    /// index order.
    fn rounded(&self) -> Vec<(usize, i32)> {
        let mut out: Vec<(usize, i32)> = Vec::new();
        match self {
            Self::Sparse(list) => {
                // A stable sort keeps the deltas of one index in the
                // order they were added, so they sum the same way.
                let mut sorted = list.clone();
                sorted.sort_by_key(|&(i, _)| i);
                let mut k = 0;
                while let Some(&(index, first)) = sorted.get(k) {
                    let mut sum = first;
                    k += 1;
                    while let Some(&(i, v)) = sorted.get(k) {
                        if i != index {
                            break;
                        }
                        sum += v;
                        k += 1;
                    }
                    out.push((index as usize, round_half_up(sum)));
                }
            }
            Self::Dense(values) => {
                out.extend(
                    values
                        .iter()
                        .enumerate()
                        .map(|(i, &v)| (i, round_half_up(v))),
                );
            }
        }
        out.retain(|&(_, d)| d != 0);
        out
    }
}

/// [`bake_cvt`] on the tables' bytes.
fn rebuild(
    cvt: &[u8],
    cvar: &[u8],
    coords: &[f32],
    pins: &[AxisPin],
) -> Result<CvtBake, &'static str> {
    let axis_count = u16::try_from(coords.len()).map_err(|_| MALFORMED)?;
    let num_cvt = cvt.len() / 2;
    let header = cvar.first_chunk::<8>().ok_or(MALFORMED)?;
    let word = |i: usize| u16::from_be_bytes([header[i], header[i + 1]]);
    if word(0) != 1 {
        return Err("cvar is not version 1");
    }
    let (count_word, data_offset) = (word(4), usize::from(word(6)));
    let mut headers = cvar.get(8..).unwrap_or_default();
    let data = cvar.get(data_offset..).ok_or(MALFORMED)?;
    let mut cursor = 0usize;
    let shared_points = if count_word & SHARED_POINT_NUMBERS == 0 {
        Vec::new()
    } else {
        let used = packed_point_numbers_byte_len(data).map_err(|_| MALFORMED)?;
        cursor = used;
        parse_packed_point_numbers(data).map_err(|_| MALFORMED)?
    };

    let mut defaults: Vec<f32> = alloc::vec![0.0; num_cvt];
    let mut kept: Vec<Kept> = Vec::new();
    let mut kept_at: BTreeMap<Vec<i16>, usize> = BTreeMap::new();
    for _ in 0..count_word & TUPLE_COUNT_MASK {
        // A header that cannot be read, or whose data size runs past the
        // table from where the header starts, ends the tuples, as
        // HarfBuzz's tuple iterator stops at it; the ones before it
        // still apply.
        let at = cvar.len() - headers.len();
        let Ok((tuple, used)) = parse_tuple_header(headers, axis_count) else {
            break;
        };
        let reach = usize::from(tuple.variation_data_size).max(used);
        if at.saturating_add(reach) > cvar.len() {
            break;
        }
        headers = headers.get(used..).unwrap_or_default();
        let end = cursor
            .checked_add(usize::from(tuple.variation_data_size))
            .ok_or(MALFORMED)?;
        // Data that runs past the table ends the tuples too, as
        // HarfBuzz's partial instancer stops there; the earlier tuples
        // still apply.
        let Some(bytes) = data.get(cursor..end) else {
            break;
        };
        cursor = end;
        // `cvar` has no shared tuples: a tuple without its own peak
        // applies nowhere, as HarfBuzz reads it.
        let Some(peak) = &tuple.embedded_peak else {
            continue;
        };
        let intermediate = tuple
            .intermediate_start
            .as_deref()
            .zip(tuple.intermediate_end.as_deref());
        let region: Vec<(f32, f32, f32)> = (0..usize::from(axis_count))
            .map(|a| {
                let p = peak.get(a).copied().unwrap_or(0.0);
                match intermediate {
                    Some((s, e)) => (
                        s.get(a).copied().unwrap_or(0.0),
                        p,
                        e.get(a).copied().unwrap_or(0.0),
                    ),
                    None if p > 0.0 => (0.0, p, p),
                    None => (p, p, 0.0),
                }
            })
            .collect();
        let Some(projected) = project_region_onto_kept_axes(&region, pins, coords) else {
            continue;
        };

        // The `cvt ` indices the tuple lists; none means every one.
        let private;
        let (listed, packed): (&[u16], &[u8]) = if tuple.private_point_numbers {
            let used = packed_point_numbers_byte_len(bytes).map_err(|_| MALFORMED)?;
            private = parse_packed_point_numbers(bytes).map_err(|_| MALFORMED)?;
            (&private, bytes.get(used..).unwrap_or_default())
        } else {
            (&shared_points, bytes)
        };
        let n = if listed.is_empty() {
            num_cvt
        } else {
            listed.len()
        };
        let (deltas, _) = read_packed_deltas_n(packed, n).map_err(|_| MALFORMED)?;
        let scale = projected.pin_scalar;
        let scaled = deltas.iter().enumerate().filter_map(|(k, &d)| {
            let index = if listed.is_empty() {
                k
            } else {
                usize::from(*listed.get(k)?)
            };
            // A zero moves nothing, so it is not kept: what the rebuilt
            // tuples hold stays in proportion to the deltas the table
            // packs, not to the cvt it covers.
            (index < num_cvt && d != 0).then_some((index, d as f32 * scale))
        });

        // A tuple left with no kept-axis peak applies at every kept
        // coordinate alike: its deltas go into `cvt `.
        if projected.kept_axes.iter().all(|&(_, p, _)| p == 0.0) {
            for (index, d) in scaled {
                if let Some(slot) = defaults.get_mut(index) {
                    *slot += d;
                }
            }
            continue;
        }
        let (region, intermediate) = kept_region(&projected.kept_axes);
        let key: Vec<i16> = region.iter().map(|&v| f2dot14_raw(v)).collect();
        let next = kept.len();
        let at = *kept_at.entry(key).or_insert(next);
        if at == next {
            kept.push(Kept {
                region,
                intermediate,
                deltas: Deltas::Sparse(Vec::new()),
            });
        }
        if let Some(tuple) = kept.get_mut(at) {
            for (index, d) in scaled {
                tuple.deltas.add(index, d, num_cvt);
            }
        }
    }

    let mut new_cvt = cvt.to_vec();
    for (pair, d) in new_cvt.chunks_exact_mut(2).zip(&defaults) {
        let old = i16::from_be_bytes([pair[0], pair[1]]);
        let new = i32::from(old).wrapping_add(round_half_up(*d)) as i16;
        pair.copy_from_slice(&new.to_be_bytes());
    }
    Ok(CvtBake {
        cvt: Some(new_cvt),
        cvar: encode_cvar(&kept, num_cvt)?,
    })
}

/// The region a tuple of the kept axes `kept` (start, peak, end each)
/// writes: the peaks, then the starts and ends when they differ from
/// the region the peaks imply; and whether it has them.
fn kept_region(kept: &[(f32, f32, f32)]) -> (Vec<f32>, bool) {
    let implied = |p: f32| if p > 0.0 { (0.0, p) } else { (p, 0.0) };
    let intermediate = kept.iter().any(|&(s, p, e)| {
        let (is, ie) = implied(p);
        (s - is).abs() > f32::EPSILON || (e - ie).abs() > f32::EPSILON
    });
    let mut region: Vec<f32> = kept.iter().map(|&(_, p, _)| p).collect();
    if intermediate {
        region.extend(kept.iter().map(|&(s, _, _)| s));
        region.extend(kept.iter().map(|&(_, _, e)| e));
    }
    (region, intermediate)
}

/// The `cvar` holding the tuples `kept`, their deltas rounded; `None`
/// when no tuple moves a value.
fn encode_cvar(kept: &[Kept], num_cvt: usize) -> Result<Option<Vec<u8>>, &'static str> {
    let mut tuples: Vec<(&Kept, Vec<u8>)> = Vec::new();
    for tuple in kept {
        let rounded = tuple.deltas.rounded();
        if rounded.is_empty() {
            continue;
        }
        let payload = [sparse_payload(&rounded), dense_payload(&rounded, num_cvt)]
            .into_iter()
            .flatten()
            .filter(|p| p.len() <= usize::from(u16::MAX))
            .min_by_key(Vec::len)
            .ok_or("cvar tuple outgrows its size field")?;
        tuples.push((tuple, payload));
    }
    if tuples.is_empty() {
        return Ok(None);
    }
    let headers_len = tuples
        .iter()
        .fold(8usize, |len, (t, _)| len + 4 + 2 * t.region.len());
    let data_offset =
        u16::try_from(headers_len).map_err(|_| "cvar tuple headers outgrow their data offset")?;
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 1, 0, 0]);
    out.extend_from_slice(&(tuples.len() as u16).to_be_bytes());
    out.extend_from_slice(&data_offset.to_be_bytes());
    for (tuple, payload) in &tuples {
        let mut index = EMBEDDED_PEAK | PRIVATE_POINT_NUMBERS;
        if tuple.intermediate {
            index |= INTERMEDIATE_REGION;
        }
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        out.extend_from_slice(&index.to_be_bytes());
        for &v in &tuple.region {
            out.extend_from_slice(&f2dot14_raw(v).to_be_bytes());
        }
    }
    for (_, payload) in &tuples {
        out.extend_from_slice(payload);
    }
    Ok(Some(out))
}

/// The payload naming the indices of `rounded` with their deltas;
/// `None` when they are too many to name or an index is past what a
/// point number holds.
fn sparse_payload(rounded: &[(usize, i32)]) -> Option<Vec<u8>> {
    if rounded.len() > MAX_LISTED {
        return None;
    }
    let mut out = Vec::new();
    let count = rounded.len() as u16;
    if count < 0x80 {
        out.push(count as u8);
    } else {
        out.extend_from_slice(&(count | 0x8000).to_be_bytes());
    }
    let mut gaps = Vec::with_capacity(rounded.len());
    let mut last = 0usize;
    for &(index, _) in rounded {
        u16::try_from(index).ok()?;
        gaps.push((index - last) as u16);
        last = index;
    }
    for run in gaps.chunk_by(|a, b| (*a > 0xFF) == (*b > 0xFF)) {
        for chunk in run.chunks(128) {
            let words = chunk.iter().any(|&g| g > 0xFF);
            out.push(((chunk.len() - 1) as u8) | if words { 0x80 } else { 0 });
            for &g in chunk {
                if words {
                    out.extend_from_slice(&g.to_be_bytes());
                } else {
                    out.push(g as u8);
                }
            }
        }
    }
    let deltas: Vec<i32> = rounded.iter().map(|&(_, d)| d).collect();
    encode_packed_deltas(&deltas, &mut out);
    Some(out)
}

/// The payload giving every one of the `num_cvt` values a delta, those
/// `rounded` does not name zero.
fn dense_payload(rounded: &[(usize, i32)], num_cvt: usize) -> Option<Vec<u8>> {
    // Every value packs into at least one bit of the delta stream; a
    // table far past the size field cannot hold them.
    if num_cvt / 64 > usize::from(u16::MAX) {
        return None;
    }
    let mut deltas = alloc::vec![0i32; num_cvt];
    for &(index, d) in rounded {
        *deltas.get_mut(index)? = d;
    }
    let mut out = alloc::vec![0u8];
    encode_packed_deltas(&deltas, &mut out);
    Some(out)
}

#[cfg(test)]
mod tests;
