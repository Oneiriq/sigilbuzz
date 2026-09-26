//! Pre- and post-context reach cursive joining, cross-checked against
//! rustybuzz with the same context set.
//!
//! A buffer that holds one piece of a longer paragraph (a line split
//! mid-word, or a run cut at a style change) still needs the joining
//! forms the letters have in the full text. HarfBuzz reads the
//! characters around the run from the buffer context; the context is
//! never shaped and produces no glyphs.

use rustybuzz::Direction as RbDirection;
use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");
const NKO: &[u8] = include_bytes!("fonts/NotoSansNKo-Regular.ttf");
const MONGOLIAN: &[u8] = include_bytes!("fonts/NotoSansMongolian-Regular.ttf");

const BEH: &str = "\u{0628}";
const ALEF: &str = "\u{0627}";

struct Shaped {
    ids: Vec<u32>,
    clusters: Vec<u32>,
}

fn sigil(data: &[u8], text: &str, pre: &str, post: &str, direction: Direction) -> Shaped {
    let blob = Blob::new(data);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.set_pre_context(pre);
    buffer.push_str(text);
    buffer.set_post_context(post);
    buffer.set_direction(direction);
    let run = shape(&font, &buffer, &[]).expect("shape");
    Shaped {
        ids: run.glyphs.iter().map(|g| g.glyph_id).collect(),
        clusters: run.glyphs.iter().map(|g| g.cluster).collect(),
    }
}

/// rustybuzz glyph ids in logical order.
fn rusty(data: &[u8], text: &str, pre: &str, post: &str, direction: RbDirection) -> Vec<u32> {
    let face = rustybuzz::Face::from_slice(data, 0).expect("parse rustybuzz face");
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.set_pre_context(pre);
    buffer.push_str(text);
    buffer.set_post_context(post);
    buffer.set_direction(direction);
    buffer.guess_segment_properties();
    let out = rustybuzz::shape(&face, &[], buffer);
    let mut ids: Vec<u32> = out.glyph_infos().iter().map(|g| g.glyph_id).collect();
    if direction == RbDirection::RightToLeft {
        ids.reverse();
    }
    ids
}

/// Shapes with both engines and asserts they agree. sigilbuzz may
/// emit right-to-left runs in logical or visual order, so either is
/// accepted there.
fn assert_matches(
    data: &[u8],
    (pre, text, post): (&str, &str, &str),
    sig_dir: Direction,
    rb_dir: RbDirection,
) -> Vec<u32> {
    let sig = sigil(data, text, pre, post, sig_dir).ids;
    let rb = rusty(data, text, pre, post, rb_dir);
    let reversed: Vec<u32> = rb.iter().rev().copied().collect();
    assert!(
        sig == rb || (sig_dir == Direction::Rtl && sig == reversed),
        "pre {pre:?} text {text:?} post {post:?}: sigilbuzz={sig:?} rustybuzz={rb:?}"
    );
    sig
}

fn arabic(case: (&str, &str, &str)) -> Vec<u32> {
    assert_matches(AMIRI, case, Direction::Rtl, RbDirection::RightToLeft)
}

fn arabic_plain(text: &str) -> Vec<u32> {
    sigil(AMIRI, text, "", "", Direction::Rtl).ids
}

#[test]
fn arabic_pre_context_selects_final_and_medial_forms() {
    let fina = arabic((BEH, BEH, ""));
    assert_ne!(
        fina,
        arabic_plain(BEH),
        "beh after beh should not be isolated"
    );
    let medi = arabic((BEH, BEH, BEH));
    assert_ne!(medi, fina);
    arabic((BEH, ALEF, ""));
    arabic((BEH, "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}", ""));
    arabic((BEH, BEH, ALEF));
}

#[test]
fn arabic_post_context_selects_initial_form() {
    let init = arabic(("", BEH, BEH));
    assert_ne!(init, arabic_plain(BEH));
    arabic(("", "\u{0628}\u{0628}", ALEF));
    // ZWJ after the run is join-causing.
    arabic(("", BEH, "\u{200D}"));
}

#[test]
fn arabic_non_joining_context_changes_nothing() {
    for case in [
        (ALEF, BEH, ""),
        ("abc", BEH, "xyz"),
        ("", ALEF, BEH),
        (" ", BEH, " "),
    ] {
        assert_eq!(arabic(case), arabic_plain(case.1), "{case:?}");
    }
}

#[test]
fn arabic_context_skips_transparent_marks() {
    let with_marks = arabic(("\u{0628}\u{064E}\u{0651}", BEH, "\u{064E}\u{0628}"));
    assert_eq!(with_marks, arabic((BEH, BEH, BEH)));
}

#[test]
fn only_five_context_characters_count() {
    // The joiner is the fifth character from the end of the
    // pre-context, so it is kept...
    let kept = arabic(("abcd\u{0628}\u{064E}\u{064E}\u{064E}\u{064E}", BEH, ""));
    assert_ne!(kept, arabic_plain(BEH));
    // ...but one mark further back it falls outside the window.
    let dropped = arabic(("\u{0628}\u{064E}\u{064E}\u{064E}\u{064E}\u{064E}", BEH, ""));
    assert_eq!(dropped, arabic_plain(BEH));
}

#[test]
fn context_produces_no_glyphs_and_keeps_clusters() {
    let shaped = sigil(
        AMIRI,
        "\u{0628}\u{0628}",
        "\u{0628}\u{0628}",
        "\u{0628}",
        Direction::Rtl,
    );
    assert_eq!(shaped.ids.len(), 2);
    let mut clusters = shaped.clusters;
    clusters.sort_unstable();
    assert_eq!(clusters, [0, 2]);
}

#[test]
fn nko_context_matches_rustybuzz() {
    let a = "\u{07CA}";
    let plain = sigil(NKO, a, "", "", Direction::Rtl).ids;
    for case in [
        (a, a, ""),
        ("", a, a),
        (a, a, a),
        (a, "\u{07CA}\u{07CA}", ""),
    ] {
        let got = assert_matches(NKO, case, Direction::Rtl, RbDirection::RightToLeft);
        assert_ne!(got, plain, "{case:?}");
    }
}

#[test]
fn mongolian_context_matches_rustybuzz() {
    // sigilbuzz shapes Mongolian vertically unless told otherwise, so
    // pick horizontal RTL, as the Mongolian parity tests do, against
    // rustybuzz's horizontal LTR.
    let a = "\u{1820}";
    let plain = sigil(MONGOLIAN, a, "", "", Direction::Rtl).ids;
    for case in [
        (a, a, ""),
        ("", a, a),
        (a, a, a),
        ("\u{1820}\u{180B}", a, ""),
    ] {
        let got = assert_matches(MONGOLIAN, case, Direction::Rtl, RbDirection::LeftToRight);
        assert_ne!(got, plain, "{case:?}");
    }
}
