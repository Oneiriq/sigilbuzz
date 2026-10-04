//! Feature to lookup resolution, shared by the GSUB and GPOS passes.
//!
//! Like HarfBuzz (`hb_ot_map_builder_t`), each table picks one script
//! and one language system for a run, and every feature comes from
//! that language system alone:
//!
//! - The script is the first of the run's candidate tags (`arab`,
//!   `dev2` then `deva`, ...) that the table lists, then `DFLT`, then
//!   `dflt`, then `latn` (`hb_ot_layout_table_select_script`).
//! - The language system is chosen by [`Script::select_lang_sys`]: the
//!   buffer language's OpenType tags in order, then a `dflt` record,
//!   then the script's default language system
//!   (`hb_ot_layout_script_select_language`).
//!
//! A feature the chosen language system does not list has no lookups,
//! even when another script or language system in the font has it.
//! The one exception is `vert`, which HarfBuzz looks up anywhere in
//! the FeatureList when the language system lacks it
//! (`F_GLOBAL_SEARCH`). When a language system lists a tag twice, the
//! first entry wins, as in `hb_ot_layout_collect_features_map`.
//!
//! Features are read by index through [`ActiveFeatures`]: when the
//! table's FeatureVariations selected a record for the font's
//! coordinates, a feature that record substitutes keeps its tag and
//! takes the record's lookups (`get_feature_variation`). The `vert`
//! search goes by index too, so it sees the substitution.
//!
//! A language system's required feature (`requiredFeatureIndex`) joins
//! the lookups of its tag, which is where HarfBuzz schedules a required
//! feature whose tag the shaper knows. [`required_feature`] exposes it
//! so the shaper can run it at the start of GSUB when its tag is one
//! the pipeline never applies, and add it to the single GPOS stage
//! whatever its tag.
//!
//! [`Script::select_lang_sys`]: crate::tables::layout::Script::select_lang_sys

use alloc::borrow::Cow;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::shape::Feature;
use crate::sync::OnceBox;
use crate::tables::layout::{ActiveFeatures, Joiners, LangSys, ScriptList};

/// Script tags HarfBuzz falls back to, in order, when none of the
/// run's own tags is in the table.
const FALLBACK_SCRIPTS: [[u8; 4]; 3] = [*b"DFLT", *b"dflt", *b"latn"];

/// Features HarfBuzz looks up in the whole FeatureList when the
/// selected language system does not list them.
const GLOBAL_SEARCH_FEATURES: [[u8; 4]; 1] = [*b"vert"];

/// The language system a table uses for a run whose candidate script
/// tags are `script_priority`, or `None` when the table has no usable
/// script.
fn select_lang_sys<'a>(
    script_list: &ScriptList<'a>,
    language_tags: &[[u8; 4]],
    script_priority: &[[u8; 4]],
) -> Option<LangSys<'a>> {
    let script = script_priority
        .iter()
        .chain(FALLBACK_SCRIPTS.iter())
        .find_map(|tag| script_list.find(*tag))?;
    script.select_lang_sys(language_tags)
}

/// The script tag a table picks for a run whose candidate script tags
/// are `script_priority`: the first of them the table lists, then
/// `DFLT`, `dflt`, or `latn` (`hb_ot_layout_table_select_script`), or
/// `None` when it lists none of those.
pub(crate) fn chosen_script(
    script_list: &ScriptList<'_>,
    script_priority: &[[u8; 4]],
) -> Option<[u8; 4]> {
    script_priority
        .iter()
        .chain(FALLBACK_SCRIPTS.iter())
        .find(|&&tag| script_list.find(tag).is_some())
        .copied()
}

/// Sorted, deduplicated lookup indices that feature `tag` selects in
/// the language system [`select_lang_sys`] picks.
///
/// `Some(empty)` means the language system does not carry the
/// feature; `None` means the table offered no usable language system
/// at all.
pub(crate) fn feature_lookup_indices(
    script_list: &ScriptList<'_>,
    features: &ActiveFeatures<'_>,
    language_tags: &[[u8; 4]],
    tag: [u8; 4],
    script_priority: &[[u8; 4]],
) -> Option<Vec<u16>> {
    let lang_sys = select_lang_sys(script_list, language_tags, script_priority);
    let found = lang_sys
        .as_ref()
        .and_then(|lang_sys| lang_sys_lookups(lang_sys, features, tag));
    if found.is_some() {
        return found;
    }
    if GLOBAL_SEARCH_FEATURES.contains(&tag) {
        if let Some(index) = features.find(tag) {
            let lookups = features
                .get(index)
                .map(|(_, feature)| feature.lookup_indices().collect())
                .unwrap_or_default();
            return Some(sorted(lookups));
        }
    }
    lang_sys.map(|_| Vec::new())
}

/// True when the language system [`select_lang_sys`] picks lists a
/// feature tagged `tag`, whatever lookups it has: HarfBuzz's feature
/// map then gives the feature an index in the table
/// (`hb_ot_layout_collect_features_map`). A FeatureVariations record
/// can leave such a feature without lookups, and it is still listed.
/// The required feature does not count, as it does not there.
pub(crate) fn lists_feature(
    script_list: &ScriptList<'_>,
    features: &ActiveFeatures<'_>,
    language_tags: &[[u8; 4]],
    tag: [u8; 4],
    script_priority: &[[u8; 4]],
) -> bool {
    select_lang_sys(script_list, language_tags, script_priority).is_some_and(|lang_sys| {
        lang_sys
            .feature_indices()
            .any(|index| features.tag(index) == Some(tag))
    })
}

/// Sorted, deduplicated lookup indices of the feature tagged `tag`
/// that the language system [`select_lang_sys`] picks lists (for
/// `vert`, found anywhere in the FeatureList when it does not list
/// it), without the required feature [`feature_lookup_indices`]
/// merges in. Empty when there is no such feature.
///
/// HarfBuzz's feature map adds these lookups with the feature's own
/// mask, and the required feature's with the global mask
/// (`hb_ot_map_builder_t::compile`). The two masks only part ways
/// where the feature picks an alternate, so this is for a caller that
/// needs to tell which lookups the two share.
pub(crate) fn listed_feature_lookups(
    script_list: &ScriptList<'_>,
    features: &ActiveFeatures<'_>,
    language_tags: &[[u8; 4]],
    tag: [u8; 4],
    script_priority: &[[u8; 4]],
) -> Vec<u16> {
    let listed =
        select_lang_sys(script_list, language_tags, script_priority).and_then(|lang_sys| {
            lang_sys
                .feature_indices()
                .find(|&index| features.tag(index) == Some(tag))
        });
    let index = listed.or_else(|| {
        if GLOBAL_SEARCH_FEATURES.contains(&tag) {
            features.find(tag)
        } else {
            None
        }
    });
    index
        .and_then(|index| features.get(index))
        .map(|(_, feature)| sorted(feature.lookup_indices().collect()))
        .unwrap_or_default()
}

