//! End-to-end contract of [`sigilbuzz::BidiMap`]: shape a
//! mixed-direction run through `Buffer::set_text_bidi` and prove that
//! every emitted cluster value maps back to the character it came
//! from in the ORIGINAL logical string, the capability RTL caret
//! math in consumers was blocked on.

use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const NOTO_HEBREW: &[u8] = include_bytes!("fonts/NotoSansHebrew-Regular.ttf");

/// Mixed logical text: LTR Latin, RTL Hebrew, digits. The Hebrew font
/// may lack Latin letter glyphs. Irrelevant here, since missing
/// glyphs still carry correct cluster values.
const MIXED: &str = "abc \u{05E9}\u{05DC}\u{05D5}\u{05DD} 123";

#[test]
fn every_shaped_cluster_maps_back_to_its_logical_char() {
    let blob = Blob::new(NOTO_HEBREW);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 16.0);

    let mut buffer = Buffer::new();
    buffer.set_text_bidi(MIXED);
    let visual_text = buffer.text().to_owned();
    let map = buffer.bidi_map().expect("set_text_bidi retains the map");
    assert!(!map.is_identity(), "mixed run must reorder");

    let run = shape(&font, &buffer, &[]).expect("shape");
    assert!(!run.is_empty());

    for glyph in &run.glyphs {
        let cluster = glyph.cluster as usize;

        // The cluster indexes the visual string at a char boundary.
        let visual_char = visual_text[cluster..]
            .chars()
            .next()
            .expect("cluster must be in range");

        // Mapping back to the logical string lands on a char start...
        let logical = map
            .visual_to_logical(cluster)
            .expect("every cluster maps to a logical offset");
        assert!(
            MIXED.is_char_boundary(logical),
            "logical offset {logical} is not a char boundary",
        );

        // ...and on the SAME character.
        let logical_char = MIXED[logical..].chars().next().expect("char start");
        assert_eq!(
            visual_char, logical_char,
            "cluster {cluster} mapped to a different character",
        );

        // The inverse lookup agrees.
        assert_eq!(
            map.logical_to_visual(logical),
            Some(cluster),
            "logical_to_visual must invert visual_to_logical at char starts",
        );
    }
}

#[test]
fn rtl_paragraph_resolves_direction_and_levels() {
    let mut buffer = Buffer::new();
    // Hebrew-leading paragraph: P2/P3 resolves RTL. The stored text is
    // already visual, so the shaping direction is an explicit LTR
    // (shaping it RTL would reverse it a second time); the paragraph
    // direction lives on the bidi map.
    buffer.set_text_bidi("\u{05E9}\u{05DC}\u{05D5}\u{05DD} abc");
    assert_eq!(buffer.direction(), Direction::Ltr);
    assert!(buffer.has_explicit_direction());

    let map = buffer.bidi_map().expect("map retained");
    assert_eq!(map.paragraph_direction(), Direction::Rtl);
    // The Hebrew run sits at an odd (RTL) embedding level; the Latin
    // run inside the RTL paragraph at an even level above it.
    assert_eq!(map.level_at_logical(0).map(|l| l % 2), Some(1));
    let latin_logical = "\u{05E9}\u{05DC}\u{05D5}\u{05DD} ".len();
    assert_eq!(map.level_at_logical(latin_logical).map(|l| l % 2), Some(0));
}

/// `(glyph_id, cluster, x_advance, y_advance, x_offset, y_offset)`.
type Pinned = (u32, u32, i32, i32, i32, i32);

fn shape_bidi(font_bytes: &[u8], text: &str) -> Vec<Pinned> {
    let blob = Blob::new(font_bytes);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.set_text_bidi(text);
    let run = shape(&font, &buffer, &[]).expect("shape");
    run.glyphs
        .iter()
        .map(|g| {
            (
                g.glyph_id,
                g.cluster,
                g.x_advance,
                g.y_advance,
                g.x_offset,
                g.y_offset,
            )
        })
        .collect()
}

