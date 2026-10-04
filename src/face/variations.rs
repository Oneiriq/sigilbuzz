//! `Face` accessors for the font variation tables (`fvar`, `avar`,
//! `HVAR`, `gvar`, `MVAR`, `VVAR`, `VARC`).

use super::Face;
use crate::error::{Error, Result};
use crate::tables::{tag, Avar, Fvar, Gvar, Hvar, Mvar, Varc, Vvar};

impl<'a> Face<'a> {
    /// Parses the `fvar` table if the font carries one. Presence
    /// of `fvar` is the definitive signal that the font is a
    /// variable font; static fonts yield `Ok(None)`.
    pub fn fvar(&self) -> Result<Option<Fvar>> {
        match self.table_bytes(tag::FVAR) {
            Ok(bytes) => Ok(Some(Fvar::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `avar` table if the font carries one. Optional
    /// even in variable fonts: `avar` only appears when the
    /// designer provides non-linear axis remapping.
    pub fn avar(&self) -> Result<Option<Avar>> {
        match self.table_bytes(tag::AVAR) {
            Ok(bytes) => Ok(Some(Avar::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `HVAR` table if the font carries one. Variable
    /// fonts with varying advances include this; fixed-metric
    /// variable fonts omit it (their advances don't change across
    /// the design space).
    pub fn hvar(&self) -> Result<Option<Hvar<'a>>> {
        match self.table_bytes(tag::HVAR) {
            Ok(bytes) => Ok(Some(Hvar::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `gvar` table if the font carries one. Present in
    /// every TrueType variable font with a `glyf` outline table;
    /// CFF2-based variable fonts carry their own `CFF2` deltas
    /// instead.
    pub fn gvar(&self) -> Result<Option<Gvar<'a>>> {
        match self.table_bytes(tag::GVAR) {
            Ok(bytes) => Ok(Some(Gvar::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `MVAR` table if the font carries one. MVAR varies
    /// font-wide instance metrics (typoAscender / Descender,
    /// x-height, sub/super offsets, strikeout, underline, ...) by
    /// axis coord; missing MVAR means those metrics stay constant
    /// across the design space. Most variable fonts ship MVAR.
    pub fn mvar(&self) -> Result<Option<Mvar<'a>>> {
        match self.table_bytes(tag::MVAR) {
            Ok(bytes) => Ok(Some(Mvar::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `VVAR` table if the font carries one. HVAR's
    /// vertical sibling: varies per-glyph advance height and
    /// top-side bearing. Only fonts that support vertical layout
    /// (CJK, vertical Latin) ship this; horizontal-only variable
    /// fonts omit it.
    pub fn vvar(&self) -> Result<Option<Vvar<'a>>> {
        match self.table_bytes(tag::VVAR) {
            Ok(bytes) => Ok(Some(Vvar::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `VARC` table if the font carries one. VARC adds
    /// per-axis variation deltas to composite-glyph component
    /// transforms and to each component's effective coord vector;
    /// shipped to date only by Chrome's experimental VARC fonts and
    /// a handful of demo files. Returns `Ok(None)` for the vast
    /// majority of fonts that don't carry it.
    ///
    /// The outline methods, [`crate::GlyphOutlines`] and shaping treat a
    /// `VARC` table this fails to parse as absent, as HarfBuzz's
    /// sanitizer drops it, and draw every glyph from `glyf` or CFF.
    pub fn varc(&self) -> Result<Option<Varc<'a>>> {
        match self.table_bytes(tag::VARC) {
            Ok(bytes) => Ok(Some(Varc::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The `VARC` table glyphs are drawn through: [`Face::varc`], with a
    /// table that does not parse (or whose bytes the directory does not
    /// hold) read as absent. HarfBuzz 14.5.0 drops a `VARC` its
    /// sanitizer rejects, such as one in the MultiItemVariationStore
    /// layout HarfBuzz adopted after 14.5.0, and draws and shapes the
    /// font as if it had none; failing every glyph instead would leave
    /// the font unusable.
    pub(crate) fn drawable_varc(&self) -> Option<Varc<'a>> {
        self.varc().ok().flatten()
    }
}
