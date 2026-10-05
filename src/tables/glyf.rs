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
//! Pass 1 follows HarfBuzz's `Glyph::get_points`: each glyph's points
//! land at the end of one running point list in the glyph's own frame,
//! followed by its four phantom points, and a composite then places
//! each component's points (phantom points included) in its own frame
//! before it drops them. The running list is what supports
//! `ARGS_ARE_XY_VALUES`-clear *anchor-mode* components: when the
//! component flag bit is clear, `arg2` is a point of the component
//! and `arg1` a point of the running list, which holds every point the
//! walk has placed so far, in whatever composite it was placed, then
//! the component's own points and its phantom points. The component
//! moves by `list[arg1] - component[arg2]`, so both sides must be
//! concrete before the child's ops can be emitted.
//!
//! The phantom points come from hmtx (and vmtx if present). The
//! `phantom_anchor_fixture_outlines_match_harfbuzz` integration test
//! covers an anchor past the points before the component
//! (hand-crafted ~1 KB fixture under `tests/fixtures/`).

mod composite;
mod simple;

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::gvar::{Gvar, MAX_TUPLE_WORK};
use crate::tables::hmtx::Hmtx;
use crate::tables::loca::Loca;
use crate::tables::outline::OutlineSink;
use crate::tables::parse::{abs_f32, Reader};
use crate::tables::vmtx::Vmtx;

use composite::read_components;
use simple::{flatten_simple_glyph, simple_point_count};

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
/// can match a component's phantom point, which follows the
/// component's points in the running point list of the walk (see the
/// module docs), and `gvar` moves the phantom points.
///
/// `vmtx` is optional. With `None` the vertical phantoms are both
/// `(0, 0)`. HarfBuzz instead gives a font without `vmtx` a top side
/// bearing of zero and an advance of an em, so pp3 is `(0, yMax)` and
/// pp4 is `(0, yMax - unitsPerEm)`; [`crate::Face`]'s outline and
/// bounds methods pass those metrics for such a font, so a component
/// anchored to a vertical phantom point lands where HarfBuzz puts it.
/// Real-world anchor-mode glyphs in horizontal fonts only ever index
/// pp1 / pp2.
#[derive(Debug, Clone, Copy)]
pub struct PhantomMetrics<'a> {
    /// Horizontal metrics. Required: every TrueType font has hmtx.
    pub hmtx: &'a Hmtx<'a>,
    /// Vertical metrics. `None` puts the vertical phantom points at the
    /// origin.
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

// Composite-glyph flag bits. ROUND_XY_TO_GRID (0x0004) and
// WE_HAVE_INSTRUCTIONS (0x0100) only matter to hinting, so the outline
// walk ignores them. USE_MY_METRICS (0x0200) only matters to the
// phantom points.
const COMP_ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
const COMP_ARGS_ARE_XY_VALUES: u16 = 0x0002;
const COMP_WE_HAVE_A_SCALE: u16 = 0x0008;
const COMP_MORE_COMPONENTS: u16 = 0x0020;
const COMP_WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
const COMP_WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
const COMP_USE_MY_METRICS: u16 = 0x0200;
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
    /// `gvar` tuple work left for the whole walk. Every glyph the walk
    /// visits decodes its own tuples, and a composite can visit the
    /// same glyph many times, so one cap ([`MAX_TUPLE_WORK`]) covers
    /// them all, as HarfBuzz shares one budget across `get_points`.
    work: usize,
}

impl FlattenBudget {
    const fn new() -> Self {
        Self {
            glyphs: MAX_FLATTEN_GLYPHS,
            points: MAX_FLATTEN_POINTS,
            work: MAX_TUPLE_WORK,
        }
    }

    /// Takes one glyph visit, or fails with `offset`, the byte offset in
    /// `glyf` of the glyph that would overspend the cap.
    fn take_glyph(&mut self, offset: usize) -> Result<()> {
        self.glyphs = self.glyphs.checked_sub(1).ok_or(Error::Malformed {
            offset,
            context: "glyf composite visits too many glyphs",
        })?;
        Ok(())
    }

