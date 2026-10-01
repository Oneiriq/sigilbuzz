//! Parses arbitrary bytes as a font and calls every table accessor, plus the
//! per-glyph outline, bounds, bitmap, SVG, and COLR entry points.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sigilbuzz::{fonts_in_collection, Face, OwnedFace};
use sigilbuzz_fuzz::{axis_coords, glyph_ids, split_control, Knobs};

fuzz_target!(|data: &[u8]| {
    let (control, font) = split_control(data, 16);
    let mut knobs = Knobs::new(control);
    let members = fonts_in_collection(font).unwrap_or(1).min(4);
    let index = u32::from(knobs.byte()) % (members + 1);

    if let Ok(owned) = OwnedFace::parse(font.to_vec(), index) {
        let face = owned.as_face();
        exercise(&face, &mut knobs);
    }
    if let Ok(face) = Face::parse_bytes(font, index) {
        exercise(&face, &mut knobs);
    }
});

fn exercise(face: &Face<'_>, knobs: &mut Knobs<'_>) {
    let _ = face.sfnt_version();
    let _ = face.num_tables();
    let _ = face.head();
    let _ = face.maxp();
    let _ = face.hhea();
    let _ = face.hmtx();
    if let Ok(cmap) = face.cmap() {
        for ch in ['A', 'z', '\u{0627}', '\u{0915}', '\u{4e00}', '\u{1f642}', '\u{10ffff}'] {
            let _ = cmap.glyph_id(ch);
            for selector in ['\u{FE00}', '\u{FE0F}', '\u{E0100}'] {
                let _ = cmap.variation_glyph(ch, selector);
            }
        }
        for selector in cmap.variation_selectors().into_iter().take(8) {
            let _ = cmap.variation_unicodes(selector);
        }
    }
    if let Ok(Some(name)) = face.name() {
        let _ = name.records().len();
        let _ = name.family_name();
        let _ = name.full_name();
        let _ = name.get(knobs.u16());
    }
    let _ = face.gdef();
    let _ = face.gpos();
    let _ = face.gsub();
    let _ = face.kern();
    let _ = face.morx();
    let _ = face.kerx();
    let _ = face.ankr();
    let _ = face.loca();
    let _ = face.glyf();
    let _ = face.fvar();
    let _ = face.avar();
    let _ = face.hvar();
    let _ = face.gvar();
    let _ = face.mvar();
    let _ = face.vvar();
    let _ = face.varc();
    let _ = face.cff();
    let _ = face.cff2();
    let _ = face.vhea();
    let _ = face.vmtx();
    let _ = face.vorg();
    let _ = face.colr();
    let _ = face.cpal();
    if let Ok(Some(math)) = face.math() {
        let _ = math.constants();
        let _ = math.glyph_info();
        let _ = math.variants();
    }
    let _ = face.base();
    let _ = face.cblc();
    let _ = face.cbdt();
    let _ = face.eblc();
    let _ = face.ebdt();
    let _ = face.sbix();
    let _ = face.svg();

    let coords = axis_coords(face, knobs);
    let ppem = knobs.u16();
    for gid in glyph_ids(face, knobs) {
        let _ = face.glyph_outline(gid);
        let _ = face.glyph_outline_at_coords(gid, &coords);
        let _ = face.glyph_bounds(gid);
        let _ = face.glyph_bounds_at_coords(gid, &coords);
        let _ = face.glyph_points(gid);
        let _ = face.glyph_bitmap(gid, 16);
        let _ = face.glyph_bitmap(gid, ppem);
        let _ = face.svg_document(gid);
        let _ = face.colr_paint(gid);
    }
}
