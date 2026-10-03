//! Composite glyph flattening: component records, their transforms
//! and anchor-mode point matching.

use alloc::vec::Vec;

use super::{
    FlatGlyph, FlattenBudget, FlattenCtx, Glyf, Transform, COMP_ARGS_ARE_XY_VALUES,
    COMP_ARG_1_AND_2_ARE_WORDS, COMP_MORE_COMPONENTS, COMP_SCALED_COMPONENT_OFFSET,
    COMP_UNSCALED_COMPONENT_OFFSET, COMP_USE_MY_METRICS, COMP_WE_HAVE_AN_X_AND_Y_SCALE,
    COMP_WE_HAVE_A_SCALE, COMP_WE_HAVE_A_TWO_BY_TWO,
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
    /// `arg1`: the x offset, or in anchor mode the point to match, an
    /// index into the walk's running point list.
    arg1: i32,
    /// `arg2`: the y offset, or in anchor mode the component's point
    /// to put on it.
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

    /// Places the component's `points`, in its glyph's frame, in the
    /// composite's: through the matrix and moved by `offset`, the
    /// component's gvar point with its delta, HarfBuzz's
    /// `transform_points`. A scaled offset moves the points before the
    /// matrix, an unscaled one after.
    fn place(&self, points: &mut [(f32, f32)], offset: (f32, f32)) {
        let transform = |points: &mut [(f32, f32)]| {
            if !self.matrix.is_identity() {
                for p in points.iter_mut() {
                    *p = self.matrix.apply(p.0, p.1);
                }
            }
        };
        let translate = |points: &mut [(f32, f32)]| {
            for p in points.iter_mut() {
                p.0 += offset.0;
                p.1 += offset.1;
            }
        };
        if self.scales_offset() {
            translate(points);
            transform(points);
        } else {
            transform(points);
            translate(points);
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

        // Arg width is flag-driven, and so is the sign: offsets are
        // signed, anchor point numbers unsigned, as HarfBuzz reads
        // them (`get_anchor_points`).
        let words = flags & COMP_ARG_1_AND_2_ARE_WORDS != 0;
        let (arg1, arg2): (i32, i32) = match (words, flags & COMP_ARGS_ARE_XY_VALUES != 0) {
            (true, true) => (i32::from(r.read_i16()?), i32::from(r.read_i16()?)),
            (true, false) => (i32::from(r.read_u16()?), i32::from(r.read_u16()?)),
            (false, true) => (i32::from(r.read_i8()?), i32::from(r.read_i8()?)),
            (false, false) => (i32::from(r.read_u8()?), i32::from(r.read_u8()?)),
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
    /// Appends the composite glyph `glyph`, a reader past its header,
    /// its glyph id, and its header's `xMin` and `yMax`, to `out`, as
    /// HarfBuzz's `Glyph::get_points` does for a composite:
    ///
    /// - Each component is walked onto the end of `out`, in its own
    ///   frame, its phantom points last (see [`Glyf::flatten`]).
    /// - Its points, phantom points included, go through its matrix
    ///   and move by its offset plus the composite's `gvar` delta for
    ///   it (an anchored component's offset is its delta alone).
    /// - An anchored component then moves so that its point `arg2`
    ///   lands on point `arg1` of the walk's running point list: every
    ///   point placed so far in the whole walk, the component's own
    ///   points and phantom points included. HarfBuzz indexes that list,
    ///   not the composite's own points; an index past either end skips
    ///   the move. Without metrics no phantom point can be matched.
    /// - The component's phantom points are dropped, after a
    ///   `USE_MY_METRICS` component has handed its unplaced ones to the
    ///   composite.
    ///
    /// The composite's own phantom points, moved by its deltas, go last.
    /// A component that would close a cycle is skipped where HarfBuzz's
    /// decycler skips it (see [`Glyf::phantom_walk`]).
    pub(super) fn flatten_composite(
        &self,
        glyph: (&mut Reader<'_>, u16, (i16, i16)),
        cx: &FlattenCtx<'_>,
        out: &mut FlatGlyph,
        depth: u8,
        budget: &mut FlattenBudget,
        path: &mut Vec<u16>,
    ) -> Result<()> {
        let (r, glyph_id, header) = glyph;
        let components = read_components(r)?;

        // A composite's gvar points are its components, one each,
        // then the phantom points. Nothing is inferred: no point sits
        // on a contour.
        let deltas = match cx.var {
            Some(var) => {
                let points: Vec<(i32, i32)> =
                    components.iter().map(Component::gvar_point).collect();
                var.gvar.glyph_point_deltas_with(
                    glyph_id,
                    var.coords,
                    &points,
                    &[],
                    &mut budget.work,
                )?
            }
            None => Vec::new(),
        };
        let mut own_deltas = [(0.0, 0.0); 4];
        for (slot, d) in own_deltas
            .iter_mut()
            .zip(deltas.iter().skip(components.len()))
        {
            *slot = *d;
        }
        let mut phantoms = cx.phantoms(glyph_id, header, &own_deltas);

        let node = path.len();
        path.push(glyph_id);
        for (index, c) in components.iter().enumerate() {
            path[node] = c.glyph_id;
            if node > 0 && path[node / 2] == c.glyph_id {
                continue;
            }
            let start = out.points.len();
            self.flatten(cx, c.glyph_id, None, out, depth + 1, budget, path)?;
            let end = out.points.len();
            let Some(placed) = out.points.get_mut(start..) else {
                continue;
            };
            if c.flags & COMP_USE_MY_METRICS != 0 {
                if let Some(last) = placed.get(placed.len().saturating_sub(4)..) {
                    for (p, q) in phantoms.iter_mut().zip(last) {
                        *p = *q;
                    }
                }
            }
            let (dx, dy) = deltas.get(index).copied().unwrap_or((0.0, 0.0));
            let (ox, oy) = c.gvar_point();
            c.place(placed, (ox as f32 + dx, oy as f32 + dy));
            if c.is_anchored() {
                let (to, from) = (c.arg1 as usize, c.arg2 as usize);
                let count = end - start;
                // The last four points of the list, and of the
                // component, are its phantom points.
                let phantom = |i: usize, len: usize| i + 4 >= len;
                let known = cx.metrics.is_some() || !(phantom(to, end) || phantom(from, count));
                if let (true, Some(&(ax, ay)), Some(&(bx, by))) =
                    (known, out.points.get(to), out.points.get(start + from))
                {
                    if from < count {
                        let (mx, my) = (ax - bx, ay - by);
                        for p in &mut out.points[start..] {
                            p.0 += mx;
                            p.1 += my;
                        }
                    }
                }
            }
            out.pop_phantoms();
        }
        path.truncate(node);
        out.push_phantoms(phantoms);
        Ok(())
    }
}
