//! `GPOS`: Glyph Positioning.
//!
//! Refines the advances and placements produced by cmap+hmtx with
//! feature-driven deltas: kerning (lookup type 2), cursive
//! attachment (type 3), mark-to-base attachment (type 4), and so
//! on. This module parses every lookup type except cursive
//! attachment (type 3), which the shaper does not apply.
//!
//! The table header is shared with GSUB: version + offsets to
//! `ScriptList`, `FeatureList`, and `LookupList`, then from version
//! 1.1 on an offset to `FeatureVariations` (see
//! [`Gpos::feature_variations`]). Each lookup in the `LookupList` has
//! a `lookupType` (see [`lookup_type`]) that determines how its
//! subtables are parsed.

use crate::error::{Error, Result};
use crate::tables::layout::{
    ActiveFeatures, FeatureList, FeatureVariations, LookupList, ScriptList,
};
use crate::tables::parse::Reader;

pub mod anchor;
pub mod chain_context;
pub mod context;
pub mod cursive;
pub mod mark_base;
pub mod mark_liga;
pub mod mark_mark;
pub mod pair_pos;
pub mod single_adj;
pub mod value_record;

pub use anchor::Anchor;
pub use chain_context::ChainContextPos;
pub use context::ContextPos;
pub use cursive::CursivePos;
pub use mark_base::{MarkAttachment, MarkBasePos};
pub use mark_liga::MarkLigaPos;
pub use mark_mark::MarkMarkPos;
pub use pair_pos::{PairPos, PairPosFormat1, PairPosFormat2};
pub use single_adj::SinglePos;
pub use value_record::{resolve_variation_delta, ValueRecord};

/// Canonical GPOS lookup type numbers.
pub mod lookup_type {
    /// Single adjustment. See [`super::SinglePos`].
    pub const SINGLE_ADJUSTMENT: u16 = 1;
    /// Pair adjustment (kerning). See [`super::PairPos`].
    pub const PAIR_ADJUSTMENT: u16 = 2;
    /// Cursive attachment: entry/exit anchors joining adjacent glyphs.
    pub const CURSIVE_ATTACHMENT: u16 = 3;
    /// Mark-to-base attachment. See [`super::MarkBasePos`].
    pub const MARK_TO_BASE: u16 = 4;
    /// Mark-to-ligature attachment. See [`super::MarkLigaPos`].
    pub const MARK_TO_LIGATURE: u16 = 5;
    /// Mark-to-mark attachment. See [`super::MarkMarkPos`].
    pub const MARK_TO_MARK: u16 = 6;
    /// Context positioning, formats 1, 2 and 3. See
    /// [`super::ContextPos`].
    pub const CONTEXT: u16 = 7;
    /// Chained context positioning, formats 1, 2 and 3. See
    /// [`super::ChainContextPos`].
    pub const CHAINED_CONTEXT: u16 = 8;
    /// Extension positioning: forwards to another lookup type.
    pub const EXTENSION: u16 = 9;
}

/// Parsed `GPOS`.
#[derive(Debug, Clone, Copy)]
pub struct Gpos<'a> {
    data: &'a [u8],
    script_list: ScriptList<'a>,
    feature_list: FeatureList<'a>,
    lookup_list: LookupList<'a>,
    /// `featureVariationsOffset` (version 1.1 and later), 0 for none.
    feature_variations_offset: u32,
    /// The FeatureVariations and the record of them the shaper selected
    /// for the font's coordinates, whose substitutions every feature
    /// lookup sees.
    feature_variation: Option<(FeatureVariations<'a>, u32)>,
    /// Language system tags the shaper tries, in order, when it
    /// resolves a feature through this view. Empty selects each
    /// script's default language system.
    language_tags: &'a [[u8; 4]],
}

