//! Unicode Variation Sequences (cmap format 14) against HarfBuzz.
//!
//! HarfBuzz reads the format 14 subtable under `(0, 5)`
//! (`CmapSubtableFormat14` in `hb-ot-cmap-table.hh`). Its normalizer
//! (`handle_variation_selector_cluster` in `hb-ot-shape-normalize.cc`)
//! looks a character followed by a variation selector up as a pair.
//! When the font has a glyph for the pair, that glyph replaces both
//! characters and the selector's cluster merges into the character's.
//! A default sequence gives the character's usual glyph. Otherwise both
//! map on their own, and the selector is hidden like any other default
//! ignorable (the space glyph with no advance).
//!
//! The font is a subset of Noto Sans CJK JP (see
//! `tests/fixtures/README.md`) with default and non-default sequences
//! for VS1, VS2, and VS17 to VS19. The expected glyphs, clusters, and
//! advances come from uharfbuzz with HarfBuzz 14.5.0, one list per
//! cluster level (MONOTONE_GRAPHEMES, MONOTONE_CHARACTERS, CHARACTERS,
//! GRAPHEMES). Every y advance and offset is zero.

use sigilbuzz::{shape, Buffer, BufferFlags, ClusterLevel, Direction, Face, Font};

const FONT: &[u8] = include_bytes!("fixtures/noto_sans_cjk_jp_uvs_subset.otf");

const LEVELS: [ClusterLevel; 4] = [
    ClusterLevel::MonotoneGraphemes,
    ClusterLevel::MonotoneCharacters,
    ClusterLevel::Characters,
    ClusterLevel::Graphemes,
];

/// `(glyph, cluster, x_advance)` for every glyph.
type Expected = &'static [(u32, u32, i32)];

