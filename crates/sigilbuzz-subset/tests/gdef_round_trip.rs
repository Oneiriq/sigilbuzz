//! GDEF subtables survive a subset.
//!
//! - Noto Sans Devanagari, Noto Serif Tibetan, and Amiri carry
//!   `MarkGlyphSetsDef` tables that GSUB / GPOS lookups reach through
//!   the `UseMarkFilteringSet` flag. The subset must keep every set at
//!   its index with the glyphs remapped, and text the subset covers
//!   must shape the same as in the source.
//! - Amiri and Rubik VF carry `LigCaretList` tables (Rubik's carets
//!   are format 3 with VariationIndex tables), and Rubik a GDEF
//!   `ItemVariationStore` that its GPOS kerning and anchors vary
//!   through.
//!
//! The subsets drop the basic Latin letters (or keep only a handful of
//! glyphs), so glyph ids really do renumber and the rewriters run.
//!
//! Of the shaping checks, the Amiri one depends on the mark glyph sets:
//! with them missing, lookups flagged `UseMarkFilteringSet` skip every
//! mark and the shadda / fatha stack in its third text lands
//! elsewhere.

use sigilbuzz::tables::tag;
use sigilbuzz::{shape, Buffer, Face, Font};
use sigilbuzz_subset::{subset, SubsetInput, SubsetOutput};

const DEVANAGARI: &[u8] = include_bytes!("../../../tests/fonts/NotoSansDevanagari-Regular.ttf");
const TIBETAN: &[u8] = include_bytes!("../../../tests/fonts/NotoSerifTibetan-Regular.ttf");
const AMIRI: &[u8] = include_bytes!("../../../tests/fixtures/amiri_regular.ttf");
const RUBIK: &[u8] = include_bytes!("../../../tests/fixtures/rubik_vf.ttf");

