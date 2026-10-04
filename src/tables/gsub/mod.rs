//! `GSUB`: Glyph Substitution.
//!
//! Rewrites the glyph stream before positioning: ligature
//! substitution replaces `(f, i)` with `ﬁ`, contextual alternates
//! pick different glyph shapes based on neighbors, and so on.
//! Every GSUB lookup type has a subtable parser in this module.
//!
//! The table header is identical to GPOS's: version + offsets to
//! `ScriptList`, `FeatureList`, and `LookupList`, then from version
//! 1.1 on an offset to `FeatureVariations` (see
//! [`Gsub::feature_variations`]). Each lookup's `lookupType` is
//! GSUB-specific, enumerated in [`lookup_type`].

use crate::buffer::ClusterLevel;
use crate::error::{Error, Result};
use crate::ot::layout_select::LayoutView;
use crate::tables::layout::accel::{accel_for, Accel, LayoutCache};
use crate::tables::layout::{
    ActiveFeatures, FeatureList, FeatureVariations, LayoutTable, Lookup, LookupList, ScriptList,
};
use crate::tables::parse::Reader;

/// Where a version 1.1 header holds `featureVariationsOffset`.
const FEATURE_VARIATIONS_OFFSET_FIELD: usize = 10;

pub mod alternate;
pub mod chain_context;
pub mod context;
pub mod ligature;
pub mod multiple;
pub mod reverse_chain;
pub mod single;

pub use alternate::Alternate;
pub use chain_context::{ChainContext, ChainContextAny, SubstLookupRecord};
pub use context::Context;
pub use ligature::Ligature;
pub use multiple::Multiple;
pub use reverse_chain::ReverseChain;
pub use single::Single;

/// GSUB lookup type numbers.
pub mod lookup_type {
    /// Single substitution (one to one). See [`super::Single`].
    pub const SINGLE: u16 = 1;
    /// Multiple substitution (one to many). See [`super::Multiple`].
    pub const MULTIPLE: u16 = 2;
    /// Alternate substitution (one to a choice of alternates). See
    /// [`super::Alternate`].
    pub const ALTERNATE: u16 = 3;
    /// Ligature substitution (many to one). See [`super::Ligature`].
    pub const LIGATURE: u16 = 4;
    /// Contextual substitution, formats 1, 2 and 3. See
    /// [`super::Context`].
    pub const CONTEXT: u16 = 5;
    /// Chained contextual substitution, formats 1, 2 and 3. See
    /// [`super::ChainContextAny`].
    pub const CHAINED_CONTEXT: u16 = 6;
    /// Extension substitution: forwards to another lookup type.
    pub const EXTENSION: u16 = 7;
    /// Reverse chained contextual single substitution. See
    /// [`super::ReverseChain`].
    pub const REVERSE_CHAINED: u16 = 8;
}

/// Parsed `GSUB`.
#[derive(Debug, Clone, Copy)]
pub struct Gsub<'a> {
    data: &'a [u8],
    script_list: ScriptList<'a>,
    feature_list: FeatureList<'a>,
    lookup_list: LookupList<'a>,
    /// `featureVariationsOffset` (version 1.1 and later), 0 for none,
    /// `None` for a version 1.1 header that ends before the field.
    feature_variations_offset: Option<u32>,
    /// The FeatureVariations and the record of them the shaper selected
    /// for the font's coordinates, whose substitutions every feature
    /// lookup sees.
    feature_variation: Option<(FeatureVariations<'a>, u32)>,
    /// Language system tags the shaper tries, in order, when it
    /// resolves a feature through this view. Empty selects each
    /// script's default language system.
    language_tags: &'a [[u8; 4]],
    /// What the font keeps for this table between shaping calls: the
    /// lookup accelerators, which let the shaper pass over lookups and
    /// subtables that cannot apply without parsing them, and the
    /// resolved language systems. `None` reads each lookup's coverages
    /// directly and walks the language system per query.
    cache: Option<&'a LayoutCache>,
    /// Cluster level of the run being shaped: a ligature merges its
    /// components' clusters only at the monotone levels, as
    /// HarfBuzz's `ligate_input` does through its buffer.
    cluster_level: ClusterLevel,
    /// Whether lookups record where glyphs are unsafe to concatenate,
    /// which only a buffer with `PRODUCE_UNSAFE_TO_CONCAT` asks for.
    unsafe_to_concat: bool,
}

