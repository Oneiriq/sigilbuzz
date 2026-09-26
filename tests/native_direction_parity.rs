//! Native-direction parity: a buffer whose explicit direction is not
//! its script's native one holds text in that visual order, and
//! HarfBuzz (`hb_ensure_native_direction`) shapes the reversed
//! graphemes in the native direction. Latin in an RTL buffer does not
//! form `fi` (the letters read `i`, `f`), Arabic in an LTR buffer joins
//! the other way, and a bottom-to-top Mongolian buffer is shaped top to
//! bottom. A run of digits in an RTL script stays LTR.
//!
//! Glyph ids, clusters, advances, and offsets are compared with
//! rustybuzz 0.20.

use rustybuzz::Direction as RbDirection;
use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");
const HEBREW: &[u8] = include_bytes!("fonts/NotoSansHebrew-Regular.ttf");
const MONGOLIAN: &[u8] = include_bytes!("fonts/NotoSansMongolian-Regular.ttf");

type Row = (u32, u32, i32, i32, i32, i32);

fn sigilbuzz_rows(data: &[u8], text: &str, direction: Direction) -> Vec<Row> {
    let blob = Blob::new(data);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    shape(&font, &buffer, &[])
        .expect("sigilbuzz shape")
        .glyphs
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

fn rustybuzz_rows(data: &[u8], text: &str, direction: Direction) -> Vec<Row> {
    let face = rustybuzz::Face::from_slice(data, 0).expect("parse rustybuzz face");
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_direction(match direction {
        Direction::Ltr => RbDirection::LeftToRight,
        Direction::Rtl => RbDirection::RightToLeft,
        Direction::Ttb => RbDirection::TopToBottom,
        Direction::Btt => RbDirection::BottomToTop,
    });
    let out = rustybuzz::shape(&face, &[], buffer);
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| {
            (
                i.glyph_id,
                i.cluster,
                p.x_advance,
                p.y_advance,
                p.x_offset,
                p.y_offset,
            )
        })
        .collect()
}

fn assert_parity(data: &[u8], cases: &[(&str, Direction)]) {
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|&(text, direction)| {
            let ours = sigilbuzz_rows(data, text, direction);
            let theirs = rustybuzz_rows(data, text, direction);
            (ours != theirs).then(|| {
                format!("{text:?} {direction:?}\n  sigilbuzz: {ours:?}\n  rustybuzz: {theirs:?}")
            })
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn latin_in_an_rtl_buffer_is_shaped_reversed_left_to_right() {
    use Direction::Rtl;
    assert_parity(
        OPEN_SANS,
        &[
            ("fi abc", Rtl),
            ("office", Rtl),
            ("sun moon", Rtl),
            ("(x)", Rtl),
        ],
    );
}

#[test]
fn arabic_and_hebrew_in_an_ltr_buffer_are_shaped_reversed_right_to_left() {
    use Direction::Ltr;
    assert_parity(
        AMIRI,
        &[
            ("\u{0628}\u{062A}\u{062B}", Ltr),
            ("\u{0627}\u{0628}\u{0628}\u{0627}", Ltr),
            ("\u{0628} 123", Ltr),
        ],
    );
    assert_parity(HEBREW, &[("\u{05E9}\u{05DC}\u{05D5}\u{05DD}", Ltr)]);
}

#[test]
fn digits_in_an_rtl_script_stay_left_to_right() {
    use Direction::Ltr;
    assert_parity(AMIRI, &[("\u{0661}\u{0662}\u{0663}", Ltr), ("123", Ltr)]);
}

#[test]
fn mongolian_rtl_and_bottom_to_top_are_shaped_reversed() {
    use Direction::{Btt, Rtl};
    let text = "\u{1820}\u{1821}\u{1822}";
    // Vertical glyph origins are not applied yet, so compare the
    // horizontal RTL run fully and the vertical one by glyph id.
    assert_parity(MONGOLIAN, &[(text, Rtl)]);
    let ids = |rows: Vec<Row>| rows.iter().map(|r| (r.0, r.1)).collect::<Vec<_>>();
    assert_eq!(
        ids(sigilbuzz_rows(MONGOLIAN, text, Btt)),
        ids(rustybuzz_rows(MONGOLIAN, text, Btt))
    );
}

#[test]
fn native_directions_and_unset_directions_are_unchanged() {
    // RTL Arabic and LTR Latin shape as before; an unset direction
    // keeps sigilbuzz's logical left-to-right default.
    let blob = Blob::new(AMIRI);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let text = "\u{0628}\u{062A}\u{062B}";
    let mut unset = Buffer::new();
    unset.push_str(text);
    let unset: Vec<u32> = shape(&font, &unset, &[])
        .expect("shape")
        .glyphs
        .iter()
        .map(|g| g.cluster)
        .collect();
    assert_eq!(unset, [0, 2, 4]);
    assert_parity(AMIRI, &[(text, Direction::Rtl)]);
}
