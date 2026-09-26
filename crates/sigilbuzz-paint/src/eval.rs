//! Paint-tree walker.
//!
//! [`evaluate`] walks the COLRv1 paint DAG rooted at a base glyph and
//! returns a flat [`DrawCmd`] sequence. Transforms compose, gradients
//! resolve to f32 color stops, and `PaintComposite` nodes wrap their
//! child output in [`DrawCmd::PushLayer`] / [`DrawCmd::PopLayer`].
//!
//! Determinism: the output for a given (face, gid, options) tuple is
//! byte-for-byte stable. The walker is depth-first, left-to-right
//! (source then backdrop for composites, in spec order), and never
//! reorders.
//!
//! Robustness: malformed offsets, unknown formats, and reference
//! cycles all truncate the output. The walker never panics.

use alloc::vec::Vec;

use sigilbuzz::tables::colr::{
    ColorLine, Colr, ColrPaint, CompositeMode, F2Dot14, Fword, PaintOffset, VarIndexBase,
};
use sigilbuzz::Face;

use crate::color::Color;
use crate::deltas::Deltas;
use crate::gradient::{ColorStop, Extend, Gradient, GradientKind};
use crate::options::{EvalOptions, Palette};
use crate::transform::{angle_to_radians, sweep_angle_to_radians, Transform2D};

/// Glyph-id alias. Mirrors the on-disk u16 used throughout sigilbuzz.
pub type GlyphId = u16;

/// Maximum DAG-walk depth. Defensive cap above and beyond the
/// visited-set cycle check. A deeply linear chain still exits before
/// blowing the stack.
pub(crate) const MAX_DEPTH: usize = 64;

/// One unit of work emitted by the evaluator.
///
/// A renderer drives compositing by holding a layer stack: every
/// [`DrawCmd::PushLayer`] starts a new fresh layer, the contents up to
/// the matching [`DrawCmd::PopLayer`] are then blended back into the
/// surface below using the recorded [`CompositeMode`].
#[derive(Debug, Clone)]
pub enum DrawCmd {
    /// Fill an outline glyph with a paint source, transformed by the
    /// accumulated 2x3 matrix.
    FillGlyph {
        /// Outline glyph to fill (the leaf of the paint subtree).
        gid: GlyphId,
        /// Accumulated transform from root to this leaf.
        transform: Transform2D,
        /// What to fill the outline with.
        paint: PaintSource,
    },
    /// Begin a layer, used to wrap a composite group. The matching
    /// [`DrawCmd::PopLayer`] applies `composite_mode` against the
    /// surface below.
    PushLayer {
        /// Composite mode used on the matching `PopLayer`.
        composite_mode: CompositeMode,
    },
    /// Finish the most recent layer, blending it into the surface.
    PopLayer,
}

/// Resolved paint source attached to a [`DrawCmd::FillGlyph`].
#[derive(Debug, Clone)]
pub enum PaintSource {
    /// Solid RGBA fill.
    Solid {
        /// Resolved color with the paint alpha folded in. For a
        /// foreground fill this is the evaluation's foreground color
        /// (see [`EvalOptions::with_foreground`]) with the paint alpha
        /// applied.
        color: Color,
        /// True when the paint used COLR palette entry `0xFFFF`, the
        /// foreground (text) color. A renderer with its own text color
        /// can substitute it here, keeping `color.a` relative to the
        /// evaluation foreground's alpha.
        is_foreground: bool,
    },
    /// Resolved gradient: palette indices already substituted for
    /// f32 RGBA, alpha multiplied in, geometry in design-unit space.
    Gradient(Gradient),
}

/// Walks `face`'s COLRv1 paint tree for `gid`, returning the draw
/// commands required to render the color glyph. Equivalent to
/// [`evaluate_with`] with [`EvalOptions::default`].
#[must_use]
pub fn evaluate(face: &Face<'_>, gid: GlyphId) -> Vec<DrawCmd> {
    evaluate_with(face, gid, &EvalOptions::new())
}

