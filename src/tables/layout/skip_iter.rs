//! HarfBuzz's glyph matching rules for GSUB and GPOS lookups.
//!
//! A lookup does not see every glyph of the run. Matching the input
//! of a ligature or a contextual rule, finding a pair's second glyph,
//! or walking back to a mark's base all go through HarfBuzz's
//! skipping iterator (`hb_ot_apply_context_t::skipping_iterator_t` in
//! `hb-ot-layout-gsubgpos.hh`), which decides per glyph whether to
//! skip it, match it, or stop:
//!
//! - **Lookup flags.** A glyph whose class the lookup ignores is
//!   skipped outright (`check_glyph_property`):
//!
//!   ```text
//!     0x0002  IgnoreBaseGlyphs      skip base glyphs
//!     0x0004  IgnoreLigatures       skip ligature glyphs
//!     0x0008  IgnoreMarks           skip marks
//!     0x0010  UseMarkFilteringSet   skip marks outside the named GDEF set
//!     0xFF00  MarkAttachmentType    skip marks of another attachment class
//!   ```
//!
//!   The class comes from GDEF's `GlyphClassDef` when the font has
//!   one. Otherwise the shaper synthesizes it from Unicode general
//!   categories and records it in the glyph's props (see
//!   [`GlyphClasses`]). An unclassified glyph (class 0 in GDEF) is
//!   none of base, ligature or mark, so no flag skips it.
//! - **Default ignorables.** An unsubstituted default-ignorable
//!   character (ZWJ, ZWNJ, soft hyphen, variation selectors, ...) is
//!   skipped *unless the rule asks for it*: when the rule's glyph at
//!   that step matches the ignorable, the ignorable is matched instead
//!   (`may_skip` returns `SKIP_MAYBE`). ZWNJ, ZWJ and "hidden"
//!   characters (CGJ, Mongolian variation selectors, tag characters)
//!   are exempt in some walks, per [`MatchContext::input`] and
//!   [`MatchContext::context`].
//!
//! Every walk reads the run as a slice of [`MatchGlyph`]s: the glyph
//! id plus the shaper's per-glyph props, laid out as in
//! [`match_prop`]. The shaper keeps those props in
//! `Glyph::unicode_props`, so a [`MatchGlyph`] is a copy of two fields.

use crate::tables::gdef::Gdef;
use crate::tables::layout::Coverage;

mod sequence;

pub use sequence::{
    apply_nested, match_backtrack, match_input, match_lookahead, InputMatch, MatchPositions,
    MAX_CONTEXT_LENGTH,
};
pub(crate) use sequence::{match_backtrack_in, match_input_in, match_lookahead_in};

/// `LookupFlag`: `RightToLeft` bit. Indicates the lookup runs in
/// RTL direction. Only GPOS type 3 (cursive) uses it; matching does
/// not.
pub const LOOKUP_FLAG_RIGHT_TO_LEFT: u16 = 0x0001;
/// `LookupFlag`: `IgnoreBaseGlyphs`. Skip base glyphs during
/// matching.
pub const LOOKUP_FLAG_IGNORE_BASE_GLYPHS: u16 = 0x0002;
/// `LookupFlag`: `IgnoreLigatures`. Skip ligature glyphs during
/// matching.
pub const LOOKUP_FLAG_IGNORE_LIGATURES: u16 = 0x0004;
/// `LookupFlag`: `IgnoreMarks`. Skip marks during matching.
pub const LOOKUP_FLAG_IGNORE_MARKS: u16 = 0x0008;
/// `LookupFlag`: `UseMarkFilteringSet`. When set, the lookup's
/// trailing `markFilteringSet` u16 indexes a `MarkGlyphSetsDef`
/// coverage in GDEF; marks *not* in that coverage are skipped.
pub const LOOKUP_FLAG_USE_MARK_FILTERING_SET: u16 = 0x0010;

/// Mask isolating the `MarkAttachmentType` byte (high byte of the
/// 16-bit flag). Non-zero values restrict marks to those with the
/// same GDEF mark-attachment class.
pub const LOOKUP_FLAG_MARK_ATTACHMENT_TYPE_MASK: u16 = 0xFF00;

