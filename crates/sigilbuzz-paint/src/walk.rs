//! Paint-tree walk in HarfBuzz's callback order.
//!
//! HarfBuzz's `hb_font_paint_glyph` reports a color glyph as nested
//! push/pop callbacks and leaves color resolution to the paint context.
//! [`paint_glyph`] walks the same tree the same way and hands every step
//! to a [`PaintSink`]. sigilbuzz-capi drives its `hb_paint_funcs_t`
//! bridge from it, [`crate::evaluate_with`] flattens it into
//! [`DrawCmd`](crate::DrawCmd)s, and the sigilbuzz renderers draw it.
//!
//! The sequence is HarfBuzz 11's (checked against the paint traces in
//! HarfBuzz's `test/api/results-paint` at the 11.0.0 tag):
//!
//! - A COLRv1 glyph is clipped to its bounds, then wrapped in
//!   `push_root_transform` / `pop_transform`. The bounds are the
//!   glyph's ClipList box, or, when it has none, the bounds of its
//!   paint tree (see [`RootClip`]). A glyph whose paint escapes every
//!   clip is unbounded and paints nothing inside the root transform.
//! - `PaintGlyph` is `push_inverse_root_transform`, `push_clip_glyph`,
//!   `push_root_transform`, the child, then `pop_transform`,
//!   `pop_clip`, `pop_transform`: the clip outline is drawn at font
//!   scale while the child stays in design units, so a transform below
//!   a `PaintGlyph` moves the fill but never the outline.
//! - `PaintColrGlyph` first offers the glyph to
//!   [`PaintSink::color_glyph`] inside `push_inverse_root_transform` /
//!   `pop_transform`. If the sink declines, the referenced glyph's paint
//!   is walked inside its ClipList box (`push_clip_rectangle`, in design
//!   units) when it has one. A glyph already on the walk stack paints
//!   nothing.
//! - `PaintColrLayers` walks its layers in order, without groups. A
//!   layer already on the walk stack is skipped.
//! - Transform paints push one transform each. Translate, scale,
//!   rotate, and skew skip the push when they are the identity, and
//!   the `AroundCenter` forms push translate, the operation, and the
//!   opposite translate.
//! - `PaintComposite` is `push_group`, the backdrop, `push_group`, the
//!   source, `pop_group(mode)`, `pop_group(SrcOver)`: the source and the
//!   backdrop composite in isolation.
//! - Sweep angles are reported in radians as `(angle + 1) * pi`.
//! - A COLRv0 glyph is one `push_clip_glyph`, `color`, `pop_clip`
//!   triple per layer, with alpha 1.
//!
//! Color references reach the sink unresolved ([`ColorRef`]), with
//! variation deltas already applied, so the sink can resolve palette
//! entries the way its own API defines; [`Resolver`] resolves them the
//! way [`crate::evaluate_with`] does.
//!
//! This module is public only for the sigilbuzz companion crates. It is
//! not part of the stable API and may change in any minor release.

use alloc::vec::Vec;
use core::f32::consts::PI;

use sigilbuzz::tables::colr::{ColorLine, Colr, ColrPaint, CompositeMode, PaintOffset};
use sigilbuzz::tables::cpal::Cpal;
use sigilbuzz::Face;

use crate::color::Color;
use crate::deltas::Deltas;
use crate::eval::GlyphId;
use crate::extents::ExtentsSink;
use crate::gradient::{ColorStop, Extend};
use crate::options::{EvalOptions, Palette};
use crate::transform::{sweep_angle_to_radians, Transform2D};

/// Maximum paint nesting depth, HarfBuzz's `HB_MAX_NESTING_LEVEL`. A
/// deeper paint is not walked.
const MAX_DEPTH: usize = 64;

/// Maximum number of paints one glyph may visit. Shared subtrees can
/// make a small paint graph expand exponentially; the walk stops once
/// this many paints have been visited.
const MAX_EDGES: u32 = 65_536;

