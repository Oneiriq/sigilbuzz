//! GSUB type 8 (reverse chaining single substitution) survives
//! subsetting.
//!
//! Open Sans gets a hand-built GSUB whose only lookup, on by default
//! through `calt`, is a reverse chaining rule: `b` becomes `x` when a
//! `c` follows. Subsetting to the glyphs of "bc" keeps `b` and `c`
//! only, so the substitute `x` has to come from the closure; without
//! it the rewrite dropped the pair and the subset stopped substituting.
//! Both shapers must agree on source and subset once the source glyph
//! ids are mapped through the subset's gid map.

#[path = "support/sfnt.rs"]
mod sfnt;

use rustybuzz::{Face as RbFace, UnicodeBuffer};
use sigilbuzz::{shape, Blob, Buffer, Face, Font};
use sigilbuzz_subset::{subset, SubsetInput};

use sfnt::{be16, edit_tables};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

/// A format 1 Coverage over `glyphs` (sorted).
fn coverage(glyphs: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    be16(&mut out, 1);
    be16(&mut out, glyphs.len() as u16);
    for &g in glyphs {
        be16(&mut out, g);
    }
    out
}

/// A GSUB 1.0 with `DFLT` and `latn` scripts, one `calt` feature and
/// one type 8 lookup substituting `input` by `substitute` when a glyph
/// of `lookahead` follows.
fn reverse_chain_gsub(input: u16, substitute: u16, lookahead: u16) -> Vec<u8> {
    // ReverseChainSingleSubstFormat1, coverages after the arrays.
    let mut sub = Vec::new();
    be16(&mut sub, 1);
    be16(&mut sub, 14); // coverageOffset
    be16(&mut sub, 0); // backtrackGlyphCount
    be16(&mut sub, 1); // lookaheadGlyphCount
    be16(&mut sub, 20); // lookaheadCoverageOffsets[0]
    be16(&mut sub, 1); // glyphCount
    be16(&mut sub, substitute);
    sub.extend_from_slice(&coverage(&[input]));
    sub.extend_from_slice(&coverage(&[lookahead]));

    // LookupList: one lookup of type 8 with the subtable.
    let mut lookups = Vec::new();
    for v in [1u16, 4, 8, 0, 1, 8] {
        be16(&mut lookups, v);
    }
    lookups.extend_from_slice(&sub);

    // FeatureList: `calt` naming lookup 0.
    let mut features = Vec::new();
    be16(&mut features, 1);
    features.extend_from_slice(b"calt");
    for v in [8u16, 0, 1, 0] {
        be16(&mut features, v);
    }

    // ScriptList: DFLT and latn, both with a default LangSys on
    // feature 0.
    let mut scripts = Vec::new();
    be16(&mut scripts, 2);
    for (tag, off) in [(b"DFLT", 14u16), (b"latn", 26)] {
        scripts.extend_from_slice(tag);
        be16(&mut scripts, off);
    }
    for _ in 0..2 {
        for v in [4u16, 0, 0, 0xFFFF, 1, 0] {
            be16(&mut scripts, v);
        }
    }

    let mut out = Vec::new();
    let feature_at = 10 + scripts.len();
    let lookup_at = feature_at + features.len();
    for v in [1u16, 0, 10, feature_at as u16, lookup_at as u16] {
        be16(&mut out, v);
    }
    out.extend_from_slice(&scripts);
    out.extend_from_slice(&features);
    out.extend_from_slice(&lookups);
    out
}

type Shaped = Vec<(u32, i32)>;

fn shape_sigilbuzz(bytes: &[u8], text: &str) -> Shaped {
    let blob = Blob::from_vec(bytes.to_vec());
    let face = Face::parse(&blob, 0).expect("sigilbuzz parses");
    let upem = face.head().expect("head").units_per_em;
    let font = Font::new(face, f32::from(upem));
    let mut buffer = Buffer::new();
    buffer.set_text(text);
    shape(&font, &buffer, &[])
        .expect("sigilbuzz shapes")
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.x_advance))
        .collect()
}

fn shape_rustybuzz(bytes: &[u8], text: &str) -> Shaped {
    let face = RbFace::from_slice(bytes, 0).expect("rustybuzz parses");
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    let out = rustybuzz::shape(&face, &[], buffer);
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| (i.glyph_id, p.x_advance))
        .collect()
}

/// The Open Sans glyph for `ch`.
fn glyph(ch: char) -> u16 {
    let face = Face::parse_bytes(OPEN_SANS, 0).unwrap();
    face.cmap().unwrap().glyph_id(ch).unwrap()
}

/// Open Sans with the reverse chaining GSUB `b -> x / _ c`. Its legacy
/// `kern` table goes too: the subsetter drops `kern`, and the shapers
/// would otherwise kern the source but not the subset.
fn font() -> Vec<u8> {
    let gsub = reverse_chain_gsub(glyph('b'), glyph('x'), glyph('c'));
    edit_tables(OPEN_SANS, &[(*b"GSUB", Some(gsub)), (*b"kern", None)])
}

#[test]
fn subset_keeps_the_reverse_chain_substitute_and_shapes_like_the_source() {
    let source = font();
    let face = Face::parse_bytes(&source, 0).unwrap();
    let input = SubsetInput {
        gids: vec![glyph('b'), glyph('c')],
        ..SubsetInput::default()
    };
    let out = subset(&face, &input).expect("subset succeeds");
    let new_x = out
        .gid_map
        .iter()
        .find(|&&(old, _)| old == glyph('x'))
        .map(|&(_, new)| u32::from(new))
        .expect("the closure keeps the substitute");

    for (engine, shaper) in [
        ("sigilbuzz", shape_sigilbuzz as fn(&[u8], &str) -> Shaped),
        ("rustybuzz", shape_rustybuzz),
    ] {
        let expected: Shaped = shaper(&source, "bc")
            .into_iter()
            .map(|(g, advance)| {
                let old = u16::try_from(g).unwrap();
                let (_, new) = out.gid_map.iter().find(|&&(o, _)| o == old).unwrap();
                (u32::from(*new), advance)
            })
            .collect();
        assert_eq!(expected[0].0, new_x, "{engine}: the source substitutes b");
        assert_eq!(shaper(&out.bytes, "bc"), expected, "{engine}");
    }
}

#[test]
fn subset_leaves_the_substitute_out_when_the_context_cannot_match() {
    let source = font();
    let face = Face::parse_bytes(&source, 0).unwrap();
    let input = SubsetInput {
        gids: vec![glyph('b'), glyph('d')],
        ..SubsetInput::default()
    };
    let out = subset(&face, &input).expect("subset succeeds");
    assert!(
        out.gid_map.iter().all(|&(old, _)| old != glyph('x')),
        "no c is kept, so the rule can never fire"
    );
}
