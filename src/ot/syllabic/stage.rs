//! GSUB stages of the syllable-based shapers, with each glyph's
//! [`GlyphInfo`] carried through the substitutions.
//!
//! HarfBuzz groups a shaper's features into stages: the lookups of
//! every feature in one stage run once each, in lookup-index order,
//! each at the glyphs whose mask has one of its features' bits
//! (`hb_ot_map_builder_t::compile` merges a lookup several features
//! share, OR-ing their masks). [`apply_stage`] does the same over
//! [`GlyphInfo::mask`].
//!
//! HarfBuzz keeps the shaper state in the glyph info, so a ligature
//! keeps its first component's state and every output of a multiple
//! substitution a copy of its source's. The glyphs here carry an index
//! into a side table instead: for the length of a stage, three glyph
//! bytes the stage does not otherwise read (`indic_position`,
//! `char_class`, `combining_class`, which GSUB copies the same way) hold
//! the glyph's slot, and after every lookup the side table is rebuilt
//! in the new glyph order. The bytes get their values back when the
//! stage ends.

use alloc::vec::Vec;

use super::GlyphInfo;
use crate::buffer::Glyph;
use crate::shape::{Feature, SyllabicGsub};
use crate::tables::layout::skip_iter::match_prop;
use crate::tables::layout::Joiners;

/// HarfBuzz's feature flags (`hb_ot_map_feature_flags_t` in
/// `hb-ot-map.hh`), kept as data in the shapers' feature tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FeatureFlags(u8);

impl FeatureFlags {
    /// No flag (`F_NONE`).
    pub(crate) const NONE: Self = Self(0);
    /// `F_GLOBAL`: the feature applies to every glyph.
    pub(crate) const GLOBAL: Self = Self(1);
    /// `F_MANUAL_ZWNJ`: lookups see ZWNJ instead of skipping it.
    pub(crate) const MANUAL_ZWNJ: Self = Self(2);
    /// `F_MANUAL_ZWJ`: lookups see ZWJ instead of skipping it.
    pub(crate) const MANUAL_ZWJ: Self = Self(4);
    /// `F_MANUAL_JOINERS`: both of the above.
    pub(crate) const MANUAL_JOINERS: Self = Self(2 | 4);
    /// `F_GLOBAL_MANUAL_JOINERS`.
    pub(crate) const GLOBAL_MANUAL_JOINERS: Self = Self(1 | 2 | 4);
    /// `F_PER_SYLLABLE`: lookups only match inside the cursor's
    /// syllable.
    pub(crate) const PER_SYLLABLE: Self = Self(8);

    /// Both flag sets.
    pub(crate) const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// True when every flag of `other` is set.
    pub(crate) const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The joiner handling the flags ask for.
    pub(crate) const fn joiners(self) -> Joiners {
        Joiners {
            auto_zwnj: !self.contains(Self::MANUAL_ZWNJ),
            auto_zwj: !self.contains(Self::MANUAL_ZWJ),
        }
    }
}

/// A feature of a shaper's feature table: HarfBuzz's
/// `hb_ot_map_feature_t`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MapFeature {
    /// The feature tag.
    pub(crate) tag: [u8; 4],
    /// HarfBuzz's flags for it.
    pub(crate) flags: FeatureFlags,
}

impl MapFeature {
    /// A table entry.
    pub(crate) const fn new(tag: &[u8; 4], flags: FeatureFlags) -> Self {
        Self { tag: *tag, flags }
    }
}

/// A mask that covers every glyph, for global features.
pub(crate) const GLOBAL_MASK: u32 = u32::MAX;

/// One feature of a stage as it runs: the mask bits a glyph needs, its
/// joiner handling, and whether it matches per syllable.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StageFeature {
    /// The feature tag.
    pub(crate) tag: [u8; 4],
    /// The [`GlyphInfo::mask`] bits it applies to ([`GLOBAL_MASK`] for
    /// every glyph).
    pub(crate) mask: u32,
    /// The feature's flags.
    pub(crate) flags: FeatureFlags,
}

