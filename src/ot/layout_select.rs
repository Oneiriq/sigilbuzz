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

use alloc::vec::Vec;

use crate::tables::layout::{ActiveFeatures, LangSys, ScriptList};

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
    let with_tag = |index: u16| {
        features
            .get(index)
            .filter(|(feature_tag, _)| *feature_tag == tag)
            .map(|(_, feature)| feature)
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
    fn empty_script_list_yields_none() {
        let f = Fixture {
            scripts: script_list(&[]),
            features: feature_list(&[]),
        };
        assert_eq!(f.lookups(&[], b"liga", &[*b"DFLT"]), None);
    }
}