/// Maximum number of color stops one walk may resolve. The paint
/// budget alone does not bound the work: every gradient visit resolves
/// its whole color line, and one line can hold 65535 stops. Real color
/// glyphs stay far below this. When a color line does not fit in what
/// is left, the walk stops.
const MAX_STOPS: u32 = 1 << 18;

/// An unresolved COLR color: a palette entry plus the alpha that
/// multiplies it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorRef {
    /// CPAL palette entry. `0xFFFF` asks for the foreground color.
    pub palette_entry: u16,
    /// Alpha factor from the paint or color stop, variation deltas
    /// applied. Not clamped.
    pub alpha: f32,
}

/// One unresolved gradient stop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StopRef {
    /// Position on the color line, variation delta applied.
    pub offset: f32,
    /// The stop's color reference.
    pub color: ColorRef,
}

/// The color line a gradient callback receives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorLineRef<'s> {
    /// Stops in font order.
    pub stops: &'s [StopRef],
    /// How the gradient extends past its ends.
    pub extend: Extend,
}

/// The clip [`paint_glyph`] puts around a whole COLRv1 glyph, outside
/// the root transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RootClip {
    /// The glyph's ClipList box in design units, variation deltas
    /// applied and rounded. HarfBuzz scales it to font units with
    /// integer rounding.
    ClipBox {
        /// Left edge.
        x_min: i32,
        /// Bottom edge.
        y_min: i32,
        /// Right edge.
        x_max: i32,
        /// Top edge.
        y_max: i32,
    },
    /// Bounds computed from the paint tree, in design units, for a
    /// glyph without a ClipList box. When `bounded` is false some fill
    /// escaped every clip; the extents are then meaningless and the
    /// glyph paints nothing.
    Extents {
        /// Left edge.
        x_min: f32,
        /// Bottom edge.
        y_min: f32,
        /// Right edge.
        x_max: f32,
        /// Top edge.
        y_max: f32,
        /// False for an unbounded glyph.
        bounded: bool,
    },
}

impl RootClip {
    /// The clip rectangle in design units as `(x_min, y_min, x_max,
    /// y_max)`.
    #[must_use]
    pub fn rect(&self) -> (f32, f32, f32, f32) {
        match *self {
            RootClip::ClipBox {
                x_min,
                y_min,
                x_max,
                y_max,
            } => (x_min as f32, y_min as f32, x_max as f32, y_max as f32),
            RootClip::Extents {
                x_min,
                y_min,
                x_max,
                y_max,
                ..
            } => (x_min, y_min, x_max, y_max),
        }
    }

    /// False for an unbounded glyph, which paints nothing.
    #[must_use]
    pub fn is_bounded(&self) -> bool {
        !matches!(self, RootClip::Extents { bounded: false, .. })
    }
}

/// Receives a color glyph's paint operations in HarfBuzz's order.
///
/// Every `push_*` is matched by exactly one pop: transforms by
/// [`PaintSink::pop_transform`], clips (glyph, rectangle, and root) by
/// [`PaintSink::pop_clip`], and groups by [`PaintSink::pop_group`].
/// Coordinates are in font design units, inside whatever transforms are
/// pushed.
pub trait PaintSink {
    /// Pushes an affine transform.
    fn push_transform(&mut self, transform: Transform2D);
    /// Pushes the transform from design units to font scale.
    fn push_root_transform(&mut self);
    /// Pushes the inverse of the root transform.
    fn push_inverse_root_transform(&mut self);
    /// Pops the most recent transform, root or not.
    fn pop_transform(&mut self);
    /// Clips to the outline of `glyph`.
    fn push_clip_glyph(&mut self, glyph: GlyphId);
    /// Clips to a rectangle given in the current coordinates.
    fn push_clip_rectangle(&mut self, x_min: f32, y_min: f32, x_max: f32, y_max: f32);
    /// Clips the whole glyph, before the root transform is pushed.
    fn push_root_clip(&mut self, clip: RootClip);
    /// Pops the most recent clip.
    fn pop_clip(&mut self);
    /// Starts an isolated group.
    fn push_group(&mut self);
    /// Ends the most recent group and composites it with `mode`.
    fn pop_group(&mut self, mode: CompositeMode);
    /// Offered every `PaintColrGlyph` target, inside the inverse root
    /// transform. Returning true means the sink painted the glyph itself
    /// and the walk skips its paint tree. The default declines.
    fn color_glyph(&mut self, glyph: GlyphId) -> bool {
        let _ = glyph;
        false
    }
    /// Fills the current clip with one color.
    fn color(&mut self, color: ColorRef);
    /// Fills the current clip with a linear gradient.
    fn linear_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        p0: (f32, f32),
        p1: (f32, f32),
        p2: (f32, f32),
    );
    /// Fills the current clip with a two-circle radial gradient.
    fn radial_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        c0: (f32, f32),
        r0: f32,
        c1: (f32, f32),
        r1: f32,
    );
    /// Fills the current clip with a sweep gradient. Angles are in
    /// radians, counter-clockwise from the positive x axis.
    fn sweep_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        center: (f32, f32),
        start_angle: f32,
        end_angle: f32,
    );
}

