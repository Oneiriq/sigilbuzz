//! Composite glyph flattening: component records, their transforms
//! and anchor-mode point matching.

use alloc::vec::Vec;

use super::{
    Contour, FlatGlyph, FlattenBudget, FlattenCtx, Glyf, Transform, COMP_ARGS_ARE_XY_VALUES,
    COMP_ARG_1_AND_2_ARE_WORDS, COMP_MORE_COMPONENTS, COMP_SCALED_COMPONENT_OFFSET,
    COMP_UNSCALED_COMPONENT_OFFSET, COMP_WE_HAVE_AN_X_AND_Y_SCALE, COMP_WE_HAVE_A_SCALE,
    COMP_WE_HAVE_A_TWO_BY_TWO,
};
use crate::error::Result;
use crate::tables::parse::Reader;

/// One component record of a composite glyph.
#[derive(Debug, Clone, Copy)]
pub(super) struct Component {
    /// The record's flags.
    pub(super) flags: u16,
    /// The glyph the component draws.
    pub(super) glyph_id: u16,
    /// `arg1`: the x offset, or in anchor mode the parent's point.
    arg1: i32,
    /// `arg2`: the y offset, or in anchor mode the component's point.
    arg2: i32,
    /// The component's 2x2 matrix, with no translation.
    matrix: Transform,
}

impl Component {
    /// True when `arg1` and `arg2` are point numbers to match rather
    /// than an offset (`ARGS_ARE_XY_VALUES` clear).
    const fn is_anchored(&self) -> bool {
        self.flags & COMP_ARGS_ARE_XY_VALUES == 0
    }

    /// True when the offset goes through the component's matrix
    /// (`SCALED_COMPONENT_OFFSET` set and `UNSCALED_COMPONENT_OFFSET`
    /// clear), HarfBuzz's `scaled_offsets`.
    const fn scales_offset(&self) -> bool {
        self.flags & (COMP_SCALED_COMPONENT_OFFSET | COMP_UNSCALED_COMPONENT_OFFSET)
            == COMP_SCALED_COMPONENT_OFFSET
    }

    /// The component's gvar point: its offset, or the origin for an
    /// anchored component.
    const fn gvar_point(&self) -> (i32, i32) {
        if self.is_anchored() {
            (0, 0)
        } else {
            (self.arg1, self.arg2)
        }
    }
}

