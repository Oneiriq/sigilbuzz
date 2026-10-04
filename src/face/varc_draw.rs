//! Drawing a `VARC` composite: the walk over its components that
//! [`Face::glyph_outline_at_coords`] and [`crate::GlyphOutlines`] make,
//! as HarfBuzz's `VARC::get_path_at` makes it.

use core::cell::OnceCell;

use alloc::vec::Vec;

use super::Face;
use crate::error::{Error, Result};
use crate::tables::glyf::PhantomMetrics;
use crate::tables::varc::{VarcComposite, VarcMemo};
use crate::tables::{tag, Cff, Cff2, Glyf, Gvar, Hmtx, Loca, OutlineSink, Varc, Vmtx};

/// Deepest a component may sit below the glyph drawn, as the `glyf`
/// composite walk caps its depth.
const MAX_VARC_DEPTH: u8 = 64;

/// Most VARC components one glyph may draw, summed over every nesting
/// level. Real VARC glyphs use a few dozen.
const MAX_VARC_COMPONENTS: usize = 2048;

/// Most path ops one glyph's components may draw, summed over all of
/// them.
const MAX_VARC_OPS: usize = 1 << 20;

/// The identity affine, in [`crate::tables::VarcComponent::transform`]'s
/// `[xx, xy, yx, yy, tx, ty]` order.
const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// One glyph drawn through `VARC`.
///
/// The walk resolves composites with one [`VarcMemo`], so one budget
/// bounds the condition and delta work of every composite the glyph
/// reaches, and a glyph reached again at the same coords is resolved
/// once. Components that share children can make the walk grow
/// exponentially with depth, so it also draws at most
/// [`MAX_VARC_COMPONENTS`] components and [`MAX_VARC_OPS`] ops, and
/// fails with `Malformed` past either or past [`MAX_VARC_DEPTH`].
///
/// It stops cycles as HarfBuzz's decycler does: `path` holds the
/// glyphs whose composites are being drawn, from the glyph drawn down,
/// and a glyph that `VARC` covers is not drawn at position `k` of the
/// path when it is the glyph at position `k / 2`, where HarfBuzz's
/// tortoise sits.
pub(crate) struct VarcDraw<'w, 'a> {
    face: &'w Face<'a>,
    varc: &'w Varc<'a>,
    /// The font's coords, rounded to F2DOT14.
    font_coords: &'w [f32],
    memo: VarcMemo,
    leaves: Leaves<'a>,
    /// The glyphs whose composites are being drawn, outermost first.
    path: Vec<u16>,
    components_left: usize,
    ops_left: usize,
}

impl<'w, 'a> VarcDraw<'w, 'a> {
    /// A walk over `varc`, the `VARC` table of `face`, in a font set to
    /// `font_coords` (rounded to F2DOT14).
    pub(crate) fn new(face: &'w Face<'a>, varc: &'w Varc<'a>, font_coords: &'w [f32]) -> Self {
        Self {
            face,
            varc,
            font_coords,
            memo: VarcMemo::new(),
            leaves: Leaves::default(),
            path: Vec::new(),
            components_left: MAX_VARC_COMPONENTS,
            ops_left: MAX_VARC_OPS,
        }
    }

    /// Draws `glyph_id` into `sink`, as HarfBuzz draws a glyph `VARC`
    /// covers. Returns `Ok(false)`, drawing nothing, when it does not
    /// cover the glyph, which is then drawn from `glyf` or CFF.
    pub(crate) fn draw<S: OutlineSink>(&mut self, glyph_id: u16, sink: &mut S) -> Result<bool> {
        let coords = self.font_coords;
        let Some(composite) = self.varc.resolve(glyph_id, coords, coords, &mut self.memo) else {
            return Ok(false);
        };
        self.path.push(glyph_id);
        self.draw_composite(glyph_id, &composite, IDENTITY, 0, sink)?;
        self.path.pop();
        Ok(true)
    }

    /// Draws the components of glyph `gid`, whose composite is
    /// `composite`, under `transform`. `depth` is the glyph's depth
    /// below the glyph drawn.
    fn draw_composite<S: OutlineSink>(
        &mut self,
        gid: u16,
        composite: &VarcComposite,
        transform: [f32; 6],
        depth: u8,
        sink: &mut S,
    ) -> Result<()> {
        for component in &composite.components {
            self.components_left = self
                .components_left
                .checked_sub(1)
                .ok_or(Error::Malformed {
                    offset: 0,
                    context: "VARC composite exceeds component budget",
                })?;
            let transform = multiply(transform, component.transform);
            self.draw_component(
                gid,
                component.gid,
                &component.coords,
                transform,
                depth + 1,
                sink,
            )?;
        }
        Ok(())
    }

