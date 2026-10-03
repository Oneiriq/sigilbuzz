//! `Face` accessors for glyph outlines: `loca` / `glyf`, `CFF ` /
//! `CFF2`, glyph points and bounds, and the VARC-aware outline walk.

use alloc::vec::Vec;

use super::{round_f32_to_i16, Face};
use crate::error::{Error, Result};
use crate::tables::glyf::PhantomMetrics;
use crate::tables::outline::OutlineSink;
use crate::tables::{tag, Cff, Cff2, Glyf, GlyphBounds, Loca, Outline, PathOp};

impl<'a> Face<'a> {
    /// Parses the `loca` table. Pulls the offset format from `head`
    /// and the glyph count from `maxp`; both must be present.
    pub fn loca(&self) -> Result<Loca<'a>> {
        let head = self.head()?;
        let maxp = self.maxp()?;
        Loca::parse(
            self.table_bytes(tag::LOCA)?,
            head.index_to_loc_format,
            maxp.num_glyphs,
        )
    }

    /// Wraps the `glyf` table.
    pub fn glyf(&self) -> Result<Glyf<'a>> {
        Ok(Glyf::new(self.table_bytes(tag::GLYF)?))
    }

    /// Returns the glyph's raw points in glyf-natural order: contour
    /// points (on-curve + off-curve) followed by the four phantom
    /// points (pp1..pp4). Thin wrapper over [`Glyf::glyph_points`]:
    /// the heavy lifting (composite flattening, phantom synthesis)
    /// lives there; this method just plumbs `loca`, `hmtx`, and the
    /// optional `vmtx` through.
    ///
    /// Used by `kerx` format-4 action type 0, which references glyph
    /// points by index. Returns `Ok(None)` for glyphs without an
    /// outline (whitespace, missing) and for fonts that lack `glyf`
    /// entirely (CFF-only); the caller should treat the missing
    /// information as "drop the kern silently", the same conservative
    /// posture sigilbuzz uses for fmt-4 fall-through everywhere else.
    pub fn glyph_points(&self, glyph_id: u16) -> Result<Option<Vec<(i16, i16)>>> {
        if self.record(tag::GLYF).is_none() {
            return Ok(None);
        }
        let loca = self.loca()?;
        let glyf = self.glyf()?;
        let hmtx = self.hmtx()?;
        let vmtx = self.vmtx()?;
        glyf.glyph_points(&loca, glyph_id, &hmtx, vmtx.as_ref())
    }

    /// Returns the design-unit bounding box for `glyph_id`, or
    /// `Ok(None)` when the glyph has no outline (e.g. a space
    /// glyph). Requires both `loca` and `glyf`. Fonts that use CFF
    /// outlines instead will yield [`Error::MissingTable`] for
    /// `glyf`.
    pub fn glyph_bounds(&self, glyph_id: u16) -> Result<Option<GlyphBounds>> {
        let loca = self.loca()?;
        let glyf = self.glyf()?;
        glyf.bounds(&loca, glyph_id)
    }

    /// Returns the design-unit bounding box for `glyph_id` at the
    /// given normalized axis coords.
    ///
    /// When the font varies (`gvar` is present and some coord is not
    /// zero), the box is the extent of the varied outline's points,
    /// off-curve points included, each edge rounded half away from
    /// zero, as HarfBuzz computes glyph extents. A glyph whose varied
    /// outline has no points gets an all-zero box. Otherwise the box
    /// is the static one from [`Face::glyph_bounds`]. `num_contours`
    /// always comes from the glyph header.
    pub fn glyph_bounds_at_coords(
        &self,
        glyph_id: u16,
        coords: &[f32],
    ) -> Result<Option<GlyphBounds>> {
        let Some(base) = self.glyph_bounds(glyph_id)? else {
            return Ok(None);
        };
        if coords.iter().all(|&c| c == 0.0) {
            return Ok(Some(base));
        }
        let Some(gvar) = self.gvar()? else {
            return Ok(Some(base));
        };
        let loca = self.loca()?;
        let glyf = self.glyf()?;
        let hmtx = self.hmtx()?;
        let vmtx = self.vmtx()?;
        let metrics = PhantomMetrics {
            hmtx: &hmtx,
            vmtx: vmtx.as_ref(),
        };
        let mut points = PointBox::default();
        glyf.outline_at_coords(
            &loca,
            glyph_id,
            Some(&gvar),
            coords,
            Some(&metrics),
            &mut points,
        )?;
        Ok(Some(points.bounds(base.num_contours)))
    }

    /// Parses the `CFF ` (Compact Font Format 1) table.
    pub fn cff(&self) -> Result<Cff<'a>> {
        Cff::parse(self.table_bytes(tag::CFF1)?)
    }

    /// Parses the `CFF2` table if the font carries one. CFF2 is the
    /// variable-font flavor of CFF; static OTF fonts use plain
    /// `CFF `.
    pub fn cff2(&self) -> Result<Cff2<'a>> {
        Cff2::parse(self.table_bytes(tag::CFF2)?)
    }

    /// Returns the full contour outline for `glyph_id` as a flat
    /// list of [`crate::tables::PathOp`]s. Works for both TrueType
    /// (`glyf`) and CFF / CFF2 fonts; the backend is inferred from
    /// the tables the font carries.
    ///
    /// Composite glyphs are flattened: the caller never sees
    /// component references. Returns `Ok(None)` for glyphs with no
    /// outline (whitespace) or for glyph ids past the end of the
    /// font's outline table.
    pub fn glyph_outline(&self, glyph_id: u16) -> Result<Option<Outline>> {
        self.glyph_outline_at_coords(glyph_id, &[])
    }

    /// Like [`Face::glyph_outline`] but applies variable-font deltas
    /// for the given normalized axis coords. For TrueType outlines
    /// the deltas come from `gvar`; for CFF2 they come from the
    /// table's own Variation Store via the `blend` charstring
    /// operator. An empty `coords` slice is equivalent to the static
    /// outline and is the cheap path taken by [`Face::glyph_outline`].
    pub fn glyph_outline_at_coords(
        &self,
        glyph_id: u16,
        coords: &[f32],
    ) -> Result<Option<Outline>> {
        let mut budget = VarcBudget {
            components_left: MAX_VARC_COMPONENTS,
            ops_left: MAX_VARC_OPS,
        };
        self.glyph_outline_at_coords_inner(glyph_id, coords, 0, &mut budget)
    }

    /// Recursive entry point used by VARC composite resolution.
    /// `depth` caps recursion through nested VARC composites the
    /// same way [`Glyf::flatten`] caps `glyf` composites. `budget`
    /// caps the total work: depth alone still lets a glyph whose
    /// components share children expand exponentially.
    fn glyph_outline_at_coords_inner(
        &self,
        glyph_id: u16,
        coords: &[f32],
        depth: u8,
        budget: &mut VarcBudget,
    ) -> Result<Option<Outline>> {
        const MAX_VARC_DEPTH: u8 = 64;
        if depth > MAX_VARC_DEPTH {
            return Err(Error::Malformed {
                offset: 0,
                context: "VARC composite recursion exceeded cap",
            });
        }

        // VARC routing: if the font ships a VARC table that covers
        // this gid, recurse through the resolved components and apply
        // each component's affine to the child outline. Children
        // outside VARC's coverage fall through to the regular glyf /
        // CFF path with the component's effective coord vector.
        if let Some(varc) = self.varc()? {
            if varc.covers(glyph_id) {
                if let Some(composite) = varc.composite(glyph_id, coords) {
                    let mut out = Outline::new();
                    for comp in &composite.components {
                        budget.components_left =
                            budget
                                .components_left
                                .checked_sub(1)
                                .ok_or(Error::Malformed {
                                    offset: 0,
                                    context: "VARC composite exceeds component budget",
                                })?;
                        let child = self.glyph_outline_at_coords_inner(
                            comp.gid,
                            &comp.coords,
                            depth + 1,
                            budget,
                        )?;
                        if let Some(child) = child {
                            budget.ops_left = budget
                                .ops_left
                                .checked_sub(child.ops().len())
                                .ok_or(Error::Malformed {
                                    offset: 0,
                                    context: "VARC composite exceeds outline budget",
                                })?;
                            for op in child.ops() {
                                out.push(transform_path_op(*op, comp.transform));
                            }
                        }
                    }
                    return Ok(Some(out));
                }
            }
        }

        // CFF / CFF2 path: presence of `CFF2` wins over `CFF ` since
        // variable fonts ship only CFF2. `CFF ` is used only when the
        // font has no `glyf`. A font carrying both takes the TrueType
        // path below.
        if self.record(tag::CFF2).is_some() {
            let cff2 = self.cff2()?;
            let mut out = Outline::new();
            let drew = cff2.outline(glyph_id, coords, &mut out)?;
            return Ok(drew.then_some(out));
        }
        if self.record(tag::CFF1).is_some() && self.record(tag::GLYF).is_none() {
            let cff = self.cff()?;
            let mut out = Outline::new();
            let drew = cff.outline(glyph_id, &mut out)?;
            return Ok(drew.then_some(out));
        }

        // TrueType path.
        let loca = self.loca()?;
        let glyf = self.glyf()?;
        let mut out = Outline::new();

        // Phantom metrics let composite anchor-mode resolve indices
        // past the contour-point count (lsb / advance-width / tsb /
        // advance-height). hmtx is required by every TrueType font;
        // vmtx is optional and only horizontal-only fonts skip it.
        let hmtx = self.hmtx()?;
        let vmtx = self.vmtx()?;
        let metrics = PhantomMetrics {
            hmtx: &hmtx,
            vmtx: vmtx.as_ref(),
        };

        // gvar moves simple glyphs' points and composite glyphs'
        // components; `outline_at_coords` skips it at the default
        // instance.
        let gvar = if coords.is_empty() {
            None
        } else {
            self.gvar()?
        };
        let drew = glyf.outline_at_coords(
            &loca,
            glyph_id,
            gvar.as_ref(),
            coords,
            Some(&metrics),
            &mut out,
        )?;
        Ok(drew.then_some(out))
    }
}

