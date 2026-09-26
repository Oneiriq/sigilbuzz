//! Paint-tree walker.
//!
//! [`evaluate`] walks the COLRv1 paint DAG rooted at a base glyph and
//! returns a flat [`DrawCmd`] sequence. Transforms compose, gradients
//! resolve to f32 color stops, and `PaintComposite` nodes wrap their
//! child output in [`DrawCmd::PushLayer`] / [`DrawCmd::PopLayer`].
//!
//! Determinism: the output for a given (face, gid, coords) tuple is
//! byte-for-byte stable. The walker is depth-first, left-to-right
//! (source then backdrop for composites, in spec order), and never
//! reorders.
//!
//! Robustness: malformed offsets, unknown formats, reference cycles,
//! and paint graphs that expand past a fixed work budget all truncate
//! the output. The walker never panics.

use alloc::vec::Vec;

use sigilbuzz::tables::colr::{
    ColorLine, Colr, ColrPaint, CompositeMode, F2Dot14, Fword, PaintOffset, VarIndexBase,
};
use sigilbuzz::tables::cpal::Cpal;
use sigilbuzz::tables::variation_store::ItemVariationStore;
use sigilbuzz::Face;

use crate::color::Color;
use crate::gradient::{ColorStop, Extend, Gradient, GradientKind};
use crate::transform::{angle_to_radians, Transform2D};

/// Glyph-id alias. Mirrors the on-disk u16 used throughout sigilbuzz.
pub type GlyphId = u16;

/// CPAL palette that [`evaluate`] and [`evaluate_at_coords`] resolve
/// colors against. Palette 0 is the font's default.
const DEFAULT_PALETTE_INDEX: u16 = 0;

/// Sentinel palette index meaning "use the foreground text color".
/// COLRv1 reserves `0xFFFF` for this; the evaluator resolves it to
/// opaque white so renderers can apply their own foreground overlay.
const FOREGROUND_PALETTE_INDEX: u16 = 0xFFFF;

/// Maximum DAG-walk depth. Defensive cap above and beyond the
/// visited-set cycle check. A deeply linear chain still exits before
/// blowing the stack.
const MAX_DEPTH: usize = 64;

/// Maximum work one evaluation may do, counted as paint nodes visited
/// plus color stops resolved. The paint graph is a DAG, so a node
/// reached from several parents is walked once per parent. A
/// `PaintColrLayers` or `PaintComposite` whose children point back at
/// shared nodes can therefore expand exponentially within
/// [`MAX_DEPTH`]. The budget bounds both run time and the size of the
/// returned command list. Real color glyphs stay far below it.
const MAX_WORK: usize = 1 << 18;

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
    Solid(Color),
    /// Resolved gradient: palette indices already substituted for
    /// f32 RGBA, alpha multiplied in, geometry in design-unit space.
    Gradient(Gradient),
}

/// Walks `face`'s COLRv1 paint tree for `gid`, returning the draw
/// commands required to render the color glyph. Equivalent to
/// [`evaluate_at_coords`] with empty `coords`.
#[must_use]
pub fn evaluate(face: &Face<'_>, gid: GlyphId) -> Vec<DrawCmd> {
    evaluate_at_coords(face, gid, &[])
}

/// Same as [`evaluate`] but applies variation deltas from `coords` to
/// every `PaintVar*` node visited. `coords` is the normalized axis
/// vector, the same shape sigilbuzz's
/// [`Face::glyph_outline_at_coords`](sigilbuzz::Face::glyph_outline_at_coords)
/// accepts. An empty slice is the static (no-deltas) path.
#[must_use]
pub fn evaluate_at_coords(face: &Face<'_>, gid: GlyphId, coords: &[f32]) -> Vec<DrawCmd> {
    evaluate_with_palette(face, gid, coords, DEFAULT_PALETTE_INDEX)
}