    /// Takes `n` points, or fails with `offset`, the byte offset in
    /// `glyf` of the simple glyph whose points would overspend the cap.
    fn take_points(&mut self, n: usize, offset: usize) -> Result<()> {
        self.points = self.points.checked_sub(n).ok_or(Error::Malformed {
            offset,
            context: "glyf composite expands to too many points",
        })?;
        Ok(())
    }
}

/// A glyph's four phantom points, in its own frame, from its header's
/// `xMin` and `yMax` and its metrics (see [`PhantomMetrics`]); without
/// `vmtx` the vertical ones are `(0, 0)`. The face passes HarfBuzz's
/// metrics for a font without `vmtx` instead of `None` (see
/// [`Vmtx::missing`]).
fn phantom_points_from(
    glyph_id: u16,
    metrics: &PhantomMetrics<'_>,
    x_min: i16,
    y_max: i16,
) -> PhantomPoints {
    let advance_w = f32::from(metrics.hmtx.advance(glyph_id).unwrap_or(0));
    let lsb = f32::from(metrics.hmtx.lsb(glyph_id).unwrap_or(0));
    let pp1_x = f32::from(x_min) - lsb;
    let pp2_x = pp1_x + advance_w;

    let (pp3_y, pp4_y) = if let Some(vmtx) = metrics.vmtx {
        let advance_h = f32::from(vmtx.advance(glyph_id).unwrap_or(0));
        let tsb = f32::from(vmtx.tsb(glyph_id).unwrap_or(0));
        let pp3 = f32::from(y_max) + tsb;
        (pp3, pp3 - advance_h)
    } else {
        (0.0, 0.0)
    };

    [(pp1_x, 0.0), (pp2_x, 0.0), (0.0, pp3_y), (0.0, pp4_y)]
}

/// A glyph's four phantom points: left and right side bearing origins
/// in x, top and bottom origins in y.
type PhantomPoints = [(f32, f32); 4];

/// The byte offset in `glyf` where `loca` puts `glyph_id`, which errors
/// about the glyph report; 0, the start of the table, for a glyph id
/// past the end of `loca`.
fn glyph_offset(loca: &Loca<'_>, glyph_id: u16) -> usize {
    loca.range(glyph_id).map_or(0, |(start, _)| start as usize)
}