/// The three class-based ignore flags.
const IGNORE_FLAGS: u16 =
    LOOKUP_FLAG_IGNORE_BASE_GLYPHS | LOOKUP_FLAG_IGNORE_LIGATURES | LOOKUP_FLAG_IGNORE_MARKS;

/// Bits of [`MatchGlyph::props`], the same layout the shaper keeps in
/// [`Glyph::unicode_props`](crate::Glyph::unicode_props), whose docs
/// map all sixteen.
///
/// The low byte holds the matching properties HarfBuzz keeps in
/// `unicode_props` and `glyph_props`; the high byte is HarfBuzz's
/// `lig_props` byte verbatim.
pub mod match_prop {
    /// Unsubstituted default-ignorable character (the same bit as
    /// `crate::buffer::unicode_prop::DEFAULT_IGNORABLE`).
    pub const DEFAULT_IGNORABLE: u16 = 1 << 0;
    /// The character is ZWJ (`unicode_prop::JOINER`).
    pub const ZWJ: u16 = 1 << 1;
    /// The character is ZWNJ (`unicode_prop::NON_JOINER`).
    pub const ZWNJ: u16 = 1 << 2;
    /// HarfBuzz's `UPROPS_MASK_HIDDEN`: a default ignorable GSUB must
    /// still see (CGJ, the Mongolian free variation selectors, tag
    /// characters). GPOS skips it like any other ignorable.
    pub const HIDDEN: u16 = 1 << 3;
    /// Mask of the synthesized glyph class, the class
    /// `hb_synthesize_glyph_classes` and later substitutions record
    /// for fonts without a GDEF `GlyphClassDef`. Zero is a base glyph.
    pub const SYNTHESIZED_CLASS: u16 = 0b11 << 4;
    /// Synthesized class value: ligature glyph.
    pub const SYNTHESIZED_LIGATURE: u16 = 1 << 4;
    /// Synthesized class value: mark.
    pub const SYNTHESIZED_MARK: u16 = 2 << 4;
    /// HarfBuzz's `HB_OT_LAYOUT_GLYPH_PROPS_LIGATED`: a ligature
    /// substitution produced the glyph, whether or not it counts as a
    /// ligature for component tracking.
    pub const LIGATED: u16 = 1 << 6;
    /// HarfBuzz's `HB_OT_LAYOUT_GLYPH_PROPS_MULTIPLIED`: the glyph came
    /// out of a multiple substitution.
    pub const MULTIPLIED: u16 = 1 << 7;
    /// Shift of the `lig_props` byte.
    pub const LIG_PROPS_SHIFT: u32 = 8;
    /// `lig_props` flag: the glyph is the ligature itself, and the low
    /// four bits count its components.
    pub const IS_LIG_BASE: u8 = 0x10;
}

/// One glyph of the run as the matching rules see it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MatchGlyph {
    /// Glyph id.
    pub id: u16,
    /// Per-glyph props, laid out as in [`match_prop`].
    pub props: u16,
}

impl MatchGlyph {
    /// A glyph with no props: not ignorable, synthesized as a base
    /// glyph, never part of a ligature.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::tables::layout::skip_iter::MatchGlyph;
    ///
    /// let g = MatchGlyph::new(42);
    /// assert_eq!((g.id, g.props), (42, 0));
    /// ```
    #[must_use]
    pub const fn new(id: u16) -> Self {
        Self { id, props: 0 }
    }

    /// A glyph with explicit props.
    #[must_use]
    pub const fn with_props(id: u16, props: u16) -> Self {
        Self { id, props }
    }

    /// True for an unsubstituted default-ignorable character.
    #[must_use]
    pub const fn is_default_ignorable(self) -> bool {
        self.props & match_prop::DEFAULT_IGNORABLE != 0
    }

    /// HarfBuzz's `lig_props` byte.
    #[must_use]
    pub const fn lig_props(self) -> u8 {
        (self.props >> match_prop::LIG_PROPS_SHIFT) as u8
    }