impl<'a> Gpos<'a> {
    /// Parses a `GPOS` table.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported GPOS major version",
            });
        }
        let script_list_off = r.read_u16()? as usize;
        let feature_list_off = r.read_u16()? as usize;
        let lookup_list_off = r.read_u16()? as usize;
        let feature_variations_offset = if minor >= 1 {
            r.read_u32().map_err(|_| Error::Truncated {
                offset: r.position(),
                context: "GPOS 1.1 header shorter than featureVariationsOffset",
            })?
        } else {
            0
        };

        let script_list =
            ScriptList::parse(data.get(script_list_off..).ok_or(Error::Malformed {
                offset: script_list_off,
                context: "GPOS scriptList offset past end",
            })?)?;
        let feature_list =
            FeatureList::parse(data.get(feature_list_off..).ok_or(Error::Malformed {
                offset: feature_list_off,
                context: "GPOS featureList offset past end",
            })?)?;
        let lookup_list =
            LookupList::parse(data.get(lookup_list_off..).ok_or(Error::Malformed {
                offset: lookup_list_off,
                context: "GPOS lookupList offset past end",
            })?)?;

        Ok(Self {
            data,
            script_list,
            feature_list,
            lookup_list,
            feature_variations_offset,
            feature_variation: None,
            language_tags: &[],
        })
    }

    /// The table's `FeatureVariations`, read when asked for. `Ok(None)`
    /// for a version 1.0 table or a null offset. Byte offsets in errors
    /// count from the start of the FeatureVariations table, except for
    /// an offset past the end of the GPOS, which reports the offset
    /// field.
    ///
    /// The shaper treats a font whose FeatureVariations fail to parse as
    /// having no GPOS at all, as HarfBuzz 14.5.0 does (see
    /// [`crate::tables::layout::feature_variations`]).
    ///
    /// # Errors
    ///
    /// [`Error::Malformed`] for an offset past the end of the table and
    /// the errors of [`FeatureVariations::parse`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::Face;
    ///
    /// // Amiri's GPOS is version 1.0, with no FeatureVariations.
    /// let data = include_bytes!("../../../tests/fixtures/amiri_regular.ttf");
    /// let face = Face::parse_bytes(data, 0)?;
    /// let gpos = face.gpos()?.expect("Amiri has GPOS");
    /// assert!(gpos.feature_variations()?.is_none());
    /// # Ok::<(), sigilbuzz::Error>(())
    /// ```
    pub fn feature_variations(&self) -> Result<Option<FeatureVariations<'a>>> {
        crate::tables::layout::feature_variations::locate(
            self.data,
            self.feature_variations_offset,
            "GPOS featureVariations offset past end",
        )
    }

    /// Returns this view with `variation`, the table's FeatureVariations
    /// and the index of the record that applies (see
    /// [`FeatureVariations::find_index`]), selected: every feature the
    /// record substitutes takes its lookups from the record's alternate
    /// Feature table. `None` selects no record.
    #[must_use]
    pub(crate) const fn with_feature_variation(
        mut self,
        variation: Option<(FeatureVariations<'a>, u32)>,
    ) -> Self {
        self.feature_variation = variation;
        self
    }

    /// The FeatureList as the shaper sees it: with the substitutions of
    /// the record [`Self::with_feature_variation`] selected, if any.
    pub(crate) const fn features(&self) -> ActiveFeatures<'a> {
        ActiveFeatures::new(self.feature_list, self.feature_variation)
    }

    /// Returns this view with a language system preference: the
    /// shaper resolves features under each script's language system
    /// for the first of `tags` the font has (see
    /// [`crate::tables::layout::Script::select_lang_sys`]) instead of
    /// the default one.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::{Face, Language};
    ///
    /// let data = include_bytes!("../../../tests/fixtures/amiri_regular.ttf");
    /// let face = Face::parse_bytes(data, 0)?;
    /// let urdu = Language::new("ur").expect("non-empty tag");
    /// let gpos = face.gpos()?.expect("Amiri has GPOS");
    /// let gpos = gpos.with_language_tags(urdu.ot_language_tags());
    /// assert_eq!(gpos.language_tags(), &[*b"URD "]);
    /// # Ok::<(), sigilbuzz::Error>(())
    /// ```
    #[must_use]
    pub const fn with_language_tags(mut self, tags: &'a [[u8; 4]]) -> Self {
        self.language_tags = tags;
        self
    }

    /// The language system preference set by
    /// [`Self::with_language_tags`]. Empty for a freshly parsed table.
    #[must_use]
    pub const fn language_tags(&self) -> &'a [[u8; 4]] {
        self.language_tags
    }

    /// Returns the parsed `ScriptList`.
    #[must_use]
    pub const fn script_list(&self) -> &ScriptList<'a> {
        &self.script_list
    }

    /// Returns the parsed `FeatureList`.
    #[must_use]
    pub const fn feature_list(&self) -> &FeatureList<'a> {
        &self.feature_list
    }

    /// Returns the parsed `LookupList`.
    #[must_use]
    pub const fn lookup_list(&self) -> &LookupList<'a> {
        &self.lookup_list
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Builds a minimal GPOS table where ScriptList, FeatureList,
    /// and LookupList are all empty. Useful for smoke-testing the
    /// top-level header parser without also having to construct the
    /// child tables.
    fn build_empty_gpos() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
                                                    // Layout: header (10 bytes) + 3 empty tables each starting
                                                    // with a u16 count = 0.
        let header_len = 10u16;
        let sl_off = header_len;
        let fl_off = sl_off + 2;
        let ll_off = fl_off + 2;
        out.extend_from_slice(&sl_off.to_be_bytes());
        out.extend_from_slice(&fl_off.to_be_bytes());
        out.extend_from_slice(&ll_off.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // scriptCount
        out.extend_from_slice(&0u16.to_be_bytes()); // featureCount
        out.extend_from_slice(&0u16.to_be_bytes()); // lookupCount
        out
    }

    #[test]
    fn parses_empty_gpos_header() {
        let bytes = build_empty_gpos();
        let gpos = Gpos::parse(&bytes).unwrap();
        assert_eq!(gpos.script_list().len(), 0);
        assert_eq!(gpos.feature_list().len(), 0);
        assert_eq!(gpos.lookup_list().len(), 0);
    }

    #[test]
    fn rejects_unsupported_major_version() {
        let mut bytes = build_empty_gpos();
        bytes[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert!(matches!(Gpos::parse(&bytes), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(Gpos::parse(&[0u8; 5]).is_err());
    }

    /// A version 1.1 table with empty lists and `fv` at byte 20.
    fn build_v11(fv: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&1u16.to_be_bytes()); // minor
        out.extend_from_slice(&14u16.to_be_bytes()); // scriptList
        out.extend_from_slice(&16u16.to_be_bytes()); // featureList
        out.extend_from_slice(&18u16.to_be_bytes()); // lookupList
        out.extend_from_slice(&20u32.to_be_bytes()); // featureVariations
        out.extend_from_slice(&[0; 6]); // three empty lists
        out.extend_from_slice(fv);
        out
    }

    /// An empty FeatureVariations 1.0.
    const EMPTY_FV: [u8; 8] = [0, 1, 0, 0, 0, 0, 0, 0];

    #[test]
    fn reads_feature_variations_from_a_1_1_header() {
        let bytes = build_v11(&EMPTY_FV);
        let table = Gpos::parse(&bytes).unwrap();
        assert_eq!(table.feature_list().len(), 0);
        let variations = table.feature_variations().unwrap();
        assert_eq!(variations.map(|v| v.len()), Some(0));
        // A null offset.
        let mut bytes = build_v11(&EMPTY_FV);
        bytes[10..14].copy_from_slice(&0u32.to_be_bytes());
        let table = Gpos::parse(&bytes).unwrap();
        assert!(table.feature_variations().unwrap().is_none());
        // Version 1.0 has no offset field, whatever follows the header.
        let mut bytes = build_v11(&EMPTY_FV);
        bytes[2..4].copy_from_slice(&0u16.to_be_bytes());
        let table = Gpos::parse(&bytes).unwrap();
        assert!(table.feature_variations().unwrap().is_none());
    }

    #[test]
    fn rejects_a_1_1_header_without_its_offset() {
        let bytes = build_v11(&EMPTY_FV);
        for len in 10..14 {
            assert_eq!(
                Gpos::parse(&bytes[..len]).unwrap_err(),
                Error::Truncated {
                    offset: 10,
                    context: "GPOS 1.1 header shorter than featureVariationsOffset",
                }
            );
        }
    }

    #[test]
    fn reports_unreadable_feature_variations() {
        let mut bytes = build_v11(&EMPTY_FV);
        bytes[10..14].copy_from_slice(&100u32.to_be_bytes());
        let table = Gpos::parse(&bytes).unwrap();
        assert_eq!(
            table.feature_variations().unwrap_err(),
            Error::Malformed {
                offset: 10,
                context: "GPOS featureVariations offset past end",
            }
        );
        let bytes = build_v11(&[0, 2, 0, 0, 0, 0, 0, 0]);
        let table = Gpos::parse(&bytes).unwrap();
        assert!(matches!(
            table.feature_variations(),
            Err(Error::Malformed { offset: 0, .. })
        ));
        let bytes = build_v11(&[0, 1, 0, 0, 0, 0, 0, 1]);
        let table = Gpos::parse(&bytes).unwrap();
        assert!(matches!(
            table.feature_variations(),
            Err(Error::Truncated { offset: 8, .. })
        ));
    }
}
