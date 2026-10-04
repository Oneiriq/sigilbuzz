//! [`GlyphOutlines`]: many glyphs of one face drawn at one instance,
//! with the outline tables read once.

use core::cell::OnceCell;
use core::fmt;

use alloc::vec::Vec;

use super::varc_draw::VarcDraw;
use super::Face;
use crate::error::Result;
use crate::font::f2dot14_coords;
use crate::tables::cff2::Cff2Shared;
use crate::tables::glyf::PhantomMetrics;
use crate::tables::{tag, Cff, Cff2, Glyf, Gvar, Hmtx, Loca, Outline, OutlineSink, Varc, Vmtx};

/// Draws glyphs of one face at one set of variation coords, reading the
/// outline tables once for all of them.
///
/// [`Face::glyph_outline_at_coords`] reads the tables for every glyph,
/// which for a `CFF2` font means parsing the table, its variation store,
/// its Private DICTs, and the region scalars of the coords again and
/// again. A run of text draws many glyphs at one instance, so this keeps
/// them: the `CFF2` or `CFF ` table, or `glyf` with `loca`, `gvar` and
/// the metrics its phantom points read, and `VARC`. Each glyph comes out
/// exactly as [`Face::glyph_outline_at_coords`] draws it, errors
/// included: a table that does not read fails every glyph that needs it,
/// except `VARC`, which counts as absent then, as for the face.
///
/// It caches through cells that are not thread-safe, so it is `Send` but
/// not `Sync`: build one per thread.
///
/// ```
/// use sigilbuzz::Face;
///
/// let data = include_bytes!("../../tests/fixtures/noto_sans_kr_vf_cff2_subset.otf");
/// let face = Face::parse_bytes(data, 0)?;
/// let coords = [0.5];
/// let outlines = face.glyph_outlines(&coords);
/// for gid in 0..face.maxp()?.num_glyphs {
///     assert_eq!(
///         outlines.outline(gid)?,
///         face.glyph_outline_at_coords(gid, &coords)?,
///     );
/// }
/// # Ok::<(), sigilbuzz::Error>(())
/// ```
pub struct GlyphOutlines<'a> {
    face: Face<'a>,
    /// The coords as shaping reads them, rounded to F2DOT14.
    coords: Vec<f32>,
    /// The face's `VARC`, `None` when it has none or it does not parse.
    varc: OnceCell<Option<Varc<'a>>>,
    tables: OnceCell<Result<Tables<'a>>>,
}

/// The table the glyphs are drawn from, chosen as
/// [`Face::glyph_outline_at_coords`] chooses it.
enum Tables<'a> {
    /// With what its glyphs share at the coords.
    Cff2(Cff2<'a>, Cff2Shared<'a>),
    Cff(Cff<'a>),
    Glyf {
        glyf: Glyf<'a>,
        loca: Loca<'a>,
        hmtx: Hmtx<'a>,
        /// The font's `vmtx`, or HarfBuzz's metrics for a font without.
        vmtx: Vmtx<'a>,
        /// `None` at the default instance.
        gvar: Option<Gvar<'a>>,
    },
}

impl<'a> Face<'a> {
    /// Returns a [`GlyphOutlines`] that draws this face's glyphs at the
    /// normalized variation coords `coords`, reading the outline tables
    /// once. The coords are rounded to F2DOT14 as
    /// [`Face::glyph_outline_at_coords`] rounds them; an empty slice
    /// draws the default instance.
    #[must_use]
    pub fn glyph_outlines(&self, coords: &[f32]) -> GlyphOutlines<'a> {
        GlyphOutlines {
            face: self.clone(),
            coords: f2dot14_coords(coords),
            varc: OnceCell::new(),
            tables: OnceCell::new(),
        }
    }
}

impl<'a> GlyphOutlines<'a> {
    /// The outline of `glyph_id`, as [`Face::glyph_outline_at_coords`]
    /// returns it: `Ok(None)` for a glyph with no outline or past the
    /// end of the outline table.
    ///
    /// # Errors
    ///
    /// The errors [`Face::glyph_outline_at_coords`] returns for the
    /// glyph.
    pub fn outline(&self, glyph_id: u16) -> Result<Option<Outline>> {
        let mut out = Outline::new();
        let drew = self.draw(glyph_id, &mut out)?;
        Ok(drew.then_some(out))
    }

    /// Draws `glyph_id` into `sink` and returns whether the glyph has an
    /// outline, the ops [`GlyphOutlines::outline`] would return, without
    /// collecting them. A glyph that fails partway may have sent some
    /// ops to `sink` before the error.
    ///
    /// # Errors
    ///
    /// The errors [`Face::glyph_outline_at_coords`] returns for the
    /// glyph.
    pub fn draw<S: OutlineSink>(&self, glyph_id: u16, sink: &mut S) -> Result<bool> {
        let varc = self.varc.get_or_init(|| self.face.drawable_varc());
        // A VARC composite is drawn by the walk the face draws it with:
        // its components carry coords of their own, so its leaves come
        // from tables the walk reads for them.
        if let Some(varc) = varc {
            if VarcDraw::new(&self.face, varc, &self.coords).draw(glyph_id, sink)? {
                return Ok(true);
            }
        }
        let tables = self
            .tables
            .get_or_init(|| self.read_tables())
            .as_ref()
            .map_err(Clone::clone)?;
        match tables {
            Tables::Cff2(cff2, shared) => cff2.outline_shared(glyph_id, &self.coords, shared, sink),
            Tables::Cff(cff) => cff.outline(glyph_id, sink),
            Tables::Glyf {
                glyf,
                loca,
                hmtx,
                vmtx,
                gvar,
            } => {
                let metrics = PhantomMetrics {
                    hmtx,
                    vmtx: Some(vmtx),
                };
                glyf.outline_at_coords(
                    loca,
                    glyph_id,
                    gvar.as_ref(),
                    &self.coords,
                    Some(&metrics),
                    sink,
                )
            }
        }
    }

    /// Reads the table the glyphs come from, in the order
    /// [`Face::glyph_outline_at_coords`] reads them.
    fn read_tables(&self) -> Result<Tables<'a>> {
        let face = &self.face;
        if face.record(tag::CFF2).is_some() {
            return Ok(Tables::Cff2(face.cff2()?, Cff2Shared::default()));
        }
        if face.record(tag::CFF1).is_some() && face.record(tag::GLYF).is_none() {
            return Ok(Tables::Cff(face.cff()?));
        }
        let loca = face.loca()?;
        let glyf = face.glyf()?;
        let hmtx = face.hmtx()?;
        let vmtx = face.vmtx()?;
        let vmtx = face.phantom_vmtx(vmtx)?;
        let gvar = if self.coords.is_empty() {
            None
        } else {
            face.gvar()?
        };
        Ok(Tables::Glyf {
            glyf,
            loca,
            hmtx,
            vmtx,
            gvar,
        })
    }
}

impl fmt::Debug for GlyphOutlines<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GlyphOutlines")
            .field("coords", &self.coords)
            .finish_non_exhaustive()
    }
}