/// Rounds a float to the nearest `i16`, halves away from zero,
/// saturating at the type bounds: the whole-unit points
/// [`Glyf::glyph_points`] reports.
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
    /// glyphs and zero-length glyphs return `None`. A composite's gvar
    /// points are its components instead, one each, which
    /// [`Glyf::outline_at_coords`] handles.
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
                offset: glyph_offset(loca, glyph_id),
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
    /// when `metrics` has it, and are `(0, 0)` otherwise (see
    /// [`PhantomMetrics`]). Glyphs without a `glyf` body take their box
    /// as `(0, 0)`.
    fn phantom_points(
        &self,
        loca: &Loca<'_>,
        glyph_id: u16,
        metrics: &PhantomMetrics<'_>,
    ) -> Result<[(f32, f32); 4]> {
        let bounds = self.bounds(loca, glyph_id)?;
        let (x_min, y_max) = match bounds {
            Some(b) => (b.x_min, b.y_max),
            None => (0, 0),
        };
        Ok(phantom_points_from(glyph_id, metrics, x_min, y_max))
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
    /// `vmtx` is optional: without it the vertical phantoms are `(0, 0)`
    /// (see [`PhantomMetrics`]), for the glyph and for the components
    /// anchored to them.
    ///
    /// Returns `Ok(None)` when the glyph id is out of range or has no
    /// outline body. Coordinates are rounded to the nearest `i16`,
    /// halves away from zero, as kerx format 4 action type 0 reads
    /// whole font units. Only a scaled or transformed component makes
    /// a point fractional. (The variation code rounds halves up, as
    /// HarfBuzz does. HarfBuzz's own font functions give kerx no
    /// contour points, so it has no rule to follow here.)
    pub fn glyph_points(
        &self,
        loca: &Loca<'_>,
        glyph_id: u16,
        hmtx: &Hmtx<'_>,
        vmtx: Option<&Vmtx<'_>>,
    ) -> Result<Option<Vec<(i16, i16)>>> {
        let metrics = PhantomMetrics { hmtx, vmtx };
        let cx = FlattenCtx {
            loca,
            metrics: Some(&metrics),
            var: None,
        };
        let Some(flat) = self.flatten_root(&cx, glyph_id, None, &mut FlattenBudget::new())? else {
            return Ok(None);
        };
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
    /// of `(dx, dy)` pairs in the glyph's point order, added to the
    /// points of a simple glyph before it is drawn; components of a
    /// composite glyph never get them. Pass `None` for the coord-free
    /// path. To draw a variable font at given coords, use
    /// [`Glyf::outline_at_coords`], which also varies components.
    ///
    /// `metrics` supplies `hmtx` (and optionally `vmtx`) for the four
    /// phantom points (LSB origin, advance-width origin, TSB origin,
    /// advance-height origin) an anchor-mode component can match (see
    /// the module docs). Passing `None` leaves a component anchored to
    /// a phantom point where its offset puts it, useful for unit tests
    /// of synthetic composites that don't ship metrics.
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
        let cx = FlattenCtx {
            loca,
            metrics,
            var: None,
        };
        self.outline_with(&cx, glyph_id, deltas, sink)
    }

    /// Drives `sink` with the ops for `glyph_id` at the normalized
    /// variation `coords`, with the glyph's `gvar` deltas applied the
    /// way HarfBuzz applies them:
    ///
    /// - A simple glyph's points move by its deltas, with the points a
    ///   tuple skips inferred from the points it lists (see
    ///   [`Gvar::glyph_point_deltas`]).
    /// - A composite glyph's deltas move its components, one delta per
    ///   component, added to the component's x and y offset before the
    ///   offset goes through the component's scale when
    ///   `SCALED_COMPONENT_OFFSET` asks for that. The offset is not
    ///   rounded. Each component is drawn at the same coords. A
    ///   component placed by matching points ignores its delta: the
    ///   points it matches already moved.
    /// - Anchor points that name a phantom point use the phantom
    ///   point moved by its delta.
    ///
    /// With no `gvar`, or with coords that are all zero (the default
    /// instance), this draws the same outline as
    /// [`Glyf::outline`] with no deltas. `metrics` works as in
    /// [`Glyf::outline`].
    ///
    /// HarfBuzz also shifts the outline left by the x of phantom
    /// point 1 (the left side bearing origin, see
    /// [`Glyf::phantom_points_at_coords`]). That x is zero unless the
    /// `hmtx` side bearing differs from the glyph's `xMin` or the
    /// point varies; this method does not shift.
    ///
    /// Returns `Ok(false)` when the glyph id is valid but has no
    /// outline data (whitespace glyph), `Ok(true)` otherwise.
    ///
    /// # Errors
    ///
    /// Returns an error when the glyph's `glyf` data or its `gvar`
    /// data is malformed.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::tables::Outline;
    /// use sigilbuzz::{Blob, Face};
    ///
    /// // A subset of Hahmlet, whose `wght` axis runs from 100 to 900.
    /// let data = include_bytes!("../../tests/fixtures/hahmlet_gvar_subset.ttf");
    /// let blob = Blob::new(data);
    /// let face = Face::parse(&blob, 0)?;
    /// let (loca, glyf, gvar) = (face.loca()?, face.glyf()?, face.gvar()?);
    /// let o = 4; // `O`
    /// let mut black = Outline::new();
    /// glyf.outline_at_coords(&loca, o, gvar.as_ref(), &[1.0], None, &mut black)?;
    /// let mut regular = Outline::new();
    /// glyf.outline(&loca, o, None, None, &mut regular)?;
    /// assert_eq!(black.len(), regular.len());
    /// assert_ne!(black, regular);
    /// # Ok::<(), sigilbuzz::Error>(())
    /// ```
    pub fn outline_at_coords<S: OutlineSink>(
        &self,
        loca: &Loca<'_>,
        glyph_id: u16,
        gvar: Option<&Gvar<'_>>,
        coords: &[f32],
        metrics: Option<&PhantomMetrics<'_>>,
        sink: &mut S,
    ) -> Result<bool> {
        let cx = FlattenCtx {
            loca,
            metrics,
            var: Variation::new(gvar, coords),
        };
        self.outline_with(&cx, glyph_id, None, sink)
    }

    /// [`Glyf::outline_at_coords`] for a caller that draws many glyphs
    /// for one result and bounds them together: the walk takes its
    /// `gvar` tuple work from `work` (see [`MAX_TUPLE_WORK`]) and leaves
    /// the rest there, and returns, with whether it drew, the glyph
    /// records it visited and the points it laid down, which its own
    /// caps bound. Draws into `sink` only once the whole glyph is
    /// flattened, so a glyph that fails sends no ops.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn outline_at_coords_with_work<S: OutlineSink>(
        &self,
        loca: &Loca<'_>,
        glyph_id: u16,
        gvar: Option<&Gvar<'_>>,
        coords: &[f32],
        metrics: Option<&PhantomMetrics<'_>>,
        sink: &mut S,
        work: &mut usize,
    ) -> Result<(bool, usize, usize)> {
        let cx = FlattenCtx {
            loca,
            metrics,
            var: Variation::new(gvar, coords),
        };
        let mut budget = FlattenBudget::new();
        budget.work = (*work).min(MAX_TUPLE_WORK);
        let flat = self.flatten_root(&cx, glyph_id, None, &mut budget);
        *work = budget.work;
        let visits = (MAX_FLATTEN_GLYPHS - budget.glyphs) as usize;
        let points = MAX_FLATTEN_POINTS - budget.points;
        let Some(flat) = flat? else {
            return Ok((false, visits, points));
        };
        flat.emit(sink);
        Ok((true, visits, points))
    }

    /// The points [`Glyf::outline_at_coords`] draws `glyph_id` from,
    /// contour points on and off the curve, handed to `visit` without
    /// drawing the outline: what a box of the outline needs, since the
    /// points the drawing adds between two off-curve points lie between
    /// them. Returns `Ok(false)` for a glyph without outline data.
    pub(crate) fn points_at_coords(
        &self,
        loca: &Loca<'_>,
        glyph_id: u16,
        gvar: Option<&Gvar<'_>>,
        coords: &[f32],
        metrics: Option<&PhantomMetrics<'_>>,
        mut visit: impl FnMut(f32, f32),
    ) -> Result<bool> {
        let cx = FlattenCtx {
            loca,
            metrics,
            var: Variation::new(gvar, coords),
        };
        let Some(flat) = self.flatten_root(&cx, glyph_id, None, &mut FlattenBudget::new())? else {
            return Ok(false);
        };
        for c in &flat.contours {
            for &(x, y) in &flat.points[c.start..=c.end] {
                visit(x, y);
            }
        }
        Ok(true)
    }

    /// Returns the four phantom points of `glyph_id` (left side
    /// bearing origin, advance origin, top origin, bottom origin) at
    /// the normalized variation `coords`, in the glyph's own frame. The
    /// parameters run in the order of [`Glyf::outline_at_coords`].
    ///
    /// The default points come from `metrics` (`hmtx` and, when
    /// present, `vmtx`), as for composite anchors. `gvar` then moves
    /// them by the glyph's phantom deltas. A composite glyph takes the
    /// phantom points of its last component flagged `USE_MY_METRICS`,
    /// at the same coords, as HarfBuzz does, at the default instance
    /// too. With no `gvar`, or coords that are all zero, nothing moves.
    ///
    /// A `USE_MY_METRICS` component that leads back to a composite
    /// being walked is skipped where HarfBuzz's cycle detector skips
    /// it, and the glyph there keeps its own points.
    ///
    /// Without `HVAR`, HarfBuzz takes a varied glyph's advance from
    /// these points: the x distance from the first to the second,
    /// rounded and at least zero.
    ///
    /// # Errors
    ///
    /// Returns an error when the glyph's `glyf`, `gvar`, or metrics
    /// data is malformed, composite glyphs nest too deep, or the walk
    /// runs over its work budget.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::tables::glyf::PhantomMetrics;
    /// use sigilbuzz::{Blob, Face};
    ///
    /// // A subset of Hahmlet, whose `wght` axis runs from 100 to 900.
    /// let data = include_bytes!("../../tests/fixtures/hahmlet_gvar_subset.ttf");
    /// let blob = Blob::new(data);
    /// let face = Face::parse(&blob, 0)?;
    /// let (loca, glyf, gvar, hmtx) = (face.loca()?, face.glyf()?, face.gvar()?, face.hmtx()?);
    /// let metrics = PhantomMetrics { hmtx: &hmtx, vmtx: None };
    /// let space = 9;
    /// // The space advances 248 units at the default weight, 265 at 900.
    /// let pp = glyf.phantom_points_at_coords(&loca, space, gvar.as_ref(), &[1.0], &metrics)?;
    /// assert_eq!((pp[1].0 - pp[0].0).round(), 265.0);
    /// # Ok::<(), sigilbuzz::Error>(())
    /// ```
    pub fn phantom_points_at_coords(
        &self,
        loca: &Loca<'_>,
        glyph_id: u16,
        gvar: Option<&Gvar<'_>>,
        coords: &[f32],
        metrics: &PhantomMetrics<'_>,
    ) -> Result<[(f32, f32); 4]> {
        let cx = FlattenCtx {
            loca,
            metrics: Some(metrics),
            var: Variation::new(gvar, coords),
        };
        self.varied_phantoms(&cx, metrics, glyph_id, 0, &mut FlattenBudget::new())
    }

    /// Shared body of [`Glyf::outline`] and [`Glyf::outline_at_coords`].
    fn outline_with<S: OutlineSink>(
        &self,
        cx: &FlattenCtx<'_>,
        glyph_id: u16,
        deltas: Option<&[(f32, f32)]>,
        sink: &mut S,
    ) -> Result<bool> {
        // Two-pass flattening: pass 1 walks the glyph (and any
        // composite children) into a flat point list with absolute
        // coordinates; pass 2 emits ops contour by contour. The
        // intermediate point list is what lets composite components
        // resolve `ARGS_ARE_XY_VALUES`-clear anchor-point matching,
        // which reads points already placed.
        let Some(flat) = self.flatten_root(cx, glyph_id, deltas, &mut FlattenBudget::new())? else {
            return Ok(false);
        };
        flat.emit(sink);
        Ok(true)
    }

    /// Phantom points of `glyph_id` with its gvar deltas applied when
    /// `cx` carries variations, and a composite's taken from its
    /// `USE_MY_METRICS` component either way: HarfBuzz's phantom-only
    /// `get_points`.
    /// `depth` and `budget` bound the walk through `USE_MY_METRICS`
    /// components as they bound [`Glyf::flatten`].
    fn varied_phantoms(
        &self,
        cx: &FlattenCtx<'_>,
        metrics: &PhantomMetrics<'_>,
        glyph_id: u16,
        depth: u8,
        budget: &mut FlattenBudget,
    ) -> Result<[(f32, f32); 4]> {
        self.phantom_walk(cx, metrics, glyph_id, depth, budget, &mut Vec::new())
    }

    /// [`Glyf::varied_phantoms`] inside the composites of `path`, which
    /// holds, for each composite on the way down, the component it is
    /// visiting.
    ///
    /// A component that would close a cycle is skipped, as HarfBuzz's
    /// decycler (`hb-decycler.hh`) skips it: the composite at `path`
    /// index `i` compares each component with the one the composite at
    /// index `i / 2` is visiting, a tortoise that moves at half the
    /// speed of the walk. A cycle is caught within twice its length,
    /// and the glyph where it closes keeps its own phantom points.
    // The walk threads its tables, limits, and path through every
    // level of the recursion.
    #[allow(clippy::too_many_arguments)]
    fn phantom_walk(
        &self,
        cx: &FlattenCtx<'_>,
        metrics: &PhantomMetrics<'_>,
        glyph_id: u16,
        depth: u8,
        budget: &mut FlattenBudget,
        path: &mut Vec<u16>,
    ) -> Result<[(f32, f32); 4]> {
        let offset = glyph_offset(cx.loca, glyph_id);
        if depth > MAX_COMPOSITE_DEPTH {
            return Err(Error::Malformed {
                offset,
                context: "glyf composite recursion exceeded cap",
            });
        }
        budget.take_glyph(offset)?;
        let mut pp = self.phantom_points(cx.loca, glyph_id, metrics)?;
        // The glyph's own gvar points come first: contour points for a
        // simple glyph, components for a composite, none when empty.
        let mut components = Vec::new();
        let own_points = match self.glyph_bytes(cx.loca, glyph_id)? {
            Some(body) if body.len() >= 10 => {
                let mut r = Reader::new(body);
                let num_contours = r.read_i16()?;
                r.skip(8)?; // bbox
                if num_contours >= 0 {
                    simple_point_count(&mut r, num_contours as u16)?
                } else {
                    components = read_components(&mut r)?;
                    components.len()
                }
            }
            _ => 0,
        };
        // gvar moves the points away from the default instance. The
        // USE_MY_METRICS components below apply at every instance, as in
        // HarfBuzz, so the points stay continuous as the coords reach
        // zero.
        if let Some(var) = cx.var {
            let work = &mut budget.work;
            let deltas = var
                .gvar
                .phantom_deltas(glyph_id, var.coords, own_points, work)?;
            for (p, d) in pp.iter_mut().zip(deltas) {
                p.0 += d.0;
                p.1 += d.1;
            }
        }
        if components.is_empty() {
            return Ok(pp);
        }
        let node = path.len();
        path.push(glyph_id);
        for c in components
            .iter()
            .filter(|c| c.flags & COMP_USE_MY_METRICS != 0)
        {
            path[node] = c.glyph_id;
            if node > 0 && path[node / 2] == c.glyph_id {
                continue;
            }
            pp = self.phantom_walk(cx, metrics, c.glyph_id, depth + 1, budget, path)?;
        }
        path.truncate(node);
        Ok(pp)
    }

    /// Walks `glyph_id` into a [`FlatGlyph`] of its contour points,
    /// without its phantom points, or `None` for a glyph without outline
    /// data. See [`Glyf::flatten`].
    fn flatten_root(
        &self,
        cx: &FlattenCtx<'_>,
        glyph_id: u16,
        deltas: Option<&[(f32, f32)]>,
        budget: &mut FlattenBudget,
    ) -> Result<Option<FlatGlyph>> {
        let mut flat = FlatGlyph::default();
        let drew = self.flatten(cx, glyph_id, deltas, &mut flat, 0, budget, &mut Vec::new())?;
        flat.pop_phantoms();
        Ok(drew.then_some(flat))
    }

    /// Appends `glyph_id`'s points to `out`, in the glyph's own frame,
    /// and then its four phantom points, as HarfBuzz's
    /// `Glyph::get_points` appends them to its running point list. A
    /// composite places its components' points, phantom points
    /// included, before it returns (see [`Glyf::flatten_composite`]),
    /// so the points it leaves are in its own frame too. Returns
    /// `Ok(false)` for an empty or out-of-range glyph, which still
    /// leaves its phantom points. Recurses through composite
    /// components, with `depth` capped by [`MAX_COMPOSITE_DEPTH`] and
    /// the total work capped by `budget`.
    ///
    /// `deltas` are dense per-point deltas for a simple root glyph
    /// ([`Glyf::outline`]); variations in `cx` take their place. `path`
    /// holds the components the composites on the way down are
    /// visiting, for the cycle check of [`Glyf::phantom_walk`].
    // The walk threads its tables, output, and limits through every
    // level of the recursion.
    #[allow(clippy::too_many_arguments)]
    fn flatten(
        &self,
        cx: &FlattenCtx<'_>,
        glyph_id: u16,
        deltas: Option<&[(f32, f32)]>,
        out: &mut FlatGlyph,
        depth: u8,
        budget: &mut FlattenBudget,
        path: &mut Vec<u16>,
    ) -> Result<bool> {
        let offset = glyph_offset(cx.loca, glyph_id);
        if depth > MAX_COMPOSITE_DEPTH {
            return Err(Error::Malformed {
                offset,
                context: "glyf composite recursion exceeded cap",
            });
        }
        budget.take_glyph(offset)?;
        // HarfBuzz reads a glyph shorter than its header as empty.
        let Some(body) = self
            .glyph_bytes(cx.loca, glyph_id)?
            .filter(|b| b.len() >= 10)
        else {
            let deltas = match cx.var {
                Some(var) => var
                    .gvar
                    .phantom_deltas(glyph_id, var.coords, 0, &mut budget.work)?,
                None => [(0.0, 0.0); 4],
            };
            out.push_phantoms(cx.phantoms(glyph_id, (0, 0), &deltas));
            return Ok(false);
        };
        let mut r = Reader::new(body);
        let num_contours = r.read_i16()?;
        let x_min = r.read_i16()?;
        r.skip(4)?; // yMin, xMax
        let y_max = r.read_i16()?;
        if num_contours >= 0 {
            let var = cx.var.map(|v| (v, glyph_id));
            let glyph = (r, offset);
            let deltas =
                flatten_simple_glyph(glyph, num_contours as u16, deltas, var, out, budget)?;
            out.push_phantoms(cx.phantoms(glyph_id, (x_min, y_max), &deltas));
        } else {
            let glyph = (&mut r, glyph_id, (x_min, y_max));
            self.flatten_composite(glyph, cx, out, depth, budget, path)?;
        }
        Ok(true)
    }
}
/// Variation inputs for one outline walk: the `gvar` table and the
/// normalized coords to evaluate it at.
#[derive(Debug, Clone, Copy)]
struct Variation<'v> {
    gvar: &'v Gvar<'v>,
    coords: &'v [f32],
}

