//! `gvar` — Glyph Variations table.
//!
//! Carries per-glyph outline deltas for a variable font. Where `HVAR`
//! varies a glyph's *advance* across the design space, `gvar` varies
//! every contour point: at coord `c` the rendered point at index `i`
//! is the base glyph's point `i` plus the weighted sum of per-region
//! `(dx, dy)` deltas for that point. sigilbuzz doesn't rasterize yet,
//! but `glyph_bounds_at_coords` needs the deltas to shift the glyph
//! bounding box — which is all this parser exposes today.
//!
//! # Layout
//!
//! ```text
//!   u16       majorVersion = 1
//!   u16       minorVersion = 0
//!   u16       axisCount
//!   u16       sharedTupleCount
//!   Offset32  sharedTuplesOffset
//!   u16       glyphCount
//!   u16       flags                (bit 0 = long offsets)
//!   Offset32  glyphVariationDataArrayOffset
//!   Offset  glyphVariationDataOffsets[glyphCount + 1]   (u16 halves or u32)
//! ```
//!
//! Each glyph's `GlyphVariationData`:
//!
//! ```text
//!   u16       tupleVariationCount   (low 12 bits = count, bit 15 = shared point numbers)
//!   Offset16  dataOffset            (from start of GlyphVariationData)
//!   TupleVariationHeader[count]
//!   (shared point numbers)
//!   per-tuple: (private point numbers)? + packed deltas (x then y)
//! ```
//!
//! A `TupleVariationHeader`:
//!
//! ```text
//!   u16       variationDataSize
//!   u16       tupleIndex   (low 12 bits = shared tuple index;
//!                           bit 15 = embedded peak tuple,
//!                           bit 14 = intermediate region,
//!                           bit 13 = private point numbers)
//!   F2DOT14   peakCoord[axisCount]           (only if bit 15 set)
//!   F2DOT14   intermediateStart[axisCount]   (only if bit 14 set)
//!   F2DOT14   intermediateEnd[axisCount]     (only if bit 14 set)
//! ```

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// A parsed `gvar` table.
#[derive(Debug, Clone)]
pub struct Gvar<'a> {
    data: &'a [u8],
    axis_count: u16,
    shared_tuples_off: u32,
    shared_tuple_count: u16,
    glyph_count: u16,
    long_offsets: bool,
    data_array_off: u32,
    /// Parsed per-glyph offsets (already doubled for short form). One
    /// more than `glyph_count`, so adjacent pairs yield each glyph's
    /// byte range.
    glyph_offsets: Vec<u32>,
}

/// A single contour point delta emitted by [`Gvar::glyph_deltas`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointDelta {
    /// Zero-based index of the contour point inside the glyph.
    pub point: u16,
    /// Accumulated x delta in font design units.
    pub dx: f32,
    /// Accumulated y delta in font design units.
    pub dy: f32,
}