    /// Ligature id: nonzero for a ligature glyph and for the marks
    /// that were inside it when it formed.
    #[must_use]
    pub const fn lig_id(self) -> u8 {
        self.lig_props() >> 5
    }

    /// True for the glyph a ligature substitution produced
    /// (HarfBuzz's `_hb_glyph_info_ligated_internal`).
    #[must_use]
    pub const fn is_lig_base(self) -> bool {
        self.lig_props() & match_prop::IS_LIG_BASE != 0
    }

    /// Component index: the 1-based ligature component a mark
    /// belongs to, or a multiple substitution's 0-based output index.
    /// Zero for the ligature glyph itself.
    #[must_use]
    pub const fn lig_comp(self) -> u8 {
        if self.is_lig_base() {
            0
        } else {
            self.lig_props() & 0x0F
        }
    }

    /// True when the glyph came out of a multiple substitution.
    #[must_use]
    pub const fn is_multiplied(self) -> bool {
        self.props & match_prop::MULTIPLIED != 0
    }

    /// True when a ligature substitution produced the glyph.
    #[must_use]
    pub const fn is_ligated(self) -> bool {
        self.props & match_prop::LIGATED != 0
    }

    /// The glyph class the shaper synthesized for it.
    #[must_use]
    pub const fn synthesized_kind(self) -> GlyphKind {
        match self.props & match_prop::SYNTHESIZED_CLASS {
            match_prop::SYNTHESIZED_LIGATURE => GlyphKind::Ligature,
            match_prop::SYNTHESIZED_MARK => GlyphKind::Mark,
            _ => GlyphKind::Base,
        }
    }
}

/// Where matching reports the glyph ranges HarfBuzz marks unsafe to
/// break or to concatenate (`hb_buffer_t::unsafe_to_break`,
/// `unsafe_to_concat`, and their `_from_outbuffer` variants when
/// `from_out` is set). Positions are indices into the [`MatchSeq`]
/// that was matched. `()` drops them.
pub(crate) trait UnsafeRanges {
    /// A range no line break may split.
    fn unsafe_to_break(&mut self, start: usize, end: usize, from_out: bool);
    /// A range whose glyphs depend on the text around them.
    fn unsafe_to_concat(&mut self, start: usize, end: usize, from_out: bool);
}

impl UnsafeRanges for () {
    fn unsafe_to_break(&mut self, _start: usize, _end: usize, _from_out: bool) {}
    fn unsafe_to_concat(&mut self, _start: usize, _end: usize, _from_out: bool) {}
}

/// A run of glyphs as HarfBuzz's skipping iterator reads it: the
/// glyphs, and for each one whether the lookup's feature is on there
/// (its feature mask) and which syllable it belongs to.
///
/// A slice of [`MatchGlyph`]s is such a run with every glyph in the
/// mask and none in a syllable. The shaper's GSUB buffer implements
/// it over its output and input halves, so a walk sees the glyphs
/// already substituted before the cursor and the pending ones after.
pub(crate) trait MatchSeq {
    /// Number of glyphs.
    fn len(&self) -> usize;

    /// The glyph at `i`, or `None` past the end.
    fn glyph(&self, i: usize) -> Option<MatchGlyph>;

    /// True when the lookup's feature is on at glyph `i`, the test
    /// `(info.mask & lookup_mask)` HarfBuzz's input walks make.
    fn in_mask(&self, _i: usize) -> bool {
        true
    }

    /// The syllable of glyph `i`, HarfBuzz's `syllable()` byte. Zero
    /// when the glyph is in no syllable.
    fn syllable(&self, _i: usize) -> u8 {
        0
    }
}

impl MatchSeq for [MatchGlyph] {
    fn len(&self) -> usize {
        <[MatchGlyph]>::len(self)
    }

    fn glyph(&self, i: usize) -> Option<MatchGlyph> {
        self.get(i).copied()
    }
}

/// A glyph's class for matching purposes, HarfBuzz's glyph props
/// class bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphKind {
    /// Not listed in GDEF's `GlyphClassDef` (or listed as a component
    /// glyph): no ignore flag skips it.
    Unclassified,
    /// Base glyph.
    Base,
    /// Ligature glyph.
    Ligature,
    /// Mark.
    Mark,
}

