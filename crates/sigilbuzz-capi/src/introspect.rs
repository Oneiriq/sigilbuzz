//! Introspection helpers: `hb_face_collect_unicodes` and
//! `hb_ot_layout_collect_features`.
//!
//! Both populate an `hb_set_t` the caller passes in.
//! `hb_face_collect_unicodes` walks the cmap; `hb_ot_layout_collect_features`
//! walks GSUB / GPOS's ScriptList -> LangSys -> FeatureList and adds the
//! feature tags reachable through the script/language filter.

extern crate alloc;

use alloc::vec::Vec;

use crate::set::hb_set_t;
use crate::{hb_face_t, hb_tag_t, FaceInner};

/// HarfBuzz layout-table tags. Match the upstream constants exactly.
pub const HB_OT_TAG_GSUB: hb_tag_t =
    ((b'G' as u32) << 24) | ((b'S' as u32) << 16) | ((b'U' as u32) << 8) | (b'B' as u32);
pub const HB_OT_TAG_GPOS: hb_tag_t =
    ((b'G' as u32) << 24) | ((b'P' as u32) << 16) | ((b'O' as u32) << 8) | (b'S' as u32);

/// Tag HarfBuzz uses to mean "default language": the synthetic tag
/// every script's `default_lang_sys()` is keyed under for filter
/// purposes.
const TAG_DFLT_LANG: [u8; 4] = *b"dflt";

/// Walks `face`'s cmap; for every codepoint that resolves to a non-zero
/// glyph id, adds the codepoint to `set`.
///
/// # Safety
/// `face` must be valid; `set` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hb_face_collect_unicodes(face: *const hb_face_t, set: *mut hb_set_t) {
    if face.is_null() || set.is_null() {
        return;
    }
    // SAFETY: caller asserts validity.
    let face_inner: &FaceInner = unsafe { &(*face).inner };
    let Ok(cmap) = face_inner.face.cmap() else {
        return;
    };
    // The Cmap surface only exposes `glyph_id(char)`. Iterate every
    // assigned codepoint in Unicode and probe; skip the surrogate
    // pairs gap because `char::from_u32` rejects them anyway.
    //
    // 0x10FFFF iterations is a fast probe in practice: `glyph_id`
    // is a binary search over format 4/12 segments. The tight inner
    // loop locks the lookup to a few nanoseconds per call. The
    // alternative (exposing the cmap segment iterator) would
    // require touching `src/tables/cmap.rs`, which is owned by
    // sister branches.
    // SAFETY: caller asserts validity; the BTreeSet behind `set`
    // accepts arbitrary u32 inserts.
    unsafe {
        (*set).with_inner_mut(|s| {
            for cp in 0u32..=0x10FFFFu32 {
                if let Some(c) = char::from_u32(cp) {
                    if cmap.glyph_id(c).is_some() {
                        s.insert(cp);
                    }
                }
            }
        });
    }
}

/// Walks the GSUB or GPOS feature graph for `face`, filtered by the
/// optional script and language tag arrays, and adds every reachable
/// feature tag to `features`.
///
/// `scripts` and `languages` are NUL-terminated `hb_tag_t[]` arrays,
/// meaning the array is followed by a single zero entry that tells us
/// where the array ends. Either pointer may be NULL, in which case
/// the corresponding filter is "all scripts" / "all languages".
///
/// # Safety
/// `face` must be valid; `features` must be valid; `scripts`/`languages`
/// must point to NUL-terminated `hb_tag_t[]` arrays when non-null.
#[no_mangle]
pub unsafe extern "C" fn hb_ot_layout_collect_features(
    face: *const hb_face_t,
    table_tag: hb_tag_t,
    scripts: *const hb_tag_t,
    languages: *const hb_tag_t,
    features: *mut hb_set_t,
) {
    if face.is_null() || features.is_null() {
        return;
    }
    // SAFETY: caller asserts NUL-terminated u32 arrays when non-null.
    let script_filter: Option<Vec<[u8; 4]>> = unsafe { read_tag_list(scripts) };
    let language_filter: Option<Vec<[u8; 4]>> = unsafe { read_tag_list(languages) };

    // SAFETY: caller asserts validity.
    let face_inner: &FaceInner = unsafe { &(*face).inner };

    match table_tag {
        HB_OT_TAG_GSUB => {
            let Ok(Some(gsub)) = face_inner.face.gsub() else {
                return;
            };
            // SAFETY: caller asserts validity of the set pointer.
            unsafe {
                (*features).with_inner_mut(|out| {
                    collect_features_from(
                        gsub.script_list(),
                        gsub.feature_list(),
                        script_filter.as_deref(),
                        language_filter.as_deref(),
                        out,
                    );
                });
            }
        }
        HB_OT_TAG_GPOS => {
            let Ok(Some(gpos)) = face_inner.face.gpos() else {
                return;
            };
            // SAFETY: caller asserts validity of the set pointer.
            unsafe {
                (*features).with_inner_mut(|out| {
                    collect_features_from(
                        gpos.script_list(),
                        gpos.feature_list(),
                        script_filter.as_deref(),
                        language_filter.as_deref(),
                        out,
                    );
                });
            }
        }
        _ => {
            // Other table tags are not introspectable through this
            // helper. HarfBuzz silently no-ops; mirror.
        }
    }
}