/// What [`paint_glyph`] found for the glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Painted {
    /// A COLRv1 paint tree was walked.
    ColrV1,
    /// COLRv0 layers were walked.
    ColrV0,
    /// The glyph has no color data; nothing reached the sink.
    Nothing,
}

/// Walks `glyph`'s color data at the normalized variation `coords`,
/// reporting each step to `sink`, the way `hb_font_paint_glyph` does:
/// a COLRv1 glyph is clipped to its [`RootClip`]. A COLRv1 paint tree
/// wins over COLRv0 layers, as in HarfBuzz.
pub fn paint_glyph(
    face: &Face<'_>,
    glyph: GlyphId,
    coords: &[f32],
    sink: &mut dyn PaintSink,
) -> Painted {
    paint(face, glyph, coords, true, sink)
}

/// Like [`paint_glyph`] without the root clip: every COLRv1 glyph is
/// walked, bounded or not, directly inside the root transform. This is
/// the walk HarfBuzz runs to compute a glyph's bounds.
pub fn paint_glyph_unclipped(
    face: &Face<'_>,
    glyph: GlyphId,
    coords: &[f32],
    sink: &mut dyn PaintSink,
) -> Painted {
    paint(face, glyph, coords, false, sink)
}

fn paint(
    face: &Face<'_>,
    glyph: GlyphId,
    coords: &[f32],
    clip: bool,
    sink: &mut dyn PaintSink,
) -> Painted {
    let Ok(Some(colr)) = face.colr() else {
        return Painted::Nothing;
    };
    if let Some(root) = colr.paint(glyph) {
        let deltas = Deltas::new(&colr, coords);
        let root_clip = if clip {
            Some(root_clip(face, &colr, &deltas, glyph, coords))
        } else {
            None
        };
        if let Some(c) = root_clip {
            sink.push_root_clip(c);
        }
        sink.push_root_transform();
        if root_clip.map_or(true, |c| c.is_bounded()) {
            let mut walker = Walker {
                colr: &colr,
                deltas,
                glyphs: alloc::vec![glyph],
                layers: Vec::new(),
                edges_left: MAX_EDGES,
                stops_left: MAX_STOPS,
                sink: &mut *sink,
            };
            walker.paint(Some(root), 0);
        }
        sink.pop_transform();
        if root_clip.is_some() {
            sink.pop_clip();
        }
        return Painted::ColrV1;
    }
    if let Some(layers) = colr.v0_layers(glyph) {
        for layer in layers.iter() {
            sink.push_clip_glyph(layer.glyph_id);
            sink.color(ColorRef {
                palette_entry: layer.palette_index,
                alpha: 1.0,
            });
            sink.pop_clip();
        }
        return Painted::ColrV0;
    }
    Painted::Nothing
}

