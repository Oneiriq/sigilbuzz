//! Each `SubsetInput` flag changes the output the way its doc comment
//! says.

use sigilbuzz::Face;
use sigilbuzz_subset::{subset, SubsetInput};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

const HINTING_TABLES: [[u8; 4]; 3] = [*b"cvt ", *b"fpgm", *b"prep"];

fn gids_for(face: &Face<'_>, text: &str) -> Vec<u16> {
    let cmap = face.cmap().unwrap();
    text.chars().map(|c| cmap.glyph_id(c).unwrap()).collect()
}

/// Glyph instructions call into `fpgm`, run after `prep`, and read
/// `cvt `. `retain_hints` used to keep the instructions and drop those
/// three tables, which left hinting that could not run. They now pass
/// through byte for byte.
#[test]
fn retain_hints_keeps_the_hinting_tables() {
    let face = Face::parse_bytes(OPEN_SANS, 0).unwrap();
    for t in HINTING_TABLES {
        assert!(face.record(t).is_some(), "fixture carries {t:?}");
    }
    let input = SubsetInput {
        gids: gids_for(&face, "Hi"),
        retain_hints: true,
        ..SubsetInput::default()
    };
    let out = subset(&face, &input).unwrap();
    let sub = Face::parse_bytes(&out.bytes, 0).unwrap();
    for t in HINTING_TABLES {
        assert_eq!(
            sub.table_bytes(t).unwrap(),
            face.table_bytes(t).unwrap(),
            "{t:?} passes through",
        );
    }
}

/// Without `retain_hints` the instructions go, and so do the tables
/// they depend on.
#[test]
fn stripped_hints_drop_the_hinting_tables() {
    let face = Face::parse_bytes(OPEN_SANS, 0).unwrap();
    let input = SubsetInput {
        gids: gids_for(&face, "Hi"),
        ..SubsetInput::default()
    };
    let out = subset(&face, &input).unwrap();
    let sub = Face::parse_bytes(&out.bytes, 0).unwrap();
    for t in HINTING_TABLES {
        assert!(sub.record(t).is_none(), "{t:?} is dropped");
    }
}