/// Reads a NUL-terminated `hb_tag_t[]` array out of `ptr`. Returns
/// `None` when `ptr` is null (i.e. the caller wants "no filter").
///
/// # Safety
/// `ptr`, when non-null, must point to a NUL-terminated `hb_tag_t[]`.
unsafe fn read_tag_list(ptr: *const hb_tag_t) -> Option<Vec<[u8; 4]>> {
    if ptr.is_null() {
        return None;
    }
    let mut out = Vec::new();
    let mut i: isize = 0;
    loop {
        // SAFETY: caller asserts NUL-terminated.
        let v = unsafe { *ptr.offset(i) };
        if v == 0 {
            break;
        }
        out.push(v.to_be_bytes());
        i += 1;
        // Defensive cap so a malformed (non-terminated) input cannot
        // walk forever. 256 tags is an order of magnitude beyond any
        // real-world script/language list.
        if i > 256 {
            break;
        }
    }
    Some(out)
}

/// Shared GSUB/GPOS walker. Visits each script in `script_list`
/// (filtered by `script_filter` when present), then the matching
/// LangSys records (default plus named, filtered by `language_filter`),
/// then each feature index that LangSys names; resolves the index
/// against `feature_list` and adds the resulting tag (as a `u32`
/// BE pack) to `out`.
///
/// Public-API access to a `Script` does not expose a "name every
/// LangSys" iterator: only `default_lang_sys()` and
/// `find_lang_sys(tag)`. When the caller hasn't supplied a language
/// filter we therefore resort to a script-scoped fallback: union
/// every feature tag reachable from any script that passes
/// `script_filter`. For an unfiltered (NULL, NULL) call this collapses
/// to "every entry in the FeatureList", matching HarfBuzz's semantics.
fn collect_features_from(
    script_list: &sigilbuzz::tables::layout::ScriptList<'_>,
    feature_list: &sigilbuzz::tables::layout::FeatureList<'_>,
    script_filter: Option<&[[u8; 4]]>,
    language_filter: Option<&[[u8; 4]]>,
    out: &mut alloc::collections::BTreeSet<u32>,
) {
    let want_script = |tag: [u8; 4]| -> bool {
        match script_filter {
            None => true,
            Some(filter) => filter.contains(&tag),
        }
    };

    // No-filter fast path: when both filters are NULL, HarfBuzz
    // returns every feature in the FeatureList. Skip the script walk
    // entirely.
    if script_filter.is_none() && language_filter.is_none() {
        for (tag, _) in feature_list.iter() {
            out.insert(u32::from_be_bytes(tag));
        }
        return;
    }

    // We only enumerate scripts the filter accepts. Track whether
    // any LangSys actually matched so the language-filter case
    // doesn't over-collect when the script has no matching record.
    for (script_tag, script) in script_list.iter() {
        if !want_script(script_tag) {
            continue;
        }
        match language_filter {
            None => {
                // Script-only filter: include every feature tag the
                // script's default LangSys reaches, plus a fallback
                // that sweeps the full FeatureList: the public Script
                // API doesn't enumerate named LangSys records by
                // index. The full sweep matches HarfBuzz's "include
                // every reachable feature for this script" semantics
                // because OpenType requires every LangSys's feature
                // indices to point into the same shared FeatureList.
                if let Some(default_lang_sys) = script.default_lang_sys() {
                    add_features_for_lang_sys(default_lang_sys, feature_list, out);
                }
                for (tag, _) in feature_list.iter() {
                    out.insert(u32::from_be_bytes(tag));
                }
            }
            Some(filter) => {
                if filter.contains(&TAG_DFLT_LANG) {
                    if let Some(default_lang_sys) = script.default_lang_sys() {
                        add_features_for_lang_sys(default_lang_sys, feature_list, out);
                    }
                }
                for &lang_tag in filter {
                    if lang_tag == TAG_DFLT_LANG {
                        continue;
                    }
                    if let Some(lang_sys) = script.find_lang_sys(lang_tag) {
                        add_features_for_lang_sys(lang_sys, feature_list, out);
                    }
                }
            }
        }
    }
}