    /// Draws glyph `gid` at `coords` under `transform`, as a component
    /// of glyph `parent`.
    fn draw_component<S: OutlineSink>(
        &mut self,
        parent: u16,
        gid: u16,
        coords: &[f32],
        transform: [f32; 6],
        depth: u8,
        sink: &mut S,
    ) -> Result<()> {
        if depth > MAX_VARC_DEPTH {
            return Err(Error::Malformed {
                offset: 0,
                context: "VARC composite recursion exceeded cap",
            });
        }
        // A component that names its parent's glyph draws that glyph's
        // own outline, as in HarfBuzz, which does not recurse on the
        // same glyph. Other components VARC covers are composites,
        // unless the decycler stops them; the rest are drawn from
        // `glyf` or CFF at their coords.
        if gid != parent && self.varc.covers(gid) {
            if self.path.get(self.path.len() / 2) == Some(&gid) {
                return Ok(());
            }
            let font_coords = self.font_coords;
            if let Some(composite) = self.varc.resolve(gid, coords, font_coords, &mut self.memo) {
                self.path.push(gid);
                let drawn = self.draw_composite(gid, &composite, transform, depth, sink);
                self.path.pop();
                return drawn;
            }
        }
        let mut placed = Placed {
            sink,
            transform: (transform != IDENTITY).then_some(transform),
            left: self.ops_left,
            drawn: 0,
        };
        self.leaves.draw(self.face, gid, coords, &mut placed)?;
        self.ops_left = self
            .ops_left
            .checked_sub(placed.drawn)
            .ok_or(Error::Malformed {
                offset: 0,
                context: "VARC composite exceeds outline budget",
            })?;
        Ok(())
    }
}

/// `a` times `b`, for affines in `[xx, xy, yx, yy, tx, ty]` order: `b`
/// applied first. The sums are taken in the order of HarfBuzz's
/// `hb_transform_t::multiply`, which composes the transforms of nested
/// components the same way.
fn multiply(a: [f32; 6], b: [f32; 6]) -> [f32; 6] {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[0] * b[4] + a[1] * b[5] + a[4],
        a[2] * b[4] + a[3] * b[5] + a[5],
    ]
}

/// A sink that places a leaf's ops: maps each point by the component's
/// transform, as HarfBuzz's `transform_point` does (none for the
/// identity), and passes at most `left` ops on, counting every op drawn.
struct Placed<'s, S> {
    sink: &'s mut S,
    /// `None` for the identity.
    transform: Option<[f32; 6]>,
    left: usize,
    drawn: usize,
}

impl<S: OutlineSink> Placed<'_, S> {
    /// Counts one op, and whether it is passed on.
    fn admit(&mut self) -> bool {
        self.drawn = self.drawn.saturating_add(1);
        if self.left == 0 {
            return false;
        }
        self.left -= 1;
        true
    }

    fn map(&self, x: f32, y: f32) -> (f32, f32) {
        match self.transform {
            None => (x, y),
            Some(m) => (m[4] + m[0] * x + m[1] * y, m[5] + m[2] * x + m[3] * y),
        }
    }
}

impl<S: OutlineSink> OutlineSink for Placed<'_, S> {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.admit() {
            let (x, y) = self.map(x, y);
            self.sink.move_to(x, y);
        }
    }

    fn line_to(&mut self, x: f32, y: f32) {
        if self.admit() {
            let (x, y) = self.map(x, y);
            self.sink.line_to(x, y);
        }
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        if self.admit() {
            let (cx, cy) = self.map(cx, cy);
            let (x, y) = self.map(x, y);
            self.sink.quad_to(cx, cy, x, y);
        }
    }

    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        if self.admit() {
            let (c1x, c1y) = self.map(c1x, c1y);
            let (c2x, c2y) = self.map(c2x, c2y);
            let (x, y) = self.map(x, y);
            self.sink.curve_to(c1x, c1y, c2x, c2y, x, y);
        }
    }

    fn close(&mut self) {
        if self.admit() {
            self.sink.close();
        }
    }
}

