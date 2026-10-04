//! `Face` accessors for glyph outlines: `loca` / `glyf`, `CFF ` /
//! `CFF2`, glyph points and bounds, and the VARC-aware outline walk.

use alloc::vec::Vec;

use super::Face;
use crate::error::{Error, Result};
use crate::font::f2dot14_coords;
use crate::tables::glyf::PhantomMetrics;
use crate::tables::parse::hb_roundf;
use crate::tables::{tag, Cff, Cff2, Glyf, GlyphBounds, Gvar, Loca, Outline, PathOp, Vmtx};

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
    /// The coords are first rounded to F2DOT14, multiples of 1/16384
    /// with halves rounded up, as HarfBuzz stores a font's coords and
    /// as shaping reads them, so the box is the one shaping uses.
    /// Coords that all round to zero give the default instance.
    ///
    /// When the font varies (`gvar` is present and some coord is not
    /// zero), the box is the extent of the varied outline's points,
    /// off-curve points included, each edge rounded as HarfBuzz's
    /// `roundf` rounds (`floor(x + 0.5)`, halves up), as HarfBuzz
    /// computes glyph extents. A glyph whose varied
    /// outline has no points, or whose box has no width or no height,
    /// gets an all-zero box, as in HarfBuzz. Otherwise the box
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
        let coords = f2dot14_coords(coords);
        if coords.is_empty() {
            return Ok(Some(base));
        }
        let Some(gvar) = self.gvar()? else {
            return Ok(Some(base));
        };
        let loca = self.loca()?;
        let glyf = self.glyf()?;
        let hmtx = self.hmtx()?;
        // `vmtx` only places the vertical phantom points, which only a
        // component anchored to one reads. A `vmtx` that does not parse
        // counts as absent, as HarfBuzz's sanitizer drops it, so it
        // cannot fail the extents of a horizontal run's glyphs.
        let vmtx = self.vmtx().ok().flatten();
        let vmtx = self.phantom_vmtx(vmtx)?;
        let metrics = PhantomMetrics {
            hmtx: &hmtx,
            vmtx: Some(&vmtx),
        };
        let tables = (&glyf, &loca, &gvar);
        varied_glyph_bounds(tables, glyph_id, &coords, &metrics, base.num_contours).map(Some)
    }

    /// The vertical metrics the phantom points of the glyphs of a walk
    /// read: `vmtx`, the font's table, or when the font has none
    /// HarfBuzz's metrics for that case (see [`Vmtx::missing`]), a top
    /// side bearing of zero and an advance of an em. A component
    /// anchored to a vertical phantom point then lands where HarfBuzz
    /// puts it.
    pub(crate) fn phantom_vmtx(&self, vmtx: Option<Vmtx<'a>>) -> Result<Vmtx<'a>> {
        match vmtx {
            Some(vmtx) => Ok(vmtx),
            None => Ok(Vmtx::missing(self.head()?.units_per_em)),
        }
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
    ///
    /// The coords are first rounded to F2DOT14, multiples of 1/16384
    /// with halves rounded up: HarfBuzz rounds a font's coords to that
    /// precision when they are set, and shaping reads them the same
    /// way, so a glyph drawn here at a `Font`'s coords is the glyph
    /// shaping measured and HarfBuzz draws. Coords that all round to
    /// zero draw the default instance. The table-level methods
    /// ([`Glyf::outline_at_coords`], [`Cff2::outline`]) take coords as
    /// given.
    pub fn glyph_outline_at_coords(
        &self,
        glyph_id: u16,
        coords: &[f32],
    ) -> Result<Option<Outline>> {
        let mut budget = VarcBudget {
            components_left: MAX_VARC_COMPONENTS,
            ops_left: MAX_VARC_OPS,
        };
        let coords = f2dot14_coords(coords);
        self.glyph_outline_at_coords_inner(glyph_id, &coords, 0, &mut budget)
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
        // vmtx is optional, and a font without it gets HarfBuzz's
        // vertical metrics for one.
        let hmtx = self.hmtx()?;
        let vmtx = self.vmtx()?;
        let vmtx = self.phantom_vmtx(vmtx)?;
        let metrics = PhantomMetrics {
            hmtx: &hmtx,
            vmtx: Some(&vmtx),
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

/// The box of glyph `glyph_id`'s outline walked at `coords` with the
/// `glyf`, `loca` and `gvar` of `tables`, as
/// [`Face::glyph_bounds_at_coords`] computes it away from the default
/// instance, with `num_contours` from the glyph header. The shaper's
/// glyph extents call it with tables it reads once per shaping call.
pub(crate) fn varied_glyph_bounds(
    tables: (&Glyf<'_>, &Loca<'_>, &Gvar<'_>),
    glyph_id: u16,
    coords: &[f32],
    metrics: &PhantomMetrics<'_>,
    num_contours: i16,
) -> Result<GlyphBounds> {
    let (glyf, loca, gvar) = tables;
    let mut points = PointBox::default();
    glyf.points_at_coords(loca, glyph_id, Some(gvar), coords, Some(metrics), |x, y| {
        points.add(x, y);
    })?;
    Ok(points.bounds(num_contours))
}

/// Bounding box of every point of an outline walk, off-curve points
/// included. Implied on-curve points lie between two of them, so they
/// never widen it.
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

    /// The box rounded as HarfBuzz's `roundf` rounds, halves up, or all
    /// zeros when it is empty: no point was seen, or it has no width or
    /// no height, as HarfBuzz's `contour_bounds_t::empty` decides before
    /// rounding.
    fn bounds(&self, num_contours: i16) -> GlyphBounds {
        if self.min.0 >= self.max.0 || self.min.1 >= self.max.1 {
            return GlyphBounds {
                x_min: 0,
                y_min: 0,
                x_max: 0,
                y_max: 0,
                num_contours,
            };
        }
        let round = |v: f32| hb_roundf(v).clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
        GlyphBounds {
            x_min: round(self.min.0),
            y_min: round(self.min.1),
            x_max: round(self.max.0),
            y_max: round(self.max.1),
            num_contours,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(points: &[(f32, f32)]) -> GlyphBounds {
        let mut b = PointBox::default();
        for &(x, y) in points {
            b.add(x, y);
        }
        b.bounds(1)
    }

    fn edges(b: GlyphBounds) -> [i16; 4] {
        [b.x_min, b.y_min, b.x_max, b.y_max]
    }

    #[test]
    fn outlines_and_bounds_read_coords_at_f2dot14_precision() {
        // A subset of Hahmlet, whose `wght` axis runs from 100 to 900.
        let data = include_bytes!("../../tests/fixtures/hahmlet_gvar_subset.ttf");
        let blob = crate::Blob::new(data);
        let face = Face::parse(&blob, 0).unwrap();
        // Glyph 4 is `O`. 0.3 is no multiple of 1/16384: HarfBuzz keeps
        // it as round(0.3 * 16384) = 4915, and so does shaping.
        let o = 4;
        let rounded = [4915.0 / 16384.0];
        let outline = face.glyph_outline_at_coords(o, &[0.3]).unwrap();
        assert_eq!(outline, face.glyph_outline_at_coords(o, &rounded).unwrap());
        assert_eq!(
            face.glyph_bounds_at_coords(o, &[0.3]).unwrap(),
            face.glyph_bounds_at_coords(o, &rounded).unwrap()
        );
        // The table-level walk takes 0.3 as given, and draws another
        // outline.
        let (loca, glyf, gvar) = (
            face.loca().unwrap(),
            face.glyf().unwrap(),
            face.gvar().unwrap(),
        );
        let mut exact = Outline::new();
        glyf.outline_at_coords(&loca, o, gvar.as_ref(), &[0.3], None, &mut exact)
            .unwrap();
        assert_ne!(outline, Some(exact));
        // A coord under half a unit is the default instance.
        assert_eq!(
            face.glyph_outline_at_coords(o, &[1.0 / 40000.0]).unwrap(),
            face.glyph_outline(o).unwrap()
        );
    }

    #[test]
    fn varied_bounds_from_the_points_box_the_drawn_outline() {
        // The varied box reads the walked points without drawing the
        // outline; the drawing only adds points between two of them,
        // so the box is the same.
        for data in [
            &include_bytes!("../../tests/fixtures/hahmlet_gvar_subset.ttf")[..],
            &include_bytes!("../../tests/fixtures/rubik_vf.ttf")[..],
        ] {
            let blob = crate::Blob::new(data);
            let face = Face::parse(&blob, 0).unwrap();
            let (loca, glyf, hmtx) = (
                face.loca().unwrap(),
                face.glyf().unwrap(),
                face.hmtx().unwrap(),
            );
            let gvar = face.gvar().unwrap().unwrap();
            let vmtx = face.phantom_vmtx(None).unwrap();
            let metrics = PhantomMetrics {
                hmtx: &hmtx,
                vmtx: Some(&vmtx),
            };
            for coords in [[1.0], [-0.625], [6145.0 / 16384.0]] {
                for gid in 0..face.maxp().unwrap().num_glyphs {
                    let mut drawn = PointBox::default();
                    let mut sink = Outline::new();
                    glyf.outline_at_coords(
                        &loca,
                        gid,
                        Some(&gvar),
                        &coords,
                        Some(&metrics),
                        &mut sink,
                    )
                    .unwrap();
                    for op in sink.ops() {
                        match *op {
                            PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => drawn.add(x, y),
                            PathOp::QuadTo { cx, cy, x, y } => {
                                drawn.add(cx, cy);
                                drawn.add(x, y);
                            }
                            PathOp::CubicTo { .. } | PathOp::Close => {}
                        }
                    }
                    let tables = (&glyf, &loca, &gvar);
                    let got = varied_glyph_bounds(tables, gid, &coords, &metrics, 1).unwrap();
                    assert_eq!(got, drawn.bounds(1), "glyph {gid} at {coords:?}");
                }
            }
        }
    }

    #[test]
    fn a_point_box_rounds_its_edges_halves_up() {
        // HarfBuzz's roundf is floor(x + 0.5): -10.5 goes up to -10.
        let b = bounds(&[(-10.5, 0.4), (99.5, 200.6)]);
        assert_eq!(edges(b), [-10, 0, 100, 201]);
        assert_eq!(b.num_contours, 1);
    }

    #[test]
    fn a_point_box_without_area_is_empty() {
        // No points, a single point, and boxes with no width or no
        // height: HarfBuzz reports zero extents for all of them, even
        // when the rounded edges would differ.
        for points in [
            &[][..],
            &[(5.0, 5.0)][..],
            &[(10.0, 0.0), (10.0, 100.0)][..],
            &[(0.0, 50.0), (100.0, 50.0)][..],
        ] {
            assert_eq!(edges(bounds(points)), [0; 4], "{points:?}");
        }
        // A sliver keeps its edges.
        assert_eq!(
            edges(bounds(&[(10.0, 0.0), (10.25, 100.0)])),
            [10, 0, 10, 100]
        );
    }
}