/// The required feature of the language system [`select_lang_sys`]
/// picks: its tag and sorted lookup indices.
pub(crate) fn required_feature(
    script_list: &ScriptList<'_>,
    features: &ActiveFeatures<'_>,
    language_tags: &[[u8; 4]],
    script_priority: &[[u8; 4]],
) -> Option<([u8; 4], Vec<u16>)> {
    let lang_sys = select_lang_sys(script_list, language_tags, script_priority)?;
    let (tag, feature) = features.get(lang_sys.required_feature_index()?)?;
    Some((tag, sorted(feature.lookup_indices().collect())))
}

/// The lookups feature `tag` selects in one language system: the
/// first feature record with that tag, plus the required feature when
/// its tag matches. `None` when the language system carries neither.
fn lang_sys_lookups(
    lang_sys: &LangSys<'_>,
    features: &ActiveFeatures<'_>,
    tag: [u8; 4],
) -> Option<Vec<u16>> {
    // The tag first: only a feature with the tag needs its
    // substitutions looked up.
    let with_tag = |index: u16| {
        if features.tag(index) != Some(tag) {
            return None;
        }
        features.get(index).map(|(_, feature)| feature)
    };
    let required = lang_sys.required_feature_index().and_then(with_tag);
    let regular = lang_sys.feature_indices().find_map(with_tag);
    if required.is_none() && regular.is_none() {
        return None;
    }
    let indices = required
        .into_iter()
        .chain(regular)
        .flat_map(|feature| feature.lookup_indices())
        .collect();
    Some(sorted(indices))
}

fn sorted(mut indices: Vec<u16>) -> Vec<u16> {
    indices.sort_unstable();
    indices.dedup();
    indices
}

/// What feature resolution reads of a GSUB or GPOS view: its lists,
/// its language preference, and the font's cache of resolved language
/// systems, if the view has one.
#[derive(Clone, Copy)]
pub(crate) struct LayoutView<'a> {
    pub(crate) script_list: ScriptList<'a>,
    pub(crate) features: ActiveFeatures<'a>,
    pub(crate) language_tags: &'a [[u8; 4]],
    pub(crate) maps: Option<&'a FeatureMaps>,
    pub(crate) plans: Option<&'a StagePlans>,
}

impl<'a> LayoutView<'a> {
    /// The lookups of one shaping stage, which `build` merges from the
    /// features of the stage: the caller's `features`, and the
    /// stage-specific `spec` (its own features, flags and settings,
    /// encoded by the caller), for a run whose candidate script tags
    /// are `script_priority`. The font's cache keeps the result, so a
    /// stage is merged once per combination of those and of the view's
    /// language and FeatureVariations record. Without a cache, or for a
    /// stage too large to keep, `build` runs each time.
    pub(crate) fn stage_plan(
        &self,
        script_priority: &[[u8; 4]],
        features: &[Feature],
        spec: &[u64],
        build: impl Fn() -> Vec<PlannedLookup>,
    ) -> Cow<'a, [PlannedLookup]> {
        let Some(plans) = self.plans else {
            return Cow::Owned(build());
        };
        let key = StageKey {
            script_priority,
            language_tags: self.language_tags,
            record: self.features.record(),
            features,
            spec,
        };
        plans.get(&key, build)
    }

    /// [`feature_lookup_indices`] for this view.
    pub(crate) fn feature_lookups(
        &self,
        tag: [u8; 4],
        script_priority: &[[u8; 4]],
    ) -> Option<Vec<u16>> {
        match self.map(script_priority).as_deref() {
            Some(map) => map.feature_lookups(tag),
            None => feature_lookup_indices(
                &self.script_list,
                &self.features,
                self.language_tags,
                tag,
                script_priority,
            ),
        }
    }

    /// [`lists_feature`] for this view.
    pub(crate) fn lists(&self, tag: [u8; 4], script_priority: &[[u8; 4]]) -> bool {
        match self.map(script_priority).as_deref() {
            Some(map) => map.lists(tag),
            None => lists_feature(
                &self.script_list,
                &self.features,
                self.language_tags,
                tag,
                script_priority,
            ),
        }
    }

    /// [`listed_feature_lookups`] for this view.
    pub(crate) fn listed_lookups(&self, tag: [u8; 4], script_priority: &[[u8; 4]]) -> Vec<u16> {
        match self.map(script_priority).as_deref() {
            Some(map) => map.listed_lookups(tag),
            None => listed_feature_lookups(
                &self.script_list,
                &self.features,
                self.language_tags,
                tag,
                script_priority,
            ),
        }
    }

    /// [`required_feature`] for this view.
    pub(crate) fn required(&self, script_priority: &[[u8; 4]]) -> Option<([u8; 4], Vec<u16>)> {
        match self.map(script_priority).as_deref() {
            Some(map) => map.required.clone(),
            None => required_feature(
                &self.script_list,
                &self.features,
                self.language_tags,
                script_priority,
            ),
        }
    }

    /// The resolved language system of a run whose candidate script
    /// tags are `script_priority`, from the font's cache: `None`
    /// without one, or when the language system is too large to keep.
    fn map(&self, script_priority: &[[u8; 4]]) -> Option<Cow<'_, FeatureMap>> {
        let maps = self.maps?;
        let key = MapKey {
            script_priority,
            language_tags: self.language_tags,
            record: self.features.record(),
        };
        maps.get(&key, || FeatureMap::build(self, script_priority))
    }
}

/// Feature indices a language system may list for its resolution to be
/// kept. Real fonts list a few dozen.
const MAX_MAP_FEATURES: usize = 1024;
/// Lookup indices one resolution may keep, over all its features.
const MAX_MAP_LOOKUPS: usize = 1 << 14;
/// Resolutions one table keeps: one per combination of script tags,
/// language and FeatureVariations record a font is shaped with.
const MAP_SLOTS: usize = 16;