impl<'a> Gvar<'a> {
    /// Parses a `gvar` table.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported gvar major version",
            });
        }
        let axis_count = r.read_u16()?;
        let shared_tuple_count = r.read_u16()?;
        let shared_tuples_off = r.read_u32()?;
        let glyph_count = r.read_u16()?;
        let flags = r.read_u16()?;
        let data_array_off = r.read_u32()?;
        let long_offsets = flags & 0x0001 != 0;

        // Parse glyph offset array: glyphCount + 1 entries.
        let n_offsets = glyph_count as usize + 1;
        let mut glyph_offsets = Vec::with_capacity(n_offsets);
        if long_offsets {
            for _ in 0..n_offsets {
                glyph_offsets.push(r.read_u32()?);
            }
        } else {
            for _ in 0..n_offsets {
                // Short offsets are stored halved — the spec multiplies
                // by two to recover the byte offset.
                let half = u32::from(r.read_u16()?);
                glyph_offsets.push(half * 2);
            }
        }

        Ok(Self {
            data,
            axis_count,
            shared_tuples_off,
            shared_tuple_count,
            glyph_count,
            long_offsets,
            data_array_off,
            glyph_offsets,
        })
    }

    /// Number of axes this gvar addresses. Matches `fvar.axisCount`.
    #[must_use]
    pub const fn axis_count(&self) -> u16 {
        self.axis_count
    }

    /// Number of glyphs covered.
    #[must_use]
    pub const fn glyph_count(&self) -> u16 {
        self.glyph_count
    }

    /// Whether the per-glyph offset array uses the long (u32) form.
    #[must_use]
    pub const fn long_offsets(&self) -> bool {
        self.long_offsets
    }

    /// Returns all contour-point deltas for `glyph_id` at the given
    /// normalized coords, summed across every contributing tuple.
    /// Glyphs with no variation data — or with indices past the end
    /// — return an empty vector.
    ///
    /// `num_points` is the glyph's total point count, inclusive of
    /// the 4 phantom points gvar expects (obtain from
    /// [`crate::tables::Glyf::point_count`]). It's required because
    /// gvar's "all-points" shortcut packs deltas without an explicit
    /// length — the count comes from the outline itself.
    ///
    /// The returned `PointDelta`s are ordered by first appearance of
    /// each point index (deltas for the same point are summed into
    /// one entry). Callers that only care about the outline bounding
    /// box can reduce via min/max on `dx` and `dy`.
    #[must_use]
    pub fn glyph_deltas(
        &self,
        glyph_id: u16,
        coords: &[f32],
        num_points: u16,
    ) -> Vec<PointDelta> {
        let Some((start, end)) = self.glyph_range(glyph_id) else {
            return Vec::new();
        };
        if start == end {
            return Vec::new();
        }
        let gvd_start = self.data_array_off as usize + start as usize;
        let gvd_end = self.data_array_off as usize + end as usize;
        if gvd_end > self.data.len() || gvd_start >= gvd_end {
            return Vec::new();
        }
        let body = &self.data[gvd_start..gvd_end];
        self.deltas_from_glyph_data(body, coords, num_points)
            .unwrap_or_default()
    }



    fn glyph_range(&self, glyph_id: u16) -> Option<(u32, u32)> {
        if glyph_id >= self.glyph_count {
            return None;
        }
        let start = *self.glyph_offsets.get(glyph_id as usize)?;
        let end = *self.glyph_offsets.get(glyph_id as usize + 1)?;
        Some((start, end))
    }

    #[allow(clippy::too_many_lines)]
    fn deltas_from_glyph_data(
        &self,
        body: &[u8],
        coords: &[f32],
        num_points: u16,
    ) -> Result<Vec<PointDelta>> {
        let mut r = Reader::new(body);
        let tvc = r.read_u16()?;
        let tuple_count = tvc & 0x0FFF;
        let has_shared_points = tvc & 0x8000 != 0;
        let data_off = r.read_u16()? as usize;

        // Read tuple variation headers. Each one owns a subrange of
        // the per-tuple serialized data.
        let mut headers: Vec<TupleVariationHeader> = Vec::with_capacity(tuple_count as usize);
        for _ in 0..tuple_count {
            headers.push(TupleVariationHeader::read(&mut r, self.axis_count)?);
        }

        // Data area starts at data_off from the beginning of the
        // GlyphVariationData. Shared point numbers (if any) come
        // first, consuming their own bytes from that region.
        if data_off > body.len() {
            return Err(Error::Truncated {
                offset: data_off,
                context: "gvar glyph data offset past end",
            });
        }
        let data_region = &body[data_off..];
        let mut cursor = 0usize;

        let shared_points: Option<Vec<u16>> = if has_shared_points {
            let (pts, used) = read_packed_point_numbers(data_region)?;
            cursor = used;
            Some(pts)
        } else {
            None
        };
        // An empty Vec from a shared-points block signals the
        // all-points shortcut. Downstream differentiates with
        // `shared_points_is_all_points`.
        let shared_points_is_all_points =
            has_shared_points && shared_points.as_ref().is_some_and(Vec::is_empty);

        // Accumulate deltas into an insertion-ordered table keyed by
        // point index. A `Vec<PointDelta>` gives deterministic
        // output and avoids HashMap ordering nondeterminism.
        let mut acc: Vec<PointDelta> = Vec::new();

        for header in &headers {
            let tuple_data_len = header.variation_data_size as usize;
            if cursor + tuple_data_len > data_region.len() {
                return Err(Error::Truncated {
                    offset: cursor,
                    context: "gvar tuple data region truncated",
                });
            }
            let tuple_bytes = &data_region[cursor..cursor + tuple_data_len];
            cursor += tuple_data_len;

            // Resolve this tuple's peak / intermediate region.
            let Some(peak) = header.resolve_peak(
                self.data,
                self.shared_tuples_off as usize,
                self.shared_tuple_count,
                self.axis_count,
            ) else {
                continue;
            };

            // Compute the region scalar.
            let scalar = tuple_scalar(
                &peak,
                header.intermediate_start.as_deref(),
                header.intermediate_end.as_deref(),
                coords,
            );
            if scalar == 0.0 {
                continue;
            }

            // Within the tuple's bytes: optional private point
            // numbers, then packed x deltas, then packed y deltas.
            let mut tr = 0usize;
            // Decide which point list applies, and whether this is
            // the all-points case.
            let (private_points, tuple_is_all_points) = if header.private_point_numbers {
                let (pts, used) = read_packed_point_numbers(tuple_bytes)?;
                tr = used;
                let all_pts = pts.is_empty();
                (Some(pts), all_pts)
            } else {
                (None, false)
            };

            let point_numbers: &[u16] = if let Some(ref pts) = private_points {
                pts.as_slice()
            } else if let Some(ref sp) = shared_points {
                sp.as_slice()
            } else {
                &[]
            };
            let is_all_points = if header.private_point_numbers {
                tuple_is_all_points
            } else if has_shared_points {
                shared_points_is_all_points
            } else {
                // Neither private nor shared point numbers present —
                // spec says this is equivalent to the all-points
                // shortcut.
                true
            };

            // X and Y streams each carry exactly `n` deltas, where n
            // is `num_points` in the all-points shortcut or
            // `point_numbers.len()` otherwise.
            let n = if is_all_points {
                num_points as usize
            } else {
                point_numbers.len()
            };
            let (xs, consumed_x) = read_packed_deltas_n(&tuple_bytes[tr..], n)?;
            tr += consumed_x;
            let (ys, _consumed_y) = read_packed_deltas_n(&tuple_bytes[tr..], n)?;

            if is_all_points {
                for i in 0..n {
                    let pt = i as u16;
                    #[allow(clippy::cast_precision_loss)]
                    let dx = scalar * xs[i] as f32;
                    #[allow(clippy::cast_precision_loss)]
                    let dy = scalar * ys[i] as f32;
                    accumulate(&mut acc, pt, dx, dy);
                }
            } else {
                for i in 0..n {
                    let Some(&pt) = point_numbers.get(i) else {
                        break;
                    };
                    #[allow(clippy::cast_precision_loss)]
                    let dx = scalar * xs[i] as f32;
                    #[allow(clippy::cast_precision_loss)]
                    let dy = scalar * ys[i] as f32;
                    accumulate(&mut acc, pt, dx, dy);
                }
            }
        }

        Ok(acc)
    }
}

