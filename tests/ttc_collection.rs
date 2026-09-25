//! TrueType Collection parsing: a real TTC is synthesized from a
//! vendored fixture font (twice), and every member must behave
//! byte-for-byte like the standalone font: same tables, same
//! metrics, same outlines.

use std::fs;

use sigilbuzz::{fonts_in_collection, Face, OwnedFace};

const FIXTURE: &str = "tests/fonts/NotoSansHebrew-Regular.ttf";

/// Builds a valid single-file TTC v1 containing `fonts` as members.
///
/// Each member's table directory is appended verbatim with its table
/// record offsets rebased to absolute file positions, exactly as the
/// spec requires (member table offsets in a TTC are absolute from the
/// start of the collection file).
fn build_ttc(fonts: &[&[u8]]) -> Vec<u8> {
    let header_len = 12 + 4 * fonts.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"ttcf");
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&u32::try_from(fonts.len()).unwrap().to_be_bytes());

    // Member offsets, computed up front.
    let mut base = header_len;
    for f in fonts {
        out.extend_from_slice(&u32::try_from(base).unwrap().to_be_bytes());
        base += f.len();
    }

    // Members, with table record offsets rebased.
    for f in fonts {
        let member_base = u32::try_from(out.len()).unwrap();
        let start = out.len();
        out.extend_from_slice(f);

        let num_tables = u16::from_be_bytes([f[4], f[5]]) as usize;
        for i in 0..num_tables {
            let rec_offset_field = start + 12 + 16 * i + 8;
            let old = u32::from_be_bytes(
                out[rec_offset_field..rec_offset_field + 4]
                    .try_into()
                    .unwrap(),
            );
            let new = old + member_base;
            out[rec_offset_field..rec_offset_field + 4].copy_from_slice(&new.to_be_bytes());
        }
    }
    out
}

#[test]
fn ttc_members_match_the_standalone_font() {
    let standalone_bytes = fs::read(FIXTURE).expect("fixture font");
    let ttc = build_ttc(&[&standalone_bytes, &standalone_bytes]);

    assert_eq!(fonts_in_collection(&ttc), Some(2));
    assert_eq!(fonts_in_collection(&standalone_bytes), None);

    let standalone = Face::parse_bytes(&standalone_bytes, 0).expect("standalone parse");

    for index in 0..2 {
        let member = Face::parse_bytes(&ttc, index).expect("member parse");
        assert_eq!(member.sfnt_version(), standalone.sfnt_version());
        assert_eq!(member.num_tables(), standalone.num_tables());

        // Every table's bytes are identical to the standalone font's.
        for record in standalone.records() {
            assert_eq!(
                member.table_bytes(record.tag).expect("member table"),
                standalone
                    .table_bytes(record.tag)
                    .expect("standalone table"),
                "table {:?} differs for member {index}",
                core::str::from_utf8(&record.tag),
            );
        }

        // Higher-level views agree too.
        let m_head = member.head().expect("member head");
        let s_head = standalone.head().expect("standalone head");
        assert_eq!(m_head.units_per_em, s_head.units_per_em);

        let gid = 4u16; // arbitrary real glyph
        assert_eq!(
            member.glyph_outline(gid).expect("member outline"),
            standalone.glyph_outline(gid).expect("standalone outline"),
        );
    }
}

#[test]
fn owned_face_indexes_into_collections() {
    let standalone_bytes = fs::read(FIXTURE).expect("fixture font");
    let ttc = build_ttc(&[&standalone_bytes, &standalone_bytes]);

    let owned = OwnedFace::parse(ttc.clone(), 1).expect("owned member parse");
    let standalone = Face::parse_bytes(&standalone_bytes, 0).expect("standalone parse");
    assert_eq!(owned.num_tables(), standalone.num_tables());
    assert_eq!(
        owned.as_face().glyph_bounds(4).expect("member bounds"),
        standalone.glyph_bounds(4).expect("standalone bounds"),
    );
}

#[test]
fn out_of_range_member_index_errors() {
    let standalone_bytes = fs::read(FIXTURE).expect("fixture font");
    let ttc = build_ttc(&[&standalone_bytes]);

    assert!(Face::parse_bytes(&ttc, 0).is_ok());
    assert!(Face::parse_bytes(&ttc, 1).is_err());
    assert!(OwnedFace::parse(ttc, 1).is_err());
}
