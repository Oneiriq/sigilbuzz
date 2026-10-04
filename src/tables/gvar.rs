//! `gvar`: Glyph Variations table.
//!
//! Carries per-glyph outline deltas for a variable font. Where `HVAR`
//! varies a glyph's *advance* across the design space, `gvar` varies
//! every contour point: at coord `c` the rendered point at index `i`
//! is the base glyph's point `i` plus the weighted sum of per-region
//! `(dx, dy)` deltas for that point.
//!
//! A tuple may list deltas for only some points. The points it skips
//! get inferred deltas (the spec's "interpolation of untouched
//! points", IUP): each skipped point takes its delta from the nearest
//! listed points before and after it on the same contour.
//! [`Gvar::glyph_point_deltas`] does that per tuple, the way HarfBuzz
//! does, and is what the outline and advance code uses.
//! [`Gvar::glyph_deltas`] only reports the listed deltas.
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
use crate::tables::parse::{abs_f32, Reader};

#[cfg(test)]
mod iup_tests;
#[cfg(test)]
pub(crate) mod testing;

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
    /// The per-glyph data offsets, read on demand: `glyph_count + 1`
    /// big-endian entries, halved `u16`s in the short form and `u32`s
    /// in the long form, so adjacent pairs give each glyph's byte
    /// range. Parsing never copies them out, so it stays cheap for
    /// fonts with tens of thousands of glyphs.
    glyph_offsets: &'a [u8],
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

        // The glyph offset array: glyphCount + 1 entries, checked to
        // fit here and read when a glyph is looked up.
        let entry_size = if long_offsets { 4 } else { 2 };
        let glyph_offsets = r.read_bytes((glyph_count as usize + 1) * entry_size)?;

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
    /// Glyphs with no variation data, or with indices past the end,
    /// return an empty vector.
    ///
    /// Only the deltas the tuples list are reported: points a tuple
    /// skips get no inferred delta here. Use
    /// [`Gvar::glyph_point_deltas`] for the deltas a renderer applies.
    /// Malformed variation data also yields an empty vector; the
    /// `Result` of [`Gvar::glyph_point_deltas`] reports it instead.
    ///
    /// `num_points` is the glyph's total point count, inclusive of
    /// the 4 phantom points gvar expects (obtain from
    /// [`crate::tables::Glyf::point_count`]). It's required because
    /// gvar's "all-points" shortcut packs deltas without an explicit
    /// length. The count comes from the outline itself.
    ///
    /// The returned `PointDelta`s are ordered by first appearance of
    /// each point index (deltas for the same point are summed into
    /// one entry).
    #[must_use]
    pub fn glyph_deltas(&self, glyph_id: u16, coords: &[f32], num_points: u16) -> Vec<PointDelta> {
        // Accumulate deltas into an insertion-ordered table keyed by
        // point index. A `Vec<PointDelta>` gives deterministic
        // output and avoids HashMap ordering nondeterminism.
        let mut acc = DeltaAccumulator::default();
        let mut work = MAX_TUPLE_WORK;
        let num_points = usize::from(num_points);
        let walked = self.walk_tuples(glyph_id, coords, num_points, &mut work, |t| {
            match t.points {
                None => {
                    for (i, (&x, &y)) in t.xs.iter().zip(t.ys).enumerate() {
                        acc.add(i as u16, t.scalar * x as f32, t.scalar * y as f32);
                    }
                }
                Some(points) => {
                    for ((&pt, &x), &y) in points.iter().zip(t.xs).zip(t.ys) {
                        acc.add(pt, t.scalar * x as f32, t.scalar * y as f32);
                    }
                }
            }
        });
        match walked {
            Ok(()) => acc.deltas,
            Err(_) => Vec::new(),
        }
    }

    /// Returns the delta of every point of `glyph_id` at the given
    /// normalized coords, with the points each tuple skips inferred
    /// from the points it lists (IUP), as HarfBuzz does.
    ///
    /// `points` are the glyph's own points in glyph order, without
    /// the phantom points: the contour points of a simple glyph, or
    /// one point per component of a composite glyph (its x and y
    /// offset). `end_points` are the simple glyph's
    /// `endPtsOfContours`. Inference runs per contour and only for
    /// points on a contour, so a composite glyph passes an empty
    /// `end_points` and gets the listed deltas only, as do the
    /// phantom points.
    ///
    /// The result holds `points.len() + 4` deltas: one per point, then
    /// the four phantom points (left side bearing origin, advance
    /// origin, top origin, and bottom origin). Glyphs without
    /// variation data get all zeros.
    ///
    /// Inference works per tuple, on the tuple's deltas already
    /// scaled by its region scalar, before the tuples are summed. For
    /// each point a tuple skips, it finds the nearest listed points
    /// before and after it on the contour (wrapping around), and per
    /// axis:
    ///
    /// - when the two listed points share the coordinate, it takes
    ///   their delta if they agree, otherwise zero;
    /// - when the point lies outside the two coordinates, it takes the
    ///   delta of the nearer one;
    /// - otherwise it interpolates linearly between the two deltas.
    ///
    /// A contour with one listed point moves rigidly by that point's
    /// delta. A contour with no listed points does not move. Listed
    /// point numbers past the end are ignored; a point listed twice
    /// gets both deltas.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Truncated`] or [`Error::Malformed`] when the
    /// glyph's variation data runs past its bounds or does not decode.
    /// The offset counts from the start of the `gvar` table.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::tables::Gvar;
    ///
    /// // A gvar with one axis and no glyph variation data.
    /// let mut bytes = Vec::new();
    /// for v in [1u16, 0, 1, 0] {
    ///     bytes.extend_from_slice(&v.to_be_bytes()); // version, axes, shared tuples
    /// }
    /// bytes.extend_from_slice(&0u32.to_be_bytes()); // shared tuples offset
    /// bytes.extend_from_slice(&1u16.to_be_bytes()); // glyph count
    /// bytes.extend_from_slice(&0u16.to_be_bytes()); // short offsets
    /// bytes.extend_from_slice(&24u32.to_be_bytes()); // data array offset
    /// bytes.extend_from_slice(&[0, 0, 0, 0]); // glyph 0 has no data
    /// let gvar = Gvar::parse(&bytes)?;
    /// let deltas = gvar.glyph_point_deltas(0, &[1.0], &[(0, 0), (100, 0)], &[1])?;
    /// assert_eq!(deltas, vec![(0.0, 0.0); 6]);
    /// # Ok::<(), sigilbuzz::Error>(())
    /// ```
    pub fn glyph_point_deltas(
        &self,
        glyph_id: u16,
        coords: &[f32],
        points: &[(i32, i32)],
        end_points: &[u16],
    ) -> Result<Vec<(f32, f32)>> {
        let mut work = MAX_TUPLE_WORK;
        self.glyph_point_deltas_with(glyph_id, coords, points, end_points, &mut work)
    }

    /// [`Gvar::glyph_point_deltas`], charging the tuples it decodes to
    /// `work`, which an outline walk shares across every glyph it
    /// visits (see [`MAX_TUPLE_WORK`]).
    pub(crate) fn glyph_point_deltas_with(
        &self,
        glyph_id: u16,
        coords: &[f32],
        points: &[(i32, i32)],
        end_points: &[u16],
        work: &mut usize,
    ) -> Result<Vec<(f32, f32)>> {
        let count = points.len() + PHANTOM_COUNT;
        let mut total = alloc::vec![(0.0_f32, 0.0_f32); count];
        // Contour membership, from the end point numbers. Points past
        // the last end point (and the phantom points) are on no
        // contour, so inference never touches them.
        let mut is_end = alloc::vec![false; points.len()];
        for &e in end_points {
            if let Some(flag) = is_end.get_mut(usize::from(e)) {
                *flag = true;
            }
        }
        // Per-tuple scratch, sized on the first tuple that lists its
        // points.
        let mut tuple: Vec<(f32, f32)> = Vec::new();
        let mut listed: Vec<bool> = Vec::new();
        self.walk_tuples(glyph_id, coords, count, work, |t| match t.points {
            None => {
                for ((slot, &x), &y) in total.iter_mut().zip(t.xs).zip(t.ys) {
                    slot.0 += t.scalar * x as f32;
                    slot.1 += t.scalar * y as f32;
                }
            }
            Some(numbers) => {
                tuple.clear();
                tuple.resize(count, (0.0, 0.0));
                listed.clear();
                listed.resize(count, false);
                for ((&pt, &x), &y) in numbers.iter().zip(t.xs).zip(t.ys) {
                    let i = usize::from(pt);
                    let Some(slot) = tuple.get_mut(i) else {
                        continue;
                    };
                    slot.0 += t.scalar * x as f32;
                    slot.1 += t.scalar * y as f32;
                    listed[i] = true;
                }
                infer_unlisted(&mut tuple, &listed, points, &is_end);
                for (slot, d) in total.iter_mut().zip(&tuple) {
                    slot.0 += d.0;
                    slot.1 += d.1;
                }
            }
        })?;
        Ok(total)
    }

    /// Deltas of the four phantom points of a glyph with `num_points`
    /// points of its own (contour points, or components). Only listed
    /// deltas apply: phantom points are on no contour. The tuples it
    /// decodes are charged to `work`, as in
    /// [`Gvar::glyph_point_deltas_with`].
    pub(crate) fn phantom_deltas(
        &self,
        glyph_id: u16,
        coords: &[f32],
        num_points: usize,
        work: &mut usize,
    ) -> Result<[(f32, f32); 4]> {
        let mut out = [(0.0_f32, 0.0_f32); PHANTOM_COUNT];
        let mut add = |i: usize, x: i32, y: i32, scalar: f32| {
            if let Some(slot) = i.checked_sub(num_points).and_then(|p| out.get_mut(p)) {
                slot.0 += scalar * x as f32;
                slot.1 += scalar * y as f32;
            }
        };
        let count = num_points + PHANTOM_COUNT;
        self.walk_tuples(glyph_id, coords, count, work, |t| match t.points {
            None => {
                for (i, (&x, &y)) in t.xs.iter().zip(t.ys).enumerate().skip(num_points) {
                    add(i, x, y, t.scalar);
                }
            }
            Some(numbers) => {
                for ((&pt, &x), &y) in numbers.iter().zip(t.xs).zip(t.ys) {
                    add(usize::from(pt), x, y, t.scalar);
                }
            }
        })?;
        Ok(out)
    }

    fn glyph_range(&self, glyph_id: u16) -> Option<(u32, u32)> {
        if glyph_id >= self.glyph_count {
            return None;
        }
        let i = usize::from(glyph_id);
        Some((self.glyph_offset(i)?, self.glyph_offset(i + 1)?))
    }

    /// Entry `i` of the glyph offset array, as a byte offset into the
    /// glyph variation data. Short offsets are stored halved: the spec
    /// multiplies them by two.
    fn glyph_offset(&self, i: usize) -> Option<u32> {
        if self.long_offsets {
            let b = self.glyph_offsets.get(i * 4..i * 4 + 4)?;
            Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        } else {
            let b = self.glyph_offsets.get(i * 2..i * 2 + 2)?;
            Some(u32::from(u16::from_be_bytes([b[0], b[1]])) * 2)
        }
    }

    /// Decodes every tuple of `glyph_id` whose region scalar at
    /// `coords` is not zero and hands it to `visit`, in table order.
    /// `num_points` is the glyph's point count including the phantom
    /// points: the all-points form packs that many deltas.
    ///
    /// Each tuple header read costs one unit of `work`, and each tuple
    /// decoded costs `num_points` more, as HarfBuzz charges its glyph
    /// budget per tuple. Running out fails the walk.
    ///
    /// Glyph ids past the end and glyphs with no data visit nothing.
    /// Errors carry offsets from the start of the table.
    fn walk_tuples<F>(
        &self,
        glyph_id: u16,
        coords: &[f32],
        num_points: usize,
        work: &mut usize,
        mut visit: F,
    ) -> Result<()>
    where
        F: FnMut(&TupleDeltas<'_>),
    {
        let Some((start, end)) = self.glyph_range(glyph_id) else {
            return Ok(());
        };
        if start == end {
            return Ok(());
        }
        // The offset array entry for this glyph, for error reports.
        let entry_size = if self.long_offsets { 4 } else { 2 };
        let entry = GVAR_HEADER_SIZE + usize::from(glyph_id) * entry_size;
        if start > end {
            return Err(Error::Malformed {
                offset: entry,
                context: "gvar glyph data offsets decrease",
            });
        }
        // Checked: two u32 offsets can overflow a 32-bit usize.
        let base = self.data_array_off as usize;
        let (Some(gvd_start), Some(gvd_end)) = (
            base.checked_add(start as usize),
            base.checked_add(end as usize),
        ) else {
            return Err(Error::Malformed {
                offset: entry,
                context: "gvar glyph data offset overflows",
            });
        };
        let Some(body) = self.data.get(gvd_start..gvd_end) else {
            return Err(Error::Truncated {
                offset: gvd_start.min(self.data.len()),
                context: "gvar glyph data past end of table",
            });
        };
        self.walk_glyph_data(body, coords, num_points, work, &mut visit)
            .map_err(|e| rebase(e, gvd_start))
    }

    /// [`Gvar::walk_tuples`] on one `GlyphVariationData`. Errors carry
    /// offsets from the start of `body`.
    fn walk_glyph_data(
        &self,
        body: &[u8],
        coords: &[f32],
        num_points: usize,
        work: &mut usize,
        visit: &mut dyn FnMut(&TupleDeltas<'_>),
    ) -> Result<()> {
        let mut r = Reader::new(body);
        let tvc = r.read_u16()?;
        let tuple_count = tvc & 0x0FFF;
        let has_shared_points = tvc & 0x8000 != 0;
        let data_off = r.read_u16()? as usize;

        // Read tuple variation headers. Each one owns a subrange of
        // the per-tuple serialized data.
        // Each header takes at least 4 bytes, which bounds the capacity.
        let mut headers: Vec<TupleVariationHeader> =
            Vec::with_capacity((tuple_count as usize).min(r.remaining() / 4));
        for _ in 0..tuple_count {
            charge(work, 1, r.position())?;
            headers.push(TupleVariationHeader::read(&mut r, self.axis_count)?);
        }

        // Data area starts at data_off from the beginning of the
        // GlyphVariationData. Shared point numbers (if any) come
        // first, consuming their own bytes from that region.
        let Some(data_region) = body.get(data_off..) else {
            return Err(Error::Truncated {
                offset: 2,
                context: "gvar glyph data offset past end",
            });
        };
        let mut cursor = 0usize;

        // An empty list from a shared-points block signals the
        // all-points shortcut.
        let shared_points: Option<Vec<u16>> = if has_shared_points {
            let (pts, used) =
                read_packed_point_numbers(data_region).map_err(|e| rebase(e, data_off))?;
            cursor = used;
            Some(pts)
        } else {
            None
        };

        // Scratch for each tuple's point numbers and deltas, reused so
        // that decoding a tuple does not allocate.
        let mut private: Vec<u16> = Vec::new();
        let mut xs: Vec<i32> = Vec::new();
        let mut ys: Vec<i32> = Vec::new();
        for header in &headers {
            let tuple_start = cursor;
            let tuple_data_len = header.variation_data_size as usize;
            let Some(tuple_bytes) = data_region.get(cursor..cursor + tuple_data_len) else {
                return Err(Error::Truncated {
                    offset: data_off + cursor,
                    context: "gvar tuple data region truncated",
                });
            };
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
                peak,
                header.intermediate_start,
                header.intermediate_end,
                coords,
            );
            if scalar == 0.0 {
                continue;
            }

            // Every decoded tuple costs the caller work in proportion
            // to the point count, so a glyph of many small tuples over
            // many points is capped, as HarfBuzz charges its budget per
            // tuple.
            charge(work, num_points.max(1), data_off + tuple_start)?;

            // Within the tuple's bytes: optional private point
            // numbers, then packed x deltas, then packed y deltas.
            let at = |e: Error| rebase(e, data_off + tuple_start);
            let mut tr = 0usize;
            if header.private_point_numbers {
                tr = read_packed_point_numbers_into(tuple_bytes, &mut private).map_err(at)?;
            }
            // Neither private nor shared point numbers means all
            // points, as does an empty list.
            let points: Option<&[u16]> = if header.private_point_numbers {
                Some(private.as_slice())
            } else {
                shared_points.as_deref()
            }
            .filter(|p| !p.is_empty());

            // X and Y streams each carry exactly `n` deltas: one per
            // listed point, or one per point in the all-points form.
            let n = points.map_or(num_points, <[u16]>::len);
            let consumed_x = read_packed_deltas_into(&tuple_bytes[tr..], n, &mut xs)
                .map_err(|e| rebase(e, data_off + tuple_start + tr))?;
            tr += consumed_x;
            read_packed_deltas_into(&tuple_bytes[tr..], n, &mut ys)
                .map_err(|e| rebase(e, data_off + tuple_start + tr))?;
            visit(&TupleDeltas {
                scalar,
                points,
                xs: &xs,
                ys: &ys,
            });
        }
        Ok(())
    }
}