/// Same as [`evaluate_at_coords`] but resolves colors against CPAL
/// palette `palette`, so a caller can pick one of the font's
/// alternate palettes (a dark theme, for example). Colors in a
/// palette the font does not have resolve to transparent, the same
/// as a missing palette entry.
///
/// ```no_run
/// use sigilbuzz::Face;
/// use sigilbuzz_paint::evaluate_with_palette;
///
/// # fn demo(face: &Face<'_>) {
/// // Glyph 42 in the font's second palette, at the default instance.
/// let cmds = evaluate_with_palette(face, 42, &[], 1);
/// # let _ = cmds;
/// # }
/// ```
#[must_use]
pub fn evaluate_with_palette(
    face: &Face<'_>,
    gid: GlyphId,
    coords: &[f32],
    palette: u16,
) -> Vec<DrawCmd> {
    let mut out = Vec::new();
    let Ok(Some(colr)) = face.colr() else {
        return out;
    };
    let cpal = face.cpal().ok().flatten();
    let var_store = resolve_var_store(&colr);
    let Some(root) = colr.paint(gid) else {
        return out;
    };

    let index_map = resolve_index_map(&colr);
    let mut ctx = EvalCtx {
        colr: &colr,
        cpal: cpal.as_ref(),
        palette,
        var_store: var_store.as_ref(),
        index_map,
        coords,
        visited: Vec::new(),
        work_left: MAX_WORK,
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
    cpal: Option<&'b Cpal<'a>>,
    /// CPAL palette that colors resolve against.
    palette: u16,
    var_store: Option<&'b ItemVariationStore<'a>>,
    /// Optional DeltaSetIndexMap from the COLR header. It redirects
    /// a paint's `var_index_base + field_index` through an
    /// indirection table before it hits the IVS. Without a map the
    /// index splits directly into an `(outer, inner)` pair.
    index_map: Option<DeltaSetIndexMap<'a>>,
    coords: &'b [f32],
    /// Glyph ids whose paint trees are currently on the walk stack.
    /// `PaintColrGlyph` checks this before recursing.
    visited: Vec<GlyphId>,
    /// Remaining work budget. See [`MAX_WORK`].
    work_left: usize,
    out: &'b mut Vec<DrawCmd>,
}

impl EvalCtx<'_, '_> {
    /// Charges `units` against the work budget. Returns `false` and
    /// empties the budget when not enough is left, which stops the
    /// rest of the walk.
    fn charge(&mut self, units: usize) -> bool {
        if let Some(left) = self.work_left.checked_sub(units) {
            self.work_left = left;
            true
        } else {
            self.work_left = 0;
            false
        }
    }
}

/// Resolves the var store referenced by the COLRv1 header, if any.
/// A missing or malformed offset yields `None`; the walker treats that
/// the same as "no variation deltas".
fn resolve_var_store<'a>(colr: &Colr<'a>) -> Option<ItemVariationStore<'a>> {
    let off = colr.var_store_offset()?;
    let data = colr.data();
    let start = off as usize;
    if start >= data.len() {
        return None;
    }
    ItemVariationStore::parse(&data[start..]).ok()
}

/// Resolves the DeltaSetIndexMap referenced by the COLRv1 header, if
/// any. A missing or malformed map yields `None`, and variation
/// indices then map directly to `(outer, inner)` pairs.
fn resolve_index_map<'a>(colr: &Colr<'a>) -> Option<DeltaSetIndexMap<'a>> {
    let off = colr.var_index_map_offset()?;
    DeltaSetIndexMap::parse(colr.data(), usize::try_from(off).ok()?)
}