/// Bounding box of every point an outline walk emits, off-curve
/// points included. Implied on-curve points lie between two emitted
/// points, so they never widen it.
struct PointBox {
    min: (f32, f32),
    max: (f32, f32),
}

impl Default for PointBox {
    fn default() -> Self {
        Self {
            min: (f32::INFINITY, f32::INFINITY),
            max: (f32::NEG_INFINITY, f32::NEG_INFINITY),
        }
    }
}

impl PointBox {
    fn add(&mut self, x: f32, y: f32) {
        self.min = (self.min.0.min(x), self.min.1.min(y));
        self.max = (self.max.0.max(x), self.max.1.max(y));
    }

    /// The box rounded half away from zero, or all zeros when no
    /// point was seen.
    fn bounds(&self, num_contours: i16) -> GlyphBounds {
        if self.min.0 > self.max.0 {
            return GlyphBounds {
                x_min: 0,
                y_min: 0,
                x_max: 0,
                y_max: 0,
                num_contours,
            };
        }
        GlyphBounds {
            x_min: round_f32_to_i16(self.min.0),
            y_min: round_f32_to_i16(self.min.1),
            x_max: round_f32_to_i16(self.max.0),
            y_max: round_f32_to_i16(self.max.1),
            num_contours,
        }
    }
}