/// Size of the fixed `gvar` header, before the glyph offset array.
const GVAR_HEADER_SIZE: usize = 20;

/// Phantom points gvar appends after a glyph's own points.
const PHANTOM_COUNT: usize = 4;

/// Cap on the tuple work one walk may do: one unit per tuple header,
/// plus the point count of every tuple it decodes. A call on one glyph
/// gets the whole cap; an outline walk shares one cap across every
/// glyph it visits, as HarfBuzz passes one budget (also `1 << 24`)
/// down its `get_points` recursion. Real glyphs stay far below it: a
/// few hundred points over at most a few hundred tuples.
pub(crate) const MAX_TUPLE_WORK: usize = 1 << 24;

/// The `context` of the error a walk fails with when its tuple work
/// runs out.
pub(crate) const OUT_OF_TUPLE_WORK: &str = "gvar variation work exceeds the cap";

/// Takes `cost` units from `work`, or fails with the byte offset of the
/// tuple data that would overspend it.
fn charge(work: &mut usize, cost: usize, offset: usize) -> Result<()> {
    *work = work.checked_sub(cost).ok_or(Error::Malformed {
        offset,
        context: OUT_OF_TUPLE_WORK,
    })?;
    Ok(())
}

/// One tuple's decoded deltas, as [`Gvar::walk_tuples`] hands them out.
struct TupleDeltas<'t> {
    /// The tuple's region scalar at the requested coords. Never zero.
    scalar: f32,
    /// The listed point numbers, or `None` for every point.
    points: Option<&'t [u16]>,
    /// Unscaled x deltas, one per listed point (or per point).
    xs: &'t [i32],
    /// Unscaled y deltas, parallel to `xs`.
    ys: &'t [i32],
}