// ----------------------------------------------------------------------------
// TupleVariationHeader
// ----------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct TupleVariationHeader {
    variation_data_size: u16,
    tuple_index: u16,
    embedded_peak: Option<Vec<f32>>,
    intermediate_start: Option<Vec<f32>>,
    intermediate_end: Option<Vec<f32>>,
    private_point_numbers: bool,
}

const FLAG_EMBEDDED_PEAK: u16 = 0x8000;
const FLAG_INTERMEDIATE_REGION: u16 = 0x4000;
const FLAG_PRIVATE_POINT_NUMBERS: u16 = 0x2000;
const TUPLE_INDEX_MASK: u16 = 0x0FFF;

impl TupleVariationHeader {
    fn read(r: &mut Reader<'_>, axis_count: u16) -> Result<Self> {
        let variation_data_size = r.read_u16()?;
        let tuple_index = r.read_u16()?;
        let embedded_peak = if tuple_index & FLAG_EMBEDDED_PEAK != 0 {
            let mut v = Vec::with_capacity(axis_count as usize);
            for _ in 0..axis_count {
                v.push(r.read_f2dot14()?);
            }
            Some(v)
        } else {
            None
        };
        let (intermediate_start, intermediate_end) =
            if tuple_index & FLAG_INTERMEDIATE_REGION != 0 {
                let mut s = Vec::with_capacity(axis_count as usize);
                for _ in 0..axis_count {
                    s.push(r.read_f2dot14()?);
                }
                let mut e = Vec::with_capacity(axis_count as usize);
                for _ in 0..axis_count {
                    e.push(r.read_f2dot14()?);
                }
                (Some(s), Some(e))
            } else {
                (None, None)
            };
        let private_point_numbers = tuple_index & FLAG_PRIVATE_POINT_NUMBERS != 0;
        Ok(Self {
            variation_data_size,
            tuple_index,
            embedded_peak,
            intermediate_start,
            intermediate_end,
            private_point_numbers,
        })
    }