/// Reads the component records of a composite glyph, starting at `r`
/// just past the glyph header.
pub(super) fn read_components(r: &mut Reader<'_>) -> Result<Vec<Component>> {
    let mut out = Vec::new();
    loop {
        let flags = r.read_u16()?;
        let glyph_id = r.read_u16()?;

        // Arg width is flag-driven. We read the raw values first
        // and decide later whether they are xy offsets or anchor
        // point indices.
        let (arg1, arg2): (i32, i32) = if flags & COMP_ARG_1_AND_2_ARE_WORDS != 0 {
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
        let mut matrix = Transform::identity();
        if flags & COMP_WE_HAVE_A_SCALE != 0 {
            let s = r.read_f2dot14()?;
            matrix.xx = s;
            matrix.yy = s;
        } else if flags & COMP_WE_HAVE_AN_X_AND_Y_SCALE != 0 {
            matrix.xx = r.read_f2dot14()?;
            matrix.yy = r.read_f2dot14()?;
        } else if flags & COMP_WE_HAVE_A_TWO_BY_TWO != 0 {
            matrix.xx = r.read_f2dot14()?;
            matrix.yx = r.read_f2dot14()?; // scale01: y' coefficient on x
            matrix.xy = r.read_f2dot14()?; // scale10: x' coefficient on y
            matrix.yy = r.read_f2dot14()?;
        }
        out.push(Component {
            flags,
            glyph_id,
            arg1,
            arg2,
            matrix,
        });
        if flags & COMP_MORE_COMPONENTS == 0 {
            break;
        }
    }
    // If WE_HAVE_INSTRUCTIONS is set the composite ends with a
    // u16 instruction count + that many bytes. We don't execute
    // TT hints so we stop here.
    Ok(out)
}

impl Glyf<'_> {
    /// Flattens the composite glyph `parent_glyph_id`, whose component
    /// records start at `r`, into `out`.
    #[allow(clippy::too_many_arguments)] // `flatten`'s parameters plus the reader.
    pub(super) fn flatten_composite(
        &self,
        r: &mut Reader<'_>,
        cx: &FlattenCtx<'_>,
        parent_glyph_id: u16,
        parent_tf: &Transform,
        out: &mut FlatGlyph,
        depth: u8,
        budget: &mut FlattenBudget,
    ) -> Result<()> {
        let components = read_components(r)?;

        // A composite's gvar points are its components, one each,
        // then the phantom points. Nothing is inferred: no point sits
        // on a contour.
        let deltas = match cx.var {
            Some(var) => {
                let points: Vec<(i32, i32)> =
                    components.iter().map(Component::gvar_point).collect();
                var.gvar.glyph_point_deltas_with(
                    parent_glyph_id,
                    var.coords,
                    &points,
                    &[],
                    &mut budget.work,
                )?
            }
            None => Vec::new(),
        };

        for (index, c) in components.iter().enumerate() {
            let (dx, dy) = deltas.get(index).copied().unwrap_or((0.0, 0.0));

            // Snapshot the parent's point count *before* this
            // component is laid down. Anchor-mode arg1 indexes into
            // exactly those points (the parent contour points already
            // emitted by previous siblings, transformed into the
            // composite's frame).
            let parent_point_count = out.points.len();

            // First flatten the child into a scratch buffer with the
            // 2x2 applied but no translation yet. Both anchor-mode
            // and xy-mode branches need access to the child's
            // pre-translation absolute points.
            let child_combined = parent_tf.compose(&c.matrix);
            let mut child_flat = FlatGlyph::default();
            self.flatten(
                cx,
                c.glyph_id,
                None,
                &child_combined,
                &mut child_flat,
                depth + 1,
                budget,
            )?;

            // The offset with its delta, in the composite's frame:
            // HarfBuzz's `transform_points` translates by it before
            // the matrix when the offset is scaled, after it otherwise.
            // An anchored component's offset is its delta alone.
            let (ox, oy) = c.gvar_point();
            let (lx, ly) = (ox as f32 + dx, oy as f32 + dy);
            let (lx, ly) = if c.scales_offset() {
                c.matrix.apply(lx, ly)
            } else {
                (lx, ly)
            };
            // The translation lives in the parent's coordinate frame,
            // so route it through the parent's linear part before
            // applying it on top of the already-transformed child
            // points.
            let offset = (
                parent_tf.xx * lx + parent_tf.xy * ly,
                parent_tf.yx * lx + parent_tf.yy * ly,
            );

            // Resolve the translation. Anchor-mode (ARGS_ARE_XY_VALUES
            // clear) computes `parent[arg1] - child[arg2]` so the
            // child's anchor point lands on the parent's, which cancels
            // the component's delta. Otherwise the offset applies.
            let (tx, ty) = if c.is_anchored() {
                let p_idx = c.arg1 as usize;
                let c_idx = c.arg2 as usize;
                let parent_anchor = resolve_anchor_point(
                    p_idx,
                    parent_point_count,
                    &out.points,
                    || -> Result<Option<(f32, f32)>> {
                        let Some(m) = cx.metrics else { return Ok(None) };
                        let phantom_idx = p_idx - parent_point_count;
                        if phantom_idx >= 4 {
                            return Ok(None);
                        }
                        let pp = self.varied_phantoms(cx, m, parent_glyph_id, depth, budget)?;
                        let (px, py) = pp[phantom_idx];
                        // Parent's phantoms live in the parent's frame
                        // which is the same frame as the points already in
                        // `out.points`, which were transformed by
                        // `parent_tf` on insertion. Apply the same
                        // transform so the subtraction below cancels
                        // out cleanly.
                        Ok(Some(parent_tf.apply(px, py)))
                    },
                )?;
                let child_point_count = child_flat.points.len();
                let child_anchor = resolve_anchor_point(
                    c_idx,
                    child_point_count,
                    &child_flat.points,
                    || -> Result<Option<(f32, f32)>> {
                        let Some(m) = cx.metrics else { return Ok(None) };
                        let phantom_idx = c_idx - child_point_count;
                        if phantom_idx >= 4 {
                            return Ok(None);
                        }
                        let pp = self.varied_phantoms(cx, m, c.glyph_id, depth + 1, budget)?;
                        let (cx_, cy_) = pp[phantom_idx];
                        // Child's phantoms share the frame of the
                        // freshly-flattened child points, which had
                        // `child_combined` baked in.
                        Ok(Some(child_combined.apply(cx_, cy_)))
                    },
                )?;
                match (parent_anchor, child_anchor) {
                    (Some((px, py)), Some((cx_, cy_))) => (px - cx_, py - cy_),
                    // Out-of-range phantom index, or no metrics passed
                    // through. Match HarfBuzz, which skips the anchor
                    // translation and keeps the component's delta.
                    _ => offset,
                }
            } else {
                offset
            };

            // Splice the child into the parent. Contour ends shift by
            // the parent's running point count; coordinates shift by
            // the resolved translation; flags follow each point.
            let point_offset = out.points.len();
            debug_assert_eq!(child_flat.points.len(), child_flat.flags.len());
            for (i, &(px, py)) in child_flat.points.iter().enumerate() {
                out.points.push((px + tx, py + ty));
                out.flags.push(child_flat.flags[i]);
            }
            for contour in &child_flat.contours {
                out.contours.push(Contour {
                    start: contour.start + point_offset,
                    end: contour.end + point_offset,
                });
            }
        }
        Ok(())
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
