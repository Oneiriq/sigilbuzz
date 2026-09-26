//! Composite glyph flattening: component records, their transforms
//! and anchor-mode point matching.

use super::{
    Contour, FlatGlyph, Glyf, PhantomMetrics, Transform, COMP_ARGS_ARE_XY_VALUES,
    COMP_ARG_1_AND_2_ARE_WORDS, COMP_MORE_COMPONENTS, COMP_ROUND_XY_TO_GRID,
    COMP_SCALED_COMPONENT_OFFSET, COMP_UNSCALED_COMPONENT_OFFSET, COMP_USE_MY_METRICS,
    COMP_WE_HAVE_AN_X_AND_Y_SCALE, COMP_WE_HAVE_A_SCALE, COMP_WE_HAVE_A_TWO_BY_TWO,
};
use crate::error::Result;
use crate::tables::loca::Loca;
use crate::tables::parse::Reader;

impl<'a> Glyf<'a> {
    pub(super) fn flatten_composite(
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
                yx = r.read_f2dot14()?; // scale01: y' coefficient on x
                xy = r.read_f2dot14()?; // scale10: x' coefficient on y
                yy = r.read_f2dot14()?;
            }

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
                        // which is the same frame as the points already in
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
                    // through. Match the historic behavior of skipping
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
        // TT hints so we stop here. The caller already has the
        // flat outline.
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
