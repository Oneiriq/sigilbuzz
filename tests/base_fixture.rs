//! Integration test for the OpenType `BASE` table.
//!
//! Drives [`Face::base`] and the BASE subtable parsers against a
//! hand-crafted synthetic font built in-test. Real fonts that ship
//! BASE are rare in OFL designs (Adobe's flagship faces include it,
//! but Adobe's licensing is not OFL; Noto Sans CJK ships a Latin
//! script entry but the file is far too heavy to vendor for one
//! integration test). A synthetic SFNT with a tiny BASE table is
//! sufficient to drive the table-bytes wiring end-to-end and
//! exercise the Face accessor.
//!
//! The fixture mirrors the layout `build_minimal_base` produces in
//! `src/tables/base.rs::tests`: one horizontal axis, one script
//! (`latn`), one baseline tag (`romn`) at y = 0, plus a default
//! MinMax of (-200, 800).

use sigilbuzz::{Blob, Face};

/// Builds an SFNT directory wrapping a single `BASE` table. We add
/// stub `head` / `maxp` / `hhea` / `cmap` / `hmtx` records too so
/// the `Face::parse` directory walk succeeds; the test itself only
/// reaches for `face.base()` so the stubs can be empty payloads.
fn synthetic_font_with_base(base_bytes: &[u8]) -> Vec<u8> {
    // Tag sort order is by ASCII; the SFNT spec doesn't actually
    // require the directory to be sorted, but real fonts are, so
    // we sort to match what a search-tree-aware client would expect.
    let tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"BASE", base_bytes.to_vec()),
        // Minimal `head`: 54 bytes of zeros is enough to satisfy
        // the table directory bounds check; we never call
        // `face.head()`.
        (b"head", vec![0u8; 54]),
        (b"maxp", vec![0u8; 6]),
    ];

    let num_tables = tables.len();
    let header_len = 12 + num_tables * 16;
    let mut out: Vec<u8> = Vec::new();

    // SFNT header: TrueType magic.
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(num_tables as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // searchRange
    out.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
    out.extend_from_slice(&0u16.to_be_bytes()); // rangeShift

    // Table directory.
    let mut cursor = header_len as u32;
    for (tag, data) in &tables {
        out.extend_from_slice(tag.as_slice());
        out.extend_from_slice(&0u32.to_be_bytes()); // checksum
        out.extend_from_slice(&cursor.to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        cursor += data.len() as u32;
    }

    for (_, data) in &tables {
        out.extend_from_slice(data);
    }
    out
}

/// Hand-rolled minimal BASE: one horizontal axis, one script
/// (`latn`), one baseline tag (`romn` at y = 0) and a default
/// MinMax of (-200, 800). Mirrors the unit-test fixture.
fn minimal_base_bytes() -> Vec<u8> {
    let u16be = |v: u16| v.to_be_bytes();
    let i16be = |v: i16| v.to_be_bytes();

    let mut out: Vec<u8> = Vec::new();

    // BASE header.
    out.extend_from_slice(&u16be(1)); // major
    out.extend_from_slice(&u16be(0)); // minor
    out.extend_from_slice(&u16be(8)); // horizAxisOffset
    out.extend_from_slice(&u16be(0)); // vertAxisOffset

    // Horizontal axis (offset 8).
    let axis_off = out.len();
    let tl_slot = axis_off;
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(0));

    // Tag list: ["romn"].
    let tag_list_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(b"romn");

    // Script list: ["latn"].
    let script_list_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(b"latn");
    let s_slot = out.len();
    out.extend_from_slice(&u16be(0));

    // BaseScript.
    let script_off = out.len();
    let bv_slot = out.len();
    out.extend_from_slice(&u16be(0));
    let mm_slot = out.len();
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(0)); // langSysCount

    // BaseValues.
    let bv_off = out.len();
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(1));
    let coord_slot = out.len();
    out.extend_from_slice(&u16be(0));

    // BaseCoord (format 1, y = 0).
    let coord_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(&i16be(0));

    // Default MinMax: (-200, 800).
    let mm_off = out.len();
    let mm_min_slot = out.len();
    out.extend_from_slice(&u16be(0));
    let mm_max_slot = out.len();
    out.extend_from_slice(&u16be(0));
    out.extend_from_slice(&u16be(0));

    let min_coord_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(&i16be(-200));
    let max_coord_off = out.len();
    out.extend_from_slice(&u16be(1));
    out.extend_from_slice(&i16be(800));

    // Backfill axis tag-list / script-list offsets.
    out[tl_slot..tl_slot + 2].copy_from_slice(&u16be((tag_list_off - axis_off) as u16));
    out[tl_slot + 2..tl_slot + 4].copy_from_slice(&u16be((script_list_off - axis_off) as u16));
    out[s_slot..s_slot + 2].copy_from_slice(&u16be((script_off - script_list_off) as u16));
    out[bv_slot..bv_slot + 2].copy_from_slice(&u16be((bv_off - script_off) as u16));
    out[mm_slot..mm_slot + 2].copy_from_slice(&u16be((mm_off - script_off) as u16));
    out[coord_slot..coord_slot + 2].copy_from_slice(&u16be((coord_off - bv_off) as u16));
    out[mm_min_slot..mm_min_slot + 2].copy_from_slice(&u16be((min_coord_off - mm_off) as u16));
    out[mm_max_slot..mm_max_slot + 2].copy_from_slice(&u16be((max_coord_off - mm_off) as u16));

    out
}

#[test]
fn face_base_is_some_for_synthetic_font_with_base() {
    let base_bytes = minimal_base_bytes();
    let font = synthetic_font_with_base(&base_bytes);
    let blob = Blob::new(&font);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(face.base().unwrap().is_some());
}

#[test]
fn face_base_resolves_baseline_and_min_max() {
    let base_bytes = minimal_base_bytes();
    let font = synthetic_font_with_base(&base_bytes);
    let blob = Blob::new(&font);
    let face = Face::parse(&blob, 0).unwrap();
    let base = face.base().unwrap().expect("BASE present");
    let axis = base.horizontal_axis().expect("horizontal axis present");
    assert_eq!(axis.baseline_tags(), vec![*b"romn"]);
    let script = axis.script(*b"latn").expect("latn script present");
    assert_eq!(script.baseline(*b"romn"), Some(0));
    assert_eq!(script.min_max(None), Some((-200, 800)));
    assert!(base.vertical_axis().is_none());
}

#[test]
fn face_base_returns_none_for_font_without_base() {
    // Open Sans ships no BASE table.
    let bytes = std::fs::read("tests/fixtures/opensans_regular.ttf").unwrap();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(face.base().unwrap().is_none());
}
