//! `glyf`: TrueType glyph data.
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
//! 2x2 transform and a translation. Components may reference further
//! composites; sigilbuzz caps recursion to avoid pathological fonts.
//!
//! # Composite flattening: two passes
//!
//! Outline emission is split into two passes. Pass 1 walks the glyph
//! and any composite children into a flat point list with absolute
//! coordinates (gvar deltas + 2x2 + translation already folded in).
//! Pass 2 walks the contour list and dispatches to the caller's
//! [`OutlineSink`].
//!
//! The intermediate point list is what supports
//! `ARGS_ARE_XY_VALUES`-clear *anchor-mode* components: when the
//! component flag bit is clear, `arg1` and `arg2` are point indices
//! into the parent's already-flattened points and the child's own
//! flattened points respectively. The translation is implied:
//! `parent[arg1] - child[arg2]`, so we need both sides as concrete
//! coordinates before we can emit the child's ops.
//!
//! Phantom-point references are resolved against hmtx (and vmtx if
//! present) at flatten time. The phantom-anchor branch is covered by
//! the `phantom_anchor_fixture_outlines_match_ttf_parser` integration
//! test (hand-crafted ~1 KB fixture under `tests/fixtures/`).

mod composite;
mod simple;

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::hmtx::Hmtx;
use crate::tables::loca::Loca;
use crate::tables::outline::OutlineSink;
use crate::tables::parse::{abs_f32, Reader};
use crate::tables::vmtx::Vmtx;

use simple::flatten_simple_glyph;

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

/// Bundle of metric tables a [`Glyf`] flattener needs to synthesize
/// phantom points during composite anchor-mode resolution.
///
/// TrueType reserves four phantom points per glyph past the contour
/// list:
/// - `pp1 = (xMin - lsb, 0)`: left-side-bearing origin.
/// - `pp2 = (xMin - lsb + advanceWidth, 0)`: advance-width origin.
/// - `pp3 = (0, yMax + tsb)`: top-side-bearing origin.
/// - `pp4 = (0, yMax + tsb - advanceHeight)`: advance-height origin.
///
/// Composite components in anchor-mode (`ARGS_ARE_XY_VALUES` clear)
/// can index past the contour-point count into these four slots; real
/// fonts use this to align components to the parent's advance-width
/// origin without hard-coded offsets.
///
/// `vmtx` is optional: horizontal-only fonts have no `vmtx` and the
/// vertical phantoms collapse to `(0, 0)`. Real-world anchor-mode
/// glyphs in horizontal fonts only ever index pp1 / pp2, so the
/// fallback is safe.
#[derive(Debug, Clone, Copy)]
pub struct PhantomMetrics<'a> {
    /// Horizontal metrics. Required: every TrueType font has hmtx.
    pub hmtx: &'a Hmtx<'a>,
    /// Vertical metrics. `None` for horizontal-only fonts.
    pub vmtx: Option<&'a Vmtx<'a>>,
}

/// A borrowed view of the `glyf` table. Parsing is free: accessors
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

// Composite-glyph flag bits. ROUND_XY_TO_GRID (0x0004),
// WE_HAVE_INSTRUCTIONS (0x0100), and USE_MY_METRICS (0x0200) only
// matter to hinting and metrics, so the outline walk ignores them.
const COMP_ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
const COMP_ARGS_ARE_XY_VALUES: u16 = 0x0002;
const COMP_WE_HAVE_A_SCALE: u16 = 0x0008;
const COMP_MORE_COMPONENTS: u16 = 0x0020;
const COMP_WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
const COMP_WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
const COMP_SCALED_COMPONENT_OFFSET: u16 = 0x0800;
const COMP_UNSCALED_COMPONENT_OFFSET: u16 = 0x1000;

/// Hard cap on composite recursion depth. The OpenType spec imposes
/// no fixed bound, but HarfBuzz uses 64 and in-the-wild glyphs never
/// exceed a handful of levels.
const MAX_COMPOSITE_DEPTH: u8 = 64;

/// Cap on the glyph records one outline walk may visit, the root
/// glyph included. A composite can name the same child many times at
/// every level, so the depth cap alone lets a few bytes expand into
/// billions of visits. Real composites visit a handful of glyphs.
const MAX_FLATTEN_GLYPHS: u32 = 1 << 16;

/// Cap on the contour points one outline walk may lay down across
/// every simple glyph it visits. One simple glyph holds at most
/// 65,535 points, and real composites hold far fewer in total.
const MAX_FLATTEN_POINTS: usize = 1 << 18;

/// Remaining work for one outline walk. See [`MAX_FLATTEN_GLYPHS`]
/// and [`MAX_FLATTEN_POINTS`].
struct FlattenBudget {
    glyphs: u32,
    points: usize,
}