/// One language system resolved once, so each feature query of a
/// shaping call is a binary search instead of a walk of the script
/// list, the language system and its features. Answers every query
/// exactly as the walk does (see [`feature_lookup_indices`],
/// [`lists_feature`], [`listed_feature_lookups`] and
/// [`required_feature`]).
#[derive(Debug, Clone)]
pub(crate) struct FeatureMap {
    /// The table has a usable script and language system.
    lang_sys: bool,
    /// The tags of the language system's features and of its required
    /// feature, sorted.
    entries: Vec<MapEntry>,
    /// [`required_feature`].
    required: Option<([u8; 4], Vec<u16>)>,
    /// The lookups of the first `vert` feature in the FeatureList, which
    /// the global search finds.
    global_vert: Option<Vec<u16>>,
}

#[derive(Debug, Clone)]
struct MapEntry {
    tag: [u8; 4],
    /// The language system lists the tag (its required feature does
    /// not count).
    listed: bool,
    /// [`lang_sys_lookups`] for the tag.
    lookups: Option<Vec<u16>>,
    /// The lookups of the first listed feature with the tag, as
    /// [`listed_feature_lookups`] reads them.
    listed_lookups: Vec<u16>,
}

impl FeatureMap {
    /// Resolves the language system `view` picks for `script_priority`,
    /// or `None` when it lists too many features or lookups to keep.
    fn build(view: &LayoutView<'_>, script_priority: &[[u8; 4]]) -> Option<Self> {
        let features = &view.features;
        let lookups_of = |index: u16| -> Option<Vec<u16>> {
            features
                .get(index)
                .map(|(_, f)| sorted(f.lookup_indices().collect()))
        };
        let global_vert = features
            .find(*b"vert")
            .map(|index| lookups_of(index).unwrap_or_default());
        let lang_sys = select_lang_sys(&view.script_list, view.language_tags, script_priority);
        let Some(lang_sys) = lang_sys else {
            return Some(Self {
                lang_sys: false,
                entries: Vec::new(),
                required: None,
                global_vert,
            });
        };
        // The first listed feature of each tag, and the first one whose
        // table reads.
        let mut firsts: BTreeMap<[u8; 4], (Option<u16>, Option<u16>)> = BTreeMap::new();
        for (n, index) in lang_sys.feature_indices().enumerate() {
            if n >= MAX_MAP_FEATURES {
                return None;
            }
            let Some(tag) = features.tag(index) else {
                continue;
            };
            let first = firsts.entry(tag).or_insert((Some(index), None));
            if first.1.is_none() && features.get(index).is_some() {
                first.1 = Some(index);
            }
        }
        let required_index = lang_sys.required_feature_index();
        let required_tag = required_index.and_then(|index| features.tag(index));
        if let Some(tag) = required_tag {
            firsts.entry(tag).or_insert((None, None));
        }
        let required = required_index.and_then(|index| {
            let (tag, f) = features.get(index)?;
            Some((tag, sorted(f.lookup_indices().collect())))
        });
        let mut total = 0usize;
        let mut entries = Vec::with_capacity(firsts.len());
        for (tag, (first_listed, first_read)) in firsts {
            // `lang_sys_lookups`: the required feature when it has the
            // tag and reads, and the first listed one that reads.
            let from_required = required_index
                .filter(|_| required_tag == Some(tag))
                .and_then(|index| features.get(index));
            let from_listed = first_read.and_then(|index| features.get(index));
            let lookups = (from_required.is_some() || from_listed.is_some()).then(|| {
                sorted(
                    from_required
                        .into_iter()
                        .chain(from_listed)
                        .flat_map(|(_, f)| f.lookup_indices())
                        .collect(),
                )
            });
            let listed_lookups = first_listed.and_then(lookups_of).unwrap_or_default();
            total = total
                .saturating_add(lookups.as_ref().map_or(0, Vec::len))
                .saturating_add(listed_lookups.len());
            if total > MAX_MAP_LOOKUPS {
                return None;
            }
            entries.push(MapEntry {
                tag,
                listed: first_listed.is_some(),
                lookups,
                listed_lookups,
            });
        }
        Some(Self {
            lang_sys: true,
            entries,
            required,
            global_vert,
        })
    }

    fn entry(&self, tag: [u8; 4]) -> Option<&MapEntry> {
        self.entries
            .binary_search_by_key(&tag, |e| e.tag)
            .ok()
            .and_then(|i| self.entries.get(i))
    }

    fn feature_lookups(&self, tag: [u8; 4]) -> Option<Vec<u16>> {
        if let Some(lookups) = self.entry(tag).and_then(|e| e.lookups.as_ref()) {
            return Some(lookups.clone());
        }
        if GLOBAL_SEARCH_FEATURES.contains(&tag) {
            if let Some(lookups) = &self.global_vert {
                return Some(lookups.clone());
            }
        }
        self.lang_sys.then(Vec::new)
    }

    fn lists(&self, tag: [u8; 4]) -> bool {
        self.entry(tag).is_some_and(|e| e.listed)
    }

    fn listed_lookups(&self, tag: [u8; 4]) -> Vec<u16> {
        match self.entry(tag).filter(|e| e.listed) {
            Some(e) => e.listed_lookups.clone(),
            None if GLOBAL_SEARCH_FEATURES.contains(&tag) => {
                self.global_vert.clone().unwrap_or_default()
            }
            None => Vec::new(),
        }
    }

    /// Heap bytes the map holds.
    fn heap_bytes(&self) -> usize {
        let lookups = |v: &Vec<u16>| v.capacity() * 2;
        self.entries.capacity() * core::mem::size_of::<MapEntry>()
            + self
                .entries
                .iter()
                .map(|e| e.lookups.as_ref().map_or(0, lookups) + lookups(&e.listed_lookups))
                .sum::<usize>()
            + self.required.as_ref().map_or(0, |(_, v)| lookups(v))
            + self.global_vert.as_ref().map_or(0, lookups)
    }
}

/// What a [`FeatureMap`] depends on besides the table.
struct MapKey<'k> {
    script_priority: &'k [[u8; 4]],
    language_tags: &'k [[u8; 4]],
    record: Option<u32>,
}