    fn resolve_peak(
        &self,
        gvar_data: &[u8],
        shared_tuples_off: usize,
        shared_tuple_count: u16,
        axis_count: u16,
    ) -> Option<Vec<f32>> {
        if let Some(ref peak) = self.embedded_peak {
            return Some(peak.clone());
        }
        let idx = self.tuple_index & TUPLE_INDEX_MASK;
        if idx >= shared_tuple_count {
            return None;
        }
        let base = shared_tuples_off + idx as usize * axis_count as usize * 2;
        let need = base + axis_count as usize * 2;
        if need > gvar_data.len() {
            return None;
        }
        let mut v = Vec::with_capacity(axis_count as usize);
        for a in 0..axis_count as usize {
            let off = base + a * 2;
            let raw = i16::from_be_bytes([gvar_data[off], gvar_data[off + 1]]);
            v.push(f32::from(raw) / 16384.0);
        }
        Some(v)
    }
}

// ----------------------------------------------------------------------------
// Region scalar (mirrors OpenType spec's supportScalar).
// ----------------------------------------------------------------------------

fn tuple_scalar(
    peak: &[f32],
    start: Option<&[f32]>,
    end: Option<&[f32]>,
    coords: &[f32],
) -> f32 {
    let mut scalar: f32 = 1.0;
    for (i, &p) in peak.iter().enumerate() {
        let c = *coords.get(i).unwrap_or(&0.0);
        // Spec: a peak of zero on an axis means the axis does not
        // participate in this region; skip without touching the
        // scalar.
        if p == 0.0 {
            continue;
        }
        if (c - p).abs() < f32::EPSILON {
            continue;
        }
        // Default region: [0, peak] or [peak, 0] depending on sign.
        let (s, e) = match (start, end) {
            (Some(s), Some(e)) => (
                *s.get(i).unwrap_or(&0.0),
                *e.get(i).unwrap_or(&0.0),
            ),
            _ => {
                if p > 0.0 {
                    (0.0, p)
                } else {
                    (p, 0.0)
                }
            }
        };
        if c < s || c > e {
            return 0.0;
        }
        if c < p {
            if (p - s).abs() < f32::EPSILON {
                return 0.0;
            }
            scalar *= (c - s) / (p - s);
        } else {
            // c > p
            if (p - e).abs() < f32::EPSILON {
                return 0.0;
            }
            scalar *= (e - c) / (e - p);
        }
        if scalar == 0.0 {
            return 0.0;
        }
    }
    scalar
}

// ----------------------------------------------------------------------------
// Packed point numbers.
// ----------------------------------------------------------------------------

