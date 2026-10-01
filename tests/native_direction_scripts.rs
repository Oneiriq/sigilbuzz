//! Native direction for scripts sigilbuzz has no shaper for, against
//! HarfBuzz.
//!
//! HarfBuzz reverses a buffer whose direction is not its script's
//! native one (`hb_ensure_native_direction` in `hb-ot-shape.cc`, with
//! `hb_script_get_horizontal_direction` in `hb-common.cc`), merging each
//! reversed grapheme's clusters at MONOTONE_CHARACTERS. sigilbuzz takes
//! the native direction of these scripts from the Bidi_Class of their
//! first strong character, so it depends on the UCD-generated
//! `bidi_class` table: the old hand-picked table classed Samaritan,
//! Mandaic, Adlam, and the others below as neutral, and shaped them as
//! left to right. Old Hungarian, Old Italic, Runic, and Tifinagh have no
//! native direction in HarfBuzz and are never reversed.
//!
//! Open Sans has none of these letters, so the glyphs are .notdef (and
//! U+0301 for the mark). The order and clusters show the reversal.
//! Expected output from uharfbuzz with HarfBuzz 14.5.0, cluster level
//! MONOTONE_CHARACTERS: `(glyph, cluster, x_advance, x_offset,
//! y_offset)`.

use sigilbuzz::{shape, Buffer, ClusterLevel, Direction, Face, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");

type Expected = &'static [(u32, u32, i32, i32, i32)];

const CASES: &[(&str, Direction, &str, Expected)] = &[
    (
        "samaritan",
        Direction::Ltr,
        "\u{0800}\u{0816}\u{0801}",
        &[(0, 0, 1229, 0, 0), (0, 0, 1229, 0, 0), (0, 6, 1229, 0, 0)],
    ),
    (
        "samaritan",
        Direction::Rtl,
        "\u{0800}\u{0816}\u{0801}",
        &[(0, 6, 1229, 0, 0), (0, 3, 1229, 0, 0), (0, 0, 1229, 0, 0)],
    ),
    (
        "mandaic",
        Direction::Ltr,
        "\u{0840}\u{0859}\u{0841}",
        &[(0, 0, 1229, 0, 0), (0, 0, 1229, 0, 0), (0, 6, 1229, 0, 0)],
    ),
    (
        "mandaic",
        Direction::Rtl,
        "\u{0840}\u{0859}\u{0841}",
        &[(0, 6, 1229, 0, 0), (0, 3, 1229, 0, 0), (0, 0, 1229, 0, 0)],
    ),
    (
        "adlam",
        Direction::Ltr,
        "\u{1E900}\u{1E944}\u{1E901}",
        &[(0, 0, 1229, 0, 0), (0, 0, 1229, 0, 0), (0, 8, 1229, 0, 0)],
    ),
    (
        "adlam",
        Direction::Rtl,
        "\u{1E900}\u{1E944}\u{1E901}",
        &[(0, 8, 1229, 0, 0), (0, 4, 1229, 0, 0), (0, 0, 1229, 0, 0)],
    ),
    (
        "kharoshthi",
        Direction::Ltr,
        "\u{10A10}\u{10A0D}\u{10A11}",
        &[(0, 0, 1229, 0, 0), (0, 0, 1229, 0, 0), (0, 8, 1229, 0, 0)],
    ),
    (
        "kharoshthi",
        Direction::Rtl,
        "\u{10A10}\u{10A0D}\u{10A11}",
        &[(0, 8, 1229, 0, 0), (0, 4, 1229, 0, 0), (0, 0, 1229, 0, 0)],
    ),
    (
        "syriac_sup",
        Direction::Ltr,
        "\u{0860}\u{0301}\u{0861}",
        &[(612, 0, 0, 0, 0), (0, 0, 1229, 0, 0), (0, 5, 1229, 0, 0)],
    ),
    (
        "syriac_sup",
        Direction::Rtl,
        "\u{0860}\u{0301}\u{0861}",
        &[(0, 5, 1229, 0, 0), (612, 3, 0, 0, 0), (0, 0, 1229, 0, 0)],
    ),
    (
        "rohingya",
        Direction::Ltr,
        "\u{10D00}\u{10D24}\u{10D01}",
        &[(0, 0, 1229, 0, 0), (0, 0, 1229, 0, 0), (0, 8, 1229, 0, 0)],
    ),
    (
        "rohingya",
        Direction::Rtl,
        "\u{10D00}\u{10D24}\u{10D01}",
        &[(0, 8, 1229, 0, 0), (0, 4, 1229, 0, 0), (0, 0, 1229, 0, 0)],
    ),
    (
        "garay",
        Direction::Ltr,
        "\u{10D50}\u{10D69}\u{10D51}",
        &[(0, 0, 1229, 0, 0), (0, 0, 1229, 0, 0), (0, 8, 1229, 0, 0)],
    ),
    (
        "garay",
        Direction::Rtl,
        "\u{10D50}\u{10D69}\u{10D51}",
        &[(0, 8, 1229, 0, 0), (0, 4, 1229, 0, 0), (0, 0, 1229, 0, 0)],
    ),
    (
        "phoenician",
        Direction::Ltr,
        "\u{10900}\u{0301}\u{10901}",
        &[(612, 0, 0, 0, 0), (0, 0, 1229, 0, 0), (0, 6, 1229, 0, 0)],
    ),
    (
        "phoenician",
        Direction::Rtl,
        "\u{10900}\u{0301}\u{10901}",
        &[(0, 6, 1229, 0, 0), (612, 4, 0, 0, 0), (0, 0, 1229, 0, 0)],
    ),
    (
        "thaana",
        Direction::Ltr,
        "\u{0780}\u{07A6}\u{0781}",
        &[(0, 0, 1229, 0, 0), (0, 0, 1229, 0, 0), (0, 4, 1229, 0, 0)],
    ),
    (
        "thaana",
        Direction::Rtl,
        "\u{0780}\u{07A6}\u{0781}",
        &[(0, 4, 1229, 0, 0), (0, 2, 1229, 0, 0), (0, 0, 1229, 0, 0)],
    ),
    (
        "old_hungarian",
        Direction::Ltr,
        "\u{10C80}\u{0301}\u{10C81}",
        &[(0, 0, 1229, 0, 0), (612, 4, 0, 0, 0), (0, 6, 1229, 0, 0)],
    ),
    (
        "old_hungarian",
        Direction::Rtl,
        "\u{10C80}\u{0301}\u{10C81}",
        &[(0, 6, 1229, 0, 0), (612, 4, 0, 0, 0), (0, 0, 1229, 0, 0)],
    ),
    (
        "old_italic",
        Direction::Ltr,
        "\u{10300}\u{0301}\u{10301}",
        &[(0, 0, 1229, 0, 0), (612, 4, 0, 0, 0), (0, 6, 1229, 0, 0)],
    ),
    (
        "old_italic",
        Direction::Rtl,
        "\u{10300}\u{0301}\u{10301}",
        &[(0, 6, 1229, 0, 0), (612, 4, 0, 0, 0), (0, 0, 1229, 0, 0)],
    ),
    (
        "runic",
        Direction::Ltr,
        "\u{16A0}\u{0301}\u{16A1}",
        &[(0, 0, 1229, 0, 0), (612, 3, 0, 0, 0), (0, 5, 1229, 0, 0)],
    ),
    (
        "runic",
        Direction::Rtl,
        "\u{16A0}\u{0301}\u{16A1}",
        &[(0, 5, 1229, 0, 0), (612, 3, 0, 0, 0), (0, 0, 1229, 0, 0)],
    ),
    (
        "tifinagh",
        Direction::Ltr,
        "\u{2D30}\u{0301}\u{2D31}",
        &[(0, 0, 1229, 0, 0), (612, 3, 0, 0, 0), (0, 5, 1229, 0, 0)],
    ),
    (
        "tifinagh",
        Direction::Rtl,
        "\u{2D30}\u{0301}\u{2D31}",
        &[(0, 5, 1229, 0, 0), (612, 3, 0, 0, 0), (0, 0, 1229, 0, 0)],
    ),
];

#[test]
fn scripts_without_a_shaper_take_harfbuzz_native_direction() {
    let font = Font::new(Face::parse_bytes(OPEN_SANS, 0).expect("parse"), 1000.0);
    let mut failures = Vec::new();
    for &(name, direction, text, want) in CASES {
        let mut buffer = Buffer::new();
        buffer.push_str(text);
        buffer.set_direction(direction);
        buffer.set_cluster_level(ClusterLevel::MonotoneCharacters);
        let got: Vec<(u32, u32, i32, i32, i32)> = shape(&font, &buffer, &[])
            .expect("shape")
            .glyphs
            .iter()
            .map(|g| (g.glyph_id, g.cluster, g.x_advance, g.x_offset, g.y_offset))
            .collect();
        if got != want {
            failures.push(format!(
                "{name} {direction:?}\n  got  {got:?}\n  want {want:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