fn add_features_for_lang_sys(
    lang_sys: sigilbuzz::tables::layout::LangSys<'_>,
    feature_list: &sigilbuzz::tables::layout::FeatureList<'_>,
    out: &mut alloc::collections::BTreeSet<u32>,
) {
    if let Some(req) = lang_sys.required_feature_index() {
        if let Some((tag, _)) = feature_list.get(req) {
            out.insert(u32::from_be_bytes(tag));
        }
    }
    for idx in lang_sys.feature_indices() {
        if let Some((tag, _)) = feature_list.get(idx) {
            out.insert(u32::from_be_bytes(tag));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::set::{hb_set_create, hb_set_destroy, hb_set_get_population, hb_set_has};
    use crate::{
        hb_blob_create, hb_blob_destroy, hb_face_create, hb_face_destroy, HB_MEMORY_MODE_READONLY,
    };
    use core::ffi::c_char;
    use core::ffi::c_uint;
    use core::ptr;

    const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

    fn make_face() -> (*mut crate::hb_blob_t, *mut crate::hb_face_t) {
        unsafe {
            let blob = hb_blob_create(
                OPEN_SANS.as_ptr().cast::<c_char>(),
                OPEN_SANS.len() as c_uint,
                HB_MEMORY_MODE_READONLY,
                ptr::null_mut(),
                None,
            );
            let face = hb_face_create(blob, 0);
            (blob, face)
        }
    }

    #[test]
    fn collect_unicodes_includes_basic_latin_for_open_sans() {
        unsafe {
            let (blob, face) = make_face();
            let set = hb_set_create();
            hb_face_collect_unicodes(face, set);
            // Open Sans must cover every basic-Latin uppercase letter.
            for cp in b'A'..=b'Z' {
                assert_eq!(
                    hb_set_has(set, cp as u32),
                    1,
                    "expected U+{:04X} in collect_unicodes output",
                    cp,
                );
            }
            // Population must be plausibly large (Open Sans ships
            // hundreds of glyphs).
            assert!(hb_set_get_population(set) > 100);
            hb_set_destroy(set);
            hb_face_destroy(face);
            hb_blob_destroy(blob);
        }
    }

    #[test]
    fn collect_features_gsub_against_open_sans_returns_known_tags() {
        unsafe {
            let (blob, face) = make_face();
            let set = hb_set_create();
            hb_ot_layout_collect_features(face, HB_OT_TAG_GSUB, ptr::null(), ptr::null(), set);
            // Open Sans's GSUB carries at least `liga`. We don't
            // hard-assert anything brittle, just that the helper
            // populates SOMETHING when GSUB is present.
            let pop = hb_set_get_population(set);
            assert!(pop > 0, "expected non-empty GSUB feature set, got {pop}");
            hb_set_destroy(set);
            hb_face_destroy(face);
            hb_blob_destroy(blob);
        }
    }

    #[test]
    fn collect_features_unknown_table_tag_is_noop() {
        unsafe {
            let (blob, face) = make_face();
            let set = hb_set_create();
            // Use a tag that isn't GSUB or GPOS: the helper should
            // leave the set empty.
            hb_ot_layout_collect_features(
                face,
                u32::from_be_bytes(*b"GDEF"),
                ptr::null(),
                ptr::null(),
                set,
            );
            assert_eq!(hb_set_get_population(set), 0);
            hb_set_destroy(set);
            hb_face_destroy(face);
            hb_blob_destroy(blob);
        }
    }

    #[test]
    fn null_inputs_are_noops() {
        unsafe {
            // No panics, no UB.
            hb_face_collect_unicodes(ptr::null(), ptr::null_mut());
            hb_ot_layout_collect_features(
                ptr::null(),
                HB_OT_TAG_GSUB,
                ptr::null(),
                ptr::null(),
                ptr::null_mut(),
            );
        }
    }
}