impl FlattenBudget {
    const fn new() -> Self {
        Self {
            glyphs: MAX_FLATTEN_GLYPHS,
            points: MAX_FLATTEN_POINTS,
        }
    }

    fn take_glyph(&mut self) -> Result<()> {
        self.glyphs = self.glyphs.checked_sub(1).ok_or(Error::Malformed {
            offset: 0,
            context: "glyf composite visits too many glyphs",
        })?;
        Ok(())
    }

    fn take_points(&mut self, n: usize) -> Result<()> {
        self.points = self.points.checked_sub(n).ok_or(Error::Malformed {
            offset: 0,
            context: "glyf composite expands to too many points",
        })?;
        Ok(())
    }
}

/// Rounds a float to the nearest `i16`, saturating at the type bounds.
/// Mirrors the helper in [`crate::Face`]; duplicated here so the glyf
/// module stays self-contained for `no_std` callers.
fn round_f32_to_i16(v: f32) -> i16 {
    let adj = if v >= 0.0 { v + 0.5 } else { v - 0.5 };
    let clamped = adj.max(i16::MIN as f32).min(i16::MAX as f32);
    clamped as i16
}

impl<'a> Glyf<'a> {
    /// Wraps the raw `glyf` bytes. No validation up front: the
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

    /// Computes the four phantom points for `glyph_id` in the glyph's
    /// own (untransformed) design-unit frame.
    ///
    /// pp1 / pp2 always read from `hmtx`. pp3 / pp4 read from `vmtx`
    /// when available; horizontal-only fonts get `(0, 0)` for both,
    /// which matches every in-the-wild glyph we've checked: anchor
    /// indices for vertical phantoms only show up in CJK fonts that
    /// also ship `vmtx`. Glyphs without a `glyf` body get all-zero
    /// phantoms, which collapses anchor mode to a zero translation
    /// like the legacy fallback before phantom resolution
    /// landed.
    fn phantom_points(
        &self,
        loca: &Loca<'_>,
        glyph_id: u16,
        metrics: &PhantomMetrics<'_>,
    ) -> Result<[(f32, f32); 4]> {
        let bounds = self.bounds(loca, glyph_id)?;
        let (x_min, y_max) = match bounds {
            Some(b) => (f32::from(b.x_min), f32::from(b.y_max)),
            None => (0.0, 0.0),
        };
        let advance_w = f32::from(metrics.hmtx.advance(glyph_id).unwrap_or(0));
        let lsb = f32::from(metrics.hmtx.lsb(glyph_id).unwrap_or(0));
        let pp1_x = x_min - lsb;
        let pp2_x = pp1_x + advance_w;

        let (pp3_y, pp4_y) = if let Some(vmtx) = metrics.vmtx {
            let advance_h = f32::from(vmtx.advance(glyph_id).unwrap_or(0));
            let tsb = f32::from(vmtx.tsb(glyph_id).unwrap_or(0));
            let pp3 = y_max + tsb;
            (pp3, pp3 - advance_h)
        } else {
            (0.0, 0.0)
        };

        Ok([(pp1_x, 0.0), (pp2_x, 0.0), (0.0, pp3_y), (0.0, pp4_y)])
    }

    /// Returns the glyph's raw points in glyf-natural order: every
    /// contour point (on-curve and off-curve, in the order they appear
    /// in the glyph's `glyf` data) followed by the four phantom points
    /// (pp1 = LSB origin, pp2 = advance-width origin, pp3 = TSB origin,
    /// pp4 = advance-height origin).
    ///
    /// Used by `kerx` format-4 action type 0, which references glyph
    /// points by index, including off-curve control points and the
    /// trailing phantoms. For composite glyphs the flat point list
    /// returned by `Glyf::flatten` is the same one composite anchor
    /// mode resolves against, so indices stay consistent across both
    /// callers.
    ///
    /// `vmtx` is optional: horizontal-only fonts have no `vmtx` and the
    /// vertical phantoms collapse to `(0, 0)`, same fallback as
    /// composite anchor-mode resolution.
    ///
    /// Returns `Ok(None)` when the glyph id is out of range or has no
    /// outline body. Coordinates are rounded to the nearest `i16`
    /// using sigilbuzz's standard half-away-from-zero policy; this
    /// matches the FUnit-integer coords kerx fmt 4 type 0 expects.
    pub fn glyph_points(
        &self,
        loca: &Loca<'_>,
        glyph_id: u16,
        hmtx: &Hmtx<'_>,
        vmtx: Option<&Vmtx<'_>>,
    ) -> Result<Option<Vec<(i16, i16)>>> {
        let metrics = PhantomMetrics { hmtx, vmtx };
        let mut flat = FlatGlyph::default();
        let identity = Transform::identity();
        let drew = self.flatten(
            loca,
            glyph_id,
            None,
            Some(&metrics),
            &identity,
            &mut flat,
            0,
            &mut FlattenBudget::new(),
        )?;
        if !drew {
            return Ok(None);
        }
        let pp = self.phantom_points(loca, glyph_id, &metrics)?;
        let mut out = Vec::with_capacity(flat.points.len() + 4);
        for &(x, y) in &flat.points {
            out.push((round_f32_to_i16(x), round_f32_to_i16(y)));
        }
        for &(px, py) in &pp {
            out.push((round_f32_to_i16(px), round_f32_to_i16(py)));
        }
        Ok(Some(out))
    }

