//! Feature to lookup resolution, shared by the GSUB and GPOS passes.
//!
//! For each feature the pipeline applies, the shaper walks the run's
//! script-tag priority list (`arab` then `DFLT`, `dev2` then `deva`
//! then `DFLT`, ...) and takes the lookups from the first script whose
//! chosen language system carries the feature. The language system is
//! chosen per script by [`Script::select_lang_sys`]: the buffer
//! language's OpenType tags in order, then a `dflt` record, then the
//! script's default language system.
//!
//! A language system's required feature (`requiredFeatureIndex`)
//! applies whenever the pipeline asks for its tag, merged with that
//! tag's regular lookups, which is where HarfBuzz schedules a required
//! feature whose tag the shaper knows.
//!
//! [`Script::select_lang_sys`]: crate::tables::layout::Script::select_lang_sys

use alloc::vec::Vec;

use crate::tables::layout::{FeatureList, LangSys, ScriptList};

/// Sorted, deduplicated lookup indices that feature `tag` selects.
///
/// Walks `script_priority` in order and returns the lookups of the
/// first script whose selected language system carries `tag`. A script
/// that is present but lacks the feature falls through to the next
/// one. When no listed script carries it, falls back to `DFLT` (unless
/// already tried) and then to the first script in the table, returning
/// that language system's lookups, possibly none. `None` means the
/// table offered no usable language system at all.
pub(crate) fn feature_lookup_indices(
    script_list: &ScriptList<'_>,
    feature_list: &FeatureList<'_>,
    language_tags: &[[u8; 4]],
    tag: [u8; 4],
    script_priority: &[[u8; 4]],
) -> Option<Vec<u16>> {
    for script_tag in script_priority {
        let Some(script) = script_list.find(*script_tag) else {
            continue;
        };
        let Some(lang_sys) = script.select_lang_sys(language_tags) else {
            continue;
        };
        if let Some(indices) = lang_sys_lookups(&lang_sys, feature_list, tag) {
            return Some(indices);
        }
    }

    let already_tried_dflt = script_priority.iter().any(|t| t == b"DFLT");
    let first_script = || script_list.iter().next().map(|(_, s)| s);
    let script = if already_tried_dflt {
        first_script()?
    } else {
        script_list.find(*b"DFLT").or_else(first_script)?
    };
    let lang_sys = script.select_lang_sys(language_tags)?;
    Some(lang_sys_lookups(&lang_sys, feature_list, tag).unwrap_or_default())
}

/// The lookups feature `tag` selects in one language system, with the
/// required feature included when its tag matches. `None` when the
/// language system does not carry the feature at all.
fn lang_sys_lookups(
    lang_sys: &LangSys<'_>,
    feature_list: &FeatureList<'_>,
    tag: [u8; 4],
) -> Option<Vec<u16>> {
    let mut indices: Vec<u16> = Vec::new();
    let mut has_feature = false;
    let required = lang_sys.required_feature_index();
    for feature_index in required.into_iter().chain(lang_sys.feature_indices()) {
        let Some((feature_tag, feature)) = feature_list.get(feature_index) else {
            continue;
        };
        if feature_tag != tag {
            continue;
        }
        has_feature = true;
        for lookup in feature.lookup_indices() {
            if !indices.contains(&lookup) {
                indices.push(lookup);
            }
        }
    }
    has_feature.then(|| {
        indices.sort_unstable();
        indices
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
            let scripts = ScriptList::parse(&self.scripts).unwrap();
            let features = FeatureList::parse(&self.features).unwrap();
            feature_lookup_indices(&scripts, &features, langs, *tag, pri)
        }
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
    fn falls_back_through_the_script_priority() {
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
        // cyrl/SRB lacks liga, so DFLT supplies it.
        assert_eq!(f.lookups(&[*b"SRB "], b"liga", &cyrl), Some(vec![11]));
        // A script missing from the priority list is never consulted,
        // except as the final first-script fallback.
        assert_eq!(f.lookups(&[*b"SRB "], b"locl", &[*b"DFLT"]), Some(vec![]));
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
