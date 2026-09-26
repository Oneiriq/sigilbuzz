//! Robustness tests for the PDF emitters: out-of-range glyph ids,
//! long gid lists, extreme coordinates, and a face whose `head` table
//! reports zero units per em.

use sigilbuzz::tables::PathOp;
use sigilbuzz::Face;
use sigilbuzz_pdf::{
    emit_otf_embedded_font, emit_path_ops, emit_type1_font, emit_type3_font, EmitError, GlyphId,
};

const OPENSANS_BYTES: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

/// Copy of `base` with `head.unitsPerEm` overwritten.
fn with_units_per_em(base: &[u8], upem: u16) -> Vec<u8> {
    let mut bytes = base.to_vec();
    let num_tables = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
    let head = bytes[12..12 + num_tables * 16]
        .chunks_exact(16)
        .find(|rec| &rec[..4] == b"head")
        .map(|rec| u32::from_be_bytes([rec[8], rec[9], rec[10], rec[11]]) as usize)
        .expect("head table present");
    bytes[head + 18..head + 20].copy_from_slice(&upem.to_be_bytes());
    bytes
}

#[test]
fn out_of_range_gids_do_not_panic() {
    let face = Face::parse_bytes(OPENSANS_BYTES, 0).expect("Open Sans parses");
    let gids: [GlyphId; 3] = [0, 60_000, u16::MAX];

    let t3 = emit_type3_font(&face, &gids);
    assert_eq!(t3.char_procs.len(), 3);
    assert_eq!(t3.widths[2], 0.0);

    let t1 = emit_type1_font(&face, &gids).expect("Type 1 emission succeeds");
    assert!(t1.char_strings_body.ends_with(b"end\n"));

    let otf = emit_otf_embedded_font(&face, OPENSANS_BYTES, &gids);
    assert_eq!(otf.widths.len(), 3);
    assert_eq!(otf.widths[2].1, 0.0);
}

#[test]
fn long_gid_list_fills_only_the_256_cid_map() {
    let face = Face::parse_bytes(OPENSANS_BYTES, 0).expect("Open Sans parses");
    let gids: Vec<GlyphId> = (0..300).collect();

    let otf = emit_otf_embedded_font(&face, OPENSANS_BYTES, &gids);
    assert_eq!(otf.cid_to_gid_map.len(), 512);
    assert_eq!(&otf.cid_to_gid_map[0..2], &[0, 0]);
    // CID 255 holds the 255th input gid, which is gid 254 here.
    assert_eq!(&otf.cid_to_gid_map[510..512], &254u16.to_be_bytes());
    assert_eq!(otf.widths.len(), 300);

    let t3 = emit_type3_font(&face, &gids);
    assert_eq!(t3.char_procs.len(), 255);
    assert_eq!(t3.encoding.last().map(|(code, _)| *code), Some(255));
}

#[test]
fn extreme_coordinates_keep_every_digit() {
    let ops = [
        PathOp::MoveTo {
            x: f32::MAX,
            y: f32::NAN,
        },
        PathOp::LineTo {
            x: -f32::MAX,
            y: f32::MIN_POSITIVE,
        },
        PathOp::Close,
    ];
    let mut out = Vec::new();
    emit_path_ops(&mut out, &ops);
    let text = String::from_utf8(out).expect("ASCII output");
    assert!(
        text.starts_with("340282350000000000000000000000000000000 0 m\n"),
        "{text}"
    );
    assert!(
        text.contains("-340282350000000000000000000000000000000 "),
        "{text}"
    );
    assert!(!text.contains("NaN") && !text.contains("inf"), "{text}");
}

#[test]
fn zero_units_per_em_face_is_handled() {
    let bytes = with_units_per_em(OPENSANS_BYTES, 0);
    let Ok(face) = Face::parse_bytes(&bytes, 0) else {
        // Rejecting the face outright is also a valid outcome.
        return;
    };
    let gids: [GlyphId; 2] = [0, 1];

    // The core parser rejects this `head`, so the emitters fall back to
    // 1000 units per em. A face that did report zero gets an error.
    match emit_type1_font(&face, &gids) {
        Ok(font) => {
            let needle: &[u8] = b"/FontMatrix [0.001 0 0 0.001 0 0]";
            assert!(font
                .font_dict_body
                .windows(needle.len())
                .any(|w| w == needle));
        }
        Err(err) => assert_eq!(err, EmitError::InvalidUnitsPerEm),
    }

    let t3 = emit_type3_font(&face, &gids);
    assert!(t3.matrix.iter().all(|v| v.is_finite()));

    let otf = emit_otf_embedded_font(&face, &bytes, &gids);
    assert!(otf.widths.iter().all(|(_, w)| w.is_finite()));
}