/// Walks one paint node. `xform` is the transform inherited from the
/// chain of ancestor transforms; `depth` is the current recursion
/// depth used for the cycle-bypass safety net. Every call charges one
/// unit of the work budget, including calls cut off by the depth cap,
/// so a wide node just above the cap still pays for its children.
fn walk_paint(ctx: &mut EvalCtx<'_, '_>, paint: ColrPaint<'_>, xform: Transform2D, depth: usize) {
    if !ctx.charge(1) || depth >= MAX_DEPTH {
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
            let dsa = var_delta(ctx, var_index_base, 2);
            let dea = var_delta(ctx, var_index_base, 3);
            emit_sweep_gradient(
                ctx,
                color_line,
                f(center_x) + dcx,
                f(center_y) + dcy,
                start_angle + dsa,
                end_angle + dea,
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
            let m = Transform2D {
                xx: xx + var_delta(ctx, var_index_base, 0),
                yx: yx + var_delta(ctx, var_index_base, 1),
                xy: xy + var_delta(ctx, var_index_base, 2),
                yy: yy + var_delta(ctx, var_index_base, 3),
                dx: dx + var_delta(ctx, var_index_base, 4),
                dy: dy + var_delta(ctx, var_index_base, 5),
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
            let dsx = var_delta(ctx, var_index_base, 0);
            let dsy = var_delta(ctx, var_index_base, 1);
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
            let dsx = var_delta(ctx, var_index_base, 0);
            let dsy = var_delta(ctx, var_index_base, 1);
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
            let ds = var_delta(ctx, var_index_base, 0);
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
            let ds = var_delta(ctx, var_index_base, 0);
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
            let da = var_delta(ctx, var_index_base, 0);
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
            let da = var_delta(ctx, var_index_base, 0);
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
            let dxa = var_delta(ctx, var_index_base, 0);
            let dya = var_delta(ctx, var_index_base, 1);
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
            let dxa = var_delta(ctx, var_index_base, 0);
            let dya = var_delta(ctx, var_index_base, 1);
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
    let color =
        resolve_palette_color(ctx.cpal, ctx.palette, palette_index).with_alpha_multiplied(alpha);
    ctx.out.push(DrawCmd::FillGlyph {
        gid: 0,
        transform: xform,
        paint: PaintSource::Solid(color),
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
    xform: Transform2D,
) {
    let Some(stops) = resolve_stops(ctx, color_line) else {
        return;
    };
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
    xform: Transform2D,
) {
    let Some(stops) = resolve_stops(ctx, color_line) else {
        return;
    };
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
    xform: Transform2D,
) {
    let Some(stops) = resolve_stops(ctx, color_line) else {
        return;
    };
    let extend: Extend = color_line.extend.into();
    ctx.out.push(DrawCmd::FillGlyph {
        gid: 0,
        transform: xform,
        paint: PaintSource::Gradient(Gradient {
            kind: GradientKind::Sweep {
                center: (cx, cy),
                start_angle: angle_to_radians(start_angle),
                end_angle: angle_to_radians(end_angle),
            },
            stops,
            extend,
        }),
    });
}

// =========================================================================
// CPAL resolution + ColorLine sampling
// =========================================================================

/// Resolves a CPAL palette entry to a float-channel color. Falls back
/// to opaque white for the `0xFFFF` foreground sentinel and to fully
/// transparent for any other lookup miss. Never panics.
fn resolve_palette_color(cpal: Option<&Cpal<'_>>, palette: u16, palette_index: u16) -> Color {
    if palette_index == FOREGROUND_PALETTE_INDEX {
        return Color::new(1.0, 1.0, 1.0, 1.0);
    }
    let Some(cpal) = cpal else {
        return Color::TRANSPARENT;
    };
    cpal.color(palette, palette_index)
        .map(Color::from_cpal)
        .unwrap_or(Color::TRANSPARENT)
}

/// Resolves every stop on a `ColorLine` against the active CPAL palette
/// and (when present) the var store. The stops are returned in the
/// order they appear on the line. Each stop charges one unit of the
/// work budget. Returns `None` when the budget cannot cover the line.
fn resolve_stops(ctx: &mut EvalCtx<'_, '_>, color_line: ColorLine<'_>) -> Option<Vec<ColorStop>> {
    if !ctx.charge(usize::from(color_line.len())) {
        return None;
    }
    // VarColorLine carries a per-stop `varIndexBase`. Non-var lines
    // surface `u32::MAX` for that field, which the check below treats
    // as "no deltas". Both forms therefore share one code path.
    let mut out = Vec::with_capacity(usize::from(color_line.len()));
    let coords = ctx.coords;
    let var_store = ctx.var_store;
    let cpal = ctx.cpal;
    let palette = ctx.palette;

    for (stop, stop_var) in color_line.stops_variable() {
        let mut offset = stop.stop_offset;
        let mut alpha = stop.alpha;
        if let Some(store) = var_store {
            if stop_var != u32::MAX && !coords.is_empty() {
                // Stops route through the same DeltaSetIndexMap as
                // paint fields when one is present: the per-stop
                // varIndexBase is treated as a flat index into the
                // map that resolves to (outer, inner) pairs.
                let (off_outer, off_inner) = match ctx.index_map.as_ref() {
                    Some(map) => match map.lookup(stop_var) {
                        Some(pair) => pair,
                        None => continue,
                    },
                    None => ((stop_var >> 16) as u16, stop_var as u16),
                };
                let (a_outer, a_inner) = match ctx.index_map.as_ref() {
                    Some(map) => match map.lookup(stop_var.wrapping_add(1)) {
                        Some(pair) => pair,
                        None => (off_outer, off_inner.wrapping_add(1)),
                    },
                    None => (off_outer, off_inner.wrapping_add(1)),
                };
                // Stop offset and per-stop alpha are both F2DOT14, so
                // the IVS' integer delta needs the same /16384 scale
                // we apply to PaintVarSolid alpha.
                offset += store.delta(off_outer, off_inner, coords) / 16384.0;
                alpha += store.delta(a_outer, a_inner, coords) / 16384.0;
            }
        }
        let color =
            resolve_palette_color(cpal, palette, stop.palette_index).with_alpha_multiplied(alpha);
        out.push(ColorStop { offset, color });
    }
    Some(out)
}

// =========================================================================
// Variation-store helpers
// =========================================================================

/// Looks up a variation delta for the `field_index`-th variable field
/// of a paint that records `var_index_base` as its anchor.
///
/// The COLRv1 spec resolves variable fields through one of two paths:
///
/// 1. **No indirection (default).** `var_index_base + field_index` is
///    the on-disk `(outer, inner)` pair; the IVS row at that pair is
///    the delta source. This is what fonts get when they don't carry
///    a `varIndexMap`.
/// 2. **DeltaSetIndexMap indirection.** When the font supplies a
///    `varIndexMap`, `var_index_base + field_index` is treated as a
///    *flat index* into the map; the map yields the actual
///    `(outer, inner)` pair, which then resolves through the IVS.
///
/// Returns `0.0` when the var store is absent, `var_index_base` is
/// the no-deltas sentinel, or `coords` is empty.
fn var_delta(ctx: &EvalCtx<'_, '_>, var_index_base: VarIndexBase, field_index: u16) -> f32 {
    if var_index_base == VarIndexBase::MAX || ctx.coords.is_empty() {
        return 0.0;
    }
    let Some(store) = ctx.var_store else {
        return 0.0;
    };
    let flat_index = var_index_base.wrapping_add(u32::from(field_index));
    let (outer, inner) = if let Some(map) = ctx.index_map.as_ref() {
        match map.lookup(flat_index) {
            Some(pair) => pair,
            None => return 0.0,
        }
    } else {
        ((flat_index >> 16) as u16, flat_index as u16)
    };
    store.delta(outer, inner, ctx.coords)
}

/// Variant of [`var_delta`] for fields whose base values live in
/// F2DOT14 fixed-point. The OpenType variation spec requires deltas
/// to share the same units as the field they patch, so an IVS that
/// stores a raw `int16` of `8192` represents a delta of `0.5` in
/// F2DOT14 space. The sigilbuzz `ItemVariationStore::delta` returns
/// that integer as `f32`; this helper finishes the conversion by
/// dividing by 16384 so the caller can add directly to an
/// already-scaled F2DOT14 value.
fn var_delta_f2dot14(ctx: &EvalCtx<'_, '_>, var_index_base: VarIndexBase, field_index: u16) -> f32 {
    var_delta(ctx, var_index_base, field_index) / 16384.0
}

/// Helper widening an `Fword` (i16 design-unit coord) to f32.
#[inline]
fn f(v: Fword) -> f32 {
    v as f32
}

// =========================================================================
// DeltaSetIndexMap
// =========================================================================

/// Borrowed view over a `DeltaSetIndexMap`, a flat array of packed
/// `(outer, inner)` pairs that COLRv1 fonts use to share IVS rows
/// between many paint records. The on-disk layout matches the
/// `DeltaSetIndexMapFormat0/1` records the OpenType spec defines for
/// HVAR / VVAR / GDEF / COLR.
///
/// ```text
///   u8   format          // 0 = u16 mapCount, 1 = u32 mapCount
///   u8   entryFormat     // bits 4-5: bytes/entry - 1; bits 0-3: inner-bits - 1
///   u16  mapCount        // (or u32 when format == 1)
///   u8[] entries         // (mapCount * bytesPerEntry) packed pairs
/// ```
///
/// Lookups clamp out-of-range indices to the last entry, mirroring
/// the spec's "the index is clamped to mapCount - 1 if it is greater
/// than or equal to mapCount" rule.
#[derive(Debug, Clone, Copy)]
struct DeltaSetIndexMap<'a> {
    /// Slice covering exactly the map's entry array.
    entries: &'a [u8],
    /// Bytes per entry (1..=4).
    entry_bytes: usize,
    /// Bit-width of the inner index (1..=16).
    inner_bits: u32,
    /// `(1 << inner_bits) - 1`, pre-computed for the hot path.
    inner_mask: u32,
    /// Number of (outer, inner) entries the map carries.
    map_count: u32,
}

impl<'a> DeltaSetIndexMap<'a> {
    /// Parses a DeltaSetIndexMap rooted at byte `start` of `data`.
    /// Returns `None` if the header is truncated or carries an
    /// unknown format.
    fn parse(data: &'a [u8], start: usize) -> Option<Self> {
        let header = data.get(start..)?;
        let (&[format, entry_format], rest) = header.split_first_chunk::<2>()?;
        let (map_count, rest) = match format {
            0 => {
                let (count, rest) = rest.split_first_chunk::<2>()?;
                (u32::from(u16::from_be_bytes(*count)), rest)
            }
            1 => {
                let (count, rest) = rest.split_first_chunk::<4>()?;
                (u32::from_be_bytes(*count), rest)
            }
            _ => return None,
        };
        let entry_bytes = ((entry_format >> 4) & 0x03) as usize + 1;
        let inner_bits = u32::from(entry_format & 0x0F) + 1;
        let total = usize::try_from(map_count).ok()?.checked_mul(entry_bytes)?;
        let entries = rest.get(..total)?;
        let inner_mask = (1u32 << inner_bits).wrapping_sub(1);
        Some(Self {
            entries,
            entry_bytes,
            inner_bits,
            inner_mask,
            map_count,
        })
    }

    /// Looks up the `(outer, inner)` pair at flat `index`, clamping
    /// to the last entry per spec. Returns `None` when the map is
    /// empty.
    fn lookup(&self, index: u32) -> Option<(u16, u16)> {
        if self.map_count == 0 {
            return None;
        }
        let idx = if index < self.map_count {
            index
        } else {
            self.map_count - 1
        } as usize;
        let off = idx.checked_mul(self.entry_bytes)?;
        let entry = self.entries.get(off..off.checked_add(self.entry_bytes)?)?;
        let raw = entry
            .iter()
            .fold(0u32, |acc, &byte| (acc << 8) | u32::from(byte));
        let inner = (raw & self.inner_mask) as u16;
        let outer = (raw >> self.inner_bits) as u16;
        Some((outer, inner))
    }
}
