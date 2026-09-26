//! Context rules without lookup records survive subsetting.
//!
//! feaLib compiles `ignore sub` and `ignore pos` statements into
//! (chained) context rules that carry no SequenceLookupRecords. Such a
//! rule applies nothing, but when it matches, the shaper stops trying
//! the later rules of the same lookup at that position, so it guards
//! the rules after it. Amiri leans on this for the Allah ligature: its
//! lookup that builds the ligature starts with ignore rules for the
//! contexts where the ligature must not form. A subsetter that treats
//! those rules as empty and drops them changes how the subset shapes.
//!
//! Every string here is shaped against Amiri and against a subset of
//! Amiri cut down to the string's own characters, with both sigilbuzz
//! and rustybuzz, and the outputs must agree glyph for glyph once the
//! source glyph ids are mapped through the subset's gid map.

use rustybuzz::{Face as RbFace, UnicodeBuffer};
use sigilbuzz::{shape, Blob, Buffer, Face, Font};
use sigilbuzz_subset::{subset, SubsetInput};

const AMIRI: &[u8] = include_bytes!("../../../tests/fixtures/amiri_regular.ttf");

/// Strings around the Allah ligature. Its `rlig` lookups open with
/// ignore rules for the contexts where it must not form: after alef
/// or waw, and with a fatha or kasra between the two lams. The strings
/// that shaped differently in a subset before ignore rules were kept
/// are the Allah word itself, its prefixed forms, and the four
/// vowelled forms at the end; the rest are controls.
const CORPUS: &[&str] = &[
    "\u{0627}\u{0644}\u{0644}\u{0647}",
    "\u{0644}\u{0644}\u{0647}",
    "\u{0648}\u{0627}\u{0644}\u{0644}\u{0647}",
    "\u{0628}\u{0627}\u{0644}\u{0644}\u{0647}",
    "\u{0641}\u{0627}\u{0644}\u{0644}\u{0647}",
    "\u{0639}\u{0628}\u{062F} \u{0627}\u{0644}\u{0644}\u{0647}",
    "\u{0627}\u{0644}\u{0644}\u{0647}\u{0645}",
    "\u{062E}\u{0644}\u{0644}\u{0647}",
    "\u{0627}\u{0644}\u{0650}\u{0644}\u{0647}",
    "\u{0622}\u{0644}\u{0650}\u{0644}\u{0647}",
    "\u{0648}\u{0644}\u{064E}\u{0644}\u{0647}",
    "\u{0648}\u{0644}\u{0650}\u{0644}\u{0647}",
];

/// `(glyph, x_advance, y_advance, x_offset, y_offset)` per output glyph.
type Shaped = Vec<(u32, i32, i32, i32, i32)>;

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
        .map(|g| (g.glyph_id, g.x_advance, g.y_advance, g.x_offset, g.y_offset))
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
        .map(|(i, p)| (i.glyph_id, p.x_advance, p.y_advance, p.x_offset, p.y_offset))
        .collect()
}

/// Subsets Amiri to the glyphs `text` maps to through cmap.
fn subset_for(face: &Face<'_>, text: &str) -> (Vec<u8>, Vec<(u16, u16)>) {
    let cmap = face.cmap().expect("cmap");
    let gids = text.chars().filter_map(|c| cmap.glyph_id(c)).collect();
    let input = SubsetInput {
        gids,
        retain_variations: false,
        ..SubsetInput::default()
    };
    let out = subset(face, &input).expect("subset succeeds");
    (out.bytes, out.gid_map)
}

/// Rewrites source glyph ids into the subset's namespace. A glyph the
/// subset dropped maps to `u32::MAX`, which never matches.
fn to_subset_ids(shaped: &Shaped, gid_map: &[(u16, u16)]) -> Shaped {
    shaped
        .iter()
        .map(|&(g, xa, ya, xo, yo)| {
            let new = u16::try_from(g)
                .ok()
                .and_then(|g| gid_map.binary_search_by_key(&g, |&(old, _)| old).ok())
                .map_or(u32::MAX, |i| u32::from(gid_map[i].1));
            (new, xa, ya, xo, yo)
        })
        .collect()
}

#[test]
fn subset_shapes_like_the_source_when_ignore_rules_matter() {
    let face = Face::parse_bytes(AMIRI, 0).unwrap();
    let mut mismatches = Vec::new();
    for text in CORPUS {
        let (bytes, gid_map) = subset_for(&face, text);
        for (engine, shaper) in [
            ("sigilbuzz", shape_sigilbuzz as fn(&[u8], &str) -> Shaped),
            ("rustybuzz", shape_rustybuzz),
        ] {
            let expected = to_subset_ids(&shaper(AMIRI, text), &gid_map);
            let got = shaper(&bytes, text);
            if expected != got {
                mismatches.push(format!(
                    "{engine} {text:?}:\n  source {expected:?}\n  subset {got:?}"
                ));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

fn u16_at(bytes: &[u8], pos: usize) -> usize {
    usize::from(u16::from_be_bytes([bytes[pos], bytes[pos + 1]]))
}

/// Counts the chained context format 3 subtables of a GSUB table that
/// carry no SubstLookupRecords, looking through Extension lookups.
fn ignore_rule_count(gsub: &[u8]) -> usize {
    let list = u16_at(gsub, 8);
    let mut count = 0;
    for i in 0..u16_at(gsub, list) {
        let lookup = list + u16_at(gsub, list + 2 + i * 2);
        for k in 0..u16_at(gsub, lookup + 4) {
            let mut kind = u16_at(gsub, lookup);
            let mut sub = lookup + u16_at(gsub, lookup + 6 + k * 2);
            if kind == 7 {
                kind = u16_at(gsub, sub + 2);
                let rel = u32::from_be_bytes([
                    gsub[sub + 4],
                    gsub[sub + 5],
                    gsub[sub + 6],
                    gsub[sub + 7],
                ]);
                sub += rel as usize;
            }
            if kind != 6 || u16_at(gsub, sub) != 3 {
                continue;
            }
            let mut pos = sub + 2;
            for _ in 0..3 {
                pos += 2 + u16_at(gsub, pos) * 2;
            }
            if u16_at(gsub, pos) == 0 {
                count += 1;
            }
        }
    }
    count
}

/// Guards against a vacuous pass: the subset for the Allah word must
/// still carry the ignore rules the source uses for it.
#[test]
fn allah_subset_keeps_its_ignore_rules() {
    let face = Face::parse_bytes(AMIRI, 0).unwrap();
    let (bytes, _) = subset_for(&face, CORPUS[0]);
    let out = Face::parse_bytes(&bytes, 0).unwrap();
    let source_rules = ignore_rule_count(face.table_bytes(*b"GSUB").unwrap());
    let subset_rules = ignore_rule_count(out.table_bytes(*b"GSUB").unwrap());
    assert!(source_rules > 0, "Amiri should carry ignore rules");
    assert!(
        subset_rules > 0,
        "the subset lost every ignore rule ({source_rules} in the source)"
    );
}