/// Same as [`evaluate`] but applies variation deltas from `coords` to
/// every `PaintVar*` node visited. `coords` is the normalized axis
/// vector, the same shape sigilbuzz's
/// [`Face::glyph_outline_at_coords`](sigilbuzz::Face::glyph_outline_at_coords)
/// accepts. An empty slice is the static (no-deltas) path.
#[must_use]
pub fn evaluate_at_coords(face: &Face<'_>, gid: GlyphId, coords: &[f32]) -> Vec<DrawCmd> {
    evaluate_with(face, gid, &EvalOptions::new().with_coords(coords))
}

/// Walks `face`'s COLRv1 paint tree for `gid` with explicit
/// [`EvalOptions`]: variation coordinates, CPAL palette, and the
/// foreground color for palette entry `0xFFFF`.
///
/// ```
/// use sigilbuzz::Face;
/// use sigilbuzz_paint::{evaluate_with, Color, EvalOptions};
///
/// // A face without a COLR table has nothing to paint.
/// let sfnt = [0u8, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
/// let face = Face::parse_bytes(&sfnt, 0).unwrap();
/// let black = Color::new(0.0, 0.0, 0.0, 1.0);
/// let options = EvalOptions::new().with_palette_index(1).with_foreground(black);
/// assert!(evaluate_with(&face, 42, &options).is_empty());
/// ```
#[must_use]
pub fn evaluate_with(face: &Face<'_>, gid: GlyphId, options: &EvalOptions<'_>) -> Vec<DrawCmd> {
    let mut out = Vec::new();
    let Ok(Some(colr)) = face.colr() else {
        return out;
    };
    let cpal = face.cpal().ok().flatten();
    let Some(root) = colr.paint(gid) else {
        return out;
    };

    let mut ctx = EvalCtx {
        colr: &colr,
        palette: Palette::new(cpal.as_ref(), options),
        deltas: Deltas::new(&colr, options.coords()),
        visited: Vec::new(),
        out: &mut out,
    };
    ctx.visited.push(gid);
    walk_paint(&mut ctx, root, Transform2D::IDENTITY, 0);
    out
}

// =========================================================================
// Internal walker
// =========================================================================

/// Mutable walker state. Keeps lifetimes contained so the public
/// `evaluate` signature stays small.
struct EvalCtx<'a, 'b> {
    colr: &'b Colr<'a>,
    /// Selected CPAL palette plus the foreground color.
    palette: Palette<'a, 'b>,
    /// Variation deltas at the requested coordinates.
    deltas: Deltas<'a, 'b>,
    /// Glyph ids whose paint trees are currently on the walk stack.
    /// `PaintColrGlyph` checks this before recursing.
    visited: Vec<GlyphId>,
    out: &'b mut Vec<DrawCmd>,
}

