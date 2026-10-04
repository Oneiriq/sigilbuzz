//! Glyph ink extents, as HarfBuzz's `hb_font_get_glyph_extents` computes
//! them for outline fonts, for fallback mark positioning and the
//! vertical origins of glyphs without `VORG`.

use core::cell::OnceCell;

use crate::error::{Error, Result};
use crate::face::{varied_glyph_bounds, Face};
use crate::tables::cff::CharstringSink;
use crate::tables::cff2::Cff2Shared;
use crate::tables::glyf::PhantomMetrics;
use crate::tables::parse::hb_roundf64;
use crate::tables::{tag, Cff, Cff2, Glyf, Gvar, Hmtx, Loca, PathOp, Varc, Vmtx};

/// HarfBuzz's `hb_glyph_extents_t`, in font design units: the left and
/// top edges, the width, and the height (negative, since y grows up).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::shape) struct Extents {
    pub(in crate::shape) x_bearing: i32,
    pub(in crate::shape) y_bearing: i32,
    pub(in crate::shape) width: i32,
    pub(in crate::shape) height: i32,
}

/// The tables glyph extents come from, each read the first time a glyph
/// needs it and kept for the rest of the shaping call, so a call that
/// asks for many glyphs' extents parses each table once. A table that
/// fails to read fails every glyph that needs it, with the error it
/// failed with.
#[derive(Default)]
pub(in crate::shape) struct ExtentsTables<'a> {
    /// `loca` and `glyf`.
    glyf: OnceCell<Result<(Loca<'a>, Glyf<'a>)>>,
    /// `gvar`, for the extents of a varied `glyf` glyph.
    gvar: OnceCell<Result<Option<Gvar<'a>>>>,
    /// The vertical metrics that place the vertical phantom points a
    /// varied `glyf` glyph's components can be anchored to (see
    /// [`Face::phantom_vmtx`]). A `vmtx` that does not parse counts as
    /// absent, as HarfBuzz's sanitizer drops it.
    vmtx: OnceCell<Result<Vmtx<'a>>>,
    /// `VARC`, whose composites take precedence over `CFF2` and `CFF `;
    /// `None` when the font has none or it does not parse, as the face's
    /// outlines read it.
    varc: OnceCell<Option<Varc<'a>>>,
    /// The CFF table a glyph's outline comes from, `None` when the font
    /// draws its glyphs some other way.
    cff: OnceCell<Option<Result<CffTable<'a>>>>,
}

/// The table [`Face::glyph_outline_at_coords`] would draw a glyph from:
/// `CFF2` whenever the font has one, else `CFF ` in a font without
/// `glyf`.
enum CffTable<'a> {
    Cff(Cff<'a>),
    /// With what its glyphs share at the call's coords.
    Cff2(Cff2<'a>, Cff2Shared<'a>),
}