/// A kept [`FeatureMap`] with its key; `None` for a language system too
/// large to keep, which is then walked each time.
struct KeyedMap {
    script_priority: Vec<[u8; 4]>,
    language_tags: Vec<[u8; 4]>,
    record: Option<u32>,
    map: Option<FeatureMap>,
}

impl KeyedMap {
    fn matches(&self, key: &MapKey<'_>) -> bool {
        self.script_priority == key.script_priority
            && self.language_tags == key.language_tags
            && self.record == key.record
    }
}

/// The language systems one table has resolved, kept by a
/// [`crate::Font`] across shaping calls: at most [`MAP_SLOTS`] of them.
/// Past that, a run's language system is walked each time, as it is
/// without a cache.
pub(crate) struct FeatureMaps {
    slots: [OnceBox<KeyedMap>; MAP_SLOTS],
}

impl FeatureMaps {
    /// No language system resolved yet.
    pub(crate) fn new() -> Self {
        Self {
            slots: core::array::from_fn(|_| OnceBox::new()),
        }
    }

    /// The map for `key`, built with `build` the first time, or `None`
    /// when it is too large to keep.
    fn get(
        &self,
        key: &MapKey<'_>,
        build: impl Fn() -> Option<FeatureMap>,
    ) -> Option<Cow<'_, FeatureMap>> {
        for slot in &self.slots {
            let keyed = match slot.get() {
                Some(keyed) => keyed,
                None => slot.get_or_init(|| KeyedMap {
                    script_priority: key.script_priority.to_vec(),
                    language_tags: key.language_tags.to_vec(),
                    record: key.record,
                    map: build(),
                }),
            };
            if keyed.matches(key) {
                return keyed.map.as_ref().map(Cow::Borrowed);
            }
        }
        build().map(Cow::Owned)
    }

    /// Heap bytes the kept maps hold.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.slots
            .iter()
            .filter_map(OnceBox::get)
            .map(|k| {
                core::mem::size_of::<KeyedMap>()
                    + (k.script_priority.capacity() + k.language_tags.capacity()) * 4
                    + k.map.as_ref().map_or(0, FeatureMap::heap_bytes)
            })
            .sum()
    }
}

impl Default for FeatureMaps {
    fn default() -> Self {
        Self::new()
    }
}

/// One lookup of a shaping stage, in a form every stage kind (the GSUB
/// stages, the syllabic shapers' stages, the GPOS stage) converts to
/// and from, so one cache keeps them all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlannedLookup {
    /// Index into the LookupList.
    pub(crate) index: u16,
    /// The alternate an AlternateSubst lookup picks.
    pub(crate) alternate: u16,
    /// The glyph mask bits the lookup applies at (syllabic stages).
    pub(crate) mask: u32,
    /// The joiner handling of the features that share the lookup.
    pub(crate) joiners: Joiners,
    /// The lookup matches within the cursor's syllable only.
    pub(crate) per_syllable: bool,
    /// The lookup applies only where the stage's mask says.
    pub(crate) masked: bool,
}

impl PlannedLookup {
    /// A lookup with every setting at its default: automatic joiners,
    /// no alternate, global.
    pub(crate) const fn new(index: u16) -> Self {
        Self {
            index,
            alternate: 0,
            mask: u32::MAX,
            joiners: Joiners::AUTO,
            per_syllable: false,
            masked: false,
        }
    }
}

/// The kinds of stage a plan's spec starts with, so that two kinds of
/// stage over the same table never share a plan.
pub(crate) mod stage_kind {
    /// The GPOS stage.
    pub(crate) const GPOS: u64 = 1;
    /// A GSUB stage of the default, Arabic and merged passes.
    pub(crate) const GSUB: u64 = 2;
    /// A GSUB stage of a syllabic shaper.
    pub(crate) const SYLLABIC: u64 = 3;
}

/// The spec word of joiner handling and two flags.
pub(crate) fn flag_bits(joiners: Joiners, first: bool, second: bool) -> u64 {
    u64::from(joiners.auto_zwnj)
        | u64::from(joiners.auto_zwj) << 1
        | u64::from(first) << 2
        | u64::from(second) << 3
}

/// Stage plans one table keeps at most.
const STAGE_SLOTS: usize = 64;
/// Caller features a kept plan's key may hold.
const MAX_STAGE_FEATURES: usize = 64;
/// Spec words a kept plan's key may hold.
const MAX_STAGE_SPEC: usize = 256;
/// Lookups a kept plan may hold.
const MAX_STAGE_LOOKUPS: usize = 4096;

/// What a stage plan depends on.
struct StageKey<'k> {
    script_priority: &'k [[u8; 4]],
    language_tags: &'k [[u8; 4]],
    record: Option<u32>,
    features: &'k [Feature],
    spec: &'k [u64],
}

impl StageKey<'_> {
    /// An FNV-1a hash of the key's spec, script tags and feature tags,
    /// which picks the first slot a lookup probes.
    fn slot_hash(&self) -> usize {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |w: u64| {
            h ^= w;
            h = h.wrapping_mul(0x0100_0000_01b3);
        };
        self.spec.iter().for_each(|&w| mix(w));
        self.script_priority
            .iter()
            .for_each(|t| mix(u64::from(u32::from_be_bytes(*t))));
        self.features
            .iter()
            .for_each(|f| mix(u64::from(u32::from_be_bytes(f.tag)) << 32 | u64::from(f.value)));
        (h >> 32) as usize
    }
}

/// A kept stage plan with its key.
struct KeyedStage {
    script_priority: Vec<[u8; 4]>,
    language_tags: Vec<[u8; 4]>,
    record: Option<u32>,
    features: Vec<Feature>,
    spec: Vec<u64>,
    lookups: Vec<PlannedLookup>,
}

impl KeyedStage {
    fn matches(&self, key: &StageKey<'_>) -> bool {
        self.spec == key.spec
            && self.script_priority == key.script_priority
            && self.record == key.record
            && self.features == key.features
            && self.language_tags == key.language_tags
    }
}