/// The ClipList box of `glyph`, else the bounds of its paint tree.
fn root_clip(
    face: &Face<'_>,
    colr: &Colr<'_>,
    deltas: &Deltas<'_, '_>,
    glyph: GlyphId,
    coords: &[f32],
) -> RootClip {
    if let Some(clip) = colr.clip_box(glyph) {
        let [x_min, y_min, x_max, y_max] = deltas.clip_box(clip);
        return RootClip::ClipBox {
            x_min,
            y_min,
            x_max,
            y_max,
        };
    }
    let mut extents = ExtentsSink::new(face, coords);
    paint_glyph_unclipped(face, glyph, coords, &mut extents);
    extents.root_clip()
}

struct Walker<'a, 'b, 's> {
    colr: &'b Colr<'a>,
    deltas: Deltas<'a, 'b>,
    /// COLR glyphs whose trees are on the walk stack.
    glyphs: Vec<GlyphId>,
    /// LayerList indices on the walk stack.
    layers: Vec<u32>,
    /// Paints the walk may still visit.
    edges_left: u32,
    /// Color stops the walk may still resolve.
    stops_left: u32,
    sink: &'s mut dyn PaintSink,
}

impl Walker<'_, '_, '_> {
    /// Walks one paint. `None` (a bad offset) paints nothing, but the
    /// caller's pushes and pops around it still happen, as in HarfBuzz.
    fn paint(&mut self, paint: Option<ColrPaint<'_>>, depth: usize) {
        let Some(paint) = paint else {
            return;
        };
        if depth >= MAX_DEPTH || self.edges_left == 0 {
            return;
        }
        self.edges_left -= 1;
        let depth = depth + 1;
        match paint {
            ColrPaint::ColrLayers {
                num_layers,
                first_layer_index,
            } => {
                for i in 0..u32::from(num_layers) {
                    let Some(index) = first_layer_index.checked_add(i) else {
                        break;
                    };
                    if self.layers.contains(&index) {
                        continue;
                    }
                    self.layers.push(index);
                    self.paint(self.colr.layer_paint(index), depth);
                    self.layers.pop();
                }
            }
            ColrPaint::ColrGlyph { glyph_id } => self.colr_glyph(glyph_id, depth),
            ColrPaint::Solid {
                palette_index,
                alpha,
            } => self.sink.color(ColorRef {
                palette_entry: palette_index,
                alpha,
            }),
            ColrPaint::VarSolid {
                palette_index,
                alpha,
                var_index_base,
            } => self.sink.color(ColorRef {
                palette_entry: palette_index,
                alpha: alpha + self.deltas.f2dot14(var_index_base, 0),
            }),
            ColrPaint::LinearGradient {
                color_line,
                x0,
                y0,
                x1,
                y1,
                x2,
                y2,
            } => {
                let Some(stops) = self.stops(color_line) else {
                    return;
                };
                self.sink.linear_gradient(
                    line(&stops, color_line),
                    (f32::from(x0), f32::from(y0)),
                    (f32::from(x1), f32::from(y1)),
                    (f32::from(x2), f32::from(y2)),
                );
            }
            ColrPaint::VarLinearGradient {
                color_line,
                x0,
                y0,
                x1,
                y1,
                x2,
                y2,
                var_index_base,
            } => {
                let Some(stops) = self.stops(color_line) else {
                    return;
                };
                let d = |i| self.deltas.raw(var_index_base, i);
                let (p0, p1, p2) = (
                    (f32::from(x0) + d(0), f32::from(y0) + d(1)),
                    (f32::from(x1) + d(2), f32::from(y1) + d(3)),
                    (f32::from(x2) + d(4), f32::from(y2) + d(5)),
                );
                self.sink
                    .linear_gradient(line(&stops, color_line), p0, p1, p2);
            }
            ColrPaint::RadialGradient {
                color_line,
                x0,
                y0,
                r0,
                x1,
                y1,
                r1,
            } => {
                let Some(stops) = self.stops(color_line) else {
                    return;
                };
                self.sink.radial_gradient(
                    line(&stops, color_line),
                    (f32::from(x0), f32::from(y0)),
                    f32::from(r0),
                    (f32::from(x1), f32::from(y1)),
                    f32::from(r1),
                );
            }
            ColrPaint::VarRadialGradient {
                color_line,
                x0,
                y0,
                r0,
                x1,
                y1,
                r1,
                var_index_base,
            } => {
                let Some(stops) = self.stops(color_line) else {
                    return;
                };
                let d = |i| self.deltas.raw(var_index_base, i);
                let (c0, rr0, c1, rr1) = (
                    (f32::from(x0) + d(0), f32::from(y0) + d(1)),
                    f32::from(r0) + d(2),
                    (f32::from(x1) + d(3), f32::from(y1) + d(4)),
                    f32::from(r1) + d(5),
                );
                self.sink
                    .radial_gradient(line(&stops, color_line), c0, rr0, c1, rr1);
            }
            ColrPaint::SweepGradient {
                color_line,
                center_x,
                center_y,
                start_angle,
                end_angle,
            } => {
                let Some(stops) = self.stops(color_line) else {
                    return;
                };
                self.sink.sweep_gradient(
                    line(&stops, color_line),
                    (f32::from(center_x), f32::from(center_y)),
                    sweep_angle_to_radians(start_angle),
                    sweep_angle_to_radians(end_angle),
                );
            }
            ColrPaint::VarSweepGradient {
                color_line,
                center_x,
                center_y,
                start_angle,
                end_angle,
                var_index_base,
            } => {
                let Some(stops) = self.stops(color_line) else {
                    return;
                };
                let center = (
                    f32::from(center_x) + self.deltas.raw(var_index_base, 0),
                    f32::from(center_y) + self.deltas.raw(var_index_base, 1),
                );
                let start = start_angle + self.deltas.f2dot14(var_index_base, 2);
                let end = end_angle + self.deltas.f2dot14(var_index_base, 3);
                self.sink.sweep_gradient(
                    line(&stops, color_line),
                    center,
                    sweep_angle_to_radians(start),
                    sweep_angle_to_radians(end),
                );
            }
            ColrPaint::Glyph {
                paint_offset,
                glyph_id,
            } => {
                self.sink.push_inverse_root_transform();
                self.sink.push_clip_glyph(glyph_id);
                self.sink.push_root_transform();
                self.child(paint_offset, depth);
                self.sink.pop_transform();
                self.sink.pop_clip();
                self.sink.pop_transform();
            }
            ColrPaint::Transform {
                paint_offset,
                xx,
                yx,
                xy,
                yy,
                dx,
                dy,
            } => {
                self.sink.push_transform(Transform2D {
                    xx,
                    yx,
                    xy,
                    yy,
                    dx,
                    dy,
                });
                self.child(paint_offset, depth);
                self.sink.pop_transform();
            }
            ColrPaint::VarTransform {
                paint_offset,
                xx,
                yx,
                xy,
                yy,
                dx,
                dy,
                var_index_base,
            } => {
                let d = |i| self.deltas.fixed(var_index_base, i);
                let m = Transform2D {
                    xx: xx + d(0),
                    yx: yx + d(1),
                    xy: xy + d(2),
                    yy: yy + d(3),
                    dx: dx + d(4),
                    dy: dy + d(5),
                };
                self.sink.push_transform(m);
                self.child(paint_offset, depth);
                self.sink.pop_transform();
            }
            ColrPaint::Translate {
                paint_offset,
                dx,
                dy,
            } => {
                let pushed = self.push_translate(f32::from(dx), f32::from(dy));
                self.child(paint_offset, depth);
                self.pop_if(pushed);
            }
            ColrPaint::VarTranslate {
                paint_offset,
                dx,
                dy,
                var_index_base,
            } => {
                let dx = f32::from(dx) + self.deltas.raw(var_index_base, 0);
                let dy = f32::from(dy) + self.deltas.raw(var_index_base, 1);
                let pushed = self.push_translate(dx, dy);
                self.child(paint_offset, depth);
                self.pop_if(pushed);
            }
            ColrPaint::Scale {
                paint_offset,
                scale_x,
                scale_y,
            } => self.around(paint_offset, depth, None, Op::Scale(scale_x, scale_y)),
            ColrPaint::VarScale {
                paint_offset,
                scale_x,
                scale_y,
                var_index_base,
            } => {
                let sx = scale_x + self.deltas.f2dot14(var_index_base, 0);
                let sy = scale_y + self.deltas.f2dot14(var_index_base, 1);
                self.around(paint_offset, depth, None, Op::Scale(sx, sy));
            }
            ColrPaint::ScaleAroundCenter {
                paint_offset,
                scale_x,
                scale_y,
                center_x,
                center_y,
            } => {
                let center = (f32::from(center_x), f32::from(center_y));
                self.around(
                    paint_offset,
                    depth,
                    Some(center),
                    Op::Scale(scale_x, scale_y),
                );
            }
            ColrPaint::VarScaleAroundCenter {
                paint_offset,
                scale_x,
                scale_y,
                center_x,
                center_y,
                var_index_base,
            } => {
                let sx = scale_x + self.deltas.f2dot14(var_index_base, 0);
                let sy = scale_y + self.deltas.f2dot14(var_index_base, 1);
                let center = self.center(center_x, center_y, var_index_base, 2);
                self.around(paint_offset, depth, Some(center), Op::Scale(sx, sy));
            }
            ColrPaint::ScaleUniform {
                paint_offset,
                scale,
            } => self.around(paint_offset, depth, None, Op::Scale(scale, scale)),
            ColrPaint::VarScaleUniform {
                paint_offset,
                scale,
                var_index_base,
            } => {
                let s = scale + self.deltas.f2dot14(var_index_base, 0);
                self.around(paint_offset, depth, None, Op::Scale(s, s));
            }
            ColrPaint::ScaleUniformAroundCenter {
                paint_offset,
                scale,
                center_x,
                center_y,
            } => {
                let center = (f32::from(center_x), f32::from(center_y));
                self.around(paint_offset, depth, Some(center), Op::Scale(scale, scale));
            }
            ColrPaint::VarScaleUniformAroundCenter {
                paint_offset,
                scale,
                center_x,
                center_y,
                var_index_base,
            } => {
                let s = scale + self.deltas.f2dot14(var_index_base, 0);
                let center = self.center(center_x, center_y, var_index_base, 1);
                self.around(paint_offset, depth, Some(center), Op::Scale(s, s));
            }
            ColrPaint::Rotate {
                paint_offset,
                angle,
            } => self.around(paint_offset, depth, None, Op::Rotate(angle)),
            ColrPaint::VarRotate {
                paint_offset,
                angle,
                var_index_base,
            } => {
                let a = angle + self.deltas.f2dot14(var_index_base, 0);
                self.around(paint_offset, depth, None, Op::Rotate(a));
            }
            ColrPaint::RotateAroundCenter {
                paint_offset,
                angle,
                center_x,
                center_y,
            } => {
                let center = (f32::from(center_x), f32::from(center_y));
                self.around(paint_offset, depth, Some(center), Op::Rotate(angle));
            }
            ColrPaint::VarRotateAroundCenter {
                paint_offset,
                angle,
                center_x,
                center_y,
                var_index_base,
            } => {
                let a = angle + self.deltas.f2dot14(var_index_base, 0);
                let center = self.center(center_x, center_y, var_index_base, 1);
                self.around(paint_offset, depth, Some(center), Op::Rotate(a));
            }
            ColrPaint::Skew {
                paint_offset,
                x_skew_angle,
                y_skew_angle,
            } => self.around(
                paint_offset,
                depth,
                None,
                Op::Skew(x_skew_angle, y_skew_angle),
            ),
            ColrPaint::VarSkew {
                paint_offset,
                x_skew_angle,
                y_skew_angle,
                var_index_base,
            } => {
                let x = x_skew_angle + self.deltas.f2dot14(var_index_base, 0);
                let y = y_skew_angle + self.deltas.f2dot14(var_index_base, 1);
                self.around(paint_offset, depth, None, Op::Skew(x, y));
            }
            ColrPaint::SkewAroundCenter {
                paint_offset,
                x_skew_angle,
                y_skew_angle,
                center_x,
                center_y,
            } => {
                let center = (f32::from(center_x), f32::from(center_y));
                let op = Op::Skew(x_skew_angle, y_skew_angle);
                self.around(paint_offset, depth, Some(center), op);
            }
            ColrPaint::VarSkewAroundCenter {
                paint_offset,
                x_skew_angle,
                y_skew_angle,
                center_x,
                center_y,
                var_index_base,
            } => {
                let x = x_skew_angle + self.deltas.f2dot14(var_index_base, 0);
                let y = y_skew_angle + self.deltas.f2dot14(var_index_base, 1);
                let center = self.center(center_x, center_y, var_index_base, 2);
                self.around(paint_offset, depth, Some(center), Op::Skew(x, y));
            }
            ColrPaint::Composite {
                source_paint_offset,
                composite_mode,
                backdrop_paint_offset,
            } => {
                self.sink.push_group();
                self.child(backdrop_paint_offset, depth);
                self.sink.push_group();
                self.child(source_paint_offset, depth);
                self.sink.pop_group(composite_mode);
                self.sink.pop_group(CompositeMode::SrcOver);
            }
            // `ColrPaint` is `#[non_exhaustive]`; a paint format this
            // walker does not know paints nothing.
            _ => {}
        }
    }

    /// `PaintColrGlyph`: offer the glyph to the sink, then walk its tree
    /// inside its clip box.
    fn colr_glyph(&mut self, glyph: GlyphId, depth: usize) {
        if self.glyphs.contains(&glyph) {
            return;
        }
        self.glyphs.push(glyph);
        self.sink.push_inverse_root_transform();
        let handled = self.sink.color_glyph(glyph);
        self.sink.pop_transform();
        if !handled {
            let clip = self
                .colr
                .clip_box(glyph)
                .map(|clip| self.deltas.clip_box(clip));
            if let Some([x_min, y_min, x_max, y_max]) = clip {
                self.sink.push_clip_rectangle(
                    x_min as f32,
                    y_min as f32,
                    x_max as f32,
                    y_max as f32,
                );
            }
            self.paint(self.colr.paint(glyph), depth);
            if clip.is_some() {
                self.sink.pop_clip();
            }
        }
        self.glyphs.pop();
    }

    fn child(&mut self, offset: PaintOffset, depth: usize) {
        let child = self.colr.paint_at(offset);
        self.paint(child, depth);
    }

    /// Resolves stop offsets and alphas at the current coordinates.
    /// Returns `None`, and ends the walk, when the line does not fit in
    /// the stop budget.
    fn stops(&mut self, color_line: ColorLine<'_>) -> Option<Vec<StopRef>> {
        let Some(left) = self.stops_left.checked_sub(u32::from(color_line.len())) else {
            self.stops_left = 0;
            self.edges_left = 0;
            return None;
        };
        self.stops_left = left;
        let stops = color_line
            .stops_variable()
            .map(|(stop, stop_var)| {
                let (d_offset, d_alpha) = self.deltas.stop(stop_var);
                StopRef {
                    offset: stop.stop_offset + d_offset,
                    color: ColorRef {
                        palette_entry: stop.palette_index,
                        alpha: stop.alpha + d_alpha,
                    },
                }
            })
            .collect();
        Some(stops)
    }

    /// A variable paint's center: FWORD fields plus design-unit deltas
    /// at `first_field` and the field after it.
    fn center(&self, x: i16, y: i16, base: u32, first_field: u16) -> (f32, f32) {
        (
            f32::from(x) + self.deltas.raw(base, first_field),
            f32::from(y) + self.deltas.raw(base, first_field + 1),
        )
    }

    /// Paints the child at `offset` inside `op`, optionally applied
    /// around `center`: translate to the center, apply `op`, translate
    /// back, each push skipped when it is the identity.
    fn around(&mut self, offset: PaintOffset, depth: usize, center: Option<(f32, f32)>, op: Op) {
        let (cx, cy) = center.unwrap_or((0.0, 0.0));
        let outer = center.is_some() && self.push_translate(cx, cy);
        let pushed = match op.transform() {
            Some(t) => {
                self.sink.push_transform(t);
                true
            }
            None => false,
        };
        let inner = center.is_some() && self.push_translate(-cx, -cy);
        self.child(offset, depth);
        self.pop_if(inner);
        self.pop_if(pushed);
        self.pop_if(outer);
    }

    fn push_translate(&mut self, dx: f32, dy: f32) -> bool {
        if dx == 0.0 && dy == 0.0 {
            return false;
        }
        self.sink.push_transform(Transform2D::translate(dx, dy));
        true
    }

    fn pop_if(&mut self, pushed: bool) {
        if pushed {
            self.sink.pop_transform();
        }
    }
}