impl OutlineSink for PointBox {
    fn move_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.add(cx, cy);
        self.add(x, y);
    }
    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.add(c1x, c1y);
        self.add(c2x, c2y);
        self.add(x, y);
    }
    fn close(&mut self) {}
}

/// Most VARC components one outline request may resolve, summed over
/// every nesting level. Mirrors HarfBuzz's graph edge cap. Real VARC
/// glyphs use a few dozen.
const MAX_VARC_COMPONENTS: usize = 2048;

/// Most path ops VARC composition may copy into composite outlines
/// for one outline request, summed over every nesting level.
const MAX_VARC_OPS: usize = 1 << 20;

/// Remaining work for one [`Face::glyph_outline_at_coords`] call.
/// Components that share children can make the resolved outline
/// grow exponentially with depth, so the whole request shares one
/// budget and fails with `Malformed` when it runs out.
struct VarcBudget {
    components_left: usize,
    ops_left: usize,
}

/// Applies a row-major `[xx, xy, yx, yy, tx, ty]` affine to a single
/// path op, transforming every point inside it. Control points and
/// endpoints alike receive the same affine, which is correct for
/// affine maps because they preserve the "control point ratio"
/// implied by Bezier evaluation.
fn transform_path_op(op: PathOp, m: [f32; 6]) -> PathOp {
    let xform =
        |x: f32, y: f32| -> (f32, f32) { (m[0] * x + m[1] * y + m[4], m[2] * x + m[3] * y + m[5]) };
    match op {
        PathOp::MoveTo { x, y } => {
            let (x, y) = xform(x, y);
            PathOp::MoveTo { x, y }
        }
        PathOp::LineTo { x, y } => {
            let (x, y) = xform(x, y);
            PathOp::LineTo { x, y }
        }
        PathOp::QuadTo { cx, cy, x, y } => {
            let (cx, cy) = xform(cx, cy);
            let (x, y) = xform(x, y);
            PathOp::QuadTo { cx, cy, x, y }
        }
        PathOp::CubicTo {
            c1x,
            c1y,
            c2x,
            c2y,
            x,
            y,
        } => {
            let (c1x, c1y) = xform(c1x, c1y);
            let (c2x, c2y) = xform(c2x, c2y);
            let (x, y) = xform(x, y);
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            }
        }
        PathOp::Close => PathOp::Close,
    }
}
