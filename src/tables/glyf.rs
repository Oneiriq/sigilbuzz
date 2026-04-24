// Parser-style code leans on bespoke index loops / byte-by-byte
// walks; the pedantic range-loop lints add noise without clarifying
// the spec-mirroring layout.
#![allow(
    clippy::bool_to_int_with_if,
    clippy::elidable_lifetime_names,
    clippy::map_unwrap_or,
    clippy::too_many_lines,
    clippy::needless_range_loop,
    clippy::similar_names
)]

//! `glyf` — TrueType glyph data.
//!
//! Parses the 10-byte glyph header, simple-glyph contour points, and
//! composite-glyph component references. The simple-glyph path emits
//! quadratic Bezier outlines through an [`OutlineSink`]; composites
//! are flattened recursively so callers never see component
//! semantics.
//!
//! # Glyph header
//!
//! ```text
//!   i16    numberOfContours   (>=0 simple, -1 composite)
//!   FWord  xMin
//!   FWord  yMin
//!   FWord  xMax
//!   FWord  yMax
//! ```
//!
//! A simple glyph follows with `endPtsOfContours[numberOfContours]`,
//! `instructionLength`, `instructions[instructionLength]`, a flags
//! stream with REPEAT support, then x / y coordinate streams whose
//! widths are flag-driven.
//!
//! A composite glyph is a chain of component records, each carrying a
//! 2×2 transform and a translation. Components may reference further
//! composites; sigilbuzz caps recursion to avoid pathological fonts.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::loca::Loca;
use crate::tables::outline::OutlineSink;
use crate::tables::parse::Reader;

/// Glyph bounding box in font design units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphBounds {
    /// Left edge.
    pub x_min: i16,
    /// Bottom edge.
    pub y_min: i16,
    /// Right edge.
    pub x_max: i16,
    /// Top edge.
    pub y_max: i16,
    /// Number of contours; negative for composite glyphs.
    pub num_contours: i16,
}

/// A borrowed view of the `glyf` table. Parsing is free — accessors
/// slice into the underlying bytes on demand.
#[derive(Debug, Clone, Copy)]
pub struct Glyf<'a> {
    data: &'a [u8],
}

// Simple-glyph flag bits.
const FLAG_ON_CURVE: u8 = 0x01;
const FLAG_X_SHORT: u8 = 0x02;
const FLAG_Y_SHORT: u8 = 0x04;
const FLAG_REPEAT: u8 = 0x08;
const FLAG_X_SAME_OR_POS: u8 = 0x10;
const FLAG_Y_SAME_OR_POS: u8 = 0x20;

// Composite-glyph flag bits.
const COMP_ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
const COMP_ARGS_ARE_XY_VALUES: u16 = 0x0002;
const COMP_ROUND_XY_TO_GRID: u16 = 0x0004;
const COMP_WE_HAVE_A_SCALE: u16 = 0x0008;
const COMP_MORE_COMPONENTS: u16 = 0x0020;
const COMP_WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
const COMP_WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
#[allow(dead_code)]
const COMP_WE_HAVE_INSTRUCTIONS: u16 = 0x0100;
const COMP_USE_MY_METRICS: u16 = 0x0200;
const COMP_SCALED_COMPONENT_OFFSET: u16 = 0x0800;
const COMP_UNSCALED_COMPONENT_OFFSET: u16 = 0x1000;

/// Hard cap on composite recursion depth. The OpenType spec imposes
/// no fixed bound, but HarfBuzz uses 64 and in-the-wild glyphs never
/// exceed a handful of levels.
const MAX_COMPOSITE_DEPTH: u8 = 64;

/// Per-point data after flag decoding, before deltas.
#[derive(Debug, Clone, Copy)]
struct Point {
    x: f32,
    y: f32,
    on_curve: bool,
}