impl<'v> Variation<'v> {
    /// `None` when there is nothing to vary: no `gvar`, or the default
    /// instance (every coord zero), where HarfBuzz skips `gvar` too.
    fn new(gvar: Option<&'v Gvar<'v>>, coords: &'v [f32]) -> Option<Self> {
        let gvar = gvar?;
        coords
            .iter()
            .any(|&c| c != 0.0)
            .then_some(Self { gvar, coords })
    }
}

/// Tables shared by every level of one outline walk.
struct FlattenCtx<'c> {
    loca: &'c Loca<'c>,
    metrics: Option<&'c PhantomMetrics<'c>>,
    var: Option<Variation<'c>>,
}

impl FlattenCtx<'_> {
    /// The phantom points of `glyph_id`, whose header holds `xMin` and
    /// `yMax` in `header`, moved by `deltas`. Without metrics they are
    /// the origin, moved; anchors do not read them then.
    fn phantoms(&self, glyph_id: u16, header: (i16, i16), deltas: &PhantomPoints) -> PhantomPoints {
        let mut pp = self.metrics.map_or([(0.0, 0.0); 4], |m| {
            phantom_points_from(glyph_id, m, header.0, header.1)
        });
        for (p, d) in pp.iter_mut().zip(deltas) {
            p.0 += d.0;
            p.1 += d.1;
        }
        pp
    }
}