    /// Drives `sink` with the ops for `glyph_id`, flattening
    /// composite glyphs recursively. `deltas` is an optional list
    /// of `(dx, dy)` pairs in the glyph's point order. Supply the
    /// output of [`crate::tables::Gvar::glyph_deltas`] folded into a
    /// dense `[f32; num_points]` pair to apply variable-font
    /// deltas. Pass `None` for the coord-free path.
    ///
    /// `metrics` supplies `hmtx` (and optionally `vmtx`) so anchor-mode
    /// composites whose anchor index points past the parent's contour
    /// points can resolve against the four phantom points (LSB origin,
    /// advance-width origin, TSB origin, advance-height origin).
    /// Passing `None` keeps the legacy zero-translation fallback for
    /// the rare phantom case, useful for unit tests of synthetic
    /// composites that don't ship metrics.
    ///
    /// Returns `Ok(false)` when the glyph id is valid but has no
    /// outline data (whitespace glyph), `Ok(true)` otherwise.
    pub fn outline<S: OutlineSink>(
        &self,
        loca: &Loca<'_>,
        glyph_id: u16,
        deltas: Option<&[(f32, f32)]>,
        metrics: Option<&PhantomMetrics<'_>>,
        sink: &mut S,
    ) -> Result<bool> {
        // Two-pass flattening: pass 1 walks the glyph (and any
        // composite children) into a flat point list with absolute
        // coordinates; pass 2 emits ops contour by contour. The
        // intermediate point list is what lets composite components
        // resolve `ARGS_ARE_XY_VALUES`-clear anchor-point matching:
        // arg1 indexes into the parent's already-flattened points
        // and arg2 into the freshly-flattened child, so we need both
        // sets of concrete coordinates before we know the child's
        // translation.
        let mut flat = FlatGlyph::default();
        let identity = Transform::identity();
        let drew = self.flatten(
            loca,
            glyph_id,
            deltas,
            metrics,
            &identity,
            &mut flat,
            0,
            &mut FlattenBudget::new(),
        )?;
        if !drew {
            return Ok(false);
        }
        flat.emit(sink);
        Ok(true)
    }

    /// Flattens `glyph_id` (transformed by `tf`) into `out`. Returns
    /// `Ok(false)` for empty / out-of-range glyphs. Recurses through
    /// composite components, with `depth` capped by
    /// [`MAX_COMPOSITE_DEPTH`] and the total work capped by `budget`.
    // The walk threads its tables, transform, output, and limits
    // through every level of the recursion.
    #[allow(clippy::too_many_arguments)]
    fn flatten(
        &self,
        loca: &Loca<'_>,
        glyph_id: u16,
        deltas: Option<&[(f32, f32)]>,
        metrics: Option<&PhantomMetrics<'_>>,
        tf: &Transform,
        out: &mut FlatGlyph,
        depth: u8,
        budget: &mut FlattenBudget,
    ) -> Result<bool> {
        if depth > MAX_COMPOSITE_DEPTH {
            return Err(Error::Malformed {
                offset: 0,
                context: "glyf composite recursion exceeded cap",
            });
        }
        budget.take_glyph()?;
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
            flatten_simple_glyph(&mut r, num_contours as u16, deltas, tf, out, budget)?;
        } else {
            self.flatten_composite(&mut r, loca, glyph_id, metrics, tf, out, depth, budget)?;
        }
        Ok(true)
    }
}

/// A 2x2 + translation affine transform. Used to flatten composite
/// glyphs without monomorphizing a nested sink tower.
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

    /// `self ∘ other`: apply `other` first, then `self`. Used by
    /// composites to chain parent * child matrices.
    fn compose(&self, other: &Self) -> Self {
        let xx = self.xx * other.xx + self.xy * other.yx;
        let xy = self.xx * other.xy + self.xy * other.yy;
        let yx = self.yx * other.xx + self.yy * other.yx;
        let yy = self.yx * other.xy + self.yy * other.yy;
        let (tx, ty) = self.apply(other.tx, other.ty);
        Self {
            xx,
            xy,
            yx,
            yy,
            tx,
            ty,
        }
    }
}