/// Where a run's glyph classes come from, HarfBuzz's
/// `has_glyph_classes` split: GDEF's `GlyphClassDef` when the font
/// has one, else the classes the shaper synthesized into each glyph's
/// props (`hb_synthesize_glyph_classes`: a nonspacing mark that is not
/// default ignorable is a mark, anything else a base glyph, and
/// substitutions update the guess).
#[derive(Debug, Clone, Copy)]
pub struct GlyphClasses<'a> {
    /// The font's GDEF, kept only when it has a `GlyphClassDef`.
    gdef: Option<&'a Gdef<'a>>,
}

impl<'a> GlyphClasses<'a> {
    /// Classes for a font whose GDEF is `gdef`.
    #[must_use]
    pub fn new(gdef: Option<&'a Gdef<'a>>) -> Self {
        Self {
            gdef: gdef.filter(|g| g.has_glyph_classes()),
        }
    }

    /// Synthesized classes only, as for a font without GDEF.
    #[must_use]
    pub const fn synthesized() -> Self {
        Self { gdef: None }
    }

    /// True when the classes come from GDEF.
    #[must_use]
    pub const fn from_gdef(&self) -> bool {
        self.gdef.is_some()
    }

    /// The class of `g`.
    #[must_use]
    pub fn kind(&self, g: MatchGlyph) -> GlyphKind {
        match self.gdef {
            Some(gdef) => match gdef.raw_glyph_class(g.id) {
                Some(1) => GlyphKind::Base,
                Some(2) => GlyphKind::Ligature,
                Some(3) => GlyphKind::Mark,
                _ => GlyphKind::Unclassified,
            },
            None => g.synthesized_kind(),
        }
    }

    /// True when `g` is a mark.
    #[must_use]
    pub fn is_mark(&self, g: MatchGlyph) -> bool {
        self.kind(g) == GlyphKind::Mark
    }

    /// The mark attachment class HarfBuzz records for a mark: GDEF's
    /// when the classes come from GDEF, else zero.
    #[must_use]
    pub fn mark_attach_class(&self, g: MatchGlyph) -> u16 {
        self.gdef.map_or(0, |gdef| gdef.mark_attach_class(g.id))
    }
}

/// A decoded `LookupFlag` plus the glyph classes and mark filtering
/// set it needs: answers "does this lookup ignore this glyph?"
/// (HarfBuzz's `check_glyph_property`, negated).
///
/// Holds only borrowed data and never allocates.
#[derive(Debug, Clone, Copy)]
pub struct MatchFilter<'a> {
    flag: u16,
    mark_attach_type: u8,
    classes: GlyphClasses<'a>,
    mark_set: Option<&'a Coverage<'a>>,
}