fn face(bytes: &'static [u8]) -> Face<'static> {
    Face::parse_bytes(bytes, 0).unwrap()
}

/// Every glyph except the basic Latin letters: enough to shape any
/// non-Latin text the way the source does, while still renumbering.
fn all_but_latin(face: &Face<'_>) -> Vec<u16> {
    let cmap = face.cmap().unwrap();
    let latin: Vec<u16> = ('A'..='Z')
        .chain('a'..='z')
        .filter_map(|c| cmap.glyph_id(c))
        .collect();
    let num_glyphs = face.maxp().unwrap().num_glyphs;
    (0..num_glyphs).filter(|g| !latin.contains(g)).collect()
}

fn subset_with(face: &Face<'_>, gids: Vec<u16>, retain_variations: bool) -> SubsetOutput {
    let input = SubsetInput {
        gids,
        retain_variations,
        ..Default::default()
    };
    subset(face, &input).expect("subset succeeds")
}

fn new_gid(out: &SubsetOutput, old: u16) -> Option<u16> {
    out.gid_map
        .iter()
        .find_map(|&(o, n)| (o == old).then_some(n))
}

/// `(glyph, cluster, x_advance, x_offset, y_offset)` per glyph.
fn shaped(face: &Face<'_>, text: &str) -> Vec<(u32, u32, i32, i32, i32)> {
    let font = Font::new(face.clone(), 1000.0);
    let mut buf = Buffer::new();
    buf.set_text(text);
    let run = shape(&font, &buf, &[]).unwrap();
    run.glyphs
        .iter()
        .map(|g| (g.glyph_id, g.cluster, g.x_advance, g.x_offset, g.y_offset))
        .collect()
}

/// Shapes `text` with the source and the subset and checks the runs
/// agree once source glyph ids are mapped into the subset.
fn assert_shapes_alike(source: &Face<'_>, out: &SubsetOutput, text: &str) {
    let subset_face = Face::parse_bytes(&out.bytes, 0).unwrap();
    let expected: Vec<_> = shaped(source, text)
        .into_iter()
        .map(|(g, c, adv, x, y)| {
            let g = new_gid(out, g as u16).expect("shaped glyph kept") as u32;
            (g, c, adv, x, y)
        })
        .collect();
    assert_eq!(shaped(&subset_face, text), expected, "text {text:?}");
}

/// Checks that every mark glyph set of the subset is the source set
/// mapped through the subset's glyph map, index for index.
fn assert_mark_sets_remapped(source: &Face<'_>, out: &SubsetOutput) {
    let src = source.gdef().unwrap().unwrap();
    let subset_face = Face::parse_bytes(&out.bytes, 0).unwrap();
    let dst = subset_face.gdef().unwrap().expect("GDEF survives");
    let mut index = 0;
    while let Some(src_set) = src.mark_filtering_set(index) {
        let dst_set = dst
            .mark_filtering_set(index)
            .unwrap_or_else(|| panic!("mark glyph set {index} missing"));
        for &(old, new) in &out.gid_map {
            assert_eq!(
                dst_set.contains(new),
                src_set.contains(old),
                "set {index}, glyph {old} -> {new}"
            );
        }
        index += 1;
    }
    assert!(index > 0, "source has mark glyph sets");
    assert!(dst.mark_filtering_set(index).is_none(), "no extra sets");
}

#[test]
fn devanagari_subset_keeps_mark_glyph_sets_and_shapes_alike() {
    let source = face(DEVANAGARI);
    let out = subset_with(&source, all_but_latin(&source), false);
    assert_mark_sets_remapped(&source, &out);
    for text in [
        "\u{915}\u{94d}\u{937}\u{93f}",
        "\u{930}\u{94d}\u{915}\u{93f}\u{902}",
        "\u{939}\u{93f}\u{902}\u{926}\u{940}",
        "\u{915}\u{943}\u{901}",
        "\u{926}\u{94d}\u{926}\u{94d}\u{927}\u{94d}\u{930}\u{94d}\u{92f}",
        "\u{936}\u{94d}\u{930}\u{940}\u{902}",
        "\u{930}\u{941}\u{902}",
        "\u{915}\u{94d}\u{930}\u{94d}\u{92f}\u{93e}\u{901}",
    ] {
        assert_shapes_alike(&source, &out, text);
    }
}

#[test]
fn tibetan_subset_keeps_mark_glyph_sets_and_shapes_alike() {
    let source = face(TIBETAN);
    let out = subset_with(&source, all_but_latin(&source), false);
    assert_mark_sets_remapped(&source, &out);
    for text in [
        "\u{f56}\u{f66}\u{f92}\u{fb2}\u{f74}\u{f56}\u{f66}",
        "\u{f40}\u{fb5}",
        "\u{f66}\u{f90}\u{fb1}\u{f7a}\u{f66}",
        "\u{f67}\u{f71}\u{f74}\u{f83}",
        "\u{f58}\u{f7a}\u{f7e}\u{f0b}\u{f63}\u{f7c}\u{f44}",
        "\u{f68}\u{f7c}\u{f7e}\u{f0b}\u{f58}\u{f53}\u{f72}\u{f0b}\u{f54}\u{f51}\u{fa8}\u{f7a}",
    ] {
        assert_shapes_alike(&source, &out, text);
    }
}

#[test]
fn amiri_subset_keeps_mark_glyph_sets_and_shapes_alike() {
    let source = face(AMIRI);
    let out = subset_with(&source, all_but_latin(&source), false);
    assert_mark_sets_remapped(&source, &out);
    for text in [
        "\u{644}\u{627}",
        "\u{628}\u{650}\u{633}\u{652}\u{645}\u{650}",
        "\u{645}\u{64f}\u{62d}\u{64e}\u{645}\u{651}\u{64e}\u{62f}",
        "\u{642}\u{64f}\u{644}\u{652} \u{647}\u{64f}\u{648}\u{64e}",
    ] {
        assert_shapes_alike(&source, &out, text);
    }
}

fn u16_at(buf: &[u8], pos: usize) -> u16 {
    u16::from_be_bytes([buf[pos], buf[pos + 1]])
}

fn u32_at(buf: &[u8], pos: usize) -> u32 {
    u32::from_be_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]])
}

/// Glyph ids of the Coverage table at `off`, in coverage-index order.
fn coverage_glyphs(buf: &[u8], off: usize) -> Vec<u16> {
    let count = usize::from(u16_at(buf, off + 2));
    match u16_at(buf, off) {
        1 => (0..count).map(|i| u16_at(buf, off + 4 + i * 2)).collect(),
        _ => (0..count)
            .flat_map(|i| u16_at(buf, off + 4 + i * 6)..=u16_at(buf, off + 6 + i * 6))
            .collect(),
    }
}

/// One caret: its format, its coordinate or point index, and for
/// format 3 the `(outer, inner)` of its VariationIndex, if any.
type Caret = (u16, u16, Option<(u16, u16)>);