/// Walks one paint node. `xform` is the transform inherited from the
/// chain of ancestor transforms; `depth` is the current recursion
/// depth used for the cycle-bypass safety net.
fn walk_paint(ctx: &mut EvalCtx<'_, '_>, paint: ColrPaint<'_>, xform: Transform2D, depth: usize) {
    if depth >= MAX_DEPTH {
        return;
    }
    match paint {
        // --------- structural: layer list, colr-glyph reference -----
        ColrPaint::ColrLayers {
            num_layers,
            first_layer_index,
        } => {
            for i in 0..u32::from(num_layers) {
                let Some(layer) = first_layer_index
                    .checked_add(i)
                    .and_then(|index| ctx.colr.layer_paint(index))
                else {
                    return;
                };
                walk_paint(ctx, layer, xform, depth + 1);
            }
        }
        ColrPaint::ColrGlyph { glyph_id } => {
            if ctx.visited.contains(&glyph_id) {
                // Cycle: bail without emitting partial output for
                // this subtree.
                return;
            }
            let Some(child) = ctx.colr.paint(glyph_id) else {
                return;
            };
            ctx.visited.push(glyph_id);
            walk_paint(ctx, child, xform, depth + 1);
            ctx.visited.pop();
        }

        // --------- leaves: solid + gradients --------------------------
        ColrPaint::Solid {
            palette_index,
            alpha,
        } => emit_solid(ctx, palette_index, alpha, xform),
        ColrPaint::VarSolid {
            palette_index,
            alpha,
            var_index_base,
        } => {
            // Alpha is F2DOT14. The IVS delta arrives as an int16
            // count of F2DOT14 ticks, so we divide by 16384 to land
            // in the same `0.0..=1.0` scale as `alpha`.
            let alpha = alpha + var_delta_f2dot14(ctx, var_index_base, 0);
            emit_solid(ctx, palette_index, alpha, xform);
        }
        ColrPaint::LinearGradient {
            color_line,
            x0,
            y0,
            x1,
            y1,
            x2,
            y2,
        } => emit_linear_gradient(
            ctx,
            color_line,
            f(x0),
            f(y0),
            f(x1),
            f(y1),
            f(x2),
            f(y2),
            None,
            xform,
        ),
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
            let dx0 = var_delta(ctx, var_index_base, 0);
            let dy0 = var_delta(ctx, var_index_base, 1);
            let dx1 = var_delta(ctx, var_index_base, 2);
            let dy1 = var_delta(ctx, var_index_base, 3);
            let dx2 = var_delta(ctx, var_index_base, 4);
            let dy2 = var_delta(ctx, var_index_base, 5);
            emit_linear_gradient(
                ctx,
                color_line,
                f(x0) + dx0,
                f(y0) + dy0,
                f(x1) + dx1,
                f(y1) + dy1,
                f(x2) + dx2,
                f(y2) + dy2,
                Some(var_index_base),
                xform,
            );
        }
        ColrPaint::RadialGradient {
            color_line,
            x0,
            y0,
            r0,
            x1,
            y1,
            r1,
        } => emit_radial_gradient(
            ctx,
            color_line,
            f(x0),
            f(y0),
            r0 as f32,
            f(x1),
            f(y1),
            r1 as f32,
            None,
            xform,
        ),
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
            let dx0 = var_delta(ctx, var_index_base, 0);
            let dy0 = var_delta(ctx, var_index_base, 1);
            let dr0 = var_delta(ctx, var_index_base, 2);
            let dx1 = var_delta(ctx, var_index_base, 3);
            let dy1 = var_delta(ctx, var_index_base, 4);
            let dr1 = var_delta(ctx, var_index_base, 5);
            emit_radial_gradient(
                ctx,
                color_line,
                f(x0) + dx0,
                f(y0) + dy0,
                r0 as f32 + dr0,
                f(x1) + dx1,
                f(y1) + dy1,
                r1 as f32 + dr1,
                Some(var_index_base),
                xform,
            );
        }
        ColrPaint::SweepGradient {
            color_line,
            center_x,
            center_y,
            start_angle,
            end_angle,
        } => emit_sweep_gradient(
            ctx,
            color_line,
            f(center_x),
            f(center_y),
            start_angle,
            end_angle,
            None,
            xform,
        ),
        ColrPaint::VarSweepGradient {
            color_line,
            center_x,
            center_y,
            start_angle,
            end_angle,
            var_index_base,
        } => {
            let dcx = var_delta(ctx, var_index_base, 0);
            let dcy = var_delta(ctx, var_index_base, 1);
            let dsa = var_delta_f2dot14(ctx, var_index_base, 2);
            let dea = var_delta_f2dot14(ctx, var_index_base, 3);
            emit_sweep_gradient(
                ctx,
                color_line,
                f(center_x) + dcx,
                f(center_y) + dcy,
                start_angle + dsa,
                end_angle + dea,
                Some(var_index_base),
                xform,
            );
        }

        // --------- glyph clip + recursive paint reference -----------
        ColrPaint::Glyph {
            paint_offset,
            glyph_id,
        } => walk_glyph_clip(ctx, paint_offset, glyph_id, xform, depth),

        // --------- transforms -----------------------------------------
        ColrPaint::Transform {
            paint_offset,
            xx,
            yx,
            xy,
            yy,
            dx,
            dy,
        } => walk_with_transform(
            ctx,
            paint_offset,
            xform,
            Transform2D {
                xx,
                yx,
                xy,
                yy,
                dx,
                dy,
            },
            depth,
        ),
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
            // Affine2x3 fields are 16.16 Fixed, so deltas are too.
            let m = Transform2D {
                xx: xx + var_delta_fixed(ctx, var_index_base, 0),
                yx: yx + var_delta_fixed(ctx, var_index_base, 1),
                xy: xy + var_delta_fixed(ctx, var_index_base, 2),
                yy: yy + var_delta_fixed(ctx, var_index_base, 3),
                dx: dx + var_delta_fixed(ctx, var_index_base, 4),
                dy: dy + var_delta_fixed(ctx, var_index_base, 5),
            };
            walk_with_transform(ctx, paint_offset, xform, m, depth);
        }
        ColrPaint::Translate {
            paint_offset,
            dx,
            dy,
        } => walk_with_transform(
            ctx,
            paint_offset,
            xform,
            Transform2D::translate(f(dx), f(dy)),
            depth,
        ),
        ColrPaint::VarTranslate {
            paint_offset,
            dx,
            dy,
            var_index_base,
        } => {
            let ddx = var_delta(ctx, var_index_base, 0);
            let ddy = var_delta(ctx, var_index_base, 1);
            walk_with_transform(
                ctx,
                paint_offset,
                xform,
                Transform2D::translate(f(dx) + ddx, f(dy) + ddy),
                depth,
            );
        }
        ColrPaint::Scale {
            paint_offset,
            scale_x,
            scale_y,
        } => walk_with_transform(
            ctx,
            paint_offset,
            xform,
            Transform2D::scale(scale_x, scale_y),
            depth,
        ),
        ColrPaint::VarScale {
            paint_offset,
            scale_x,
            scale_y,
            var_index_base,
        } => {
            let dsx = var_delta_f2dot14(ctx, var_index_base, 0);
            let dsy = var_delta_f2dot14(ctx, var_index_base, 1);
            walk_with_transform(
                ctx,
                paint_offset,
                xform,
                Transform2D::scale(scale_x + dsx, scale_y + dsy),
                depth,
            );
        }
        ColrPaint::ScaleAroundCenter {
            paint_offset,
            scale_x,
            scale_y,
            center_x,
            center_y,
        } => walk_with_transform(
            ctx,
            paint_offset,
            xform,
            Transform2D::scale(scale_x, scale_y).around_center(f(center_x), f(center_y)),
            depth,
        ),
        ColrPaint::VarScaleAroundCenter {
            paint_offset,
            scale_x,
            scale_y,
            center_x,
            center_y,
            var_index_base,
        } => {
            let dsx = var_delta_f2dot14(ctx, var_index_base, 0);
            let dsy = var_delta_f2dot14(ctx, var_index_base, 1);
            let dcx = var_delta(ctx, var_index_base, 2);
            let dcy = var_delta(ctx, var_index_base, 3);
            walk_with_transform(
                ctx,
                paint_offset,
                xform,
                Transform2D::scale(scale_x + dsx, scale_y + dsy)
                    .around_center(f(center_x) + dcx, f(center_y) + dcy),
                depth,
            );
        }
        ColrPaint::ScaleUniform {
            paint_offset,
            scale,
        } => walk_with_transform(
            ctx,
            paint_offset,
            xform,
            Transform2D::scale(scale, scale),
            depth,
        ),
        ColrPaint::VarScaleUniform {
            paint_offset,
            scale,
            var_index_base,
        } => {
            let ds = var_delta_f2dot14(ctx, var_index_base, 0);
            walk_with_transform(
                ctx,
                paint_offset,
                xform,
                Transform2D::scale(scale + ds, scale + ds),
                depth,
            );
        }
        ColrPaint::ScaleUniformAroundCenter {
            paint_offset,
            scale,
            center_x,
            center_y,
        } => walk_with_transform(
            ctx,
            paint_offset,
            xform,
            Transform2D::scale(scale, scale).around_center(f(center_x), f(center_y)),
            depth,
        ),
        ColrPaint::VarScaleUniformAroundCenter {
            paint_offset,
            scale,
            center_x,
            center_y,
            var_index_base,
        } => {
            let ds = var_delta_f2dot14(ctx, var_index_base, 0);
            let dcx = var_delta(ctx, var_index_base, 1);
            let dcy = var_delta(ctx, var_index_base, 2);
            walk_with_transform(
                ctx,
                paint_offset,
                xform,
                Transform2D::scale(scale + ds, scale + ds)
                    .around_center(f(center_x) + dcx, f(center_y) + dcy),
                depth,
            );
        }
        ColrPaint::Rotate {
            paint_offset,
            angle,
        } => walk_with_transform(
            ctx,
            paint_offset,
            xform,
            Transform2D::rotate(angle_to_radians(angle)),
            depth,
        ),
        ColrPaint::VarRotate {
            paint_offset,
            angle,
            var_index_base,
        } => {
            let da = var_delta_f2dot14(ctx, var_index_base, 0);
            walk_with_transform(
                ctx,
                paint_offset,
                xform,
                Transform2D::rotate(angle_to_radians(angle + da)),
                depth,
            );
        }
        ColrPaint::RotateAroundCenter {
            paint_offset,
            angle,
            center_x,
            center_y,
        } => walk_with_transform(
            ctx,
            paint_offset,
            xform,
            Transform2D::rotate(angle_to_radians(angle)).around_center(f(center_x), f(center_y)),
            depth,
        ),
        ColrPaint::VarRotateAroundCenter {
            paint_offset,
            angle,
            center_x,
            center_y,
            var_index_base,
        } => {
            let da = var_delta_f2dot14(ctx, var_index_base, 0);
            let dcx = var_delta(ctx, var_index_base, 1);
            let dcy = var_delta(ctx, var_index_base, 2);
            walk_with_transform(
                ctx,
                paint_offset,
                xform,
                Transform2D::rotate(angle_to_radians(angle + da))
                    .around_center(f(center_x) + dcx, f(center_y) + dcy),
                depth,
            );
        }
        ColrPaint::Skew {
            paint_offset,
            x_skew_angle,
            y_skew_angle,
        } => walk_with_transform(
            ctx,
            paint_offset,
            xform,
            Transform2D::skew(
                angle_to_radians(x_skew_angle),
                angle_to_radians(y_skew_angle),
            ),
            depth,
        ),
        ColrPaint::VarSkew {
            paint_offset,
            x_skew_angle,
            y_skew_angle,
            var_index_base,
        } => {
            let dxa = var_delta_f2dot14(ctx, var_index_base, 0);
            let dya = var_delta_f2dot14(ctx, var_index_base, 1);
            walk_with_transform(
                ctx,
                paint_offset,
                xform,
                Transform2D::skew(
                    angle_to_radians(x_skew_angle + dxa),
                    angle_to_radians(y_skew_angle + dya),
                ),
                depth,
            );
        }
        ColrPaint::SkewAroundCenter {
            paint_offset,
            x_skew_angle,
            y_skew_angle,
            center_x,
            center_y,
        } => walk_with_transform(
            ctx,
            paint_offset,
            xform,
            Transform2D::skew(
                angle_to_radians(x_skew_angle),
                angle_to_radians(y_skew_angle),
            )
            .around_center(f(center_x), f(center_y)),
            depth,
        ),
        ColrPaint::VarSkewAroundCenter {
            paint_offset,
            x_skew_angle,
            y_skew_angle,
            center_x,
            center_y,
            var_index_base,
        } => {
            let dxa = var_delta_f2dot14(ctx, var_index_base, 0);
            let dya = var_delta_f2dot14(ctx, var_index_base, 1);
            let dcx = var_delta(ctx, var_index_base, 2);
            let dcy = var_delta(ctx, var_index_base, 3);
            walk_with_transform(
                ctx,
                paint_offset,
                xform,
                Transform2D::skew(
                    angle_to_radians(x_skew_angle + dxa),
                    angle_to_radians(y_skew_angle + dya),
                )
                .around_center(f(center_x) + dcx, f(center_y) + dcy),
                depth,
            );
        }

        // --------- composite -----------------------------------------
        ColrPaint::Composite {
            source_paint_offset,
            composite_mode,
            backdrop_paint_offset,
        } => walk_composite(
            ctx,
            source_paint_offset,
            composite_mode,
            backdrop_paint_offset,
            xform,
            depth,
        ),

        // --------- non_exhaustive guard -------------------------------
        // `ColrPaint` is `#[non_exhaustive]` upstream so a future
        // sigilbuzz release can add a paint format without breaking
        // this crate's build. Unknown formats truncate the output.
        _ => {}
    }
}