/// Text, then HarfBuzz's output at each of [`LEVELS`].
const CASES: &[(&str, [Expected; 4])] = &[
    // U+845B: VS17 has its own glyph, VS18 is a default sequence, VS19
    // is not listed.
    (
        "\u{845B}\u{E0100}",
        [
            &[(29, 0, 1000)],
            &[(29, 0, 1000)],
            &[(29, 0, 1000)],
            &[(29, 0, 1000)],
        ],
    ),
    (
        "\u{845B}\u{E0101}",
        [
            &[(12, 0, 1000)],
            &[(12, 0, 1000)],
            &[(12, 0, 1000)],
            &[(12, 0, 1000)],
        ],
    ),
    (
        "\u{845B}\u{E0102}",
        [
            &[(12, 0, 1000), (1, 0, 0)],
            &[(12, 0, 1000), (1, 3, 0)],
            &[(12, 0, 1000), (1, 3, 0)],
            &[(12, 0, 1000), (1, 0, 0)],
        ],
    ),
    // U+6F22: VS1 and VS18 share a glyph, VS17 is a default sequence.
    (
        "\u{6F22}\u{FE00}",
        [
            &[(21, 0, 1000)],
            &[(21, 0, 1000)],
            &[(21, 0, 1000)],
            &[(21, 0, 1000)],
        ],
    ),
    (
        "\u{6F22}\u{E0100}",
        [
            &[(10, 0, 1000)],
            &[(10, 0, 1000)],
            &[(10, 0, 1000)],
            &[(10, 0, 1000)],
        ],
    ),
    (
        "\u{6F22}\u{E0101}",
        [
            &[(21, 0, 1000)],
            &[(21, 0, 1000)],
            &[(21, 0, 1000)],
            &[(21, 0, 1000)],
        ],
    ),
    // Punctuation with standardized variants: U+3001 has VS1 as its
    // default and VS2 as a variant, U+FF01 the other way round.
    (
        "\u{3001}\u{FE00}",
        [
            &[(7, 0, 1000)],
            &[(7, 0, 1000)],
            &[(7, 0, 1000)],
            &[(7, 0, 1000)],
        ],
    ),
    (
        "\u{3001}\u{FE01}",
        [
            &[(38, 0, 1000)],
            &[(38, 0, 1000)],
            &[(38, 0, 1000)],
            &[(38, 0, 1000)],
        ],
    ),
    (
        "\u{FF01}\u{FE00}",
        [
            &[(40, 0, 1000)],
            &[(40, 0, 1000)],
            &[(40, 0, 1000)],
            &[(40, 0, 1000)],
        ],
    ),
    (
        "\u{FF01}\u{FE01}",
        [
            &[(26, 0, 1000)],
            &[(26, 0, 1000)],
            &[(26, 0, 1000)],
            &[(26, 0, 1000)],
        ],
    ),
    (
        "\u{9089}\u{E0102}",
        [
            &[(33, 0, 1000)],
            &[(33, 0, 1000)],
            &[(33, 0, 1000)],
            &[(33, 0, 1000)],
        ],
    ),
    // A base with no sequences at all.
    (
        "a\u{FE00}",
        [
            &[(4, 0, 563), (1, 0, 0)],
            &[(4, 0, 563), (1, 1, 0)],
            &[(4, 0, 563), (1, 1, 0)],
            &[(4, 0, 563), (1, 0, 0)],
        ],
    ),
    // A second selector after a resolved sequence stays on its own.
    (
        "\u{845B}\u{E0100}\u{E0101}",
        [
            &[(29, 0, 1000), (1, 0, 0)],
            &[(29, 0, 1000), (1, 7, 0)],
            &[(29, 0, 1000), (1, 7, 0)],
            &[(29, 0, 1000), (1, 0, 0)],
        ],
    ),
    (
        "\u{845B}\u{E0101}\u{FE00}",
        [
            &[(12, 0, 1000), (1, 0, 0)],
            &[(12, 0, 1000), (1, 7, 0)],
            &[(12, 0, 1000), (1, 7, 0)],
            &[(12, 0, 1000), (1, 0, 0)],
        ],
    ),
    // A selector with no base.
    (
        "\u{E0100}\u{845B}",
        [
            &[(1, 0, 0), (12, 4, 1000)],
            &[(1, 0, 0), (12, 4, 1000)],
            &[(1, 0, 0), (12, 4, 1000)],
            &[(1, 0, 0), (12, 4, 1000)],
        ],
    ),
    // Sequences inside other text ('b' is not in the font).
    (
        "a\u{845B}\u{E0100}b\u{6F22}\u{FE00}",
        [
            &[(4, 0, 563), (29, 1, 1000), (0, 8, 1000), (21, 9, 1000)],
            &[(4, 0, 563), (29, 1, 1000), (0, 8, 1000), (21, 9, 1000)],
            &[(4, 0, 563), (29, 1, 1000), (0, 8, 1000), (21, 9, 1000)],
            &[(4, 0, 563), (29, 1, 1000), (0, 8, 1000), (21, 9, 1000)],
        ],
    ),
    // Only the character right before the selector counts.
    (
        "\u{845B}\u{0301}\u{E0100}",
        [
            &[(12, 0, 1000), (0, 0, 0), (1, 0, 0)],
            &[(12, 0, 1000), (0, 3, 0), (1, 5, 0)],
            &[(12, 0, 1000), (0, 3, 0), (1, 5, 0)],
            &[(12, 0, 1000), (0, 0, 0), (1, 0, 0)],
        ],
    ),
    (
        "\u{845B}\u{E0100}\u{0301}",
        [
            &[(29, 0, 1000), (0, 0, 0)],
            &[(29, 0, 1000), (0, 7, 0)],
            &[(29, 0, 1000), (0, 7, 0)],
            &[(29, 0, 1000), (0, 0, 0)],
        ],
    ),
];

fn shape_at(font: &Font<'_>, text: &str, level: ClusterLevel) -> Vec<(u32, u32, i32)> {
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(Direction::Ltr);
    buffer.set_cluster_level(level);
    let run = shape(font, &buffer, &[]).expect("shape");
    for g in &run.glyphs {
        assert_eq!((g.y_advance, g.x_offset, g.y_offset), (0, 0, 0), "{text:?}");
    }
    run.glyphs
        .iter()
        .map(|g| (g.glyph_id, g.cluster, g.x_advance))
        .collect()
}