/// Moves the offset of a parse error found in a sub-slice that starts
/// `base` bytes further in.
fn rebase(e: Error, base: usize) -> Error {
    match e {
        Error::Truncated { offset, context } => Error::Truncated {
            offset: offset.saturating_add(base),
            context,
        },
        Error::Malformed { offset, context } => Error::Malformed {
            offset: offset.saturating_add(base),
            context,
        },
        other => other,
    }
}

/// Infers the deltas of the points one tuple does not list (`listed`
/// false), contour by contour, from the listed points around them.
/// `deltas` holds the tuple's scaled deltas, one per point plus the
/// phantom points; `orig` the glyph's default point positions; and
/// `is_end` marks the last point of each contour.
fn infer_unlisted(
    deltas: &mut [(f32, f32)],
    listed: &[bool],
    orig: &[(i32, i32)],
    is_end: &[bool],
) {
    let mut start = 0;
    for (end, _) in is_end.iter().enumerate().filter(|&(_, &e)| e) {
        if end >= start {
            infer_contour(deltas, listed, orig, start, end);
        }
        start = end + 1;
    }
}

/// [`infer_unlisted`] for the contour `start..=end`.
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

/// HarfBuzz's `infer_delta` on one axis: the delta of a point at
/// `target` between listed points at `prev` and `next` whose deltas
/// are `prev_delta` and `next_delta`.
// Exact comparisons on purpose: the coordinates are integers, and equal
// deltas must agree bit for bit, as in HarfBuzz.
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

