//! End-to-end Type 1 emission against a vendored TTF.
//!
//! The Type 1 emitter doesn't actually need a PostScript-format font
//! to ingest from — it converts sigilbuzz [`PathOp`]s, regardless of
//! whether they came from a `glyf` outline (Open Sans) or a CFF
//! charstring (any modern OTF). This test exercises the OpenSans
//! `glyf` path because that's the fixture the Type 3 test already
//! uses; the byte-for-byte conversion happens at the PathOp boundary,
//! not at the source-font level.
//!
//! [`PathOp`]: sigilbuzz::tables::PathOp

use sigilbuzz::Face;
use sigilbuzz_pdf::{emit_type1_font, GlyphId};

const OPENSANS_BYTES: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

fn load_face() -> Face<'static> {
    Face::parse_bytes(OPENSANS_BYTES, 0).expect("Open Sans parses")
}

#[test]
fn opensans_capital_a_emits_well_formed_type1_charstring() {
    let face = load_face();
    let cmap = face.cmap().unwrap();
    let gid_a: GlyphId = cmap.glyph_id('A').expect("Open Sans has 'A'");

    let font = emit_type1_font(&face, &[gid_a]).expect("Type 1 emission succeeds");

    // CharStrings body must contain a /gN entry where N is the gid
    // for 'A'. The body is a mix of ASCII PostScript (the dict
    // framing and per-entry headers) and *binary* charstring bytes,
    // so we search by raw byte slice rather than UTF-8 tokens.
    let needle = format!("/g{gid_a} ");
    let body: &[u8] = &font.char_strings_body;
    let needle_bytes = needle.as_bytes();
    let start = body
        .windows(needle_bytes.len())
        .position(|w| w == needle_bytes)
        .unwrap_or_else(|| panic!("CharStrings body missing /g{gid_a} entry"));
    let after_name = &body[start + needle.len()..];
    // Skip the ASCII length token + " -| ".
    let body_start = after_name
        .iter()
        .position(|&b| b == b'-')
        .expect("RD prefix present")
        + 3; // skip "-| "
    let cs = &after_name[body_start..];
    assert!(
        !cs.is_empty(),
        "expected a non-empty charstring for /g{gid_a}"
    );
    // hsbw is op 13. The first encoded operand is lsb (0 → byte 139),
    // followed by the advance number (1, 2, or 5 bytes depending on
    // its magnitude), followed by op 13.
    assert_eq!(
        cs[0], 139,
        "first hsbw operand should be 0 (encoded as 139)"
    );

    // op 13 must appear within the first 7 bytes — that's the worst
    // case (lsb single byte + advance 5-byte form + op = 7).
    let hsbw_pos = cs[..7]
        .iter()
        .position(|&b| b == 13)
        .expect("hsbw op (13) present near start of charstring");
    assert!(
        (2..=6).contains(&hsbw_pos),
        "hsbw op should appear after lsb+advance encoding (got pos {hsbw_pos})"
    );

    // Charstring must end with endchar (op 14) — after stripping the
    // closing " |-\n" framing.
    // Find the trailing " |-\n" after the charstring, then confirm
    // the byte directly before it is 14.
    let frame_pos = cs.windows(3).position(|w| w == b" |-").expect("|- frame");
    assert_eq!(cs[frame_pos - 1], 14, "charstring must end with endchar");
}

#[test]
fn opensans_emission_is_deterministic() {
    let face = load_face();
    let cmap = face.cmap().unwrap();
    let mut gids = Vec::new();
    for cp in 0x21u32..=0x7E {
        if let Some(gid) = cmap.glyph_id(char::from_u32(cp).unwrap()) {
            gids.push(gid);
        }
    }

    let a = emit_type1_font(&face, &gids).unwrap();
    let b = emit_type1_font(&face, &gids).unwrap();
    assert_eq!(a, b, "same face + gids must yield byte-identical output");
}

#[test]
fn font_dict_body_advertises_type_1() {
    let face = load_face();
    let font = emit_type1_font(&face, &[]).expect("emission succeeds for empty gid list");
    let s = std::str::from_utf8(&font.font_dict_body).unwrap();
    assert!(s.contains("/FontType 1 def"));
    assert!(s.contains("/FontMatrix"));
    assert!(s.contains("/FontBBox"));
}

#[test]
fn private_dict_advertises_cleartext_charstrings() {
    let face = load_face();
    let font = emit_type1_font(&face, &[]).unwrap();
    let s = std::str::from_utf8(&font.private_dict_body).unwrap();
    // /lenIV -1 = "the charstrings that follow are cleartext, not
    // eexec-encrypted." Adobe Reader and modern consumers honour
    // this; see the type1 module docs.
    assert!(
        s.contains("/lenIV -1 def"),
        "private dict must declare /lenIV -1 (cleartext charstrings)"
    );
}
