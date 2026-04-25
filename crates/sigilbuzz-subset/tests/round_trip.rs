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
    // enough to prove the CFF dispatch surfaces a clean error path.
    // Without a maxp the closure walk can't compute num_glyphs, so the
    // error variant here is MissingTable rather than Unsupported. The
    // original-intent invariant — "CFF input never panics, never
    // bubbles a Parse error" — is what this test still guards.
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
    // Either MissingTable(maxp) (no maxp in the synthetic fixture) or
    // Unsupported (for a real CFF source under non-identity gid map);
    // both are clean.
    assert!(matches!(
        err,
        SubsetError::MissingTable(_) | SubsetError::Unsupported(_)
    ));
}

/// Builds a minimal SFNT directory with the supplied tables, sorted
/// ascending by tag, with correct offsets / lengths / checksums of zero
/// (Face::parse_bytes only validates structural shape).
fn build_synthetic_sfnt(version: u32, mut tables: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
    tables.sort_by_key(|(t, _)| *t);
    let n = tables.len();
    let mut out = Vec::new();
    out.extend_from_slice(&version.to_be_bytes());
    out.extend_from_slice(&(n as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // searchRange
    out.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
    out.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
    let header_len = 12 + n * 16;
    let mut data_off = header_len;
    for (_, body) in &tables {
        data_off = (data_off + ((body.len() + 3) & !3)).max(data_off);
    }
    let mut cur_off = header_len;
    // Reserve directory space; fill below.
    let dir_off = out.len();
    for _ in 0..n {
        out.extend_from_slice(&[0u8; 16]);
    }
    let mut entries: Vec<(usize, usize)> = Vec::with_capacity(n);
    for (_, body) in &tables {
        let off = out.len();
        out.extend_from_slice(body);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        entries.push((off, body.len()));
        cur_off = off;
    }
    let _ = cur_off;
    let _ = data_off;
    for (i, ((tag, _), (off, len))) in tables.iter().zip(entries.iter()).enumerate() {
        let d = dir_off + i * 16;
        out[d..d + 4].copy_from_slice(tag);
        out[d + 4..d + 8].copy_from_slice(&0u32.to_be_bytes());
        out[d + 8..d + 12].copy_from_slice(&(*off as u32).to_be_bytes());
        out[d + 12..d + 16].copy_from_slice(&(*len as u32).to_be_bytes());
    }
    out
}

/// Builds a minimal `head` table (54 bytes) with sensible defaults.
fn build_minimal_head() -> Vec<u8> {
    let mut head = Vec::with_capacity(54);
    head.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // version
    head.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // fontRev
    head.extend_from_slice(&0u32.to_be_bytes()); // checkSumAdjustment
    head.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes()); // magicNumber
    head.extend_from_slice(&0u16.to_be_bytes()); // flags
    head.extend_from_slice(&1000u16.to_be_bytes()); // unitsPerEm
    head.extend_from_slice(&0u64.to_be_bytes()); // created
    head.extend_from_slice(&0u64.to_be_bytes()); // modified
    head.extend_from_slice(&0i16.to_be_bytes()); // xMin
    head.extend_from_slice(&0i16.to_be_bytes()); // yMin
    head.extend_from_slice(&0i16.to_be_bytes()); // xMax
    head.extend_from_slice(&0i16.to_be_bytes()); // yMax
    head.extend_from_slice(&0u16.to_be_bytes()); // macStyle
    head.extend_from_slice(&8u16.to_be_bytes()); // lowestRecPPEM
    head.extend_from_slice(&0i16.to_be_bytes()); // fontDirectionHint
    head.extend_from_slice(&0i16.to_be_bytes()); // indexToLocFormat
    head.extend_from_slice(&0i16.to_be_bytes()); // glyphDataFormat
    head
}

/// Builds a minimal `maxp` v0.5 table — the 6-byte short form used by
/// CFF fonts.
fn build_minimal_maxp(num_glyphs: u16) -> Vec<u8> {
    let mut maxp = Vec::new();
    maxp.extend_from_slice(&0x0000_5000u32.to_be_bytes()); // version 0.5
    maxp.extend_from_slice(&num_glyphs.to_be_bytes());
    maxp
}

#[test]
fn cff_identity_passthrough_preserves_table_bytes() {
    // Synthesise a CFF1 font with just enough tables to satisfy the
    // closure walker: head + maxp + a minimal CFF table. With
    // gids = [] and drop_unhandled = true the closure walker keeps
    // only gid 0; if num_glyphs = 1 the kept set is the identity, so
    // the CFF dispatch hits the passthrough branch.
    //
    // The CFF body itself is a 4-byte header — Face::parse_bytes only
    // validates the SFNT directory shape, and the subset entry's
    // identity-passthrough path never re-parses the CFF body. The
    // round-trip we care about is "same bytes survive into output".
    let cff_body: Vec<u8> = vec![1, 0, 4, 1]; // major=1 minor=0 hdrSize=4 offSize=1
    let head = build_minimal_head();
    let maxp = build_minimal_maxp(1);

    let bytes = build_synthetic_sfnt(
        0x4F54_544Fu32, // 'OTTO'
        vec![
            (*b"CFF ", cff_body.clone()),
            (*b"head", head),
            (*b"maxp", maxp),
        ],
    );
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let out = subset(&face, &SubsetInput::default()).expect("CFF identity passthrough succeeds");
    // gid_map is identity over kept set: [(0, 0)].
    assert_eq!(out.gid_map, vec![(0u16, 0u16)]);

    // The CFF body must travel byte-identical into the output.
    let new_face = Face::parse_bytes(&out.bytes, 0).expect("subset face re-parses");
    let new_cff = new_face
        .table_bytes(*b"CFF ")
        .expect("CFF survives passthrough");
    assert_eq!(new_cff, cff_body.as_slice());
    // Output advertises 'OTTO' too.
    assert_eq!(&out.bytes[0..4], &0x4F54_544Fu32.to_be_bytes());
}

#[test]
fn cff2_identity_passthrough_preserves_table_bytes() {
    // CFF2 mirrors CFF1: identity passthrough preserves the table
    // bytes. Use a 5-byte CFF2 header: major=2 minor=0 hdrSize=5
    // topDictLength=0.
    let cff2_body: Vec<u8> = vec![2, 0, 5, 0, 0];
    let head = build_minimal_head();
    let maxp = build_minimal_maxp(1);
    let bytes = build_synthetic_sfnt(
        0x4F54_544Fu32,
        vec![
            (*b"CFF2", cff2_body.clone()),
            (*b"head", head),
            (*b"maxp", maxp),
        ],
    );
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let out = subset(&face, &SubsetInput::default()).expect("CFF2 identity passthrough succeeds");
    assert_eq!(out.gid_map, vec![(0u16, 0u16)]);
    let new_face = Face::parse_bytes(&out.bytes, 0).expect("subset face re-parses");
    let new_cff2 = new_face
        .table_bytes(*b"CFF2")
        .expect("CFF2 survives passthrough");
    assert_eq!(new_cff2, cff2_body.as_slice());
}

#[test]
fn cff_non_identity_subset_errors_unsupported() {
    // Same fixture but with num_glyphs = 2: the closure walker keeps
    // only gid 0, so the kept set is [0] — not the identity over a
    // 2-glyph font. The dispatch must surface Unsupported with the
    // dedicated CFF rewrite-staged context string.
    let cff_body: Vec<u8> = vec![1, 0, 4, 1];
    let head = build_minimal_head();
    let maxp = build_minimal_maxp(2);
    let bytes = build_synthetic_sfnt(
        0x4F54_544Fu32,
        vec![
            (*b"CFF ", cff_body),
            (*b"head", head),
            (*b"maxp", maxp),
        ],
    );
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let err = subset(&face, &SubsetInput::default()).unwrap_err();
    match err {
        SubsetError::Unsupported(msg) => {
            assert!(
                msg.contains("CFF"),
                "expected CFF-specific Unsupported message, got {msg}",
            );
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}