/// Walks a `PaintGlyph` clip.
///
/// `PaintGlyph` paints the *child* paint, masked through `glyph_id`'s
/// outline. For now the evaluator collapses the mask into the
/// `FillGlyph` command's `gid` field. Leaf paints carry the outline
/// glyph they're filling, and intermediate `PaintGlyph` containers
/// override the leaf's `gid` for any descendants. This works for the
/// vast majority of COLRv1 fonts because `PaintGlyph` always wraps a
/// solid- or gradient-leaf directly, never another `PaintGlyph`.
fn walk_glyph_clip(
    ctx: &mut EvalCtx<'_, '_>,
    paint_offset: PaintOffset,
    glyph_id: GlyphId,
    xform: Transform2D,
    depth: usize,
) {
    let Some(child) = ctx.colr.paint_at(paint_offset) else {
        return;
    };
    // Collect the FillGlyph commands the child emits and rewrite them
    // to fill `glyph_id` instead. A child that emits multiple fills
    // (e.g. a Composite) gets every fill rebound to the same outline,
    // which matches how the spec defines the "paint S inside outline G"
    // mask: the mask applies to whatever the source paint draws.
    let start = ctx.out.len();
    walk_paint(ctx, child, xform, depth + 1);
    for cmd in &mut ctx.out[start..] {
        if let DrawCmd::FillGlyph { gid, .. } = cmd {
            *gid = glyph_id;
        }
    }
}