impl<'a> Glyf<'a> {
    /// Wraps the raw `glyf` bytes. No validation up front — the
    /// table is too large and too dense to validate whole-table in
    /// linear time; accessors bound-check each read.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    /// Returns the raw byte range for `glyph_id` inside `glyf`, or
    /// `None` when the glyph has no outline.
    fn glyph_bytes(&self, loca: &Loca<'_>, glyph_id: u16) -> Result<Option<&'a [u8]>> {
        let Some((start, end)) = loca.range(glyph_id) else {
            return Ok(None);
        };
        if start == end {
            return Ok(None);
        }
        let start = start as usize;
        let end = end as usize;
        if end > self.data.len() || start > end {
            return Err(Error::Truncated {
                offset: start,
                context: "glyf range from loca falls outside glyf table",
            });
        }
        Ok(Some(&self.data[start..end]))
    }

    /// Returns the number of contour points in `glyph_id`, inclusive
    /// of the 4 phantom points gvar expects (left-side-bearing,
    /// right-side-bearing, top, bottom). For simple glyphs the count
    /// comes from `endPtsOfContours[numContours-1] + 1`; composite
    /// glyphs and zero-length glyphs return `None` (composites don't
    /// participate in gvar's per-point delta scheme in sigilbuzz's
    /// current cut).
    pub fn point_count(&self, loca: &Loca<'_>, glyph_id: u16) -> Result<Option<u16>> {
        let Some(body) = self.glyph_bytes(loca, glyph_id)? else {
            return Ok(None);
        };
        if body.len() < 10 {
            return Ok(None);
        }
        let mut r = Reader::new(body);
        let num_contours = r.read_i16()?;
        if num_contours < 0 {
            return Ok(None);
        }
        r.skip(8)?; // xMin/yMin/xMax/yMax.
        if num_contours == 0 {
            return Ok(Some(4));
        }
        let last_idx = (num_contours - 1) as usize;
        r.skip(last_idx * 2)?;
        let last_end_pt = r.read_u16()?;
        Ok(Some(last_end_pt.saturating_add(1).saturating_add(4)))
    }

    /// Returns the bounding box for `glyph_id`. The glyph's byte
    /// range comes from `loca`; an empty range means the glyph has
    /// no outline and `None` is returned.
    pub fn bounds(&self, loca: &Loca<'_>, glyph_id: u16) -> Result<Option<GlyphBounds>> {
        let Some(body) = self.glyph_bytes(loca, glyph_id)? else {
            return Ok(None);
        };
        if body.len() < 10 {
            return Err(Error::Truncated {
                offset: 0,
                context: "glyf header shorter than 10 bytes",
            });
        }
        let mut r = Reader::new(body);
        let num_contours = r.read_i16()?;
        let x_min = r.read_i16()?;
        let y_min = r.read_i16()?;
        let x_max = r.read_i16()?;
        let y_max = r.read_i16()?;
        Ok(Some(GlyphBounds {
            x_min,
            y_min,
            x_max,
            y_max,
            num_contours,
        }))
    }

    /// Drives `sink` with the ops for `glyph_id`, flattening
    /// composite glyphs recursively. `deltas` is an optional list
    /// of `(dx, dy)` pairs in the glyph's point order — supply the
    /// output of [`crate::tables::Gvar::glyph_deltas`] folded into a
    /// dense `[f32; num_points]` pair to apply variable-font
    /// deltas. Pass `None` for the coord-free path.
    ///
    /// Returns `Ok(false)` when the glyph id is valid but has no
    /// outline data (whitespace glyph), `Ok(true)` otherwise.
    pub fn outline<S: OutlineSink>(
        &self,
        loca: &Loca<'_>,
        glyph_id: u16,
        deltas: Option<&[(f32, f32)]>,
        sink: &mut S,
    ) -> Result<bool> {
        // Composite flattening is driven by an explicit transform
        // stack instead of nested `ChildSink` wrappers — nesting
        // blows up monomorphisation. Identity as the initial frame.
        let identity = Transform::identity();
        self.outline_with_transform(loca, glyph_id, deltas, &identity, sink, 0)
    }

    fn outline_with_transform<S: OutlineSink>(
        &self,
        loca: &Loca<'_>,
        glyph_id: u16,
        deltas: Option<&[(f32, f32)]>,
        tf: &Transform,
        sink: &mut S,
        depth: u8,
    ) -> Result<bool> {
        if depth > MAX_COMPOSITE_DEPTH {
            return Err(Error::Malformed {
                offset: 0,
                context: "glyf composite recursion exceeded cap",
            });
        }
        let Some(body) = self.glyph_bytes(loca, glyph_id)? else {
            return Ok(false);
        };
        if body.len() < 10 {
            return Ok(false);
        }
        let mut r = Reader::new(body);
        let num_contours = r.read_i16()?;
        r.skip(8)?; // bbox
        if num_contours >= 0 {
            let mut wrapper = TransformedSink { sink, tf };
            emit_simple_glyph(&mut r, num_contours as u16, deltas, &mut wrapper)?;
        } else {
            self.emit_composite(&mut r, loca, tf, sink, depth)?;
        }
        Ok(true)
    }

    fn emit_composite<S: OutlineSink>(
        &self,
        r: &mut Reader<'_>,
        loca: &Loca<'_>,
        parent: &Transform,
        sink: &mut S,
        depth: u8,
    ) -> Result<()> {
        loop {
            let flags = r.read_u16()?;
            let component_id = r.read_u16()?;

            // Arguments: either two i16 (WORDS) or two i8 (bytes).
            // ARGS_ARE_XY_VALUES distinguishes translation (used
            // here) from anchor-point matching (OK to ignore for
            // outline flattening of typical fonts — they stay at
            // (0,0) offset which matches the glyph's own layout).
            let (dx, dy): (f32, f32) = if flags & COMP_ARG_1_AND_2_ARE_WORDS != 0 {
                let a = r.read_i16()?;
                let b = r.read_i16()?;
                if flags & COMP_ARGS_ARE_XY_VALUES != 0 {
                    (f32::from(a), f32::from(b))
                } else {
                    (0.0, 0.0)
                }
            } else {
                let a = r.read_i8()?;
                let b = r.read_i8()?;
                if flags & COMP_ARGS_ARE_XY_VALUES != 0 {
                    (f32::from(a), f32::from(b))
                } else {
                    (0.0, 0.0)
                }
            };

            let (mut xx, mut xy, mut yx, mut yy) = (1.0_f32, 0.0_f32, 0.0_f32, 1.0_f32);
            if flags & COMP_WE_HAVE_A_SCALE != 0 {
                let s = r.read_f2dot14()?;
                xx = s;
                yy = s;
            } else if flags & COMP_WE_HAVE_AN_X_AND_Y_SCALE != 0 {
                xx = r.read_f2dot14()?;
                yy = r.read_f2dot14()?;
            } else if flags & COMP_WE_HAVE_A_TWO_BY_TWO != 0 {
                xx = r.read_f2dot14()?;
                xy = r.read_f2dot14()?;
                yx = r.read_f2dot14()?;
                yy = r.read_f2dot14()?;
            }

            // SCALED_COMPONENT_OFFSET: the spec allows Apple-style
            // offset scaling; most fonts don't use it. We honour it
            // by pre-multiplying the translation through the 2x2.
            let (tx, ty) = if flags & COMP_SCALED_COMPONENT_OFFSET != 0
                && flags & COMP_UNSCALED_COMPONENT_OFFSET == 0
            {
                (xx * dx + xy * dy, yx * dx + yy * dy)
            } else {
                (dx, dy)
            };

            let local = Transform { xx, xy, yx, yy, tx, ty };
            let combined = parent.compose(&local);

            // Composite children are always drawn without deltas —
            // gvar deltas for composites target the composite's own
            // translation offsets rather than child points; sigilbuzz
            // does not yet apply that per-component delta, so the
            // child draws in its own design space.
            self.outline_with_transform(loca, component_id, None, &combined, sink, depth + 1)?;

            if flags & COMP_MORE_COMPONENTS == 0 {
                break;
            }
            let _ = (COMP_ROUND_XY_TO_GRID, COMP_USE_MY_METRICS); // silence unused constants
        }
        // If WE_HAVE_INSTRUCTIONS is set the composite ends with a
        // u16 instruction count + that many bytes. We don't execute
        // TT hints so we stop here — the caller already has the
        // flat outline.
        Ok(())
    }
}

/// A 2x2 + translation affine transform. Used to flatten composite
/// glyphs without monomorphising a nested sink tower.
#[derive(Debug, Clone, Copy)]
struct Transform {
    xx: f32,
    xy: f32,
    yx: f32,
    yy: f32,
    tx: f32,
    ty: f32,
}

impl Transform {
    const fn identity() -> Self {
        Self {
            xx: 1.0,
            xy: 0.0,
            yx: 0.0,
            yy: 1.0,
            tx: 0.0,
            ty: 0.0,
        }
    }

    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.xx * x + self.xy * y + self.tx,
            self.yx * x + self.yy * y + self.ty,
        )
    }

    /// `self ∘ other` — apply `other` first, then `self`. Used by
    /// composites to chain parent × child matrices.
    fn compose(&self, other: &Self) -> Self {
        let xx = self.xx * other.xx + self.xy * other.yx;
        let xy = self.xx * other.xy + self.xy * other.yy;
        let yx = self.yx * other.xx + self.yy * other.yx;
        let yy = self.yx * other.xy + self.yy * other.yy;
        let (tx, ty) = self.apply(other.tx, other.ty);
        Self { xx, xy, yx, yy, tx, ty }
    }
}