impl StageFeature {
    /// The stage entry for `feature`, which applies to glyphs with
    /// `bit`, or to every glyph when the feature is global.
    pub(crate) const fn of(feature: MapFeature, bit: u32) -> Self {
        let mask = if feature.flags.contains(FeatureFlags::GLOBAL) {
            GLOBAL_MASK
        } else {
            bit
        };
        Self {
            tag: feature.tag,
            mask,
            flags: feature.flags,
        }
    }
}

/// True when the caller turned `tag` off with a zero-valued feature.
pub(crate) fn user_disabled(features: &[Feature], tag: [u8; 4]) -> bool {
    features.iter().any(|f| f.tag == tag && f.value == 0)
}

/// The lookups of `tag` in the font, for a run of candidate script
/// tags `script_priority`.
pub(crate) fn feature_lookups(
    runner: &SyllabicGsub<'_>,
    tag: [u8; 4],
    script_priority: &[[u8; 4]],
) -> Vec<u16> {
    let gsub = runner.gsub();
    crate::ot::layout_select::feature_lookup_indices(
        gsub.script_list(),
        &gsub.features(),
        gsub.language_tags(),
        tag,
        script_priority,
    )
    .unwrap_or_default()
}

/// True when the language system the run selects lists `tag` and the
/// caller did not turn it off: HarfBuzz's feature map then has the
/// feature and gives it a mask bit (`hb_ot_map_builder_t::compile`).
///
/// Presence is what counts, not lookups: a FeatureVariations record
/// that leaves the feature without lookups keeps its bit, and a
/// required feature with the tag gives it none, as HarfBuzz looks a
/// feature up in the language system's feature list alone.
pub(crate) fn has_feature(
    runner: &SyllabicGsub<'_>,
    features: &[Feature],
    tag: [u8; 4],
    script_priority: &[[u8; 4]],
) -> bool {
    let gsub = runner.gsub();
    !user_disabled(features, tag)
        && crate::ot::layout_select::lists_feature(
            gsub.script_list(),
            &gsub.features(),
            gsub.language_tags(),
            tag,
            script_priority,
        )
}

/// Adds the caller's features to a shaper's last stage, where HarfBuzz
/// puts them (`hb_ot_shape_collect_features`): each enabled feature
/// the stage does not list already and `excluded` does not rule out,
/// as a global feature. `rvrn` and the direction features are ruled
/// out too, since the pipeline runs them first. A feature with a value
/// above 1 selects an alternate. Those are returned instead, to run on
/// their own after the stage.
pub(crate) fn add_user_features(
    stage: &mut Vec<StageFeature>,
    features: &[Feature],
    excluded: fn([u8; 4]) -> bool,
) -> Vec<([u8; 4], u32)> {
    let mut alternates: Vec<([u8; 4], u32)> = Vec::new();
    for f in features {
        let known = stage.iter().any(|s| s.tag == f.tag)
            || alternates.iter().any(|&(t, _)| t == f.tag)
            || excluded(f.tag)
            || matches!(&f.tag, b"rvrn" | b"ltra" | b"ltrm" | b"rtla" | b"rtlm");
        if known || f.value == 0 {
            continue;
        }
        if f.value == 1 {
            stage.push(StageFeature {
                tag: f.tag,
                mask: GLOBAL_MASK,
                flags: FeatureFlags::GLOBAL,
            });
        } else {
            alternates.push((f.tag, f.value));
        }
    }
    alternates
}

/// Applies feature `tag` to the whole run with alternate `value` (the
/// caller's feature value, 1-based as in HarfBuzz).
pub(crate) fn apply_alternate_feature(
    runner: &mut SyllabicGsub<'_>,
    script_priority: &[[u8; 4]],
    tag: [u8; 4],
    value: u32,
    glyphs: &mut Vec<Glyph>,
) {
    let alternate = value.saturating_sub(1).min(u32::from(u16::MAX)) as u16;
    for index in feature_lookups(runner, tag, script_priority) {
        runner.apply_lookup_alternate(index, alternate, glyphs);
    }
}

