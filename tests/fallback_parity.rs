//! Fallback positioning parity with rustybuzz.
//!
//! Space characters a font does not map are drawn with the space glyph
//! and given the width of their kind, as HarfBuzz's
//! `_hb_ot_shape_fallback_spaces` does: an em fraction for the em
//! spaces, a digit's width for U+2007 FIGURE SPACE, a period's for
//! U+2008 PUNCTUATION SPACE, half a space for U+202F NARROW NO-BREAK
//! SPACE. And when nothing in the font positions marks, the marks get
//! HarfBuzz's fallback positions (below).

use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
const NOTO_DEVANAGARI: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Regular.ttf");
const RUBIK: &[u8] = include_bytes!("fixtures/rubik_vf.ttf");

/// `(glyph id, cluster, x advance, y advance, x offset, y offset)`.
type Row = (u32, u32, i32, i32, i32, i32);

fn sigilbuzz_rows(font_bytes: &[u8], text: &str, direction: Direction) -> Vec<Row> {
    let blob = Blob::new(font_bytes);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    shape(&font, &buffer, &[])
        .expect("shape")
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

fn rustybuzz_rows(font_bytes: &[u8], text: &str, direction: rustybuzz::Direction) -> Vec<Row> {
    let face = rustybuzz::Face::from_slice(font_bytes, 0).expect("parse face");
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    buffer.set_direction(direction);
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

fn assert_ltr_parity(font: &[u8], text: &str) {
    assert_eq!(
        sigilbuzz_rows(font, text, Direction::Ltr),
        rustybuzz_rows(font, text, rustybuzz::Direction::LeftToRight),
        "{text:?}"
    );
}

#[test]
fn missing_space_characters_take_their_fallback_widths() {
    // Open Sans maps U+2000..U+200A but not these three.
    assert_ltr_parity(OPEN_SANS, "a\u{202F}b\u{205F}c\u{3000}d");
    // Noto Sans Devanagari maps none of the special spaces, but has
    // digits and a period for the figure and punctuation spaces.
    for text in [
        "\u{0915}\u{2000}\u{0915}\u{2001}\u{0915}\u{2002}\u{0915}\u{2003}\u{0915}",
        "\u{0915}\u{2004}\u{0915}\u{2005}\u{0915}\u{2006}\u{0915}\u{2009}\u{0915}\u{200A}",
        "\u{0967}\u{2007}\u{0968}\u{2008}\u{0969}",
        "\u{0915}\u{202F}\u{0915}\u{205F}\u{0915}\u{3000}\u{0915}",
    ] {
        assert_ltr_parity(NOTO_DEVANAGARI, text);
    }
}

#[test]
fn a_fallback_space_draws_with_the_space_glyph() {
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).expect("parse face");
    let space = face.cmap().expect("cmap").glyph_id(' ').expect("space");
    let upem = i32::from(face.head().expect("head").units_per_em);
    let rows = sigilbuzz_rows(OPEN_SANS, "\u{3000}\u{205F}", Direction::Ltr);
    assert_eq!(rows[0].0, u32::from(space));
    // IDEOGRAPHIC SPACE is an em, MEDIUM MATHEMATICAL SPACE 4/18 em.
    assert_eq!(rows[0].2, upem);
    assert_eq!(rows[1].2, upem * 4 / 18);
}

#[test]
fn figure_and_punctuation_spaces_follow_the_font_at_default_coords() {
    // Rubik is variable; at the default instance the widths come from
    // hmtx like everything else.
    assert_ltr_parity(RUBIK, "1\u{2007}2\u{2008}3\u{2009}4");
}

// Fallback mark positioning.
//
// When nothing in the font positions marks, HarfBuzz's
// `_hb_ot_shape_fallback_mark_position` places each mark from its
// combining class and the ink extents of its base. The fonts below are
// the vendored ones with their GPOS table removed, so rustybuzz takes
// the same path. Clusters are not compared here: HarfBuzz merges each
// base and its marks into one grapheme cluster, which sigilbuzz does
// not do yet.

const NOTO_HEBREW: &[u8] = include_bytes!("fonts/NotoSansHebrew-Regular.ttf");
const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");

/// `font` rebuilt without its `drop` table.
fn without_table(font: &[u8], drop: [u8; 4]) -> Vec<u8> {
    let be16 = |at: usize| u16::from_be_bytes([font[at], font[at + 1]]);
    let be32 = |at: usize| u32::from_be_bytes([font[at], font[at + 1], font[at + 2], font[at + 3]]);
    let records: Vec<([u8; 4], u32, usize, usize)> = (0..usize::from(be16(4)))
        .map(|i| {
            let at = 12 + 16 * i;
            let tag = [font[at], font[at + 1], font[at + 2], font[at + 3]];
            (
                tag,
                be32(at + 4),
                be32(at + 8) as usize,
                be32(at + 12) as usize,
            )
        })
        .filter(|r| r.0 != drop)
        .collect();
    assert!(records.len() < usize::from(be16(4)), "no {drop:?} table");
    let count = records.len() as u16;
    let selector = 15 - count.leading_zeros() as u16;
    let range = (1u16 << selector) * 16;
    let mut out = font[..4].to_vec();
    for v in [count, range, selector, count * 16 - range] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    let mut data = Vec::new();
    let data_start = 12 + 16 * records.len();
    for (tag, checksum, offset, length) in &records {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum.to_be_bytes());
        out.extend_from_slice(&((data_start + data.len()) as u32).to_be_bytes());
        out.extend_from_slice(&(*length as u32).to_be_bytes());
        data.extend_from_slice(&font[*offset..offset + length]);
        while data.len() % 4 != 0 {
            data.push(0);
        }
    }
    out.extend_from_slice(&data);
    out
}