// ----------------------------------------------------------------------------
// TupleVariationHeader
// ----------------------------------------------------------------------------

/// One tuple variation header. The peak and intermediate rows borrow
/// the table's bytes, `axis_count` big-endian F2DOT14 values each, so
/// reading a header never allocates.
#[derive(Debug, Clone, Copy)]
struct TupleVariationHeader<'a> {
    variation_data_size: u16,
    tuple_index: u16,
    embedded_peak: Option<&'a [u8]>,
    intermediate_start: Option<&'a [u8]>,
    intermediate_end: Option<&'a [u8]>,
    private_point_numbers: bool,
}

const FLAG_EMBEDDED_PEAK: u16 = 0x8000;
const FLAG_INTERMEDIATE_REGION: u16 = 0x4000;
const FLAG_PRIVATE_POINT_NUMBERS: u16 = 0x2000;
const TUPLE_INDEX_MASK: u16 = 0x0FFF;

impl<'a> TupleVariationHeader<'a> {
    fn read(r: &mut Reader<'a>, axis_count: u16) -> Result<Self> {
        let variation_data_size = r.read_u16()?;
        let tuple_index = r.read_u16()?;
        // Each tuple is `axis_count` F2DOT14 values.
        let row = usize::from(axis_count) * 2;
        let embedded_peak = if tuple_index & FLAG_EMBEDDED_PEAK != 0 {
            Some(r.read_bytes(row)?)
        } else {
            None
        };
        let (intermediate_start, intermediate_end) = if tuple_index & FLAG_INTERMEDIATE_REGION != 0
        {
            let s = r.read_bytes(row)?;
            let e = r.read_bytes(row)?;
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

    /// The tuple's peak row: its own, or the shared tuple it names.
    /// `None` when the shared tuple does not exist.
    fn resolve_peak(
        &self,
        gvar_data: &'a [u8],
        shared_tuples_off: usize,
        shared_tuple_count: u16,
        axis_count: u16,
    ) -> Option<&'a [u8]> {
        if let Some(peak) = self.embedded_peak {
            return Some(peak);
        }
        let idx = self.tuple_index & TUPLE_INDEX_MASK;
        if idx >= shared_tuple_count {
            return None;
        }
        let row = axis_count as usize * 2;
        // Checked: `shared_tuples_off` is a u32 from the font, so the
        // sum can overflow a 32-bit usize.
        let base = shared_tuples_off.checked_add(idx as usize * row)?;
        gvar_data.get(base..base.checked_add(row)?)
    }
}

/// The F2DOT14 value at index `i` of a row of big-endian F2DOT14
/// values, or `None` past the end.
fn f2dot14_at(row: &[u8], i: usize) -> Option<f32> {
    let b = row.get(i * 2..i * 2 + 2)?;
    Some(f32::from(i16::from_be_bytes([b[0], b[1]])) / 16384.0)
}

// ----------------------------------------------------------------------------
// Region scalar (mirrors OpenType spec's supportScalar).
// ----------------------------------------------------------------------------

/// The region scalar of a tuple at `coords`. `peak`, `start`, and `end`
/// are rows of big-endian F2DOT14 values, one per axis.
fn tuple_scalar(peak: &[u8], start: Option<&[u8]>, end: Option<&[u8]>, coords: &[f32]) -> f32 {
    let mut scalar: f32 = 1.0;
    for i in 0..peak.len() / 2 {
        let p = f2dot14_at(peak, i).unwrap_or(0.0);
        let c = *coords.get(i).unwrap_or(&0.0);
        // Spec: a peak of zero on an axis means the axis does not
        // participate in this region; skip without touching the
        // scalar.
        if p == 0.0 {
            continue;
        }
        if abs_f32(c - p) < f32::EPSILON {
            continue;
        }
        // Default region: [0, peak] or [peak, 0] depending on sign.
        let (s, e) = match (start, end) {
            (Some(s), Some(e)) => (
                f2dot14_at(s, i).unwrap_or(0.0),
                f2dot14_at(e, i).unwrap_or(0.0),
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
            if abs_f32(p - s) < f32::EPSILON {
                return 0.0;
            }
            scalar *= (c - s) / (p - s);
        } else {
            // c > p
            if abs_f32(p - e) < f32::EPSILON {
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
/// glyph"; signaled by an empty returned `Vec`. Callers must treat
/// that as the all-points case rather than a zero-length point list.
pub(crate) fn read_packed_point_numbers(data: &[u8]) -> Result<(Vec<u16>, usize)> {
    let mut out = Vec::new();
    let used = read_packed_point_numbers_into(data, &mut out)?;
    Ok((out, used))
}

/// [`read_packed_point_numbers`] into `out`, which it clears first.
/// Returns the number of bytes consumed.
fn read_packed_point_numbers_into(data: &[u8], out: &mut Vec<u16>) -> Result<usize> {
    out.clear();
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
        return Ok(cursor);
    }

    out.reserve(count as usize);
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
    Ok(cursor)
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
#[cfg(test)]
fn read_packed_deltas_n(data: &[u8], n: usize) -> Result<(Vec<i32>, usize)> {
    let mut out = Vec::new();
    let used = read_packed_deltas_into(data, n, &mut out)?;
    Ok((out, used))
}

/// Decodes exactly `n` packed delta values from `data` into `out`,
/// which it clears first, and returns the number of bytes consumed.
/// Each control byte covers up to 64 values; deltas are i8, i16, or
/// implicit zeros. Unused bytes inside an over-long run are consumed
/// to keep the cursor consistent for the next decode call.
fn read_packed_deltas_into(data: &[u8], n: usize, out: &mut Vec<i32>) -> Result<usize> {
    out.clear();
    out.reserve(n);
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
    Ok(cursor)
}

/// Per-point delta sums in order of first appearance.
///
/// `slot[pt]` holds the index of point `pt` in `deltas` plus one, or
/// zero when the point has no entry yet. The side table makes each
/// update constant time. A linear search per update was quadratic in
/// the point count and let one glyph with tens of thousands of points
/// stall the parser.
#[derive(Default)]
struct DeltaAccumulator {
    deltas: Vec<PointDelta>,
    slot: Vec<u32>,
}

impl DeltaAccumulator {
    fn add(&mut self, pt: u16, dx: f32, dy: f32) {
        // Deterministic append: do not sort. If the same point index
        // already has an entry (shared across tuples), fold into it.
        let idx = usize::from(pt);
        if idx >= self.slot.len() {
            self.slot.resize(idx + 1, 0);
        }
        match self.slot[idx]
            .checked_sub(1)
            .and_then(|i| self.deltas.get_mut(i as usize))
        {
            Some(existing) => {
                existing.dx += dx;
                existing.dy += dy;
            }
            None => {
                self.deltas.push(PointDelta { point: pt, dx, dy });
                // At most 65,536 distinct points, so the length fits.
                self.slot[idx] = self.deltas.len() as u32;
            }
        }
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
        // Short offsets are halved: entries 0, 3, 5 give glyph 0 bytes 0
        // to 6 and glyph 1 bytes 6 to 10.
        out[22..24].copy_from_slice(&3u16.to_be_bytes());
        out[24..26].copy_from_slice(&5u16.to_be_bytes());
        let g = Gvar::parse(&out).unwrap();
        assert_eq!(g.glyph_range(0), Some((0, 6)));
        assert_eq!(g.glyph_range(1), Some((6, 10)));
        assert_eq!(g.glyph_range(2), None);
        // The offset array must fit, though parsing reads none of it.
        out.pop();
        assert!(matches!(
            Gvar::parse(&out),
            Err(Error::Truncated { offset: 20, .. })
        ));
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
        data.push(0x2C); // low count byte -> 300
        data.push(0x80); // WORDS, run_count-1=0 -> one u16 delta
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
        let peak = 0x4000u16.to_be_bytes(); // 1.0
        assert!((tuple_scalar(&peak, None, None, &[1.0]) - 1.0).abs() < 1e-6);
        assert!((tuple_scalar(&peak, None, None, &[0.5]) - 0.5).abs() < 1e-6);
        assert!(tuple_scalar(&peak, None, None, &[0.0]).abs() < 1e-6);
        assert!(tuple_scalar(&peak, None, None, &[-0.5]).abs() < 1e-6);
    }

    #[test]
    fn tuple_scalar_reads_intermediate_rows() {
        // Peak 0.5 inside the region from 0.25 to 1.0, on one axis.
        let (peak, start, end) = (0x2000u16, 0x1000u16, 0x4000u16);
        let row = |v: u16| v.to_be_bytes();
        let scalar = |c: f32| tuple_scalar(&row(peak), Some(&row(start)), Some(&row(end)), &[c]);
        assert!((scalar(0.5) - 1.0).abs() < 1e-6);
        assert!((scalar(0.375) - 0.5).abs() < 1e-6);
        assert!((scalar(0.75) - 0.5).abs() < 1e-6);
        assert!(scalar(0.2).abs() < 1e-6);
        assert!(scalar(1.0).abs() < 1e-6);
        // Coords past the rows play no part.
        let two_axes = tuple_scalar(&row(peak), Some(&row(start)), Some(&row(end)), &[0.5, 1.0]);
        assert!((two_axes - 1.0).abs() < 1e-6);
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
        out[data_array_slot..data_array_slot + 4].copy_from_slice(&data_array_off.to_be_bytes());

        let gvd_start = out.len();
        out.extend_from_slice(&1u16.to_be_bytes()); // tupleVariationCount = 1
        let data_off_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());

        let data_size_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // variationDataSize: patch
        out.extend_from_slice(&FLAG_EMBEDDED_PEAK.to_be_bytes());
        write_f2dot14(&mut out, 1.0);

        let data_off_val = (out.len() - gvd_start) as u16;
        out[data_off_slot..data_off_slot + 2].copy_from_slice(&data_off_val.to_be_bytes());

        // Tuple data: all-points x deltas +10, y deltas 0.
        let tuple_start = out.len();
        out.push(0x03); // i8 run, run_count-1 = 3 -> 4 deltas
        out.resize(out.len() + 4, 10);
        out.push(0x83); // ALL_ZERO | run_count-1 = 3 -> 4 zero deltas
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