/// A composite component's 2x2 matrix: `x' = xx * x + xy * y`,
/// `y' = yx * x + yy * y`.
#[derive(Debug, Clone, Copy)]
struct Transform {
    xx: f32,
    xy: f32,
    yx: f32,
    yy: f32,
}

impl Transform {
    const fn identity() -> Self {
        Self {
            xx: 1.0,
            xy: 0.0,
            yx: 0.0,
            yy: 1.0,
        }
    }

    fn is_identity(&self) -> bool {
        self.xx == 1.0 && self.xy == 0.0 && self.yx == 0.0 && self.yy == 1.0
    }

    /// Applies the matrix with HarfBuzz's operations in HarfBuzz's
    /// order (`contour_point_t::transform`), so the floats agree.
    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (x * self.xx + y * self.xy, x * self.yx + y * self.yy)
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
    /// Appends four phantom points, which no contour holds.
    fn push_phantoms(&mut self, phantoms: PhantomPoints) {
        self.points.extend_from_slice(&phantoms);
        self.flags
            .extend_from_slice(&[FlatPoint { on_curve: false }; 4]);
    }

    /// Drops the four phantom points [`FlatGlyph::push_phantoms`] put
    /// last.
    fn pop_phantoms(&mut self) {
        let len = self.points.len().saturating_sub(4);
        self.points.truncate(len);
        self.flags.truncate(len);
    }

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
