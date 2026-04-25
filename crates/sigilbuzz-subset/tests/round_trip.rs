//! Round-trip integration tests against the bundled Open Sans
//! fixture.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};
use sigilbuzz_subset::{subset, SubsetError, SubsetInput};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

fn open_sans_face() -> Face<'static> {
    Face::parse_bytes(OPEN_SANS, 0).unwrap()
}

fn cmap_lookup(face: &Face<'_>, ch: char) -> u16 {
    face.cmap().unwrap().glyph_id(ch).unwrap()
}

#[test]
fn subsets_open_sans_to_abc() {
    let face = open_sans_face();
    let original_size = OPEN_SANS.len();

    let gid_a = cmap_lookup(&face, 'A');
    let gid_b = cmap_lookup(&face, 'B');
    let gid_c = cmap_lookup(&face, 'C');

    let input = SubsetInput {
        gids: vec![gid_a, gid_b, gid_c],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: false,
        retain_variations: false,
    };
    let out = subset(&face, &input).expect("subset succeeds");
    let subset_size = out.bytes.len();

    // The subset must be parseable as a face on its own.
    let blob = Blob::from_vec(out.bytes.clone());
    let subset_face = Face::parse(&blob, 0).expect("subset face parses");

    // numGlyphs >= 4: notdef + A + B + C.
    let new_num_glyphs = subset_face.maxp().unwrap().num_glyphs;
    assert!(
        new_num_glyphs >= 4,
        "expected at least 4 glyphs, got {new_num_glyphs}",
    );

    // Each character resolves through the new cmap to a non-zero gid
    // less than the new numGlyphs.
    for ch in ['A', 'B', 'C'] {
        let gid = subset_face
            .cmap()
            .unwrap()
            .glyph_id(ch)
            .unwrap_or_else(|| panic!("subset cmap lost {ch}"));
        assert!(
            gid > 0 && gid < new_num_glyphs,
            "subset cmap of {ch} = {gid} out of [1..{new_num_glyphs})",
        );
    }

    // Advance widths must match across the original and subset for
    // every kept gid. We consult the gid_map to translate.
    let original_hmtx = face.hmtx().unwrap();
    let subset_hmtx = subset_face.hmtx().unwrap();
    for (old, new) in &out.gid_map {
        let want = original_hmtx.advance(*old).unwrap_or(0);
        let got = subset_hmtx.advance(*new).unwrap_or(0);
        assert_eq!(
            want, got,
            "advance mismatch: old gid {old} = {want}, new gid {new} = {got}",
        );
    }

    // Subset must be < 30 % of the original.
    let pct = subset_size * 100 / original_size;
    eprintln!(
        "open_sans subset → {{A,B,C}}: original {original_size} bytes, subset {subset_size} bytes ({pct}%)",
    );
    assert!(
        subset_size < original_size * 30 / 100,
        "subset size {subset_size} not < 30% of {original_size}",
    );

    // Sanity: gid_map starts with (0, 0).
    assert_eq!(out.gid_map[0], (0, 0));
}

#[test]
fn subset_is_deterministic() {
    let face = open_sans_face();
    let gid_a = cmap_lookup(&face, 'A');
    let gid_b = cmap_lookup(&face, 'B');
    let gid_c = cmap_lookup(&face, 'C');

    let input = SubsetInput {
        gids: vec![gid_a, gid_b, gid_c],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: false,
        retain_variations: false,
    };

    let a = subset(&face, &input).unwrap();
    let b = subset(&face, &input).unwrap();
    assert_eq!(a.bytes, b.bytes, "subset output is non-deterministic");
    assert_eq!(a.gid_map, b.gid_map);
}

#[test]
fn subset_input_order_is_irrelevant() {
    // Different input orderings should still produce the same closure
    // and the same byte output, since gid_map sorts by old gid.
    let face = open_sans_face();
    let gid_a = cmap_lookup(&face, 'A');
    let gid_b = cmap_lookup(&face, 'B');
    let gid_c = cmap_lookup(&face, 'C');

    let i1 = SubsetInput {
        gids: vec![gid_a, gid_b, gid_c],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: false,
        retain_variations: false,
    };
    let i2 = SubsetInput {
        gids: vec![gid_c, gid_b, gid_a],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: false,
        retain_variations: false,
    };
    let a = subset(&face, &i1).unwrap();
    let b = subset(&face, &i2).unwrap();
    assert_eq!(a.bytes, b.bytes);
}