impl<'a> ExtentsTables<'a> {
    /// The extents of glyph `gid` of `face` at `coords`, or `None` when
    /// the font has no outline table sigilbuzz reads extents from (only
    /// `glyf`, `CFF ` and `CFF2` are consulted; HarfBuzz also asks the
    /// bitmap and color tables). `hmtx` is the font's.
    ///
    /// - `glyf` at the default instance: the glyph header's box, with
    ///   the `hmtx` left side bearing as the left edge (HarfBuzz follows
    ///   the rasterizers there), and zero extents for an empty glyph.
    /// - `glyf` at other coordinates: the box moved by the glyph's
    ///   `gvar` deltas (see [`Face::glyph_bounds_at_coords`]).
    /// - `CFF ` and `CFF2`: the box of every outline point, control
    ///   points included, rounded to whole units, as HarfBuzz's
    ///   charstring extents are. The charstring runs into a sink that
    ///   only keeps the box. A glyph `VARC` draws gets the box of its
    ///   [`Face::glyph_outline_at_coords`] outline.
    pub(in crate::shape) fn glyph_extents(
        &self,
        face: &Face<'a>,
        coords: &[f32],
        hmtx: &Hmtx<'a>,
        gid: u16,
    ) -> Result<Option<Extents>> {
        match self.glyf_bounds(face, coords, hmtx, gid) {
            Ok(extents) => return Ok(Some(extents)),
            Err(Error::MissingTable { .. }) => {}
            Err(e) => return Err(e),
        }
        let has_cff = face.record(tag::CFF1).is_some() || face.record(tag::CFF2).is_some();
        if !has_cff {
            return Ok(None);
        }
        let varc = self.varc.get_or_init(|| face.drawable_varc());
        let cff = self.cff.get_or_init(|| {
            if face.record(tag::CFF2).is_some() {
                Some(
                    face.cff2()
                        .map(|t| CffTable::Cff2(t, Cff2Shared::default())),
                )
            } else if face.record(tag::GLYF).is_none() {
                Some(face.cff().map(CffTable::Cff))
            } else {
                None
            }
        });
        let cff = match cff {
            Some(cff) if !varc.as_ref().is_some_and(|v| v.covers(gid)) => cff,
            // A VARC composite, or a font with `CFF ` and `glyf` whose
            // `glyf` could not be read: draw the outline as
            // `glyph_outline_at_coords` draws it.
            _ => {
                let outline = face.glyph_outline_at_coords(gid, coords)?;
                return Ok(Some(outline.map_or_else(Extents::default, |o| {
                    let mut b = ControlBox::default();
                    b.add_ops(o.ops());
                    b.extents()
                })));
            }
        };
        let mut b = ControlBox::default();
        match cff.as_ref().map_err(Clone::clone)? {
            CffTable::Cff2(cff2, shared) => cff2.outline_shared(gid, coords, shared, &mut b)?,
            CffTable::Cff(cff) => cff.draw(gid, &mut b)?,
        };
        Ok(Some(b.extents()))
    }

    /// The `glyf` extents of glyph `gid`, failing with
    /// [`Error::MissingTable`] when the font has no `glyf` or `loca`.
    fn glyf_bounds(
        &self,
        face: &Face<'a>,
        coords: &[f32],
        hmtx: &Hmtx<'a>,
        gid: u16,
    ) -> Result<Extents> {
        let (loca, glyf) = self
            .glyf
            .get_or_init(|| Ok((face.loca()?, face.glyf()?)))
            .as_ref()
            .map_err(Clone::clone)?;
        let Some(mut b) = glyf.bounds(loca, gid)? else {
            return Ok(Extents::default());
        };
        let varied = coords.iter().any(|&c| c != 0.0);
        if varied {
            let gvar = self
                .gvar
                .get_or_init(|| face.gvar())
                .as_ref()
                .map_err(Clone::clone)?;
            if let Some(gvar) = gvar {
                let vmtx = self
                    .vmtx
                    .get_or_init(|| face.phantom_vmtx(face.vmtx().ok().flatten()))
                    .as_ref()
                    .map_err(Clone::clone)?;
                let metrics = PhantomMetrics {
                    hmtx,
                    vmtx: Some(vmtx),
                };
                let tables = (glyf, loca, gvar);
                b = varied_glyph_bounds(tables, gid, coords, &metrics, b.num_contours)?;
            }
        }
        let (x_min, x_max) = (b.x_min.min(b.x_max), b.x_min.max(b.x_max));
        let (y_min, y_max) = (b.y_min.min(b.y_max), b.y_min.max(b.y_max));
        let lsb = if coords.is_empty() {
            hmtx.lsb(gid).unwrap_or(x_min)
        } else {
            x_min
        };
        Ok(Extents {
            x_bearing: i32::from(lsb),
            y_bearing: i32::from(y_max),
            width: i32::from(x_max) - i32::from(x_min),
            height: i32::from(y_min) - i32::from(y_max),
        })
    }
}