/// The operation of a scale, rotate, or skew paint. Angles are
/// F2DOT14 half-turns.
#[derive(Debug, Clone, Copy)]
enum Op {
    Scale(f32, f32),
    Rotate(f32),
    Skew(f32, f32),
}

impl Op {
    /// The matrix HarfBuzz pushes for this operation, or `None` when it
    /// is the identity and nothing is pushed.
    fn transform(self) -> Option<Transform2D> {
        match self {
            Op::Scale(sx, sy) => (sx != 1.0 || sy != 1.0).then_some(Transform2D::scale(sx, sy)),
            Op::Rotate(a) => (a != 0.0).then(|| {
                let (s, c) = ((a * PI).sin(), (a * PI).cos());
                Transform2D {
                    xx: c,
                    yx: s,
                    xy: -s,
                    yy: c,
                    dx: 0.0,
                    dy: 0.0,
                }
            }),
            Op::Skew(x, y) => (x != 0.0 || y != 0.0).then(|| Transform2D {
                xx: 1.0,
                yx: (y * PI).tan(),
                xy: (-x * PI).tan(),
                yy: 1.0,
                dx: 0.0,
                dy: 0.0,
            }),
        }
    }
}

fn line<'s>(stops: &'s [StopRef], color_line: ColorLine<'_>) -> ColorLineRef<'s> {
    ColorLineRef {
        stops,
        extend: color_line.extend.into(),
    }
}

