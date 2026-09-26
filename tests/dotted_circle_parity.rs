//! Dotted-circle insertion parity: a dependent mark that starts a
//! syllable in an Indic, Khmer, Myanmar, or USE script gets U+25CC
//! DOTTED CIRCLE inserted in front of it, as HarfBuzz's
//! `hb_syllabic_insert_dotted_circles` does, unless the buffer turns
//! it off (`HB_BUFFER_FLAG_DO_NOT_INSERT_DOTTED_CIRCLE`) or the font
//! has no dotted circle glyph.
//!
//! Glyph ids, advances, and offsets are compared with rustybuzz 0.20.
//! Clusters are left out: the Indic shaper does not merge a syllable's
//! clusters the way HarfBuzz does yet.

use rustybuzz::{BufferFlags, Direction as RbDirection};
use sigilbuzz::{shape, Blob, Buffer, BufferFlags as Flags, Direction, Face, Font};

const DEVANAGARI: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Regular.ttf");
const BENGALI: &[u8] = include_bytes!("fonts/NotoSansBengali-Regular.ttf");
const KHMER: &[u8] = include_bytes!("fonts/NotoSansKhmer-Regular.ttf");
const MYANMAR: &[u8] = include_bytes!("fonts/NotoSansMyanmar-Regular.ttf");
const BALINESE: &[u8] = include_bytes!("fonts/NotoSansBalinese-Regular.ttf");
const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");

type Row = (u32, i32, i32, i32);

fn sigilbuzz_rows(data: &[u8], text: &str, circles: bool) -> Vec<Row> {
    let blob = Blob::new(data);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(Direction::Ltr);
    buffer.set_flags(if circles {
        Flags::DEFAULT
    } else {
        Flags::DO_NOT_INSERT_DOTTED_CIRCLE
    });
    shape(&font, &buffer, &[])
        .expect("sigilbuzz shape")
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.x_advance, g.x_offset, g.y_offset))
        .collect()
}

fn rustybuzz_rows(data: &[u8], text: &str, circles: bool) -> Vec<Row> {
    let face = rustybuzz::Face::from_slice(data, 0).expect("parse rustybuzz face");
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_direction(RbDirection::LeftToRight);
    if !circles {
        buffer.set_flags(BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE);
    }
    let out = rustybuzz::shape(&face, &[], buffer);
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| (i.glyph_id, p.x_advance, p.x_offset, p.y_offset))
        .collect()
}

fn assert_parity(data: &[u8], texts: &[&str], circles: bool) {
    let failures: Vec<String> = texts
        .iter()
        .filter_map(|text| {
            let ours = sigilbuzz_rows(data, text, circles);
            let theirs = rustybuzz_rows(data, text, circles);
            (ours != theirs)
                .then(|| format!("{text:?}\n  sigilbuzz: {ours:?}\n  rustybuzz: {theirs:?}"))
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

const DEVANAGARI_BROKEN: &[&str] = &[
    "\u{093F}",
    "\u{093E}",
    "\u{094D}",
    "\u{0902}",
    "\u{093F}\u{0902}",
    "\u{0915} \u{093F}",
];

#[test]
fn devanagari_orphan_marks_sit_on_a_dotted_circle() {
    assert_parity(DEVANAGARI, DEVANAGARI_BROKEN, true);
    let circle = sigilbuzz_rows(DEVANAGARI, "\u{25CC}", true)[0].0;
    let lone_i = sigilbuzz_rows(DEVANAGARI, "\u{093F}", true);
    assert_eq!(lone_i.len(), 2);
    assert!(lone_i.iter().any(|row| row.0 == circle));
}

#[test]
fn bengali_orphan_marks_sit_on_a_dotted_circle() {
    assert_parity(BENGALI, &["\u{09BF}", "\u{09C7}\u{0995}", "\u{09CD}"], true);
}

#[test]
fn khmer_myanmar_and_use_orphans_sit_on_a_dotted_circle() {
    assert_parity(
        KHMER,
        &["\u{17C1}\u{1780}", "\u{17B6}", "\u{17D2}\u{1780}"],
        true,
    );
    assert_parity(MYANMAR, &["\u{102C}", "\u{1031}\u{1000}"], true);
    assert_parity(BALINESE, &["\u{1B36}", "\u{1B3E}\u{1B13}"], true);
}

#[test]
fn the_flag_turns_insertion_off() {
    assert_parity(DEVANAGARI, DEVANAGARI_BROKEN, false);
    assert_parity(KHMER, &["\u{17C1}\u{1780}", "\u{17B6}"], false);
    assert_eq!(sigilbuzz_rows(DEVANAGARI, "\u{093F}", false).len(), 1);
}

#[test]
fn fonts_without_a_dotted_circle_get_none() {
    assert_parity(OPEN_SANS, &["\u{093F}", "\u{17C1}\u{1780}"], true);
}