impl<'a> MatchFilter<'a> {
    /// An "accept every glyph" filter with synthesized classes.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            flag: 0,
            mark_attach_type: 0,
            classes: GlyphClasses::synthesized(),
            mark_set: None,
        }
    }

    /// Builds a filter from the `LookupFlag` bits, the font's GDEF,
    /// and the optional `markFilteringSet` trailer of the lookup.
    /// `mark_filtering_set_index` is read from the lookup header when
    /// [`LOOKUP_FLAG_USE_MARK_FILTERING_SET`] is on.
    #[must_use]
    pub fn for_lookup(
        flag: u16,
        gdef: Option<&'a Gdef<'a>>,
        mark_filtering_set_index: Option<u16>,
    ) -> Self {
        let mark_set = match (gdef, mark_filtering_set_index) {
            (Some(g), Some(idx)) => g.mark_filtering_set(idx),
            _ => None,
        };
        Self {
            flag,
            mark_attach_type: (flag >> 8) as u8,
            classes: GlyphClasses::new(gdef),
            mark_set,
        }
    }

    /// The same filter with a different `LookupFlag`, as when HarfBuzz
    /// replaces an iterator's lookup props (the mark-to-base search
    /// ignores marks whatever the lookup says).
    #[must_use]
    pub const fn with_flag(self, flag: u16) -> Self {
        Self {
            flag,
            mark_attach_type: (flag >> 8) as u8,
            ..self
        }
    }

    /// The raw `LookupFlag`.
    #[must_use]
    pub const fn flag(&self) -> u16 {
        self.flag
    }

    /// Where the filter reads glyph classes from.
    #[must_use]
    pub const fn classes(&self) -> GlyphClasses<'a> {
        self.classes
    }

    /// `true` when the filter skips nothing.
    #[must_use]
    pub const fn is_pass_through(&self) -> bool {
        self.flag
            & (IGNORE_FLAGS
                | LOOKUP_FLAG_USE_MARK_FILTERING_SET
                | LOOKUP_FLAG_MARK_ATTACHMENT_TYPE_MASK)
            == 0
    }

    /// `true` when the lookup ignores `g`. As in HarfBuzz's
    /// `match_properties_mark`, a mark filtering set takes precedence
    /// over the mark attachment type.
    #[must_use]
    pub fn is_skipped(&self, g: MatchGlyph) -> bool {
        if self.is_pass_through() {
            return false;
        }
        let kind = self.classes.kind(g);
        let ignore = match kind {
            GlyphKind::Base => LOOKUP_FLAG_IGNORE_BASE_GLYPHS,
            GlyphKind::Ligature => LOOKUP_FLAG_IGNORE_LIGATURES,
            GlyphKind::Mark => LOOKUP_FLAG_IGNORE_MARKS,
            GlyphKind::Unclassified => 0,
        };
        if self.flag & ignore != 0 {
            return true;
        }
        if kind != GlyphKind::Mark {
            return false;
        }
        if self.flag & LOOKUP_FLAG_USE_MARK_FILTERING_SET != 0 {
            // A missing set (no GDEF, or a bad index) covers nothing.
            return !self.mark_set.is_some_and(|set| set.contains(g.id));
        }
        self.mark_attach_type != 0
            && self.classes.mark_attach_class(g) != u16::from(self.mark_attach_type)
    }
}

/// How a feature's lookups treat ZWNJ and ZWJ: HarfBuzz's `auto_zwnj`
/// and `auto_zwj`, cleared by the `F_MANUAL_ZWNJ` and `F_MANUAL_ZWJ`
/// feature flags the shapers set.
///
/// With `auto_zwj`, input matching skips a ZWJ the rule does not ask
/// for; with `auto_zwnj`, GSUB context (backtrack and lookahead)
/// matching skips a ZWNJ. A lookup shared by several features gets
/// the intersection ([`Joiners::and`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Joiners {
    /// Skip ZWNJ in GSUB context matching.
    pub auto_zwnj: bool,
    /// Skip ZWJ in input matching.
    pub auto_zwj: bool,
}

impl Joiners {
    /// The default: both joiners are skipped where the rules allow.
    pub const AUTO: Self = Self {
        auto_zwnj: true,
        auto_zwj: true,
    };
    /// `F_MANUAL_ZWJ`: a ZWJ in the input must be part of the rule.
    pub const MANUAL_ZWJ: Self = Self {
        auto_zwnj: true,
        auto_zwj: false,
    };
    /// `F_MANUAL_JOINERS`: neither joiner is skipped automatically.
    pub const MANUAL: Self = Self {
        auto_zwnj: false,
        auto_zwj: false,
    };

    /// The flags of a lookup shared by two features.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::tables::layout::skip_iter::Joiners;
    ///
    /// assert_eq!(Joiners::AUTO.and(Joiners::MANUAL_ZWJ), Joiners::MANUAL_ZWJ);
    /// ```
    #[must_use]
    pub const fn and(self, other: Self) -> Self {
        Self {
            auto_zwnj: self.auto_zwnj && other.auto_zwnj,
            auto_zwj: self.auto_zwj && other.auto_zwj,
        }
    }
}

/// The table a lookup belongs to. GPOS matching also passes over ZWNJ
/// and hidden characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutTable {
    /// Glyph substitution.
    Gsub,
    /// Glyph positioning.
    Gpos,
}