#[test]
fn variation_sequences_shape_like_harfbuzz() {
    let face = Face::parse_bytes(FONT, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut failures = Vec::new();
    for (text, expected) in CASES {
        for (level, want) in LEVELS.iter().zip(expected) {
            let got = shape_at(&font, text, *level);
            if got != *want {
                failures.push(format!(
                    "{text:?} {level:?}\n  got  {got:?}\n  want {want:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn variation_glyph_lookups_match_harfbuzz() {
    // hb_font_get_variation_glyph and hb_font_get_nominal_glyph.
    let face = Face::parse_bytes(FONT, 0).expect("parse face");
    let cases = [
        ('\u{845B}', '\u{E0100}', Some(29)),
        ('\u{845B}', '\u{E0101}', Some(12)),
        ('\u{845B}', '\u{E0102}', None),
        ('\u{6F22}', '\u{FE00}', Some(21)),
        ('\u{3001}', '\u{FE00}', Some(7)),
        ('a', '\u{FE00}', None),
        ('\u{9089}', '\u{E0100}', Some(19)),
        ('\u{9089}', '\u{E0102}', Some(33)),
    ];
    for (base, selector, want) in cases {
        assert_eq!(
            face.variation_glyph(base, selector).expect("cmap"),
            want,
            "{base:?} {selector:?}"
        );
    }
    let cmap = face.cmap().expect("cmap");
    for (ch, want) in [
        ('\u{845B}', 12),
        ('\u{6F22}', 10),
        ('\u{3001}', 7),
        ('a', 4),
    ] {
        assert_eq!(cmap.glyph_id(ch), Some(want));
    }
}

#[test]
fn collected_selectors_and_unicodes_match_harfbuzz() {
    // hb_face_collect_variation_selectors and
    // hb_face_collect_variation_unicodes.
    let cmap = Face::parse_bytes(FONT, 0)
        .and_then(|face| face.cmap())
        .expect("cmap");
    assert_eq!(
        cmap.variation_selectors(),
        [0xFE00, 0xFE01, 0xE0100, 0xE0101, 0xE0102]
    );
    let cases: [(u32, &[u32]); 6] = [
        (0xFE00, &[0x3001, 0x3002, 0x6F22, 0xFF01, 0xFF0C]),
        (0xFE01, &[0x3001, 0x3002, 0xFF01, 0xFF0C]),
        (0xE0100, &[0x4E08, 0x6F22, 0x845B, 0x8FBB, 0x9089]),
        (0xE0101, &[0x4E08, 0x6F22, 0x845B, 0x8FBB, 0x9089]),
        (0xE0102, &[0x9089]),
        (0xE0103, &[]),
    ];
    for (selector, want) in cases {
        assert_eq!(cmap.variation_unicodes(selector), want, "{selector:X}");
    }
}

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");

/// Font, text, not-found glyph, cluster level, flags, and the expected
/// glyphs.
type NotFoundCase = (
    &'static [u8],
    &'static str,
    Option<u32>,
    ClusterLevel,
    BufferFlags,
    Expected,
);

/// `hb_buffer_set_not_found_variation_selector_glyph`: font, text, the
/// not-found glyph, cluster level, flags, and HarfBuzz 14.5.0's output
/// as `(glyph, cluster, x_advance)`. These come from HarfBuzz's C API
/// (uharfbuzz does not expose the setting). Every y advance and offset
/// is zero.
const NOT_FOUND_CASES: &[NotFoundCase] = &[
    (FONT, "a\u{FE00}", None, MC, NONE, &[(4, 0, 563), (1, 1, 0)]),
    // Unset, an unresolved selector is hidden. Meanwhile it is no mark,
    // so the marks after it stay with it.
    (
        FONT,
        "a\u{FE00}\u{0301}",
        None,
        MC,
        NONE,
        &[(4, 0, 563), (1, 1, 0), (0, 4, 0)],
    ),
    (
        FONT,
        "a\u{FE00}\u{0301}",
        None,
        MC,
        PRESERVE,
        &[(4, 0, 563), (0, 1, 1000), (0, 4, 0)],
    ),
    (
        OPEN_SANS,
        "a\u{FE00}\u{0301}\u{0302}",
        None,
        MC,
        NONE,
        &[(68, 0, 1139), (3, 1, 0), (612, 4, 0), (0, 6, 1229)],
    ),
    (
        OPEN_SANS,
        "a\u{FE00}\u{0301}",
        None,
        MC,
        PRESERVE,
        &[(68, 0, 1139), (0, 1, 1229), (612, 4, 0)],
    ),
    (
        AMIRI,
        "a\u{FE00}\u{0301}\u{0302}",
        None,
        MC,
        NONE,
        &[(6256, 0, 420), (1, 1, 0), (6519, 4, 0), (6520, 6, 0)],
    ),
    (
        AMIRI,
        "a\u{FE00}\u{0301}",
        None,
        MC,
        PRESERVE,
        &[(6256, 0, 420), (0, 1, 364), (6519, 4, 0)],
    ),
    (
        FONT,
        "a\u{FE00}",
        Some(5),
        MC,
        NONE,
        &[(4, 0, 563), (5, 1, 0)],
    ),
    (
        FONT,
        "a\u{FE00}",
        Some(0),
        MC,
        NONE,
        &[(4, 0, 563), (0, 1, 0)],
    ),
    (
        FONT,
        "\u{845B}\u{E0102}",
        Some(5),
        MC,
        NONE,
        &[(12, 0, 1000), (5, 3, 0)],
    ),
    // A resolved sequence, then a further selector: hidden as usual.
    (
        FONT,
        "\u{845B}\u{E0100}\u{E0101}",
        Some(5),
        MC,
        NONE,
        &[(29, 0, 1000), (1, 7, 0)],
    ),
    (
        FONT,
        "\u{845B}\u{E0102}\u{FE00}",
        Some(5),
        MC,
        NONE,
        &[(12, 0, 1000), (5, 3, 0), (1, 7, 0)],
    ),
    // A default sequence resolves, so nothing is shown.
    (
        FONT,
        "\u{845B}\u{E0101}",
        Some(5),
        MC,
        NONE,
        &[(12, 0, 1000)],
    ),
    // A selector with no base is not looked up as a pair.
    (
        FONT,
        "\u{FE00}a",
        Some(5),
        MC,
        NONE,
        &[(1, 0, 0), (4, 3, 563)],
    ),
    (
        FONT,
        "a\u{FE00}\u{0301}",
        Some(5),
        MC,
        NONE,
        &[(4, 0, 563), (5, 1, 0), (0, 4, 0)],
    ),
    (
        FONT,
        "a\u{FE00}",
        Some(5),
        ClusterLevel::MonotoneGraphemes,
        NONE,
        &[(4, 0, 563), (5, 0, 0)],
    ),
    (
        FONT,
        "a\u{FE00}",
        Some(5),
        ClusterLevel::Characters,
        NONE,
        &[(4, 0, 563), (5, 1, 0)],
    ),
    (
        FONT,
        "a\u{FE00}",
        Some(5),
        ClusterLevel::Graphemes,
        NONE,
        &[(4, 0, 563), (5, 0, 0)],
    ),
    // Removing default ignorables keeps the shown selector.
    (
        FONT,
        "a\u{FE00}\u{FE01}",
        Some(5),
        MC,
        REMOVE,
        &[(4, 0, 563), (5, 1, 0)],
    ),
    (
        FONT,
        "a\u{FE00}\u{FE01}",
        Some(5),
        MC,
        BufferFlags::PRESERVE_DEFAULT_IGNORABLES,
        &[(4, 0, 563), (5, 1, 0), (0, 4, 1000)],
    ),
    (
        FONT,
        "a\u{FE00}b",
        Some(12),
        MC,
        NONE,
        &[(4, 0, 563), (12, 1, 0), (0, 4, 1000)],
    ),
    (
        FONT,
        "\u{3001}\u{FE02}",
        Some(12),
        MC,
        NONE,
        &[(7, 0, 1000), (12, 3, 0)],
    ),
    (
        OPEN_SANS,
        "a\u{FE00}",
        Some(5),
        MC,
        NONE,
        &[(68, 0, 1139), (5, 1, 0)],
    ),
    (
        OPEN_SANS,
        "a\u{FE00}\u{0301}",
        Some(70),
        MC,
        NONE,
        &[(68, 0, 1139), (70, 1, 0), (612, 4, 0)],
    ),
    (
        OPEN_SANS,
        "a\u{FE00}b",
        Some(70),
        MC,
        NONE,
        &[(68, 0, 1139), (70, 1, 0), (69, 4, 1255)],
    ),
];

const MC: ClusterLevel = ClusterLevel::MonotoneCharacters;
const NONE: BufferFlags = BufferFlags::DEFAULT;
const PRESERVE: BufferFlags = BufferFlags::PRESERVE_DEFAULT_IGNORABLES;
const REMOVE: BufferFlags = BufferFlags::REMOVE_DEFAULT_IGNORABLES;

#[test]
fn unresolved_selectors_show_the_not_found_glyph_like_harfbuzz() {
    let mut failures = Vec::new();
    for &(data, text, not_found, level, flags, want) in NOT_FOUND_CASES {
        let font = Font::new(Face::parse_bytes(data, 0).expect("parse face"), 1000.0);
        let mut buffer = Buffer::new();
        buffer.push_str(text);
        buffer.set_direction(Direction::Ltr);
        buffer.set_cluster_level(level);
        buffer.set_flags(flags);
        buffer.set_not_found_variation_selector_glyph(not_found);
        let run = shape(&font, &buffer, &[]).expect("shape");
        let got: Vec<(u32, u32, i32)> = run
            .glyphs
            .iter()
            .map(|g| {
                assert_eq!((g.y_advance, g.x_offset, g.y_offset), (0, 0, 0), "{text:?}");
                (g.glyph_id, g.cluster, g.x_advance)
            })
            .collect();
        if got != want {
            failures.push(format!(
                "{text:?} {not_found:?} {level:?} {flags:?}\n  got  {got:?}\n  want {want:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
