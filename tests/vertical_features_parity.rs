//! The default GSUB features of vertical text, checked against
//! HarfBuzz.
//!
//! HarfBuzz turns `calt`, `clig`, `liga` and `rclt` on by default in
//! horizontal text only (`horizontal_features` in `hb-ot-shape.cc`),
//! and vertical text gets `vert` alone, never `vrt2`. A caller can turn
//! any of them on or off in either direction. Every expectation is
//! HarfBuzz 14.5.0's output (through uharfbuzz 0.56.2) at the
//! MONOTONE_CHARACTERS cluster level: glyph id, cluster as a UTF-8 byte
//! offset, x and y advance, and x and y offset.

use sigilbuzz::{shape, Blob, Buffer, ClusterLevel, Direction, Face, Feature, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
const CJK: &[u8] = include_bytes!("fixtures/noto_sans_cjk_jp_uvs_subset.otf");
const MONGOLIAN: &[u8] = include_bytes!("fonts/NotoSansMongolian-Regular.ttf");

type Row = (u32, u32, i32, i32, i32, i32);

fn rows(font: &[u8], text: &str, direction: Direction, features: &[Feature]) -> Vec<Row> {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).expect("face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    buffer.set_cluster_level(ClusterLevel::MonotoneCharacters);
    let run = shape(&font, &buffer, features).expect("shape");
    run.glyphs
        .iter()
        .map(|g| {
            let p = (g.x_advance, g.y_advance, g.x_offset, g.y_offset);
            (g.glyph_id, g.cluster, p.0, p.1, p.2, p.3)
        })
        .collect()
}

fn on(tag: &[u8; 4]) -> Feature {
    Feature {
        tag: *tag,
        value: 1,
    }
}

fn off(tag: &[u8; 4]) -> Feature {
    Feature {
        tag: *tag,
        value: 0,
    }
}

#[test]
fn vertical_text_forms_no_ligature_unless_asked() {
    let separate = [
        (82, 0, 0, -2789, -618, -1942),
        (73, 1, 0, -2789, -347, -2178),
        (73, 2, 0, -2789, -347, -2178),
        (76, 3, 0, -2789, -259, -2146),
        (70, 4, 0, -2789, -487, -1942),
        (72, 5, 0, -2789, -574, -1942),
    ];
    assert_eq!(rows(OPEN_SANS, "office", Direction::Ttb, &[]), separate);
    let ligated = [
        (82, 0, 0, -2789, -618, -1942),
        (605, 1, 0, -2789, -954, -2178),
        (70, 4, 0, -2789, -487, -1942),
        (72, 5, 0, -2789, -574, -1942),
    ];
    assert_eq!(
        rows(OPEN_SANS, "office", Direction::Ttb, &[on(b"liga")]),
        ligated
    );
}

#[test]
fn vertical_text_gets_vert_and_never_vrt2_by_default() {
    // The fixture has both `vert` and `vrt2`. With `vert` off HarfBuzz
    // applies neither, so the punctuation keeps its horizontal forms.
    let text = "\u{3001}\u{3002}";
    assert_eq!(
        rows(CJK, text, Direction::Ttb, &[]),
        [(23, 0, 0, -1000, -500, -880), (24, 3, 0, -1000, -500, -880)]
    );
    assert_eq!(
        rows(CJK, text, Direction::Ttb, &[off(b"vert")]),
        [(7, 0, 0, -1000, -500, -880), (8, 3, 0, -1000, -500, -880)]
    );
    // A caller can turn `vert` on in horizontal text.
    assert_eq!(
        rows(CJK, text, Direction::Ltr, &[on(b"vert")]),
        [(23, 0, 1000, 0, 0, 0), (24, 3, 1000, 0, 0, 0)]
    );
}

#[test]
fn vertical_mongolian_runs_neither_calt_nor_rclt() {
    // Noto Sans Mongolian lists the same contextual lookups under `calt`
    // and `rclt`, and either one changes the glyph of U+1822 here.
    // HarfBuzz runs neither in vertical text.
    assert_eq!(
        rows(MONGOLIAN, "\u{1820}\u{1822}\u{1828}", Direction::Ttb, &[]),
        [
            (8, 0, 0, -1000, -393, -880),
            (26, 3, 0, -1000, -180, -880),
            (76, 6, 0, -1000, -213, -880)
        ]
    );
}