/// How many subtables of a lookup HarfBuzz gives a cache, the first
/// ones (`hb_accelerate_subtables_context_t::dispatch`). See
/// [`MatchContext::with_rule_set_digests`].
pub(crate) const SUBTABLE_CACHES: usize = 8;

/// Everything one lookup's matching depends on: its flags, its table,
/// its feature's joiner handling, and whether its feature matches
/// within one syllable. Hands out the [`SkipRules`] for input and for
/// context walks.
#[derive(Debug, Clone, Copy)]
pub struct MatchContext<'a> {
    filter: MatchFilter<'a>,
    table: LayoutTable,
    joiners: Joiners,
    per_syllable: bool,
    rule_set_digests: bool,
}

impl<'a> MatchContext<'a> {
    /// A context for one lookup.
    #[must_use]
    pub const fn new(filter: MatchFilter<'a>, table: LayoutTable, joiners: Joiners) -> Self {
        Self {
            filter,
            table,
            joiners,
            per_syllable: false,
            rule_set_digests: true,
        }
    }

    /// The same context for one subtable of the lookup: `digests` is
    /// false past the first [`SUBTABLE_CACHES`] subtables. HarfBuzz
    /// gives only those a cache (`hb_accelerate_subtables_context_t`
    /// in `hb-ot-layout-gsubgpos.hh`), and a class-based context rule
    /// set checks its digest of first input classes only with one.
    pub(crate) const fn with_rule_set_digests(self, digests: bool) -> Self {
        Self {
            rule_set_digests: digests,
            ..self
        }
    }

    /// True when a class-based context rule set checks its digest of
    /// first input classes (see [`Self::with_rule_set_digests`]).
    pub(crate) const fn rule_set_digests(&self) -> bool {
        self.rule_set_digests
    }

    /// The same context for a feature HarfBuzz registers with
    /// `F_PER_SYLLABLE` (`per_syllable` true): a GSUB walk then stops
    /// at glyphs of another syllable than the cursor's. GPOS walks
    /// ignore the setting, as in HarfBuzz.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::tables::layout::MatchContext;
    ///
    /// let cx = MatchContext::plain().with_per_syllable(true);
    /// assert!(cx.per_syllable());
    /// ```
    #[must_use]
    pub const fn with_per_syllable(self, per_syllable: bool) -> Self {
        Self {
            per_syllable,
            ..self
        }
    }

    /// True when the lookup's feature matches within one syllable.
    #[must_use]
    pub const fn per_syllable(&self) -> bool {
        self.per_syllable
    }

    /// A GSUB context with no lookup flags and automatic joiners.
    #[must_use]
    pub const fn plain() -> Self {
        Self::new(MatchFilter::none(), LayoutTable::Gsub, Joiners::AUTO)
    }

