//! Glyph flags checked against HarfBuzz.
//!
//! Every expectation is HarfBuzz 14.5.0's output (through uharfbuzz
//! 0.56.2) for the same font, text, direction, cluster level, and
//! buffer flags: glyph id, cluster, and `hb_glyph_info_get_glyph_flags`
//! for each glyph, in output order. rustybuzz 0.20 produces no
//! unsafe-to-concatenate or tatweel flags, so it is not compared.

use sigilbuzz::{shape, Blob, Buffer, BufferFlags, ClusterLevel, Direction, Face, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
const RUBIK: &[u8] = include_bytes!("fixtures/rubik_vf.ttf");
const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");
const MONGOLIAN: &[u8] = include_bytes!("fonts/NotoSansMongolian-Regular.ttf");
const DEVANAGARI: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Regular.ttf");
const SINHALA: &[u8] = include_bytes!("fonts/NotoSansSinhala-Regular.ttf");

const MC: ClusterLevel = ClusterLevel::MonotoneCharacters;
const CONCAT: BufferFlags = BufferFlags::PRODUCE_UNSAFE_TO_CONCAT;

/// Shapes `text` and returns (glyph id, cluster, flag bits) per glyph.
fn flags(
    font: &[u8],
    text: &str,
    direction: Direction,
    level: ClusterLevel,
    buffer_flags: BufferFlags,
) -> Vec<(u32, u32, u32)> {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).expect("face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    buffer.set_cluster_level(level);
    buffer.set_flags(buffer_flags);
    let run = shape(&font, &buffer, &[]).expect("shape");
    run.glyphs
        .iter()
        .map(|g| (g.glyph_id, g.cluster, g.flags.bits()))
        .collect()
}

#[test]
fn kerned_pairs_are_unsafe_to_break() {
    // PairPos (PairSet::apply): a pair that moved a glyph marks it.
    assert_eq!(
        flags(
            OPEN_SANS,
            "AVATAR",
            Direction::Ltr,
            MC,
            BufferFlags::DEFAULT
        ),
        [
            (36, 0, 0),
            (57, 1, 1),
            (36, 2, 1),
            (55, 3, 1),
            (36, 4, 1),
            (53, 5, 0)
        ]
    );
}

#[test]
fn concat_flags_come_only_on_request() {
    // Every glyph of "office" sat in a lookup's view (the ligature
    // looks ahead, the kerning looks at every pair), but nothing moved
    // across a break, so only PRODUCE_UNSAFE_TO_CONCAT shows it.
    assert_eq!(
        flags(
            OPEN_SANS,
            "office",
            Direction::Ltr,
            MC,
            BufferFlags::DEFAULT
        ),
        [(82, 0, 0), (605, 1, 0), (70, 4, 0), (72, 5, 0)]
    );
    assert_eq!(
        flags(OPEN_SANS, "office", Direction::Ltr, MC, CONCAT),
        [(82, 0, 2), (605, 1, 2), (70, 4, 2), (72, 5, 2)]
    );
    // At the character level a ligature keeps the first component's
    // cluster.
    assert_eq!(
        flags(
            RUBIK,
            "ffi",
            Direction::Ltr,
            ClusterLevel::Characters,
            CONCAT
        ),
        [(262, 0, 2)]
    );
}

#[test]
fn arabic_joining_marks_joined_letters() {
    // arabic_joining: beh, beh, alef. The second beh changes the first
    // one's form, which makes the pair unsafe to break, or with
    // PRODUCE_SAFE_TO_INSERT_TATWEEL a place a tatweel may go.
    let text = "\u{0628}\u{0628}\u{0627}";
    assert_eq!(
        flags(AMIRI, text, Direction::Rtl, MC, BufferFlags::DEFAULT),
        [(1543, 4, 1), (2570, 2, 1), (3256, 0, 0)]
    );
    let tatweel = BufferFlags::PRODUCE_SAFE_TO_INSERT_TATWEEL;
    assert_eq!(
        flags(AMIRI, text, Direction::Rtl, MC, tatweel),
        [(1543, 4, 5), (2570, 2, 1), (3256, 0, 0)]
    );
    // Mongolian joins through the Universal Shaping Engine.
    let text = "\u{1828}\u{1823}\u{1829}\u{182D}";
    assert_eq!(
        flags(MONGOLIAN, text, Direction::Ltr, MC, BufferFlags::DEFAULT),
        [(72, 0, 0), (32, 3, 1), (80, 6, 1), (118, 9, 1)]
    );
}

#[test]
fn a_failed_lookahead_marks_what_it_examined() {
    // Amiri's kerning has a chained rule for hamza with two lookahead
    // glyphs: at the hamza it walks past the ZWNJ to the end of the
    // run and fails, so HarfBuzz marks the hamza through the end unsafe
    // to concatenate. The rule is the second subtable of its lookup,
    // which sigilbuzz used to skip once the first did not match.
    assert_eq!(
        flags(
            AMIRI,
            "\u{0627}\u{0621}\u{200C}",
            Direction::Rtl,
            MC,
            CONCAT
        ),
        [(1, 4, 2), (49, 2, 2), (55, 0, 2)]
    );
}

#[test]
fn syllables_are_unsafe_to_break_inside() {
    // The Indic and USE shapers mark each syllable unsafe to break
    // (setup_syllables), so a mark of its own cluster is marked.
    let text = "\u{0918}\u{0941}\u{0902}\u{091E}\u{091B}\u{0919}";
    assert_eq!(
        flags(DEVANAGARI, text, Direction::Ltr, MC, BufferFlags::DEFAULT),
        [
            (59, 0, 0),
            (34, 3, 1),
            (100, 6, 1),
            (65, 9, 0),
            (62, 12, 0),
            (60, 15, 0)
        ]
    );
    let text = "\u{0DB3}\u{0D83}\u{0DAB}";
    assert_eq!(
        flags(SINHALA, text, Direction::Ltr, MC, BufferFlags::DEFAULT),
        [(48, 0, 0), (5, 3, 1), (41, 6, 0)]
    );
}
