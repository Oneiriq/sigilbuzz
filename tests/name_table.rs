//! Integration test for the `name` table parser against the
//! vendored Open Sans regular fixture.
//!
//! Picks the family / subfamily / postscript / full / version
//! accessors and asserts each one returns the upstream string. This
//! is the path oniq's font browser exercises after #210 lands; we
//! assert the strings round-trip rather than making any claim about
//! the encoding selection internals.

use sigilbuzz::{Blob, Face};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");

#[test]
fn open_sans_exposes_expected_name_strings() {
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).unwrap();
    let name = face
        .name()
        .expect("Open Sans has a name table")
        .expect("name table parses cleanly");

    assert_eq!(name.family_name().as_deref(), Some("Open Sans"));
    assert_eq!(name.subfamily_name().as_deref(), Some("Regular"));
    // Open Sans's full name (Name ID 4) is the family name without
    // the subfamily appended in this build.
    assert_eq!(name.full_name().as_deref(), Some("Open Sans"));
    assert_eq!(name.postscript_name().as_deref(), Some("OpenSans"));

    let unique = name.unique_id().expect("unique id present");
    assert!(unique.contains("Open Sans"), "got {unique:?}");

    let version = name.version().expect("version string present");
    assert!(version.starts_with("Version "), "got {version:?}");

    // Records iterator surfaces the raw directory so callers that
    // need niche IDs (designer URL, license, ...) still have a path.
    assert!(!name.records().is_empty());
}
