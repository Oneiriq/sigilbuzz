//! A feature list that names a tag more than once takes the last
//! value, as HarfBuzz's map builder does when it merges the entries of
//! a global feature. HarfBuzz 14.5.0 shapes Calibri "office" with
//! `liga=0,liga=1` with the ffi ligature, and "AVAT" with
//! `kern=0,kern=1` kerned; Open Sans has the same features.

use sigilbuzz::{shape, Blob, Buffer, Face, Feature, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");

fn feature(tag: &[u8; 4], value: u32) -> Feature {
    Feature { tag: *tag, value }
}

fn glyphs(text: &str, features: &[Feature]) -> Vec<(u32, i32, u32)> {
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    shape(&font, &buffer, features)
        .unwrap()
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.x_advance, g.cluster))
        .collect()
}

#[test]
fn the_last_liga_entry_wins() {
    let on = glyphs("office", &[]);
    let off = glyphs("office", &[feature(b"liga", 0)]);
    assert_ne!(on, off, "Open Sans ligates ffi");
    assert_eq!(
        glyphs("office", &[feature(b"liga", 0), feature(b"liga", 1)]),
        on
    );
    assert_eq!(
        glyphs("office", &[feature(b"liga", 1), feature(b"liga", 0)]),
        off
    );
    // Other features in between do not change which entry is last.
    let between = [
        feature(b"liga", 0),
        feature(b"kern", 1),
        feature(b"liga", 1),
    ];
    assert_eq!(glyphs("office", &between), on);
}

#[test]
fn the_last_kern_entry_wins() {
    let on = glyphs("AVAT", &[]);
    let off = glyphs("AVAT", &[feature(b"kern", 0)]);
    assert_ne!(on, off, "Open Sans kerns AV and AT");
    assert_eq!(
        glyphs("AVAT", &[feature(b"kern", 0), feature(b"kern", 1)]),
        on
    );
    assert_eq!(
        glyphs("AVAT", &[feature(b"kern", 1), feature(b"kern", 0)]),
        off
    );
}