/// The tables the leaves of a walk are drawn from, read the first time
/// a leaf needs them, in the order [`Face::glyph_outline_at_coords`]
/// reads them for a glyph `VARC` does not draw, and kept for the walk.
/// A table that fails to read fails every leaf that needs it.
#[derive(Default)]
struct Leaves<'a> {
    tables: OnceCell<Result<LeafTables<'a>>>,
    /// Read for the first leaf away from the default instance.
    gvar: OnceCell<Result<Option<Gvar<'a>>>>,
}

/// The table a face draws glyphs from without `VARC`.
enum LeafTables<'a> {
    Cff2(Cff2<'a>),
    Cff(Cff<'a>),
    Glyf {
        glyf: Glyf<'a>,
        loca: Loca<'a>,
        hmtx: Hmtx<'a>,
        /// The font's `vmtx`, or HarfBuzz's metrics for a font without.
        vmtx: Vmtx<'a>,
    },
}

impl<'a> Leaves<'a> {
    /// Draws glyph `gid` of `face` at `coords` from `glyf` or CFF, as
    /// [`Face::glyph_outline_at_coords`] draws a glyph `VARC` does not.
    fn draw<S: OutlineSink>(
        &self,
        face: &Face<'a>,
        gid: u16,
        coords: &[f32],
        sink: &mut S,
    ) -> Result<bool> {
        let tables = self
            .tables
            .get_or_init(|| read_leaf_tables(face))
            .as_ref()
            .map_err(Clone::clone)?;
        match tables {
            LeafTables::Cff2(cff2) => cff2.outline(gid, coords, sink),
            LeafTables::Cff(cff) => cff.outline(gid, sink),
            LeafTables::Glyf {
                glyf,
                loca,
                hmtx,
                vmtx,
            } => {
                let gvar = if coords.is_empty() {
                    None
                } else {
                    self.gvar
                        .get_or_init(|| face.gvar())
                        .as_ref()
                        .map_err(Clone::clone)?
                        .as_ref()
                };
                let metrics = PhantomMetrics {
                    hmtx,
                    vmtx: Some(vmtx),
                };
                glyf.outline_at_coords(loca, gid, gvar, coords, Some(&metrics), sink)
            }
        }
    }
}

/// Reads the table `face` draws glyphs from without `VARC`: `CFF2`
/// whenever the font has one, else `CFF ` in a font without `glyf`,
/// else `glyf` with `loca` and the metrics its phantom points read.
fn read_leaf_tables<'a>(face: &Face<'a>) -> Result<LeafTables<'a>> {
    if face.record(tag::CFF2).is_some() {
        return Ok(LeafTables::Cff2(face.cff2()?));
    }
    if face.record(tag::CFF1).is_some() && face.record(tag::GLYF).is_none() {
        return Ok(LeafTables::Cff(face.cff()?));
    }
    let loca = face.loca()?;
    let glyf = face.glyf()?;
    let hmtx = face.hmtx()?;
    let vmtx = face.vmtx()?;
    let vmtx = face.phantom_vmtx(vmtx)?;
    Ok(LeafTables::Glyf {
        glyf,
        loca,
        hmtx,
        vmtx,
    })
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;
    use crate::tables::varc::tests::{
        build_varc, condition_list, deep_value_condition, mention_store, with_conditions,
    };
    use crate::tables::varc::MAX_WALK_WORK;
    use crate::tables::{Outline, PathOp};
    use crate::Blob;

    /// `font` with its `VARC` table replaced by `varc`.
    fn with_varc(font: &[u8], varc: &[u8]) -> Vec<u8> {
        let face = Face::parse_bytes(font, 0).unwrap();
        let mut tables: Vec<([u8; 4], &[u8])> = face
            .records()
            .iter()
            .filter(|r| r.tag != tag::VARC)
            .map(|r| (r.tag, face.table_bytes(r.tag).unwrap()))
            .collect();
        tables.push((tag::VARC, varc));
        tables.sort_by_key(|t| t.0);
        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        let mut offset = 12 + 16 * tables.len();
        let mut body = Vec::new();
        for (tag, data) in &tables {
            out.extend_from_slice(tag);
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            body.extend_from_slice(data);
            while body.len() % 4 != 0 {
                body.push(0);
            }
            offset = 12 + 16 * tables.len() + body.len();
        }
        out.extend_from_slice(&body);
        out
    }

    /// `varc_parity.ttf` (glyph 1 a box, glyph 2 a triangle) with a VARC
    /// table in which glyph 3 has one component per entry of `axis0`,
    /// each naming glyph 4 with axis 0 set to that F2DOT14 value, and
    /// glyph 4 two components: the box, gated by 20 And levels whose two
    /// offsets name the same child over a Value condition walking 60,000
    /// region mentions, and the triangle, its translation varying
    /// through the same delta set.
    fn heavy_fan_out(axis0: &[i16]) -> Vec<u8> {
        let mut fan = Vec::new();
        for &value in axis0 {
            // HAVE_AXES, glyph 4, axis indices list 0, one i16 value.
            fan.extend_from_slice(&[0x02, 0x00, 0x04, 0x00, 0x40]);
            fan.extend_from_slice(&value.to_be_bytes());
        }
        // The box, gated by condition 0, then the triangle, translated
        // by variation index 0.
        let heavy = vec![
            0x80, 0x80, 0x00, 0x01, 0x00, 0x18, 0x00, 0x02, 0x00, 0x00, 0x00,
        ];
        let table = build_varc(
            &[3, 4],
            &[&fan, &heavy],
            Some(&mention_store(60_000)),
            Some(&[&[0x00, 0x00]]),
        );
        let table = with_conditions(table, &condition_list(&[deep_value_condition(20, -30_000)]));
        with_varc(
            include_bytes!("../../tests/fixtures/varc_parity.ttf"),
            &table,
        )
    }

    /// Draws glyph 3 of `font` at the default instance, returning the
    /// outline, the work units and the condition visits of the walk.
    fn walk(font: &[u8]) -> (Outline, u64, u32) {
        let blob = Blob::new(font);
        let face = Face::parse(&blob, 0).unwrap();
        let varc = face.varc().unwrap().unwrap();
        let mut draw = VarcDraw::new(&face, &varc, &[]);
        let mut out = Outline::new();
        assert!(draw.draw(3, &mut out).unwrap());
        let work = draw.memo.work_done();
        let visits = draw.memo.condition_visits();
        // The face draws the same outline.
        assert_eq!(face.glyph_outline(3).unwrap(), Some(out.clone()));
        (out, work, visits)
    }

    fn move_xs(outline: &Outline) -> Vec<f32> {
        outline
            .ops()
            .iter()
            .filter_map(|op| match *op {
                PathOp::MoveTo { x, .. } => Some(x),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn components_at_distinct_coords_share_one_work_budget() {
        // 600 components, each reaching glyph 4 at its own coords, whose
        // condition and deltas cost 180,047 units there: 2 to keep the
        // coords, 2 for the records, 2 for their coords, 40 for the And
        // offsets, 120,001 for the Value condition (scalars, one region
        // axis, the walk) and 60,000 for the translation. One budget per
        // composite let every one of them spend that, 108 million units
        // in all. Now glyph 3 (1801 units: its coords, its 600 records
        // and their coords) and five copies of glyph 4 fit, the sixth
        // runs out at its translation, and the rest draw nothing.
        // Axis 0 starts just past 0.5, where the condition starts to hold.
        let axis0: Vec<i16> = (0..600).map(|i| 8193 + i).collect();
        let (out, work, visits) = walk(&heavy_fan_out(&axis0));
        assert_eq!(work, MAX_WALK_WORK);
        assert_eq!(visits, 6 * 41);
        let xs = move_xs(&out);
        assert_eq!(xs.len(), 12);
        // Box, then triangle translated by 60,000 deltas of 1 times axis 0,
        // summed in f32 as the walk sums them.
        for (i, pair) in xs.chunks(2).enumerate().take(5) {
            let scalar = (8193.0 + i as f32) / 16384.0;
            let tx = (0..60_000).fold(0.0f32, |sum, _| sum + scalar);
            assert_eq!(pair, [0.0, tx]);
        }
        assert_eq!(xs[10..], [0.0, 0.0]);
    }

    #[test]
    fn a_glyph_reached_again_at_the_same_coords_is_resolved_once() {
        // 600 components reaching glyph 4 at axis 0 = 1: its 180,047
        // units are spent once, and every copy is drawn with them.
        let (out, work, visits) = walk(&heavy_fan_out(&[16384; 600]));
        assert_eq!(work, 1 + 600 * 3 + 180_047);
        assert_eq!(visits, 41);
        let xs = move_xs(&out);
        assert_eq!(xs.len(), 1200);
        assert!(xs.chunks(2).all(|pair| pair == [0.0, 60_000.0]));
    }
}