/// The stage plans one table has merged, kept by a [`crate::Font`]
/// across shaping calls: at most [`STAGE_SLOTS`] of them, each with a
/// key of at most [`MAX_STAGE_FEATURES`] caller features and
/// [`MAX_STAGE_SPEC`] spec words and at most [`MAX_STAGE_LOOKUPS`]
/// lookups. A stage past those limits is merged each time, as it is
/// without a cache.
pub(crate) struct StagePlans {
    slots: [OnceBox<KeyedStage>; STAGE_SLOTS],
}

impl StagePlans {
    /// No stage merged yet.
    pub(crate) fn new() -> Self {
        Self {
            slots: core::array::from_fn(|_| OnceBox::new()),
        }
    }

    /// The plan for `key`, merged with `build` the first time.
    fn get<'s>(
        &'s self,
        key: &StageKey<'_>,
        build: impl Fn() -> Vec<PlannedLookup>,
    ) -> Cow<'s, [PlannedLookup]> {
        if key.features.len() > MAX_STAGE_FEATURES || key.spec.len() > MAX_STAGE_SPEC {
            return Cow::Owned(build());
        }
        let mut built: Option<Vec<PlannedLookup>> = None;
        // Probing starts at a slot the key hashes to, so a stage usually
        // finds its plan at the first slot it looks at.
        let start = key.slot_hash() % STAGE_SLOTS;
        let order = (start..STAGE_SLOTS).chain(0..start);
        for slot in order.filter_map(|i| self.slots.get(i)) {
            if let Some(kept) = slot.get() {
                if kept.matches(key) {
                    return Cow::Borrowed(&kept.lookups);
                }
                continue;
            }
            let lookups = built.take().unwrap_or_else(&build);
            if lookups.len() > MAX_STAGE_LOOKUPS {
                return Cow::Owned(lookups);
            }
            let kept = slot.get_or_init(|| KeyedStage {
                script_priority: key.script_priority.to_vec(),
                language_tags: key.language_tags.to_vec(),
                record: key.record,
                features: key.features.to_vec(),
                spec: key.spec.to_vec(),
                lookups: lookups.clone(),
            });
            if kept.matches(key) {
                return Cow::Borrowed(&kept.lookups);
            }
            // Another thread kept a different stage here first.
            built = Some(lookups);
        }
        Cow::Owned(built.unwrap_or_else(build))
    }

    /// Heap bytes the kept plans hold.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.slots
            .iter()
            .filter_map(OnceBox::get)
            .map(|k| {
                core::mem::size_of::<KeyedStage>()
                    + (k.script_priority.capacity() + k.language_tags.capacity()) * 4
                    + k.features.capacity() * core::mem::size_of::<Feature>()
                    + k.spec.capacity() * 8
                    + k.lookups.capacity() * core::mem::size_of::<PlannedLookup>()
            })
            .sum()
    }
}

impl Default for StagePlans {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for StagePlans {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let kept = self.slots.iter().filter(|s| s.get().is_some()).count();
        f.debug_struct("StagePlans").field("kept", &kept).finish()
    }
}

impl core::fmt::Debug for FeatureMaps {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let kept = self.slots.iter().filter(|s| s.get().is_some()).count();
        f.debug_struct("FeatureMaps").field("kept", &kept).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::layout::{FeatureList, FeatureVariations};
    use alloc::vec;

    /// Minimal GSUB-style blob pieces: a ScriptList, a FeatureList,
    /// and nothing else (lookups are only referenced by index).
    struct Fixture {
        scripts: Vec<u8>,
        features: Vec<u8>,
    }

    fn lang_sys(required: u16, features: &[u16]) -> Vec<u8> {
        let mut out = vec![0, 0];
        out.extend_from_slice(&required.to_be_bytes());
        out.extend_from_slice(&(features.len() as u16).to_be_bytes());
        for f in features {
            out.extend_from_slice(&f.to_be_bytes());
        }
        out
    }

    /// One script: optional default LangSys plus sorted records.
    type ScriptSpec = ([u8; 4], Option<Vec<u8>>, Vec<([u8; 4], Vec<u8>)>);

    fn script_list(scripts: &[ScriptSpec]) -> Vec<u8> {
        let mut out = (scripts.len() as u16).to_be_bytes().to_vec();
        let records = out.len();
        out.resize(records + scripts.len() * 6, 0);
        for (i, (tag, default, langs)) in scripts.iter().enumerate() {
            let start = out.len();
            let rec = records + i * 6;
            out[rec..rec + 4].copy_from_slice(tag);
            out[rec + 4..rec + 6].copy_from_slice(&(start as u16).to_be_bytes());
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(&(langs.len() as u16).to_be_bytes());
            let lang_records = out.len();
            out.resize(lang_records + langs.len() * 6, 0);
            if let Some(body) = default {
                let rel = (out.len() - start) as u16;
                out[start..start + 2].copy_from_slice(&rel.to_be_bytes());
                out.extend_from_slice(body);
            }
            for (j, (ltag, body)) in langs.iter().enumerate() {
                let rel = (out.len() - start) as u16;
                let slot = lang_records + j * 6;
                out[slot..slot + 4].copy_from_slice(ltag);
                out[slot + 4..slot + 6].copy_from_slice(&rel.to_be_bytes());
                out.extend_from_slice(body);
            }
        }
        out
    }

