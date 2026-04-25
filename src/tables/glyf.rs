// Parser-style code leans on bespoke index loops / byte-by-byte
// walks; the pedantic range-loop lints add noise without clarifying
// the spec-mirroring layout.
#![allow(
    clippy::bool_to_int_with_if,
    clippy::elidable_lifetime_names,
    clippy::map_unwrap_or,
    clippy::too_many_lines,
    clippy::too_many_arguments,
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
//!
//! # Composite flattening: two passes
//!
//! Outline emission is split into two passes. Pass 1 walks the glyph
//! and any composite children into a flat point list with absolute
//! coordinates (gvar deltas + 2×2 + translation already folded in).
//! Pass 2 walks the contour list and dispatches to the caller's
//! [`OutlineSink`].
//!
//! The intermediate point list is what supports
//! `ARGS_ARE_XY_VALUES`-clear *anchor-mode* components: when the
//! component flag bit is clear, `arg1` and `arg2` are point indices
//! into the parent's already-flattened points and the child's own
//! flattened points respectively. The translation is implied —
//! `parent[arg1] - child[arg2]` — so we need both sides as concrete
//! coordinates before we can emit the child's ops.
//!
//! Phantom-point references are resolved against hmtx (and vmtx if
//! present) at flatten time. The phantom-anchor branch is covered by
//! the `phantom_anchor_fixture_outlines_match_ttf_parser` integration
//! test (hand-crafted ~1 KB fixture under `tests/fixtures/`).

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::hmtx::Hmtx;
use crate::tables::loca::Loca;
use crate::tables::outline::OutlineSink;
use crate::tables::parse::Reader;
use crate::tables::vmtx::Vmtx;

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
/// - `pp1 = (xMin - lsb, 0)` — left-side-bearing origin.
/// - `pp2 = (xMin - lsb + advanceWidth, 0)` — advance-width origin.
/// - `pp3 = (0, yMax + tsb)` — top-side-bearing origin.
/// - `pp4 = (0, yMax + tsb - advanceHeight)` — advance-height origin.
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
    /// Horizontal metrics. Required — every TrueType font has hmtx.
    pub hmtx: &'a Hmtx<'a>,
    /// Vertical metrics. `None` for horizontal-only fonts.
    pub vmtx: Option<&'a Vmtx<'a>>,
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

    /// Computes the four phantom points for `glyph_id` in the glyph's
    /// own (untransformed) design-unit frame.
    ///
    /// pp1 / pp2 always read from `hmtx`. pp3 / pp4 read from `vmtx`
    /// when available; horizontal-only fonts get `(0, 0)` for both,
    /// which matches every in-the-wild glyph we've checked — anchor
    /// indices for vertical phantoms only show up in CJK fonts that
    /// also ship `vmtx`. Glyphs without a `glyf` body get all-zero
    /// phantoms, which collapses anchor mode to a zero translation
    /// — same as the legacy fallback before phantom resolution
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

    /// Drives `sink` with the ops for `glyph_id`, flattening
    /// composite glyphs recursively. `deltas` is an optional list
    /// of `(dx, dy)` pairs in the glyph's point order — supply the
    /// output of [`crate::tables::Gvar::glyph_deltas`] folded into a
    /// dense `[f32; num_points]` pair to apply variable-font
    /// deltas. Pass `None` for the coord-free path.
    ///
    /// `metrics` supplies `hmtx` (and optionally `vmtx`) so anchor-mode
    /// composites whose anchor index points past the parent's contour
    /// points can resolve against the four phantom points (LSB origin,
    /// advance-width origin, TSB origin, advance-height origin).
    /// Passing `None` keeps the legacy zero-translation fallback for
    /// the rare phantom case — useful for unit tests of synthetic
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
        // Two-pass flattening: phase 1 walks the glyph (and any
        // composite children) into a flat point list with absolute
        // coordinates; phase 2 emits ops contour by contour. The
        // intermediate point list is what lets composite components
        // resolve `ARGS_ARE_XY_VALUES`-clear anchor-point matching:
        // arg1 indexes into the parent's already-flattened points
        // and arg2 into the freshly-flattened child, so we need both
        // sets of concrete coordinates before we know the child's
        // translation.
        let mut flat = FlatGlyph::default();
        let identity = Transform::identity();
        let drew = self.flatten(loca, glyph_id, deltas, metrics, &identity, &mut flat, 0)?;
        if !drew {
            return Ok(false);
        }
        flat.emit(sink);
        Ok(true)
    }

    /// Flattens `glyph_id` (transformed by `tf`) into `out`. Returns
    /// `Ok(false)` for empty / out-of-range glyphs. Recurses through
    /// composite components, with `depth` capped by
    /// [`MAX_COMPOSITE_DEPTH`].
    fn flatten(
        &self,
        loca: &Loca<'_>,
        glyph_id: u16,
        deltas: Option<&[(f32, f32)]>,
        metrics: Option<&PhantomMetrics<'_>>,
        tf: &Transform,
        out: &mut FlatGlyph,
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
            flatten_simple_glyph(&mut r, num_contours as u16, deltas, tf, out)?;
        } else {
            self.flatten_composite(&mut r, loca, glyph_id, metrics, tf, out, depth)?;
        }
        Ok(true)
    }

    fn flatten_composite(
        &self,
        r: &mut Reader<'_>,
        loca: &Loca<'_>,
        parent_glyph_id: u16,
        metrics: Option<&PhantomMetrics<'_>>,
        parent_tf: &Transform,
        out: &mut FlatGlyph,
        depth: u8,
    ) -> Result<()> {
        loop {
            let flags = r.read_u16()?;
            let component_id = r.read_u16()?;

            // Arg width is flag-driven. We read the raw values first
            // and decide later whether they are xy offsets or anchor
            // point indices.
            let (raw_a, raw_b): (i32, i32) = if flags & COMP_ARG_1_AND_2_ARE_WORDS != 0 {
                let a = r.read_i16()?;
                let b = r.read_i16()?;
                (i32::from(a), i32::from(b))
            } else {
                // Anchor mode uses unsigned point indices when args
                // are not WORDS; xy mode uses signed bytes. The
                // distinction is the ARGS_ARE_XY_VALUES flag.
                if flags & COMP_ARGS_ARE_XY_VALUES != 0 {
                    let a = r.read_i8()?;
                    let b = r.read_i8()?;
                    (i32::from(a), i32::from(b))
                } else {
                    let a = r.read_u8()?;
                    let b = r.read_u8()?;
                    (i32::from(a), i32::from(b))
                }
            };

            // OpenType stores the 2x2 in column-major order
            // (xscale, scale01, scale10, yscale) where the resulting
            // transform is:
            //   x' = xscale * x + scale10 * y
            //   y' = scale01 * x + yscale * y
            // [`Transform`]'s `apply` is `xx*x + xy*y, yx*x + yy*y`,
            // so `xy` receives `scale10` and `yx` receives `scale01`.
            // Crossing those wires is invisible for symmetric scales
            // (the only kind exercised by Open Sans + most of Amiri)
            // but flips the axes for shear / rotation matrices.
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
                yx = r.read_f2dot14()?; // scale01 — y' coefficient on x
                xy = r.read_f2dot14()?; // scale10 — x' coefficient on y
                yy = r.read_f2dot14()?;
            }

            // Snapshot the parent's point count *before* this
            // component is laid down. Anchor-mode arg1 indexes into
            // exactly those points (the parent contour points already
            // emitted by previous siblings, transformed into the
            // composite's frame).
            let parent_point_count = out.points.len();

            // First flatten the child into a scratch buffer with the
            // 2x2 applied but no translation yet — both anchor-mode
            // and xy-mode branches need access to the child's
            // pre-translation absolute points.
            let child_local = Transform {
                xx,
                xy,
                yx,
                yy,
                tx: 0.0,
                ty: 0.0,
            };
            let child_combined = parent_tf.compose(&child_local);
            let mut child_flat = FlatGlyph::default();
            self.flatten(
                loca,
                component_id,
                None,
                metrics,
                &child_combined,
                &mut child_flat,
                depth + 1,
            )?;

            // Resolve the translation. Anchor-mode (ARGS_ARE_XY_VALUES
            // clear) computes `parent[arg1] - child[arg2]` so the
            // child's anchor point lands on the parent's. Otherwise
            // the args are signed offsets and SCALED_COMPONENT_OFFSET
            // optionally pre-multiplies them through the 2x2.
            let (tx, ty) = if flags & COMP_ARGS_ARE_XY_VALUES != 0 {
                let dx = raw_a as f32;
                let dy = raw_b as f32;
                let (lx, ly) = if flags & COMP_SCALED_COMPONENT_OFFSET != 0
                    && flags & COMP_UNSCALED_COMPONENT_OFFSET == 0
                {
                    (xx * dx + xy * dy, yx * dx + yy * dy)
                } else {
                    (dx, dy)
                };
                // The translation lives in the parent's coordinate
                // frame, so route it through the parent's transform
                // (rotation + scale + translation) before applying
                // it on top of the already-transformed child points.
                let tx = parent_tf.xx * lx + parent_tf.xy * ly;
                let ty = parent_tf.yx * lx + parent_tf.yy * ly;
                (tx, ty)
            } else {
                let p_idx = raw_a as usize;
                let c_idx = raw_b as usize;
                let parent_anchor = resolve_anchor_point(
                    p_idx,
                    parent_point_count,
                    &out.points,
                    || -> Result<Option<(f32, f32)>> {
                        let Some(m) = metrics else { return Ok(None) };
                        let pp = self.phantom_points(loca, parent_glyph_id, m)?;
                        let phantom_idx = p_idx - parent_point_count;
                        if phantom_idx >= 4 {
                            return Ok(None);
                        }
                        let (px, py) = pp[phantom_idx];
                        // Parent's phantoms live in the parent's frame
                        // — same frame as the points already in
                        // `out.points`, which were transformed by
                        // `parent_tf` on insertion. Apply the same
                        // transform so the subtraction below cancels
                        // out cleanly.
                        Ok(Some(parent_tf.apply(px, py)))
                    },
                )?;
                let child_anchor = resolve_anchor_point(
                    c_idx,
                    child_flat.points.len(),
                    &child_flat.points,
                    || -> Result<Option<(f32, f32)>> {
                        let Some(m) = metrics else { return Ok(None) };
                        let pp = self.phantom_points(loca, component_id, m)?;
                        let phantom_idx = c_idx - child_flat.points.len();
                        if phantom_idx >= 4 {
                            return Ok(None);
                        }
                        let (cx, cy) = pp[phantom_idx];
                        // Child's phantoms share the frame of the
                        // freshly-flattened child points, which had
                        // `child_combined` baked in.
                        Ok(Some(child_combined.apply(cx, cy)))
                    },
                )?;
                match (parent_anchor, child_anchor) {
                    (Some((px, py)), Some((cx, cy))) => (px - cx, py - cy),
                    // Out-of-range phantom index, or no metrics passed
                    // through. Match the historic behaviour of skipping
                    // the translation rather than refusing to draw.
                    _ => (0.0, 0.0),
                }
            };

            // Splice the child into the parent. Contour ends shift by
            // the parent's running point count; coordinates shift by
            // the resolved translation; flags follow each point.
            let point_offset = out.points.len();
            debug_assert_eq!(child_flat.points.len(), child_flat.flags.len());
            for (i, &(cx, cy)) in child_flat.points.iter().enumerate() {
                out.points.push((cx + tx, cy + ty));
                out.flags.push(child_flat.flags[i]);
            }
            for c in &child_flat.contours {
                out.contours.push(Contour {
                    start: c.start + point_offset,
                    end: c.end + point_offset,
                });
            }

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

/// Two-pass flatten target. Phase 1 of [`Glyf::outline`] fills this
/// with absolute coordinates (deltas + composite transforms already
/// folded in); phase 2 walks `contours` and dispatches to the
/// caller's [`OutlineSink`]. Composite anchor-mode resolution reaches
/// into `points` to compute the parent ↔ child anchor pair, which is
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

/// Resolves an anchor-point index to a concrete `(x, y)` pair.
/// Indices below `real_point_count` index into `points`; indices at
/// or above that boundary are phantom-point references and route
/// through `phantom`, which is invoked lazily so non-anchor-mode
/// components pay nothing.
fn resolve_anchor_point<F>(
    idx: usize,
    real_point_count: usize,
    points: &[(f32, f32)],
    phantom: F,
) -> Result<Option<(f32, f32)>>
where
    F: FnOnce() -> Result<Option<(f32, f32)>>,
{
    if idx < real_point_count {
        return Ok(Some(points[idx]));
    }
    phantom()
}

fn flatten_simple_glyph(
    r: &mut Reader<'_>,
    num_contours: u16,
    deltas: Option<&[(f32, f32)]>,
    tf: &Transform,
    out: &mut FlatGlyph,
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

    // Materialise absolute, transformed points with optional deltas.
    // Deltas live in design-unit space and apply *before* the
    // composite transform — gvar feeds them into the simple-glyph
    // coord stream, so they share the glyph's own frame.
    let base_idx = out.points.len();
    for (i, &f) in flags.iter().enumerate() {
        let mut x = xs[i] as f32;
        let mut y = ys[i] as f32;
        if let Some(ds) = deltas {
            if let Some(&(dx, dy)) = ds.get(i) {
                x += dx;
                y += dy;
            }
        }
        let (tx, ty) = tf.apply(x, y);
        out.points.push((tx, ty));
        out.flags.push(FlatPoint {
            on_curve: f & FLAG_ON_CURVE != 0,
        });
    }

    // Per-contour ends, rebased onto the running point count.
    let mut start: usize = 0;
    for &end in &end_pts {
        let end_idx = end as usize;
        if end_idx >= xs.len() || end_idx < start {
            return Err(Error::Malformed {
                offset: 0,
                context: "glyf endPtsOfContours out of range",
            });
        }
        out.contours.push(Contour {
            start: base_idx + start,
            end: base_idx + end_idx,
        });
        start = end_idx + 1;
    }
    Ok(())
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
    if (cur_x - start_x).abs() > 1e-6 || (cur_y - start_y).abs() > 1e-6 {
        sink.line_to(start_x, start_y);
    }
    sink.close();
}

#[cfg(test)]
#[allow(
    clippy::vec_init_then_push,
    clippy::cast_possible_wrap,
    clippy::same_item_push
)]
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

    /// Builds a minimal `hmtx` body with one long metric per glyph.
    fn build_hmtx(longs: &[(u16, i16)]) -> Vec<u8> {
        let mut b = Vec::new();
        for (adv, lsb) in longs {
            b.extend_from_slice(&adv.to_be_bytes());
            b.extend_from_slice(&lsb.to_be_bytes());
        }
        b
    }

    #[test]
    fn phantom_points_match_spec_formula() {
        // Single simple glyph with bbox (xMin=10, yMax=200) plus an
        // hmtx record (advance=300, lsb=4). Expected phantoms:
        //   pp1 = (xMin - lsb, 0)             = (6,   0)
        //   pp2 = (pp1 + advance, 0)          = (306, 0)
        //   pp3 = (0, 0)   — no vmtx
        //   pp4 = (0, 0)   — no vmtx
        let body = build_simple_glyph(
            &[0],
            &[(10, 0, true)], // single contour point at (10, 0)
        );
        // Patch the bbox bytes to set yMax=200 explicitly (build_header
        // wrote yMax=1000 by default; we want a known number).
        let mut body = body;
        body[2..4].copy_from_slice(&10i16.to_be_bytes()); // xMin
        body[8..10].copy_from_slice(&200i16.to_be_bytes()); // yMax

        let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&body);

        let hmtx_bytes = build_hmtx(&[(300, 4)]);
        let hmtx = Hmtx::parse(&hmtx_bytes, 1, 1).unwrap();
        let metrics = PhantomMetrics {
            hmtx: &hmtx,
            vmtx: None,
        };
        let pp = glyf.phantom_points(&loca, 0, &metrics).unwrap();
        assert!((pp[0].0 - 6.0).abs() < 1e-4);
        assert!((pp[0].1 - 0.0).abs() < 1e-4);
        assert!((pp[1].0 - 306.0).abs() < 1e-4);
        assert!((pp[1].1 - 0.0).abs() < 1e-4);
        assert!((pp[2].0 - 0.0).abs() < 1e-4);
        assert!((pp[2].1 - 0.0).abs() < 1e-4);
        assert!((pp[3].0 - 0.0).abs() < 1e-4);
        assert!((pp[3].1 - 0.0).abs() < 1e-4);
    }

    #[test]
    fn phantom_points_use_vmtx_when_present() {
        // Same glyph, this time with vmtx supplying advance=1000,
        // tsb=50. yMax=200 → pp3 = (0, 250); pp4 = (0, -750).
        let body = build_simple_glyph(&[0], &[(10, 0, true)]);
        let mut body = body;
        body[2..4].copy_from_slice(&10i16.to_be_bytes());
        body[8..10].copy_from_slice(&200i16.to_be_bytes());
        let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&body);

        let hmtx_bytes = build_hmtx(&[(300, 4)]);
        let hmtx = Hmtx::parse(&hmtx_bytes, 1, 1).unwrap();
        // vmtx body: one long metric (advance=1000, tsb=50).
        let mut vmtx_bytes = Vec::new();
        vmtx_bytes.extend_from_slice(&1000u16.to_be_bytes());
        vmtx_bytes.extend_from_slice(&50i16.to_be_bytes());
        let vmtx = Vmtx::parse(&vmtx_bytes, 1, 1).unwrap();
        let metrics = PhantomMetrics {
            hmtx: &hmtx,
            vmtx: Some(&vmtx),
        };
        let pp = glyf.phantom_points(&loca, 0, &metrics).unwrap();
        assert!((pp[2].1 - 250.0).abs() < 1e-4, "pp3 y = {}", pp[2].1);
        assert!((pp[3].1 + 750.0).abs() < 1e-4, "pp4 y = {}", pp[3].1);
    }

    #[test]
    fn phantom_points_no_glyph_body_yields_zero_pp1_pp2() {
        // Empty glyph (zero loca range) → bounds returns None →
        // phantom calc folds xMin/yMax to 0. With advance=500, lsb=10,
        // pp1=(0-10,0)=(-10,0), pp2=(490,0).
        let loca_bytes = build_loca_short(&[0, 0]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&[]);
        let hmtx_bytes = build_hmtx(&[(500, 10)]);
        let hmtx = Hmtx::parse(&hmtx_bytes, 1, 1).unwrap();
        let metrics = PhantomMetrics {
            hmtx: &hmtx,
            vmtx: None,
        };
        let pp = glyf.phantom_points(&loca, 0, &metrics).unwrap();
        assert!((pp[0].0 + 10.0).abs() < 1e-4);
        assert!((pp[1].0 - 490.0).abs() < 1e-4);
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
        glyf.outline(&loca, 0, None, None, &mut o).unwrap();
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
        glyf.outline(&loca, 0, None, None, &mut o).unwrap();
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
        glyf.outline(&loca, 0, Some(&deltas), None, &mut o).unwrap();
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
            &[
                (0, 0, true),
                (100, 0, true),
                (100, 100, true),
                (0, 100, true),
            ],
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
        glyf.outline(&loca, 0, None, None, &mut o).unwrap();
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
            &[
                (0, 0, true),
                (100, 0, true),
                (100, 100, true),
                (0, 100, true),
            ],
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
        glyf.outline(&loca, 0, None, None, &mut o).unwrap();
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
    fn composite_anchor_mode_translates_child_to_parent_anchor() {
        // Two-component composite (glyph 0):
        //   1. First component is a contour that *contributes* the
        //      parent's flattened points — a 4-point square anchored
        //      at (10, 20)..(20, 20)..(20, 30)..(10, 30). It draws
        //      itself unchanged.
        //   2. Second component (glyph 2) is a single triangle whose
        //      first point is (0, 0). It is matched in anchor mode
        //      with arg1=1 (parent point index 1 → (20, 20)) and
        //      arg2=0 (child point index 0 → (0, 0)). The implied
        //      translation is parent[1] - child[0] = (20, 20).
        //
        // The test confirms:
        //   - Anchor mode reads two unsigned bytes (no XY_VALUES, no
        //     WORDS) and treats them as point indices.
        //   - The translation is computed from the parent's already-
        //     flattened points (component 1) and the child's own
        //     anchor point.
        //   - Child ops are emitted with the resolved translation.
        //
        // Glyph layout: 0 = composite parent, 1 = parent's "anchor"
        // donor (a 4-point square), 2 = anchor-mode child (triangle).

        // Glyph 1: anchor-donor square at (10,20),(20,20),(20,30),(10,30).
        let g1 = build_simple_glyph(
            &[3],
            &[
                (10, 20, true),
                (20, 20, true),
                (20, 30, true),
                (10, 30, true),
            ],
        );

        // Glyph 2: triangle at (0,0),(40,0),(0,40).
        let g2 = build_simple_glyph(&[2], &[(0, 0, true), (40, 0, true), (0, 40, true)]);

        // Glyph 0: composite. First component glyph 1 with xy
        // translation (0, 0); second component glyph 2 in anchor mode
        // (arg1=1 → parent point 1 = (20, 20); arg2=0 → child point 0
        // = (0, 0)).
        let mut g0 = build_header(-1, 0, 0, 100, 100);
        // Component A: glyph 1, xy_values, words, MORE_COMPONENTS.
        let flags_a: u16 =
            COMP_ARGS_ARE_XY_VALUES | COMP_ARG_1_AND_2_ARE_WORDS | COMP_MORE_COMPONENTS;
        g0.extend_from_slice(&flags_a.to_be_bytes());
        g0.extend_from_slice(&1u16.to_be_bytes());
        g0.extend_from_slice(&0i16.to_be_bytes());
        g0.extend_from_slice(&0i16.to_be_bytes());
        // Component B: glyph 2, anchor mode (no XY_VALUES, no WORDS,
        // last component).
        let flags_b: u16 = 0; // anchor mode, byte args, last.
        g0.extend_from_slice(&flags_b.to_be_bytes());
        g0.extend_from_slice(&2u16.to_be_bytes());
        g0.push(1u8); // arg1 = parent point 1
        g0.push(0u8); // arg2 = child point 0

        // Lay out the glyf table with each glyph on a 2-byte boundary
        // for the short loca format.
        let mut glyf_bytes = Vec::new();
        let off0 = glyf_bytes.len() as u32;
        glyf_bytes.extend_from_slice(&g0);
        if glyf_bytes.len() % 2 != 0 {
            glyf_bytes.push(0);
        }
        let off1 = glyf_bytes.len() as u32;
        glyf_bytes.extend_from_slice(&g1);
        if glyf_bytes.len() % 2 != 0 {
            glyf_bytes.push(0);
        }
        let off2 = glyf_bytes.len() as u32;
        glyf_bytes.extend_from_slice(&g2);
        if glyf_bytes.len() % 2 != 0 {
            glyf_bytes.push(0);
        }
        let off_end = glyf_bytes.len() as u32;

        let loca_bytes = build_loca_short(&[
            (off0 / 2) as u16,
            (off1 / 2) as u16,
            (off2 / 2) as u16,
            (off_end / 2) as u16,
        ]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 3).unwrap();
        let glyf = Glyf::new(&glyf_bytes);

        let mut o = Outline::new();
        glyf.outline(&loca, 0, None, None, &mut o).unwrap();

        // First six ops: square contour from glyph 1 unchanged.
        assert!(matches!(o.ops()[0], PathOp::MoveTo { x: 10.0, y: 20.0 }));
        assert!(matches!(o.ops()[1], PathOp::LineTo { x: 20.0, y: 20.0 }));
        assert!(matches!(o.ops()[2], PathOp::LineTo { x: 20.0, y: 30.0 }));
        assert!(matches!(o.ops()[3], PathOp::LineTo { x: 10.0, y: 30.0 }));
        assert!(matches!(o.ops()[4], PathOp::LineTo { x: 10.0, y: 20.0 }));
        assert!(matches!(o.ops()[5], PathOp::Close));

        // Anchor-mode triangle: child[0] = (0, 0) lands on parent[1]
        // = (20, 20), so every child point shifts by (+20, +20).
        // (0,0)->(20,20), (40,0)->(60,20), (0,40)->(20,60).
        assert!(matches!(o.ops()[6], PathOp::MoveTo { x: 20.0, y: 20.0 }));
        assert!(matches!(o.ops()[7], PathOp::LineTo { x: 60.0, y: 20.0 }));
        assert!(matches!(o.ops()[8], PathOp::LineTo { x: 20.0, y: 60.0 }));
        assert!(matches!(o.ops()[9], PathOp::LineTo { x: 20.0, y: 20.0 }));
        assert!(matches!(o.ops()[10], PathOp::Close));
    }

    #[test]
    fn composite_two_by_two_uses_column_major_layout() {
        // OpenType stores the 2x2 in column-major order. A 90° CCW
        // rotation has xscale=0, scale01=1, scale10=-1, yscale=0, so
        // (x, y) → (-y, x). Pin that mapping with a single-point
        // contour at (10, 0): after rotation it should land at
        // (0, 10), and with translation (50, 5) at (50, 15).
        let child = build_simple_glyph(&[0], &[(10, 0, true)]);
        let mut parent = build_header(-1, 0, 0, 100, 100);
        let flags: u16 =
            COMP_ARGS_ARE_XY_VALUES | COMP_ARG_1_AND_2_ARE_WORDS | COMP_WE_HAVE_A_TWO_BY_TWO;
        parent.extend_from_slice(&flags.to_be_bytes());
        parent.extend_from_slice(&1u16.to_be_bytes());
        parent.extend_from_slice(&50i16.to_be_bytes()); // dx
        parent.extend_from_slice(&5i16.to_be_bytes()); //  dy
        let one = 16384i16; // 1.0 in F2Dot14
        parent.extend_from_slice(&0i16.to_be_bytes()); // xscale = 0
        parent.extend_from_slice(&one.to_be_bytes()); //  scale01 = 1
        parent.extend_from_slice(&(-one).to_be_bytes()); // scale10 = -1
        parent.extend_from_slice(&0i16.to_be_bytes()); // yscale = 0

        let mut glyf_bytes = Vec::new();
        let p_off = glyf_bytes.len() as u32;
        glyf_bytes.extend_from_slice(&parent);
        if glyf_bytes.len() % 2 != 0 {
            glyf_bytes.push(0);
        }
        let c_off = glyf_bytes.len() as u32;
        glyf_bytes.extend_from_slice(&child);
        if glyf_bytes.len() % 2 != 0 {
            glyf_bytes.push(0);
        }
        let end_off = glyf_bytes.len() as u32;
        let loca_bytes =
            build_loca_short(&[(p_off / 2) as u16, (c_off / 2) as u16, (end_off / 2) as u16]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 2).unwrap();
        let glyf = Glyf::new(&glyf_bytes);
        let mut o = Outline::new();
        glyf.outline(&loca, 0, None, None, &mut o).unwrap();
        match o.ops()[0] {
            PathOp::MoveTo { x, y } => {
                assert!((x - 50.0).abs() < 1e-3, "x = {x}");
                assert!((y - 15.0).abs() < 1e-3, "y = {y}");
            }
            other => panic!("expected MoveTo, got {other:?}"),
        }
    }

    #[test]
    fn composite_anchor_mode_resolves_parent_phantom_point() {
        // Parent (glyph 0) is a composite with two components:
        //   - Component A (glyph 1): a square at (10, 0) → (40, 30).
        //     Its xMin=10 and lsb=4 imply pp1=(6,0) and
        //     pp2=(6+advance,0).
        //     The composite parent inherits its own metrics from
        //     gid 0 (advance=300, lsb=4); xMin/yMax from the parent
        //     header are 10 and 30. Parent's own pp1=(6,0),
        //     pp2=(306,0).
        //   - Component B (glyph 2): triangle (0,0)/(40,0)/(0,40).
        //     Anchor mode targets parent's pp2 (index =
        //     numContourPoints + 1) and child's own point 0.
        //
        // Expected translation = parent.pp2 - child[0]
        //   = (306, 0) - (0, 0) = (306, 0).
        let g1 = build_simple_glyph(
            &[3],
            &[(10, 0, true), (40, 0, true), (40, 30, true), (10, 30, true)],
        );
        let g2 = build_simple_glyph(&[2], &[(0, 0, true), (40, 0, true), (0, 40, true)]);

        // Parent composite header. xMin=10, yMin=0, xMax=40, yMax=30
        // — matches the donor square so the parent's bounds line up
        // with its real points.
        let mut g0 = build_header(-1, 10, 0, 40, 30);
        let flags_a: u16 =
            COMP_ARGS_ARE_XY_VALUES | COMP_ARG_1_AND_2_ARE_WORDS | COMP_MORE_COMPONENTS;
        g0.extend_from_slice(&flags_a.to_be_bytes());
        g0.extend_from_slice(&1u16.to_be_bytes());
        g0.extend_from_slice(&0i16.to_be_bytes());
        g0.extend_from_slice(&0i16.to_be_bytes());
        // Component B in anchor mode (no XY_VALUES, no WORDS, last).
        // Parent has 4 real points after component A; index 5 = pp2.
        // Child has 3 real points; index 0 = first contour point.
        let flags_b: u16 = 0;
        g0.extend_from_slice(&flags_b.to_be_bytes());
        g0.extend_from_slice(&2u16.to_be_bytes());
        g0.push(5u8); // arg1 = parent pp2 (numContourPoints + 1)
        g0.push(0u8); // arg2 = child point 0

        let mut glyf_bytes = Vec::new();
        let off0 = glyf_bytes.len() as u32;
        glyf_bytes.extend_from_slice(&g0);
        if glyf_bytes.len() % 2 != 0 {
            glyf_bytes.push(0);
        }
        let off1 = glyf_bytes.len() as u32;
        glyf_bytes.extend_from_slice(&g1);
        if glyf_bytes.len() % 2 != 0 {
            glyf_bytes.push(0);
        }
        let off2 = glyf_bytes.len() as u32;
        glyf_bytes.extend_from_slice(&g2);
        if glyf_bytes.len() % 2 != 0 {
            glyf_bytes.push(0);
        }
        let off_end = glyf_bytes.len() as u32;

        let loca_bytes = build_loca_short(&[
            (off0 / 2) as u16,
            (off1 / 2) as u16,
            (off2 / 2) as u16,
            (off_end / 2) as u16,
        ]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 3).unwrap();
        let glyf = Glyf::new(&glyf_bytes);

        // Per-glyph metrics. Parent (gid 0): advance=300, lsb=4 →
        // pp1=(10-4, 0)=(6,0), pp2=(306,0). Other glyphs need only
        // be parseable.
        let hmtx_bytes = build_hmtx(&[(300, 4), (60, 4), (40, 0)]);
        let hmtx = Hmtx::parse(&hmtx_bytes, 3, 3).unwrap();
        let metrics = PhantomMetrics {
            hmtx: &hmtx,
            vmtx: None,
        };

        let mut o = Outline::new();
        glyf.outline(&loca, 0, None, Some(&metrics), &mut o)
            .unwrap();

        // First six ops are component A's square unchanged.
        // Ops 6..= are the anchor-mode triangle, translated by
        // parent.pp2 = (306, 0).
        // child[0]=(0,0)   -> (306, 0)
        // child[1]=(40,0)  -> (346, 0)
        // child[2]=(0,40)  -> (306, 40)
        match o.ops()[6] {
            PathOp::MoveTo { x, y } => {
                assert!((x - 306.0).abs() < 1e-4, "got x={x}");
                assert!((y - 0.0).abs() < 1e-4, "got y={y}");
            }
            other => panic!("expected MoveTo at 6, got {other:?}"),
        }
        match o.ops()[7] {
            PathOp::LineTo { x, y } => {
                assert!((x - 346.0).abs() < 1e-4);
                assert!((y - 0.0).abs() < 1e-4);
            }
            other => panic!("expected LineTo at 7, got {other:?}"),
        }
        match o.ops()[8] {
            PathOp::LineTo { x, y } => {
                assert!((x - 306.0).abs() < 1e-4);
                assert!((y - 40.0).abs() < 1e-4);
            }
            other => panic!("expected LineTo at 8, got {other:?}"),
        }
    }

    #[test]
    fn composite_anchor_phantom_without_metrics_falls_back_to_zero() {
        // Same composite shape as the phantom-resolution test, but
        // with `metrics=None`. The legacy fallback applies: the
        // anchor index is out-of-range and the translation collapses
        // to (0, 0). Pin the behaviour so callers that opt out of
        // phantom resolution still get a stable answer.
        let g1 = build_simple_glyph(
            &[3],
            &[(0, 0, true), (10, 0, true), (10, 10, true), (0, 10, true)],
        );
        let g2 = build_simple_glyph(&[0], &[(0, 0, true)]);

        let mut g0 = build_header(-1, 0, 0, 10, 10);
        let flags_a: u16 =
            COMP_ARGS_ARE_XY_VALUES | COMP_ARG_1_AND_2_ARE_WORDS | COMP_MORE_COMPONENTS;
        g0.extend_from_slice(&flags_a.to_be_bytes());
        g0.extend_from_slice(&1u16.to_be_bytes());
        g0.extend_from_slice(&0i16.to_be_bytes());
        g0.extend_from_slice(&0i16.to_be_bytes());
        let flags_b: u16 = 0;
        g0.extend_from_slice(&flags_b.to_be_bytes());
        g0.extend_from_slice(&2u16.to_be_bytes());
        g0.push(5u8); // pp2
        g0.push(0u8);

        let mut glyf_bytes = Vec::new();
        glyf_bytes.extend_from_slice(&g0);
        if glyf_bytes.len() % 2 != 0 {
            glyf_bytes.push(0);
        }
        let off1 = glyf_bytes.len() as u32;
        glyf_bytes.extend_from_slice(&g1);
        if glyf_bytes.len() % 2 != 0 {
            glyf_bytes.push(0);
        }
        let off2 = glyf_bytes.len() as u32;
        glyf_bytes.extend_from_slice(&g2);
        if glyf_bytes.len() % 2 != 0 {
            glyf_bytes.push(0);
        }
        let off_end = glyf_bytes.len() as u32;
        let loca_bytes = build_loca_short(&[
            0,
            (off1 / 2) as u16,
            (off2 / 2) as u16,
            (off_end / 2) as u16,
        ]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 3).unwrap();
        let glyf = Glyf::new(&glyf_bytes);

        let mut o = Outline::new();
        glyf.outline(&loca, 0, None, None, &mut o).unwrap();
        // Component A drew 4 points + close, so child's MoveTo lands
        // at op index 6 with no translation: child[0]=(0,0).
        match o.ops()[6] {
            PathOp::MoveTo { x, y } => {
                assert!((x - 0.0).abs() < 1e-4);
                assert!((y - 0.0).abs() < 1e-4);
            }
            other => panic!("expected MoveTo at 6, got {other:?}"),
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
        assert!(glyf.outline(&loca, 0, None, None, &mut o).is_err());
    }
}