/// The box of an outline's points, control points included, as
/// HarfBuzz's charstring extents collect it: in `f64`, the precision
/// the charstring is evaluated in, and with a move counted once a
/// segment starts from it, so a move that starts no segment does not
/// count.
struct ControlBox {
    min: (f64, f64),
    max: (f64, f64),
    /// The last move, until a segment starts from it.
    pending: Option<(f64, f64)>,
}

impl Default for ControlBox {
    fn default() -> Self {
        Self {
            min: (f64::INFINITY, f64::INFINITY),
            max: (f64::NEG_INFINITY, f64::NEG_INFINITY),
            pending: None,
        }
    }
}

impl ControlBox {
    fn add(&mut self, x: f64, y: f64) {
        self.min = (self.min.0.min(x), self.min.1.min(y));
        self.max = (self.max.0.max(x), self.max.1.max(y));
    }

    /// Adds the move a segment starts from, if it has not been added.
    fn start_segment(&mut self) {
        if let Some((x, y)) = self.pending.take() {
            self.add(x, y);
        }
    }

    /// Adds every point of `ops`, a drawn outline's.
    fn add_ops(&mut self, ops: &[PathOp]) {
        let p = |x: f32, y: f32| (f64::from(x), f64::from(y));
        for op in ops {
            match *op {
                PathOp::MoveTo { x, y } => self.pending = Some(p(x, y)),
                PathOp::LineTo { x, y } => {
                    self.start_segment();
                    let (x, y) = p(x, y);
                    self.add(x, y);
                }
                PathOp::QuadTo { cx, cy, x, y } => {
                    self.start_segment();
                    for (x, y) in [p(cx, cy), p(x, y)] {
                        self.add(x, y);
                    }
                }
                PathOp::CubicTo {
                    c1x,
                    c1y,
                    c2x,
                    c2y,
                    x,
                    y,
                } => {
                    self.start_segment();
                    for (x, y) in [p(c1x, c1y), p(c2x, c2y), p(x, y)] {
                        self.add(x, y);
                    }
                }
                PathOp::Close => {}
            }
        }
    }

    /// The box with each edge rounded as HarfBuzz's `roundf` rounds a
    /// `double`, `floor(x + 0.5)`, halves up, and the width and height
    /// taken between the rounded edges and clamped to `i32`, as in
    /// HarfBuzz. A box with no width (or no height) has zero x extents
    /// (or y extents).
    fn extents(&self) -> Extents {
        // The bearing at `from` and the extent to `to`, both rounded;
        // `as` saturates, as HarfBuzz's `hb_clamp_to` clamps.
        let edge = |from: f64, to: f64| {
            let (from, to) = (hb_roundf64(from), hb_roundf64(to));
            (from as i32, (to - from) as i32)
        };
        let mut e = Extents::default();
        if self.min.0 < self.max.0 {
            (e.x_bearing, e.width) = edge(self.min.0, self.max.0);
        }
        if self.min.1 < self.max.1 {
            (e.y_bearing, e.height) = edge(self.max.1, self.min.1);
        }
        e
    }
}

