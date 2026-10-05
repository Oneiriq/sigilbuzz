//! A `VARC` table the core cannot parse is left out of a subset with a
//! warning, as the face's outlines read the font without it and as
//! HarfBuzz drops it. Before, `subset` failed on such a font.
//!
//! The table splices in a MultiItemVariationStore in the layout HarfBuzz
//! adopted after 14.5.0 (a 32-bit region count, and an Offset32 from each
//! MultiVarData to its delta-set INDEX), which the core rejects.

use sigilbuzz::Face;
use sigilbuzz_subset::{subset, SubsetInput};

const GLYF_FONT: &[u8] = include_bytes!("../../../tests/fixtures/varc_parity.ttf");

/// A `VARC` table covering `covered`, each glyph one component naming
/// glyph 1, over a MultiItemVariationStore in the post-14.5.0 layout.
fn new_layout_varc(covered: &[u16]) -> Vec<u8> {
    let mut coverage = vec![0, 1];
    coverage.extend_from_slice(&(covered.len() as u16).to_be_bytes());
    for g in covered {
        coverage.extend_from_slice(&g.to_be_bytes());
    }

    let mut store = Vec::new();
    store.extend_from_slice(&1u16.to_be_bytes()); // format
    store.extend_from_slice(&12u32.to_be_bytes()); // region list
    store.extend_from_slice(&1u16.to_be_bytes()); // one MultiVarData
    store.extend_from_slice(&30u32.to_be_bytes()); // at 30
    store.extend_from_slice(&1u32.to_be_bytes()); // region count (u32)
    store.extend_from_slice(&8u32.to_be_bytes()); // Offset32 to the region
    store.extend_from_slice(&1u16.to_be_bytes()); // axis count
    store.extend_from_slice(&0u16.to_be_bytes()); // axis 0
    store.extend_from_slice(&0i16.to_be_bytes());
    store.extend_from_slice(&0x4000i16.to_be_bytes());
    store.extend_from_slice(&0x4000i16.to_be_bytes());
    assert_eq!(store.len(), 30);
    store.push(1); // MultiVarData format
    store.extend_from_slice(&1u16.to_be_bytes());
    store.extend_from_slice(&0u16.to_be_bytes());
    store.extend_from_slice(&9u32.to_be_bytes()); // Offset32 to the INDEX
    store.extend_from_slice(&1u32.to_be_bytes());
    store.extend_from_slice(&[1, 1, 3, 0x00, 100]);

    let record = [0x18, 0x00, 0x01, 0x00, 0x00, 0x00];
    let mut records = (covered.len() as u32).to_be_bytes().to_vec();
    records.push(1); // offSize
    for i in 0..=covered.len() {
        records.push((1 + i * record.len()) as u8);
    }
    for _ in covered {
        records.extend_from_slice(&record);
    }

    let mut out = Vec::new();
    out.extend_from_slice(&[0, 1, 0, 0]); // version 1.0
    let mut at = 24u32;
    for part in [&coverage, &store] {
        out.extend_from_slice(&at.to_be_bytes());
        at += part.len() as u32;
    }
    out.extend_from_slice(&0u32.to_be_bytes()); // no conditions
    out.extend_from_slice(&0u32.to_be_bytes()); // no axis indices
    out.extend_from_slice(&at.to_be_bytes()); // glyph records
    out.extend_from_slice(&coverage);
    out.extend_from_slice(&store);
    out.extend_from_slice(&records);
    out
}

/// `font` with its tables rewritten into a new directory, `VARC`
/// replaced by `varc`, or dropped when `varc` is `None`.
fn with_varc(font: &[u8], varc: Option<&[u8]>) -> Vec<u8> {
    let face = Face::parse_bytes(font, 0).unwrap();
    let mut tables: Vec<([u8; 4], &[u8])> = face
        .records()
        .iter()
        .filter(|r| &r.tag != b"VARC")
        .map(|r| (r.tag, face.table_bytes(r.tag).unwrap()))
        .collect();
    if let Some(varc) = varc {
        tables.push((*b"VARC", varc));
    }
    tables.sort_by_key(|t| t.0);
    let header = 12 + 16 * tables.len();
    let mut out = Vec::new();
    out.extend_from_slice(&font[..4]);
    out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    let mut body = Vec::new();
    for (tag, data) in &tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&((header + body.len()) as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.extend_from_slice(data);
        while body.len() % 4 != 0 {
            body.push(0);
        }
    }
    out.extend_from_slice(&body);
    out
}

#[test]
fn a_subset_leaves_out_a_varc_that_does_not_parse() {
    let bad_varc = new_layout_varc(&[2, 3]);
    let with_bad = with_varc(GLYF_FONT, Some(&bad_varc));
    let without = with_varc(GLYF_FONT, None);
    let bad_face = Face::parse_bytes(&with_bad, 0).unwrap();
    assert!(bad_face.varc().is_err(), "the spliced VARC must not parse");
    let plain_face = Face::parse_bytes(&without, 0).unwrap();

    for drop_unhandled in [false, true] {
        let input = SubsetInput {
            gids: vec![0, 1, 2, 3],
            drop_unhandled,
            ..Default::default()
        };
        let got = subset(&bad_face, &input).expect("subset succeeds without VARC");
        let want = subset(&plain_face, &input).expect("subset of the font without VARC");
        assert_eq!(
            got.bytes, want.bytes,
            "the subset equals the subset of the font without VARC (drop_unhandled {drop_unhandled})"
        );
        let out = Face::parse_bytes(&got.bytes, 0).unwrap();
        assert!(out.record(*b"VARC").is_none(), "VARC is left out");
        assert!(
            got.warnings
                .iter()
                .any(|w| w.table == *b"VARC" && w.dropped == "the whole table"),
            "a warning names the dropped VARC: {:?}",
            got.warnings
        );
    }
}
