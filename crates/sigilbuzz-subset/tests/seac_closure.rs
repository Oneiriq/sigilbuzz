//! A subset that keeps a CFF1 seac glyph keeps the base and accent
//! glyphs it draws.
//!
//! No vendored font uses seac, so the test grafts a hand-built `CFF `
//! onto Source Code Pro's other tables: 96 glyphs like the source's,
//! glyph 2 the `A` (SID 34), glyph 95 the `grave` (SID 124), and glyph
//! 94 an `Agrave` drawn by `0 150 65 193 endchar`, codes 65 and 193 of
//! the Standard Encoding. A subset of glyph 94 alone has to keep 2 and
//! 95, or its seac finds no glyphs to draw.

#[path = "support/sfnt.rs"]
mod support;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;
use sigilbuzz_subset::{
    emit_charset_format0, encode_dict_offset_placeholder, encode_index, encode_int_operand,
    patch_dict_offset, subset, SubsetInput,
};
use support::edit_tables;

const SOURCE_CODE_PRO: &[u8] =
    include_bytes!("../../../tests/fonts/SourceCodePro-Latin-Subset.otf");

/// The glyph count of Source Code Pro's subset, which the grafted `CFF `
/// keeps so `maxp` and `hmtx` still fit.
const GLYPHS: u16 = 96;

/// A charstring pushing `operands` and ending in `op`.
fn cs(operands: &[i32], op: u8) -> Vec<u8> {
    let mut out: Vec<u8> = operands
        .iter()
        .flat_map(|&v| encode_int_operand(v))
        .collect();
    out.push(op);
    out
}

/// A box `w` wide and `h` high at `(x, y)`.
fn shape(x: i32, y: i32, w: i32, h: i32) -> Vec<u8> {
    let mut out = cs(&[x, y], 21); // rmoveto
    out.extend(cs(&[w, 0, 0, h, -w], 6)); // hlineto
    out.push(14); // endchar
    out
}

/// The hand-built name-keyed `CFF `.
fn seac_cff() -> Vec<u8> {
    let mut charstrings: Vec<Vec<u8>> = (0..GLYPHS)
        .map(|g| shape(10, 0, 100 + i32::from(g), 200))
        .collect();
    charstrings[2] = shape(20, 0, 400, 600); // A
    charstrings[95] = shape(0, 0, 120, 80); // grave
    charstrings[94] = cs(&[0, 150, 65, 193], 14); // Agrave
    let sids: Vec<u16> = (1..GLYPHS)
        .map(|g| match g {
            2 => 34,
            95 => 124,
            g => 200 + g,
        })
        .collect();

    let mut top: Vec<u8> = Vec::new();
    let slots: Vec<usize> = [15u8, 17]
        .iter()
        .map(|&op| {
            let at = top.len();
            top.extend_from_slice(&encode_dict_offset_placeholder());
            top.push(op);
            at
        })
        .collect();
    let private_slot = top.len();
    top.extend_from_slice(&encode_dict_offset_placeholder());
    top.extend_from_slice(&encode_dict_offset_placeholder());
    top.push(18);

    let mut out = vec![1u8, 0, 4, 1];
    out.extend_from_slice(&encode_index(&[b"SeacTest"]));
    // The Top DICT follows the INDEX's five header bytes.
    let top_at = out.len() + 5;
    out.extend_from_slice(&encode_index(&[&top[..]]));
    out.extend_from_slice(&encode_index(&[])); // String INDEX
    out.extend_from_slice(&encode_index(&[])); // Global Subr INDEX
    let charset_at = out.len();
    out.extend_from_slice(&emit_charset_format0(&sids));
    let charstrings_at = out.len();
    let refs: Vec<&[u8]> = charstrings.iter().map(Vec::as_slice).collect();
    out.extend_from_slice(&encode_index(&refs));
    let private_at = out.len();
    patch_dict_offset(&mut out, top_at + slots[0], charset_at as i32);
    patch_dict_offset(&mut out, top_at + slots[1], charstrings_at as i32);
    // An empty Private DICT.
    patch_dict_offset(&mut out, top_at + private_slot, 0);
    patch_dict_offset(&mut out, top_at + private_slot + 5, private_at as i32);
    out
}

#[test]
fn a_subset_of_a_seac_glyph_keeps_and_draws_its_components() {
    let font = edit_tables(SOURCE_CODE_PRO, &[(tag::CFF1, Some(seac_cff()))]);
    let face = Face::parse_bytes(&font, 0).expect("grafted font parses");
    let want = face
        .glyph_outline(94)
        .expect("the seac draws")
        .expect("it has an outline");
    // The accent lands 150 units up, after the base.
    assert_eq!(
        want.len(),
        2 * face.glyph_outline(2).unwrap().unwrap().len()
    );

    let input = SubsetInput {
        gids: vec![94],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: true,
    };
    let out = subset(&face, &input).expect("subset");
    let kept: Vec<u16> = out.gid_map.iter().map(|&(old, _)| old).collect();
    assert_eq!(kept, vec![0, 2, 94, 95], "seac components kept");

    let sub = Face::parse_bytes(&out.bytes, 0).expect("subset parses");
    let new = out.gid_map.iter().find(|&&(old, _)| old == 94).unwrap().1;
    let got = sub
        .glyph_outline(new)
        .expect("the seac still draws")
        .expect("it has an outline");
    assert_eq!(got, want);
}