impl CharstringSink for ControlBox {
    fn move_to(&mut self, x: f64, y: f64) {
        self.pending = Some((x, y));
    }
    fn line_to(&mut self, x: f64, y: f64) {
        self.start_segment();
        self.add(x, y);
    }
    fn curve_to(&mut self, c1x: f64, c1y: f64, c2x: f64, c2y: f64, x: f64, y: f64) {
        self.start_segment();
        self.add(c1x, c1y);
        self.add(c2x, c2y);
        self.add(x, y);
    }
    fn close(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::cff::{op_code, Index, Interp2};
    use crate::tables::Outline;
    use crate::Blob;

    fn control_box(ops: &[PathOp]) -> Extents {
        let mut b = ControlBox::default();
        b.add_ops(ops);
        b.extents()
    }

    #[test]
    fn cff_extents_are_the_same_with_the_tables_read_once() {
        // The extents keep the CFF table, the variation store, the
        // Private DICTs and the region scalars for the next glyph. Every
        // glyph must get the extents a fresh read gives it, at the
        // default instance and away from it, whatever order the glyphs
        // come in, and they must box the outline the face draws, which
        // only differs by its points' rounding to f32.
        let fonts: [(&[u8], &[&[f32]]); 2] = [
            (
                include_bytes!("../../../tests/fonts/SourceCodePro-Latin-Subset.otf"),
                &[&[]],
            ),
            (
                include_bytes!("../../../tests/fixtures/noto_sans_kr_vf_cff2_subset.otf"),
                &[&[], &[0.25], &[-0.5], &[1.0]],
            ),
        ];
        for (data, settings) in fonts {
            let blob = Blob::new(data);
            let face = Face::parse(&blob, 0).unwrap();
            let hmtx = face.hmtx().unwrap();
            let n = face.maxp().unwrap().num_glyphs;
            for &coords in settings {
                let tables = ExtentsTables::default();
                for gid in (0..n).rev().chain(0..n) {
                    let fresh = ExtentsTables::default().glyph_extents(&face, coords, &hmtx, gid);
                    let got = tables.glyph_extents(&face, coords, &hmtx, gid).unwrap();
                    assert_eq!(got, fresh.unwrap(), "glyph {gid} at {coords:?}");
                    let drawn = face
                        .glyph_outline_at_coords(gid, coords)
                        .unwrap()
                        .map_or_else(Extents::default, |o| control_box(o.ops()));
                    let got = got.unwrap();
                    for (a, b) in [
                        (got.x_bearing, drawn.x_bearing),
                        (got.y_bearing, drawn.y_bearing),
                        (got.width, drawn.width),
                        (got.height, drawn.height),
                    ] {
                        assert!(
                            (a - b).abs() <= 1,
                            "glyph {gid} at {coords:?}: {got:?} {drawn:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn cff_extents_keep_the_charstring_precision() {
        // `0 0 rmoveto 100 749.4999847 rlineto`, the second operand a
        // 16.16 number, 0x02ED7FFF / 65536. HarfBuzz evaluates
        // charstrings in double, so the top edge is just under 749.5
        // and rounds to 749. In f32 the operand is 749.5 exactly, which
        // rounds to 750: the outline, drawn in f32, has that.
        let cs = [
            139,
            139,
            op_code::RMOVETO,
            255,
            0x00,
            0x64,
            0x00,
            0x00,
            255,
            0x02,
            0xED,
            0x7F,
            0xFF,
            op_code::RLINETO,
        ];
        let mut b = ControlBox::default();
        let mut interp = Interp2::new(Index::default(), Index::default(), &mut b, None);
        interp.run(&cs, 0).unwrap();
        interp.finish();
        assert_eq!(
            b.extents(),
            Extents {
                x_bearing: 0,
                y_bearing: 749,
                width: 100,
                height: -749,
            }
        );
        let mut outline = Outline::new();
        let mut interp = Interp2::new(Index::default(), Index::default(), &mut outline, None);
        interp.run(&cs, 0).unwrap();
        interp.finish();
        assert_eq!(outline.ops()[1], PathOp::LineTo { x: 100.0, y: 749.5 });
    }

    #[test]
    fn the_control_box_counts_control_points_and_rounds() {
        let ops = [
            PathOp::MoveTo { x: 10.4, y: 0.0 },
            PathOp::CubicTo {
                c1x: 10.4,
                c1y: 700.6,
                c2x: 300.0,
                c2y: 700.6,
                x: 300.0,
                y: 0.0,
            },
            PathOp::Close,
            // A trailing move draws nothing.
            PathOp::MoveTo { x: 900.0, y: 900.0 },
        ];
        assert_eq!(
            control_box(&ops),
            Extents {
                x_bearing: 10,
                y_bearing: 701,
                width: 290,
                height: -701,
            }
        );
        assert_eq!(control_box(&[]), Extents::default());
    }
}