    /// The lookup's flag filter.
    #[must_use]
    pub const fn filter(&self) -> &MatchFilter<'a> {
        &self.filter
    }

    /// The lookup's table.
    #[must_use]
    pub const fn table(&self) -> LayoutTable {
        self.table
    }

    /// The feature's joiner handling.
    #[must_use]
    pub const fn joiners(&self) -> Joiners {
        self.joiners
    }

    /// The same context for a nested lookup with its own flags; the
    /// table and joiner handling carry over, as in HarfBuzz's
    /// `recurse`.
    #[must_use]
    pub const fn with_filter(self, filter: MatchFilter<'a>) -> Self {
        Self { filter, ..self }
    }

    /// Rules for input walks (`skipping_iterator_t::init` with
    /// `context_match = false`): ZWNJ is skipped only in GPOS, ZWJ
    /// when the feature allows it, hidden characters only in GPOS.
    /// Every glyph the walk stops at must have the lookup's feature
    /// on (see [`MatchSeq::in_mask`]).
    #[must_use]
    pub const fn input(&self) -> SkipRules<'a> {
        let gpos = matches!(self.table, LayoutTable::Gpos);
        SkipRules {
            filter: self.filter,
            ignore_zwnj: gpos,
            ignore_zwj: self.joiners.auto_zwj,
            ignore_hidden: gpos,
            check_mask: true,
            syllable: 0,
        }
    }

    /// Rules for backtrack and lookahead walks (`context_match =
    /// true`): ZWJ is always skipped, ZWNJ in GPOS or when the feature
    /// allows it, hidden characters only in GPOS. The feature mask is
    /// not checked.
    #[must_use]
    pub const fn context(&self) -> SkipRules<'a> {
        let gpos = matches!(self.table, LayoutTable::Gpos);
        SkipRules {
            filter: self.filter,
            ignore_zwnj: gpos || self.joiners.auto_zwnj,
            ignore_zwj: true,
            ignore_hidden: gpos,
            check_mask: false,
            syllable: 0,
        }
    }

    /// The syllable a walk from the glyph at `cursor` stays in:
    /// HarfBuzz's `matcher.syllable`, the cursor glyph's syllable for a
    /// per-syllable GSUB feature, zero (any syllable) otherwise.
    fn cursor_syllable<S: MatchSeq + ?Sized>(&self, seq: &S, cursor: usize) -> u8 {
        if self.per_syllable && matches!(self.table, LayoutTable::Gsub) {
            seq.syllable(cursor)
        } else {
            0
        }
    }

    /// [`Self::input`] for a walk whose cursor glyph is `seq[cursor]`.
    pub(crate) fn input_at<S: MatchSeq + ?Sized>(&self, seq: &S, cursor: usize) -> SkipRules<'a> {
        SkipRules {
            syllable: self.cursor_syllable(seq, cursor),
            ..self.input()
        }
    }

    /// [`Self::context`] for a walk whose cursor glyph is
    /// `seq[cursor]`.
    pub(crate) fn context_at<S: MatchSeq + ?Sized>(&self, seq: &S, cursor: usize) -> SkipRules<'a> {
        SkipRules {
            syllable: self.cursor_syllable(seq, cursor),
            ..self.context()
        }
    }
}

/// HarfBuzz's `matcher_t::may_skip` verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaySkip {
    /// The glyph takes part in matching.
    No,
    /// The lookup flags ignore the glyph.
    Yes,
    /// A default ignorable: skipped unless the rule matches it.
    Maybe,
}

/// One walk's skipping rules, HarfBuzz's `matcher_t` (lookup props
/// plus the ZWNJ, ZWJ and hidden exemptions, the feature mask for
/// input walks, and the syllable of a per-syllable feature). Get one
/// from [`MatchContext::input`] or [`MatchContext::context`].
#[derive(Debug, Clone, Copy)]
pub struct SkipRules<'a> {
    filter: MatchFilter<'a>,
    ignore_zwnj: bool,
    ignore_zwj: bool,
    ignore_hidden: bool,
    /// Input walks stop at a glyph whose feature mask is off as at a
    /// mismatch (`matcher.mask` is the lookup mask). Context walks do
    /// not check it (`matcher.mask` is all ones).
    check_mask: bool,
    /// The cursor's syllable, when the walk may not leave it, or zero
    /// for any syllable.
    syllable: u8,
}

impl<'a> SkipRules<'a> {
    /// The same rules with other lookup props, HarfBuzz's
    /// `set_lookup_props`.
    #[must_use]
    pub const fn with_filter(self, filter: MatchFilter<'a>) -> Self {
        Self { filter, ..self }
    }

