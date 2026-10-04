//! Drawing a `VARC` composite: the walk over its components that
//! [`Face::glyph_outline_at_coords`] and [`crate::GlyphOutlines`] make,
//! as HarfBuzz's `VARC::get_path_at` makes it.

use core::cell::OnceCell;

use super::Face;
use crate::error::{Error, Result};
use crate::tables::glyf::PhantomMetrics;
use crate::tables::varc::VarcComposite;
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
/// Every composite the walk reaches is resolved in the font set to the
/// coords of the glyph drawn, so a `RESET_UNSPECIFIED_AXES` component
/// starts from them however deep it sits. Components that share
/// children can make the walk grow exponentially with depth, so it
/// draws at most [`MAX_VARC_COMPONENTS`] components and
/// [`MAX_VARC_OPS`] ops, and fails with `Malformed` past either or
/// past [`MAX_VARC_DEPTH`].
pub(crate) struct VarcDraw<'w, 'a> {
    face: &'w Face<'a>,
    varc: &'w Varc<'a>,
    /// The font's coords, rounded to F2DOT14.
    font_coords: &'w [f32],
    leaves: Leaves<'a>,
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
            leaves: Leaves::default(),
            components_left: MAX_VARC_COMPONENTS,
            ops_left: MAX_VARC_OPS,
        }
    }

    /// Draws `glyph_id` into `sink`, as HarfBuzz draws a glyph `VARC`
    /// has a record for. Returns `Ok(false)`, drawing nothing, when it
    /// has none; the glyph is then drawn from `glyf` or CFF.
    pub(crate) fn draw<S: OutlineSink>(&mut self, glyph_id: u16, sink: &mut S) -> Result<bool> {
        let coords = self.font_coords;
        let Some(composite) = self.varc.composite(glyph_id, coords) else {
            return Ok(false);
        };
        self.draw_composite(glyph_id, &composite, IDENTITY, 0, sink)?;
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
        // same glyph. Other components VARC has a record for are
        // composites, resolved in the font set to the coords of the
        // glyph drawn; the rest are drawn from `glyf` or CFF at their
        // coords.
        if gid != parent {
            let composite = self
                .varc
                .composite_with_font_coords(gid, coords, self.font_coords);
            if let Some(composite) = composite {
                return self.draw_composite(gid, &composite, transform, depth, sink);
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
