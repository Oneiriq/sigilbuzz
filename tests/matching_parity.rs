//! Parity contract for HarfBuzz's lookup matching rules: sigilbuzz
//! matches rustybuzz glyph for glyph, advance for advance and offset
//! for offset when
//!
//! - default-ignorable characters (ZWJ, ZWNJ, ZWSP, soft hyphen) sit
//!   inside a ligature, a contextual rule, or a kerning pair: the
//!   skipping iterator passes over them unless the rule names them or
//!   the feature handles joiners itself, and ZWNJ still breaks a
//!   ligature;
//! - the font has no GDEF: glyph classes are synthesized from Unicode
//!   general categories, so lookups that ignore marks skip nonspacing
//!   marks, marks lose their advance, and mark attachment finds its
//!   base.
//!
//! Clusters are compared where the two engines' cluster models agree
//! (ligatures); a mark or joiner that HarfBuzz folds into its base's
//! grapheme cluster keeps its own cluster in sigilbuzz, which is a
//! separate buffer-model difference.

use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
const RUBIK: &[u8] = include_bytes!("fixtures/rubik_vf.ttf");
const SOURCE_SANS: &[u8] = include_bytes!("fonts/SourceSans3VF-Latin-Subset.otf");
const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");
const HEBREW: &[u8] = include_bytes!("fonts/NotoSansHebrew-Regular.ttf");
const THAI: &[u8] = include_bytes!("fonts/NotoSansThai-Regular.ttf");

/// One glyph: id, cluster, x/y advance, x/y offset.
type Out = (u32, u32, i32, i32, i32, i32);

fn sigil(bytes: &[u8], text: &str, rtl: bool) -> Vec<Out> {
    let blob = Blob::new(bytes);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    if rtl {
        buffer.set_direction(Direction::Rtl);
    }
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

fn rusty(bytes: &[u8], text: &str, rtl: bool) -> Vec<Out> {
    let face = rustybuzz::Face::from_slice(bytes, 0).expect("parse rustybuzz face");
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    buffer.set_direction(if rtl {
        rustybuzz::Direction::RightToLeft
    } else {
        rustybuzz::Direction::LeftToRight
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

/// Everything but the cluster.
fn positions(run: &[Out]) -> Vec<(u32, i32, i32, i32, i32)> {
    run.iter().map(|g| (g.0, g.2, g.3, g.4, g.5)).collect()
}

/// Asserts positional parity for every string, and cluster parity
/// too when `clusters` is set.
fn assert_parity(label: &str, bytes: &[u8], strings: &[&str], rtl: bool, clusters: bool) {
    for text in strings {
        let (s, r) = (sigil(bytes, text, rtl), rusty(bytes, text, rtl));
        assert_eq!(positions(&s), positions(&r), "{label} {text:?}");
        if clusters {
            assert_eq!(s, r, "{label} {text:?} clusters");
        }
    }
}

/// `bytes` with its GDEF table hidden: the table record's tag becomes
/// `GDEE`, which keeps the table directory sorted.
fn without_gdef(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let count = usize::from(u16::from_be_bytes([out[4], out[5]]));
    for record in 0..count {
        let at = 12 + record * 16;
        if &out[at..at + 4] == b"GDEF" {
            out[at..at + 4].copy_from_slice(b"GDEE");
        }
    }
    out
}

#[test]
fn ligatures_form_across_default_ignorables_but_not_zwnj() {
    // f ZWJ i, f SHY i and f ZWSP i ligate (the ignorable ends up
    // after the ligature, hidden); f ZWNJ i does not.
    let strings = [
        "fi",
        "f\u{200D}i",
        "f\u{00AD}i",
        "f\u{200B}i",
        "f\u{200C}i",
        "ff\u{00AD}i",
        "f\u{200D}fi",
    ];
    assert_parity("Open Sans", OPEN_SANS, &strings, false, true);
    assert_parity("Rubik", RUBIK, &strings, false, true);
}

#[test]
fn arabic_contexts_and_ligatures_see_through_zwsp_and_soft_hyphen() {
    // Lam-alef and the joining contexts of Amiri's rlig/calt rules
    // span a ZWSP or a soft hyphen; the Arabic shaper's ligating
    // features handle ZWJ manually, so lam ZWJ alef stays apart.
    let strings = [
        "\u{0644}\u{200B}\u{0627}",
        "\u{0644}\u{00AD}\u{0627}",
        "\u{0628}\u{00AD}\u{0633}\u{0645}",
        "\u{0628}\u{200B}\u{0633}",
        "\u{0644}\u{200D}\u{0627}",
        "\u{0627}\u{0644}\u{0644}\u{200D}\u{0647}",
    ];
    assert_parity("Amiri", AMIRI, &strings, true, false);
}

#[test]
fn kerning_pairs_span_joiners_and_other_ignorables() {
    let strings = [
        "AV",
        "A\u{200D}V",
        "A\u{200C}V",
        "A\u{200B}V",
        "A\u{00AD}V",
        "T\u{200D}o",
    ];
    // GPOS PairPos (Rubik, Source Sans) and the legacy kern table
    // (Open Sans).
    assert_parity("Rubik", RUBIK, &strings, false, false);
    assert_parity("Source Sans", SOURCE_SANS, &strings, false, false);
    assert_parity("Open Sans", OPEN_SANS, &strings, false, false);
}

#[test]
fn fonts_without_gdef_get_synthesized_glyph_classes() {
    // Mark-to-base and mark-to-mark attachment, a kerning pair and a
    // ligature whose lookups ignore marks, all across nonspacing marks
    // that only the synthesized classes know about.
    let latin = [
        "x\u{0301}",
        "T\u{0301}o",
        "V\u{0323}A",
        "f\u{0301}i",
        "q\u{0323}\u{0301}",
    ];
    assert_parity("Open Sans", &without_gdef(OPEN_SANS), &latin, false, false);
    assert_parity("Rubik", &without_gdef(RUBIK), &latin, false, false);
    let arabic = [
        "\u{0628}\u{064E}",
        "\u{0644}\u{064E}\u{0627}",
        "\u{0628}\u{0651}\u{064E}",
        "\u{0627}\u{0644}\u{0644}\u{0651}\u{064E}\u{0647}",
        "\u{0633}\u{0652}\u{0645}",
    ];
    assert_parity("Amiri", &without_gdef(AMIRI), &arabic, true, false);
    let hebrew = ["\u{05D1}\u{05BC}", "\u{05E9}\u{05C1}\u{05B8}"];
    assert_parity(
        "Noto Sans Hebrew",
        &without_gdef(HEBREW),
        &hebrew,
        true,
        false,
    );
    let thai = ["\u{0E01}\u{0E48}", "\u{0E01}\u{0E34}\u{0E48}"];
    assert_parity("Noto Sans Thai", &without_gdef(THAI), &thai, false, false);
}

#[test]
fn a_stripped_gdef_is_really_gone() {
    let bytes = without_gdef(OPEN_SANS);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).expect("parse face");
    assert!(face.gdef().expect("read GDEF").is_none());
    assert!(rustybuzz::Face::from_slice(&bytes, 0).is_some());
}
