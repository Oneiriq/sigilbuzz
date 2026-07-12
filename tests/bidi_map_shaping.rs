//! End-to-end contract of [`sigilbuzz::BidiMap`]: shape a
//! mixed-direction run through `Buffer::set_text_bidi` and prove that
//! every emitted cluster value maps back to the character it came
//! from in the ORIGINAL logical string — the capability RTL caret
//! math in consumers (oniq) was blocked on.

use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const NOTO_HEBREW: &[u8] = include_bytes!("fonts/NotoSansHebrew-Regular.ttf");

/// Mixed logical text: LTR Latin, RTL Hebrew, digits. The Hebrew font
/// may lack Latin letter glyphs — irrelevant here, since missing
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
    // Hebrew-leading paragraph: P2/P3 resolves RTL.
    buffer.set_text_bidi("\u{05E9}\u{05DC}\u{05D5}\u{05DD} abc");
    assert_eq!(buffer.direction(), Direction::Rtl);

    let map = buffer.bidi_map().expect("map retained");
    assert_eq!(map.paragraph_direction(), Direction::Rtl);
    // The Hebrew run sits at an odd (RTL) embedding level; the Latin
    // run inside the RTL paragraph at an even level above it.
    assert_eq!(map.level_at_logical(0).map(|l| l % 2), Some(1));
    let latin_logical = "\u{05E9}\u{05DC}\u{05D5}\u{05DD} ".len();
    assert_eq!(map.level_at_logical(latin_logical).map(|l| l % 2), Some(0));
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
