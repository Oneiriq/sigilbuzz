//! End-to-end OTF/TrueType embedded emission against Open Sans.
//!
//! The OTF emitter ships the input font bytes verbatim, builds a
//! 256-CID Identity-H map, and tabulates per-glyph widths in PDF
//! 1000-unit character space. None of that requires actually
//! parsing the outline. The test just confirms the wrapper data
//! structure is internally consistent.

use sigilbuzz::Face;
use sigilbuzz_pdf::{emit_otf_embedded_font, GlyphId};

const OPENSANS_BYTES: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

fn load_face() -> Face<'static> {
    Face::parse_bytes(OPENSANS_BYTES, 0).expect("Open Sans parses")
}

fn ascii_printable_gids(face: &Face<'_>) -> Vec<GlyphId> {
    let cmap = face.cmap().expect("cmap is present");
    let mut gids = Vec::new();
    for cp in 0x21u32..=0x7E {
        if let Some(gid) = cmap.glyph_id(char::from_u32(cp).unwrap()) {
            gids.push(gid);
        }
    }
    gids
}

#[test]
fn opensans_ascii_emits_consistent_otf_embedded_font() {
    let face = load_face();
    let gids = ascii_printable_gids(&face);
    assert!(!gids.is_empty());

    let font = emit_otf_embedded_font(&face, OPENSANS_BYTES, &gids);

    // Program is the input font bytes verbatim, no subsetting.
    assert_eq!(
        font.program.len(),
        OPENSANS_BYTES.len(),
        "program is the full unmodified font byte slice"
    );
    assert_eq!(font.program, OPENSANS_BYTES);

    // CIDToGIDMap is exactly 2 bytes x 256 entries.
    assert_eq!(font.cid_to_gid_map.len(), 2 * 256);

    // Each requested gid contributes one entry to the widths
    // array, in input order.
    assert_eq!(font.widths.len(), gids.len());
    for (i, &gid) in gids.iter().enumerate() {
        assert_eq!(font.widths[i].0, gid);
    }

    // Font dict body is well-formed Type 0 with Identity-H encoding.
    let s = std::str::from_utf8(&font.font_dict_body).unwrap();
    assert!(s.contains("/Subtype /Type0"));
    assert!(s.contains("/Encoding /Identity-H"));
    assert!(s.contains("/CIDToGIDMap"));
    // Open Sans is a TrueType (glyf) font, so the descriptor uses
    // /FontFile2.
    let dsc = std::str::from_utf8(&font.descriptor_body).unwrap();
    assert!(
        dsc.contains("/FontFile2"),
        "TrueType program must be referenced via /FontFile2"
    );
}

#[test]
fn cid_to_gid_map_is_identity_for_first_few_cids() {
    let face = load_face();
    let gids = ascii_printable_gids(&face);
    let font = emit_otf_embedded_font(&face, OPENSANS_BYTES, &gids);

    // CID 0 is reserved for /.notdef -> gid 0.
    assert_eq!(&font.cid_to_gid_map[0..2], &[0u8, 0u8]);

    // CIDs 1.. should map to the requested gids in order.
    for (i, &gid) in gids.iter().enumerate() {
        let cid = i + 1;
        if cid >= 256 {
            break;
        }
        let off = cid * 2;
        let entry = u16::from_be_bytes([font.cid_to_gid_map[off], font.cid_to_gid_map[off + 1]]);
        assert_eq!(entry, gid, "CID {cid} should map to gid {gid}");
    }
}

#[test]
fn widths_are_in_pdf_1000_unit_space() {
    let face = load_face();
    let gids = ascii_printable_gids(&face);
    let font = emit_otf_embedded_font(&face, OPENSANS_BYTES, &gids);

    let upem = face.head().unwrap().units_per_em as f32;
    let hmtx = face.hmtx().unwrap();
    for (gid, width) in &font.widths {
        let advance = hmtx.advance(*gid).unwrap_or(0) as f32;
        let expected = advance * 1000.0 / upem;
        // Allow for f32 rounding drift, but it should be exact for
        // these inputs.
        assert!(
            (width - expected).abs() < 0.001,
            "gid {gid} width {width} drifted from expected {expected}"
        );
    }
}

#[test]
fn opensans_emission_is_deterministic() {
    let face = load_face();
    let gids = ascii_printable_gids(&face);
    let a = emit_otf_embedded_font(&face, OPENSANS_BYTES, &gids);
    let b = emit_otf_embedded_font(&face, OPENSANS_BYTES, &gids);
    assert_eq!(a, b);
}