/// Every ligature in the GDEF LigCaretList with its carets, keyed by
/// glyph id. Format 3 devices resolve against the CaretValue.
fn lig_carets(gdef: &[u8]) -> Vec<(u16, Vec<Caret>)> {
    let list = usize::from(u16_at(gdef, 8));
    if list == 0 {
        return Vec::new();
    }
    let glyphs = coverage_glyphs(gdef, list + usize::from(u16_at(gdef, list)));
    glyphs
        .into_iter()
        .enumerate()
        .map(|(i, gid)| {
            let lig = list + usize::from(u16_at(gdef, list + 4 + i * 2));
            let carets = (0..usize::from(u16_at(gdef, lig)))
                .map(|k| {
                    let caret = lig + usize::from(u16_at(gdef, lig + 2 + k * 2));
                    let format = u16_at(gdef, caret);
                    let device = (format == 3 && u16_at(gdef, caret + 4) != 0).then(|| {
                        let d = caret + usize::from(u16_at(gdef, caret + 4));
                        assert_eq!(u16_at(gdef, d + 4), 0x8000, "VariationIndex");
                        (u16_at(gdef, d), u16_at(gdef, d + 2))
                    });
                    (format, u16_at(gdef, caret + 2), device)
                })
                .collect();
            (gid, carets)
        })
        .collect()
}

/// Checks that every kept ligature has exactly its source carets, with
/// `keep_devices` deciding whether format 3 VariationIndex tables are
/// expected to survive.
fn assert_carets_remapped(source: &Face<'_>, out: &SubsetOutput, keep_devices: bool) {
    let src = lig_carets(source.table_bytes(tag::GDEF).unwrap());
    let subset_face = Face::parse_bytes(&out.bytes, 0).unwrap();
    let dst = lig_carets(subset_face.table_bytes(tag::GDEF).unwrap());
    let expected: Vec<(u16, Vec<Caret>)> = src
        .into_iter()
        .filter_map(|(gid, carets)| {
            let carets = carets
                .into_iter()
                .map(|(f, v, d)| (f, v, d.filter(|_| keep_devices)))
                .collect();
            new_gid(out, gid).map(|g| (g, carets))
        })
        .collect();
    assert!(!expected.is_empty(), "subset keeps some ligatures");
    assert_eq!(dst, expected);
}

#[test]
fn amiri_subset_keeps_its_lig_carets() {
    let source = face(AMIRI);
    let out = subset_with(&source, all_but_latin(&source), false);
    assert_carets_remapped(&source, &out, false);
}

/// Rubik: the ligatures with carets, a few letters and combining
/// marks. Its carets are format 3 with VariationIndex tables.
fn rubik_gids(source: &Face<'_>) -> Vec<u16> {
    let gdef = source.table_bytes(tag::GDEF).unwrap();
    let cmap = source.cmap().unwrap();
    let mut gids: Vec<u16> = lig_carets(gdef).into_iter().map(|(g, _)| g).collect();
    gids.extend(
        "AVTaovqxbf\u{300}\u{301}\u{308}"
            .chars()
            .filter_map(|c| cmap.glyph_id(c)),
    );
    gids
}

#[test]
fn rubik_subset_keeps_carets_mark_sets_and_the_variation_store() {
    let source = face(RUBIK);
    let out = subset_with(&source, rubik_gids(&source), true);
    assert_carets_remapped(&source, &out, true);
    assert_mark_sets_remapped(&source, &out);

    // The store is copied verbatim and placed last, which is where the
    // instancer's prune and partial bake expect it.
    let src_gdef = source.table_bytes(tag::GDEF).unwrap();
    let subset_face = Face::parse_bytes(&out.bytes, 0).unwrap();
    let gdef = subset_face.table_bytes(tag::GDEF).unwrap();
    assert_eq!(u16_at(gdef, 2), 3, "GDEF 1.3");
    let src_store = &src_gdef[u32_at(src_gdef, 14) as usize..];
    let store = &gdef[u32_at(gdef, 14) as usize..];
    assert_eq!(store, &src_store[..store.len()]);
    assert!(subset_face
        .gdef()
        .unwrap()
        .unwrap()
        .item_variation_store()
        .is_some());
}

#[test]
fn rubik_static_subset_drops_the_store_and_caret_variations() {
    let source = face(RUBIK);
    let out = subset_with(&source, rubik_gids(&source), false);
    assert_carets_remapped(&source, &out, false);
    let subset_face = Face::parse_bytes(&out.bytes, 0).unwrap();
    let gdef = subset_face.table_bytes(tag::GDEF).unwrap();
    assert_eq!(u16_at(gdef, 2), 2, "GDEF 1.2: mark sets, no store");
    assert_mark_sets_remapped(&source, &out);
}
