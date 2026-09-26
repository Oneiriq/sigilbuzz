//! Buffer flag parity: `PRESERVE_DEFAULT_IGNORABLES`,
//! `REMOVE_DEFAULT_IGNORABLES`, and `EOT` change the output exactly as
//! the same HarfBuzz flags do (dotted-circle flags are covered in
//! `dotted_circle_parity.rs`).
//!
//! - PRESERVE: default ignorables keep the font's glyph and advance;
//!   nothing is zeroed or swapped for the space glyph.
//! - REMOVE: they are deleted, their clusters merged into a neighbor
//!   (backward at every cluster level, forward only at the monotone
//!   ones), even when the font has a space glyph. PRESERVE wins when
//!   both are set.
//! - EOT: HarfBuzz's OpenType shaper reads no end-of-text state, so
//!   the output is the default one.
//!
//! Glyph ids, clusters, advances, and offsets are compared with
//! rustybuzz 0.20 at each cluster level it has.

use rustybuzz::{BufferClusterLevel, Direction as RbDirection};
use sigilbuzz::{shape, Blob, Buffer, BufferFlags, ClusterLevel, Direction, Face, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");
const DEVANAGARI: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Regular.ttf");

type Row = (u32, u32, i32, i32, i32, i32);

const LEVELS: [(ClusterLevel, BufferClusterLevel); 3] = [
    (
        ClusterLevel::MonotoneGraphemes,
        BufferClusterLevel::MonotoneGraphemes,
    ),
    (
        ClusterLevel::MonotoneCharacters,
        BufferClusterLevel::MonotoneCharacters,
    ),
    (ClusterLevel::Characters, BufferClusterLevel::Characters),
];

fn sigilbuzz_rows(
    data: &[u8],
    text: &str,
    direction: Direction,
    flags: BufferFlags,
    level: ClusterLevel,
) -> Vec<Row> {
    let blob = Blob::new(data);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    buffer.set_flags(flags);
    buffer.set_cluster_level(level);
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

fn rustybuzz_rows(
    data: &[u8],
    text: &str,
    direction: Direction,
    flags: BufferFlags,
    level: BufferClusterLevel,
) -> Vec<Row> {
    let face = rustybuzz::Face::from_slice(data, 0).expect("parse rustybuzz face");
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_direction(match direction {
        Direction::Ltr => RbDirection::LeftToRight,
        Direction::Rtl => RbDirection::RightToLeft,
        Direction::Ttb => RbDirection::TopToBottom,
        Direction::Btt => RbDirection::BottomToTop,
    });
    buffer.set_flags(rustybuzz::BufferFlags::from_bits_truncate(flags.bits()));
    buffer.set_cluster_level(level);
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

fn assert_parity(data: &[u8], direction: Direction, flags: BufferFlags, texts: &[&str]) {
    let mut failures = Vec::new();
    for &text in texts {
        for (ours, theirs) in LEVELS {
            let a = sigilbuzz_rows(data, text, direction, flags, ours);
            let b = rustybuzz_rows(data, text, direction, flags, theirs);
            if a != b {
                failures.push(format!(
                    "{text:?} {flags:?} {ours:?}\n  sigilbuzz: {a:?}\n  rustybuzz: {b:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

const LATIN: &[&str] = &[
    "a\u{00AD}b",
    "a\u{200B}b",
    "a\u{200D}b",
    "a\u{2060}b\u{2066}c",
    "a\u{FE0F}b",
    "\u{FEFF}ab",
    "\u{200B}a",
    "ab\u{200B}",
    "\u{200B}\u{200B}a",
    "a\u{E0001}\u{E0041}b",
];

const ARABIC: &[&str] = &[
    "\u{0628}\u{200D}\u{0628}",
    "\u{0628}\u{200C}\u{0628}",
    "\u{0627}\u{00AD}\u{0627}",
    "\u{200D}\u{0628}",
    "\u{0628}\u{200E}",
];

#[test]
fn preserve_keeps_default_ignorables_visible() {
    let flags = BufferFlags::PRESERVE_DEFAULT_IGNORABLES;
    assert_parity(OPEN_SANS, Direction::Ltr, flags, LATIN);
    assert_parity(AMIRI, Direction::Rtl, flags, ARABIC);
    assert_parity(
        DEVANAGARI,
        Direction::Ltr,
        flags,
        &["\u{0915}\u{00AD}\u{0916}"],
    );
    // The soft hyphen keeps an advance instead of hiding.
    let rows = sigilbuzz_rows(
        OPEN_SANS,
        "a\u{00AD}b",
        Direction::Ltr,
        flags,
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!(rows.len(), 3);
    assert!(rows[1].2 > 0, "{rows:?}");
}

#[test]
fn remove_deletes_default_ignorables_and_merges_their_clusters() {
    let flags = BufferFlags::REMOVE_DEFAULT_IGNORABLES;
    assert_parity(OPEN_SANS, Direction::Ltr, flags, LATIN);
    assert_parity(AMIRI, Direction::Rtl, flags, ARABIC);
    let rows = sigilbuzz_rows(
        OPEN_SANS,
        "a\u{200B}b",
        Direction::Ltr,
        flags,
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!(rows.len(), 2);
    // A leading deleted cluster merges forward only at the monotone
    // levels.
    let clusters = |level| -> Vec<u32> {
        sigilbuzz_rows(OPEN_SANS, "\u{200B}a", Direction::Ltr, flags, level)
            .iter()
            .map(|r| r.1)
            .collect()
    };
    assert_eq!(clusters(ClusterLevel::MonotoneCharacters), [0]);
    assert_eq!(clusters(ClusterLevel::Characters), [3]);
    assert_eq!(clusters(ClusterLevel::Graphemes), [3]);
}

#[test]
fn preserve_wins_over_remove() {
    let both = BufferFlags::PRESERVE_DEFAULT_IGNORABLES | BufferFlags::REMOVE_DEFAULT_IGNORABLES;
    assert_parity(OPEN_SANS, Direction::Ltr, both, LATIN);
    for text in LATIN {
        assert_eq!(
            sigilbuzz_rows(
                OPEN_SANS,
                text,
                Direction::Ltr,
                both,
                ClusterLevel::MonotoneCharacters
            ),
            sigilbuzz_rows(
                OPEN_SANS,
                text,
                Direction::Ltr,
                BufferFlags::PRESERVE_DEFAULT_IGNORABLES,
                ClusterLevel::MonotoneCharacters
            ),
            "{text:?}"
        );
    }
}

#[test]
fn eot_changes_nothing() {
    assert_parity(OPEN_SANS, Direction::Ltr, BufferFlags::EOT, LATIN);
    for text in LATIN.iter().chain(ARABIC) {
        let data = if text.chars().any(|c| c > '\u{05FF}' && c < '\u{0700}') {
            AMIRI
        } else {
            OPEN_SANS
        };
        for flags in [BufferFlags::EOT, BufferFlags::BOT | BufferFlags::EOT] {
            assert_eq!(
                sigilbuzz_rows(data, text, Direction::Ltr, flags, ClusterLevel::Characters),
                sigilbuzz_rows(
                    data,
                    text,
                    Direction::Ltr,
                    flags - BufferFlags::EOT,
                    ClusterLevel::Characters
                ),
                "{text:?} {flags:?}"
            );
        }
    }
}