    /// The lookup-flag filter these rules apply.
    #[must_use]
    pub const fn filter(&self) -> &MatchFilter<'a> {
        &self.filter
    }

    /// HarfBuzz's `may_skip`.
    #[must_use]
    pub fn may_skip(&self, g: MatchGlyph) -> MaySkip {
        if self.filter.is_skipped(g) {
            return MaySkip::Yes;
        }
        let p = g.props;
        if p & match_prop::DEFAULT_IGNORABLE != 0
            && (self.ignore_zwnj || p & match_prop::ZWNJ == 0)
            && (self.ignore_zwj || p & match_prop::ZWJ == 0)
            && (self.ignore_hidden || p & match_prop::HIDDEN == 0)
        {
            return MaySkip::Maybe;
        }
        MaySkip::No
    }

    /// HarfBuzz's `skipping_iterator_t::match` for glyph `i` of `seq`.
    /// `matches` is the rule's verdict on the glyph id, `None` when
    /// the walk has no match function. Returns `Some(true)` to stop
    /// and match, `Some(false)` to stop and fail, `None` to skip.
    ///
    /// As in `matcher_t::may_match`, a glyph outside the feature mask
    /// (input walks) or outside the cursor's syllable (per-syllable
    /// features) does not match, whatever the rule says.
    fn step<S: MatchSeq + ?Sized>(
        &self,
        seq: &S,
        i: usize,
        g: MatchGlyph,
        matches: impl FnOnce(u16) -> Option<bool>,
    ) -> Option<bool> {
        let skip = self.may_skip(g);
        if skip == MaySkip::Yes {
            return None;
        }
        let gated = (self.check_mask && !seq.in_mask(i))
            || (self.syllable != 0 && seq.syllable(i) != self.syllable);
        let verdict = if gated { Some(false) } else { matches(g.id) };
        match verdict {
            Some(true) => Some(true),
            None if skip == MaySkip::No => Some(true),
            _ if skip == MaySkip::No => Some(false),
            _ => None,
        }
    }

    /// [`Self::next`] over any [`MatchSeq`]. On failure returns
    /// HarfBuzz's `unsafe_to`: one past the glyph that failed to match,
    /// or the length of the run when the walk ran out of glyphs.
    pub(crate) fn next_in<S: MatchSeq + ?Sized>(
        &self,
        seq: &S,
        from: usize,
        mut matches: impl FnMut(u16) -> Option<bool>,
    ) -> Result<usize, usize> {
        let len = seq.len();
        let mut i = from;
        while i < len {
            let g = seq.glyph(i).unwrap_or_default();
            match self.step(seq, i, g, &mut matches) {
                Some(true) => return Ok(i),
                Some(false) => return Err(i + 1),
                None => {}
            }
            i += 1;
        }
        Err(len)
    }

    /// [`Self::prev`] over any [`MatchSeq`]. On failure returns
    /// HarfBuzz's `unsafe_from`: the glyph before the one that failed
    /// to match (at least zero), or zero when the walk ran out.
    pub(crate) fn prev_in<S: MatchSeq + ?Sized>(
        &self,
        seq: &S,
        before: usize,
        mut matches: impl FnMut(u16) -> Option<bool>,
    ) -> Result<usize, usize> {
        let mut i = before.min(seq.len());
        while i > 0 {
            i -= 1;
            let g = seq.glyph(i).unwrap_or_default();
            match self.step(seq, i, g, &mut matches) {
                Some(true) => return Ok(i),
                Some(false) => return Err(i.max(1) - 1),
                None => {}
            }
        }
        Err(0)
    }

    /// HarfBuzz's `skipping_iterator_t::next`, starting at `from`
    /// (inclusive): the index of the first glyph the walk stops at
    /// with a match, or `None` when it stops at a mismatch or runs out
    /// of glyphs.
    pub fn next(
        &self,
        glyphs: &[MatchGlyph],
        from: usize,
        matches: impl FnMut(u16) -> Option<bool>,
    ) -> Option<usize> {
        self.next_in(glyphs, from, matches).ok()
    }

    /// HarfBuzz's `skipping_iterator_t::prev`: like [`Self::next`],
    /// walking backward from `before - 1`.
    pub fn prev(
        &self,
        glyphs: &[MatchGlyph],
        before: usize,
        matches: impl FnMut(u16) -> Option<bool>,
    ) -> Option<usize> {
        self.prev_in(glyphs, before, matches).ok()
    }

    /// [`Self::next`] without a match function: the first glyph at or
    /// after `from` the walk does not skip.
    #[must_use]
    pub fn next_any(&self, glyphs: &[MatchGlyph], from: usize) -> Option<usize> {
        self.next(glyphs, from, |_| None)
    }

    /// [`Self::prev`] without a match function: the nearest glyph
    /// before `before` the walk does not skip.
    #[must_use]
    pub fn prev_any(&self, glyphs: &[MatchGlyph], before: usize) -> Option<usize> {
        self.prev(glyphs, before, |_| None)
    }
}

#[cfg(test)]
mod tests;