/// `(glyph id, x advance, y advance, x offset, y offset)` per glyph.
fn positions(rows: &[Row]) -> Vec<(u32, i32, i32, i32, i32)> {
    rows.iter().map(|r| (r.0, r.2, r.3, r.4, r.5)).collect()
}

fn assert_position_parity(font: &[u8], text: &str, rtl: bool) {
    let (dir, rb_dir) = if rtl {
        (Direction::Rtl, rustybuzz::Direction::RightToLeft)
    } else {
        (Direction::Ltr, rustybuzz::Direction::LeftToRight)
    };
    let sig = positions(&sigilbuzz_rows(font, text, dir));
    let rb = positions(&rustybuzz_rows(font, text, rb_dir));
    assert_eq!(sig, rb, "{text:?}");
}

#[test]
fn latin_marks_without_gpos_match_rustybuzz() {
    let open_sans = without_table(OPEN_SANS, *b"GPOS");
    for text in [
        "q\u{0300}\u{0301}",
        "b\u{0323}",
        "x\u{0309}\u{0323}",
        "\u{0131}\u{0303}m",
        "A\u{030F}B",
    ] {
        assert_position_parity(&open_sans, text, false);
    }
    // The dot below sits under the b, centered on its advance, with no
    // advance of its own.
    let rows = sigilbuzz_rows(&open_sans, "b\u{0323}", Direction::Ltr);
    let (base, mark) = (rows[0], rows[1]);
    assert_eq!(mark.2, 0);
    assert!(mark.5 < 0, "{rows:?}");
    assert!(mark.4 < 0 && mark.4 > -base.2, "{rows:?}");

    let rubik = without_table(RUBIK, *b"GPOS");
    for text in [
        // Attached below, overlay, and stacked above.
        "c\u{0327}k\u{0327}",
        "a\u{0338}b",
        "q\u{0302}\u{0301}\u{0308}",
        "p\u{0326}\u{0328}",
    ] {
        assert_position_parity(&rubik, text, false);
    }
}