/// One merged lookup of a stage.
#[derive(Debug, Clone, Copy)]
struct StageLookup {
    index: u16,
    mask: u32,
    joiners: Joiners,
    per_syllable: bool,
}

/// The lookups of a stage of `features` (those the caller did not turn
/// off with `user`), merged by lookup index and sorted, as
/// `hb_ot_map_builder_t::compile` builds a stage.
///
/// Each feature adds the lookups its listed feature has, with its mask
/// and flags. When a feature of the stage has the tag of the language
/// system's required feature, the stage runs the required feature too,
/// ahead of the others, on every glyph, with automatic joiner handling
/// and across syllables, as HarfBuzz adds it with the global mask and
/// default flags. A lookup several of them share applies where any of
/// their masks is on, skips joiners only where all of them do, and
/// matches per syllable as the first of them that has it says.
fn stage_lookups(
    runner: &SyllabicGsub<'_>,
    script_priority: &[[u8; 4]],
    features: &[StageFeature],
    user: &[Feature],
) -> Vec<StageLookup> {
    let gsub = runner.gsub();
    let active = gsub.features();
    let enabled = || features.iter().filter(|f| !user_disabled(user, f.tag));
    let required = crate::ot::layout_select::required_feature(
        gsub.script_list(),
        &active,
        gsub.language_tags(),
        script_priority,
    )
    .filter(|(tag, _)| enabled().any(|f| f.tag == *tag))
    .map(|(_, indices)| indices)
    .unwrap_or_default();
    let mut lookups: Vec<StageLookup> = Vec::new();
    let mut add = |index: u16, mask: u32, joiners: Joiners, per_syllable: bool| match lookups
        .iter_mut()
        .find(|l| l.index == index)
    {
        Some(l) => {
            l.mask |= mask;
            l.joiners = l.joiners.and(joiners);
        }
        None => lookups.push(StageLookup {
            index,
            mask,
            joiners,
            per_syllable,
        }),
    };
    for index in required {
        add(index, GLOBAL_MASK, Joiners::AUTO, false);
    }
    for f in enabled() {
        let listed = crate::ot::layout_select::listed_feature_lookups(
            gsub.script_list(),
            &active,
            gsub.language_tags(),
            f.tag,
            script_priority,
        );
        let per_syllable = f.flags.contains(FeatureFlags::PER_SYLLABLE);
        for index in listed {
            add(index, f.mask, f.flags.joiners(), per_syllable);
        }
    }
    lookups.sort_unstable_by_key(|l| l.index);
    lookups
}

/// A glyph's side-table entry for the length of a stage.
#[derive(Debug, Clone, Copy)]
struct Slot {
    info: GlyphInfo,
    saved: [u8; 3],
    last_id: u32,
    last_props: u16,
}

/// The glyph's ligated and multiplied bits, which a ligature or a
/// multiple substitution sets.
fn substitution_props(g: &Glyph) -> u16 {
    g.unicode_props & (match_prop::LIGATED | match_prop::MULTIPLIED)
}

/// The largest slot index the three borrowed bytes hold.
const MAX_SLOTS: usize = 1 << 24;

fn stash(g: &mut Glyph, slot: usize) {
    g.indic_position = slot as u8;
    g.char_class = (slot >> 8) as u8;
    g.combining_class = (slot >> 16) as u8;
}

fn slot_of(g: &Glyph) -> usize {
    usize::from(g.indic_position)
        | usize::from(g.char_class) << 8
        | usize::from(g.combining_class) << 16
}