/// Decodes the packed-point-number stream at the start of `data`.
/// Returns the point index list plus the number of bytes consumed. A
/// count byte of zero is a shortcut meaning "all points in the
/// glyph"; signalled by an empty returned `Vec`. Callers must treat
/// that as the all-points case rather than a zero-length point list.
pub(crate) fn read_packed_point_numbers(data: &[u8]) -> Result<(Vec<u16>, usize)> {
    if data.is_empty() {
        return Err(Error::Truncated {
            offset: 0,
            context: "packed point numbers: empty",
        });
    }
    let first = data[0];
    let (count, mut cursor) = if first & 0x80 == 0 {
        (u16::from(first), 1usize)
    } else {
        if data.len() < 2 {
            return Err(Error::Truncated {
                offset: 1,
                context: "packed point numbers: missing second count byte",
            });
        }
        let high = (u16::from(first) & 0x7F) << 8;
        (high | u16::from(data[1]), 2usize)
    };

    if count == 0 {
        // Shortcut: all points. Caller discovers the actual length
        // from the delta stream.
        return Ok((Vec::new(), cursor));
    }

    let mut out = Vec::with_capacity(count as usize);
    let mut last: u32 = 0;
    while out.len() < count as usize {
        if cursor >= data.len() {
            return Err(Error::Truncated {
                offset: cursor,
                context: "packed point numbers: control byte missing",
            });
        }
        let control = data[cursor];
        cursor += 1;
        let points_are_words = control & 0x80 != 0;
        let run_count = (control & 0x7F) as usize + 1;
        let remaining = count as usize - out.len();
        let run = run_count.min(remaining);
        for _ in 0..run {
            let delta: u32 = if points_are_words {
                if cursor + 2 > data.len() {
                    return Err(Error::Truncated {
                        offset: cursor,
                        context: "packed point numbers: u16 payload",
                    });
                }
                let v = u16::from_be_bytes([data[cursor], data[cursor + 1]]);
                cursor += 2;
                u32::from(v)
            } else {
                if cursor >= data.len() {
                    return Err(Error::Truncated {
                        offset: cursor,
                        context: "packed point numbers: u8 payload",
                    });
                }
                let v = data[cursor];
                cursor += 1;
                u32::from(v)
            };
            last = last.saturating_add(delta);
            out.push(last.min(u32::from(u16::MAX)) as u16);
        }
    }
    Ok((out, cursor))
}

// ----------------------------------------------------------------------------
// Packed deltas.
// ----------------------------------------------------------------------------

const DELTA_ALL_ZERO: u8 = 0x80;
const DELTA_WORDS: u8 = 0x40;
const DELTA_COUNT_MASK: u8 = 0x3F;

/// Decodes exactly `n` packed delta values from `data`. Each control
/// byte covers up to 64 values; deltas are i8, i16, or implicit
/// zeros. Unused bytes inside an over-long run are consumed to keep
/// the cursor consistent for the next decode call.
fn read_packed_deltas_n(data: &[u8], n: usize) -> Result<(Vec<i32>, usize)> {
    let mut out = Vec::with_capacity(n);
    let mut cursor = 0usize;
    while out.len() < n {
        if cursor >= data.len() {
            return Err(Error::Truncated {
                offset: cursor,
                context: "packed deltas: truncated before n reached",
            });
        }
        let control = data[cursor];
        cursor += 1;
        let run = (control & DELTA_COUNT_MASK) as usize + 1;
        let remaining = n - out.len();
        let take = run.min(remaining);
        if control & DELTA_ALL_ZERO != 0 {
            out.resize(out.len() + take, 0);
        } else if control & DELTA_WORDS != 0 {
            if cursor + take * 2 > data.len() {
                return Err(Error::Truncated {
                    offset: cursor,
                    context: "packed deltas: i16 run (n)",
                });
            }
            for _ in 0..take {
                let v = i16::from_be_bytes([data[cursor], data[cursor + 1]]);
                cursor += 2;
                out.push(i32::from(v));
            }
            let unused = run - take;
            if unused > 0 {
                if cursor + unused * 2 > data.len() {
                    return Err(Error::Truncated {
                        offset: cursor,
                        context: "packed deltas: i16 run (n) unused tail",
                    });
                }
                cursor += unused * 2;
            }
        } else {
            if cursor + take > data.len() {
                return Err(Error::Truncated {
                    offset: cursor,
                    context: "packed deltas: i8 run (n)",
                });
            }
            for _ in 0..take {
                #[allow(clippy::cast_possible_wrap)]
                let v = data[cursor] as i8;
                cursor += 1;
                out.push(i32::from(v));
            }
            let unused = run - take;
            if unused > 0 {
                if cursor + unused > data.len() {
                    return Err(Error::Truncated {
                        offset: cursor,
                        context: "packed deltas: i8 run (n) unused tail",
                    });
                }
                cursor += unused;
            }
        }
    }
    Ok((out, cursor))
}

