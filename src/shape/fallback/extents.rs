//! Glyph ink extents, as HarfBuzz's `hb_font_get_glyph_extents` computes
//! them for outline fonts, for fallback mark positioning and the
//! vertical origins of glyphs without `VORG`.

use core::cell::OnceCell;

use crate::error::{Error, Result};
use crate::face::{varied_glyph_bounds, Face};
use crate::tables::cff2::Cff2Shared;
use crate::tables::glyf::PhantomMetrics;
use crate::tables::outline::OutlineSink;
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
    /// `vmtx`, which places the vertical phantom points a varied `glyf`
    /// glyph's components can be anchored to. One that does not parse
    /// counts as absent, as HarfBuzz's sanitizer drops it.
    vmtx: OnceCell<Option<Vmtx<'a>>>,
    /// `VARC`, whose composites take precedence over `CFF2` and `CFF `.
    varc: OnceCell<Result<Option<Varc<'a>>>>,
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
        let varc = self
            .varc
            .get_or_init(|| face.varc())
            .as_ref()
            .map_err(Clone::clone)?;
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
            CffTable::Cff(cff) => cff.outline(gid, &mut b)?,
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
                let vmtx = self.vmtx.get_or_init(|| face.vmtx().ok().flatten());
                let metrics = PhantomMetrics {
                    hmtx,
                    vmtx: vmtx.as_ref(),
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
/// HarfBuzz's charstring extents collect it: a move counts once a
/// segment starts from it, so a move that starts no segment does not
/// count.
struct ControlBox {
    min: (f32, f32),
    max: (f32, f32),
    /// The last move, until a segment starts from it.
    pending: Option<(f32, f32)>,
}

impl Default for ControlBox {
    fn default() -> Self {
        Self {
            min: (f32::INFINITY, f32::INFINITY),
            max: (f32::NEG_INFINITY, f32::NEG_INFINITY),
            pending: None,
        }
    }
}

impl ControlBox {
    fn add(&mut self, x: f32, y: f32) {
        self.min = (self.min.0.min(x), self.min.1.min(y));
        self.max = (self.max.0.max(x), self.max.1.max(y));
    }

    /// Adds the move a segment starts from, if it has not been added.
    fn start_segment(&mut self) {
        if let Some((x, y)) = self.pending.take() {
            self.add(x, y);
        }
    }

    /// Adds every point of `ops`.
    fn add_ops(&mut self, ops: &[PathOp]) {
        for op in ops {
            match *op {
                PathOp::MoveTo { x, y } => self.move_to(x, y),
                PathOp::LineTo { x, y } => self.line_to(x, y),
                PathOp::QuadTo { cx, cy, x, y } => self.quad_to(cx, cy, x, y),
                PathOp::CubicTo {
                    c1x,
                    c1y,
                    c2x,
                    c2y,
                    x,
                    y,
                } => self.curve_to(c1x, c1y, c2x, c2y, x, y),
                PathOp::Close => self.close(),
            }
        }
    }

    /// The box with each edge rounded as HarfBuzz's `roundf` rounds,
    /// halves up; a box with no width (or no height) has zero x extents
    /// (or y extents), as in HarfBuzz.
    fn extents(&self) -> Extents {
        let round = crate::tables::parse::hb_round;
        let mut e = Extents::default();
        if self.min.0 < self.max.0 {
            e.x_bearing = round(self.min.0);
            e.width = round(self.max.0) - e.x_bearing;
        }
        if self.min.1 < self.max.1 {
            e.y_bearing = round(self.max.1);
            e.height = round(self.min.1) - e.y_bearing;
        }
        e
    }
}

impl OutlineSink for ControlBox {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pending = Some((x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.start_segment();
        self.add(x, y);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.start_segment();
        self.add(cx, cy);
        self.add(x, y);
    }
    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
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
    use crate::Blob;

    fn control_box(ops: &[PathOp]) -> Extents {
        let mut b = ControlBox::default();
        b.add_ops(ops);
        b.extents()
    }

    #[test]
    fn cff_extents_match_the_box_of_the_drawn_outline() {
        // The extents run each charstring into a box with the tables
        // read once; they must equal the box of the outline the face
        // draws, glyph by glyph, at the default instance and away from
        // it, whatever order the glyphs come in.
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
                    let want = face
                        .glyph_outline_at_coords(gid, coords)
                        .unwrap()
                        .map_or_else(Extents::default, |o| control_box(o.ops()));
                    let got = tables.glyph_extents(&face, coords, &hmtx, gid).unwrap();
                    assert_eq!(got, Some(want), "glyph {gid} at {coords:?}");
                }
            }
        }
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