/// Walks a transform-family paint by composing the local matrix into
/// the inherited one before recursing.
fn walk_with_transform(
    ctx: &mut EvalCtx<'_, '_>,
    paint_offset: PaintOffset,
    xform: Transform2D,
    local: Transform2D,
    depth: usize,
) {
    let Some(child) = ctx.colr.paint_at(paint_offset) else {
        return;
    };
    walk_paint(ctx, child, local.then(xform), depth + 1);
}

/// Walks a `PaintComposite` by emitting `PushLayer` / `PopLayer`
/// around the source paint, with the backdrop drawn first underneath.
///
/// Per the COLRv1 spec the composite is `source COMPOSITE_MODE
/// backdrop`. We emit:
///
/// 1. backdrop draw commands,
/// 2. `PushLayer { mode }`,
/// 3. source draw commands,
/// 4. `PopLayer`: the consumer blends the layer over the backdrop.
fn walk_composite(
    ctx: &mut EvalCtx<'_, '_>,
    source_off: PaintOffset,
    mode: CompositeMode,
    backdrop_off: PaintOffset,
    xform: Transform2D,
    depth: usize,
) {
    let Some(backdrop) = ctx.colr.paint_at(backdrop_off) else {
        return;
    };
    let Some(source) = ctx.colr.paint_at(source_off) else {
        return;
    };
    walk_paint(ctx, backdrop, xform, depth + 1);
    ctx.out.push(DrawCmd::PushLayer {
        composite_mode: mode,
    });
    walk_paint(ctx, source, xform, depth + 1);
    ctx.out.push(DrawCmd::PopLayer);
}

