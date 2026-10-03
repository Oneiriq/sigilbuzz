//! Each `SubsetInput` flag changes the output the way its doc comment
//! says.

use sigilbuzz::Face;
use sigilbuzz_subset::{subset, SubsetError, SubsetInput};

#[path = "support/sfnt.rs"]
mod support;

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

const SOURCE_SANS_3_VF: &[u8] =
    include_bytes!("../../../tests/fonts/SourceSans3VF-Latin-Subset.otf");
const SOURCE_CODE_PRO: &[u8] =
    include_bytes!("../../../tests/fonts/SourceCodePro-Latin-Subset.otf");

const LAYOUT_TABLES: [[u8; 4]; 3] = [*b"GSUB", *b"GPOS", *b"GDEF"];
const VARIATION_TABLES: [[u8; 4]; 3] = [*b"fvar", *b"avar", *b"HVAR"];

/// The x advances of `text` at the normalized `wght` coordinate
/// `coord`.
fn advances_at(face: &Face<'_>, text: &str, coord: f32) -> Vec<i32> {
    let coords = [coord];
    let font = sigilbuzz::Font::new(face.clone(), 1000.0).with_coords(&coords);
    let mut buffer = sigilbuzz::Buffer::new();
    buffer.push_str(text);
    let run = sigilbuzz::shape(&font, &buffer, &[]).unwrap();
    run.glyphs.iter().map(|g| g.x_advance).collect()
}

/// Subsetting a CFF2 font to a few glyphs used to drop GSUB, GPOS,
/// GDEF, fvar, avar, and HVAR whatever the flags said, so the subset
/// lost its kerning and stopped being variable. With the defaults the
/// tables now stay, rewritten for the new glyph ids like on the `glyf`
/// path.
#[test]
fn cff2_subset_keeps_layout_and_variations_by_default() {
    let face = Face::parse_bytes(SOURCE_SANS_3_VF, 0).unwrap();
    let input = SubsetInput {
        gids: gids_for(&face, "fiAVT"),
        ..SubsetInput::default()
    };
    let out = subset(&face, &input).unwrap();
    let sub = Face::parse_bytes(&out.bytes, 0).unwrap();
    assert!(sub.record(*b"CFF2").is_some());
    assert!(sub.maxp().unwrap().num_glyphs < face.maxp().unwrap().num_glyphs);
    for t in LAYOUT_TABLES.iter().chain(&VARIATION_TABLES) {
        assert!(sub.record(*t).is_some(), "{t:?} is kept");
    }
    assert_eq!(
        sub.table_bytes(*b"fvar").unwrap(),
        face.table_bytes(*b"fvar").unwrap()
    );

    // Kerning survives: the subset shapes "AVAT" like the source at the
    // default instance. (The GDEF rewriter drops the variation store,
    // so kerning deltas at other instances are lost, on the glyf path
    // too. See src/gdef.rs.)
    assert_ne!(
        advances_at(&face, "AV", 0.0)[0],
        advances_at(&face, "A", 0.0)[0],
        "the source kerns AV",
    );
    assert_eq!(
        advances_at(&sub, "AVAT", 0.0),
        advances_at(&face, "AVAT", 0.0)
    );
    // HVAR survives: each advance follows the axis as in the source.
    for ch in ["A", "V", "T"] {
        assert_eq!(advances_at(&sub, ch, 1.0), advances_at(&face, ch, 1.0));
    }
    assert_ne!(advances_at(&sub, "A", 1.0), advances_at(&sub, "A", 0.0));
}

/// The flags also apply when every glyph is kept, where the CFF path
/// copies tables through. It used to copy the layout and variable-font
/// tables even when asked to drop them.
#[test]
fn cff_passthrough_drops_what_the_flags_drop() {
    let face = Face::parse_bytes(SOURCE_SANS_3_VF, 0).unwrap();
    let all: Vec<u16> = (0..face.maxp().unwrap().num_glyphs).collect();
    let input = SubsetInput {
        gids: all,
        retain_layout: false,
        retain_variations: false,
        ..SubsetInput::default()
    };
    let out = subset(&face, &input).unwrap();
    let sub = Face::parse_bytes(&out.bytes, 0).unwrap();
    assert_eq!(
        sub.table_bytes(*b"CFF2").unwrap(),
        face.table_bytes(*b"CFF2").unwrap()
    );
    for t in LAYOUT_TABLES.iter().chain(&VARIATION_TABLES) {
        assert!(sub.record(*t).is_none(), "{t:?} is dropped");
    }
}

/// In strict mode a table without a subset implementation is an error.
/// The CFF path used to drop such tables silently even in strict mode.
/// The fixture's `BASE` used to be one; it is kept now, so a `DSIG` is
/// grafted on to stand in for the tables that still are not.
#[test]
fn cff_strict_mode_rejects_unhandled_tables() {
    let font = support::edit_tables(
        SOURCE_CODE_PRO,
        &[(*b"DSIG", Some(vec![0, 0, 0, 1, 0, 0, 0, 0]))],
    );
    let face = Face::parse_bytes(&font, 0).unwrap();
    let strict = SubsetInput {
        gids: gids_for(&face, "Hi"),
        drop_unhandled: false,
        ..SubsetInput::default()
    };
    assert!(matches!(
        subset(&face, &strict),
        Err(SubsetError::Unsupported(_))
    ));
    let permissive = SubsetInput {
        drop_unhandled: true,
        ..strict
    };
    let out = subset(&face, &permissive).unwrap();
    let sub = Face::parse_bytes(&out.bytes, 0).unwrap();
    assert!(sub.record(*b"DSIG").is_none());
}

/// `BASE` has a subset implementation, so strict mode keeps it. Source
/// Code Pro's has no reference glyphs and passes through.
#[test]
fn cff_strict_mode_keeps_base() {
    let face = Face::parse_bytes(SOURCE_CODE_PRO, 0).unwrap();
    assert!(face.record(*b"BASE").is_some(), "fixture carries BASE");
    let strict = SubsetInput {
        gids: gids_for(&face, "Hi"),
        drop_unhandled: false,
        ..SubsetInput::default()
    };
    let out = subset(&face, &strict).unwrap();
    let sub = Face::parse_bytes(&out.bytes, 0).unwrap();
    assert_eq!(
        sub.table_bytes(*b"BASE").unwrap(),
        face.table_bytes(*b"BASE").unwrap()
    );
}