impl<'a> Gsub<'a> {
    /// Parses a `GSUB` table. A version 1.1 header too short for its
    /// `featureVariationsOffset` still parses, and
    /// [`Self::feature_variations`] reports the missing field.
    ///
    /// # Errors
    ///
    /// [`Error::Truncated`] when the header ends before the LookupList
    /// offset, [`Error::Malformed`] for a major version other than 1 or
    /// a list offset past the end, and the errors of the ScriptList,
    /// FeatureList, and LookupList parsers.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported GSUB major version",
            });
        }
        let script_list_off = r.read_u16()? as usize;
        let feature_list_off = r.read_u16()? as usize;
        let lookup_list_off = r.read_u16()? as usize;
        // HarfBuzz rejects a version 1.1 table without the whole field,
        // and the shaper then leaves the table out (see
        // `feature_variations`), so a short header is not an error here.
        let feature_variations_offset = if minor >= 1 {
            r.read_u32().ok()
        } else {
            Some(0)
        };

        let script_list =
            ScriptList::parse(data.get(script_list_off..).ok_or(Error::Malformed {
                offset: script_list_off,
                context: "GSUB scriptList offset past end",
            })?)?;
        let feature_list =
            FeatureList::parse(data.get(feature_list_off..).ok_or(Error::Malformed {
                offset: feature_list_off,
                context: "GSUB featureList offset past end",
            })?)?;
        let lookup_list =
            LookupList::parse(data.get(lookup_list_off..).ok_or(Error::Malformed {
                offset: lookup_list_off,
                context: "GSUB lookupList offset past end",
            })?)?;

        Ok(Self {
            data,
            script_list,
            feature_list,
            lookup_list,
            feature_variations_offset,
            feature_variation: None,
            language_tags: &[],
            cache: None,
            cluster_level: ClusterLevel::MonotoneCharacters,
            unsafe_to_concat: false,
        })
    }

    /// The table's `FeatureVariations`, read when asked for. `Ok(None)`
    /// for a version 1.0 table or a null offset. Byte offsets in errors
    /// count from the start of the FeatureVariations table, except for
    /// an offset field that is cut short or points past the end of the
    /// GSUB, which reports the field.
    ///
    /// The shaper treats a font for which this returns an error as
    /// having no GSUB at all, as HarfBuzz 14.5.0 does (see
    /// [`crate::tables::layout::feature_variations`]).
    ///
    /// # Errors
    ///
    /// [`Error::Truncated`] at byte 10 for a version 1.1 header that
    /// ends before its `featureVariationsOffset`, [`Error::Malformed`]
    /// for an offset past the end of the table, and the errors of
    /// [`FeatureVariations::parse`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::Face;
    ///
    /// let data = include_bytes!("../../../tests/fixtures/rubik_vf.ttf");
    /// let face = Face::parse_bytes(data, 0)?;
    /// let gsub = face.gsub()?.expect("Rubik has GSUB");
    /// let variations = gsub.feature_variations()?.expect("GSUB 1.1");
    /// assert_eq!(variations.len(), 1);
    /// // Record 0 gives `rvrn` (feature 20) the lookup that swaps in
    /// // the heavy weights' currency signs.
    /// let rvrn = variations.substitute(0, 20).expect("substituted");
    /// assert_eq!(rvrn.lookup_indices().collect::<Vec<_>>(), [0]);
    /// # Ok::<(), sigilbuzz::Error>(())
    /// ```
    pub fn feature_variations(&self) -> Result<Option<FeatureVariations<'a>>> {
        let offset = self.feature_variations_offset.ok_or(Error::Truncated {
            offset: FEATURE_VARIATIONS_OFFSET_FIELD,
            context: "GSUB 1.1 header shorter than featureVariationsOffset",
        })?;
        crate::tables::layout::feature_variations::locate(
            self.data,
            offset,
            "GSUB featureVariations offset past end",
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

    /// Returns this view with what the font keeps for the table: the
    /// lookup accelerators [`Self::lookup_accel`] hands out and the
    /// language systems [`Self::layout_view`] resolves, each built the
    /// first time it is needed.
    #[must_use]
    pub(crate) const fn with_cache(mut self, cache: Option<&'a LayoutCache>) -> Self {
        self.cache = cache;
        self
    }

    /// True when the view has the font's cache, so its lookup
    /// accelerators are digests rather than coverages read per use.
    pub(crate) const fn has_cache(&self) -> bool {
        self.cache.is_some()
    }

    /// The table's length in bytes.
    pub(crate) const fn table_len(&self) -> usize {
        self.data.len()
    }

    /// What feature resolution (`crate::ot::layout_select`) reads of
    /// this view.
    pub(crate) fn layout_view(&self) -> LayoutView<'a> {
        LayoutView {
            script_list: self.script_list,
            features: self.features(),
            language_tags: self.language_tags,
            maps: self.cache.map(|c| &c.maps),
            plans: self.cache.map(|c| &c.plans),
        }
    }

    /// The accelerator of `lookup`, lookup `index` of the table: the
    /// font's, or one built for this use when the view has none.
    pub(crate) fn lookup_accel<'l>(&self, index: u16, lookup: &Lookup<'l>) -> Accel<'a, 'l> {
        accel_for(self.cache, LayoutTable::Gsub, index, lookup)
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
    /// let data = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
    /// let face = Face::parse_bytes(data, 0)?;
    /// let romanian = Language::new("ro").expect("non-empty tag");
    /// let gsub = face.gsub()?.expect("Open Sans has GSUB");
    /// let gsub = gsub.with_language_tags(romanian.ot_language_tags());
    /// assert_eq!(gsub.language_tags(), &[*b"ROM "]);
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

    /// Returns this view set to merge ligature clusters the way the
    /// shaped buffer's `level` asks.
    #[must_use]
    pub(crate) const fn with_cluster_level(mut self, level: ClusterLevel) -> Self {
        self.cluster_level = level;
        self
    }

    /// The cluster level set by [`Self::with_cluster_level`];
    /// [`ClusterLevel::MonotoneCharacters`] for a freshly parsed table.
    #[must_use]
    pub(crate) const fn cluster_level(&self) -> ClusterLevel {
        self.cluster_level
    }

    /// The same view for a buffer that asks for unsafe-to-concatenate
    /// glyph flags (`BufferFlags::PRODUCE_UNSAFE_TO_CONCAT`).
    #[must_use]
    pub(crate) const fn with_unsafe_to_concat(mut self, produce: bool) -> Self {
        self.unsafe_to_concat = produce;
        self
    }

    /// True when lookups record where glyphs are unsafe to concatenate.
    #[must_use]
    pub(crate) const fn unsafe_to_concat(&self) -> bool {
        self.unsafe_to_concat
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

    fn build_empty_gsub() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
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
    fn parses_empty_gsub_header() {
        let bytes = build_empty_gsub();
        let gsub = Gsub::parse(&bytes).unwrap();
        assert_eq!(gsub.script_list().len(), 0);
        assert_eq!(gsub.feature_list().len(), 0);
        assert_eq!(gsub.lookup_list().len(), 0);
    }

    #[test]
    fn rejects_unsupported_major_version() {
        let mut bytes = build_empty_gsub();
        bytes[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert!(matches!(Gsub::parse(&bytes), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(Gsub::parse(&[0u8; 5]).is_err());
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
        let table = Gsub::parse(&bytes).unwrap();
        assert_eq!(table.feature_list().len(), 0);
        let variations = table.feature_variations().unwrap();
        assert_eq!(variations.map(|v| v.len()), Some(0));
        // A null offset.
        let mut bytes = build_v11(&EMPTY_FV);
        bytes[10..14].copy_from_slice(&0u32.to_be_bytes());
        let table = Gsub::parse(&bytes).unwrap();
        assert!(table.feature_variations().unwrap().is_none());
        // Version 1.0 has no offset field, whatever follows the header.
        let mut bytes = build_v11(&EMPTY_FV);
        bytes[2..4].copy_from_slice(&0u16.to_be_bytes());
        let table = Gsub::parse(&bytes).unwrap();
        assert!(table.feature_variations().unwrap().is_none());
    }

    #[test]
    fn a_1_1_header_without_its_offset_parses_and_reports_it() {
        // The three lists share the empty one at byte 10, where the
        // offset field would start.
        let bytes = [0, 1, 0, 1, 0, 10, 0, 10, 0, 10, 0, 0, 0];
        for len in 12..=13 {
            let table = Gsub::parse(&bytes[..len]).unwrap();
            assert_eq!(table.script_list().len(), 0);
            assert_eq!(table.feature_list().len(), 0);
            assert_eq!(table.lookup_list().len(), 0);
            assert_eq!(
                table.feature_variations().unwrap_err(),
                Error::Truncated {
                    offset: 10,
                    context: "GSUB 1.1 header shorter than featureVariationsOffset",
                }
            );
        }
        // A 1.0 header has no such field.
        let mut bytes = bytes;
        bytes[3] = 0;
        let table = Gsub::parse(&bytes[..12]).unwrap();
        assert!(table.feature_variations().unwrap().is_none());
        // The header still needs its three list offsets.
        assert!(matches!(
            Gsub::parse(&bytes[..9]),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn reports_unreadable_feature_variations() {
        let mut bytes = build_v11(&EMPTY_FV);
        bytes[10..14].copy_from_slice(&100u32.to_be_bytes());
        let table = Gsub::parse(&bytes).unwrap();
        assert_eq!(
            table.feature_variations().unwrap_err(),
            Error::Malformed {
                offset: 10,
                context: "GSUB featureVariations offset past end",
            }
        );
        let bytes = build_v11(&[0, 2, 0, 0, 0, 0, 0, 0]);
        let table = Gsub::parse(&bytes).unwrap();
        assert!(matches!(
            table.feature_variations(),
            Err(Error::Malformed { offset: 0, .. })
        ));
        let bytes = build_v11(&[0, 1, 0, 0, 0, 0, 0, 1]);
        let table = Gsub::parse(&bytes).unwrap();
        assert!(matches!(
            table.feature_variations(),
            Err(Error::Truncated { offset: 8, .. })
        ));
    }
}