// =========================================================================
// Leaf emitters
// =========================================================================

fn emit_solid(ctx: &mut EvalCtx<'_, '_>, palette_index: u16, alpha: F2Dot14, xform: Transform2D) {
    let (color, is_foreground) = ctx.palette.resolve(palette_index, alpha);
    ctx.out.push(DrawCmd::FillGlyph {
        gid: 0,
        transform: xform,
        paint: PaintSource::Solid {
            color,
            is_foreground,
        },
    });
}

#[allow(clippy::too_many_arguments)]
fn emit_linear_gradient(
    ctx: &mut EvalCtx<'_, '_>,
    color_line: ColorLine<'_>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    var_base: Option<VarIndexBase>,
    xform: Transform2D,
) {
    let stops = resolve_stops(ctx, color_line, var_base);
    let extend: Extend = color_line.extend.into();
    ctx.out.push(DrawCmd::FillGlyph {
        gid: 0,
        transform: xform,
        paint: PaintSource::Gradient(Gradient {
            kind: GradientKind::Linear {
                p0: (x0, y0),
                p1: (x1, y1),
                p2: (x2, y2),
            },
            stops,
            extend,
        }),
    });
}

#[allow(clippy::too_many_arguments)]
fn emit_radial_gradient(
    ctx: &mut EvalCtx<'_, '_>,
    color_line: ColorLine<'_>,
    x0: f32,
    y0: f32,
    r0: f32,
    x1: f32,
    y1: f32,
    r1: f32,
    var_base: Option<VarIndexBase>,
    xform: Transform2D,
) {
    let stops = resolve_stops(ctx, color_line, var_base);
    let extend: Extend = color_line.extend.into();
    ctx.out.push(DrawCmd::FillGlyph {
        gid: 0,
        transform: xform,
        paint: PaintSource::Gradient(Gradient {
            kind: GradientKind::Radial {
                c0: (x0, y0),
                r0,
                c1: (x1, y1),
                r1,
            },
            stops,
            extend,
        }),
    });
}