/// Runs one stage: the lookups of `features` (those the caller did not
/// turn off with `user`), and of the required feature when one of them
/// has its tag, merged by lookup index (see [`stage_lookups`]), each at
/// the glyphs whose info mask meets its features' masks and, for
/// per-syllable features, inside each syllable. `info` has one entry
/// per glyph and stays aligned with the glyphs. Glyphs a substitution
/// touched get [`GlyphInfo::substituted`].
pub(crate) fn apply_stage(
    runner: &mut SyllabicGsub<'_>,
    script_priority: &[[u8; 4]],
    features: &[StageFeature],
    user: &[Feature],
    glyphs: &mut Vec<Glyph>,
    info: &mut Vec<GlyphInfo>,
) {
    if glyphs.len() != info.len() || glyphs.is_empty() || glyphs.len() > MAX_SLOTS {
        return;
    }
    let lookups = stage_lookups(runner, script_priority, features, user);
    if lookups.is_empty() {
        return;
    }

    let mut slots: Vec<Slot> = glyphs
        .iter_mut()
        .zip(info.iter())
        .enumerate()
        .map(|(i, (g, &info))| {
            let saved = [g.indic_position, g.char_class, g.combining_class];
            stash(g, i);
            // The syllable rides on the glyph through every
            // substitution, where per-syllable matching reads it.
            g.syllable = info.syllable;
            Slot {
                info,
                saved,
                last_id: g.glyph_id,
                last_props: substitution_props(g),
            }
        })
        .collect();
    for lookup in lookups {
        let joiners = lookup.joiners;
        let substituted: Vec<bool>;
        {
            let table = &slots;
            let get = |g: &Glyph| table.get(slot_of(g));
            // Every glyph has the global features' bits, as HarfBuzz
            // sets the global mask on every glyph.
            let global = lookup.mask == GLOBAL_MASK;
            let applies =
                |g: &Glyph| global || get(g).is_some_and(|s| s.info.mask & lookup.mask != 0);
            substituted =
                runner.apply_lookup(lookup.index, joiners, lookup.per_syllable, glyphs, &applies);
        }
        slots = glyphs
            .iter_mut()
            .enumerate()
            .map(|(i, g)| {
                let mut slot = slots.get(slot_of(g)).copied().unwrap_or(Slot {
                    info: GlyphInfo::default(),
                    saved: [0; 3],
                    last_id: g.glyph_id,
                    last_props: substitution_props(g),
                });
                // A substitution produced the glyph, even one that kept
                // its id, or it has a new id or new ligature props.
                let props = substitution_props(g);
                if substituted.get(i).copied().unwrap_or(false)
                    || g.glyph_id != slot.last_id
                    || props != slot.last_props
                {
                    slot.info.substituted = true;
                }
                slot.last_id = g.glyph_id;
                slot.last_props = props;
                stash(g, i);
                slot
            })
            .collect();
    }
    info.clear();
    for (g, slot) in glyphs.iter_mut().zip(&slots) {
        [g.indic_position, g.char_class, g.combining_class] = slot.saved;
        info.push(slot.info);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_map_to_joiner_handling() {
        assert_eq!(FeatureFlags::GLOBAL.joiners(), Joiners::AUTO);
        assert_eq!(FeatureFlags::MANUAL_JOINERS.joiners(), Joiners::MANUAL);
        assert_eq!(FeatureFlags::MANUAL_ZWJ.joiners(), Joiners::MANUAL_ZWJ);
        let f = FeatureFlags::GLOBAL_MANUAL_JOINERS.union(FeatureFlags::PER_SYLLABLE);
        assert!(f.contains(FeatureFlags::GLOBAL));
        assert!(f.contains(FeatureFlags::PER_SYLLABLE));
        assert_eq!(f.joiners(), Joiners::MANUAL);
    }

    #[test]
    fn global_features_cover_every_glyph() {
        let f = MapFeature::new(b"pres", FeatureFlags::GLOBAL_MANUAL_JOINERS);
        assert_eq!(StageFeature::of(f, 4).mask, GLOBAL_MASK);
        let f = MapFeature::new(b"pref", FeatureFlags::MANUAL_JOINERS);
        assert_eq!(StageFeature::of(f, 4).mask, 4);
    }

    #[test]
    fn slots_round_trip_through_the_borrowed_bytes() {
        let mut g = Glyph::new(1, 0);
        for slot in [0, 1, 255, 256, 65_535, 65_536, MAX_SLOTS - 1] {
            stash(&mut g, slot);
            assert_eq!(slot_of(&g), slot);
        }
    }
}