#[test]
fn hebrew_points_without_gpos_match_rustybuzz() {
    let noto = without_table(NOTO_HEBREW, *b"GPOS");
    for text in [
        "\u{05D1}\u{05BC}\u{05B0}\u{05E8}\u{05B5}\u{05D0}\u{05E9}\u{05B4}\u{05D9}\u{05EA}",
        "\u{05E9}\u{05C1}\u{05B8}\u{05DC}\u{05D5}\u{05B9}\u{05DD}",
        "\u{05D0}\u{05B7}\u{05B0}\u{05BD}",
        "\u{05DB}\u{05BF}\u{05E9}\u{05C2}",
    ] {
        assert_position_parity(&noto, text, true);
    }
}

#[test]
fn arabic_marks_without_gpos_match_rustybuzz() {
    let amiri = without_table(AMIRI, *b"GPOS");
    for text in [
        "\u{0628}\u{064E}",
        "\u{0628}\u{0651}\u{064E}\u{0645}\u{0650}",
        // Kasra and hamza below: the modifier mark moves first and both
        // stack below.
        "\u{0628}\u{0650}\u{0655}",
        // Lam-alef ligature with a mark on each component.
        "\u{0644}\u{064E}\u{0627}\u{064B}",
    ] {
        assert_position_parity(&amiri, text, true);
    }
}

/// `font` with GPOS script `from` renamed to `to` (same sort position).
fn rename_gpos_script(font: &[u8], from: [u8; 4], to: [u8; 4]) -> Vec<u8> {
    let be16 = |at: usize| usize::from(u16::from_be_bytes([font[at], font[at + 1]]));
    let be32 = |at: usize| {
        u32::from_be_bytes([font[at], font[at + 1], font[at + 2], font[at + 3]]) as usize
    };
    let gpos = (0..be16(4))
        .map(|i| 12 + 16 * i)
        .find(|&at| font[at..at + 4] == *b"GPOS")
        .map(|at| be32(at + 8))
        .expect("GPOS");
    let scripts = gpos + be16(gpos + 4);
    let mut out = font.to_vec();
    for i in 0..be16(scripts) {
        let at = scripts + 2 + 6 * i;
        if out[at..at + 4] == from {
            out[at..at + 4].copy_from_slice(&to);
            return out;
        }
    }
    panic!("no {from:?} script");
}

#[test]
fn cff_fonts_position_marks_from_charstring_extents() {
    // Source Code Pro's subset has no GPOS and no combining marks, so
    // the acute draws as .notdef, still a mark by its character.
    // HarfBuzz takes CFF extents from the charstrings (rustybuzz 0.20
    // has no CFF extents and only zeroes the mark), so the mark lands
    // above the a, centered on its advance.
    const SOURCE_CODE: &[u8] = include_bytes!("fonts/SourceCodePro-Latin-Subset.otf");
    let rows = sigilbuzz_rows(SOURCE_CODE, "a\u{0301}", Direction::Ltr);
    let (base, mark) = (rows[0], rows[1]);
    assert_eq!(mark.0, 0);
    assert_eq!(mark.2, 0);
    assert!(mark.5 > 0, "{rows:?}");
    assert!(mark.4 < 0 && mark.4 > -base.2, "{rows:?}");
}

#[test]
fn hebrew_ignores_gpos_without_a_hebr_script() {
    // HarfBuzz's Hebrew shaper only applies GPOS when its ScriptList
    // has `hebr`; without it the marks fall back.
    let rubik = rename_gpos_script(RUBIK, *b"hebr", *b"hebq");
    let text = "\u{05E9}\u{05C1}\u{05B8}\u{05DC}\u{05D5}\u{05B9}\u{05DD}";
    assert_position_parity(&rubik, text, true);
    assert_ne!(
        positions(&sigilbuzz_rows(&rubik, text, Direction::Rtl)),
        positions(&sigilbuzz_rows(RUBIK, text, Direction::Rtl)),
    );
}
