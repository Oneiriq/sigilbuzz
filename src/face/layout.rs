//! `Face` accessors for the OpenType and AAT layout tables (`GDEF`,
//! `GPOS`, `GSUB`, `kern`, `morx`, `kerx`, `ankr`) and for `MATH` and
//! `BASE`.

use super::Face;
use crate::error::{Error, Result};
use crate::tables::{tag, Ankr, Base, Gdef, Gpos, Gsub, KernTable, Kerx, Math, Morx};

impl<'a> Face<'a> {
    /// Parses the `GDEF` table if the font carries one. Fonts without
    /// `GDEF` get `Ok(None)`. The shaper handles missing `GDEF` by
    /// treating every glyph as a base, which is the OpenType default.
    pub fn gdef(&self) -> Result<Option<Gdef<'a>>> {
        match self.table_bytes(tag::GDEF) {
            Ok(bytes) => Ok(Some(Gdef::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `GPOS` table if the font carries one. Returns
    /// `Ok(None)` when the font has no positioning features. Not
    /// every font does, and kerning-less output is still valid.
    pub fn gpos(&self) -> Result<Option<Gpos<'a>>> {
        match self.table_bytes(tag::GPOS) {
            Ok(bytes) => Ok(Some(Gpos::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the legacy `kern` table if the font carries one. Used
    /// as a fallback when the GPOS `kern` feature yields no lookups
    /// since many older fonts (Open Sans among them) ship their
    /// kerning here rather than in GPOS.
    pub fn kern(&self) -> Result<Option<KernTable<'a>>> {
        match self.table_bytes(tag::KERN) {
            Ok(bytes) => Ok(Some(KernTable::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the AAT `morx` table if the font carries one.
    /// sigilbuzz reaches for `morx` only when the font has no GSUB,
    /// which is the same policy HarfBuzz uses; AAT-only fonts (some
    /// legacy macOS system fonts, most third-party AAT designs) are
    /// the common case.
    pub fn morx(&self) -> Result<Option<Morx<'a>>> {
        match self.table_bytes(tag::MORX) {
            Ok(bytes) => Ok(Some(Morx::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the AAT `kerx` table if the font carries one. Used
    /// only when the font has no GPOS kern feature, so mainstream
    /// fonts are unaffected. `kerx` format-2 needs `numGlyphs` to
    /// bound-check format-0 class lookups, so we plumb it through
    /// from `maxp`.
    pub fn kerx(&self) -> Result<Option<Kerx<'a>>> {
        match self.table_bytes(tag::KERX) {
            Ok(bytes) => {
                let num_glyphs = self.maxp()?.num_glyphs;
                Ok(Some(Kerx::parse(bytes, num_glyphs)?))
            }
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the AAT `ankr` (Anchor Point) table if the font carries
    /// one. Pairs with `kerx` format-4 action type 1: the kerx state
    /// machine emits `(mark_anchor_idx, current_anchor_idx)` pairs and
    /// the apply path resolves them through `ankr.anchor_for(gid, idx)`
    /// into concrete `(x, y)` design-unit coordinates.
    pub fn ankr(&self) -> Result<Option<Ankr<'a>>> {
        match self.table_bytes(tag::ANKR) {
            Ok(bytes) => Ok(Some(Ankr::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `GSUB` table if the font carries one. Returns
    /// `Ok(None)` when the font has no glyph substitution features.
    pub fn gsub(&self) -> Result<Option<Gsub<'a>>> {
        match self.table_bytes(tag::GSUB) {
            Ok(bytes) => Ok(Some(Gsub::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `MATH` table if the font carries one. Math
    /// typography fonts (STIX 2 Math, Latin Modern Math, Cambria
    /// Math, Asana Math, XITS Math) ship this; everything else
    /// returns `Ok(None)`. sigilbuzz exposes the parsed structure;
    /// running an actual math layout pass is the consumer's job
    /// (LuaTeX, MathML renderers, ...).
    pub fn math(&self) -> Result<Option<Math<'a>>> {
        match self.table_bytes(tag::MATH) {
            Ok(bytes) => Ok(Some(Math::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `BASE` table if the font carries one. BASE
    /// provides per-script baseline metrics (`romn`, `ideo`,
    /// `hang`, `math`, ...) plus min/max clamps so a typesetting
    /// engine can align glyphs from different scripts on a common
    /// baseline. Most fonts omit BASE. Adobe's flagship faces,
    /// some Noto / SIL designs, and a handful of math fonts ship
    /// it. v1.1 BASE tables can carry IVS-varied baseline coords;
    /// the parsed [`Base`] exposes its variation store via
    /// [`Base::variation_store`].
    pub fn base(&self) -> Result<Option<Base<'a>>> {
        match self.table_bytes(tag::BASE) {
            Ok(bytes) => Ok(Some(Base::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