    /// FeatureList whose feature `i` has tag `tags[i]` and the single
    /// lookup index `i + 10`.
    fn feature_list(tags: &[[u8; 4]]) -> Vec<u8> {
        let mut out = (tags.len() as u16).to_be_bytes().to_vec();
        let header = 2 + tags.len() * 6;
        for (i, tag) in tags.iter().enumerate() {
            out.extend_from_slice(tag);
            out.extend_from_slice(&((header + i * 6) as u16).to_be_bytes());
        }
        for i in 0..tags.len() {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&(i as u16 + 10).to_be_bytes());
        }
        out
    }

    impl Fixture {
        fn lookups(&self, langs: &[[u8; 4]], tag: &[u8; 4], pri: &[[u8; 4]]) -> Option<Vec<u16>> {
            self.varied_lookups(None, langs, tag, pri)
        }

        /// [`Self::lookups`] with the one record of the FeatureVariations
        /// `variations` selected.
        fn varied_lookups(
            &self,
            variations: Option<&[u8]>,
            langs: &[[u8; 4]],
            tag: &[u8; 4],
            pri: &[[u8; 4]],
        ) -> Option<Vec<u16>> {
            let scripts = ScriptList::parse(&self.scripts).unwrap();
            let features = self.features(variations);
            feature_lookup_indices(&scripts, &features, langs, *tag, pri)
        }

        fn features<'a>(&'a self, variations: Option<&'a [u8]>) -> ActiveFeatures<'a> {
            let list = FeatureList::parse(&self.features).unwrap();
            let record = variations.map(|v| (FeatureVariations::parse(v).unwrap(), 0));
            ActiveFeatures::new(list, record)
        }
    }

    /// FeatureVariations with one record that always applies and gives
    /// feature `index` the single lookup `lookup`.
    fn substituting(index: u16, lookup: u16) -> Vec<u8> {
        let mut out = vec![0, 1, 0, 0, 0, 0, 0, 1];
        out.extend_from_slice(&0u32.to_be_bytes()); // null ConditionSet
        out.extend_from_slice(&16u32.to_be_bytes()); // substitution
        out.extend_from_slice(&[0, 1, 0, 0, 0, 1]);
        out.extend_from_slice(&index.to_be_bytes());
        out.extend_from_slice(&12u32.to_be_bytes()); // alternate Feature
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&lookup.to_be_bytes());
        out
    }

    /// Features: 0 = locl (TRK), 1 = liga (default), 2 = liga (TRK),
    /// 3 = rlig (required in AZE).
    fn latin_fixture() -> Fixture {
        Fixture {
            scripts: script_list(&[(
                *b"latn",
                Some(lang_sys(0xFFFF, &[1])),
                vec![
                    (*b"AZE ", lang_sys(3, &[1])),
                    (*b"TRK ", lang_sys(0xFFFF, &[0, 2])),
                ],
            )]),
            features: feature_list(&[*b"locl", *b"liga", *b"liga", *b"rlig"]),
        }
    }

    #[test]
    fn language_selects_its_lang_sys() {
        let f = latin_fixture();
        let latn = [*b"latn", *b"DFLT"];
        assert_eq!(f.lookups(&[], b"liga", &latn), Some(vec![11]));
        assert_eq!(f.lookups(&[*b"TRK "], b"liga", &latn), Some(vec![12]));
        assert_eq!(f.lookups(&[*b"TRK "], b"locl", &latn), Some(vec![10]));
        // No default-LangSys locl: the fallback returns an empty list.
        assert_eq!(f.lookups(&[], b"locl", &latn), Some(vec![]));
    }

    #[test]
    fn candidates_are_tried_in_order() {
        let f = latin_fixture();
        let latn = [*b"latn"];
        assert_eq!(
            f.lookups(&[*b"XYZ ", *b"TRK "], b"liga", &latn),
            Some(vec![12])
        );
        assert_eq!(f.lookups(&[*b"XYZ "], b"liga", &latn), Some(vec![11]));
    }

    #[test]
    fn required_feature_joins_its_tag() {
        let f = latin_fixture();
        let latn = [*b"latn"];
        // AZE lists no rlig, but its required feature is an rlig.
        assert_eq!(f.lookups(&[*b"AZE "], b"rlig", &latn), Some(vec![13]));
        assert_eq!(f.lookups(&[*b"AZE "], b"liga", &latn), Some(vec![11]));
        // Other language systems do not see it.
        assert_eq!(f.lookups(&[*b"TRK "], b"rlig", &latn), Some(vec![]));
    }

    #[test]
    fn one_language_system_supplies_every_feature() {
        let f = Fixture {
            scripts: script_list(&[
                (*b"DFLT", Some(lang_sys(0xFFFF, &[1])), vec![]),
                (
                    *b"cyrl",
                    Some(lang_sys(0xFFFF, &[])),
                    vec![(*b"SRB ", lang_sys(0xFFFF, &[0]))],
                ),
            ]),
            features: feature_list(&[*b"locl", *b"liga"]),
        };
        let cyrl = [*b"cyrl", *b"DFLT"];
        assert_eq!(f.lookups(&[*b"SRB "], b"locl", &cyrl), Some(vec![10]));
        // cyrl/SRB lacks liga. HarfBuzz does not borrow it from DFLT:
        // the run's script is cyrl, and cyrl/SRB has no liga.
        assert_eq!(f.lookups(&[*b"SRB "], b"liga", &cyrl), Some(vec![]));
        assert_eq!(f.lookups(&[], b"liga", &cyrl), Some(vec![]));
        // A run whose own script is missing uses DFLT, never another
        // script that happens to be in the table.
        assert_eq!(f.lookups(&[*b"SRB "], b"locl", &[*b"latn"]), Some(vec![]));
        assert_eq!(f.lookups(&[*b"SRB "], b"liga", &[*b"latn"]), Some(vec![11]));
    }

    #[test]
    fn script_fallback_is_dflt_then_lowercase_dflt_then_latn() {
        let tagged = |tags: &[[u8; 4]]| Fixture {
            scripts: script_list(
                &tags
                    .iter()
                    .enumerate()
                    .map(|(i, tag)| (*tag, Some(lang_sys(0xFFFF, &[i as u16])), vec![]))
                    .collect::<Vec<_>>(),
            ),
            features: feature_list(&[*b"liga", *b"liga", *b"liga"]),
        };
        let thai = [*b"thai"];
        // Script lists are sorted by tag: DFLT < dflt < latn.
        let f = tagged(&[*b"DFLT", *b"dflt", *b"latn"]);
        assert_eq!(f.lookups(&[], b"liga", &thai), Some(vec![10]));
        let f = tagged(&[*b"dflt", *b"latn"]);
        assert_eq!(f.lookups(&[], b"liga", &thai), Some(vec![10]));
        let f = tagged(&[*b"cyrl", *b"latn"]);
        assert_eq!(f.lookups(&[], b"liga", &thai), Some(vec![11]));
        // No candidate and no fallback script: nothing usable.
        let f = tagged(&[*b"cyrl"]);
        assert_eq!(f.lookups(&[], b"liga", &thai), None);
    }

    #[test]
    fn first_of_duplicate_feature_tags_wins() {
        let f = Fixture {
            scripts: script_list(&[(*b"latn", Some(lang_sys(0xFFFF, &[2, 0, 1])), vec![])]),
            features: feature_list(&[*b"liga", *b"calt", *b"liga"]),
        };
        assert_eq!(f.lookups(&[], b"liga", &[*b"latn"]), Some(vec![12]));
        assert_eq!(f.lookups(&[], b"calt", &[*b"latn"]), Some(vec![11]));
    }

    #[test]
    fn vert_is_found_outside_the_language_system() {
        let f = Fixture {
            scripts: script_list(&[
                (*b"DFLT", Some(lang_sys(0xFFFF, &[])), vec![]),
                (*b"kana", Some(lang_sys(0xFFFF, &[0, 1])), vec![]),
            ]),
            features: feature_list(&[*b"vert", *b"vrt2"]),
        };
        let dflt = [*b"DFLT"];
        assert_eq!(f.lookups(&[], b"vert", &dflt), Some(vec![10]));
        // Only vert gets the global search.
        assert_eq!(f.lookups(&[], b"vrt2", &dflt), Some(vec![]));
    }

    #[test]
    fn required_feature_of_the_selected_language_system() {
        let f = latin_fixture();
        let scripts = ScriptList::parse(&f.scripts).unwrap();
        let features = f.features(None);
        let latn = [*b"latn"];
        assert_eq!(
            required_feature(&scripts, &features, &[*b"AZE "], &latn),
            Some((*b"rlig", vec![13]))
        );
        assert_eq!(
            required_feature(&scripts, &features, &[*b"TRK "], &latn),
            None
        );
        assert_eq!(required_feature(&scripts, &features, &[], &latn), None);
    }

    #[test]
    fn a_substituted_feature_keeps_its_tag_and_takes_the_alternate_lookups() {
        let f = latin_fixture();
        let latn = [*b"latn"];
        // Feature 1 is the default language system's liga.
        let v = substituting(1, 99);
        assert_eq!(
            f.varied_lookups(Some(&v), &[], b"liga", &latn),
            Some(vec![99])
        );
        // TRK's liga is feature 2, which the record leaves alone.
        assert_eq!(
            f.varied_lookups(Some(&v), &[*b"TRK "], b"liga", &latn),
            Some(vec![12])
        );
        // The required feature of AZE is feature 3.
        let v = substituting(3, 98);
        assert_eq!(
            f.varied_lookups(Some(&v), &[*b"AZE "], b"rlig", &latn),
            Some(vec![98])
        );
        let scripts = ScriptList::parse(&f.scripts).unwrap();
        let features = f.features(Some(&v));
        assert_eq!(
            required_feature(&scripts, &features, &[*b"AZE "], &latn),
            Some((*b"rlig", vec![98]))
        );
    }

    #[test]
    fn the_vert_search_sees_substitutions() {
        let f = Fixture {
            scripts: script_list(&[
                (*b"DFLT", Some(lang_sys(0xFFFF, &[])), vec![]),
                (*b"kana", Some(lang_sys(0xFFFF, &[0, 1])), vec![]),
            ]),
            features: feature_list(&[*b"vert", *b"vrt2"]),
        };
        let v = substituting(0, 97);
        let dflt = [*b"DFLT"];
        assert_eq!(
            f.varied_lookups(Some(&v), &[], b"vert", &dflt),
            Some(vec![97])
        );
        assert_eq!(f.lookups(&[], b"vert", &dflt), Some(vec![10]));
    }

    #[test]
    fn a_listed_feature_stays_listed_without_lookups() {
        let f = latin_fixture();
        let scripts = ScriptList::parse(&f.scripts).unwrap();
        let latn = [*b"latn"];
        // Feature 1, the default language system's liga, loses its
        // lookups to a null alternate.
        let mut v = vec![0, 1, 0, 0, 0, 0, 0, 1];
        v.extend_from_slice(&0u32.to_be_bytes()); // null ConditionSet
        v.extend_from_slice(&16u32.to_be_bytes()); // substitution
        v.extend_from_slice(&[0, 1, 0, 0, 0, 1, 0, 1]); // feature 1
        v.extend_from_slice(&0u32.to_be_bytes()); // null alternate
        let features = f.features(Some(&v));
        assert_eq!(
            feature_lookup_indices(&scripts, &features, &[], *b"liga", &latn),
            Some(vec![])
        );
        let lists = |features: &ActiveFeatures<'_>, langs: &[[u8; 4]], tag: &[u8; 4]| {
            lists_feature(&scripts, features, langs, *tag, &latn)
        };
        assert!(lists(&features, &[], b"liga"));
        assert!(lists(&f.features(None), &[], b"liga"));
        // The default language system has no locl, and AZE has rlig
        // only as its required feature.
        assert!(!lists(&features, &[], b"locl"));
        assert!(lists(&features, &[*b"TRK "], b"locl"));
        assert!(!lists(&features, &[*b"AZE "], b"rlig"));
        // No usable script.
        let empty = script_list(&[]);
        let empty = ScriptList::parse(&empty).unwrap();
        assert!(!lists_feature(&empty, &features, &[], *b"liga", &latn));
    }

    #[test]
    fn listed_lookups_leave_the_required_feature_out() {
        let f = latin_fixture();
        let scripts = ScriptList::parse(&f.scripts).unwrap();
        let features = f.features(None);
        let latn = [*b"latn"];
        let listed = |langs: &[[u8; 4]], tag: &[u8; 4]| {
            listed_feature_lookups(&scripts, &features, langs, *tag, &latn)
        };
        // AZE has rlig only as its required feature.
        assert_eq!(listed(&[*b"AZE "], b"rlig"), Vec::<u16>::new());
        assert_eq!(listed(&[*b"AZE "], b"liga"), [11]);
        assert_eq!(listed(&[*b"TRK "], b"liga"), [12]);
        assert_eq!(listed(&[*b"TRK "], b"rlig"), Vec::<u16>::new());

        // A required liga, feature 1, next to the listed liga, feature
        // 0: only the listed one's lookup. `vert`, which the language
        // system does not list, comes from the FeatureList.
        let f = Fixture {
            scripts: script_list(&[(*b"latn", Some(lang_sys(1, &[0])), vec![])]),
            features: feature_list(&[*b"liga", *b"liga", *b"vert"]),
        };
        let scripts = ScriptList::parse(&f.scripts).unwrap();
        let features = f.features(None);
        let listed = |tag: &[u8; 4]| listed_feature_lookups(&scripts, &features, &[], *tag, &latn);
        assert_eq!(listed(b"liga"), [10]);
        assert_eq!(f.lookups(&[], b"liga", &latn), Some(vec![10, 11]));
        assert_eq!(listed(b"vert"), [12]);
        assert_eq!(listed(b"kern"), Vec::<u16>::new());
    }

    #[test]
    fn empty_script_list_yields_none() {
        let f = Fixture {
            scripts: script_list(&[]),
            features: feature_list(&[]),
        };
        assert_eq!(f.lookups(&[], b"liga", &[*b"DFLT"]), None);
    }

    /// Every query through a kept [`FeatureMap`] answers as the walk
    /// does, for the tags each real font lists and some it does not,
    /// over several scripts, languages and FeatureVariations records.
    #[test]
    fn kept_language_systems_answer_as_the_walk_does() {
        use crate::tables::layout::accel::LayoutCache;
        use crate::tables::layout::LayoutTable;
        use crate::Face;
        let fonts: [&[u8]; 5] = [
            include_bytes!("../../tests/fixtures/opensans_regular.ttf"),
            include_bytes!("../../tests/fixtures/amiri_regular.ttf"),
            include_bytes!("../../tests/fonts/NotoSansDevanagari-Regular.ttf"),
            include_bytes!("../../tests/fonts/NotoSansKR-Palt-Subset.ttf"),
            include_bytes!("../../tests/fixtures/rubik_vf.ttf"),
        ];
        let priorities: [&[[u8; 4]]; 6] = [
            &[*b"latn"],
            &[*b"arab"],
            &[*b"dev2", *b"deva"],
            &[*b"hang"],
            &[*b"cyrl"],
            &[],
        ];
        let languages: [&[[u8; 4]]; 4] = [&[], &[*b"TRK "], &[*b"URD ", *b"ARA "], &[*b"KOR "]];
        let unknown = [*b"zzzz", *b"vert", *b"kern", *b"liga", *b"rvrn", *b"mark"];
        for data in fonts {
            let face = Face::parse_bytes(data, 0).unwrap();
            for gsub in [true, false] {
                let (script_list, list, variations, count) = if gsub {
                    let Some(t) = face.gsub().unwrap() else {
                        continue;
                    };
                    let v = t.feature_variations().ok().flatten();
                    (
                        *t.script_list(),
                        *t.feature_list(),
                        v,
                        t.lookup_list().len(),
                    )
                } else {
                    let Some(t) = face.gpos().unwrap() else {
                        continue;
                    };
                    let v = t.feature_variations().ok().flatten();
                    (
                        *t.script_list(),
                        *t.feature_list(),
                        v,
                        t.lookup_list().len(),
                    )
                };
                let table = if gsub {
                    LayoutTable::Gsub
                } else {
                    LayoutTable::Gpos
                };
                let records: Vec<Option<(FeatureVariations<'_>, u32)>> = core::iter::once(None)
                    .chain(
                        variations
                            .into_iter()
                            .flat_map(|v| (0..v.len()).map(move |r| Some((v, r)))),
                    )
                    .collect();
                let mut tags: Vec<[u8; 4]> = list.iter().map(|(tag, _)| tag).collect();
                tags.extend(unknown);
                for record in records {
                    let features = ActiveFeatures::new(list, record);
                    let cache = LayoutCache::new(table, count);
                    for languages in languages {
                        let walk = LayoutView {
                            script_list,
                            features,
                            language_tags: languages,
                            maps: None,
                            plans: None,
                        };
                        let kept = LayoutView {
                            maps: Some(&cache.maps),
                            ..walk
                        };
                        for priority in priorities {
                            assert_eq!(kept.required(priority), walk.required(priority));
                            for &tag in &tags {
                                let what = (tag, priority, languages, record.map(|r| r.1));
                                assert_eq!(
                                    kept.feature_lookups(tag, priority),
                                    walk.feature_lookups(tag, priority),
                                    "{what:?}"
                                );
                                assert_eq!(kept.lists(tag, priority), walk.lists(tag, priority));
                                assert_eq!(
                                    kept.listed_lookups(tag, priority),
                                    walk.listed_lookups(tag, priority),
                                    "{what:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    const LATN: &[[u8; 4]] = &[*b"latn"];

    #[test]
    fn stage_plans_are_kept_per_key() {
        let plans = StagePlans::new();
        let built = core::cell::Cell::new(0);
        let build = |n: u16| {
            built.set(built.get() + 1);
            (0..n).map(PlannedLookup::new).collect::<Vec<_>>()
        };
        let liga = [Feature {
            tag: *b"liga",
            value: 0,
        }];
        let key = |spec: &'static [u64], features: &'static [Feature]| StageKey {
            script_priority: LATN,
            language_tags: &[],
            record: None,
            features,
            spec,
        };
        let first = plans.get(&key(&[1, 0], &[]), || build(3));
        assert!(matches!(first, Cow::Borrowed(_)));
        assert_eq!(first.len(), 3);
        // The same key reads the kept plan.
        let again = plans.get(&key(&[1, 0], &[]), || build(9));
        assert_eq!(again.len(), 3);
        assert_eq!(built.get(), 1);
        // Another spec or feature list is another plan.
        assert_eq!(plans.get(&key(&[1, 1], &[]), || build(2)).len(), 2);
        let with_liga: &'static [Feature] = Box::leak(Box::new(liga));
        assert_eq!(plans.get(&key(&[1, 0], with_liga), || build(4)).len(), 4);
        assert_eq!(built.get(), 3);
        // Keys and plans past the limits are merged each time.
        let long: &'static [u64] = Box::leak(alloc::vec![7; MAX_STAGE_SPEC + 1].into_boxed_slice());
        assert!(matches!(
            plans.get(&key(long, &[]), || build(1)),
            Cow::Owned(_)
        ));
        let huge = plans.get(&key(&[5], &[]), || build(MAX_STAGE_LOOKUPS as u16 + 1));
        assert!(matches!(huge, Cow::Owned(_)));
        // Once every slot holds another key, plans are not kept.
        for i in 0..STAGE_SLOTS as u64 {
            let spec: &'static [u64] = Box::leak(Box::new([100 + i]));
            plans.get(&key(spec, &[]), || build(1));
        }
        assert!(matches!(
            plans.get(&key(&[999], &[]), || build(1)),
            Cow::Owned(_)
        ));
        assert!(plans.heap_bytes() > 0);
    }
}