#[test]
fn empty_gid_set_implicitly_keeps_notdef_when_drop_unhandled() {
    // drop_unhandled=true (default-ish): empty set is tolerated and
    // produces a font with just .notdef.
    let face = open_sans_face();
    let input = SubsetInput {
        gids: vec![],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: false,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let blob = Blob::from_vec(out.bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    assert_eq!(subset_face.maxp().unwrap().num_glyphs, 1);
}

#[test]
fn empty_gid_set_errors_when_strict() {
    let face = open_sans_face();
    let input = SubsetInput {
        gids: vec![],
        retain_hints: false,
        drop_unhandled: false,
        retain_layout: false,
        retain_variations: false,
    };
    let err = subset(&face, &input).unwrap_err();
    assert!(matches!(err, SubsetError::EmptyGidSet));
}

#[test]
fn out_of_range_gid_errors() {
    let face = open_sans_face();
    let n = face.maxp().unwrap().num_glyphs;
    let input = SubsetInput {
        gids: vec![n + 100],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: false,
        retain_variations: false,
    };
    let err = subset(&face, &input).unwrap_err();
    assert!(
        matches!(err, SubsetError::GidOutOfRange { .. }),
        "expected GidOutOfRange, got {err:?}",
    );
}

#[test]
fn shaping_subset_font_matches_remap() {
    // After subsetting Open Sans to {A, B, C}, shaping each character
    // through the subset must produce gids equal to what cmap_lookup
    // returns for those characters in the subset font — i.e. the
    // glyph stream is consistent with the new gid namespace.
    let face = open_sans_face();
    let gid_a = cmap_lookup(&face, 'A');
    let gid_b = cmap_lookup(&face, 'B');
    let gid_c = cmap_lookup(&face, 'C');

    let input = SubsetInput {
        gids: vec![gid_a, gid_b, gid_c],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: false,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let blob = Blob::from_vec(out.bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(subset_face.clone(), 16.0);

    for ch in ['A', 'B', 'C'] {
        let mut buf = Buffer::new();
        let s = ch.to_string();
        buf.set_text(&s);
        let run = shape(&font, &buf, &[]).expect("shape succeeds");
        assert_eq!(run.glyphs.len(), 1, "{ch}: expected 1 glyph");
        let shaped_gid: u32 = run.glyphs[0].glyph_id;
        let expected_gid: u32 = subset_face.cmap().unwrap().glyph_id(ch).unwrap().into();
        assert_eq!(
            shaped_gid, expected_gid,
            "shaping {ch} through subset gave gid {shaped_gid}, expected {expected_gid}",
        );
    }
}

#[test]
fn cff_font_errors_cleanly() {
    // We synthesise a minimal SFNT directory advertising a CFF table
    // — Face::parse only validates the directory shape, so this is
    // enough to prove the early CFF branch fires before we touch
    // anything else.
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(&0x4F54_544Fu32.to_be_bytes()); // 'OTTO'
    bytes.extend_from_slice(&1u16.to_be_bytes()); // numTables
    bytes.extend_from_slice(&0u16.to_be_bytes()); // searchRange
    bytes.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
    bytes.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
    bytes.extend_from_slice(b"CFF "); // tag
    bytes.extend_from_slice(&0u32.to_be_bytes()); // checksum
    bytes.extend_from_slice(&(12u32 + 16).to_be_bytes()); // offset
    bytes.extend_from_slice(&4u32.to_be_bytes()); // length
    bytes.extend_from_slice(&[0u8; 4]); // CFF body

    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let input = SubsetInput::default();
    let err = subset(&face, &input).unwrap_err();
    assert!(matches!(err, SubsetError::Unsupported(_)));
}