fn emit_simple_glyph<S: OutlineSink>(
    r: &mut Reader<'_>,
    num_contours: u16,
    deltas: Option<&[(f32, f32)]>,
    sink: &mut S,
) -> Result<()> {
    if num_contours == 0 {
        return Ok(());
    }
    // endPtsOfContours.
    let mut end_pts = Vec::with_capacity(num_contours as usize);
    for _ in 0..num_contours {
        end_pts.push(r.read_u16()?);
    }
    let total_points = end_pts
        .last()
        .copied()
        .map(|e| e.saturating_add(1))
        .unwrap_or(0);

    // instructions — skip.
    let instr_len = r.read_u16()? as usize;
    r.skip(instr_len)?;

    // Flags with REPEAT expansion.
    let mut flags = Vec::with_capacity(total_points as usize);
    while flags.len() < total_points as usize {
        let f = r.read_u8()?;
        flags.push(f);
        if f & FLAG_REPEAT != 0 {
            let rep = r.read_u8()?;
            for _ in 0..rep {
                flags.push(f);
                if flags.len() >= total_points as usize {
                    break;
                }
            }
        }
    }
    flags.truncate(total_points as usize);

    // X coordinates.
    let mut xs = Vec::with_capacity(total_points as usize);
    let mut x_cur: i32 = 0;
    for &f in &flags {
        let short = f & FLAG_X_SHORT != 0;
        let same_or_pos = f & FLAG_X_SAME_OR_POS != 0;
        let delta: i32 = if short {
            let v = i32::from(r.read_u8()?);
            if same_or_pos {
                v
            } else {
                -v
            }
        } else if same_or_pos {
            0
        } else {
            i32::from(r.read_i16()?)
        };
        x_cur += delta;
        xs.push(x_cur);
    }

    // Y coordinates.
    let mut ys = Vec::with_capacity(total_points as usize);
    let mut y_cur: i32 = 0;
    for &f in &flags {
        let short = f & FLAG_Y_SHORT != 0;
        let same_or_pos = f & FLAG_Y_SAME_OR_POS != 0;
        let delta: i32 = if short {
            let v = i32::from(r.read_u8()?);
            if same_or_pos {
                v
            } else {
                -v
            }
        } else if same_or_pos {
            0
        } else {
            i32::from(r.read_i16()?)
        };
        y_cur += delta;
        ys.push(y_cur);
    }

    // Materialise absolute points with optional deltas.
    let mut points = Vec::with_capacity(total_points as usize);
    for (i, &f) in flags.iter().enumerate() {
        let mut x = xs[i] as f32;
        let mut y = ys[i] as f32;
        if let Some(ds) = deltas {
            if let Some(&(dx, dy)) = ds.get(i) {
                x += dx;
                y += dy;
            }
        }
        points.push(Point {
            x,
            y,
            on_curve: f & FLAG_ON_CURVE != 0,
        });
    }

    // Emit per contour.
    let mut start: usize = 0;
    for &end in &end_pts {
        let end_idx = end as usize;
        if end_idx >= points.len() || end_idx < start {
            return Err(Error::Malformed {
                offset: 0,
                context: "glyf endPtsOfContours out of range",
            });
        }
        emit_contour(&points[start..=end_idx], sink);
        start = end_idx + 1;
    }
    Ok(())
}

