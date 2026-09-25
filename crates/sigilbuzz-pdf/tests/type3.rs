//! End-to-end Type 3 emission against a vendored TTF.
//!
//! Loads Open Sans, picks the gids for the printable ASCII range
//! through the cmap, emits a `Type3Font`, and asserts the resulting
//! data structure is internally consistent: every CharProc body is
//! framed by `d1` / `f`, every gid has a matching encoding entry and
//! width, and the font's `FontBBox` envelopes every per-glyph bbox.
//!
//! The byte-length of one specific glyph's content stream is locked
//! down as a snapshot. Any future change to the emitter that
//! shifts that length silently is a regression we want to catch.

use sigilbuzz::Face;
use sigilbuzz_pdf::{emit_type3_font, Bbox, GlyphId};

const OPENSANS_BYTES: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

fn load_face() -> Face<'static> {
    Face::parse_bytes(OPENSANS_BYTES, 0).expect("Open Sans parses")
}

/// Walk the printable ASCII range and collect every gid the cmap
/// resolves. Codepoints with no glyph (in practice none, for ASCII
/// printables in a Latin font) are silently skipped.
fn ascii_printable_gids(face: &Face<'_>) -> Vec<GlyphId> {
    let cmap = face.cmap().expect("cmap is present");
    let mut gids = Vec::new();
    for cp in 0x21u32..=0x7E {
        let ch = char::from_u32(cp).unwrap();
        if let Some(gid) = cmap.glyph_id(ch) {
            gids.push(gid);
        }
    }
    gids
}

fn bbox_envelopes(outer: &Bbox, inner: &Bbox) -> bool {
    if inner.is_empty() {
        return true;
    }
    outer.xmin <= inner.xmin
        && outer.ymin <= inner.ymin
        && outer.xmax >= inner.xmax
        && outer.ymax >= inner.ymax
}

/// Format an `f32` exactly the way the emitter does: shortest
/// round-trip representation, trailing `.0` stripped.
fn fmt_num(v: f32) -> String {
    let s = format!("{v}");
    if let Some(stripped) = s.strip_suffix(".0") {
        stripped.to_string()
    } else {
        s
    }
}

#[test]
fn opensans_ascii_printable_emits_consistent_type3_font() {
    let face = load_face();
    let gids = ascii_printable_gids(&face);
    assert!(
        !gids.is_empty(),
        "ASCII printables should resolve in Open Sans"
    );

    let font = emit_type3_font(&face, &gids);

    // Field-length invariants.
    assert_eq!(font.char_procs.len(), font.encoding.len());
    assert_eq!(font.char_procs.len(), font.widths.len());
    assert_eq!(font.char_procs.len(), gids.len());

    // FontMatrix is the standard 1/upem identity scale.
    let upem = face.head().unwrap().units_per_em;
    let s = 1.0_f32 / f32::from(upem);
    assert_eq!(font.matrix, [s, 0.0, 0.0, s, 0.0, 0.0]);

    for (i, cp) in font.char_procs.iter().enumerate() {
        // Encoding char codes are sequential and start at 1.
        assert_eq!(font.encoding[i].0, (i + 1) as u8);
        assert_eq!(font.encoding[i].1, cp.name);

        // Width matches the parallel widths array.
        assert_eq!(font.widths[i], cp.width);

        // Body must start with the d1 prologue: "<width> 0 <l> <b>
        // <r> <t> d1\n". We cannot eyeball the exact byte string
        // (variable widths/bboxes), but we can assert the d1 token
        // lives at the right offset.
        let body = &cp.body;
        let prefix = format!(
            "{} 0 {} {} {} {} d1\n",
            fmt_num(cp.width),
            fmt_num(cp.bbox.xmin),
            fmt_num(cp.bbox.ymin),
            fmt_num(cp.bbox.xmax),
            fmt_num(cp.bbox.ymax),
        );
        assert!(
            body.starts_with(prefix.as_bytes()),
            "CharProc {i} ({}) body does not start with d1 prologue: {:?}",
            cp.name,
            std::str::from_utf8(body).unwrap_or("<non-utf8>")
        );

        // Body must end with the fill epilogue.
        assert!(
            body.ends_with(b"f\n"),
            "CharProc {i} ({}) body does not end with f\\n",
            cp.name
        );

        // Per-glyph bbox is enveloped by the font bbox.
        assert!(
            bbox_envelopes(&font.bbox, &cp.bbox),
            "FontBBox {:?} fails to envelope glyph bbox {:?}",
            font.bbox,
            cp.bbox
        );
    }
}

#[test]
fn opensans_emission_is_deterministic() {
    let face = load_face();
    let gids = ascii_printable_gids(&face);
    let a = emit_type3_font(&face, &gids);
    let b = emit_type3_font(&face, &gids);
    assert_eq!(a, b, "same face + gids must yield byte-identical output");
}

#[test]
fn opensans_capital_a_charproc_length_is_stable() {
    // Snapshot guard: locks down the byte length of the content
    // stream emitted for the gid behind 'A'. Any future change to
    // the emitter (number formatting, op sequence) that perturbs
    // this length should be a deliberate update of the constant.
    let face = load_face();
    let cmap = face.cmap().unwrap();
    let gid_a = cmap.glyph_id('A').expect("Open Sans has 'A'");

    let font = emit_type3_font(&face, &[gid_a]);
    assert_eq!(font.char_procs.len(), 1);

    let body_len = font.char_procs[0].body.len();
    assert_eq!(
        body_len, EXPECTED_OPENSANS_A_BODY_LEN,
        "Open Sans 'A' CharProc body length drifted (got {body_len}, \
         expected {EXPECTED_OPENSANS_A_BODY_LEN}). Update the constant \
         if the emitter change is intended."
    );
}

/// Captured against `opensans_regular.ttf` at the time this test
/// was written. Updating this constant should be a deliberate act
/// tied to an emitter change, not a silent regression.
const EXPECTED_OPENSANS_A_BODY_LEN: usize = 242;