/// Pins the shaped output of `set_text_bidi` text. The text is stored
/// in visual order and shaped with an explicit LTR direction, so the
/// glyphs come back in that same visual order: no second reversal even
/// though the paragraphs below resolve RTL.
///
/// Order, glyph ids, clusters, and advances are exactly what the
/// shaper produced before RTL output became visual order. Two mark
/// offsets moved with the attachment rework: a mark now follows a
/// kerning placement on its base (the lamed's +19 below), as in
/// HarfBuzz.
///
/// The all-Hebrew paragraph is one script run in an LTR buffer, which
/// HarfBuzz reads as text in visual order (hb_ensure_native_direction):
/// it reverses the graphemes and shapes them right to left. Its pins
/// are that output, which matches rustybuzz on the same visual text
/// apart from the order of the marks within a grapheme (HarfBuzz sorts
/// them by combining class) and the grapheme clusters.
#[test]
fn set_text_bidi_output_is_pinned() {
    const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");
    let cases: [(&[u8], &str, &[Pinned]); 5] = [
        (
            NOTO_HEBREW,
            "abc \u{05E9}\u{05DC}\u{05D5}\u{05DD} 123",
            &[
                (0, 0, 500, 0, 0, 0),
                (0, 1, 500, 0, 0, 0),
                (0, 2, 500, 0, 0, 0),
                (106, 3, 270, 0, 0, 0),
                (0, 4, 500, 0, 0, 0),
                (0, 5, 500, 0, 0, 0),
                (0, 6, 500, 0, 0, 0),
                (106, 7, 270, 0, 0, 0),
                (23, 8, 684, 0, 0, 0),
                (124, 10, 301, 0, 0, 0),
                (55, 12, 541, 0, 19, 0),
                (96, 14, 730, 0, 0, 0),
            ],
        ),
        (
            NOTO_HEBREW,
            "\u{05E9}\u{05C1}\u{05B8}\u{05DC}\u{05D5}\u{05B9}\u{05DD} abc",
            &[
                (0, 0, 500, 0, 0, 0),
                (0, 1, 500, 0, 0, 0),
                (0, 2, 500, 0, 0, 0),
                (106, 3, 270, 0, 0, 0),
                (23, 4, 684, 0, 0, 0),
                (46, 6, 0, 0, -678, 0),
                (124, 8, 301, 0, 0, 0),
                (55, 10, 541, 0, 19, 0),
                (79, 12, 0, 0, -418, 0),
                (100, 14, 0, 0, 0, 0),
                (96, 16, 730, 0, 0, 0),
            ],
        ),
        (
            NOTO_HEBREW,
            "\u{05D1}\u{05BC}\u{05B0}\u{05E8}\u{05B5}\u{05D0}\u{05E9}\u{05C1}\u{05B4}\u{05D9}\u{05EA}",
            &[
                (107, 0, 685, 0, 0, 0),
                (100, 6, 0, 0, 0, 0),
                (45, 4, 0, 0, 66, 0),
                (138, 2, 275, 0, -20, 0),
                (96, 8, 730, 0, 0, 0),
                (117, 12, 0, 0, 153, 0),
                (3, 10, 612, 0, -20, 0),
                (15, 18, 0, 0, 98, 0),
                (95, 16, 0, 0, 320, 0),
                (86, 14, 523, 0, 0, 0),
                (12, 20, 547, 0, -25, 0),
            ],
        ),
        (
            AMIRI,
            "\u{0628}\u{0650}\u{0633}\u{0652}\u{0645}\u{0650} abc",
            &[
                (6256, 0, 420, 0, 0, 0),
                (6257, 1, 486, 0, 0, 0),
                (6258, 2, 413, 0, 0, 0),
                (1, 3, 292, 0, 0, 0),
                (96, 4, 0, 0, 0, 0),
                (1857, 6, 389, 0, 0, 0),
                (98, 8, 0, 0, -376, 0),
                (1930, 10, 568, 0, 0, 0),
                (96, 12, 0, 0, -526, 0),
                (1589, 14, 883, 0, 0, 0),
            ],
        ),
        (
            AMIRI,
            "abc \u{0645}\u{0631}\u{062D}\u{0628}\u{0627}",
            &[
                (6256, 0, 420, 0, 0, 0),
                (6257, 1, 486, 0, 0, 0),
                (6258, 2, 413, 0, 0, 0),
                (1, 3, 292, 0, 0, 0),
                (55, 4, 217, 0, 0, 0),
                (3763, 6, 587, 0, 0, 0),
                (3548, 8, 55, 0, 0, 0),
                (1888, 10, 343, 0, 0, 0),
                (85, 12, 452, 0, 0, 0),
            ],
        ),
    ];
    for (font, text, expected) in cases {
        assert_eq!(shape_bidi(font, text), expected, "{text:?}");
    }
}

#[test]
fn set_text_bidi_matches_an_explicit_ltr_shape_of_the_visual_text() {
    // The contract in one line: set_text_bidi(text) shapes exactly
    // like set_text(visual) with an explicit LTR direction.
    let text = "\u{05E9}\u{05C1}\u{05B8}\u{05DC}\u{05D5}\u{05B9}\u{05DD} abc";
    let blob = Blob::new(NOTO_HEBREW);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let mut bidi = Buffer::new();
    bidi.set_text_bidi(text);
    let visual = bidi.text().to_owned();

    let mut plain = Buffer::new();
    plain.set_direction(Direction::Ltr);
    plain.set_text(&visual);

    let a = shape(&font, &bidi, &[]).expect("shape bidi");
    let b = shape(&font, &plain, &[]).expect("shape plain");
    assert_eq!(a.glyphs, b.glyphs);
}

#[test]
fn text_mutations_invalidate_the_map() {
    let mut buffer = Buffer::new();
    buffer.set_text_bidi("abc \u{05D0}");
    assert!(buffer.bidi_map().is_some());

    buffer.set_text("plain");
    assert!(buffer.bidi_map().is_none(), "set_text must drop the map");

    buffer.set_text_bidi("abc \u{05D0}");
    buffer.push_str("more");
    assert!(buffer.bidi_map().is_none(), "push_str must drop the map");

    buffer.set_text_bidi("abc \u{05D0}");
    buffer.clear();
    assert!(buffer.bidi_map().is_none(), "clear must drop the map");
}

#[test]
fn pure_ltr_map_is_identity_and_matches_set_text_semantics() {
    let mut buffer = Buffer::new();
    buffer.set_text_bidi("hello world");
    assert_eq!(buffer.text(), "hello world");
    assert_eq!(buffer.direction(), Direction::Ltr);
    let map = buffer.bidi_map().expect("map retained even for LTR");
    assert!(map.is_identity());
    assert_eq!(map.visual_to_logical(6), Some(6));
}