/// Emits the TrueType quadratic-pair expansion for one closed contour.
///
/// Matches ttf-parser's convention: every `Close` is preceded by an
/// explicit `LineTo` back to the start when the last emitted point
/// differs from it.
///
/// - Two consecutive off-curve points imply an on-curve midpoint.
/// - A contour that begins off-curve either borrows its last point
///   as the implicit start (when the last is on-curve) or uses the
///   midpoint between first and last.
fn emit_contour<S: OutlineSink>(pts: &[Point], sink: &mut S) {
    if pts.is_empty() {
        return;
    }
    let n = pts.len();

    // Determine the starting on-curve point.
    let first_on_curve = pts[0].on_curve;
    let last_on_curve = pts[n - 1].on_curve;
    let (start_x, start_y) = if first_on_curve {
        (pts[0].x, pts[0].y)
    } else if last_on_curve {
        (pts[n - 1].x, pts[n - 1].y)
    } else {
        // Midpoint between first and last off-curve points.
        ((pts[0].x + pts[n - 1].x) * 0.5, (pts[0].y + pts[n - 1].y) * 0.5)
    };
    sink.move_to(start_x, start_y);

    // Single-point contour — nothing more to draw.
    if n == 1 {
        sink.close();
        return;
    }

    // Iterate in the order the contour is walked. The start anchor
    // has already been emitted; set the cursor accordingly.
    //
    // ttf-parser models the contour as a ring: when the first point
    // is off-curve and the last is on-curve, the last point supplies
    // the start anchor and we walk from index 0 through n-1 with the
    // convention that the endpoint of the final quad is the start.
    let skip_first = first_on_curve;
    let end_before_wrap = if !first_on_curve && last_on_curve { n - 1 } else { n };

    let mut i = if skip_first { 1 } else { 0 };
    let mut cur_x = start_x;
    let mut cur_y = start_y;

    while i < end_before_wrap {
        let p = pts[i];
        if p.on_curve {
            sink.line_to(p.x, p.y);
            cur_x = p.x;
            cur_y = p.y;
            i += 1;
        } else {
            // Off-curve control. The next point either is on-curve
            // (direct endpoint) or off-curve (implicit midpoint
            // becomes endpoint). When `i == end_before_wrap - 1`
            // the "next" point wraps to the contour's first
            // unskipped index.
            let next_idx = i + 1;
            if next_idx < end_before_wrap {
                let q = pts[next_idx];
                if q.on_curve {
                    sink.quad_to(p.x, p.y, q.x, q.y);
                    cur_x = q.x;
                    cur_y = q.y;
                    i = next_idx + 1;
                } else {
                    let mx = (p.x + q.x) * 0.5;
                    let my = (p.y + q.y) * 0.5;
                    sink.quad_to(p.x, p.y, mx, my);
                    cur_x = mx;
                    cur_y = my;
                    i = next_idx;
                }
            } else {
                // Off-curve is the last point in the walk. The
                // endpoint is the contour's start.
                sink.quad_to(p.x, p.y, start_x, start_y);
                cur_x = start_x;
                cur_y = start_y;
                i = next_idx;
            }
        }
    }

    // Emit an explicit LineTo back to the start when we didn't
    // already land there. Matches ttf-parser / HarfBuzz convention.
    if (cur_x - start_x).abs() > 1e-6 || (cur_y - start_y).abs() > 1e-6 {
        sink.line_to(start_x, start_y);
    }
    sink.close();
}