#[allow(clippy::too_many_arguments)]
fn emit_sweep_gradient(
    ctx: &mut EvalCtx<'_, '_>,
    color_line: ColorLine<'_>,
    cx: f32,
    cy: f32,
    start_angle: F2Dot14,
    end_angle: F2Dot14,
    var_base: Option<VarIndexBase>,
    xform: Transform2D,
) {
    let stops = resolve_stops(ctx, color_line, var_base);
    let extend: Extend = color_line.extend.into();
    ctx.out.push(DrawCmd::FillGlyph {
        gid: 0,
        transform: xform,
        paint: PaintSource::Gradient(Gradient {
            kind: GradientKind::Sweep {
                center: (cx, cy),
                start_angle: sweep_angle_to_radians(start_angle),
                end_angle: sweep_angle_to_radians(end_angle),
            },
            stops,
            extend,
        }),
    });
}

// =========================================================================
// ColorLine sampling
// =========================================================================

/// Resolves every stop on a `ColorLine` against the active CPAL palette
/// and (when present) the var store. The stops are returned in the
/// order they appear on the line.
fn resolve_stops(
    ctx: &EvalCtx<'_, '_>,
    color_line: ColorLine<'_>,
    _var_base: Option<VarIndexBase>,
) -> Vec<ColorStop> {
    // VarColorLine carries a per-stop `varIndexBase`; non-var lines
    // surface `u32::MAX` for that field, which the delta lookup
    // collapses to a zero contribution. We therefore use the same
    // code path for both forms.
    let mut out = Vec::with_capacity(color_line.len() as usize);
    for (stop, stop_var) in color_line.stops_variable() {
        let (d_offset, d_alpha) = ctx.deltas.stop(stop_var);
        let (color, is_foreground) = ctx
            .palette
            .resolve(stop.palette_index, stop.alpha + d_alpha);
        out.push(ColorStop {
            offset: stop.stop_offset + d_offset,
            color,
            is_foreground,
        });
    }
    out
}

// =========================================================================
// Variation-store helpers
// =========================================================================

/// Raw delta for a paint field; see [`Deltas::raw`].
fn var_delta(ctx: &EvalCtx<'_, '_>, var_index_base: VarIndexBase, field_index: u16) -> f32 {
    ctx.deltas.raw(var_index_base, field_index)
}

/// Delta for an F2DOT14 paint field (alpha, scale, angle). The IVS
/// stores deltas in the field's own units, so a raw 8192 is 0.5.
fn var_delta_f2dot14(ctx: &EvalCtx<'_, '_>, var_index_base: VarIndexBase, field_index: u16) -> f32 {
    ctx.deltas.f2dot14(var_index_base, field_index)
}

/// Delta for a 16.16 Fixed paint field (`VarAffine2x3`).
fn var_delta_fixed(ctx: &EvalCtx<'_, '_>, var_index_base: VarIndexBase, field_index: u16) -> f32 {
    ctx.deltas.fixed(var_index_base, field_index)
}

/// Helper widening an `Fword` (i16 design-unit coord) to f32.
#[inline]
fn f(v: Fword) -> f32 {
    v as f32
}