fn accumulate(acc: &mut Vec<PointDelta>, pt: u16, dx: f32, dy: f32) {
    // Deterministic append — do not sort. If the same point index
    // already has an entry (shared across tuples), fold into it.
    if let Some(existing) = acc.iter_mut().find(|e| e.point == pt) {
        existing.dx += dx;
        existing.dy += dy;
    } else {
        acc.push(PointDelta { point: pt, dx, dy });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
        let raw = (v * 16384.0).round() as i16;
        out.extend_from_slice(&raw.to_be_bytes());
    }

    #[test]
    fn header_parses_short_offsets() {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
        out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
        out.extend_from_slice(&0u16.to_be_bytes()); // sharedTupleCount
        out.extend_from_slice(&0u32.to_be_bytes()); // sharedTuplesOffset
        out.extend_from_slice(&2u16.to_be_bytes()); // glyphCount
        out.extend_from_slice(&0u16.to_be_bytes()); // flags
        out.extend_from_slice(&40u32.to_be_bytes()); // data array offset
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        let g = Gvar::parse(&out).unwrap();
        assert_eq!(g.glyph_count(), 2);
        assert_eq!(g.axis_count(), 1);
        assert!(!g.long_offsets());
    }

    #[test]
    fn header_parses_long_offsets() {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0x0001u16.to_be_bytes()); // flags: long offsets
        out.extend_from_slice(&100u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        let g = Gvar::parse(&out).unwrap();
        assert!(g.long_offsets());
        assert_eq!(g.glyph_count(), 1);
    }

    #[test]
    fn rejects_unsupported_major_version() {
        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&[0u8; 16]);
        assert!(matches!(Gvar::parse(&out), Err(Error::Malformed { .. })));
    }

    #[test]
    fn packed_points_all_points_shortcut() {
        let data = [0x00];
        let (pts, used) = read_packed_point_numbers(&data).unwrap();
        assert!(pts.is_empty());
        assert_eq!(used, 1);
    }

    #[test]
    fn packed_points_single_byte_count_u8_deltas() {
        // 3 points: [5, 7, 10]. Count = 3 (one byte). Control byte
        // with run_count-1=2 (0x02), u8 deltas 5, 2, 3.
        let data = [0x03, 0x02, 5, 2, 3];
        let (pts, used) = read_packed_point_numbers(&data).unwrap();
        assert_eq!(pts, alloc::vec![5, 7, 10]);
        assert_eq!(used, 5);
    }

    #[test]
    fn packed_points_two_byte_count_u16_deltas() {
        // Count = 300 via two-byte count. First delta is u16 = 100.
        let mut data = Vec::new();
        data.push(0x81); // top count byte
        data.push(0x2C); // low count byte → 300
        data.push(0x80); // WORDS, run_count-1=0 → one u16 delta
        data.extend_from_slice(&100u16.to_be_bytes());
        // Remaining 299 points: u8 deltas of 1 apiece.
        let mut remaining = 299usize;
        while remaining > 0 {
            let run = remaining.min(128);
            data.push(((run - 1) & 0x7F) as u8);
            data.resize(data.len() + run, 1);
            remaining -= run;
        }
        let (pts, used) = read_packed_point_numbers(&data).unwrap();
        assert_eq!(pts.len(), 300);
        assert_eq!(pts[0], 100);
        assert_eq!(pts[1], 101);
        assert_eq!(pts[299], 399);
        assert_eq!(used, data.len());
    }

    #[test]
    fn packed_deltas_zero_run() {
        let data = [0x84];
        let (deltas, used) = read_packed_deltas_n(&data, 5).unwrap();
        assert_eq!(deltas, alloc::vec![0, 0, 0, 0, 0]);
        assert_eq!(used, 1);
    }

    #[test]
    fn packed_deltas_i8_run() {
        let data = [0x02, 0xFF, 0x02, 0xFD];
        let (deltas, used) = read_packed_deltas_n(&data, 3).unwrap();
        assert_eq!(deltas, alloc::vec![-1, 2, -3]);
        assert_eq!(used, 4);
    }

    #[test]
    fn packed_deltas_i16_run() {
        let mut data = Vec::new();
        data.push(0x41); // WORDS | run_count-1=1
        data.extend_from_slice(&500i16.to_be_bytes());
        data.extend_from_slice(&(-500i16).to_be_bytes());
        let (deltas, used) = read_packed_deltas_n(&data, 2).unwrap();
        assert_eq!(deltas, alloc::vec![500, -500]);
        assert_eq!(used, 5);
    }

    #[test]
    fn packed_deltas_n_stops_early_with_unused_tail() {
        // Control byte says 4 i8 deltas, but we only want 2. Trailing
        // payload bytes must still be skipped to keep the cursor
        // consistent for the next run.
        let data = [0x03, 1, 2, 3, 4];
        let (deltas, used) = read_packed_deltas_n(&data, 2).unwrap();
        assert_eq!(deltas, alloc::vec![1, 2]);
        assert_eq!(used, 5);
    }

    #[test]
    fn tuple_scalar_peaks_at_one_and_tapers() {
        let peak = [1.0];
        assert!((tuple_scalar(&peak, None, None, &[1.0]) - 1.0).abs() < 1e-6);
        assert!((tuple_scalar(&peak, None, None, &[0.5]) - 0.5).abs() < 1e-6);
        assert!(tuple_scalar(&peak, None, None, &[0.0]).abs() < 1e-6);
        assert!(tuple_scalar(&peak, None, None, &[-0.5]).abs() < 1e-6);
    }

    fn build_single_glyph_gvar() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
        out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
        out.extend_from_slice(&0u16.to_be_bytes()); // sharedTupleCount
        let shared_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // glyphCount
        out.extend_from_slice(&0u16.to_be_bytes()); // flags (short offsets)
        let data_array_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());
        let glyph_off_start = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());

        let shared_off = out.len() as u32;
        out[shared_off_slot..shared_off_slot + 4].copy_from_slice(&shared_off.to_be_bytes());

        let data_array_off = out.len() as u32;
        out[data_array_slot..data_array_slot + 4]
            .copy_from_slice(&data_array_off.to_be_bytes());

        let gvd_start = out.len();
        out.extend_from_slice(&1u16.to_be_bytes()); // tupleVariationCount = 1
        let data_off_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());

        let data_size_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // variationDataSize — patch
        out.extend_from_slice(&FLAG_EMBEDDED_PEAK.to_be_bytes());
        write_f2dot14(&mut out, 1.0);

        let data_off_val = (out.len() - gvd_start) as u16;
        out[data_off_slot..data_off_slot + 2].copy_from_slice(&data_off_val.to_be_bytes());

        // Tuple data: all-points x deltas +10, y deltas 0.
        let tuple_start = out.len();
        out.push(0x03); // i8 run, run_count-1 = 3 → 4 deltas
        out.resize(out.len() + 4, 10);
        out.push(0x83); // ALL_ZERO | run_count-1 = 3 → 4 zero deltas
        let tuple_len = (out.len() - tuple_start) as u16;
        out[data_size_slot..data_size_slot + 2].copy_from_slice(&tuple_len.to_be_bytes());

        let glyph_len = (out.len() as u32 - data_array_off) as u16;
        out[glyph_off_start + 2..glyph_off_start + 4]
            .copy_from_slice(&(glyph_len / 2).to_be_bytes());

        out
    }

    #[test]
    fn glyph_deltas_all_points_shortcut_applies_deltas() {
        let bytes = build_single_glyph_gvar();
        let g = Gvar::parse(&bytes).unwrap();
        let d = g.glyph_deltas(0, &[1.0], 4);
        assert_eq!(d.len(), 4);
        for (i, entry) in d.iter().enumerate() {
            assert_eq!(entry.point, i as u16);
            assert!((entry.dx - 10.0).abs() < 1e-3);
            assert!(entry.dy.abs() < 1e-6);
        }
        let d0 = g.glyph_deltas(0, &[0.0], 4);
        assert!(d0.is_empty());
    }

    #[test]
    fn glyph_deltas_scales_with_coord() {
        let bytes = build_single_glyph_gvar();
        let g = Gvar::parse(&bytes).unwrap();
        let d = g.glyph_deltas(0, &[0.5], 4);
        assert_eq!(d.len(), 4);
        for entry in &d {
            assert!((entry.dx - 5.0).abs() < 1e-3);
        }
    }
}