/// Outline sink wrapper that applies a flat `Transform` (borrowed)
/// to every emitted op. Composite flattening builds the composed
/// transform per component and wraps the caller's sink exactly once
/// — the recursive call takes a fresh `TransformedSink` with the
/// composed matrix, so the resulting monomorphisation depth is
/// bounded by 1 (identity wrapper → transformed wrapper).
struct TransformedSink<'s, 't, S: OutlineSink> {
    sink: &'s mut S,
    tf: &'t Transform,
}

impl<'s, 't, S: OutlineSink> OutlineSink for TransformedSink<'s, 't, S> {
    fn move_to(&mut self, x: f32, y: f32) {
        let (mx, my) = self.tf.apply(x, y);
        self.sink.move_to(mx, my);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let (mx, my) = self.tf.apply(x, y);
        self.sink.line_to(mx, my);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (cx, cy) = self.tf.apply(cx, cy);
        let (mx, my) = self.tf.apply(x, y);
        self.sink.quad_to(cx, cy, mx, my);
    }
    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        let (c1x, c1y) = self.tf.apply(c1x, c1y);
        let (c2x, c2y) = self.tf.apply(c2x, c2y);
        let (mx, my) = self.tf.apply(x, y);
        self.sink.curve_to(c1x, c1y, c2x, c2y, mx, my);
    }
    fn close(&mut self) {
        self.sink.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::head::IndexToLocFormat;
    use crate::tables::outline::{Outline, PathOp};
    use alloc::vec;
    use alloc::vec::Vec;

    fn build_header(num_contours: i16, xmin: i16, ymin: i16, xmax: i16, ymax: i16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&num_contours.to_be_bytes());
        out.extend_from_slice(&xmin.to_be_bytes());
        out.extend_from_slice(&ymin.to_be_bytes());
        out.extend_from_slice(&xmax.to_be_bytes());
        out.extend_from_slice(&ymax.to_be_bytes());
        out
    }

    fn build_loca_short(offsets: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        for o in offsets {
            out.extend_from_slice(&o.to_be_bytes());
        }
        out
    }

    #[test]
    fn reads_bounds_from_simple_glyph() {
        let g0_body: Vec<u8> = Vec::new();
        let g1_body = build_header(1, 10, -200, 500, 1500);

        let mut glyf = Vec::new();
        glyf.extend_from_slice(&g0_body);
        glyf.extend_from_slice(&g1_body);

        let loca_bytes = build_loca_short(&[0, 0, (g1_body.len() as u16) / 2]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 2).unwrap();
        let glyf_view = Glyf::new(&glyf);

        assert!(glyf_view.bounds(&loca, 0).unwrap().is_none());
        let b = glyf_view.bounds(&loca, 1).unwrap().unwrap();
        assert_eq!(b.num_contours, 1);
        assert_eq!(b.x_min, 10);
        assert_eq!(b.y_min, -200);
        assert_eq!(b.x_max, 500);
        assert_eq!(b.y_max, 1500);
    }

    #[test]
    fn composite_glyph_reports_negative_contour_count() {
        let body = build_header(-1, 0, 0, 1000, 1000);
        let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&body);
        let b = glyf.bounds(&loca, 0).unwrap().unwrap();
        assert_eq!(b.num_contours, -1);
    }

    #[test]
    fn out_of_range_glyph_yields_none_from_loca() {
        let loca_bytes = build_loca_short(&[0, 10]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&[0u8; 20]);
        assert!(glyf.bounds(&loca, 7).unwrap().is_none());
    }

    #[test]
    fn rejects_range_past_glyf_end() {
        let loca_bytes = build_loca_short(&[0, 50]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&[0u8; 8]);
        assert!(glyf.bounds(&loca, 0).is_err());
    }

    #[test]
    fn point_count_simple_glyph_adds_four_phantom_points() {
        let mut body = build_header(1, 0, 0, 100, 100);
        body.extend_from_slice(&3u16.to_be_bytes());
        let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&body);
        assert_eq!(glyf.point_count(&loca, 0).unwrap(), Some(8));
    }

    #[test]
    fn point_count_composite_glyph_returns_none() {
        let body = build_header(-1, 0, 0, 100, 100);
        let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&body);
        assert_eq!(glyf.point_count(&loca, 0).unwrap(), None);
    }

    #[test]
    fn point_count_empty_glyph_returns_none() {
        let loca_bytes = build_loca_short(&[0, 0]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&[]);
        assert_eq!(glyf.point_count(&loca, 0).unwrap(), None);
    }

    #[test]
    fn rejects_range_shorter_than_header() {
        let loca_bytes = build_loca_short(&[0, 3]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&[0u8; 10]);
        assert!(glyf.bounds(&loca, 0).is_err());
    }

    // ------------------------------------------------------------------
    // Simple-glyph outline decoding.
    // ------------------------------------------------------------------

    /// Builds a simple glyph with absolute point coordinates, marking
    /// each point as on/off curve. Forces long-form x/y flags so the
    /// test fixtures are easy to read.
    fn build_simple_glyph(end_pts: &[u16], pts: &[(i16, i16, bool)]) -> Vec<u8> {
        let mut body = build_header(end_pts.len() as i16, 0, 0, 1000, 1000);
        for e in end_pts {
            body.extend_from_slice(&e.to_be_bytes());
        }
        body.extend_from_slice(&0u16.to_be_bytes()); // instructions length

        // Flags: ON_CURVE bit only; neither short nor same.
        for &(_, _, on) in pts {
            let f = if on { FLAG_ON_CURVE } else { 0 };
            body.push(f);
        }
        // X as deltas from previous (starting at 0), long form.
        let mut prev = 0i16;
        for &(x, _, _) in pts {
            let dx = x - prev;
            body.extend_from_slice(&dx.to_be_bytes());
            prev = x;
        }
        let mut prev = 0i16;
        for &(_, y, _) in pts {
            let dy = y - prev;
            body.extend_from_slice(&dy.to_be_bytes());
            prev = y;
        }
        body
    }

    #[test]
    fn simple_glyph_rectangle_emits_four_lines() {
        // Closed rectangle: four on-curve corners. ttf-parser's
        // convention (and ours) emits an explicit LineTo back to the
        // start before Close.
        let pts = [
            (100, 100, true),
            (500, 100, true),
            (500, 400, true),
            (100, 400, true),
        ];
        let body = build_simple_glyph(&[3], &pts);
        let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&body);
        let mut o = Outline::new();
        glyf.outline(&loca, 0, None, &mut o).unwrap();
        assert_eq!(
            o.ops(),
            &[
                PathOp::MoveTo { x: 100.0, y: 100.0 },
                PathOp::LineTo { x: 500.0, y: 100.0 },
                PathOp::LineTo { x: 500.0, y: 400.0 },
                PathOp::LineTo { x: 100.0, y: 400.0 },
                PathOp::LineTo { x: 100.0, y: 100.0 },
                PathOp::Close,
            ]
        );
    }

    #[test]
    fn simple_glyph_two_consecutive_off_curve_implies_midpoint() {
        // Contour: on(0,0), off(10,20), off(30,20), on(40,0). Two
        // off-curve points in a row → implicit midpoint at (20, 20).
        let pts = [
            (0, 0, true),
            (10, 20, false),
            (30, 20, false),
            (40, 0, true),
        ];
        let body = build_simple_glyph(&[3], &pts);
        let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&body);
        let mut o = Outline::new();
        glyf.outline(&loca, 0, None, &mut o).unwrap();
        // Expected: MoveTo(0,0), QuadTo(10,20 -> 20,20),
        //           QuadTo(30,20 -> 40,0), LineTo(0,0), Close.
        let ops = o.ops();
        assert!(matches!(ops[0], PathOp::MoveTo { x: 0.0, y: 0.0 }));
        match ops[1] {
            PathOp::QuadTo { cx, cy, x, y } => {
                assert!((cx - 10.0).abs() < 1e-4);
                assert!((cy - 20.0).abs() < 1e-4);
                assert!((x - 20.0).abs() < 1e-4);
                assert!((y - 20.0).abs() < 1e-4);
            }
            _ => panic!("expected QuadTo at 1"),
        }
        match ops[2] {
            PathOp::QuadTo { cx, cy, x, y } => {
                assert!((cx - 30.0).abs() < 1e-4);
                assert!((cy - 20.0).abs() < 1e-4);
                assert!((x - 40.0).abs() < 1e-4);
                assert!((y - 0.0).abs() < 1e-4);
            }
            _ => panic!("expected QuadTo at 2"),
        }
        assert!(matches!(ops[3], PathOp::LineTo { x: 0.0, y: 0.0 }));
        assert!(matches!(ops[4], PathOp::Close));
    }

    #[test]
    fn simple_glyph_with_deltas_shifts_points() {
        // Rectangle again, this time with gvar deltas moving every
        // point by (+5, -3).
        let pts = [
            (100, 100, true),
            (500, 100, true),
            (500, 400, true),
            (100, 400, true),
        ];
        let body = build_simple_glyph(&[3], &pts);
        let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&body);
        let mut o = Outline::new();
        let deltas: Vec<(f32, f32)> = vec![(5.0, -3.0); 4];
        glyf.outline(&loca, 0, Some(&deltas), &mut o).unwrap();
        assert!(matches!(o.ops()[0], PathOp::MoveTo { x: 105.0, y: 97.0 }));
        assert!(matches!(o.ops()[1], PathOp::LineTo { x: 505.0, y: 97.0 }));
    }

    // ------------------------------------------------------------------
    // Composite-glyph flattening.
    // ------------------------------------------------------------------

    #[test]
    fn composite_glyph_translates_child_outline() {
        // Child (glyph 1): rectangle at origin 0..100 × 0..100.
        let child = build_simple_glyph(
            &[3],
            &[(0, 0, true), (100, 0, true), (100, 100, true), (0, 100, true)],
        );
        // Parent (glyph 0): composite referencing child with translation (+200, +300).
        let mut parent = build_header(-1, 0, 0, 400, 500);
        let flags: u16 = COMP_ARGS_ARE_XY_VALUES | COMP_ARG_1_AND_2_ARE_WORDS;
        parent.extend_from_slice(&flags.to_be_bytes());
        parent.extend_from_slice(&1u16.to_be_bytes()); // component id = 1
        parent.extend_from_slice(&200i16.to_be_bytes()); // dx
        parent.extend_from_slice(&300i16.to_be_bytes()); // dy
        // no MORE_COMPONENTS → single component.

        // Lay out glyf with parent first, child second.
        let mut glyf_bytes = Vec::new();
        let parent_off = 0u32;
        glyf_bytes.extend_from_slice(&parent);
        // Pad to even boundary (short loca format needs even offsets).
        if glyf_bytes.len() % 2 != 0 {
            glyf_bytes.push(0);
        }
        let child_off = glyf_bytes.len() as u32;
        glyf_bytes.extend_from_slice(&child);
        if glyf_bytes.len() % 2 != 0 {
            glyf_bytes.push(0);
        }
        let end_off = glyf_bytes.len() as u32;

        let loca_bytes = build_loca_short(&[
            (parent_off / 2) as u16,
            (child_off / 2) as u16,
            (end_off / 2) as u16,
        ]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 2).unwrap();
        let glyf = Glyf::new(&glyf_bytes);

        let mut o = Outline::new();
        glyf.outline(&loca, 0, None, &mut o).unwrap();
        // Child starts at (0,0), translated to (200, 300). The
        // closing LineTo brings the pen back to the start before
        // Close — matches ttf-parser's convention.
        assert!(matches!(o.ops()[0], PathOp::MoveTo { x: 200.0, y: 300.0 }));
        assert!(matches!(o.ops()[1], PathOp::LineTo { x: 300.0, y: 300.0 }));
        assert!(matches!(o.ops()[2], PathOp::LineTo { x: 300.0, y: 400.0 }));
        assert!(matches!(o.ops()[3], PathOp::LineTo { x: 200.0, y: 400.0 }));
        assert!(matches!(o.ops()[4], PathOp::LineTo { x: 200.0, y: 300.0 }));
        assert!(matches!(o.ops()[5], PathOp::Close));
    }

    #[test]
    fn composite_with_scale_doubles_child() {
        // Child: unit square at (0,0)..(100,100). Parent scales ×2.
        let child = build_simple_glyph(
            &[3],
            &[(0, 0, true), (100, 0, true), (100, 100, true), (0, 100, true)],
        );
        let mut parent = build_header(-1, 0, 0, 200, 200);
        let flags: u16 =
            COMP_ARGS_ARE_XY_VALUES | COMP_ARG_1_AND_2_ARE_WORDS | COMP_WE_HAVE_A_SCALE;
        parent.extend_from_slice(&flags.to_be_bytes());
        parent.extend_from_slice(&1u16.to_be_bytes());
        parent.extend_from_slice(&0i16.to_be_bytes());
        parent.extend_from_slice(&0i16.to_be_bytes());
        // Scale 2.0 in F2Dot14 = 32768, but that overflows i16 — the
        // spec tops out at 2x so store 0x7FFF as a close proxy, or
        // just test with 1.5 (which fits as 24576).
        let scale_raw: i16 = 24576; // 1.5
        parent.extend_from_slice(&scale_raw.to_be_bytes());

        let mut glyf_bytes = Vec::new();
        let parent_off = 0u32;
        glyf_bytes.extend_from_slice(&parent);
        if glyf_bytes.len() % 2 != 0 {
            glyf_bytes.push(0);
        }
        let child_off = glyf_bytes.len() as u32;
        glyf_bytes.extend_from_slice(&child);
        if glyf_bytes.len() % 2 != 0 {
            glyf_bytes.push(0);
        }
        let end_off = glyf_bytes.len() as u32;

        let loca_bytes = build_loca_short(&[
            (parent_off / 2) as u16,
            (child_off / 2) as u16,
            (end_off / 2) as u16,
        ]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 2).unwrap();
        let glyf = Glyf::new(&glyf_bytes);

        let mut o = Outline::new();
        glyf.outline(&loca, 0, None, &mut o).unwrap();
        // 1.5 × (100, 100) = (150, 150).
        match o.ops()[2] {
            PathOp::LineTo { x, y } => {
                assert!((x - 150.0).abs() < 1e-3);
                assert!((y - 150.0).abs() < 1e-3);
            }
            _ => panic!("expected LineTo at 2"),
        }
    }

    #[test]
    fn composite_recursion_limit_rejects_self_reference() {
        // Glyph 0 references glyph 0 — infinite loop.
        let mut body = build_header(-1, 0, 0, 1000, 1000);
        let flags: u16 = COMP_ARGS_ARE_XY_VALUES | COMP_ARG_1_AND_2_ARE_WORDS;
        body.extend_from_slice(&flags.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes()); // self reference
        body.extend_from_slice(&0i16.to_be_bytes());
        body.extend_from_slice(&0i16.to_be_bytes());
        if body.len() % 2 != 0 {
            body.push(0);
        }
        let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&body);
        let mut o = Outline::new();
        assert!(glyf.outline(&loca, 0, None, &mut o).is_err());
    }
}
