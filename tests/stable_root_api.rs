//! Stable crate-root API surface test.
//!
//! Audit follow-up #235. Verifies that the curated subset of
//! `sigilbuzz::ot` and `sigilbuzz::unicode` graduated in 0.20.0 is
//! reachable through crate-root paths *without* going through the
//! `#[doc(hidden)]` parent modules. This is the contract: downstream
//! consumers (text renderers, rasterizers, future shapers) are free to write
//! `use sigilbuzz::{UnicodeScript, BidiInfo, JoiningType, JoiningForm,
//! feature, BidiClass, bidi_class, script_of, is_hangul_jamo}` and
//! pin against those names. The deep paths stay internal.
//!
//! If a re-export here goes away, the test breaks at compile time:
//! that is exactly the alarm we want.

// Crate-root names only. The compiler enforces that none of these go
// through `sigilbuzz::ot::*` or `sigilbuzz::unicode::*`.
use sigilbuzz::{
    bidi_class, feature, is_hangul_jamo, script_of, BidiClass, BidiInfo, JoiningForm, JoiningType,
    UnicodeScript,
};

#[test]
fn unicode_script_classifies_via_root_name() {
    assert_eq!(script_of('A'), UnicodeScript::Latin);
    assert_eq!(script_of('字'), UnicodeScript::Han);
    assert_eq!(script_of('\u{0915}'), UnicodeScript::Devanagari);
}

#[test]
fn hangul_jamo_predicate_via_root_name() {
    assert!(is_hangul_jamo('\u{1100}'));
    assert!(!is_hangul_jamo('A'));
}

#[test]
fn bidi_class_lookup_via_root_name() {
    // Latin uppercase is L (left-to-right strong).
    assert_eq!(bidi_class('A'), BidiClass::L);
    // Hebrew alef is R (right-to-left strong).
    assert_eq!(bidi_class('\u{05D0}'), BidiClass::R);
}

#[test]
fn bidi_info_runs_via_root_name() {
    let info = BidiInfo::new("Hello", None);
    assert_eq!(info.char_count(), 5);
    // Pure-Latin paragraph defaults to LTR: every level resolves to 0.
    assert!(info.levels().iter().all(|&lvl| lvl == 0));
}

#[test]
fn joining_type_via_root_name() {
    // Crate-root re-export keeps the enum intact; smoke-test by
    // round-tripping the `U` (non-joining) and `D` (dual-joining)
    // variants which are stable across the joining-type spec.
    let u = JoiningType::U;
    let d = JoiningType::D;
    assert_ne!(u, d);
    assert_eq!(u, JoiningType::U);
}

#[test]
fn joining_form_via_root_name() {
    // Same shape: Arabic joining-form enum reachable at root.
    let isol = JoiningForm::Isol;
    let init = JoiningForm::Init;
    assert_ne!(isol, init);
}

#[test]
fn feature_constants_via_root_name() {
    // `feature` is the module re-export; constants are byte arrays.
    assert_eq!(feature::LIGA, *b"liga");
    assert_eq!(feature::KERN, *b"kern");
    assert_eq!(feature::CALT, *b"calt");
}