/// Flat point representation in the parent composite's frame. Both
/// real contour points and (eventually) phantom points share this
/// shape. `on_curve` is meaningful only for contour points.
#[derive(Debug, Clone, Copy)]
struct FlatPoint {
    on_curve: bool,
}

/// One closed contour within a [`FlatGlyph`], delimited by start /
/// end indices into the parent point + flag arrays.
#[derive(Debug, Clone, Copy)]
struct Contour {
    start: usize,
    end: usize,
}

/// Two-pass flatten target. Pass 1 of [`Glyf::outline`] fills this
/// with absolute coordinates (deltas + composite transforms already
/// folded in); pass 2 walks `contours` and dispatches to the
/// caller's [`OutlineSink`]. Composite anchor-mode resolution reaches
/// into `points` to compute the parent <-> child anchor pair, which is
/// why the intermediate representation exists.
#[derive(Debug, Default)]
struct FlatGlyph {
    points: Vec<(f32, f32)>,
    flags: Vec<FlatPoint>,
    contours: Vec<Contour>,
}

impl FlatGlyph {
    fn emit<S: OutlineSink>(&self, sink: &mut S) {
        for c in &self.contours {
            // `flatten_simple_glyph` guarantees `start..=end` is in
            // range, and `flatten_composite` only ever copies
            // contiguous slices, so the indexing is safe by
            // construction.
            let coords = &self.points[c.start..=c.end];
            let flags = &self.flags[c.start..=c.end];
            emit_contour(coords, flags, sink);
        }
    }
}

/// Emits the TrueType quadratic-pair expansion for one closed
/// contour. `coords` and `flags` are parallel slices of equal
/// length.
///
/// Matches ttf-parser's convention: every `Close` is preceded by an
/// explicit `LineTo` back to the start when the last emitted point
/// differs from it.
///
/// - Two consecutive off-curve points imply an on-curve midpoint.
/// - A contour that begins off-curve either borrows its last point
///   as the implicit start (when the last is on-curve) or uses the
///   midpoint between first and last.
fn emit_contour<S: OutlineSink>(coords: &[(f32, f32)], flags: &[FlatPoint], sink: &mut S) {
    debug_assert_eq!(coords.len(), flags.len());
    if coords.is_empty() {
        return;
    }
    let n = coords.len();

    // Determine the starting on-curve point.
    let first_on_curve = flags[0].on_curve;
    let last_on_curve = flags[n - 1].on_curve;
    let (start_x, start_y) = if first_on_curve {
        coords[0]
    } else if last_on_curve {
        coords[n - 1]
    } else {
        // Midpoint between first and last off-curve points.
        let (x0, y0) = coords[0];
        let (xn, yn) = coords[n - 1];
        ((x0 + xn) * 0.5, (y0 + yn) * 0.5)
    };
    sink.move_to(start_x, start_y);

    // Single-point contour: nothing more to draw.
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
    let end_before_wrap = if !first_on_curve && last_on_curve {
        n - 1
    } else {
        n
    };

    let mut i = if skip_first { 1 } else { 0 };
    let mut cur_x = start_x;
    let mut cur_y = start_y;

    while i < end_before_wrap {
        let (px, py) = coords[i];
        if flags[i].on_curve {
            sink.line_to(px, py);
            cur_x = px;
            cur_y = py;
            i += 1;
        } else {
            // Off-curve control. The next point either is on-curve
            // (direct endpoint) or off-curve (implicit midpoint
            // becomes endpoint). When `i == end_before_wrap - 1`
            // the "next" point wraps to the contour's first
            // unskipped index.
            let next_idx = i + 1;
            if next_idx < end_before_wrap {
                let (qx, qy) = coords[next_idx];
                if flags[next_idx].on_curve {
                    sink.quad_to(px, py, qx, qy);
                    cur_x = qx;
                    cur_y = qy;
                    i = next_idx + 1;
                } else {
                    let mx = (px + qx) * 0.5;
                    let my = (py + qy) * 0.5;
                    sink.quad_to(px, py, mx, my);
                    cur_x = mx;
                    cur_y = my;
                    i = next_idx;
                }
            } else {
                // Off-curve is the last point in the walk. The
                // endpoint is the contour's start.
                sink.quad_to(px, py, start_x, start_y);
                cur_x = start_x;
                cur_y = start_y;
                i = next_idx;
            }
        }
    }

    // Emit an explicit LineTo back to the start when we didn't
    // already land there. Matches ttf-parser / HarfBuzz convention.
    if abs_f32(cur_x - start_x) > 1e-6 || abs_f32(cur_y - start_y) > 1e-6 {
        sink.line_to(start_x, start_y);
    }
    sink.close();
}

#[cfg(test)]
#[allow(clippy::vec_init_then_push, clippy::same_item_push)]
mod tests;