/// Resolves [`ColorRef`]s against a CPAL palette the way
/// [`crate::evaluate_with`] does: palette entry `0xFFFF` is the
/// foreground color with the paint alpha applied, and an entry the font
/// cannot supply falls back to the foreground color, as in HarfBuzz.
#[derive(Debug, Clone, Copy)]
pub struct Resolver<'a, 'b> {
    palette: Palette<'a, 'b>,
}

impl<'a, 'b> Resolver<'a, 'b> {
    /// Resolves against `cpal` (if any) with `options`' palette index
    /// and foreground color.
    #[must_use]
    pub fn new(cpal: Option<&'b Cpal<'a>>, options: &EvalOptions<'_>) -> Self {
        Self {
            palette: Palette::new(cpal, options),
        }
    }

    /// The color for `color`, and whether it is the foreground entry.
    #[must_use]
    pub fn color(&self, color: ColorRef) -> (Color, bool) {
        self.palette.resolve(color.palette_entry, color.alpha)
    }

    /// Resolves every stop of `line`, in order.
    #[must_use]
    pub fn stops(&self, line: ColorLineRef<'_>) -> Vec<ColorStop> {
        line.stops
            .iter()
            .map(|stop| {
                let (color, is_foreground) = self.color(stop.color);
                ColorStop {
                    offset: stop.offset,
                    color,
                    is_foreground,
                }
            })
            .collect()
    }
}
